//! [`TimelineRenderer`]: turns a timeline frame's decoded layers into the preview picture
//! through one render graph per frame, and keeps rendered frames in the frame cache.
//!
//! Per frame, each video track bottom first contributes one composite layer:
//!
//! ```text
//!   clip:        source (Y'CbCr → working space, gamut included) → effects → layer
//!                (crop, transform, opacity, blend in the composite pass)
//!   transition:  A: outgoing clip placed alone on a transparent canvas ─┐
//!                B: incoming clip placed alone on a transparent canvas ─┴→ TransitionNode
//!                → layer (canvas-sized, normal blend)
//!   all layers → composite stack (bilinear cap at reduced quality) → output (8-bit sRGB,
//!   dithered with the frame number as seed) → the cached and presented texture
//! ```
//!
//! The canvas is the sequence's (from the renderer's config), scaled by the
//! [`QualityLevel`]: layer transforms are mapped into the preview canvas, so a half
//! preview composites at half size. Frames are cached under [`TimelinePlan::span_hash`]
//! and the timeline frame, with the preview size and quality in the variant;
//! [`CachedFrames`] (the scrubber's index) lists the full-quality frames resident in the
//! cache and follows its evictions.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use media::{Primaries, VideoFrame};
use project::params::{ParamDef, ParamSchema, ParamValue as ProjectValue};
use render::colour::WorkingSpace;
use render::crop::Crop;
use render::frame_cache::{FrameCache, FrameCacheConfig, FrameCacheStats, FrameKey, RenderVariant};
use render::graph::{Graph, Port, TexturePool};
use render::interop::{ImportedFrame, SurfaceImporter};
use render::lut::Lut;
use render::nodes::{
    CompositeNode, GpuLut, Layer as CompositeLayer, LutNode, OutputNode, YuvSourceNode,
    composite_stack_capped,
};
use render::renderer::Renderer;
use render::shaders::ShaderLibrary;
use render::transform::{Affine2, Resample, Transform2D};
use render::transition::{Direction, ParamValue, Transition, TransitionNode, TransitionRegistry};
use render::upload::{UploadedFrame, upload_frame};
use render::wgpu;

use super::plan::{ClipLook, EffectLook, FrameDesc, TimelinePlan, TrackFrame};
use crate::lookahead::Layer;
use crate::quality::QualityLevel;
use crate::scrub::CachedFrames;

/// The format of rendered preview frames.
pub const PREVIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The transition drawn for a kind this build does not know.
const FALLBACK_TRANSITION: &str = "cross-dissolve";

/// Canvas, working space and cache ceilings of a [`TimelineRenderer`].
#[derive(Clone, Copy, Debug)]
pub struct RendererConfig {
    /// The sequence canvas in pixels ([`TimelinePlan::canvas`]).
    pub canvas: (u32, u32),
    pub working: WorkingSpace,
    /// `None`: ceilings from the device's and the machine's memory.
    pub cache: Option<FrameCacheConfig>,
}

impl RendererConfig {
    pub fn new(canvas: (u32, u32)) -> Self {
        Self {
            canvas,
            working: WorkingSpace::default(),
            cache: None,
        }
    }
}

/// Counters for tests and the debug panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Frames rendered through a graph.
    pub rendered: u64,
    /// Frames answered from the frame cache.
    pub cache_hits: u64,
    /// Clips with no decoded picture at their frame (offline or failed sources).
    pub missing_pictures: u64,
    /// Pictures that could not be put on the GPU.
    pub upload_errors: u64,
    /// Transitions of a kind this build does not know, drawn as a cross dissolve.
    pub unknown_transitions: u64,
    /// LUT files that could not be read.
    pub lut_errors: u64,
}

/// A rendered (or cached) preview frame.
#[derive(Debug)]
pub struct RenderedFrame {
    pub frame: i64,
    /// [`PREVIEW_FORMAT`], at the quality's preview size; the cache holds it too, so
    /// nothing renders into it again.
    pub texture: wgpu::Texture,
    pub quality: QualityLevel,
    pub from_cache: bool,
    /// The submission that renders it (none for a cache hit).
    pub submission: Option<wgpu::SubmissionIndex>,
}

