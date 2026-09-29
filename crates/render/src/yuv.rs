//! Y'CbCr to RGB on the GPU for biplanar frames (NV12, P010 and their 4:2:2 and 4:4:4
//! siblings): one full-screen pass that samples the luma and chroma planes and applies the
//! frame's matrix (BT.601, BT.709, BT.2020) and range (limited or full) at its bit depth.
//!
//! The output holds the encoded R'G'B' values (no transfer function is undone yet), and
//! chroma is sampled as if centred between luma samples, as swscale does, so the GPU
//! matches `media`'s CPU conversion; transfer functions and chroma siting arrive with the
//! colour-space plumbing task.

use media::{ColourInfo, ColourRange, HwFrame, HwSurfaceFormat};
use wgpu::util::DeviceExt;

const SHADER: &str = r#"
struct Params {
    // Rows of the 3x4 matrix taking (Y, Cb, Cr, 1) samples, as the texture returns them,
    // to R'G'B'.
    r: vec4<f32>,
    g: vec4<f32>,
    b: vec4<f32>,
}

@group(0) @binding(0) var luma: texture_2d<f32>;
@group(0) @binding(1) var chroma: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: Params;

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

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let y = textureSampleLevel(luma, samp, in.uv, 0.0).r;
    let c = textureSampleLevel(chroma, samp, in.uv, 0.0).rg;
    let v = vec4<f32>(y, c, 1.0);
    let rgb = vec3<f32>(dot(params.r, v), dot(params.g, v), dot(params.b, v));
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

/// The Y'CbCr to R'G'B' matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl YuvMatrix {
    /// From FFmpeg's matrix name; untagged video is taken as BT.709 above 576 lines and
    /// BT.601 at or below, as `media`'s CPU conversion does.
    pub fn from_colour(colour: &ColourInfo, height: u32) -> Self {
        match colour.matrix {
            Some("bt709") => Self::Bt709,
            Some("bt2020nc" | "bt2020c") => Self::Bt2020,
            Some("smpte170m" | "bt470bg" | "fcc") => Self::Bt601,
            _ if height > 576 => Self::Bt709,
            _ => Self::Bt601,
        }
    }

    /// (Kr, Kb).
    fn weights(self) -> (f32, f32) {
        match self {
            Self::Bt601 => (0.299, 0.114),
            Self::Bt709 => (0.2126, 0.0722),
            Self::Bt2020 => (0.2627, 0.0593),
        }
    }
}

/// How to read a biplanar frame's samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YuvParams {
    pub matrix: YuvMatrix,
    pub full_range: bool,
    /// Significant bits per sample (8 or 10).
    pub bit_depth: u8,
    /// Bits of the texture format holding a sample (8 for `R8Unorm`, 16 for `R16Unorm`
    /// holding 10-bit samples in its high bits).
    pub container_bits: u8,
    /// log2 of the chroma subsampling.
    pub chroma_shift: (u8, u8),
}

impl YuvParams {
    /// Parameters for a hardware frame. A range tag in the bitstream wins over the
    /// surface's; untagged video uses the surface's.
    pub fn for_frame(frame: &HwFrame) -> Self {
        Self::new(
            &frame.colour(),
            frame.format(),
            frame.is_full_range(),
            frame.height(),
        )
    }

    pub fn new(
        colour: &ColourInfo,
        format: HwSurfaceFormat,
        surface_full_range: bool,
        height: u32,
    ) -> Self {
        let full_range = match colour.range {
            ColourRange::Full => true,
            ColourRange::Limited => false,
            ColourRange::Unspecified => surface_full_range,
        };
        Self {
            matrix: YuvMatrix::from_colour(colour, height),
            full_range,
            bit_depth: format.bit_depth(),
            container_bits: format.bytes_per_sample() * 8,
            chroma_shift: format.chroma_shift(),
        }
    }

    /// Rows of the matrix taking normalised texture samples (Y, Cb, Cr, 1) to R'G'B'.
    pub fn matrix_rows(&self) -> [[f32; 4]; 3] {
        let n = f32::from(self.bit_depth);
        let max = 2f32.powf(n) - 1.0;
        let container_max = 2f32.powi(i32::from(self.container_bits)) - 1.0;
        // A sample `s` from the texture holds code value `s * container_max / 2^shift`,
        // where `shift` puts `bit_depth` bits at the top of the container.
        let shift = 2f32.powi(i32::from(self.container_bits - self.bit_depth));
        let to_code = container_max / shift;
        let scale = 2f32.powf(n - 8.0);
        let (y_off, y_range, c_off, c_range) = if self.full_range {
            (0.0, max, 128.0 * scale, max)
        } else {
            (16.0 * scale, 219.0 * scale, 128.0 * scale, 224.0 * scale)
        };
        // Y' = (s_y * to_code - y_off) / y_range, likewise for Cb and Cr.
        let ys = to_code / y_range;
        let yo = -y_off / y_range;
        let cs = to_code / c_range;
        let co = -c_off / c_range;
        let (kr, kb) = self.matrix.weights();
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
    }

    fn uniform(&self) -> [f32; 12] {
        let rows = self.matrix_rows();
        let mut out = [0.0; 12];
        for (i, row) in rows.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(row);
        }
        out
    }
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
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

    /// Records the conversion of `luma` and `chroma` (a biplanar frame) into `target`,
    /// which must be one of [`TARGET_FORMATS`]; the frame is scaled to the target's size. Returns `false` for an unsupported target format.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        luma: &wgpu::TextureView,
        chroma: &wgpu::TextureView,
        params: &YuvParams,
        target: &wgpu::TextureView,
        target_format: wgpu::TextureFormat,
    ) -> bool {
        let Some((_, pipeline)) = self.pipelines.iter().find(|(f, _)| *f == target_format) else {
            return false;
        };
        let uniform = params.uniform();
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuv params"),
            contents: &uniform
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv planes"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(luma),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(chroma),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
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

    fn colour(matrix: Option<&'static str>, range: ColourRange) -> ColourInfo {
        ColourInfo {
            primaries: None,
            transfer: None,
            matrix,
            range,
            chroma_location: None,
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
            &colour(Some("bt709"), ColourRange::Limited),
            HwSurfaceFormat::Nv12,
            false,
            1080,
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
        let p = YuvParams::new(
            &colour(Some("bt2020nc"), ColourRange::Unspecified),
            HwSurfaceFormat::P010,
            false,
            2160,
        );
        assert_eq!(p.matrix, YuvMatrix::Bt2020);
        let rows = p.matrix_rows();
        // 10-bit codes in the top bits of a 16-bit sample.
        let s = |code: f32| code * 64.0 / 65535.0;
        close(apply(&rows, s(64.0), s(512.0), s(512.0)), [0.0, 0.0, 0.0]);
        close(apply(&rows, s(940.0), s(512.0), s(512.0)), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn full_range_and_untagged_matrices() {
        let p = YuvParams::new(
            &colour(None, ColourRange::Unspecified),
            HwSurfaceFormat::Nv12,
            true,
            480,
        );
        assert!(p.full_range);
        assert_eq!(p.matrix, YuvMatrix::Bt601);
        let rows = p.matrix_rows();
        close(
            apply(&rows, 1.0, 128.0 / 255.0, 128.0 / 255.0),
            [1.0, 1.0, 1.0],
        );
        close(
            apply(&rows, 0.0, 128.0 / 255.0, 128.0 / 255.0),
            [0.0, 0.0, 0.0],
        );
    }
}
