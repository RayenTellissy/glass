//! Animated and nested timeline rendering (Decision 75), on real media and the GPU:
//!
//! - keyframed opacity, position and LUT intensity render each frame with the value at
//!   the source time it shows (against single-value renders and a CPU mix)
//! - frame cache keys follow evaluated values: an edit elsewhere keeps animated frames
//!   cached, a key edit re-renders only the frames whose values changed
//! - a compound clip renders exactly as its clips placed flat, a compound with its own
//!   look as one layer, a nested sequence at another rate shows its nearest frame from
//!   its own media frames, and an edit inside it re-renders the parent frames showing it
//!
//! GPU checks skip without an adapter, media checks without the fixtures.

// Dirty frames are lists of ranges, often of one.
#![allow(clippy::single_range_in_vec_init)]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine::lookahead::{GopIndexes, Layer};
use engine::quality::QualityLevel;
use engine::scrub::{Decoded, ScrubDecoder, SoftwareDecoderOptions, SoftwareScrubDecoder};
use engine::timeline::{DirtyRanges, RendererConfig, TimelinePlan, TimelineRenderer, TrackFrame};
use media::VideoFrame;
use project::params::{ParamValue, effects};
use project::{
    ClipParam, ClipSpec, EffectId, ItemId, KeyTarget, MediaId, Project, Rational, Resolution,
    SequenceFormat, SequenceId, Time, TimeRange, TrackId, TrackKind, Transform, TransitionSpec,
};
use render::frame_cache::{FrameCacheConfig, SpillFormat};
use render::lut::{Lut, Lut3d};
use render::wgpu;

const FPS: i32 = 30;
const CANVAS: (u32, u32) = (640, 360);
const MAIN: &str = "scenes_cuts_640x360_30.mp4";
const OTHER: &str = "sync_h264_aac_48k.mp4";
/// 25 fps, one second.
const AT_25: &str = "h264_fullrange_640x360.mp4";

fn at(frames: i64) -> Time {
    Time::from_frames(frames, Rational::new(FPS, 1)).expect("time")
}

fn range(start: i64, end: i64) -> TimeRange {
    TimeRange::between(at(start), at(end)).expect("range")
}

fn format(rate: i32) -> SequenceFormat {
    SequenceFormat {
        resolution: Resolution::new(CANVAS.0, CANVAS.1),
        frame_rate: Rational::new(rate, 1),
        ..SequenceFormat::default()
    }
}

/// A project with `media` listed and an empty sequence at `rate` with video tracks V2
/// and V1 (top first).
struct Timeline {
    project: Project,
    seq: SequenceId,
    v1: TrackId,
    v2: TrackId,
    media: Vec<MediaId>,
}

fn timeline(media: &[&Path]) -> Timeline {
    let mut p = Project::new("timeline-layers", "layers");
    let mut ids = Vec::new();
    for (i, path) in media.iter().enumerate() {
        let (q, id) = p
            .add_media(format!("media-{i}"), path.to_string_lossy())
            .expect("media");
        p = q;
        ids.push(id);
    }
    let (p, seq) = p.add_sequence_with("main", format(FPS)).expect("sequence");
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).expect("V2");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("V1");
    Timeline {
        project: p,
        seq,
        v1,
        v2,
        media: ids,
    }
}

fn clip(media: MediaId, start: i64, end: i64, source: i64) -> ClipSpec {
    ClipSpec {
        media,
        range: range(start, end),
        source_start: at(source),
    }
}

fn plan(p: &Project, seq: SequenceId) -> TimelinePlan {
    TimelinePlan::build(p, seq).expect("plan")
}

/// Exact software decoding, a decoder opened for each frame (as the scrubber's exact
/// path does after a seek), sharing the GOP indexes.
#[derive(Default)]
struct Decoders {
    gops: GopIndexes,
}

