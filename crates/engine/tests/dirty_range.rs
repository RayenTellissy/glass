//! Dirty-range invalidation: an edit re-renders only the frames it changes.
//!
//! On real media and the GPU: a sequence of three clips (two on V1, one on V2 over the
//! second) plays once with a cold cache, then after each edit (opacity on clip 2, a trim
//! of clip 3's tail, a transition between clips 1 and 2, V2 taken off the picture, a
//! marker, and an undo) it plays again through `TimelinePlayback::set_plan`, and the test
//! checks exactly which frames rendered and that every other frame was a cache hit whose
//! decoding the look-ahead skipped. Skips without an adapter or the fixtures.
//!
//! On a stand-in timeline (no GPU, no media): an edit while playing swaps the plan at a
//! frame boundary and keeps the look-ahead lane of an unchanged segment, a second pass
//! decodes nothing, and a skipped frame the cache let go of is decoded again.

mod common;
mod stand_in;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use audio::Clock;
use engine::lookahead::{GopIndexes, Layer};
use engine::quality::QualityLevel;
use engine::scrub::{
    CachedFrames, Decoded, ScrubDecoder, SoftwareDecoderOptions, SoftwareScrubDecoder,
};
use engine::threads::ThreadRegistry;
use engine::timeline::{
    DirtyFrames, DirtyRanges, FrameRender, Presented, RenderStats, RendererConfig, TimelineConfig,
    TimelinePlan, TimelinePlayback, TimelineRenderer,
};
use media::{DecoderOptions, HwPreference, VideoFrame};
use project::{
    ClipSpec, ColourLabel, ItemId, MarkerOwner, MarkerSpec, Project, Rational, Resolution,
    SequenceFormat, SequenceId, Time, TimeRange, TrackId, TrackKind, Transform, TransitionSpec,
};
use render::frame_cache::{FrameCacheConfig, FrameCacheStats, SegmentHash, SpillFormat};

const WAIT: Duration = Duration::from_secs(20);
const FPS: i32 = 30;

fn at(frames: i64) -> Time {
    Time::from_frames(frames, Rational::new(FPS, 1)).expect("time")
}

fn range(start: i64, end: i64) -> TimeRange {
    TimeRange::between(at(start), at(end)).expect("range")
}

struct Three {
    project: Project,
    seq: SequenceId,
    v2: TrackId,
    c1: ItemId,
    c2: ItemId,
    c3: ItemId,
}

/// V1: clip 1 (0..30, source 0) and clip 2 (30..60, source 1.5 s, room for a head
/// handle); V2 above: clip 3 (40..70) at half size, so clip 2 shows around it.
fn three_clips(main: &Path, other: &Path, canvas: (u32, u32)) -> Three {
    let p = Project::new("dirty-range", "dirty range");
    let (p, m1) = p.add_media("main", main.to_string_lossy()).expect("media");
    let (p, m2) = p
        .add_media("other", other.to_string_lossy())
        .expect("media");
    let (p, seq) = p
        .add_sequence_with(
            "main",
            SequenceFormat {
                resolution: Resolution::new(canvas.0, canvas.1),
                frame_rate: Rational::new(FPS, 1),
                ..SequenceFormat::default()
            },
        )
        .expect("sequence");
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).expect("V2");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("V1");
    let clip = |media, start, end, source| ClipSpec {
        media,
        range: range(start, end),
        source_start: at(source),
    };
    let (p, c1) = p.insert_clip(v1, clip(m1, 0, 30, 0)).expect("clip 1");
    let (p, c2) = p.insert_clip(v1, clip(m1, 30, 60, 45)).expect("clip 2");
    let (p, c3) = p.insert_clip(v2, clip(m2, 40, 70, 0)).expect("clip 3");
    let p = p
        .set_clip_transform(
            c3,
            Transform {
                scale: [0.5, 0.5],
                ..Transform::IDENTITY
            },
        )
        .expect("clip 3 transform");
    Three {
        project: p,
        seq,
        v2,
        c1,
        c2,
        c3,
    }
}

