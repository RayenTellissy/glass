//! The GPU half of shapes: outlines packed into an accumulation atlas and binned into
//! sparse strips ([`super::strips`]), the passes that accumulate and paint them
//! ([`ShapeNode`]), and single-outline coverage for masks ([`CoverageNode`]). The byte
//! layouts here mirror the structs in `shape.wgsl`.

use std::ops::Range;

use wgpu::util::DeviceExt;

use super::strips::{self, Piece};
use super::{Affine, FillRule, Outline, Paint, Shape};
use crate::graph::{Node, NodeContext, TextureDesc};
use crate::renderer::{Renderer, Uniform};

/// The accumulation texture format: `R32Float` when the device can blend it (it was
/// created with `wgpu::Features::FLOAT32_BLENDABLE`, which every Apple GPU has), else
/// `R16Float`, whose rounding leaves up to about 1e-3 of coverage behind a shape's edges.
pub fn accumulation_format(device: &wgpu::Device) -> wgpu::TextureFormat {
    if device
        .features()
        .contains(wgpu::Features::FLOAT32_BLENDABLE)
    {
        wgpu::TextureFormat::R32Float
    } else {
        wgpu::TextureFormat::R16Float
    }
}

/// Atlas heights are rounded up to this so animated shapes reuse pooled textures.
const ATLAS_ROWS: u32 = 256;

/// Atlas textures one paint pass reads (bindings 4 to 7 of `shape.wgsl`); fills beyond
/// that reuse them after a paint pass.
const MAX_ATLASES: usize = 4;

const PIECE_BYTES: usize = 48;
const ITEM_BYTES: usize = 96;
const STOP_BYTES: usize = 32;

/// One outline placed on the canvas and in the atlas, with its paint.
#[derive(Clone, Debug)]
struct Item {
    /// Canvas box: x, y, width, height in whole pixels.
    rect: [u32; 4],
    atlas: [u32; 2],
    /// Which atlas fill holds it.
    fill: u32,
    rule: FillRule,
    kind: u32,
    /// Canvas to paint space.
    inverse: Affine,
    stops: [u32; 2],
    grad: [f32; 4],
    color: [f32; 4],
}

/// The items, accumulate pieces and paint pieces of one atlas fill.
#[derive(Clone, Debug, Default, PartialEq)]
struct Batch {
    items: Range<u32>,
    edges: Range<u32>,
    spans: Range<u32>,
}

/// Everything the passes read, built on the CPU when the node is made.
#[derive(Clone, Debug, Default)]
struct Prepared {
    items: Vec<Item>,
    /// Accumulate pieces (an edge's strip in a band), grouped by item.
    edges: Vec<Piece>,
    /// Paint pieces (intervals and covered gaps of each band), grouped by item.
    spans: Vec<Piece>,
    /// Outline edges binned.
    edge_count: usize,
    stops: Vec<([f32; 4], f32)>,
    /// Each atlas fill, in paint order.
    batches: Vec<Batch>,
    atlas: (u32, u32),
}

fn premultiply(c: [f32; 4]) -> [f32; 4] {
    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
}

/// The whole pixels of `outline`'s bounds that lie on the canvas.
fn canvas_box(outline: &Outline, canvas: (u32, u32)) -> Option<[u32; 4]> {
    let (lo, hi) = outline.bounds()?;
    let x0 = lo.x.floor().max(0.0);
    let y0 = lo.y.floor().max(0.0);
    let x1 = hi.x.ceil().min(canvas.0 as f32);
    let y1 = hi.y.ceil().min(canvas.1 as f32);
    if !(x1 > x0 && y1 > y0) {
        return None;
    }
    Some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
}

/// Shelf packing of one atlas fill.
#[derive(Clone, Copy, Debug)]
struct Packer {
    size: (u32, u32),
    x: u32,
    y: u32,
    shelf: u32,
}

impl Packer {
    fn new(size: (u32, u32)) -> Self {
        Self {
            size,
            x: 0,
            y: 0,
            shelf: 0,
        }
    }

