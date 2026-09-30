use crate::blend::BlendMode;
use crate::crop::Crop;
use crate::graph::{Graph, Node, NodeContext, Port, TextureDesc};
use crate::renderer::{Effect, Filter, Uniform};
use crate::transform::{self, Affine2, Resample, ShutterTransforms};

/// One layer of a [`CompositeNode`]: how its texture is placed on the canvas and blended.
/// Per-layer settings are fields here and in the shader's `Layer` record (written by
/// `Layer::gpu_words`); [`Layer::reference`] is the CPU version of what the shader does
/// with them.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub blend: BlendMode,
    /// Layer pixels to canvas pixels (see [`crate::transform`]); the identity puts the
    /// layer at the canvas's top left.
    pub transform: ShutterTransforms,
    pub resample: Resample,
    /// 0 to 1, scaling the layer's premultiplied colour before it is blended, so every
    /// blend mode fades towards the backdrop.
    pub opacity: f32,
    /// Per-edge crop and soft edge, in layer pixels (see [`crate::crop`]).
    pub crop: Crop,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            blend: BlendMode::Normal,
            transform: ShutterTransforms::default(),
            resample: Resample::default(),
            opacity: 1.0,
            crop: Crop::default(),
        }
    }
}

impl Layer {
    pub fn blended(blend: BlendMode) -> Self {
        Self {
            blend,
            ..Self::default()
        }
    }

    pub fn with_transform(mut self, transform: impl Into<ShutterTransforms>) -> Self {
        self.transform = transform.into();
        self
    }

    pub fn with_resample(mut self, resample: Resample) -> Self {
        self.resample = resample;
        self
    }

    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn with_crop(mut self, crop: Crop) -> Self {
        self.crop = crop;
        self
    }

    /// The opacity used: clamped to 0..1, and 0 when the crop leaves nothing.
    fn visible_opacity(&self, size: (u32, u32)) -> f64 {
        let [x0, y0, x1, y1] = self.crop.rect(size);
        if x1 <= x0 || y1 <= y0 || self.opacity.is_nan() {
            return 0.0;
        }
        f64::from(self.opacity.clamp(0.0, 1.0))
    }

    /// Canvas pixels to layer pixels at the middle of the shutter interval, or `None`
    /// when the layer is scaled to nothing.
    pub fn canvas_to_layer(&self) -> Option<Affine2> {
        self.transform.centre().inverse()
    }

    /// The filter actually used: nearest when every canvas pixel centre lands on a layer
    /// pixel centre, since it is exact and the cheapest there.
    pub fn effective_resample(&self) -> Resample {
        match self.canvas_to_layer() {
            Some(m) if m.maps_pixel_centres() => Resample::Nearest,
            _ => self.resample,
        }
    }

    /// The layer's premultiplied colour at canvas point `q` (a pixel centre), from its
    /// premultiplied `texels` of `size` pixels: the CPU reference for the shader.
    pub fn reference(&self, texels: &[[f32; 4]], size: (u32, u32), q: [f64; 2]) -> [f64; 4] {
        let Some(m) = self.canvas_to_layer() else {
            return [0.0; 4];
        };
        let p = m.apply(q);
        let coverage = self.crop.coverage(size, p, m.footprint()) * self.visible_opacity(size);
        if coverage <= 0.0 {
            return [0.0; 4];
        }
        transform::resample(texels, size, p, self.effective_resample()).map(|c| c * coverage)
    }

    /// The shader's `Layer` record for a texture of `size` pixels sampled with
    /// `resample`, as 20 words.
    fn gpu_words(&self, size: (u32, u32), resample: Resample) -> Uniform {
        // A layer scaled to nothing samples a point outside itself everywhere.
        let m = self.canvas_to_layer().unwrap_or(Affine2 {
            a: 0.0,
            b: 0.0,
            c: 0.0,
            d: 0.0,
            tx: -1.0,
            ty: -1.0,
        });
        let [fx, fy] = m.footprint();
        let opacity = self.visible_opacity(size);
        Uniform::new()
            .f32(size.0 as f32)
            .f32(size.1 as f32)
            .u32(self.blend.code())
            .u32(resample.code())
            .vec4([m.a, m.b, m.c, m.d].map(|v| v as f32))
            .vec4([m.tx, m.ty, fx.max(1e-6), fy.max(1e-6)].map(|v| v as f32))
            .vec4(self.crop.rect(size).map(|v| v as f32))
            .vec4([opacity as f32, self.crop.feather() as f32, 0.0, 0.0])
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
    max_resample: Resample,
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
            max_resample: Resample::Bicubic,
        }
    }

    /// Caps every layer's filter at `max` (bicubic layers sample bilinear under a
    /// bilinear cap): playback can trade the sharper filter for about half the GPU time
    /// of a transformed layer, and paused frames and export keep it.
    pub fn with_max_resample(mut self, max: Resample) -> Self {
        self.max_resample = max;
        self
    }

    /// The filter a layer is sampled with in this pass.
    pub fn resample_of(&self, layer: &Layer) -> Resample {
        layer.effective_resample().min(self.max_resample)
    }

    /// The shader entry point: the one for the most expensive filter any layer uses,
    /// so a pass without transformed layers carries no bicubic code.
    fn entry(&self) -> &'static str {
        match self.layers.iter().map(|l| self.resample_of(l)).max() {
            Some(Resample::Bicubic) => "fs_bicubic",
            Some(Resample::Bilinear) => "fs_bilinear",
            _ => "fs_nearest",
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
                (Some(l), Some(s)) => (l.clone(), (s.width, s.height)),
                _ => (Layer::default(), (0, 0)),
            };
            u = u.extend(&layer.gpu_words(size, self.resample_of(&layer)));
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
        let list = |f: &dyn Fn(&Layer) -> &'static str| {
            self.layers.iter().map(f).collect::<Vec<_>>().join(",")
        };
        vec![
            ("blend", list(&|l| l.blend.id())),
            ("resample", list(&|l| self.resample_of(l).id())),
        ]
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
                entry: self.entry(),
                uniform: uniform.bytes(),
                inputs: &bound,
                filter: Filter::Linear,
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
    composite_stack_capped(graph, canvas, background, layers, Resample::Bicubic)
}

/// [`composite_stack`] with every layer's filter capped at `max_resample` (see
/// [`CompositeNode::with_max_resample`]).
pub fn composite_stack_capped(
    graph: &mut Graph<'_>,
    canvas: (u32, u32),
    background: Option<Port>,
    layers: Vec<(Port, Layer)>,
    max_resample: Resample,
) -> Option<Port> {
    let mut result = background;
    for chunk in layers.chunks(CompositeNode::MAX_LAYERS) {
        let mut inputs: Vec<Port> = result.into_iter().collect();
        inputs.extend(chunk.iter().map(|(p, _)| *p));
        let node = CompositeNode::new(
            canvas,
            result.is_some(),
            chunk.iter().map(|(_, l)| l.clone()).collect(),
        )
        .with_max_resample(max_resample);
        result = Some(graph.add(node, &inputs).out(0));
    }
    result
}
