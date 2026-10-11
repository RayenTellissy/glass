//! Look-ahead decode across cuts and transitions: frames arrive in order and on time
//! through cuts between two files, both sides of a transition decode, lanes never hold
//! more than the depth, and seek and stop drain without leaving threads behind.

mod common;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use audio::Clock;
use audio::video_sync::VideoSync;
use engine::frame_queue::FrameStamp;
use engine::lookahead::{
    FramesError, GopIndexes, Layer, Lookahead, LookaheadConfig, LookaheadFrames, MediaDecoder,
    MediaDecoderOptions, PlanSegment, PreRoll, SegmentDecoder, SegmentList, SourceId, SourceRef,
};
use engine::threads::ThreadRegistry;
use media::{DecoderOptions, HwPreference, MediaError, Rational, Timestamp, VideoFrame};
use render::frame_cache::SegmentHash;

const FPS: i64 = 30;
const WAIT: Duration = Duration::from_secs(10);

fn timeline_rate() -> Rational {
    Rational::new(1, FPS as i32)
}

/// Both fixtures use 512 ticks per frame: 1/15360 at 30 fps, 1/12800 at 25 fps.
struct Fixture {
    source: SourceRef,
    fps: i64,
    time_base: Rational,
}

impl Fixture {
    fn at_frame(&self, frame: i64) -> Timestamp {
        Timestamp::new(frame * 512, self.time_base)
    }
}

fn fixtures() -> Option<(Fixture, Fixture)> {
    let a = common::fixture("h264_1080p30.mp4")?;
    let b = common::fixture("hevc_10bit_1080p25.mp4")?;
    Some((
        Fixture {
            source: SourceRef::new(SourceId(1), a),
            fps: 30,
            time_base: Rational::new(1, 15360),
        },
        Fixture {
            source: SourceRef::new(SourceId(2), b),
            fps: 25,
            time_base: Rational::new(1, 12800),
        },
    ))
}

fn segment(
    fixture: &Fixture,
    source_frame: i64,
    timeline: std::ops::Range<i64>,
    layer: u32,
) -> PlanSegment {
    PlanSegment {
        source: fixture.source.clone(),
        timeline: timeline.clone(),
        source_start: fixture.at_frame(source_frame),
        layer,
        hash: SegmentHash::from(timeline.start as u64),
    }
}

/// The source frame timeline frame `t` of a segment shows: the last one at or before
/// `start + (t - timeline start) / 30 s`.
fn expected_source_frame(fixture: &Fixture, source_frame: i64, offset: i64) -> i64 {
    source_frame + (offset * fixture.fps).div_euclid(FPS)
}

/// The cut test's plan: three cuts across two files, every segment starting between
/// keyframes so its seek has to decode forward.
/// (fixture, first source frame, timeline range) per segment.
type CutSpec = [(usize, i64, std::ops::Range<i64>)];

/// The 1080p cut test: three cuts across two files, every segment starting between
/// keyframes so its seek has to decode forward.
const CUTS_1080P: [(usize, i64, std::ops::Range<i64>); 4] = [
    (0, 0, 0..30),
    (1, 10, 30..55),
    (0, 45, 55..85),
    (1, 24, 85..115),
];

/// Three cuts inside the 4K file, each landing mid-GOP.
const CUTS_4K: [(usize, i64, std::ops::Range<i64>); 4] = [
    (0, 10, 0..15),
    (0, 40, 15..30),
    (0, 5, 30..45),
    (0, 35, 45..60),
];

fn cuts_plan(fixtures: &[&Fixture], spec: &CutSpec) -> (SegmentList, Vec<(usize, i64)>) {
    let plan = SegmentList::new(
        spec.iter()
            .map(|(f, frame, range)| segment(fixtures[*f], *frame, range.clone(), 0)),
    );
    let starts = spec.iter().map(|(f, frame, _)| (*f, *frame)).collect();
    (plan, starts)
}

fn media_decoders(options: MediaDecoderOptions) -> impl FnMut(usize) -> MediaDecoder {
    let gops = GopIndexes::new();
    move |_| MediaDecoder::new(options, gops.clone())
}

fn next(
    frames: &mut LookaheadFrames<VideoFrame>,
    layers: &mut Vec<Layer<VideoFrame>>,
) -> FrameStamp {
    frames.next_into_timeout(layers, WAIT).expect("next frame")
}