    /// The spot for a `w` x `h` box on the current or a new shelf, if the fill has room.
    fn place(&mut self, w: u32, h: u32) -> Option<[u32; 2]> {
        let mut next = *self;
        if next.x + w > next.size.0 {
            next.x = 0;
            next.y += next.shelf;
            next.shelf = 0;
        }
        if next.y + h > next.size.1 || w > next.size.0 {
            return None;
        }
        let at = [next.x, next.y];
        next.x += w;
        next.shelf = next.shelf.max(h);
        *self = next;
        Some(at)
    }
}

impl Prepared {
    /// Places item `index` in the current atlas fill: after the last one, else by
    /// repacking the fill tallest first, else in a new fill. Fills stay contiguous in
    /// paint order.
    fn place(&mut self, index: usize, packer: &mut Packer) {
        let [_, _, w, h] = self.items[index].rect;
        let fill = self.batches.len().saturating_sub(1) as u32;
        if !self.batches.is_empty() {
            if let Some(at) = packer.place(w, h) {
                self.items[index].atlas = at;
                self.items[index].fill = fill;
                return;
            }
            let first = self.batches[fill as usize].items.start as usize;
            let mut order: Vec<usize> = (first..=index).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(self.items[i].rect[3]));
            let mut repack = Packer::new(packer.size);
            let spots: Option<Vec<[u32; 2]>> = order
                .iter()
                .map(|&i| repack.place(self.items[i].rect[2], self.items[i].rect[3]))
                .collect();
            if let Some(spots) = spots {
                for (&i, at) in order.iter().zip(spots) {
                    self.items[i].atlas = at;
                }
                self.items[index].fill = fill;
                *packer = repack;
                return;
            }
        }
        *packer = Packer::new(packer.size);
        let at = packer.place(w, h).unwrap_or_default();
        self.items[index].atlas = at;
        self.items[index].fill = self.batches.len() as u32;
        let (edges, spans) = (self.edges.len() as u32, self.spans.len() as u32);
        self.batches.push(Batch {
            items: index as u32..index as u32,
            edges: edges..edges,
            spans: spans..spans,
        });
    }

    fn push(
        &mut self,
        outline: &Outline,
        paint: &Paint,
        transform: &Affine,
        canvas: (u32, u32),
        packer: &mut Packer,
    ) {
        let Some(rect) = canvas_box(outline, canvas) else {
            return;
        };
        let index = self.items.len();
        let first_stop = self.stops.len() as u32;
        let sorted = paint.sorted_stops();
        self.stops
            .extend(sorted.iter().map(|s| (premultiply(s.color), s.offset)));
        let (kind, grad, color) = match paint {
            Paint::Solid(c) => (0, [0.0; 4], premultiply(*c)),
            Paint::Linear { start, end, .. } => (1, [start.x, start.y, end.x, end.y], [0.0; 4]),
            Paint::Radial { center, radius, .. } => {
                (2, [center.x, center.y, *radius, 0.0], [0.0; 4])
            }
        };
        self.items.push(Item {
            rect,
            atlas: [0, 0],
            fill: 0,
            rule: outline.rule,
            kind,
            // A collapsed transform has no area to paint; identity keeps the maths finite.
            inverse: transform.inverse().unwrap_or_default(),
            stops: [first_stop, sorted.len() as u32],
            grad,
            color,
        });
        self.place(index, packer);
        let (ox, oy) = (rect[0] as f32, rect[1] as f32);
        let edges: Vec<[f32; 4]> = outline
            .edges()
            .map(|(a, b)| [a.x - ox, a.y - oy, b.x - ox, b.y - oy])
            .collect();
        self.edge_count += edges.len();
        strips::bin(
            &edges,
            rect,
            outline.rule,
            index as u32,
            &mut self.edges,
            &mut self.spans,
        );
        if let Some(batch) = self.batches.last_mut() {
            batch.items.end = index as u32 + 1;
            batch.edges.end = self.edges.len() as u32;
            batch.spans.end = self.spans.len() as u32;
        }
    }

    /// Rows of the atlas any fill uses.
    fn rows(&self) -> u32 {
        self.items
            .iter()
            .map(|i| i.atlas[1] + i.rect[3])
            .max()
            .unwrap_or(0)
    }

    /// The accumulate pieces, then the paint pieces (instance `edges.len()` onwards).
    fn piece_bytes(&self) -> Vec<u8> {
        let count = self.edges.len() + self.spans.len();
        let mut out = vec![0; count.max(1) * PIECE_BYTES];
        let pieces = self.edges.iter().chain(&self.spans);
        for (bytes, piece) in out.as_chunks_mut::<PIECE_BYTES>().0.iter_mut().zip(pieces) {
            let [r0, r1, r2, r3] = piece.rect.map(f32::to_bits);
            let [d0, d1, d2, d3] = piece.data.map(f32::to_bits);
            let words = [r0, r1, r2, r3, d0, d1, d2, d3, piece.item, piece.kind];
            for (b, w) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
                *b = w.to_le_bytes();
            }
        }
        out
    }

    /// Fragments the accumulate passes and the paint passes shade.
    fn fragments(&self) -> (u64, u64) {
        let area = |pieces: &[Piece]| pieces.iter().map(Piece::area).sum();
        (area(&self.edges), area(&self.spans))
    }

    fn item_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.items.len().max(1) * ITEM_BYTES);
        for item in &self.items {
            let [a, b, c, d, e, f] = item.inverse.0;
            let floats = |out: &mut Vec<u8>, v: &[f32]| {
                for x in v {
                    out.extend_from_slice(&x.to_le_bytes());
                }
            };
            floats(&mut out, &item.rect.map(|v| v as f32));
            floats(&mut out, &item.atlas.map(|v| v as f32));
            let slot = item.fill % MAX_ATLASES as u32;
            out.extend_from_slice(&(item.rule.code() | slot << 8).to_le_bytes());
            out.extend_from_slice(&item.kind.to_le_bytes());
            floats(&mut out, &[a, b, c, d, e, f]);
            out.extend_from_slice(&item.stops[0].to_le_bytes());
            out.extend_from_slice(&item.stops[1].to_le_bytes());
            floats(&mut out, &item.grad);
            floats(&mut out, &item.color);
        }
        out.resize(out.len().max(ITEM_BYTES), 0);
        out
    }

    fn stop_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.stops.len().max(1) * STOP_BYTES);
        for (color, offset) in &self.stops {
            for v in color.iter().chain([offset, &0.0, &0.0, &0.0]) {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out.resize(out.len().max(STOP_BYTES), 0);
        out
    }
}