impl Decoders {
    /// The source frame segment `s` of `plan` shows at timeline frame `t`, and its
    /// presentation time in seconds.
    fn decode(&mut self, plan: &TimelinePlan, s: usize, t: i64) -> (f64, VideoFrame) {
        let segment = plan.segment(s);
        // One seek straight to the frame: the stepped walk towards it can land past a
        // frame it returned on the way.
        let options = SoftwareDecoderOptions {
            cancel_step: std::time::Duration::from_secs(3600),
            ..SoftwareDecoderOptions::default()
        };
        let mut decoder =
            SoftwareScrubDecoder::open(&segment.source, &self.gops, options).expect("open");
        let at = segment.source_time_at(t, plan.frame_duration());
        match decoder.decode_exact(at, &|| false).expect("decode") {
            Decoded::Frame(pts, frame) => (pts.seconds(), frame),
            other => panic!("frame {t}: {other:?}"),
        }
    }

    /// Every layer timeline frame `t` reads, decoded exactly.
    fn layers(&mut self, plan: &TimelinePlan, t: i64) -> Vec<Layer<VideoFrame>> {
        plan.frame(t)
            .segments(plan)
            .into_iter()
            .map(|s| {
                let (_, frame) = self.decode(plan, s, t);
                let segment = plan.segment(s);
                Layer {
                    layer: segment.layer,
                    segment: segment.hash,
                    source: segment.source.id,
                    source_pts: None,
                    frame: Some(Arc::new(frame)),
                    skipped: false,
                }
            })
            .collect()
    }
}

fn renderer(gpu: &common::Gpu) -> TimelineRenderer {
    let mut config = RendererConfig::new(CANVAS);
    config.cache = Some(FrameCacheConfig {
        vram_ceiling: 1 << 30,
        ram_ceiling: 1 << 30,
        spill_format: SpillFormat::Native,
        spill_slots: FrameCacheConfig::SPILL_SLOTS,
    });
    TimelineRenderer::new(&gpu.device, &gpu.queue, config)
}

fn wait(gpu: &common::Gpu, submission: Option<wgpu::SubmissionIndex>) {
    if let Some(submission) = submission {
        gpu.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .expect("poll");
    }
}

/// Frame `t` of `plan` rendered by a fresh renderer, as 8-bit RGBA.
fn single(gpu: &common::Gpu, decoders: &mut Decoders, plan: &TimelinePlan, t: i64) -> Vec<u8> {
    let mut renderer = renderer(gpu);
    let layers = decoders.layers(plan, t);
    let rendered = renderer.render(plan, t, &layers, QualityLevel::Full);
    wait(gpu, rendered.submission);
    common::read_rgba8(gpu, &rendered.texture)
}

/// Renders every frame of `frames` through `renderer` (its cache answering when it can)
/// and returns the frames it had to render.
fn render_all(
    gpu: &common::Gpu,
    renderer: &mut TimelineRenderer,
    decoders: &mut Decoders,
    plan: &TimelinePlan,
    frames: std::ops::Range<i64>,
) -> BTreeSet<i64> {
    let mut rendered = BTreeSet::new();
    for t in frames {
        if renderer.lookup(plan, t, QualityLevel::Full).is_some() {
            continue;
        }
        let layers = decoders.layers(plan, t);
        let frame = renderer.render(plan, t, &layers, QualityLevel::Full);
        assert!(!frame.from_cache);
        wait(gpu, frame.submission);
        rendered.insert(t);
    }
    rendered
}

fn srgb_decode(v: u8) -> f64 {
    let v = f64::from(v) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_encode(v: f64) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let e = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (e * 255.0).round() as u8
}

/// The largest difference, over the colour channels, between `shown` and `top` mixed
/// over `bottom` at `alpha` in linear light (both opaque renders). Channels where either
/// render is at the end of the 8-bit range are left out: decoded video can sit outside
/// it in the working space (a slightly negative blue in black), which the renders clamp
/// and the mix does not.
fn mix_error(shown: &[u8], top: &[u8], bottom: &[u8], alpha: f64) -> u8 {
    let inside = |v: u8| (2..=253).contains(&v);
    shown
        .as_chunks::<4>()
        .0
        .iter()
        .zip(top.as_chunks::<4>().0.iter().zip(bottom.as_chunks::<4>().0))
        .flat_map(|(s, (a, b))| (0..3).map(move |c| (s[c], a[c], b[c])))
        .filter(|&(_, a, b)| inside(a) && inside(b))
        .map(|(s, a, b)| {
            let mixed = alpha * srgb_decode(a) + (1.0 - alpha) * srgb_decode(b);
            s.abs_diff(srgb_encode(mixed))
        })
        .max()
        .unwrap_or(0)
}