fn frames(r: std::ops::Range<i64>) -> BTreeSet<i64> {
    r.collect()
}

/// One pass over the whole sequence from frame 0.
struct Pass {
    /// Frames rendered (not cache hits).
    rendered: BTreeSet<i64>,
    played: i64,
    decoded: u64,
    seeks: u64,
}

/// Plays `playback` from frame 0 to the end of `plan`, collecting which frames were
/// rendered and what the look-ahead decoded meanwhile.
fn pass<R: FrameRender>(
    playback: &TimelinePlayback<R>,
    presented: &mut Presented<R::Picture>,
    end: i64,
) -> (Pass, HashMap<i64, R::Picture>) {
    let before = playback.lookahead_stats();
    let generation = playback.seek(0);
    playback.play();
    let mut rendered = BTreeSet::new();
    let mut pictures = HashMap::new();
    let mut played = 0;
    let mut last = -1;
    while last < end - 1 {
        let frame = presented
            .pop_timeout(WAIT)
            .unwrap_or_else(|e| panic!("no frame after {last}: {e:?}"));
        if frame.stamp.generation < generation {
            continue;
        }
        let frame = frame.frame;
        assert_eq!(frame.frame, last + 1, "frames in order");
        last = frame.frame;
        played += 1;
        if !frame.from_cache {
            rendered.insert(frame.frame);
        }
        pictures.insert(frame.frame, frame.texture);
    }
    // The look-ahead has handed out everything; wait for the end so the counters settle.
    let started = std::time::Instant::now();
    while !playback.stats().ended {
        assert!(started.elapsed() < WAIT, "playback did not end");
        std::thread::sleep(Duration::from_millis(2));
    }
    let after = playback.lookahead_stats();
    (
        Pass {
            rendered,
            played,
            decoded: after.frames_decoded - before.frames_decoded,
            seeks: after.seeks - before.seeks,
        },
        pictures,
    )
}

/// Every layer of timeline frame `t`, decoded exactly in software.
fn exact_layers(plan: &TimelinePlan, t: i64) -> Vec<Layer<VideoFrame>> {
    let gops = GopIndexes::new();
    let mut layers = Vec::new();
    for segment in plan.segments().segments() {
        if !segment.timeline.contains(&t) {
            continue;
        }
        let mut decoder = SoftwareScrubDecoder::open(
            &segment.source,
            &gops,
            SoftwareDecoderOptions {
                project_rate: media::Rational::new(FPS, 1),
                ..Default::default()
            },
        )
        .expect("open");
        let at = segment.source_time_at(t, plan.frame_duration());
        let (pts, frame) = match decoder.decode_exact(at, &|| false).expect("decode") {
            Decoded::Frame(pts, frame) => (pts, frame),
            other => panic!("frame {t}: {other:?}"),
        };
        layers.push(Layer {
            layer: segment.layer,
            segment: segment.hash,
            source: segment.source.id,
            source_pts: Some(pts),
            frame: Some(Arc::new(frame)),
            skipped: false,
        });
    }
    layers
}

fn cache_config() -> FrameCacheConfig {
    FrameCacheConfig {
        vram_ceiling: 512 << 20,
        ram_ceiling: 512 << 20,
        spill_format: SpillFormat::Native,
        spill_slots: FrameCacheConfig::SPILL_SLOTS,
    }
}

/// Timeline frame `t` of `plan` rendered alone by a fresh renderer.
fn single_frame(gpu: &common::Gpu, plan: &TimelinePlan, canvas: (u32, u32), t: i64) -> Vec<u8> {
    let mut config = RendererConfig::new(canvas);
    config.cache = Some(cache_config());
    let mut renderer = TimelineRenderer::new(&gpu.device, &gpu.queue, config);
    let (texture, _) = renderer.render_frame(plan, t, &exact_layers(plan, t), QualityLevel::Full);
    common::read_rgba8(gpu, &texture)
}

/// An edit of the re-render test.
struct Step<'a> {
    name: &'static str,
    edit: Box<dyn Fn(&Project) -> Project + 'a>,
    dirty: DirtyFrames,
    rendered: BTreeSet<i64>,
    end: i64,
}