/// The buffers and bind groups of one node's passes.
struct Bound {
    uniform: wgpu::Buffer,
    pieces: wgpu::Buffer,
    items: wgpu::Buffer,
    stops: wgpu::Buffer,
}

impl Bound {
    fn new(device: &wgpu::Device, prepared: &Prepared, canvas: (u32, u32)) -> Self {
        let storage = |label, contents: &[u8]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: wgpu::BufferUsages::STORAGE,
            })
        };
        let uniform = Uniform::new()
            .f32(canvas.0 as f32)
            .f32(canvas.1 as f32)
            .f32(prepared.atlas.0 as f32)
            .f32(prepared.atlas.1 as f32)
            .align16();
        Self {
            uniform: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("shape globals"),
                contents: uniform.bytes(),
                usage: wgpu::BufferUsages::UNIFORM,
            }),
            pieces: storage("shape pieces", &prepared.piece_bytes()),
            items: storage("shape items", &prepared.item_bytes()),
            stops: storage("shape stops", &prepared.stop_bytes()),
        }
    }

    fn group(
        &self,
        device: &wgpu::Device,
        pipeline: &wgpu::RenderPipeline,
        atlases: &[&wgpu::TextureView],
        dummy: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let buffers = [&self.uniform, &self.pieces, &self.items, &self.stops];
        let mut entries: Vec<wgpu::BindGroupEntry<'_>> = buffers
            .iter()
            .zip(0u32..)
            .map(|(b, binding)| wgpu::BindGroupEntry {
                binding,
                resource: b.as_entire_binding(),
            })
            .collect();
        entries.extend((0..MAX_ATLASES).map(|i| wgpu::BindGroupEntry {
            binding: 4 + i as u32,
            resource: wgpu::BindingResource::TextureView(atlases.get(i).copied().unwrap_or(dummy)),
        }));
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shape"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        })
    }
}