fn checksum(pixels: &[u8]) -> u64 {
    // FNV-1a.
    pixels.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn opacity(item: ItemId) -> KeyTarget<'static> {
    KeyTarget::Clip {
        item,
        param: ClipParam::Opacity,
    }
}

fn setup(names: &[&str]) -> Option<(common::Gpu, Vec<PathBuf>)> {
    let Some(gpu) = common::gpu() else {
        eprintln!("skipped: no GPU adapter with 16-bit normalised textures");
        return None;
    };
    let paths: Option<Vec<PathBuf>> = names.iter().map(|n| common::fixture(n)).collect();
    Some((gpu, paths?))
}

#[test]
fn animated_opacity_fades_frame_by_frame() {
    let Some((gpu, paths)) = setup(&[MAIN, OTHER]) else {
        return;
    };
    // A on V2 fades in over B on V1, which covers the canvas.
    let tl = timeline(&[&paths[0], &paths[1]]);
    let (base, _) = tl
        .project
        .insert_clip(tl.v1, clip(tl.media[0], 0, 45, 100))
        .expect("B");
    let (opaque, a) = base
        .insert_clip(tl.v2, clip(tl.media[1], 0, 45, 0))
        .expect("A");
    let p = opaque
        .animate_param(opacity(a), at(0))
        .and_then(|p| p.set_key(opacity(a), at(0), ParamValue::Float(0.0)))
        .and_then(|p| p.set_key(opacity(a), at(30), ParamValue::Float(1.0)))
        .expect("keys");
    let animated = plan(&p, tl.seq);
    let opaque = plan(&opaque, tl.seq);
    let below = plan(&base, tl.seq);
    let s = (0..animated.segments().segments().len())
        .find(|&s| animated.look(s).item == a)
        .expect("A's segment");
    assert!(animated.is_animated(s));
    let mut decoders = Decoders::default();
    for t in [0, 7, 15, 22, 30, 40] {
        // Linear from 0 at source 0 s to 1 at 1 s, then held.
        let alpha = (t as f64 / 30.0).min(1.0);
        assert_eq!(animated.look_at(s, t).opacity, alpha, "frame {t}");
        let shown = single(&gpu, &mut decoders, &animated, t);
        let top = single(&gpu, &mut decoders, &opaque, t);
        let bottom = single(&gpu, &mut decoders, &below, t);
        let error = mix_error(&shown, &top, &bottom, alpha);
        eprintln!("opacity {alpha:.3} at frame {t}: max {error} levels from the CPU mix");
        assert!(error <= 2, "frame {t}: {error} levels from the CPU mix");
        if (0.2..0.8).contains(&alpha) {
            // Neither the opaque nor the hidden clip: the fade shows.
            for other in [&top, &bottom] {
                let apart = shown.iter().zip(other.iter()).map(|(a, b)| a.abs_diff(*b));
                assert!(apart.max().unwrap_or(0) > 20, "frame {t}: the fade shows");
            }
        }
    }
}

#[test]
fn animated_position_moves_the_layer_by_its_value() {
    let Some((gpu, paths)) = setup(&[MAIN]) else {
        return;
    };
    let tl = timeline(&[&paths[0]]);
    let half = Transform {
        scale: [0.5, 0.5],
        ..Transform::IDENTITY
    };
    let (p, a) = tl
        .project
        .insert_clip(tl.v1, clip(tl.media[0], 0, 40, 0))
        .expect("clip");
    let still = p.set_clip_transform(a, half).expect("transform");
    let position = KeyTarget::Clip {
        item: a,
        param: ClipParam::Position,
    };
    // Two pixels a frame to the right, then down 1 px a frame.
    let moving = still
        .animate_param(position, at(0))
        .and_then(|p| p.set_key(position, at(0), ParamValue::Point([0.0, 0.0])))
        .and_then(|p| p.set_key(position, at(30), ParamValue::Point([60.0, 0.0])))
        .and_then(|p| p.set_key(position, at(40), ParamValue::Point([60.0, 10.0])))
        .expect("keys");
    let still = plan(&still, tl.seq);
    let moving = plan(&moving, tl.seq);
    let mut decoders = Decoders::default();
    let w = i64::from(CANVAS.0);
    for (t, (dx, dy)) in [(0, (0, 0)), (10, (20, 0)), (15, (30, 0)), (35, (60, 5))] {
        let base = single(&gpu, &mut decoders, &still, t);
        let shown = single(&gpu, &mut decoders, &moving, t);
        // The half-size layer covers 160..480 x 90..270 unmoved; compare its inside,
        // a few pixels in from the edges, against the unmoved render shifted.
        let mut worst = 0u8;
        let mut unshifted = 0u8;
        for y in 95..265 {
            for x in 165..475 {
                let moved = ((y + dy) * w + x + dx) as usize * 4;
                let from = (y * w + x) as usize * 4;
                for c in 0..3 {
                    worst = worst.max(shown[moved + c].abs_diff(base[from + c]));
                    unshifted = unshifted.max(shown[from + c].abs_diff(base[from + c]));
                }
            }
        }
        eprintln!("position ({dx}, {dy}) at frame {t}: max {worst} levels from the shifted layer");
        assert!(
            worst <= 2,
            "frame {t}: {worst} levels from the shifted layer"
        );
        if (dx, dy) != (0, 0) {
            assert!(unshifted > 20, "frame {t}: the layer moved");
        }
    }
}