enum Picture {
    Uploaded(UploadedFrame),
    Imported(ImportedFrame),
}

impl Picture {
    fn size(&self) -> (u32, u32) {
        let texture = match self {
            Picture::Uploaded(f) => &f.textures[0],
            Picture::Imported(f) => f.luma.texture(),
        };
        (texture.width(), texture.height())
    }

    fn node(&self, working: WorkingSpace) -> YuvSourceNode<'_> {
        match self {
            Picture::Uploaded(f) => YuvSourceNode::uploaded(f, working),
            Picture::Imported(f) => YuvSourceNode::imported(f, working),
        }
    }
}

/// Renders timeline frames on the render thread, which owns it (see the module docs).
pub struct TimelineRenderer {
    renderer: Renderer,
    pool: TexturePool,
    cache: FrameCache,
    cached: CachedFrames,
    registry: Arc<TransitionRegistry>,
    /// Parameter schemas of the transitions, by kind.
    schemas: HashMap<String, ParamSchema>,
    importer: SurfaceImporter,
    luts: HashMap<PathBuf, Option<Arc<GpuLut>>>,
    canvas: (u32, u32),
    working: WorkingSpace,
    /// Frames the cache has let go of so far, to bring `cached` in step when it moves.
    released: u64,
    rendered_frames: u64,
    stats: RenderStats,
}

