use std::sync::Arc;

use super::{Direction, Easing, MAX_PARAMS, ParamValue, Transition};
use crate::graph::{Node, NodeContext, TextureDesc};
use crate::renderer::{Effect, Filter, Uniform};

/// Draws a [`Transition`] from A (input 0, outgoing) to B (input 1, incoming), both
/// premultiplied linear light, at a linear progress eased on the CPU. The output is the
/// size of A in the working format; B is read at the same place, filtered when its size
/// differs. Unset parameters take their manifest defaults.
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

    fn uniform(&self, size: (u32, u32)) -> Uniform {
        let (index, vector) = self
            .direction
            .map_or((0, [0.0; 2]), |d| (d.index(), d.vector()));
        let header = Uniform::new()
            .f32(self.progress)
            .f32(self.linear)
            .u32(index)
            .u32(0)
            .f32(vector[0])
            .f32(vector[1])
            .f32(size.0 as f32)
            .f32(size.1 as f32);
        self.params.iter().copied().fold(header, Uniform::vec4)
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
        vec![inputs[0].with_format(TextureDesc::WORKING_FORMAT)]
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
        let size = outputs[0].texture().size();
        let uniform = self.uniform((size.width, size.height));
        cx.draw(
            &Effect {
                label: "transition",
                shader: self.transition.shader(),
                entry: "fs",
                uniform: uniform.bytes(),
                inputs: &[inputs[0], inputs[1]],
                filter: Filter::Linear,
            },
            outputs[0],
        );
    }
}