/// A 9-point `.cube` that inverts every channel, written once per test binary.
fn invert_cube() -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("timeline-layers-invert.cube");
    let lut = Lut {
        cube: Some(Lut3d::from_fn(9, |rgb| rgb.map(|v| 1.0 - v))),
        ..Default::default()
    };
    std::fs::write(&path, lut.to_cube()).expect("write cube");
    path
}

#[test]
fn animated_lut_intensity_changes_the_output() {
    let Some((gpu, paths)) = setup(&[MAIN]) else {
        return;
    };
    let tl = timeline(&[&paths[0]]);
    let schema = effects::lut();
    let (p, a) = tl
        .project
        .insert_clip(tl.v1, clip(tl.media[0], 0, 40, 0))
        .expect("clip");
    let (p, effect) = p.add_effect(a, &schema, None).expect("LUT");
    let cube = invert_cube();
    let p = p
        .set_effect_param(
            effect,
            &schema,
            "file",
            ParamValue::Text(cube.to_string_lossy().into_owned()),
        )
        .expect("file");
    let constant = |v: f64| {
        plan(
            &p.set_effect_param(effect, &schema, "intensity", ParamValue::Float(v))
                .expect("intensity"),
            tl.seq,
        )
    };
    let target = |effect: EffectId| KeyTarget::Effect {
        effect,
        schema: &schema,
        param: "intensity",
    };
    let animated = p
        .animate_param(target(effect), at(0))
        .and_then(|p| p.set_key(target(effect), at(0), ParamValue::Float(0.0)))
        .and_then(|p| p.set_key(target(effect), at(30), ParamValue::Float(1.0)))
        .expect("keys");
    let animated = plan(&animated, tl.seq);
    assert!(animated.is_animated(0));
    let mut decoders = Decoders::default();
    let mut sums = Vec::new();
    for (t, value) in [(0, 0.0), (15, 0.5), (30, 1.0)] {
        let shown = single(&gpu, &mut decoders, &animated, t);
        let expected = single(&gpu, &mut decoders, &constant(value), t);
        assert!(
            shown == expected,
            "frame {t}: intensity {value} as a constant renders the same"
        );
        sums.push(checksum(&shown));
    }
    // The same source frame at another intensity differs.
    let other = single(&gpu, &mut decoders, &constant(1.0), 15);
    assert_ne!(checksum(&other), sums[1], "intensity 0.5 differs from 1");
    let none = single(&gpu, &mut decoders, &constant(0.0), 30);
    assert_ne!(checksum(&none), sums[2], "intensity 1 differs from 0");
}