impl TimelineRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, config: RendererConfig) -> Self {
        let registry = TransitionRegistry::standard();
        let library = ShaderLibrary::default();
        registry.add_shaders(&library);
        let cache = match config.cache {
            Some(c) => FrameCache::new(c),
            None => FrameCache::for_device(device),
        };
        Self {
            renderer: Renderer::with_shaders(device, queue, library),
            pool: TexturePool::new(),
            cache,
            cached: CachedFrames::new(),
            registry,
            schemas: HashMap::new(),
            importer: SurfaceImporter::default(),
            luts: HashMap::new(),
            canvas: (config.canvas.0.max(1), config.canvas.1.max(1)),
            working: config.working,
            released: 0,
            rendered_frames: 0,
            stats: RenderStats::default(),
        }
    }

    pub fn device(&self) -> &wgpu::Device {
        self.renderer.device()
    }

    pub fn canvas(&self) -> (u32, u32) {
        self.canvas
    }

    /// The scrubber's index of full-quality frames in the cache (cheap to clone).
    pub fn cached_frames(&self) -> CachedFrames {
        self.cached.clone()
    }

    pub fn stats(&self) -> RenderStats {
        self.stats
    }

    pub fn cache_stats(&self) -> FrameCacheStats {
        self.cache.stats()
    }

    /// The cache variant of frames rendered at `quality`.
    pub fn variant(&self, quality: QualityLevel) -> RenderVariant {
        let (width, height) = quality.preview_size(self.canvas);
        RenderVariant {
            width,
            height,
            quality: quality.cache_step(),
            settings: u64::from(self.working.primaries == Primaries::Bt2020),
        }
    }

    pub fn key(&self, plan: &TimelinePlan, frame: i64, quality: QualityLevel) -> FrameKey {
        FrameKey {
            segment: plan.span_hash(frame),
            frame: frame as u64,
            variant: self.variant(quality),
        }
    }

    /// The cached frame `frame` at `quality`, refreshing its recency.
    pub fn lookup(
        &mut self,
        plan: &TimelinePlan,
        frame: i64,
        quality: QualityLevel,
    ) -> Option<wgpu::Texture> {
        let key = self.key(plan, frame, quality);
        let device = self.renderer.device().clone();
        let queue = self.renderer.queue().clone();
        let hit = self.cache.lookup(&device, &queue, &key)?;
        self.stats.cache_hits += 1;
        Some(hit.texture)
    }

    /// Timeline frame `frame` at `quality`: from the cache when it is there, else
    /// rendered from `layers` (the look-ahead's or the scrubber's, any order) and cached.
    pub fn render(
        &mut self,
        plan: &TimelinePlan,
        frame: i64,
        layers: &[Layer<VideoFrame>],
        quality: QualityLevel,
    ) -> RenderedFrame {
        if let Some(texture) = self.lookup(plan, frame, quality) {
            return RenderedFrame {
                frame,
                texture,
                quality,
                from_cache: true,
                submission: None,
            };
        }
        let desc = plan.frame(frame);
        let (texture, submission) = self.render_desc(plan, &desc, layers, quality);
        let key = self.key(plan, frame, quality);
        let device = self.renderer.device().clone();
        let queue = self.renderer.queue().clone();
        self.cache.insert(&device, &queue, key, texture.clone());
        if quality == QualityLevel::Full {
            self.cached.insert(frame);
        }
        self.cache.maintain(&device);
        self.follow_evictions(plan);
        RenderedFrame {
            frame,
            texture,
            quality,
            from_cache: false,
            submission: Some(submission),
        }
    }

    /// Renders `desc` from `layers` into a new texture, bypassing the cache (thumbnails,
    /// tests, and a frame whose description the caller changed). Returns the texture
    /// and the submission that renders it.
    pub fn render_desc(
        &mut self,
        plan: &TimelinePlan,
        desc: &FrameDesc,
        layers: &[Layer<VideoFrame>],
        quality: QualityLevel,
    ) -> (wgpu::Texture, wgpu::SubmissionIndex) {
        let size = quality.preview_size(self.canvas);
        let to_preview = Affine2::scale(
            f64::from(size.0) / f64::from(self.canvas.0),
            f64::from(size.1) / f64::from(self.canvas.1),
        );
        let cap = quality.max_resample();
        let device = self.renderer.device().clone();
        let queue = self.renderer.queue().clone();

        // Everything the graph borrows is made first: pictures, LUT nodes, transitions.
        let mut pictures: Vec<(usize, Picture)> = Vec::new();
        for s in desc.segments(plan) {
            let hash = plan.segment(s).hash;
            let frame = layers
                .iter()
                .find(|l| l.segment == hash)
                .and_then(|l| l.frame.as_ref());
            let Some(frame) = frame else {
                self.stats.missing_pictures += 1;
                continue;
            };
            let picture = match frame.hardware() {
                Some(hw) => self
                    .importer
                    .import(&device, hw)
                    .map(Picture::Imported)
                    .map_err(|e| {
                        tracing::warn!("timeline: frame {} cannot be imported: {e}", desc.frame)
                    }),
                None => upload_frame(&device, &queue, frame)
                    .map(Picture::Uploaded)
                    .map_err(|e| {
                        tracing::warn!("timeline: frame {} cannot be uploaded: {e}", desc.frame)
                    }),
            };
            match picture {
                Ok(picture) => pictures.push((s, picture)),
                Err(()) => self.stats.upload_errors += 1,
            }
        }
        let mut effects: HashMap<usize, Vec<LutNode>> = HashMap::new();
        for (s, _) in &pictures {
            let nodes = self.effect_nodes(plan.look(*s));
            effects.insert(*s, nodes);
        }
        let mut transitions: HashMap<usize, TransitionNode> = HashMap::new();
        for track in &desc.tracks {
            if let TrackFrame::Transition { transition, linear } = *track {
                let node = self.transition_node(plan, transition, linear);
                transitions.insert(transition, node);
            }
        }
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("timeline frame"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PREVIEW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());

        let mut graph = Graph::new();
        let place = Placement {
            pictures: &pictures,
            plan,
            canvas: self.canvas,
            working: self.working,
            to_preview,
        };
        let mut stack = Vec::with_capacity(desc.tracks.len());
        for track in &desc.tracks {
            match *track {
                TrackFrame::Clip(s) => stack.extend(place.clip(&mut graph, &mut effects, s)),
                TrackFrame::Transition { transition, .. } => {
                    let t = plan.transition(transition);
                    let a = place.alone(&mut graph, &mut effects, t.outgoing, size, cap);
                    let b = place.alone(&mut graph, &mut effects, t.incoming, size, cap);
                    if let Some(node) = transitions.remove(&transition) {
                        let mixed = graph.add(node, &[a, b]).out(0);
                        stack.push((mixed, CompositeLayer::default()));
                    }
                }
            }
        }
        let picture = composite_stack_capped(&mut graph, size, None, stack, cap)
            .unwrap_or_else(|| empty(&mut graph, size));
        let output = graph.add(
            OutputNode::preview(self.working).with_seed(desc.frame as u32),
            &[picture],
        );
        graph.export(output.out(0), &target_view);
        let submission = match graph.compile() {
            Ok(compiled) => compiled.execute(&self.renderer, &mut self.pool, None),
            Err(error) => {
                // Every graph built here is well formed; an error leaves the frame blank.
                tracing::error!("timeline: frame {} graph: {error}", desc.frame);
                queue.submit(std::iter::empty())
            }
        };
        self.stats.rendered += 1;
        self.rendered_frames += 1;
        if self.rendered_frames.is_multiple_of(60) {
            self.pool.trim(120);
        }
        (target, submission)
    }

    /// Re-reads which full-quality frames of `plan` are resident (after a new plan, or
    /// to start an index from a warm cache).
    pub fn reindex(&mut self, plan: &TimelinePlan) {
        let resident = self.resident_frames(plan);
        self.cached.clear();
        for frame in resident {
            self.cached.insert(frame);
        }
    }

    fn resident_frames(&self, plan: &TimelinePlan) -> BTreeSet<i64> {
        let variant = self.variant(QualityLevel::Full);
        self.cache
            .keys()
            .filter(|(key, _)| {
                key.variant == variant && plan.span_hash(key.frame as i64) == key.segment
            })
            .map(|(key, _)| key.frame as i64)
            .collect()
    }

    /// Drops frames the cache let go of (dropped from RAM, failed spills, invalidated)
    /// from the scrubber's index.
    fn follow_evictions(&mut self, plan: &TimelinePlan) {
        let stats = self.cache.stats();
        let released = stats.ram_evictions + stats.spill_dropped + stats.invalidated;
        if released == self.released {
            return;
        }
        self.released = released;
        let resident = self.resident_frames(plan);
        self.cached.retain(|frame| resident.contains(frame));
    }

    fn effect_nodes(&mut self, look: &ClipLook) -> Vec<LutNode> {
        let mut nodes = Vec::new();
        for effect in &look.effects {
            match effect {
                EffectLook::Lut {
                    file,
                    space,
                    intensity,
                } => {
                    if let Some(lut) = self.lut(file) {
                        nodes.push(LutNode::new(lut, self.working, *space, *intensity));
                    }
                }
            }
        }
        nodes
    }

    /// The LUT in `file`, read and uploaded on first use.
    fn lut(&mut self, file: &Path) -> Option<Arc<GpuLut>> {
        if let Some(lut) = self.luts.get(file) {
            return lut.clone();
        }
        let lut = match Lut::read(file) {
            Ok(lut) => Some(Arc::new(GpuLut::new(
                self.renderer.device(),
                self.renderer.queue(),
                &lut,
            ))),
            Err(error) => {
                tracing::warn!("timeline: LUT {}: {error}", file.display());
                self.stats.lut_errors += 1;
                None
            }
        };
        self.luts.insert(file.to_owned(), lut.clone());
        lut
    }

    fn transition_node(
        &mut self,
        plan: &TimelinePlan,
        index: usize,
        linear: f64,
    ) -> TransitionNode {
        let look = plan.transition(index);
        let transition = match self.registry.get(&look.kind) {
            Some(t) => t.clone(),
            None => {
                self.stats.unknown_transitions += 1;
                match self.registry.get(FALLBACK_TRANSITION) {
                    Some(t) => t.clone(),
                    None => unreachable!("cross dissolve is bundled"),
                }
            }
        };
        let mut node = TransitionNode::new(transition.clone(), linear, look.easing);
        if let Some(direction) = look.direction.as_deref().and_then(Direction::from_id) {
            node = node.direction(direction);
        }
        let schema = self
            .schemas
            .entry(transition.id().to_owned())
            .or_insert_with(|| schema_of(&transition));
        for param in transition.params() {
            if let Some(value) = look
                .params
                .get(schema, &param.id)
                .and_then(|v| shader_value(&v))
            {
                node = node.value(&param.id, &value);
            }
        }
        node
    }
}

