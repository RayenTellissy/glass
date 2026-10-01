//! The visual regression harness: reference sequences described as [`Scene`]s (layers of
//! procedural sources, each with its blend mode, transform, crop and opacity), rendered
//! through the render graph (sources, `composite_stack`, `OutputNode::preview` dithered
//! with the frame number as its seed) into 8-bit sRGB, frame by frame.
//!
//! Adding a case means adding a [`Case`] to [`cases::all`]; see CONTRIBUTING.md, "Visual
//! regression tests".

pub mod cases;
// The tolerant comparison the UI snapshots use, shared by path: `ui` depends on `render`,
// so `render`'s tests cannot depend on `ui`.
#[allow(dead_code)]
#[path = "../../../ui/src/snapshot/compare.rs"]
pub mod compare;

use std::path::Path;

use media::colour::Defaulted;
use media::{
    ChromaSiting, ColourSpace, Hdr, HwSurfaceFormat, Matrix, Primaries, Range, TransferFn,
};
use render::colour::WorkingSpace;
use render::graph::{Graph, Port, TexturePool};
use render::nodes::{Layer, OutputNode, YuvSourceNode, composite_stack};
use render::renderer::Renderer;
use render::text::{
    self, FontFamily, GlyphAtlas, GlyphInfo, GlyphTransform, PreparedText, TextEngine, TextFormat,
    TextPaint,
};
use render::wgpu;
use render::yuv::{PlaneLayout, YuvParams, YuvPlanes};

use crate::common;
use compare::Tolerance;

/// The font every text in a case is set in, which every macOS ships. The goldens are
/// macOS-only until the Geist fonts are bundled.
pub const ARIAL: &str = "/System/Library/Fonts/Supplemental/Arial.ttf";

/// The canvas every case renders at: small, so the goldens stay small.
pub const CANVAS: (u32, u32) = (320, 180);

/// Hardware GPUs: the compositor is float maths with one code of dither, so a channel may
/// be off by 6 levels anywhere, and by more on up to 0.05% of the pixels (28 of a 320 x
/// 180 frame). A layer moved by one pixel changes every pixel along its edges (over 300
/// for a 100 x 60 layer) and fails, as does a wrong blend mode, colour or opacity.
pub const GPU: Tolerance = Tolerance {
    channel: 6,
    max_differing_ratio: 0.000_5,
};

/// Software rasterisers (lavapipe, WARP) round and filter differently: 24 levels, and
/// more on up to 0.5% of the pixels. That catches a missing, moved or wrongly blended
/// layer, not a one-pixel shift.
pub const SOFTWARE: Tolerance = Tolerance {
    channel: 24,
    max_differing_ratio: 0.005,
};

/// One reference sequence.
#[derive(Clone, Copy)]
pub struct Case {
    /// The golden folder name: lowercase ASCII letters, digits and `_`, unique.
    pub name: &'static str,
    /// The frame indices rendered and compared; each is also the dither seed.
    pub frames: &'static [u32],
    /// The scene at a frame index.
    pub scene: fn(u32) -> Scene,
}

/// Layers bottom first, composited over transparent black. Make the bottom layer opaque
/// and cover the canvas: goldens are stored without alpha.
pub struct Scene {
    pub layers: Vec<(Source, Layer)>,
}

/// What a layer shows. Pixels are linear light in BT.709, premultiplied.
pub enum Source {
    /// 160 x 90 NV12 colour bars over a luma ramp (8-bit, limited range, BT.709), through
    /// `YuvSourceNode`: the path a decoded video frame takes.
    Bars,
    /// A 64 x 64 soft orange-to-magenta disc with a feathered alpha edge (an uploaded
    /// still).
    Disc,
    /// A 320 x 180 opaque gradient: dark to bright left to right, blue to green top to
    /// bottom (an uploaded still).
    Gradient,
    /// A solid premultiplied colour of the given size.
    Fill { colour: [f32; 4], size: (u32, u32) },
    /// Text drawn onto a transparent canvas-sized layer.
    Text(TextSpec),
}

/// A text in Arial.
pub struct TextSpec {
    pub text: &'static str,
    /// Font size in pixels.
    pub size: f32,
    pub paint: TextPaint,
    /// The per-glyph transform hook (the text animator's).
    pub glyphs: Option<Box<dyn Fn(&GlyphInfo) -> GlyphTransform>>,
}