#[test]
fn animated_frames_are_cached_by_their_values() {
    let Some((gpu, paths)) = setup(&[MAIN, OTHER]) else {
        return;
    };
    let tl = timeline(&[&paths[0], &paths[1]]);
    // V1: A fades in over its first second, holds, and fades to 0.5 over its second;
    // then B, unanimated.
    let (p, a) = tl
        .project
        .insert_clip(tl.v1, clip(tl.media[0], 0, 60, 0))
        .expect("A");
    let (p, b) = p
        .insert_clip(tl.v1, clip(tl.media[1], 60, 90, 0))
        .expect("B");
    let p = p
        .animate_param(opacity(a), at(0))
        .and_then(|p| p.set_key(opacity(a), at(0), ParamValue::Float(0.0)))
        .and_then(|p| p.set_key(opacity(a), at(30), ParamValue::Float(1.0)))
        .and_then(|p| p.set_key(opacity(a), at(59), ParamValue::Float(0.5)))
        .expect("keys");
    let mut renderer = renderer(&gpu);
    let mut decoders = Decoders::default();
    let before = plan(&p, tl.seq);
    let cold = render_all(&gpu, &mut renderer, &mut decoders, &before, 0..90);
    assert_eq!(cold.len(), 90);

    // An edit to B leaves A's animated frames cached.
    let faded = p.set_clip_opacity(b, 0.5).expect("B opacity");
    let after = plan(&faded, tl.seq);
    let dirty = DirtyRanges::between(&p, &faded).sequence(tl.seq);
    let again = render_all(&gpu, &mut renderer, &mut decoders, &after, 0..90);
    eprintln!(
        "unrelated edit: dirty {dirty:?}, {} of 90 frames rendered again",
        again.len()
    );
    assert_eq!(again, (60..90).collect());

    // Moving A's last key changes the frames after the key before it (30) only.
    let keyed = faded
        .set_key(opacity(a), at(59), ParamValue::Float(0.25))
        .expect("key");
    let dirty = DirtyRanges::between(&faded, &keyed).sequence(tl.seq);
    let keyed_plan = plan(&keyed, tl.seq);
    let changed: BTreeSet<i64> = (0..90)
        .filter(|&t| after.span_hash(t) != keyed_plan.span_hash(t))
        .collect();
    let again = render_all(&gpu, &mut renderer, &mut decoders, &keyed_plan, 0..90);
    let dirty_frames = (0..90).filter(|&t| dirty.contains(t)).count();
    eprintln!(
        "key edit: dirty {dirty:?} ({dirty_frames} frames), {} rendered again",
        again.len()
    );
    assert_eq!(again, (31..60).collect(), "frames 0..=30 keep their values");
    assert_eq!(changed, again);
    assert!(again.iter().all(|t| dirty.contains(*t)));

    // Undo: every frame is in the cache under its old key.
    let undone = render_all(&gpu, &mut renderer, &mut decoders, &after, 0..90);
    assert!(undone.is_empty(), "{undone:?}");
}

/// V1: A (0..30) and B (30..60) with a centred 10-frame dissolve; V2: C (10..40) at half
/// size moving right. Returns the project, the three items and the dissolve's id.
fn three_clips(tl: &Timeline) -> (Project, [ItemId; 3]) {
    let (p, a) = tl
        .project
        .insert_clip(tl.v1, clip(tl.media[0], 0, 30, 0))
        .expect("A");
    let (p, b) = p
        .insert_clip(tl.v1, clip(tl.media[0], 30, 60, 90))
        .expect("B");
    let (p, c) = p
        .insert_clip(tl.v2, clip(tl.media[1], 10, 40, 0))
        .expect("C");
    let (p, _) = p
        .add_transition(TransitionSpec::new(Some(a), Some(b), at(10)))
        .expect("dissolve");
    let p = p
        .set_clip_transform(
            c,
            Transform {
                scale: [0.5, 0.5],
                ..Transform::IDENTITY
            },
        )
        .expect("C transform");
    let position = KeyTarget::Clip {
        item: c,
        param: ClipParam::Position,
    };
    let p = p
        .animate_param(position, at(0))
        .and_then(|p| p.set_key(position, at(0), ParamValue::Point([-100.0, 0.0])))
        .and_then(|p| p.set_key(position, at(30), ParamValue::Point([100.0, 20.0])))
        .expect("C keys");
    (p, [a, b, c])
}