/// What placing a clip's picture needs.
struct Placement<'p> {
    pictures: &'p [(usize, Picture)],
    plan: &'p TimelinePlan,
    canvas: (u32, u32),
    working: WorkingSpace,
    to_preview: Affine2,
}

impl<'p> Placement<'p> {
    /// Segment `s`'s picture through its effects, and its composite layer; `None` when
    /// it has no picture.
    fn clip(
        &self,
        graph: &mut Graph<'p>,
        effects: &mut HashMap<usize, Vec<LutNode>>,
        s: usize,
    ) -> Option<(Port, CompositeLayer)> {
        let (_, picture) = self.pictures.iter().find(|(p, _)| *p == s)?;
        let mut port = graph.add(picture.node(self.working), &[]).out(0);
        for lut in effects.remove(&s).unwrap_or_default() {
            port = graph.add(lut, &[port]).out(0);
        }
        let layer = clip_layer(
            self.plan.look(s),
            picture.size(),
            self.canvas,
            &self.to_preview,
        );
        Some((port, layer))
    }

    /// Segment `s`'s clip placed alone on a transparent canvas of `size` (a side of a
    /// transition); transparent when there is no clip or no picture.
    fn alone(
        &self,
        graph: &mut Graph<'p>,
        effects: &mut HashMap<usize, Vec<LutNode>>,
        s: Option<usize>,
        size: (u32, u32),
        cap: Resample,
    ) -> Port {
        s.and_then(|s| self.clip(graph, effects, s))
            .and_then(|layer| composite_stack_capped(graph, size, None, vec![layer], cap))
            .unwrap_or_else(|| empty(graph, size))
    }
}