fn layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let both = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
    let buffer = |binding, ty| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: both,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let storage = wgpu::BufferBindingType::Storage { read_only: true };
    let mut entries = vec![
        buffer(0, wgpu::BufferBindingType::Uniform),
        buffer(1, storage),
        buffer(2, storage),
        buffer(3, storage),
    ];
    entries.extend((0..MAX_ATLASES as u32).map(|i| wgpu::BindGroupLayoutEntry {
        binding: 4 + i,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }));
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("shape"),
        entries: &entries,
    })
}

/// The pipeline for `vertex`/`fragment` of `shape.wgsl` writing `format` with `blend`.
fn pipeline(
    renderer: &Renderer,
    name: &'static str,
    (vertex, fragment): (&'static str, &'static str),
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    renderer.custom_pipeline("shape", name, format, |device, module| {
        let layout = layout(device);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(name),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(name),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some(vertex),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(fragment),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    })
}

const ADD: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
};

/// Splits the node's timestamp writes over its first and last pass.
struct Timestamps<'a> {
    first: Option<wgpu::RenderPassTimestampWrites<'a>>,
    last: Option<wgpu::RenderPassTimestampWrites<'a>>,
}

impl<'a> Timestamps<'a> {
    fn new(t: Option<wgpu::RenderPassTimestampWrites<'a>>) -> Self {
        Self {
            first: t.as_ref().map(|t| wgpu::RenderPassTimestampWrites {
                query_set: t.query_set,
                beginning_of_pass_write_index: t.beginning_of_pass_write_index,
                end_of_pass_write_index: None,
            }),
            last: t.map(|t| wgpu::RenderPassTimestampWrites {
                beginning_of_pass_write_index: None,
                ..t
            }),
        }
    }

    /// The writes for a pass; `last` marks the node's final pass.
    fn pass(&mut self, last: bool) -> Option<wgpu::RenderPassTimestampWrites<'a>> {
        match (self.first.take(), last) {
            (Some(mut f), true) => {
                f.end_of_pass_write_index =
                    self.last.take().and_then(|l| l.end_of_pass_write_index);
                Some(f)
            }
            (Some(f), false) => Some(f),
            (None, true) => self.last.take(),
            (None, false) => None,
        }
    }
}