/// The device, the shared sources, and what the renders reuse across frames.
pub struct Studio {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// The adapter's name and type, for the log.
    pub adapter: String,
    pub software: bool,
    renderer: Renderer,
    pool: TexturePool,
    engine: TextEngine,
    atlas: GlyphAtlas,
    bars_luma: wgpu::TextureView,
    bars_chroma: wgpu::TextureView,
    bars_params: YuvParams,
    disc: wgpu::TextureView,
    gradient: wgpu::TextureView,
}

impl Studio {
    /// `None` without a GPU adapter. Panics when Arial is missing; check [`ARIAL`] first.
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .ok()?;
        let info = adapter.get_info();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).ok()?;
        let renderer = Renderer::new(&device, &queue);
        let engine = TextEngine::with_font_files(&[Path::new(ARIAL)]).expect("Arial loads");
        let atlas = GlyphAtlas::new(&device, &queue);
        let (bars_luma, bars_chroma, bars_params) = bars(&device, &queue);
        let disc = common::rgba16f(&device, &queue, (64, 64), &disc_pixels())
            .create_view(&Default::default());
        let gradient = common::rgba16f(&device, &queue, CANVAS, &gradient_pixels())
            .create_view(&Default::default());
        Some(Self {
            adapter: format!("{} ({:?}, {:?})", info.name, info.device_type, info.backend),
            software: info.device_type == wgpu::DeviceType::Cpu,
            device,
            queue,
            renderer,
            pool: TexturePool::new(),
            engine,
            atlas,
            bars_luma,
            bars_chroma,
            bars_params,
            disc,
            gradient,
        })
    }

    /// The scene through the graph into 8-bit sRGB (`OutputNode::preview`, seeded with
    /// `frame`), read back as RGBA8 rows.
    pub fn render(&mut self, scene: &Scene, frame: u32) -> Vec<u8> {
        // Resources the graph borrows are made first: fills and prepared texts.
        let mut fills = Vec::new();
        let mut texts: Vec<PreparedText> = Vec::new();
        self.atlas.begin_frame();
        for (source, _) in &scene.layers {
            match source {
                Source::Fill { colour, size } => {
                    let pixels = vec![*colour; (size.0 * size.1) as usize];
                    fills.push(
                        common::rgba16f(&self.device, &self.queue, *size, &pixels)
                            .create_view(&Default::default()),
                    );
                }
                Source::Text(spec) => {
                    let format = TextFormat {
                        family: FontFamily::Named("Arial".to_owned()),
                        size: spec.size,
                        ..Default::default()
                    };
                    let layout = self.engine.layout(spec.text, &format);
                    texts.push(self.atlas.prepare(
                        &mut self.engine,
                        &layout,
                        &spec.paint,
                        spec.glyphs.as_deref(),
                    ));
                }
                _ => {}
            }
        }

        let target = common::blank(&self.device, wgpu::TextureFormat::Rgba8Unorm, CANVAS);
        let target_view = target.create_view(&Default::default());
        let mut g = Graph::new();
        let (mut fill, mut text) = (fills.iter(), texts.iter());
        let mut layers: Vec<(Port, Layer)> = Vec::new();
        for (source, layer) in &scene.layers {
            let port = match source {
                Source::Bars => g
                    .add(
                        YuvSourceNode::new(
                            YuvPlanes::Biplanar {
                                luma: &self.bars_luma,
                                chroma: &self.bars_chroma,
                            },
                            self.bars_params,
                            Primaries::Bt709,
                            WorkingSpace::BT709,
                        ),
                        &[],
                    )
                    .out(0),
                Source::Disc => g.import("disc", &self.disc),
                Source::Gradient => g.import("gradient", &self.gradient),
                Source::Fill { .. } => g.import("fill", fill.next().expect("made above")),
                Source::Text(_) => text::add_to_graph(
                    &mut g,
                    &self.atlas,
                    text.next().expect("prepared above"),
                    CANVAS,
                ),
            };
            layers.push((port, layer.clone()));
        }
        let composite = composite_stack(&mut g, CANVAS, None, layers).expect("a scene has layers");
        let output = g.add(
            OutputNode::preview(WorkingSpace::BT709).with_seed(frame),
            &[composite],
        );
        g.export(output.out(0), &target_view);
        g.compile()
            .expect("the scene compiles")
            .execute(&self.renderer, &mut self.pool, None);
        common::read(&self.device, &self.queue, &target)
            .iter()
            .flat_map(|px| px.map(|c| (c * 255.0).round() as u8))
            .collect()
    }
}