/// A transparent canvas.
fn empty(graph: &mut Graph<'_>, size: (u32, u32)) -> Port {
    graph
        .add(CompositeNode::new(size, false, Vec::new()), &[])
        .out(0)
}

/// The composite layer of a clip whose picture is `layer` pixels: the project transform
/// (offsets from the canvas and layer centres) on the full canvas, mapped into the
/// preview canvas.
fn clip_layer(
    look: &ClipLook,
    layer: (u32, u32),
    canvas: (u32, u32),
    to_preview: &Affine2,
) -> CompositeLayer {
    let t = &look.transform;
    let matrix = Transform2D {
        position: t.canvas_position(canvas),
        scale: t.scale,
        rotation: t.rotation,
        anchor: t.layer_anchor(layer),
    }
    .matrix();
    let c = &look.crop;
    CompositeLayer::blended(look.blend)
        .with_transform(to_preview.then_apply(&matrix))
        .with_resample(Resample::Bicubic)
        .with_opacity(look.opacity as f32)
        .with_crop(Crop {
            left: c.left,
            top: c.top,
            right: c.right,
            bottom: c.bottom,
            feather: c.feather,
        })
}

/// The transition's parameters as a project schema, so stored values are read (and
/// sanitised) the way the inspector writes them. Parameters that do not parse are left
/// out and keep their manifest defaults.
fn schema_of(transition: &Transition) -> ParamSchema {
    let defs = transition
        .schema_params()
        .iter()
        .filter_map(|json| serde_json::from_value::<ParamDef>(json.clone()).ok())
        .collect();
    ParamSchema::new(transition.id(), transition.name(), defs)
}

fn shader_value(value: &ProjectValue) -> Option<ParamValue> {
    Some(match value {
        ProjectValue::Float(v) | ProjectValue::Angle(v) => ParamValue::Number(*v),
        ProjectValue::Int(v) => ParamValue::Number(*v as f64),
        ProjectValue::Bool(v) => ParamValue::Bool(*v),
        ProjectValue::Choice(c) => ParamValue::Choice(c.clone()),
        ProjectValue::Colour(c) => ParamValue::Colour(*c),
        ProjectValue::Point(p) => ParamValue::Point(*p),
        ProjectValue::Text(_) => return None,
    })
}
