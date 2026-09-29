//! Y'CbCr to RGB on the GPU: one full-screen pass that samples the luma and chroma planes
//! of a frame (biplanar NV12/P010 and their 4:2:2 and 4:4:4 siblings from hardware
//! decoders, or three planes from a software decode, 8 to 16 bits), applies the frame's
//! matrix and range at its bit depth, sites chroma where the file says it sits, and
//! writes either linear light ([`YuvOutput::Linear`], the working space, for
//! `Rgba16Float`) or the encoded R'G'B' values ([`YuvOutput::Encoded`], what swscale
//! produces, for comparisons and 8-bit previews).
//!
//! Chroma siting: the chroma sample `i` of a plane subsampled by `f` sits at luma position
//! `offset + i * f` ([`media::ChromaSiting::offset`]), so the luma pixel at `k` reads
//! chroma at texel coordinate `(k - offset) / f + 0.5` with bilinear filtering. Centred
//! siting reduces to sampling at the luma pixel's own normalised coordinate.
//!
//! Linear light is relative to SDR reference white (203 nits): BT.1886 and the pure
//! power laws are display EOTFs, sRGB is the IEC piecewise curve, PQ is absolute
//! (10,000 nits is about 49.3) and HLG applies the BT.2100 OOTF for a 1000-nit display
//! (see [`media::colour`], which holds the matching CPU functions). Primaries are carried
//! but not converted; gamut conversion into the working primaries belongs to the render
//! graph.

use media::{ColourSpace, HwFrame, HwSurfaceFormat, Matrix, PixelFormat, Range, TransferFn};
use wgpu::util::DeviceExt;

const SHADER: &str = r#"
struct Params {
    // Rows of the 3x4 matrix taking (Y, Cb, Cr, 1) samples, as the textures return them,
    // to R'G'B'.
    r: vec4<f32>,
    g: vec4<f32>,
    b: vec4<f32>,
    // First chroma sample's position in luma samples, and the subsampling factors.
    chroma_offset: vec2<f32>,
    chroma_step: vec2<f32>,
    transfer: u32,
    linear: u32,
    planar: u32,
    _pad: u32,
}

@group(0) @binding(0) var luma: texture_2d<f32>;
@group(0) @binding(1) var chroma_a: texture_2d<f32>;
@group(0) @binding(2) var chroma_b: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VsOut;
    o.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    return o;
}

fn pq_nits(v: f32) -> f32 {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 4096.0 * 128.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 4096.0 * 32.0;
    let c3 = 2392.0 / 4096.0 * 32.0;
    let p = pow(v, 1.0 / m2);
    return 10000.0 * pow(max(p - c1, 0.0) / (c2 - c3 * p), 1.0 / m1);
}

fn hlg_scene(v: f32) -> f32 {
    let a = 0.17883277;
    let b = 1.0 - 4.0 * a;
    let c = 0.55991073;
    if v <= 0.5 {
        return v * v / 3.0;
    }
    return (exp((v - c) / a) + b) / 12.0;
}