#[test]
fn a_compound_clip_renders_as_its_clips_placed_flat() {
    let Some((gpu, paths)) = setup(&[MAIN, OTHER]) else {
        return;
    };
    let tl = timeline(&[&paths[0], &paths[1]]);
    let (flat, items) = three_clips(&tl);
    let (nested, compound, clip) = flat.nest_items(&items, "Compound").expect("nest");
    let flat_plan = plan(&flat, tl.seq);
    let nested_plan = plan(&nested, tl.seq);
    assert_eq!(nested_plan.nested_looks().len(), 1);
    assert_eq!(nested_plan.nested(0).sequence, compound);
    assert_eq!(nested_plan.nested(0).item, clip);
    // The same media in the same segments, decoded at the same times.
    assert_eq!(
        nested_plan.segments().segments().len(),
        flat_plan.segments().segments().len()
    );
    let mut decoders = Decoders::default();
    let mut differing = Vec::new();
    for t in 0..60 {
        assert!(
            matches!(nested_plan.frame(t).tracks[..], [TrackFrame::Nested { frame, .. }] if frame == t),
            "frame {t}"
        );
        let a = single(&gpu, &mut decoders, &flat_plan, t);
        let b = single(&gpu, &mut decoders, &nested_plan, t);
        if checksum(&a) != checksum(&b) {
            differing.push(t);
        }
    }
    assert!(differing.is_empty(), "frames differ: {differing:?}");
    eprintln!("compound: 60 of 60 frames equal to the flat clips (checksums)");
}

#[test]
fn a_compound_with_its_own_look_renders_as_one_layer() {
    let Some((gpu, paths)) = setup(&[MAIN, OTHER]) else {
        return;
    };
    let tl = timeline(&[&paths[0], &paths[1]]);
    let (flat, items) = three_clips(&tl);
    let (nested, _, clip_item) = flat.nest_items(&items, "Compound").expect("nest");
    // The compound sits on V2 now; V1 gets a clip below it, covering the canvas.
    let (below, _) = tl
        .project
        .insert_clip(tl.v1, clip(tl.media[0], 0, 60, 200))
        .expect("below");
    let (nested, _) = nested
        .insert_clip(tl.v1, clip(tl.media[0], 0, 60, 200))
        .expect("below");
    let faded = nested.set_clip_opacity(clip_item, 0.5).expect("opacity");
    let opaque = plan(&nested, tl.seq);
    let faded = plan(&faded, tl.seq);
    let below = plan(&below, tl.seq);
    let mut decoders = Decoders::default();
    // A full turn is the identity but not placed as it is: the nested composite renders
    // on its own and is placed as one layer, which shows the same picture.
    let turned = nested
        .set_clip_transform(
            clip_item,
            Transform {
                rotation: 360.0,
                ..Transform::IDENTITY
            },
        )
        .expect("turn");
    let turned = plan(&turned, tl.seq);
    for t in [5, 28] {
        let as_is = single(&gpu, &mut decoders, &opaque, t);
        let one_layer = single(&gpu, &mut decoders, &turned, t);
        let worst = as_is
            .iter()
            .zip(&one_layer)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        eprintln!("compound as one layer at frame {t}: max {worst} levels from placed as is");
        assert!(worst <= 1, "frame {t}: {worst} levels");
    }
    // Over the dissolve and C: the whole composite fades as one picture (C over the
    // dissolve, then faded over the clip below), not each clip on its own.
    for t in [5, 20, 28, 45] {
        let top = single(&gpu, &mut decoders, &opaque, t);
        let bottom = single(&gpu, &mut decoders, &below, t);
        let shown = single(&gpu, &mut decoders, &faded, t);
        let error = mix_error(&shown, &top, &bottom, 0.5);
        eprintln!("compound at 50% at frame {t}: max {error} levels from the CPU mix");
        assert!(error <= 2, "frame {t}: {error} levels");
    }
    // At half preview resolution the nested composite renders at half size too.
    let mut renderer = renderer(&gpu);
    let layers = decoders.layers(&faded, 20);
    let half = renderer.render(&faded, 20, &layers, QualityLevel::Half);
    wait(&gpu, half.submission);
    assert_eq!(
        (half.texture.width(), half.texture.height()),
        (CANVAS.0 / 2, CANVAS.1 / 2)
    );
}