fn step<'a>(
    name: &'static str,
    edit: impl Fn(&Project) -> Project + 'a,
    dirty: &[std::ops::Range<i64>],
    rendered: BTreeSet<i64>,
    end: i64,
) -> Step<'a> {
    Step {
        name,
        edit: Box::new(edit),
        dirty: DirtyFrames::from_ranges(dirty.iter().cloned()),
        rendered,
        end,
    }
}

#[test]
fn each_edit_re_renders_exactly_the_frames_it_changes() {
    let Some(gpu) = common::gpu() else {
        eprintln!("skipped: no GPU adapter with 16-bit normalised textures");
        return;
    };
    let (Some(main), Some(other)) = (
        common::fixture("h264_1080p30.mp4"),
        common::fixture("prores422_1080p23976.mov"),
    ) else {
        return;
    };
    let canvas = (640, 360);
    let three = three_clips(&main, &other, canvas);
    let seq = three.seq;
    let plan = |p: &Project| Arc::new(TimelinePlan::build(p, seq).expect("plan"));
    let mut config = TimelineConfig::new(canvas);
    config.decoder.decoder = DecoderOptions::with_hw(HwPreference::Off);
    config.decode_workers = 4;
    // Every frame at full resolution: the governor's choices would change cache keys.
    config.quality = None;
    config.renderer.cache = Some(cache_config());
    config.registry = ThreadRegistry::new();
    let mut project = three.project.clone();
    let (mut playback, mut presented) = TimelinePlayback::start(
        plan(&project),
        &gpu.device,
        &gpu.queue,
        Clock::system(48_000),
        config,
    )
    .expect("start");

    let (cold, _) = pass(&playback, &mut presented, 70);
    assert_eq!(cold.played, 70);
    assert_eq!(
        cold.rendered,
        frames(0..70),
        "a cold cache renders every frame"
    );
    assert!(cold.decoded > 0);
    eprintln!(
        "cold: 70 frames rendered, {} source frames decoded, {} seeks",
        cold.decoded, cold.seeks
    );

    // Each edit, the frames it dirties, the frames that render again and the length.
    let undo_point = std::cell::RefCell::new(None);
    let steps = [
        step(
            "opacity on clip 2",
            |p| p.set_clip_opacity(three.c2, 0.5).expect("opacity"),
            &[30..60],
            frames(30..60),
            70,
        ),
        // Clip 3 now ends over clip 2: 55..60 shows clip 2 alone, 60..70 is gone.
        step(
            "trim clip 3's tail",
            |p| p.trim_item(three.c3, range(40, 55)).expect("trim"),
            &[40..70],
            frames(55..60),
            60,
        ),
        // A centred 10-frame dissolve on the cut at 30. Clips 1 and 2 decode longer
        // segments now, but the frames they showed keep their keys.
        step(
            "transition between clips 1 and 2",
            |p| {
                let (p, _) = p
                    .add_transition(TransitionSpec::new(Some(three.c1), Some(three.c2), at(10)))
                    .expect("transition");
                *undo_point.borrow_mut() = Some(p.clone());
                p
            },
            &[25..35],
            frames(25..35),
            60,
        ),
        // The project has no track visibility flag yet: an audio track shows no picture,
        // which is what hiding V2 does to the frames.
        step(
            "V2 off the picture",
            |p| p.set_track_kind(three.v2, TrackKind::Audio).expect("hide"),
            &[40..55],
            frames(40..55),
            60,
        ),
        step(
            "a marker",
            |p| {
                p.add_marker(
                    MarkerOwner::Sequence(seq),
                    MarkerSpec::at(at(20), ColourLabel::Blue),
                )
                .expect("marker")
                .0
            },
            &[],
            BTreeSet::new(),
            60,
        ),
        // Undo back to the transition: V2's frames are still in the cache under their
        // keys from before it was hidden.
        step(
            "undo to the transition",
            |_| undo_point.borrow().clone().expect("undo point"),
            &[40..55],
            BTreeSet::new(),
            60,
        ),
    ];

    for Step {
        name,
        edit,
        dirty,
        rendered: expected,
        end,
    } in steps
    {
        let edited = edit(&project);
        let ranges = DirtyRanges::between(&project, &edited).sequence(seq);
        assert_eq!(ranges, dirty, "{name}: dirty ranges");
        let next = plan(&edited);
        assert_eq!(next.end(), end, "{name}: sequence length");
        playback
            .set_plan(next.clone(), ranges.clone())
            .expect("same format");
        let (done, pictures) = pass(&playback, &mut presented, end);
        let hits = done.played - done.rendered.len() as i64;
        eprintln!(
            "{name}: dirty {ranges:?}, {} of {} frames rendered again, {hits} cache hits, \
             {} source frames decoded, {} seeks",
            done.rendered.len(),
            done.played,
            done.decoded,
            done.seeks
        );
        assert_eq!(done.rendered, expected, "{name}: frames rendered again");
        assert!(
            done.rendered.iter().all(|t| ranges.contains(*t)),
            "{name}: rendered outside the dirty ranges"
        );
        if expected.is_empty() {
            assert_eq!(
                (done.decoded, done.seeks),
                (0, 0),
                "{name}: a sequence the cache holds whole decodes nothing"
            );
        } else {
            assert!(
                done.decoded < cold.decoded,
                "{name}: cached runs were decoded"
            );
        }
        // A cache hit inside the dirty ranges and one outside them show what a fresh
        // render of the new plan shows.
        for t in [20, 45] {
            let shown = common::read_rgba8(&gpu, &pictures[&t]);
            assert!(
                shown == single_frame(&gpu, &next, canvas, t),
                "{name}: frame {t} differs from a fresh render"
            );
        }
        project = edited;
    }
    let stats = playback.stats();
    assert_eq!(stats.replans, 6);
    assert_eq!(stats.refills, 0);
    playback.shutdown();
}