fn srgb(v: f32) -> f32 {
    if v <= 0.04045 {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

// Linear light relative to 203-nit reference white; the codes follow `transfer_code`.
fn to_linear(e: vec3<f32>) -> vec3<f32> {
    switch params.transfer {
        case 0u: { return pow(e, vec3<f32>(2.4)); }
        case 1u: { return vec3<f32>(srgb(e.r), srgb(e.g), srgb(e.b)); }
        case 2u: { return pow(e, vec3<f32>(2.2)); }
        case 3u: { return pow(e, vec3<f32>(2.8)); }
        case 5u: { return vec3<f32>(pq_nits(e.r), pq_nits(e.g), pq_nits(e.b)) / 203.0; }
        case 6u: {
            let s = vec3<f32>(hlg_scene(e.r), hlg_scene(e.g), hlg_scene(e.b));
            let ys = dot(s, vec3<f32>(0.2627, 0.6780, 0.0593));
            if ys <= 0.0 {
                return vec3<f32>(0.0);
            }
            return s * (1000.0 * pow(ys, 0.2) / 203.0);
        }
        default: { return e; }
    }
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let luma_size = vec2<f32>(textureDimensions(luma));
    let chroma_size = vec2<f32>(textureDimensions(chroma_a));
    let k = in.uv * luma_size - 0.5;
    let c = (k - params.chroma_offset) / params.chroma_step + 0.5;
    let cuv = c / chroma_size;
    let y = textureSampleLevel(luma, samp, in.uv, 0.0).r;
    let ca = textureSampleLevel(chroma_a, samp, cuv, 0.0);
    let cb = textureSampleLevel(chroma_b, samp, cuv, 0.0);
    let cr = select(ca.g, cb.r, params.planar != 0u);
    let v = vec4<f32>(y, ca.r, cr, 1.0);
    let e = clamp(
        vec3<f32>(dot(params.r, v), dot(params.g, v), dot(params.b, v)),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    if params.linear != 0u {
        return vec4<f32>(to_linear(e), 1.0);
    }
    return vec4<f32>(e, 1.0);
}
"#;

/// What the pass writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum YuvOutput {
    /// Linear light relative to SDR reference white, the working space; meant for
    /// `Rgba16Float` targets (PQ and HLG exceed 1.0).
    #[default]
    Linear,
    /// The encoded R'G'B' values in 0..1, as swscale produces them.
    Encoded,
}

/// How a frame's samples are laid out in textures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneLayout {
    /// Three planes (Y, Cb, Cr) rather than Y plus interleaved CbCr.
    pub planar: bool,
    /// Bits of the texture format holding a sample: 8 (`R8Unorm`) or 16 (`R16Unorm`).
    pub container_bits: u8,
    /// Samples sit in the high bits of the container (P010) rather than the low bits
    /// (`yuv420p10le`).
    pub msb_aligned: bool,
    /// log2 of the chroma subsampling.
    pub chroma_shift: (u8, u8),
}

impl PlaneLayout {
    /// A hardware surface: biplanar, 10-bit samples in the high bits of 16.
    pub fn from_surface(format: HwSurfaceFormat) -> Self {
        Self {
            planar: false,
            container_bits: format.bytes_per_sample() * 8,
            msb_aligned: true,
            chroma_shift: format.chroma_shift(),
        }
    }

    /// A CPU pixel format the converter reads, or `None` (packed RGB, YUV with alpha,
    /// big-endian and packed formats). Planar RGB (`gbrp`, and `gbrap` whose alpha plane
    /// is uploaded but not read yet) goes through the identity matrix, planes G, B, R in
    /// the Y, Cb, Cr slots: how stills arrive from `media::source`.
    pub fn from_pixel_format(format: PixelFormat) -> Option<Self> {
        let name = format.name();
        let depth = format.bit_depth();
        let container_bits = if depth > 8 { 16 } else { 8 };
        if depth > 8 && !name.ends_with("le") {
            return None;
        }
        if name.starts_with("gbrp") || name.starts_with("gbrap") {
            return Some(Self {
                planar: true,
                container_bits,
                msb_aligned: false,
                chroma_shift: (0, 0),
            });
        }
        if format.is_rgb() || format.has_alpha() || format.is_hardware() {
            return None;
        }
        let planar = match format.plane_count() {
            3 => true,
            2 if name.starts_with("nv") || name.starts_with('p') => false,
            _ => return None,
        };
        Some(Self {
            planar,
            container_bits,
            // P010, P012 and P016 keep their samples in the high bits.
            msb_aligned: !planar && depth > 8,
            chroma_shift: format.chroma_shift(),
        })
    }

    /// Texture formats for the luma and each chroma plane.
    pub fn texture_formats(&self) -> (wgpu::TextureFormat, wgpu::TextureFormat) {
        use wgpu::TextureFormat as F;
        match (self.container_bits > 8, self.planar) {
            (false, true) => (F::R8Unorm, F::R8Unorm),
            (false, false) => (F::R8Unorm, F::Rg8Unorm),
            (true, true) => (F::R16Unorm, F::R16Unorm),
            (true, false) => (F::R16Unorm, F::Rg16Unorm),
        }
    }
}

/// How to read a frame's samples and what to write.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YuvParams {
    pub matrix: Matrix,
    pub full_range: bool,
    /// Significant bits per sample (8, 10, 12 or 16).
    pub bit_depth: u8,
    pub layout: PlaneLayout,
    /// First chroma sample's position in luma samples ([`media::ChromaSiting::offset`]).
    pub chroma_offset: (f32, f32),
    pub transfer: TransferFn,
    pub output: YuvOutput,
}

impl YuvParams {
    /// Parameters for a hardware frame, from its colour description; linear output.
    pub fn for_frame(frame: &HwFrame) -> Self {
        Self::new(
            &frame.colour_space(),
            PlaneLayout::from_surface(frame.format()),
        )
    }

    /// Parameters for a CPU frame's planes, or `None` when the converter cannot read its
    /// pixel format.
    pub fn for_cpu_frame(frame: &media::VideoFrame) -> Option<Self> {
        let layout = PlaneLayout::from_pixel_format(frame.format())?;
        Some(Self::new(&frame.colour_space(), layout))
    }

