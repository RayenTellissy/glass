use std::sync::Arc;

use super::{Direction, Easing, MAX_PARAMS, MAX_PASSES, ParamValue, Transition};
use crate::graph::{Node, NodeContext, TextureDesc};
use crate::renderer::{Effect, Filter, Uniform};

/// Draws a [`Transition`] from A (input 0, outgoing) to B (input 1, incoming), both
/// premultiplied linear light, at a linear progress eased on the CPU. The output is the
/// size of A in the working format; B is read at the same place, filtered when its size
/// differs. Unset parameters take their manifest defaults.
///
/// A transition with extra passes draws each into one more output (after the result,
/// sized by [`super::Pass::size`]) before `fs` draws the result; every draw binds A, B
/// and the targets of the passes before it, a 1x1 placeholder for the rest. The node's
/// timestamps span all its passes.
#[derive(Clone, Debug)]
pub struct TransitionNode {
    transition: Arc<Transition>,
    linear: f32,
    progress: f32,
    direction: Option<Direction>,
    params: [[f32; 4]; MAX_PARAMS],
}

impl TransitionNode {
    /// `linear` is the position in the transition, 0 at its start and 1 at its end
    /// (clamped), shaped by `easing`.
    pub fn new(transition: Arc<Transition>, linear: f64, easing: Easing) -> Self {
        let linear = if linear.is_nan() {
            0.0
        } else {
            linear.clamp(0.0, 1.0)
        };
        let mut params = [[0.0; 4]; MAX_PARAMS];
        for (slot, p) in params.iter_mut().zip(transition.params()) {
            *slot = p.pack(&p.default).unwrap_or_default();
        }
        Self {
            direction: transition.direction_or_default(None),
            linear: linear as f32,
            progress: easing.apply(linear) as f32,
            transition,
            params,
        }
    }

    /// The transition at its thumbnail progress with defaults, as the browser shows it.
    pub fn thumbnail(transition: Arc<Transition>) -> Self {
        let at = transition.thumbnail();
        Self::new(transition, at, Easing::Linear)
    }

    /// Travels in `direction`; one the transition does not offer leaves the default.
    pub fn direction(mut self, direction: Direction) -> Self {
        self.direction = self.transition.direction_or_default(Some(direction));
        self
    }

    /// Sets the parameter `id`. An unknown id, or a value of the wrong kind, is ignored
    /// (the parameter keeps its value).
    pub fn value(mut self, id: &str, value: &ParamValue) -> Self {
        if let Some((slot, packed)) = self
            .transition
            .params()
            .iter()
            .position(|p| p.id == id)
            .and_then(|i| Some((i, self.transition.params()[i].pack(value)?)))
        {
            self.params[slot] = packed;
        }
        self
    }

    /// The eased progress the shader sees.
    pub fn progress(&self) -> f32 {
        self.progress
    }

    /// The uniform for drawing pass `pass` (the pass count for `fs`) into a target of
    /// `target` pixels.
    fn uniform(&self, size: (u32, u32), pass: u32, target: (u32, u32)) -> Uniform {
        let (index, vector) = self
            .direction
            .map_or((0, [0.0; 2]), |d| (d.index(), d.vector()));
        let header = Uniform::new()
            .f32(self.progress)
            .f32(self.linear)
            .u32(index)
            .u32(pass)
            .f32(vector[0])
            .f32(vector[1])
            .f32(size.0 as f32)
            .f32(size.1 as f32)
            .f32(target.0 as f32)
            .f32(target.1 as f32)
            .align16();
        self.params.iter().copied().fold(header, Uniform::vec4)
    }
}

/// Splits the node's timestamp writes over its first and last pass.
struct Timestamps<'a> {
    first: Option<wgpu::RenderPassTimestampWrites<'a>>,
    end: Option<u32>,
    set: Option<&'a wgpu::QuerySet>,
}

impl<'a> Timestamps<'a> {
    fn new(t: Option<wgpu::RenderPassTimestampWrites<'a>>) -> Self {
        Self {
            end: t.as_ref().and_then(|t| t.end_of_pass_write_index),
            set: t.as_ref().map(|t| t.query_set),
            first: t.map(|t| wgpu::RenderPassTimestampWrites {
                end_of_pass_write_index: None,
                ..t
            }),
        }
    }

    fn pass(&mut self, last: bool) -> Option<wgpu::RenderPassTimestampWrites<'a>> {
        let end = if last { self.end.take() } else { None };
        match (self.first.take(), end) {
            (Some(mut f), end) => {
                f.end_of_pass_write_index = end;
                Some(f)
            }
            (None, Some(end)) => self.set.map(|query_set| wgpu::RenderPassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: None,
                end_of_pass_write_index: Some(end),
            }),
            (None, None) => None,
        }
    }
}

impl Node for TransitionNode {
    fn label(&self) -> &str {
        "transition"
    }

    fn input_count(&self) -> usize {
        2
    }

    fn outputs(&self, inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        let size = (inputs[0].width, inputs[0].height);
        std::iter::once(inputs[0].with_format(TextureDesc::WORKING_FORMAT))
            .chain(self.transition.passes().iter().map(|p| {
                let (width, height) = p.size(size, self.direction);
                TextureDesc::working(width, height)
            }))
            .collect()
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![
            ("transition", self.transition.id().to_owned()),
            ("progress", format!("{:.3}", self.progress)),
            (
                "direction",
                self.direction.map_or("none", Direction::id).to_owned(),
            ),
        ]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let extent = |view: &wgpu::TextureView| {
            let size = view.texture().size();
            (size.width, size.height)
        };
        let size = extent(outputs[0]);
        let passes = self.transition.passes();
        let renderer = cx.renderer;
        let mut timestamps = Timestamps::new(cx.timestamps());
        let mut bound = [renderer.dummy_view(); 2 + MAX_PASSES];
        bound[0] = inputs[0];
        bound[1] = inputs[1];
        for i in 0..=passes.len() {
            let (entry, target) = match passes.get(i) {
                Some(p) => (p.entry, outputs[1 + i]),
                None => ("fs", outputs[0]),
            };
            let uniform = self.uniform(size, i as u32, extent(target));
            renderer.draw(
                cx.encoder,
                &Effect {
                    label: "transition",
                    shader: self.transition.shader(),
                    entry,
                    uniform: uniform.bytes(),
                    inputs: &bound,
                    filter: Filter::Linear,
                },
                target,
                timestamps.pass(i == passes.len()),
            );
            if i < passes.len() {
                bound[2 + i] = target;
            }
        }
    }
}
