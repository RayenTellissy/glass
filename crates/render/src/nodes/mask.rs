use crate::graph::{Node, NodeContext, TextureDesc};
use crate::mask::{self, Mask};
use crate::renderer::{Effect, Filter, Uniform};

/// The mask textures' format: coverage 0 to 1 in the red channel.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

/// One mask as the node draws it.
#[derive(Clone, Copy, Debug)]
struct Pass {
    /// Coverage inputs (its shapes), at least 1.
    shapes: usize,
    feather: f32,
    invert: bool,
    mode: mask::MaskMode,
}

impl Pass {
    fn blurred(&self) -> bool {
        mask::kernel_radius(self.feather) > 0
    }

    /// Scratch textures its union and blur need.
    fn scratch(&self) -> usize {
        match (self.shapes, self.blurred()) {
            (1, false) => 0,
            (1, true) | (2, false) => 1,
            _ => 2,
        }
    }
}

/// Masks a layer: input 0 is the layer (working space, premultiplied), then the coverage
/// of every mask's shapes in order (from [`crate::shape::CoverageNode`]s, the layer's
/// size; one for a mask without shapes). Per mask, the shapes are unioned, feathered
/// (horizontal pass, then a vertical one that also inverts and combines with the masks
/// so far) into one of two `R16Float` masks used in turn; the last one multiplies the
/// layer. Outputs: the masked layer, the combined mask, then scratch (none of them for no
/// masks, which copies the layer). Build it with
/// [`mask::add_to_graph`]; the CPU reference is [`mask::reference`].
#[derive(Clone, Debug)]
pub struct MaskNode {
    size: (u32, u32),
    passes: Vec<Pass>,
}

impl MaskNode {
    /// `size` is the layer's.
    pub fn new(size: (u32, u32), masks: &[Mask]) -> Self {
        Self {
            size: (size.0.max(1), size.1.max(1)),
            passes: masks
                .iter()
                .map(|m| Pass {
                    shapes: m.shapes.len().max(1),
                    feather: m.feather_radius(),
                    invert: m.invert,
                    mode: m.mode,
                })
                .collect(),
        }
    }

    /// Coverage inputs after the layer.
    fn coverages(&self) -> usize {
        self.passes.iter().map(|p| p.shapes).sum()
    }

    fn accumulators(&self) -> usize {
        self.passes.len().min(2)
    }

    fn scratch(&self) -> usize {
        self.passes.iter().map(Pass::scratch).max().unwrap_or(0)
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

impl Node for MaskNode {
    fn label(&self) -> &str {
        "mask"
    }

    fn input_count(&self) -> usize {
        1 + self.coverages()
    }

    fn outputs(&self, _inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        let (w, h) = self.size;
        let mask = TextureDesc {
            width: w,
            height: h,
            format: FORMAT,
        };
        let mut out = vec![TextureDesc::working(w, h)];
        out.extend(std::iter::repeat_n(
            mask,
            self.accumulators() + self.scratch(),
        ));
        out
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        let masks = self
            .passes
            .iter()
            .map(|p| {
                format!(
                    "{:?} {} shapes, feather {}{}",
                    p.mode,
                    p.shapes,
                    p.feather,
                    if p.invert { ", inverted" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        vec![("masks", masks)]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let (layer, coverages) = (inputs[0], &inputs[1..]);
        let accumulators = &outputs[1..1 + self.accumulators()];
        let scratch = &outputs[1 + self.accumulators()..];
        let texel = [1.0 / self.size.0 as f32, 1.0 / self.size.1 as f32];
        let renderer = cx.renderer;
        let dummy = renderer.dummy_view();
        let mut timestamps = Timestamps::new(cx.timestamps());
        let mut draw = |entry: &'static str,
                        uniform: &Uniform,
                        inputs: [&wgpu::TextureView; 2],
                        filter: Filter,
                        target: &wgpu::TextureView,
                        last: bool| {
            renderer.draw(
                cx.encoder,
                &Effect {
                    label: "mask",
                    shader: "mask",
                    entry,
                    uniform: uniform.bytes(),
                    inputs: &inputs,
                    filter,
                },
                target,
                timestamps.pass(last),
            );
        };
        let params = |radius: u32, falloff: f32, invert: bool, combine: u32| {
            Uniform::new()
                .u32(radius)
                .f32(falloff)
                .u32(u32::from(invert))
                .u32(combine)
                .f32(texel[0])
                .f32(texel[1])
        };
        let plain = params(0, 0.0, false, 0);

        let mut next = 0;
        let mut previous: Option<&wgpu::TextureView> = None;
        for (i, pass) in self.passes.iter().enumerate() {
            let shapes = &coverages[next..next + pass.shapes];
            next += pass.shapes;
            // Shapes unioned, alternating between the two scratch textures.
            let mut current = shapes[0];
            let mut free = 0;
            for &shape in &shapes[1..] {
                let target = scratch[free];
                draw(
                    "fs_union",
                    &plain,
                    [current, shape],
                    Filter::Nearest,
                    target,
                    false,
                );
                current = target;
                free = 1 - free;
            }
            let radius = mask::kernel_radius(pass.feather);
            let falloff = mask::falloff(pass.feather);
            if radius > 0 {
                let target = scratch[free];
                let blur = params(radius, falloff, false, 0);
                draw(
                    "fs_blur_h",
                    &blur,
                    [current, dummy],
                    Filter::Linear,
                    target,
                    false,
                );
                current = target;
            }
            let combine = if previous.is_some() {
                pass.mode.code()
            } else {
                0
            };
            // The last mask lands in output 1.
            let target = accumulators[(self.passes.len() - 1 - i) % accumulators.len()];
            draw(
                "fs_combine",
                &params(radius, falloff, pass.invert, combine),
                [current, previous.unwrap_or(dummy)],
                Filter::Linear,
                target,
                false,
            );
            previous = Some(target);
        }
        match previous {
            Some(combined) => draw(
                "fs_apply",
                &plain,
                [layer, combined],
                Filter::Nearest,
                outputs[0],
                true,
            ),
            None => draw(
                "fs_copy",
                &plain,
                [layer, dummy],
                Filter::Nearest,
                outputs[0],
                true,
            ),
        }
    }
}