    pub fn new(colour: &ColourSpace, layout: PlaneLayout) -> Self {
        Self {
            matrix: colour.matrix,
            full_range: colour.range == Range::Full,
            bit_depth: colour.bit_depth.clamp(8, layout.container_bits),
            layout,
            chroma_offset: colour.siting.offset(layout.chroma_shift),
            transfer: colour.transfer,
            output: YuvOutput::Linear,
        }
    }

    pub fn with_output(self, output: YuvOutput) -> Self {
        Self { output, ..self }
    }

    /// The encoded R'G'B' output, as swscale produces it.
    pub fn encoded(self) -> Self {
        self.with_output(YuvOutput::Encoded)
    }

    /// Rows of the matrix taking normalised texture samples (Y, Cb, Cr, 1) to R'G'B'.
    pub fn matrix_rows(&self) -> [[f32; 4]; 3] {
        let n = f64::from(self.bit_depth);
        let max = 2f64.powf(n) - 1.0;
        let container_bits = i32::from(self.layout.container_bits);
        let container_max = 2f64.powi(container_bits) - 1.0;
        // A texture sample `s` holds code value `s * container_max / 2^shift`, where
        // `shift` is how far the samples sit above the low bits.
        let shift = if self.layout.msb_aligned {
            2f64.powi(container_bits - i32::from(self.bit_depth))
        } else {
            1.0
        };
        let to_code = container_max / shift;
        let scale = 2f64.powf(n - 8.0);
        let (y_off, y_range, c_off, c_range) = if self.full_range {
            (0.0, max, 128.0 * scale, max)
        } else {
            (16.0 * scale, 219.0 * scale, 128.0 * scale, 224.0 * scale)
        };
        // Y' = (s_y * to_code - y_off) / y_range, likewise for Cb and Cr.
        let ys = to_code / y_range;
        let yo = -y_off / y_range;
        let Some((kr, kb)) = self.matrix.weights() else {
            // Identity: the planes hold G, B and R, all scaled like luma.
            return [
                [0.0, 0.0, ys as f32, yo as f32],
                [ys as f32, 0.0, 0.0, yo as f32],
                [0.0, ys as f32, 0.0, yo as f32],
            ];
        };
        let cs = to_code / c_range;
        let co = -c_off / c_range;
        let kg = 1.0 - kr - kb;
        let r_cr = 2.0 * (1.0 - kr);
        let b_cb = 2.0 * (1.0 - kb);
        let g_cb = -2.0 * kb * (1.0 - kb) / kg;
        let g_cr = -2.0 * kr * (1.0 - kr) / kg;
        // R = Y' + r_cr Cr, G = Y' + g_cb Cb + g_cr Cr, B = Y' + b_cb Cb.
        [
            [ys, 0.0, r_cr * cs, yo + r_cr * co],
            [ys, g_cb * cs, g_cr * cs, yo + (g_cb + g_cr) * co],
            [ys, b_cb * cs, 0.0, yo + b_cb * co],
        ]
        .map(|row| row.map(|v| v as f32))
    }

    fn uniform(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(80);
        for row in self.matrix_rows() {
            for v in row {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        let (sx, sy) = self.layout.chroma_shift;
        for v in [
            self.chroma_offset.0,
            self.chroma_offset.1,
            f32::from(1u16 << sx),
            f32::from(1u16 << sy),
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [
            transfer_code(self.transfer),
            u32::from(self.output == YuvOutput::Linear),
            u32::from(self.layout.planar),
            0,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

fn transfer_code(transfer: TransferFn) -> u32 {
    match transfer {
        TransferFn::Bt1886 => 0,
        TransferFn::Srgb => 1,
        TransferFn::Gamma22 => 2,
        TransferFn::Gamma28 => 3,
        TransferFn::Linear => 4,
        TransferFn::Pq => 5,
        TransferFn::Hlg => 6,
    }
}

/// A frame's planes as texture views.
#[derive(Clone, Copy, Debug)]
pub enum YuvPlanes<'a> {
    /// Luma plus interleaved CbCr (NV12, P010 and siblings).
    Biplanar {
        luma: &'a wgpu::TextureView,
        chroma: &'a wgpu::TextureView,
    },
    /// Three planes.
    Planar {
        y: &'a wgpu::TextureView,
        cb: &'a wgpu::TextureView,
        cr: &'a wgpu::TextureView,
    },
}

/// Output formats the converter renders to.
pub const TARGET_FORMATS: [wgpu::TextureFormat; 2] = [
    wgpu::TextureFormat::Rgba16Float,
    wgpu::TextureFormat::Rgba8Unorm,
];

/// The Y'CbCr to RGB pass. Create once per device on the render thread.
#[derive(Debug)]
pub struct YuvConverter {
    layout: wgpu::BindGroupLayout,
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    sampler: wgpu::Sampler,
}

impl YuvConverter {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv to rgb"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv planes"),
            entries: &[
                texture(0),
                texture(1),
                texture(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuv to rgb"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipelines = TARGET_FORMATS
            .iter()
            .map(|&format| {
                let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("yuv to rgb"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(format.into())],
                    }),
                    multiview_mask: None,
                    cache: None,
                });
                (format, pipeline)
            })
            .collect();
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuv planes"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self {
            layout,
            pipelines,
            sampler,
        }
    }