#[test]
fn a_nested_sequence_at_another_rate_shows_its_nearest_frames() {
    let Some(path) = common::fixture(AT_25) else {
        return;
    };
    let mut tl = timeline(&[&path]);
    // A 25 fps sequence of the 25 fps file from its start, nested in the 30 fps main.
    let (p, inner) = tl
        .project
        .add_sequence_with("25 fps", format(25))
        .expect("inner");
    let (p, iv1) = p
        .add_track(inner, TrackKind::Video, "V1", None)
        .expect("V1");
    let (p, _) = p
        .insert_clip(
            iv1,
            ClipSpec {
                media: tl.media[0],
                range: TimeRange::new(Time::ZERO, Time::seconds(1)),
                source_start: Time::ZERO,
            },
        )
        .expect("inner clip");
    let (p, _) = p
        .insert_sequence_clip(tl.v1, inner, range(0, 30), Time::ZERO)
        .expect("nested");
    tl.project = p;
    let main = plan(&tl.project, tl.seq);
    let mut decoders = Decoders::default();
    for t in 0..30 {
        // Nested time t / 30 s is 25 t / 30 nested frames: the nearest, halves later.
        let expected = (t * 50 + 30_i64).div_euclid(60);
        let desc = main.frame(t);
        assert_eq!(
            desc.tracks,
            [TrackFrame::Nested {
                nested: 0,
                frame: expected
            }],
            "frame {t}"
        );
        // The decoded media frame is the nested frame's own.
        let [s] = desc.segments(&main)[..] else {
            panic!("frame {t}: one segment");
        };
        // The look-ahead shows the media frame presented at or before the segment's
        // source time: inside the nested frame's own media frame.
        let instant = main.segment(s).source_time_at(t, main.frame_duration());
        let (num, den) = (
            i128::from(instant.value) * i128::from(instant.time_base.num),
            i128::from(instant.time_base.den),
        );
        let frame = (num * 25).div_euclid(den);
        assert_eq!(frame, i128::from(expected), "frame {t}: media frame shown");
        // Decoded exactly (one seek from the keyframe, no walk), it is that frame.
        let (pts, _) = decoders.decode(&main, s, t);
        assert_eq!(pts, expected as f64 / 25.0, "frame {t}: decoded");
    }
}

#[test]
fn an_edit_inside_a_nested_sequence_re_renders_the_frames_showing_it() {
    let Some((gpu, paths)) = setup(&[MAIN, OTHER]) else {
        return;
    };
    let tl = timeline(&[&paths[0], &paths[1]]);
    // V1: X (0..30), then a compound of I1 (30..60) and I2 (60..90).
    let (p, _) = tl
        .project
        .insert_clip(tl.v1, clip(tl.media[1], 0, 30, 0))
        .expect("X");
    let (p, i1) = p
        .insert_clip(tl.v1, clip(tl.media[0], 30, 60, 0))
        .expect("I1");
    let (p, i2) = p
        .insert_clip(tl.v1, clip(tl.media[0], 60, 90, 60))
        .expect("I2");
    let (p, compound, _) = p.nest_items(&[i1, i2], "Compound").expect("nest");
    let mut renderer = renderer(&gpu);
    let mut decoders = Decoders::default();
    let before = plan(&p, tl.seq);
    assert_eq!(
        render_all(&gpu, &mut renderer, &mut decoders, &before, 0..90).len(),
        90
    );

    // Fading I2 inside the compound: the parent frames showing it, and no others.
    let edited = p.set_clip_opacity(i2, 0.5).expect("opacity");
    let dirty = DirtyRanges::between(&p, &edited);
    assert_eq!(
        dirty.sequence(compound).ranges(),
        Some(&[30..60][..]),
        "the compound's own frames"
    );
    let parent = dirty.sequence(tl.seq);
    let after = plan(&edited, tl.seq);
    let again = render_all(&gpu, &mut renderer, &mut decoders, &after, 0..90);
    eprintln!(
        "nested edit: parent dirty {parent:?}, {} of 90 frames rendered again",
        again.len()
    );
    assert_eq!(again, (60..90).collect());
    assert!(again.iter().all(|t| parent.contains(*t)));
    // Every frame equals a fresh render of the edited timeline.
    for t in [45, 75] {
        let cached = renderer
            .lookup(&after, t, QualityLevel::Full)
            .expect("cached");
        assert!(
            common::read_rgba8(&gpu, &cached) == single(&gpu, &mut decoders, &after, t),
            "frame {t}"
        );
    }
}