/// A renderer whose cache is a map from (span hash, frame): renders nothing, presents
/// the frame and the key it was rendered under, and counts renders.
#[derive(Clone, Default)]
struct Keyed {
    cache: Arc<Mutex<HashMap<(SegmentHash, i64), ()>>>,
    cached: CachedFrames,
    rendered: Arc<Mutex<Vec<i64>>>,
}

impl Keyed {
    /// The cache lets go of frame `t` without the index hearing of it yet.
    fn evict(&self, plan: &TimelinePlan, t: i64) {
        self.cache
            .lock()
            .expect("cache")
            .remove(&(plan.span_hash(t), t));
    }
}

impl FrameRender for Keyed {
    type Frame = ();
    type Picture = SegmentHash;

    fn render_frame(
        &mut self,
        plan: &TimelinePlan,
        t: i64,
        _: &[Layer<()>],
        _: QualityLevel,
    ) -> (SegmentHash, bool) {
        let key = plan.span_hash(t);
        let mut cache = self.cache.lock().expect("cache");
        if cache.contains_key(&(key, t)) {
            return (key, true);
        }
        cache.insert((key, t), ());
        self.cached.insert(t);
        self.rendered.lock().expect("rendered").push(t);
        (key, false)
    }

    fn lookup_frame(
        &mut self,
        plan: &TimelinePlan,
        t: i64,
        _: QualityLevel,
    ) -> Option<SegmentHash> {
        let key = plan.span_hash(t);
        self.cache
            .lock()
            .expect("cache")
            .contains_key(&(key, t))
            .then_some(key)
    }

    fn replan(&mut self, plan: &TimelinePlan, dirty: &DirtyFrames) {
        let cache = self.cache.lock().expect("cache");
        self.cached
            .retain(|t| !dirty.contains(*t) || cache.contains_key(&(plan.span_hash(*t), *t)));
    }

    fn cached_frames(&self) -> CachedFrames {
        self.cached.clone()
    }

    fn counters(&self) -> (RenderStats, FrameCacheStats) {
        Default::default()
    }
}