    /// Records the conversion of `planes` into `target`, which must be one of
    /// [`TARGET_FORMATS`]; the frame is scaled to the target's size. Returns `false` for
    /// an unsupported target format.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        planes: YuvPlanes<'_>,
        params: &YuvParams,
        target: &wgpu::TextureView,
        target_format: wgpu::TextureFormat,
    ) -> bool {
        let Some((_, pipeline)) = self.pipelines.iter().find(|(f, _)| *f == target_format) else {
            return false;
        };
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuv params"),
            contents: &params.uniform(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let (luma, a, b) = match planes {
            YuvPlanes::Biplanar { luma, chroma } => (luma, chroma, chroma),
            YuvPlanes::Planar { y, cb, cr } => (y, cb, cr),
        };
        let view = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv planes"),
            layout: &self.layout,
            entries: &[
                view(0, luma),
                view(1, a),
                view(2, b),
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("yuv to rgb"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media::colour::{Defaulted, LayoutHint};
    use media::{ChromaSiting, ColourInfo, ColourRange, Hdr, Primaries};

    fn colour(matrix: Matrix, range: Range, bit_depth: u8) -> ColourSpace {
        ColourSpace {
            primaries: Primaries::Bt709,
            transfer: TransferFn::Bt1886,
            matrix,
            range,
            siting: ChromaSiting::Left,
            bit_depth,
            hdr: Hdr::None,
            defaulted: Defaulted::default(),
        }
    }

    fn apply(rows: &[[f32; 4]; 3], y: f32, cb: f32, cr: f32) -> [f32; 3] {
        let v = [y, cb, cr, 1.0];
        rows.map(|r| r.iter().zip(v).map(|(a, b)| a * b).sum())
    }

    fn close(a: [f32; 3], b: [f32; 3]) {
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() < 1e-3, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn limited_range_8bit_bt709() {
        let p = YuvParams::new(
            &colour(Matrix::Bt709, Range::Limited, 8),
            PlaneLayout::from_surface(HwSurfaceFormat::Nv12),
        );
        let rows = p.matrix_rows();
        let s = |code: f32| code / 255.0;
        close(apply(&rows, s(16.0), s(128.0), s(128.0)), [0.0, 0.0, 0.0]);
        close(apply(&rows, s(235.0), s(128.0), s(128.0)), [1.0, 1.0, 1.0]);
        // BT.709 red: Y' 63, Cb 102, Cr 240.
        let red = apply(&rows, s(63.0), s(102.0), s(240.0));
        assert!(red[0] > 0.99 && red[1] < 0.01 && red[2] < 0.01, "{red:?}");
    }

    #[test]
    fn ten_bit_in_16bit_containers() {
        let untagged_uhd = ColourSpace::resolve(
            &ColourInfo {
                primaries: Some("bt2020"),
                transfer: None,
                matrix: None,
                range: ColourRange::Unspecified,
                chroma_location: None,
            },
            LayoutHint {
                bit_depth: 10,
                rgb: false,
                jpeg: false,
                surface_full_range: Some(false),
                height: 2160,
            },
        );
        let p = YuvParams::new(
            &untagged_uhd,
            PlaneLayout::from_surface(HwSurfaceFormat::P010),
        );
        assert_eq!(p.matrix, Matrix::Bt2020);
        let rows = p.matrix_rows();
        // 10-bit codes in the top bits of a 16-bit sample.
        let s = |code: f32| code * 64.0 / 65535.0;
        close(apply(&rows, s(64.0), s(512.0), s(512.0)), [0.0, 0.0, 0.0]);
        close(apply(&rows, s(940.0), s(512.0), s(512.0)), [1.0, 1.0, 1.0]);
        // The same codes in the low bits (yuv420p10le planes).
        let planar = PlaneLayout::from_pixel_format(
            PixelFormat::from_name("yuv420p10le").expect("known format"),
        )
        .expect("readable");
        assert!(planar.planar && !planar.msb_aligned);
        let rows = YuvParams::new(&untagged_uhd, planar).matrix_rows();
        let s = |code: f32| code / 65535.0;
        close(apply(&rows, s(940.0), s(512.0), s(512.0)), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn twelve_bit_full_and_limited() {
        let layout = PlaneLayout::from_pixel_format(
            PixelFormat::from_name("yuv422p12le").expect("known format"),
        )
        .expect("readable");
        assert_eq!(layout.chroma_shift, (1, 0));
        let s = |code: f32| code / 65535.0;
        let limited = YuvParams::new(&colour(Matrix::Bt709, Range::Limited, 12), layout);
        let rows = limited.matrix_rows();
        close(apply(&rows, s(256.0), s(2048.0), s(2048.0)), [0.0; 3]);
        close(apply(&rows, s(3760.0), s(2048.0), s(2048.0)), [1.0; 3]);
        let full = YuvParams::new(&colour(Matrix::Bt709, Range::Full, 12), layout);
        close(
            apply(&full.matrix_rows(), s(4095.0), s(2048.0), s(2048.0)),
            [1.0; 3],
        );
    }

    #[test]
    fn full_range_and_identity() {
        let p = YuvParams::new(
            &colour(Matrix::Bt601, Range::Full, 8),
            PlaneLayout::from_surface(HwSurfaceFormat::Nv12),
        );
        let rows = p.matrix_rows();
        close(
            apply(&rows, 1.0, 128.0 / 255.0, 128.0 / 255.0),
            [1.0, 1.0, 1.0],
        );
        close(
            apply(&rows, 0.0, 128.0 / 255.0, 128.0 / 255.0),
            [0.0, 0.0, 0.0],
        );
        let gbr = YuvParams::new(
            &colour(Matrix::Identity, Range::Full, 8),
            PlaneLayout::from_surface(HwSurfaceFormat::Nv24),
        );
        // Planes hold G, B, R.
        close(apply(&gbr.matrix_rows(), 0.25, 0.5, 1.0), [1.0, 0.25, 0.5]);
    }

    #[test]
    fn layouts_of_cpu_formats() {
        let layout = |name| PixelFormat::from_name(name).and_then(PlaneLayout::from_pixel_format);
        let nv12 = layout("nv12").expect("nv12");
        assert!(!nv12.planar && nv12.container_bits == 8);
        let p010 = layout("p010le").expect("p010");
        assert!(!p010.planar && p010.msb_aligned && p010.container_bits == 16);
        assert!(layout("yuv420p").is_some_and(|l| l.planar && l.container_bits == 8));
        assert!(layout("yuvj420p").is_some());
        assert!(layout("rgb24").is_none());
        assert!(layout("yuva420p").is_none());
        assert!(layout("gbrp").is_some_and(|l| l.planar && l.chroma_shift == (0, 0)));
        assert!(layout("gbrap16le").is_some_and(|l| l.planar && l.container_bits == 16));
        assert!(layout("gbrp16be").is_none());
        assert!(layout("yuv420p10be").is_none());
    }

    #[test]
    fn chroma_offsets_follow_siting() {
        let layout = PlaneLayout::from_surface(HwSurfaceFormat::Nv12);
        let at = |siting| {
            YuvParams::new(
                &colour(Matrix::Bt709, Range::Limited, 8).with_siting(siting),
                layout,
            )
            .chroma_offset
        };
        assert_eq!(at(ChromaSiting::Center), (0.5, 0.5));
        assert_eq!(at(ChromaSiting::Left), (0.0, 0.5));
        assert_eq!(at(ChromaSiting::TopLeft), (0.0, 0.0));
        let p210 = PlaneLayout::from_surface(HwSurfaceFormat::P210);
        let p = YuvParams::new(&colour(Matrix::Bt709, Range::Limited, 10), p210);
        assert_eq!(
            p.chroma_offset,
            (0.0, 0.0),
            "4:2:2 is not subsampled vertically"
        );
    }

    #[test]
    fn uniform_is_80_bytes() {
        let p = YuvParams::new(
            &ColourSpace::BT709,
            PlaneLayout::from_surface(HwSurfaceFormat::Nv12),
        );
        assert_eq!(p.uniform().len(), 80);
    }
}