/// Problems with the registry: bad or duplicate names, cases without frames.
pub fn registry_problems(cases: &[Case]) -> Vec<String> {
    let mut problems = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let valid_name = !case.name.is_empty()
            && case
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !valid_name {
            problems.push(format!(
                "case {:?}: names use lowercase letters, digits and _",
                case.name
            ));
        }
        if cases[..i].iter().any(|other| other.name == case.name) {
            problems.push(format!("case {:?} is registered twice", case.name));
        }
        if case.frames.is_empty() {
            problems.push(format!("case {:?} has no frames", case.name));
        }
    }
    problems
}

/// The golden file name for a frame index, `frame_012.png`.
pub fn golden_file_name(frame: u32) -> String {
    format!("frame_{frame:03}.png")
}

/// BT.709 colour bars (75%) over a luma ramp in the bottom third, as NV12 planes.
fn bars(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::TextureView, wgpu::TextureView, YuvParams) {
    const W: usize = 160;
    const H: usize = 90;
    const BARS: [[f64; 3]; 7] = [
        [0.75, 0.75, 0.75],
        [0.75, 0.75, 0.0],
        [0.0, 0.75, 0.75],
        [0.0, 0.75, 0.0],
        [0.75, 0.0, 0.75],
        [0.75, 0.0, 0.0],
        [0.0, 0.0, 0.75],
    ];
    // Encoded R'G'B' to limited-range Y'CbCr, BT.709.
    let ycbcr = |[r, g, b]: [f64; 3]| {
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        [
            16.0 + 219.0 * y,
            128.0 + 224.0 * (b - y) / 1.8556,
            128.0 + 224.0 * (r - y) / 1.5748,
        ]
    };
    let rgb = |x: usize, y: usize| {
        if y < H * 2 / 3 {
            BARS[x * BARS.len() / W]
        } else {
            [x as f64 / (W - 1) as f64; 3]
        }
    };
    let mut luma = vec![0u8; W * H];
    let mut chroma = vec![0u8; W * H / 2];
    for y in 0..H {
        for x in 0..W {
            luma[y * W + x] = ycbcr(rgb(x, y))[0].round() as u8;
        }
    }
    for cy in 0..H / 2 {
        for cx in 0..W / 2 {
            let mut sum = [0.0; 2];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let s = ycbcr(rgb(cx * 2 + dx, cy * 2 + dy));
                sum[0] += s[1] / 4.0;
                sum[1] += s[2] / 4.0;
            }
            let at = (cy * W / 2 + cx) * 2;
            chroma[at] = sum[0].round() as u8;
            chroma[at + 1] = sum[1].round() as u8;
        }
    }
    let colour = ColourSpace {
        primaries: Primaries::Bt709,
        transfer: TransferFn::Bt1886,
        matrix: Matrix::Bt709,
        range: Range::Limited,
        siting: ChromaSiting::Center,
        bit_depth: 8,
        hdr: Hdr::None,
        defaulted: Defaulted::default(),
    };
    let (lf, cf) = render::interop::plane_formats(HwSurfaceFormat::Nv12);
    (
        common::texture(device, queue, lf, (W as u32, H as u32), &luma)
            .create_view(&Default::default()),
        common::texture(device, queue, cf, (W as u32 / 2, H as u32 / 2), &chroma)
            .create_view(&Default::default()),
        YuvParams::new(&colour, PlaneLayout::from_surface(HwSurfaceFormat::Nv12)),
    )
}

fn disc_pixels() -> Vec<[f32; 4]> {
    let mut pixels = Vec::with_capacity(64 * 64);
    for y in 0..64 {
        for x in 0..64 {
            let (dx, dy) = (x as f32 + 0.5 - 32.0, y as f32 + 0.5 - 32.0);
            let r = (dx * dx + dy * dy).sqrt() / 30.0;
            // Opaque to r = 0.7, a smoothstep to nothing at r = 1.
            let t = ((r - 0.7) / 0.3).clamp(0.0, 1.0);
            let alpha = 1.0 - t * t * (3.0 - 2.0 * t);
            let colour = [1.0, 0.45 - 0.35 * r.min(1.0), 0.05 + 0.6 * r.min(1.0)];
            pixels.push([
                colour[0] * alpha,
                colour[1] * alpha,
                colour[2] * alpha,
                alpha,
            ]);
        }
    }
    pixels
}

fn gradient_pixels() -> Vec<[f32; 4]> {
    let (w, h) = CANVAS;
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let u = x as f32 / (w - 1) as f32;
            let v = y as f32 / (h - 1) as f32;
            // Squared so the ramp looks even once encoded.
            let l = u * u;
            pixels.push([l * 0.9, l * (0.3 + 0.6 * v), l * (1.0 - 0.7 * v), 1.0]);
        }
    }
    pixels
}
