//! The text render-graph nodes: glyph coverage, shadow blur and colour.

use wgpu::util::DeviceExt;

use super::atlas::{GlyphAtlas, PreparedText};
use super::{Shadow, TextPaint};
use crate::graph::{Node, NodeContext, TextureDesc};
use crate::renderer::{EffectKey, Uniform};
use crate::shaders;

/// Bytes per glyph instance: 15 `f32`s padded to 16.
const INSTANCE_BYTES: u64 = 64;

/// The instanced glyph-quad pipeline, one per atlas (and so per device).
pub(crate) struct GlyphPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl GlyphPipeline {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text glyphs"),
            source: wgpu::ShaderSource::Wgsl(
                shaders::embedded("text_glyph").unwrap_or_default().into(),
            ),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text glyphs"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text glyphs"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2,
            4 => Float32x2, 5 => Float32x2, 6 => Float32x3,
        ];
        let max = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text glyphs"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_glyph"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: INSTANCE_BYTES,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_glyph"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TextureDesc::WORKING_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: max,
                        alpha: max,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            sampler: linear_sampler(device, "text glyphs"),
        }
    }
}

fn linear_sampler(device: &wgpu::Device, label: &str) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    })
}

impl PreparedText {
    /// The glyph quads' bounds on the canvas after their transforms, as min x, min y,
    /// max x, max y in pixels; `None` when nothing draws.
    pub fn bounds(&self, atlas: &GlyphAtlas) -> Option<[f32; 4]> {
        let mut bounds: Option<[f32; 4]> = None;
        for quad in quads(self, atlas) {
            for corner in quad.corners() {
                let b = bounds.get_or_insert([corner[0], corner[1], corner[0], corner[1]]);
                b[0] = b[0].min(corner[0]);
                b[1] = b[1].min(corner[1]);
                b[2] = b[2].max(corner[0]);
                b[3] = b[3].max(corner[1]);
            }
        }
        bounds
    }
}

/// One glyph quad, including the one-pixel border around its atlas image.
struct Quad {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 2],
    pivot: [f32; 2],
    offset: [f32; 2],
    scale: [f32; 2],
    rotation: f32,
    opacity: f32,
    channel: f32,
}

impl Quad {
    fn corners(&self) -> [[f32; 2]; 4] {
        let (s, c) = self.rotation.sin_cos();
        [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]].map(|[u, v]| {
            let x = (self.pos[0] + u * self.size[0] - self.pivot[0]) * self.scale[0];
            let y = (self.pos[1] + v * self.size[1] - self.pivot[1]) * self.scale[1];
            [
                self.pivot[0] + c * x - s * y + self.offset[0],
                self.pivot[1] + s * x + c * y + self.offset[1],
            ]
        })
    }

    fn write(&self, out: &mut Vec<u8>) {
        let words = [
            self.pos[0],
            self.pos[1],
            self.size[0],
            self.size[1],
            self.uv[0],
            self.uv[1],
            self.pivot[0],
            self.pivot[1],
            self.offset[0],
            self.offset[1],
            self.scale[0],
            self.scale[1],
            self.rotation,
            self.opacity,
            self.channel,
            0.0,
        ];
        for w in words {
            out.extend_from_slice(&w.to_le_bytes());
        }
    }
}

/// The text's quads with their atlas rectangles; glyphs no longer in the atlas are
/// skipped.
fn quads<'a>(text: &'a PreparedText, atlas: &'a GlyphAtlas) -> impl Iterator<Item = Quad> + 'a {
    text.glyphs.iter().filter_map(|g| {
        let e = atlas.entry(&g.key).filter(|e| !e.is_empty())?;
        let t = &g.transform;
        Some(Quad {
            pos: [
                (g.origin[0] + e.left - 1) as f32,
                (g.origin[1] - e.top - 1) as f32,
            ],
            size: [(e.width + 2) as f32, (e.height + 2) as f32],
            uv: [(e.x - 1) as f32, (e.y - 1) as f32],
            pivot: g.pivot,
            offset: t.offset,
            scale: t.scale,
            rotation: t.rotation,
            opacity: t.opacity.min(1.0),
            channel: g.channel as f32,
        })
    })
}