fn begin_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    label: &str,
    target: &wgpu::TextureView,
    clear: bool,
    timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if clear {
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: timestamps,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

fn accumulate(
    cx: &mut NodeContext<'_>,
    bound: &Bound,
    acc: &wgpu::TextureView,
    edges: Range<u32>,
    timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
) {
    let format = acc.texture().format();
    let pipeline = pipeline(
        cx.renderer,
        "shape accumulate",
        ("vs_accumulate", "fs_accumulate"),
        format,
        Some(ADD),
    );
    let dummy = cx.renderer.dummy_view();
    let group = bound.group(cx.renderer.device(), &pipeline, &[], dummy);
    let mut pass = begin_pass(cx.encoder, "shape accumulate", acc, true, timestamps);
    pass.set_pipeline(&pipeline);
    pass.set_bind_group(0, &group, &[]);
    if !edges.is_empty() {
        pass.draw(0..6, edges);
    }
}

/// Shapes drawn in order, source-over, into a transparent `Rgba16Float` layer the size
/// of the canvas (linear light, premultiplied). Outputs: the layer, then one to four
/// accumulation atlases (scratch, not meant to be read: canvas-wide, their height rounded
/// up to 256 rows, written only where edges cross). Outlines are packed into atlas fills,
/// each one accumulate pass; one paint pass then draws every outline of up to four fills,
/// so the layer is written once unless the outlines need more than four canvases' worth
/// of atlas.
#[derive(Clone, Debug)]
pub struct ShapeNode {
    canvas: (u32, u32),
    format: wgpu::TextureFormat,
    shapes: usize,
    prepared: Prepared,
}

impl ShapeNode {
    /// Flattens, strokes and packs `shapes` (shape coordinates map to canvas pixels
    /// through each shape's transform; parts off the canvas are clipped).
    pub fn new(renderer: &Renderer, canvas: (u32, u32), shapes: &[Shape]) -> Self {
        let canvas = (canvas.0.max(1), canvas.1.max(1));
        let mut prepared = Prepared::default();
        let mut packer = Packer::new(canvas);
        for shape in shapes {
            if let (Some(outline), Some(fill)) = (shape.fill_outline(), &shape.fill) {
                prepared.push(&outline, &fill.paint, &shape.transform, canvas, &mut packer);
            }
            if let (Some(outline), Some(stroke)) = (shape.stroke_outline(), &shape.stroke) {
                prepared.push(
                    &outline,
                    &stroke.paint,
                    &shape.transform,
                    canvas,
                    &mut packer,
                );
            }
        }
        let rows = prepared
            .rows()
            .next_multiple_of(ATLAS_ROWS)
            .clamp(1, canvas.1);
        prepared.atlas = (canvas.0, rows);
        Self {
            canvas,
            format: accumulation_format(renderer.device()),
            shapes: shapes.len(),
            prepared,
        }
    }

    /// Outlines drawn (one per fill and per stroke that reaches the canvas).
    pub fn outline_count(&self) -> usize {
        self.prepared.items.len()
    }

    pub fn edge_count(&self) -> usize {
        self.prepared.edge_count
    }

    /// Fragments the accumulate passes shade (each edge's strip to the end of its
    /// interval, per band).
    pub fn accumulate_fragments(&self) -> u64 {
        self.prepared.fragments().0
    }

    /// Fragments the paint passes shade (intervals, and the gaps between them that have
    /// coverage).
    pub fn paint_fragments(&self) -> u64 {
        self.prepared.fragments().1
    }

    /// Atlas fills, each an accumulate pass.
    pub fn batch_count(&self) -> usize {
        self.prepared.batches.len()
    }
}

impl Node for ShapeNode {
    fn label(&self) -> &str {
        "shape"
    }

    fn input_count(&self) -> usize {
        0
    }

    fn outputs(&self, _inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        let (w, h) = self.prepared.atlas;
        let atlas = TextureDesc {
            width: w,
            height: h,
            format: self.format,
        };
        let atlases = self.batch_count().clamp(1, MAX_ATLASES);
        let mut out = vec![TextureDesc::working(self.canvas.0, self.canvas.1)];
        out.extend(std::iter::repeat_n(atlas, atlases));
        out
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![
            ("shapes", self.shapes.to_string()),
            ("outlines", self.outline_count().to_string()),
            ("edges", self.edge_count().to_string()),
            ("pieces", self.prepared.spans.len().to_string()),
            ("batches", self.batch_count().to_string()),
        ]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        _inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let (layer, atlases) = (outputs[0], &outputs[1..]);
        let bound = Bound::new(cx.renderer.device(), &self.prepared, self.canvas);
        let mut timestamps = Timestamps::new(cx.timestamps());
        let paint = pipeline(
            cx.renderer,
            "shape paint",
            ("vs_paint", "fs_paint"),
            layer.texture().format(),
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        let batches = &self.prepared.batches;
        if batches.is_empty() {
            begin_pass(
                cx.encoder,
                "shape paint",
                layer,
                true,
                timestamps.pass(true),
            );
            return;
        }
        let dummy = cx.renderer.dummy_view();
        let groups = batches.chunks(MAX_ATLASES).count();
        for (g, group) in batches.chunks(MAX_ATLASES).enumerate() {
            for (batch, atlas) in group.iter().zip(atlases) {
                accumulate(
                    cx,
                    &bound,
                    atlas,
                    batch.edges.clone(),
                    timestamps.pass(false),
                );
            }
            // Paint pieces follow the accumulate pieces in the buffer.
            let offset = self.prepared.edges.len() as u32;
            let spans = group[0].spans.start + offset..group[group.len() - 1].spans.end + offset;
            let bind = bound.group(cx.renderer.device(), &paint, atlases, dummy);
            let mut pass = begin_pass(
                cx.encoder,
                "shape paint",
                layer,
                g == 0,
                timestamps.pass(g + 1 == groups),
            );
            pass.set_pipeline(&paint);
            pass.set_bind_group(0, &bind, &[]);
            if !spans.is_empty() {
                pass.draw(0..6, spans);
            }
        }
    }
}

/// One outline's coverage (0 to 1 after its fill rule) in the red channel of an
/// `R16Float` texture the size of the canvas, for masks. Outputs: the coverage, and the
/// canvas-sized accumulation texture (scratch, written only where edges cross).
#[derive(Clone, Debug)]
pub struct CoverageNode {
    canvas: (u32, u32),
    format: wgpu::TextureFormat,
    rule: FillRule,
    prepared: Prepared,
}

impl CoverageNode {
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

    pub fn new(renderer: &Renderer, canvas: (u32, u32), outline: &Outline) -> Self {
        let canvas = (canvas.0.max(1), canvas.1.max(1));
        let mut prepared = Prepared {
            atlas: canvas,
            ..Default::default()
        };
        if let Some(rect) = canvas_box(outline, canvas) {
            prepared.push(
                outline,
                &Paint::Solid([1.0; 4]),
                &Affine::IDENTITY,
                canvas,
                &mut Packer::new(canvas),
            );
            // The accumulation is canvas-sized, so the box sits at its own position.
            prepared.items[0].atlas = [rect[0], rect[1]];
        }
        Self {
            canvas,
            format: accumulation_format(renderer.device()),
            rule: outline.rule,
            prepared,
        }
    }
}

impl Node for CoverageNode {
    fn label(&self) -> &str {
        "coverage"
    }

    fn input_count(&self) -> usize {
        0
    }

    fn outputs(&self, _inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        let (w, h) = self.canvas;
        vec![
            TextureDesc {
                width: w,
                height: h,
                format: Self::FORMAT,
            },
            TextureDesc {
                width: w,
                height: h,
                format: self.format,
            },
        ]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![
            ("rule", format!("{:?}", self.rule)),
            ("edges", self.prepared.edge_count.to_string()),
        ]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        _inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let (coverage, acc) = (outputs[0], outputs[1]);
        let bound = Bound::new(cx.renderer.device(), &self.prepared, self.canvas);
        let mut timestamps = Timestamps::new(cx.timestamps());
        let edges = self.prepared.edges.len() as u32;
        accumulate(cx, &bound, acc, 0..edges, timestamps.pass(false));
        // The pieces do not overlap, and pixels outside them stay cleared to zero.
        let cover = pipeline(
            cx.renderer,
            "shape cover",
            ("vs_paint", "fs_cover"),
            coverage.texture().format(),
            None,
        );
        let group = bound.group(
            cx.renderer.device(),
            &cover,
            &[acc],
            cx.renderer.dummy_view(),
        );
        let mut pass = begin_pass(
            cx.encoder,
            "shape cover",
            coverage,
            true,
            timestamps.pass(true),
        );
        pass.set_pipeline(&cover);
        pass.set_bind_group(0, &group, &[]);
        let spans = edges..edges + self.prepared.spans.len() as u32;
        if !spans.is_empty() {
            pass.draw(0..6, spans);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{Geometry, Rect};

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Shape {
        Shape::new(Geometry::Rect {
            rect: Rect::new(x, y, w, h),
            radius: 0.0,
        })
        .filled(Paint::Solid([1.0; 4]))
    }

    fn prepare(canvas: (u32, u32), shapes: &[Shape]) -> Prepared {
        let mut prepared = Prepared::default();
        let mut packer = Packer::new(canvas);
        for s in shapes {
            let o = s.fill_outline().expect("filled");
            prepared.push(
                &o,
                &Paint::Solid([1.0; 4]),
                &s.transform,
                canvas,
                &mut packer,
            );
        }
        prepared
    }

    #[test]
    fn boxes_are_clipped_to_the_canvas() {
        let p = prepare(
            (100, 50),
            &[rect(-10.5, 40.2, 30.0, 30.0), rect(200.0, 0.0, 5.0, 5.0)],
        );
        assert_eq!(p.items.len(), 1, "off-canvas shapes are dropped");
        assert_eq!(p.items[0].rect, [0, 40, 20, 10]);
        assert_eq!(p.edge_count, 2, "horizontal edges are skipped");
        assert_eq!(
            p.edges.len(),
            2,
            "one strip per side, merged down the bands"
        );
        assert_eq!(p.edges[0].data[0], -10.5, "left edge, in box coordinates");
    }

    #[test]
    fn packing_keeps_paint_order_across_fills() {
        let shapes: Vec<Shape> = (0..5).map(|i| rect(i as f32, 0.0, 60.0, 30.0)).collect();
        let p = prepare((100, 64), &shapes);
        // 60 wide in a 100-wide atlas: one per shelf, two shelves of 30 per fill.
        assert_eq!(p.batches.len(), 3);
        assert_eq!(p.batches[0].items, 0..2);
        assert_eq!(p.batches[1].items, 2..4);
        assert_eq!(p.batches[2].items, 4..5);
        assert_eq!(p.items[1].atlas, [0, 30]);
        assert_eq!(p.items[2].atlas, [0, 0]);
        let edges: usize = p.batches.iter().map(|b| b.edges.len()).sum();
        let spans: usize = p.batches.iter().map(|b| b.spans.len()).sum();
        assert_eq!(edges, p.edges.len());
        assert_eq!(spans, p.spans.len());
        for b in &p.batches {
            let items = b.items.clone();
            assert!(
                p.edges[b.edges.start as usize..b.edges.end as usize]
                    .iter()
                    .chain(&p.spans[b.spans.start as usize..b.spans.end as usize])
                    .all(|piece| items.contains(&piece.item))
            );
        }
    }

    #[test]
    fn a_fill_is_repacked_tallest_first_before_starting_another() {
        let sized = |w: f32, h: f32| rect(0.0, 0.0, w, h);
        let p = prepare(
            (100, 100),
            &[sized(70.0, 30.0), sized(30.0, 70.0), sized(70.0, 40.0)],
        );
        assert_eq!(p.batches.len(), 1);
        assert_eq!(p.items[1].atlas, [0, 0]);
        assert_eq!(p.items[2].atlas, [30, 0]);
        assert_eq!(p.items[0].atlas, [0, 70]);
        assert_eq!(p.rows(), 100);
    }

    #[test]
    fn byte_layouts_match_the_shader() {
        let mut p = prepare((10, 10), &[rect(1.0, 1.0, 2.0, 2.0)]);
        p.stops.push(([0.5; 4], 0.25));
        assert_eq!(p.item_bytes().len(), ITEM_BYTES);
        assert_eq!(
            p.piece_bytes().len(),
            (p.edges.len() + p.spans.len()) * PIECE_BYTES
        );
        assert_eq!(p.stop_bytes().len(), STOP_BYTES);
    }
}