#[test]
fn an_edit_while_playing_swaps_the_plan_and_keeps_unchanged_lanes() {
    let rate = Rational::new(FPS, 1);
    let p = Project::new("stand-in", "stand-in");
    let (p, media) = p
        .add_media("stand-in", "/nowhere/stand-in.mov")
        .expect("media");
    let (p, seq) = p.add_sequence("main", rate, 48_000).expect("sequence");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("V1");
    let clip = |start, end| ClipSpec {
        media,
        range: range(start, end),
        source_start: at(start),
    };
    let (p, _) = p.insert_clip(v1, clip(0, 300)).expect("clip 1");
    let (p, second) = p.insert_clip(v1, clip(300, 600)).expect("clip 2");
    let before = Arc::new(TimelinePlan::build(&p, seq).expect("plan"));
    let renderer = Keyed::default();
    let mut config = stand_in::config(&ThreadRegistry::new(), 2);
    config.quality = None;
    // Nobody takes frames while the edit comes, so render waits a few frames past the
    // 100th, in the first clip.
    let (mut playback, mut presented) = stand_in::start(
        before.clone(),
        renderer.clone(),
        config,
        Duration::from_micros(300),
    );
    playback.play();
    let mut shown = Vec::new();
    while shown.len() < 100 {
        let frame = presented.pop_timeout(WAIT).expect("frame").frame;
        shown.push((frame.frame, frame.texture));
    }
    // Fading clip 2 changes frames 300..600; its decode segment stays the same.
    let edited = p.set_clip_opacity(second, 0.5).expect("opacity");
    let after = Arc::new(TimelinePlan::build(&edited, seq).expect("plan"));
    let dirty = DirtyRanges::between(&p, &edited).sequence(seq);
    assert_eq!(dirty, DirtyFrames::Ranges(vec![300..600]));
    assert_eq!(before.segments().segments(), after.segments().segments());
    playback
        .set_plan(after.clone(), dirty)
        .expect("same format");
    while shown.last().is_none_or(|(t, _)| *t < 599) {
        let frame = presented.pop_timeout(WAIT).expect("frame").frame;
        shown.push((frame.frame, frame.texture));
    }
    let order: Vec<i64> = shown.iter().map(|(t, _)| *t).collect();
    assert_eq!(
        order,
        (0..600).collect::<Vec<_>>(),
        "every frame once, in order"
    );
    for (t, key) in &shown {
        if *t >= 300 {
            assert_eq!(*key, after.span_hash(*t), "frame {t} under the new plan");
        } else {
            assert_eq!(*key, before.span_hash(*t), "frame {t}");
        }
    }
    let lookahead = playback.lookahead_stats();
    assert_eq!(
        lookahead.seeks, 2,
        "one pre-roll per segment: the edit kept clip 2's lane and its decoder"
    );
    assert_eq!(playback.stats().replans, 1);

    // Again from the start: every frame is cached, nothing decodes or renders.
    let rendered = renderer.rendered.lock().expect("rendered").len();
    assert_eq!(rendered, 600);
    let (again, _) = pass(&playback, &mut presented, 600);
    assert_eq!(again.played, 600);
    assert!(again.rendered.is_empty(), "{:?}", again.rendered);
    assert_eq!((again.decoded, again.seeks), (0, 0));

    // The cache lets go of frame 400 after the look-ahead decided to skip it: render
    // finds it missing, and the look-ahead decodes it again.
    renderer.evict(&after, 400);
    let (refilled, _) = pass(&playback, &mut presented, 600);
    assert_eq!(refilled.rendered, BTreeSet::from([400]));
    assert_eq!(playback.stats().refills, 1);
    assert!(refilled.decoded >= 1 && refilled.seeks >= 1);
    eprintln!(
        "stand-in: {} frames, {} seeks; second pass decoded {}; refill decoded {} with {} seeks",
        shown.len(),
        lookahead.seeks,
        again.decoded,
        refilled.decoded,
        refilled.seeks
    );
    playback.shutdown();
}