/// Draws a prepared text's glyph quads into a coverage texture: fill coverage in red,
/// outline coverage in green, their union in blue, each scaled by the glyph's opacity.
pub struct GlyphCoverageNode<'a> {
    atlas: &'a GlyphAtlas,
    text: &'a PreparedText,
    size: (u32, u32),
}

impl<'a> GlyphCoverageNode<'a> {
    pub fn new(atlas: &'a GlyphAtlas, text: &'a PreparedText, size: (u32, u32)) -> Self {
        Self { atlas, text, size }
    }
}

impl Node for GlyphCoverageNode<'_> {
    fn label(&self) -> &str {
        "text glyphs"
    }

    fn input_count(&self) -> usize {
        0
    }

    fn outputs(&self, _inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![TextureDesc::working(self.size.0, self.size.1)]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![("glyphs", self.text.glyphs.len().to_string())]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        _inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let device = cx.renderer.device();
        let pipeline = self.atlas.pipeline();
        let mut instances = Vec::with_capacity(self.text.glyphs.len() * INSTANCE_BYTES as usize);
        for quad in quads(self.text, self.atlas) {
            quad.write(&mut instances);
        }
        let count = (instances.len() as u64 / INSTANCE_BYTES) as u32;
        let atlas_size = self.atlas.size() as f32;
        let uniform = Uniform::new()
            .f32(self.size.0 as f32)
            .f32(self.size.1 as f32)
            .f32(atlas_size)
            .f32(atlas_size);
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("text glyphs"),
            contents: uniform.bytes(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text glyphs"),
            layout: &pipeline.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(self.atlas.view()),
                },
            ],
        });
        let buffer = (count > 0).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("text glyph instances"),
                contents: &instances,
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let timestamps = cx.timestamps();
        let mut pass = cx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("text glyphs"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: outputs[0],
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(buffer) = &buffer {
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..6, 0..count);
        }
    }
}

/// Uniform words shared by the passes in `text.wgsl`.
fn text_uniform(
    paint: Option<&TextPaint>,
    shadow: Option<&Shadow>,
    size: (u32, u32),
    radius: u32,
) -> Uniform {
    let zero = [0.0; 4];
    let outline = paint.and_then(|p| p.outline).map_or(zero, |o| o.color);
    let shadow_color = paint
        .and_then(|p| p.shadow)
        .or(shadow.copied())
        .map_or(zero, |s| s.color);
    let offset = shadow.map_or([0.0; 2], |s| s.offset);
    Uniform::new()
        .vec4(paint.map_or(zero, |p| p.fill))
        .vec4(outline)
        .vec4(shadow_color)
        .f32(offset[0])
        .f32(offset[1])
        .f32(size.0 as f32)
        .f32(size.1 as f32)
        .f32(shadow.map_or(0.0, |s| s.blur.max(0.0)))
        .f32(radius as f32)
        .align16()
}

/// Draws `entry` of `text.wgsl` as a full-screen pass that clears `target` and shades only
/// `region` (min x, min y, max x, max y in pixels; the whole target when `None`).
fn draw_text_pass(
    cx: &mut NodeContext<'_>,
    label: &str,
    entry: &'static str,
    uniform: &Uniform,
    inputs: &[&wgpu::TextureView],
    target: &wgpu::TextureView,
    region: Option<[u32; 4]>,
) {
    let device = cx.renderer.device();
    let pipeline = cx.renderer.pipeline(EffectKey {
        shader: "text",
        entry,
        format: target.texture().format(),
        inputs: inputs.len() as u8,
        volumes: 0,
    });
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: uniform.bytes(),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let sampler = linear_sampler(device, label);
    let mut entries = vec![
        wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::Sampler(&sampler),
        },
    ];
    entries.extend(
        inputs
            .iter()
            .zip(2u32..)
            .map(|(view, binding)| wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(view),
            }),
    );
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &entries,
    });
    let timestamps = cx.timestamps();
    let mut pass = cx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: timestamps,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    let (w, h) = (target.texture().width(), target.texture().height());
    let [x0, y0, x1, y1] = region.unwrap_or([0, 0, w, h]);
    let (x1, y1) = (x1.min(w), y1.min(h));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
    pass.set_pipeline(&pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Horizontal,
    Vertical,
}