/// Checks a delivered cut-plan frame against the plan; returns the error, if any.
fn check_cut_frame(
    plan: &SegmentList,
    starts: &[(usize, i64)],
    fixtures: &[&Fixture],
    stamp: &FrameStamp,
    layers: &[Layer<VideoFrame>],
) -> Result<(), String> {
    let t = stamp.index;
    let (i, segment) = plan
        .segments()
        .iter()
        .enumerate()
        .find(|(_, s)| s.timeline.contains(&t))
        .ok_or_else(|| format!("frame {t} is outside the plan"))?;
    let (f, first) = starts[i];
    let fixture = fixtures[f];
    let [layer] = layers else {
        return Err(format!("frame {t}: {} layers, expected 1", layers.len()));
    };
    let expected = fixture.at_frame(expected_source_frame(
        fixture,
        first,
        t - segment.timeline.start,
    ));
    if layer.source != fixture.source.id || layer.source_pts != Some(expected) {
        return Err(format!(
            "frame {t}: source {:?} at {:?}, expected {:?} at {expected:?}",
            layer.source, layer.source_pts, fixture.source.id
        ));
    }
    if layer.frame.is_none() {
        return Err(format!("frame {t}: no picture"));
    }
    Ok(())
}

struct Timing {
    startup: Duration,
    late: usize,
    /// Arrival minus the refresh that shows the frame, per frame: negative is early.
    lateness: Vec<f64>,
    cut_slack: Vec<f64>,
    hardware_frames: usize,
    frames: usize,
}

/// Plays the cut plan in real time: a presenter on the master clock (the system stand-in)
/// shows one frame per 60 Hz refresh through `VideoSync`, and render pulls frames up to
/// three ahead of what is on screen, like the present queue. A frame is late when it
/// arrives after the refresh that first shows it.
fn play_cuts(
    fixtures: &[&Fixture],
    spec: &CutSpec,
    depth: usize,
    options: MediaDecoderOptions,
) -> (Timing, engine::lookahead::LookaheadStats) {
    let (plan, starts) = cuts_plan(fixtures, spec);
    let last = plan.segments().last().expect("segments").timeline.end;
    let cuts: Vec<i64> = plan.segments()[1..]
        .iter()
        .map(|s| s.timeline.start)
        .collect();
    let config = LookaheadConfig {
        frame_duration: timeline_rate(),
        registry: ThreadRegistry::new(),
        depth,
        ..LookaheadConfig::default()
    };
    let (lookahead, mut frames) =
        Lookahead::start(plan.clone(), config, media_decoders(options)).expect("start");

    let mut layers = Vec::new();
    let started = Instant::now();
    let first = next(&mut frames, &mut layers);
    let startup = started.elapsed();
    assert_eq!(first.index, 0);
    let mut errors = Vec::new();
    let mut hardware_frames = 0;
    let mut check = |stamp: &FrameStamp, layers: &[Layer<VideoFrame>]| {
        if let Err(e) = check_cut_frame(&plan, &starts, fixtures, stamp, layers) {
            errors.push(e);
        }
        hardware_frames += layers
            .iter()
            .filter(|l| l.frame.as_ref().is_some_and(|f| f.hardware().is_some()))
            .count();
    };
    check(&first, &layers);

    const AHEAD: i64 = 3;
    let refresh = 1.0 / 60.0;
    let t0 = Instant::now();
    let clock = Clock::system(48_000);
    let mut sync = VideoSync::new(FPS as u32, 1);
    let mut arrival = vec![t0; last as usize];
    let mut shown_at: Vec<Option<Instant>> = vec![None; last as usize];
    let mut delivered = 0;
    for k in 0.. {
        let at = t0 + Duration::from_secs_f64(f64::from(k) * refresh);
        let want = sync.frame_at(clock.at(at), refresh).max(0);
        if want >= last {
            break;
        }
        while delivered < (want + AHEAD).min(last - 1) {
            let stamp = next(&mut frames, &mut layers);
            arrival[stamp.index as usize] = Instant::now();
            delivered = stamp.index;
            check(&stamp, &layers);
        }
        if let Some(wait) = at.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        shown_at[want as usize].get_or_insert(at);
    }
    assert!(errors.is_empty(), "wrong frames: {errors:#?}");

    let mut lateness = Vec::new();
    let mut cut_slack = Vec::new();
    // Frame 0 is the pre-roll before the clock starts, reported as the startup time.
    for (n, shown) in shown_at.iter().enumerate().skip(1) {
        let Some(shown) = shown else { continue };
        let late = if arrival[n] > *shown {
            (arrival[n] - *shown).as_secs_f64()
        } else {
            -(*shown - arrival[n]).as_secs_f64()
        };
        lateness.push(late);
        if cuts.contains(&(n as i64)) {
            cut_slack.push(-late);
        }
    }
    let stats = lookahead.stats();
    assert!(stats.max_lane_queued <= depth, "{stats:?}");
    drop(frames);
    lookahead.shutdown();
    let late = lateness.iter().filter(|l| **l > 0.0).count();
    (
        Timing {
            startup,
            late,
            lateness,
            cut_slack,
            hardware_frames,
            frames: last as usize,
        },
        stats,
    )
}

