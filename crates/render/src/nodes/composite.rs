use crate::blend::BlendMode;
use crate::graph::{Graph, Node, NodeContext, Port, TextureDesc};
use crate::renderer::{Effect, Filter, Uniform};

/// One layer of a [`CompositeNode`]. Later per-layer settings (opacity, transform,
/// crop) become fields here, written into the shader's `Layer` by
/// [`CompositeNode::uniform`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layer {
    pub blend: BlendMode,
}

impl Layer {
    pub fn blended(blend: BlendMode) -> Self {
        Self { blend }
    }
}

/// Layers over an optional background, bottom first, in one pass: each layer is
/// blended onto the result so far with its [`BlendMode`] (see [`crate::blend`]). At most
/// [`CompositeNode::MAX_LAYERS`] per node; [`composite_stack`] chains nodes for more.
#[derive(Clone, Debug)]
pub struct CompositeNode {
    canvas: (u32, u32),
    background: bool,
    layers: Vec<Layer>,
}

impl CompositeNode {
    pub const MAX_LAYERS: usize = 8;

    /// Inputs: the background (when `background`), then one texture per layer. Layers
    /// beyond [`Self::MAX_LAYERS`] are dropped.
    pub fn new(canvas: (u32, u32), background: bool, mut layers: Vec<Layer>) -> Self {
        layers.truncate(Self::MAX_LAYERS);
        Self {
            canvas,
            background,
            layers,
        }
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    fn uniform(&self, sizes: &[wgpu::Extent3d]) -> Uniform {
        let mut u = Uniform::new()
            .u32(self.layers.len() as u32)
            .u32(u32::from(self.background))
            .align16();
        for i in 0..Self::MAX_LAYERS {
            let (layer, size) = match (self.layers.get(i), sizes.get(i)) {
                (Some(l), Some(s)) => (l.clone(), (s.width as f32, s.height as f32)),
                _ => (Layer::default(), (0.0, 0.0)),
            };
            u = u.f32(size.0).f32(size.1).u32(layer.blend.code()).u32(0);
        }
        u
    }
}

impl Node for CompositeNode {
    fn label(&self) -> &str {
        "composite"
    }

    fn input_count(&self) -> usize {
        usize::from(self.background) + self.layers.len()
    }

    fn outputs(&self, _inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![TextureDesc::working(self.canvas.0, self.canvas.1)]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        let modes: Vec<&str> = self.layers.iter().map(|l| l.blend.id()).collect();
        vec![("blend", modes.join(","))]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let dummy = cx.renderer.dummy_view();
        let (background, layers) = if self.background {
            (inputs[0], &inputs[1..])
        } else {
            (dummy, inputs)
        };
        let sizes: Vec<wgpu::Extent3d> = layers.iter().map(|v| v.texture().size()).collect();
        let mut bound = vec![background];
        bound.extend(layers.iter().copied());
        bound.resize(1 + Self::MAX_LAYERS, dummy);
        let uniform = self.uniform(&sizes);
        cx.draw(
            &Effect {
                label: "composite",
                shader: "composite",
                entry: "fs",
                uniform: uniform.bytes(),
                inputs: &bound,
                filter: Filter::Nearest,
            },
            outputs[0],
        );
    }
}

/// Adds nodes compositing any number of `layers` over `background` (or transparent),
/// [`CompositeNode::MAX_LAYERS`] per pass, and returns the result.
pub fn composite_stack(
    graph: &mut Graph<'_>,
    canvas: (u32, u32),
    background: Option<Port>,
    layers: Vec<(Port, Layer)>,
) -> Option<Port> {
    let mut result = background;
    for chunk in layers.chunks(CompositeNode::MAX_LAYERS) {
        let mut inputs: Vec<Port> = result.into_iter().collect();
        inputs.extend(chunk.iter().map(|(p, _)| *p));
        let node = CompositeNode::new(
            canvas,
            result.is_some(),
            chunk.iter().map(|(_, l)| l.clone()).collect(),
        );
        result = Some(graph.add(node, &inputs).out(0));
    }
    result
}