/// One direction of the shadow's separable Gaussian blur. The horizontal pass reads the
/// coverage union, shifted by the shadow offset; the vertical pass reads its result. Both
/// shade only the text's bounds (shifted, and grown by the kernel radius) when known.
pub struct ShadowBlurNode {
    shadow: Shadow,
    direction: Direction,
    bounds: Option<[f32; 4]>,
}

impl ShadowBlurNode {
    /// The largest kernel radius in pixels.
    pub const MAX_RADIUS: u32 = 96;

    /// The blurred shadow is one channel.
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

    pub fn horizontal(shadow: Shadow) -> Self {
        Self {
            shadow,
            direction: Direction::Horizontal,
            bounds: None,
        }
    }

    pub fn vertical(shadow: Shadow) -> Self {
        Self {
            shadow,
            direction: Direction::Vertical,
            bounds: None,
        }
    }

    /// Limits the pass to the shadow of text within `bounds` (see
    /// [`PreparedText::bounds`]).
    pub fn with_bounds(mut self, bounds: Option<[f32; 4]>) -> Self {
        self.bounds = bounds;
        self
    }

    /// Three standard deviations, rounded up.
    pub fn radius(&self) -> u32 {
        ((self.shadow.blur.max(0.0) * 3.0).ceil() as u32).min(Self::MAX_RADIUS)
    }

    fn region(&self) -> Option<[u32; 4]> {
        let [x0, y0, x1, y1] = self.bounds?;
        let r = self.radius() as f32 + 2.0;
        let [dx, dy] = self.shadow.offset;
        Some([
            (x0 + dx - r).floor().max(0.0) as u32,
            (y0 + dy - r).floor().max(0.0) as u32,
            (x1 + dx + r).ceil().max(0.0) as u32,
            (y1 + dy + r).ceil().max(0.0) as u32,
        ])
    }
}

impl Node for ShadowBlurNode {
    fn label(&self) -> &str {
        match self.direction {
            Direction::Horizontal => "text shadow h",
            Direction::Vertical => "text shadow v",
        }
    }

    fn input_count(&self) -> usize {
        1
    }

    fn outputs(&self, inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![inputs[0].with_format(Self::FORMAT)]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![
            ("offset", format!("{:?}", self.shadow.offset)),
            ("blur", self.shadow.blur.to_string()),
        ]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let texture = outputs[0].texture();
        let size = (texture.width(), texture.height());
        let uniform = text_uniform(None, Some(&self.shadow), size, self.radius());
        let entry = match self.direction {
            Direction::Horizontal => "fs_shadow_h",
            Direction::Vertical => "fs_shadow_v",
        };
        draw_text_pass(
            cx,
            self.label(),
            entry,
            &uniform,
            inputs,
            outputs[0],
            self.region(),
        );
    }
}

/// Colours a text coverage texture: fill over outline over (with a second input) the
/// blurred shadow, premultiplied linear light on transparent black.
pub struct TextCompositeNode {
    paint: TextPaint,
}

impl TextCompositeNode {
    pub fn new(paint: &TextPaint) -> Self {
        Self {
            paint: paint.clone(),
        }
    }
}

impl Node for TextCompositeNode {
    fn label(&self) -> &str {
        "text fill"
    }

    fn input_count(&self) -> usize {
        if self.paint.shadow.is_some() { 2 } else { 1 }
    }

    fn outputs(&self, inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![inputs[0].with_format(TextureDesc::WORKING_FORMAT)]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![("fill", format!("{:?}", self.paint.fill))]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let texture = outputs[0].texture();
        let size = (texture.width(), texture.height());
        let uniform = text_uniform(Some(&self.paint), self.paint.shadow.as_ref(), size, 0);
        let entry = if inputs.len() > 1 {
            "fs_paint_shadow"
        } else {
            "fs_paint"
        };
        draw_text_pass(cx, "text fill", entry, &uniform, inputs, outputs[0], None);
    }
}