fn report(name: &str, timing: &Timing, stats: &engine::lookahead::LookaheadStats) {
    let mut sorted = timing.lateness.clone();
    sorted.sort_by(f64::total_cmp);
    let ms = |s: f64| s * 1000.0;
    let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p).round() as usize];
    println!(
        "{name}: {} frames shown, {} late; lateness ms (arrival minus refresh, negative is early): \
         max {:.1}, p99 {:.1}, median {:.1}; cut slack ms {:?}; startup {:.1} ms; \
         {} seeks, mean {:.1} ms, max {:.1} ms; {} of {} frames from hardware",
        timing.lateness.len(),
        timing.late,
        ms(pct(1.0)),
        ms(pct(0.99)),
        ms(pct(0.5)),
        timing
            .cut_slack
            .iter()
            .map(|s| (ms(*s) * 10.0).round() / 10.0)
            .collect::<Vec<_>>(),
        ms(timing.startup.as_secs_f64()),
        stats.seeks,
        ms(stats.seek_total.as_secs_f64()) / stats.seeks.max(1) as f64,
        ms(stats.seek_max.as_secs_f64()),
        timing.hardware_frames,
        timing.frames,
    );
}

/// The decoder set-ups compared: the default first.
fn decoder_modes() -> [(&'static str, MediaDecoderOptions); 3] {
    let default = MediaDecoderOptions::default();
    let other = match default.pre_roll {
        PreRoll::Steady => PreRoll::Software,
        PreRoll::Software => PreRoll::Steady,
    };
    let name = |pre_roll| match pre_roll {
        PreRoll::Steady => "hardware seek and playback",
        PreRoll::Software => "software pre-roll, hardware playback",
    };
    [
        (name(default.pre_roll), default),
        (
            name(other),
            MediaDecoderOptions {
                pre_roll: other,
                ..default
            },
        ),
        (
            "software seek and playback",
            MediaDecoderOptions {
                decoder: DecoderOptions::with_hw(HwPreference::Off),
                ..default
            },
        ),
    ]
}

/// Lateness is wall-clock time, held to the budget (no late frame) by the shared timing
/// rules (`common::timing`): every frame under `GLASS_STRICT_TIMING=1`, the median
/// otherwise.
#[test]
fn three_cuts_across_two_files_play_with_no_late_frames_at_1080p() {
    let Some((a, b)) = fixtures() else {
        return;
    };
    let _serial = common::timing::serial();
    let depth = engine::lookahead::default_depth(1920, 1080, 8);
    let mut default_lateness = None;
    for (name, options) in decoder_modes() {
        let (timing, stats) = play_cuts(&[&a, &b], &CUTS_1080P, depth, options);
        report(&format!("1080p, depth {depth}, {name}"), &timing, &stats);
        assert_eq!(stats.decode_errors, 0);
        default_lateness.get_or_insert(timing.lateness);
    }
    // Only the default is held to the budget; the others are context for it.
    let lateness = common::timing::Samples::new(
        default_lateness
            .expect("a decoder mode")
            .iter()
            .map(|s| s * 1000.0)
            .collect(),
    );
    common::timing::assert_wall_within(
        "frame lateness with the default decoder",
        &lateness,
        common::timing::Stat::Max,
        0.0,
    );
}

/// Context for the default pre-roll at 4K, where VideoToolbox seeks are slowest; reported,
/// not held to a lateness budget.
#[test]
fn cuts_inside_a_4k_file_report_their_timing() {
    let Some(path) = common::fixture("h264_4k30.mp4") else {
        return;
    };
    let fixture = Fixture {
        source: SourceRef::new(SourceId(3), path),
        fps: 30,
        time_base: Rational::new(1, 15360),
    };
    let _serial = common::timing::serial();
    let depth = engine::lookahead::default_depth(3840, 2160, 8);
    for (name, options) in decoder_modes() {
        let (timing, stats) = play_cuts(&[&fixture], &CUTS_4K, depth, options);
        report(&format!("4K, depth {depth}, {name}"), &timing, &stats);
        assert_eq!(stats.decode_errors, 0);
    }
}

#[test]
fn a_transition_delivers_both_sides() {
    let Some((a, b)) = fixtures() else {
        return;
    };
    let plan = SegmentList::new([segment(&a, 0, 0..40, 0), segment(&b, 0, 25..60, 1)]);
    let config = LookaheadConfig {
        decode_workers: 2,
        frame_duration: timeline_rate(),
        registry: ThreadRegistry::new(),
        ..LookaheadConfig::default()
    };
    let (lookahead, mut frames) =
        Lookahead::start(plan, config, media_decoders(MediaDecoderOptions::default()))
            .expect("start");
    let mut layers = Vec::new();
    for t in 0..60 {
        let stamp = next(&mut frames, &mut layers);
        assert_eq!(stamp.index, t);
        let mut expected = Vec::new();
        if t < 40 {
            expected.push((0, a.source.id, a.at_frame(t)));
        }
        if t >= 25 {
            let k = expected_source_frame(&b, 0, t - 25);
            expected.push((1, b.source.id, b.at_frame(k)));
        }
        let got: Vec<_> = layers
            .iter()
            .map(|l| (l.layer, l.source, l.source_pts.expect("picture")))
            .collect();
        assert_eq!(got, expected, "frame {t}");
        assert!(layers.iter().all(|l| l.frame.is_some()), "frame {t}");
    }
    assert_eq!(
        frames.next_into_timeout(&mut layers, WAIT).unwrap_err(),
        FramesError::Finished
    );
    assert_eq!(lookahead.stats().decode_errors, 0);
}

#[test]
fn seek_and_stop_drain_cleanly_with_no_threads_left() {
    let Some((a, b)) = fixtures() else {
        return;
    };
    let (plan, starts) = cuts_plan(&[&a, &b], &CUTS_1080P);
    let registry = ThreadRegistry::new();
    let config = LookaheadConfig {
        frame_duration: timeline_rate(),
        registry: registry.clone(),
        ..LookaheadConfig::default()
    };
    let workers = config.decode_workers.max(2);
    let (lookahead, mut frames) = Lookahead::start(
        plan.clone(),
        config,
        media_decoders(MediaDecoderOptions::default()),
    )
    .expect("start");
    assert_eq!(registry.len(), workers + 1);
    let mut layers = Vec::new();
    for t in 0..10 {
        assert_eq!(next(&mut frames, &mut layers).index, t);
    }
    // Forward into the third segment, back into the first, then forward again before the
    // previous seek has delivered anything.
    for (target, generation) in [(70, 1), (5, 2)] {
        assert_eq!(lookahead.seek(target), generation);
        let stamp = next(&mut frames, &mut layers);
        assert_eq!((stamp.index, stamp.generation), (target, generation));
        check_cut_frame(&plan, &starts, &[&a, &b], &stamp, &layers).expect("frame after seek");
        assert_eq!(next(&mut frames, &mut layers).index, target + 1);
    }
    lookahead.seek(40);
    let generation = lookahead.seek(100);
    let stamp = next(&mut frames, &mut layers);
    assert_eq!((stamp.index, stamp.generation), (100, generation));
    check_cut_frame(&plan, &starts, &[&a, &b], &stamp, &layers).expect("frame after seeks");

    // Let every lane fill, so workers are parked on full queues, then stop.
    std::thread::sleep(Duration::from_millis(300));
    let (done, stopped) = mpsc::channel();
    std::thread::spawn(move || {
        lookahead.shutdown();
        let _ = done.send(());
    });
    stopped.recv_timeout(WAIT).expect("every thread joins");
    assert!(
        registry.is_empty(),
        "threads left: {:?}",
        registry.snapshot()
    );
    assert_eq!(
        frames.next_into_timeout(&mut layers, WAIT).unwrap_err(),
        FramesError::Disconnected
    );
}

/// Frames of a made-up 30 fps source of `len` frames: each frame is (source id, index).
struct Counting {
    len: i64,
    source: u64,
    at: i64,
    delay: Duration,
}

impl Counting {
    fn new(delay: Duration) -> Self {
        Self {
            len: 1_000,
            source: 0,
            at: 0,
            delay,
        }
    }
}

impl SegmentDecoder for Counting {
    type Frame = (u64, i64);

    fn seek(&mut self, source: &SourceRef, at: Timestamp) -> Result<(), MediaError> {
        self.source = source.id.0;
        self.at = at.rescale_floor(timeline_rate()).value;
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<(Timestamp, (u64, i64))>, MediaError> {
        if self.at >= self.len {
            return Ok(None);
        }
        std::thread::sleep(self.delay);
        let frame = self.at;
        self.at += 1;
        Ok(Some((
            Timestamp::new(frame, timeline_rate()),
            (self.source, frame),
        )))
    }
}

fn counting_segment(source: u64, source_frame: i64, timeline: std::ops::Range<i64>) -> PlanSegment {
    PlanSegment {
        source: SourceRef::new(SourceId(source), PathBuf::from(format!("{source}.mov"))),
        timeline: timeline.clone(),
        source_start: Timestamp::new(source_frame, timeline_rate()),
        layer: 0,
        hash: SegmentHash::from(timeline.start as u64),
    }
}

/// Polls until `value` has not changed for 100 ms.
fn settled(mut value: impl FnMut() -> u64) -> u64 {
    let deadline = Instant::now() + WAIT;
    let mut last = value();
    let mut since = Instant::now();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        let now = value();
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() > Duration::from_millis(100) {
            break;
        }
    }
    last
}

#[test]
fn lanes_never_hold_more_than_the_depth() {
    let depth = 4;
    let workers = 2;
    let plan = SegmentList::new([
        counting_segment(1, 100, 0..50),
        counting_segment(2, 0, 50..100),
        counting_segment(1, 300, 100..150),
    ]);
    let config = LookaheadConfig {
        decode_workers: workers,
        depth,
        frame_duration: timeline_rate(),
        registry: ThreadRegistry::new(),
    };
    let (lookahead, mut frames) =
        Lookahead::start(plan, config, |_| Counting::new(Duration::ZERO)).expect("start");

    // Nothing consumed: each worker fills its lane to the depth and parks; the third
    // segment waits for a worker.
    let queued = settled(|| lookahead.stats().frames_queued);
    assert_eq!(queued, (workers * depth) as u64);
    assert_eq!(lookahead.stats().max_lane_queued, depth);

    let mut layers = Vec::new();
    let mut expected = (100..150).chain(0..50).chain(300..350).map(|k| k as i64);
    for t in 0..150 {
        let stamp = frames.next_into_timeout(&mut layers, WAIT).expect("frame");
        assert_eq!(stamp.index, t);
        let [layer] = &layers[..] else {
            panic!("frame {t}: {} layers", layers.len());
        };
        let (_, k) = **layer.frame.as_ref().expect("picture");
        assert_eq!(Some(k), expected.next(), "frame {t}");
    }
    let stats = lookahead.stats();
    assert!(stats.max_lane_queued <= depth, "{stats:?}");
    assert!(frames.max_total_queued() <= (workers + 1) * depth);
    assert_eq!(stats.frames_queued, 150);
}

#[test]
fn many_quick_seeks_land_on_the_last_one() {
    let plan = SegmentList::new([
        counting_segment(1, 0, 0..100),
        counting_segment(2, 0, 100..200),
    ]);
    let registry = ThreadRegistry::new();
    let config = LookaheadConfig {
        decode_workers: 2,
        depth: 3,
        frame_duration: timeline_rate(),
        registry: registry.clone(),
    };
    let (lookahead, mut frames) =
        Lookahead::start(plan, config, |_| Counting::new(Duration::from_millis(1))).expect("start");
    let mut layers = Vec::new();
    let mut generation = 0;
    for i in 0..20 {
        generation = lookahead.seek((i * 37) % 200);
    }
    let target = (19 * 37) % 200;
    let stamp = frames.next_into_timeout(&mut layers, WAIT).expect("frame");
    assert_eq!((stamp.index, stamp.generation), (target, generation));
    let (source, k) = **layers[0].frame.as_ref().expect("picture");
    assert_eq!((source, k), (2, target - 100));
    drop(lookahead);
    assert!(registry.is_empty());
    assert!(frames.next_into_timeout(&mut layers, WAIT).is_err());
}

#[test]
fn too_many_overlapping_segments_drop_the_extra_one() {
    let plan = SegmentList::new([
        counting_segment(1, 0, 0..30),
        counting_segment(2, 0, 10..30),
        counting_segment(3, 0, 20..30),
    ]);
    let config = LookaheadConfig {
        decode_workers: 2,
        depth: 3,
        frame_duration: timeline_rate(),
        registry: ThreadRegistry::new(),
    };
    let (lookahead, mut frames) =
        Lookahead::start(plan, config, |_| Counting::new(Duration::ZERO)).expect("start");
    let mut layers = Vec::new();
    for t in 0..30 {
        let stamp = frames.next_into_timeout(&mut layers, WAIT).expect("frame");
        assert_eq!(stamp.index, t);
        assert_eq!(layers.len(), if t < 10 { 1 } else { 2 }, "frame {t}");
    }
    assert_eq!(lookahead.stats().segments_dropped, 1);
}
