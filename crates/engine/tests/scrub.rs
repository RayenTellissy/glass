//! Scrub mode with a manual clock and a made-up decoder: the nearest-frame answer, the
//! 40 ms pause rule, superseding, prefetching, the UI side never waiting, and the clean
//! hand-back to playback.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::lookahead::{Layer, PlanSegment, SegmentList, SourceId, SourceRef};
use engine::scrub::{
    CachedFrames, Decoded, ManualClock, Ready, ScrubConfig, ScrubDecoder, ScrubRender, Scrubber,
    Shown,
};
use engine::threads::ThreadRegistry;
use media::{MediaError, Rational, Timestamp};
use render::frame_cache::SegmentHash;

const WAIT: Duration = Duration::from_secs(10);
const PAUSE: Duration = Duration::from_millis(40);

fn rate() -> Rational {
    Rational::new(1, 30)
}

/// A 30 fps source with a keyframe every 30 frames; source time ticks are frames.
struct Fake {
    source: u64,
    /// Decoding this source frame spins until cancelled, after saying so on `entered`.
    hold: Option<i64>,
    entered: Sender<i64>,
    calls: Arc<Mutex<Vec<i64>>>,
}

impl ScrubDecoder for Fake {
    type Frame = (u64, i64);

    fn decode_exact(
        &mut self,
        at: Timestamp,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Decoded<(u64, i64)>, MediaError> {
        let frame = at.rescale_floor(rate()).value;
        self.calls.lock().unwrap().push(frame);
        if self.hold == Some(frame) {
            let _ = self.entered.send(frame);
            let started = Instant::now();
            while !cancel() {
                assert!(
                    started.elapsed() < WAIT,
                    "decode of {frame} never cancelled"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            return Ok(Decoded::Cancelled);
        }
        Ok(Decoded::Frame(at, (self.source, frame)))
    }

    fn keyframes(&mut self, from: Timestamp, to: Timestamp, out: &mut Vec<Timestamp>) {
        let from = from.rescale_floor(rate()).value;
        let to = to.rescale_floor(rate()).value;
        let first = from.div_euclid(30) * 30;
        out.extend(
            (first..=to)
                .step_by(30)
                .filter(|k| *k >= from)
                .map(|k| Timestamp::new(k, rate())),
        );
    }
}

/// What render was asked for: (timeline frame, (source, source frame) per layer).
type Rendered = Arc<Mutex<Vec<(i64, Vec<Option<(u64, i64)>>)>>>;

struct FakeRender {
    rendered: Rendered,
    started: Arc<Mutex<Vec<i64>>>,
    /// Rendering this frame waits for a message on `release`.
    hold: Option<(i64, Receiver<()>)>,
}

impl ScrubRender<(u64, i64)> for FakeRender {
    fn render(&mut self, frame: i64, layers: &[Layer<(u64, i64)>]) {
        self.started.lock().unwrap().push(frame);
        if let Some((held, release)) = &self.hold
            && *held == frame
        {
            let _ = release.recv_timeout(WAIT);
        }
        let layers = layers.iter().map(|l| l.frame.as_deref().copied()).collect();
        self.rendered.lock().unwrap().push((frame, layers));
    }
}

fn segment(
    source: u64,
    timeline: std::ops::Range<i64>,
    source_frame: i64,
    layer: u32,
) -> PlanSegment {
    PlanSegment {
        source: SourceRef::new(SourceId(source), PathBuf::from(format!("{source}.mov"))),
        timeline: timeline.clone(),
        source_start: Timestamp::new(source_frame, rate()),
        layer,
        hash: SegmentHash::from(timeline.start as u64),
    }
}

struct Rig {
    scrubber: Scrubber,
    clock: Arc<ManualClock>,
    cached: CachedFrames,
    rendered: Rendered,
    render_started: Arc<Mutex<Vec<i64>>>,
    calls: Arc<Mutex<Vec<i64>>>,
    entered: Receiver<i64>,
    registry: Arc<ThreadRegistry>,
}

struct Options {
    plan: SegmentList,
    cached: &'static [i64],
    prefetch_per_move: usize,
    hold_decode: Option<i64>,
    hold_render: Option<(i64, Receiver<()>)>,
    failing_source: Option<u64>,
    shown: Option<i64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            plan: SegmentList::new([segment(1, 0..300, 0, 0)]),
            cached: &[],
            prefetch_per_move: 0,
            hold_decode: None,
            hold_render: None,
            failing_source: None,
            shown: None,
        }
    }
}

fn rig(options: Options) -> Rig {
    let clock = Arc::new(ManualClock::new());
    let cached = CachedFrames::new();
    for &frame in options.cached {
        cached.insert(frame);
    }
    let rendered = Rendered::default();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (entered_tx, entered) = mpsc::channel();
    let registry = ThreadRegistry::new();
    let config = ScrubConfig {
        pause: PAUSE,
        frame_duration: rate(),
        prefetch_per_move: options.prefetch_per_move,
        registry: registry.clone(),
        ..ScrubConfig::default()
    };
    let hold = options.hold_decode;
    let failing = options.failing_source;
    let decoder_calls = calls.clone();
    let make_decoder = move |source: &SourceRef| {
        if Some(source.id.0) == failing {
            return Err(MediaError::Image {
                path: source.path.to_path_buf(),
                message: "test source".into(),
            });
        }
        Ok(Fake {
            source: source.id.0,
            hold,
            entered: entered_tx.clone(),
            calls: decoder_calls.clone(),
        })
    };
    let render_started = Arc::new(Mutex::new(Vec::new()));
    let render = FakeRender {
        rendered: rendered.clone(),
        started: render_started.clone(),
        hold: options.hold_render,
    };
    let scrubber = Scrubber::start(
        options.plan,
        config,
        cached.clone(),
        clock.clone(),
        make_decoder,
        render,
        options.shown,
    )
    .expect("scrub thread starts");
    Rig {
        scrubber,
        clock,
        cached,
        rendered,
        render_started,
        calls,
        entered,
        registry,
    }
}

impl Rig {
    fn ready(&mut self) -> Ready {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(ready) = self.scrubber.poll_ready() {
                return ready;
            }
            assert!(Instant::now() < deadline, "no exact frame published");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn rendered_frames(&self) -> Vec<i64> {
        self.rendered
            .lock()
            .unwrap()
            .iter()
            .map(|(f, _)| *f)
            .collect()
    }

    fn wait_for(&self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn shown(frame: i64, exact: bool) -> Option<Shown> {
    Some(Shown { frame, exact })
}

#[test]
fn requests_answer_with_the_nearest_cached_frame_toward_the_motion() {
    let mut rig = rig(Options {
        cached: &[10, 50],
        ..Options::default()
    });
    let s = &mut rig.scrubber;
    // First request: no direction, the closer frame.
    assert_eq!(s.request(20), shown(10, false));
    // Moving forward: 50 is 26 ahead, 10 is 14 behind; ahead wins within twice as far.
    assert_eq!(s.request(24), shown(50, false));
    // Moving backward from 22: 10 is 12 ahead, 50 is 28 behind.
    assert_eq!(s.request(22), shown(10, false));
    assert_eq!(s.request(44), shown(50, false));
    // Moving backward from 43, but 10 is more than twice as far as 50.
    assert_eq!(s.request(43), shown(50, false));
    assert_eq!(s.request(50), shown(50, true));
    // Repeating the last frame is not a new request.
    assert_eq!(s.request(50), shown(50, true));
    let stats = s.stats();
    assert_eq!(
        (
            stats.requests,
            stats.exact_hits,
            stats.nearest_hits,
            stats.held
        ),
        (6, 1, 5, 0)
    );
}

#[test]
fn with_nothing_cached_the_frame_on_screen_stays() {
    let mut rig = rig(Options {
        shown: Some(5),
        ..Options::default()
    });
    assert_eq!(rig.scrubber.request(80), shown(5, false));
    assert_eq!(rig.scrubber.request(5), shown(5, true));
    assert_eq!(rig.scrubber.stats().held, 2);

    let mut empty = self::rig(Options::default());
    assert_eq!(empty.scrubber.request(80), None);
}

#[test]
fn the_exact_frame_is_decoded_only_after_a_40_ms_pause() {
    let mut rig = rig(Options::default());
    // A pointer moving every 20 ms never pauses long enough.
    for frame in [100, 104, 108, 112] {
        rig.scrubber.request(frame);
        rig.clock.advance(Duration::from_millis(20));
    }
    rig.clock.advance(Duration::from_millis(19));
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        rig.calls.lock().unwrap().is_empty(),
        "decoded before the pause"
    );
    assert_eq!(rig.scrubber.poll_ready(), None);

    rig.clock.advance(Duration::from_millis(1));
    assert_eq!(
        rig.ready(),
        Ready {
            frame: 112,
            request: 4
        }
    );
    assert_eq!(*rig.calls.lock().unwrap(), [112]);
    assert_eq!(*rig.rendered.lock().unwrap(), [(112, vec![Some((1, 112))])]);
    assert!(rig.cached.contains(112));
    assert_eq!(rig.scrubber.shown(), shown(112, true));

    // The pause is configurable.
    let clock = Arc::new(ManualClock::new());
    let rendered = Rendered::default();
    let (tx, _rx) = mpsc::channel();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut scrubber = Scrubber::start(
        SegmentList::new([segment(1, 0..300, 0, 0)]),
        ScrubConfig {
            pause: Duration::from_millis(100),
            frame_duration: rate(),
            prefetch_per_move: 0,
            registry: ThreadRegistry::new(),
            ..ScrubConfig::default()
        },
        CachedFrames::new(),
        clock.clone(),
        move |source: &SourceRef| {
            Ok(Fake {
                source: source.id.0,
                hold: None,
                entered: tx.clone(),
                calls: calls.clone(),
            })
        },
        FakeRender {
            rendered: rendered.clone(),
            started: Arc::default(),
            hold: None,
        },
        None,
    )
    .unwrap();
    scrubber.request(7);
    clock.advance(Duration::from_millis(99));
    std::thread::sleep(Duration::from_millis(50));
    assert!(rendered.lock().unwrap().is_empty());
    clock.advance(Duration::from_millis(1));
    let deadline = Instant::now() + WAIT;
    while scrubber.poll_ready().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_cached_exact_frame_is_published_without_decoding() {
    let mut rig = rig(Options {
        cached: &[60],
        ..Options::default()
    });
    assert_eq!(rig.scrubber.request(60), shown(60, true));
    rig.clock.advance(PAUSE);
    assert_eq!(rig.ready().frame, 60);
    assert!(rig.calls.lock().unwrap().is_empty());
    assert_eq!(rig.scrubber.stats().exact_from_cache, 1);
}

#[test]
fn a_newer_request_supersedes_the_decode_in_flight() {
    let mut rig = rig(Options {
        hold_decode: Some(100),
        ..Options::default()
    });
    rig.scrubber.request(100);
    rig.clock.advance(PAUSE);
    assert_eq!(rig.entered.recv_timeout(WAIT), Ok(100));

    rig.scrubber.request(200);
    rig.wait_for("the superseded count", |rig| {
        rig.scrubber.stats().superseded == 1
    });
    rig.clock.advance(PAUSE);
    assert_eq!(
        rig.ready(),
        Ready {
            frame: 200,
            request: 2
        }
    );
    assert_eq!(rig.rendered_frames(), [200]);
    assert!(!rig.cached.contains(100));
    let stats = rig.scrubber.stats();
    assert_eq!((stats.exact_decodes, stats.published), (1, 1));
}

#[test]
fn a_frame_finished_for_an_older_request_is_not_shown() {
    let (release, hold) = mpsc::channel();
    let mut rig = rig(Options {
        hold_render: Some((100, hold)),
        ..Options::default()
    });
    rig.scrubber.request(100);
    rig.clock.advance(PAUSE);
    rig.wait_for("render of 100 to start", |rig| {
        !rig.render_started.lock().unwrap().is_empty()
    });
    // Render of 100 is under way; the pointer moves on before it finishes.
    rig.scrubber.request(150);
    release.send(()).unwrap();
    rig.wait_for("render of 100", |rig| rig.cached.contains(100));
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(rig.scrubber.poll_ready(), None);
    assert_eq!(rig.scrubber.stats().published, 0);
    rig.clock.advance(PAUSE);
    assert_eq!(rig.ready().frame, 150);
}

#[test]
fn requests_never_wait_for_the_worker_or_the_cache_index() {
    let (release, hold) = mpsc::channel();
    let mut rig = rig(Options {
        cached: &[0, 300],
        hold_render: Some((100, hold)),
        ..Options::default()
    });
    rig.scrubber.request(100);
    rig.clock.advance(PAUSE);
    rig.wait_for("render of 100 to start", |rig| {
        !rig.render_started.lock().unwrap().is_empty()
    });

    // The worker is stuck in render; requests still answer at once.
    let started = Instant::now();
    let mut slowest = Duration::ZERO;
    for i in 0..2_000 {
        let before = Instant::now();
        let shown = rig.scrubber.request(101 + i % 150);
        slowest = slowest.max(before.elapsed());
        assert!(shown.is_some());
    }
    eprintln!(
        "2000 requests with the worker blocked: {:.2} ms, slowest {:.3} ms",
        started.elapsed().as_secs_f64() * 1e3,
        slowest.as_secs_f64() * 1e3
    );

    // A writer holding the index: the answer is the frame on screen.
    let on_screen = rig.scrubber.shown();
    let (locked_tx, locked_rx) = mpsc::channel();
    let (unlock_tx, unlock_rx) = mpsc::channel::<()>();
    let cached = rig.cached.clone();
    let writer = std::thread::spawn(move || {
        cached.retain(|_| {
            let _ = locked_tx.send(());
            let _ = unlock_rx.recv_timeout(WAIT);
            true
        });
    });
    locked_rx.recv_timeout(WAIT).unwrap();
    let held_before = rig.scrubber.stats().held;
    let shown = rig.scrubber.request(299);
    assert_eq!(shown.map(|s| s.frame), on_screen.map(|s| s.frame));
    assert_eq!(rig.scrubber.stats().held, held_before + 1);
    drop(unlock_tx);
    writer.join().unwrap();
    assert_eq!(rig.scrubber.request(298), shown_frame(300));

    release.send(()).unwrap();
}

fn shown_frame(frame: i64) -> Option<Shown> {
    shown(frame, false)
}

#[test]
fn moving_prefetches_keyframe_frames_ahead_nearest_first() {
    let mut rig = rig(Options {
        prefetch_per_move: 2,
        ..Options::default()
    });
    rig.scrubber.request(10);
    // The first request has no direction: nothing to prefetch.
    std::thread::sleep(Duration::from_millis(30));
    assert!(rig.rendered_frames().is_empty());

    rig.scrubber.request(12);
    rig.wait_for("two prefetches", |rig| rig.scrubber.stats().prefetched == 2);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(rig.rendered_frames(), [30, 60]);

    // The next move allows two more; only 90 is within 90 frames of 14.
    rig.scrubber.request(14);
    rig.wait_for("a third prefetch", |rig| {
        rig.scrubber.stats().prefetched == 3
    });
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(rig.rendered_frames(), [30, 60, 90]);
    assert_eq!(rig.scrubber.request(28), shown(30, false));

    assert!(rig.calls.lock().unwrap().iter().all(|f| f % 30 == 0));
    assert_eq!(rig.scrubber.stats().exact_decodes, 0);

    // Backward: keyframe frames behind the pointer, nearest first.
    let mut rig = self::rig(Options {
        prefetch_per_move: 2,
        ..Options::default()
    });
    rig.scrubber.request(200);
    rig.scrubber.request(198);
    rig.wait_for("backward prefetches", |rig| {
        rig.scrubber.stats().prefetched == 2
    });
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(rig.rendered_frames(), [180, 150]);
}

#[test]
fn prefetch_skips_cached_frames_and_transitions() {
    let plan = SegmentList::new([
        segment(1, 0..70, 0, 0),
        // A transition over 50..70: frames 50 and 60 have two layers.
        segment(2, 50..200, 0, 1),
    ]);
    let mut rig = rig(Options {
        plan,
        cached: &[30],
        prefetch_per_move: 4,
        ..Options::default()
    });
    rig.scrubber.request(10);
    rig.scrubber.request(11);
    rig.wait_for("a prefetch", |rig| rig.scrubber.stats().prefetched == 1);
    std::thread::sleep(Duration::from_millis(20));
    // In the window 12..=101: source 1's keyframes show first at 30 (cached) and 60 (in
    // the transition), source 2's at 50 (in the transition) and 80.
    assert_eq!(rig.rendered_frames(), [80]);
}

#[test]
fn prefetch_gives_way_when_the_pause_runs_out() {
    let mut rig = rig(Options {
        prefetch_per_move: 2,
        hold_decode: Some(30),
        ..Options::default()
    });
    rig.scrubber.request(10);
    rig.scrubber.request(12);
    assert_eq!(rig.entered.recv_timeout(WAIT), Ok(30));
    rig.clock.advance(PAUSE);
    assert_eq!(rig.ready().frame, 12);
    let stats = rig.scrubber.stats();
    assert_eq!((stats.prefetch_cancelled, stats.exact_decodes), (1, 1));
    assert!(!rig.cached.contains(30));
}

#[test]
fn a_transition_frame_decodes_every_layer_bottom_first() {
    let plan = SegmentList::new([segment(2, 50..200, 0, 1), segment(1, 0..100, 0, 0)]);
    let mut rig = rig(Options {
        plan,
        failing_source: Some(3),
        ..Options::default()
    });
    rig.scrubber.request(60);
    rig.clock.advance(PAUSE);
    rig.ready();
    assert_eq!(
        *rig.rendered.lock().unwrap(),
        [(60, vec![Some((1, 60)), Some((2, 10))])]
    );
}

#[test]
fn a_source_that_fails_to_open_renders_an_empty_layer() {
    let plan = SegmentList::new([segment(3, 0..100, 0, 0)]);
    let mut rig = rig(Options {
        plan,
        failing_source: Some(3),
        ..Options::default()
    });
    rig.scrubber.request(5);
    rig.clock.advance(PAUSE);
    assert_eq!(rig.ready().frame, 5);
    assert_eq!(*rig.rendered.lock().unwrap(), [(5, vec![None])]);
    assert_eq!(rig.scrubber.stats().decode_errors, 1);
    // A gap renders with no layers.
    rig.scrubber.request(150);
    rig.clock.advance(PAUSE);
    assert_eq!(rig.ready().frame, 150);
    assert_eq!(rig.rendered.lock().unwrap()[1], (150, vec![]));
}

#[test]
fn finish_hands_back_the_pointer_and_leaves_no_thread() {
    let mut rig = rig(Options {
        hold_decode: Some(40),
        ..Options::default()
    });
    assert_eq!(rig.registry.len(), 1);
    rig.scrubber.request(20);
    rig.scrubber.request(40);
    rig.clock.advance(PAUSE);
    assert_eq!(rig.entered.recv_timeout(WAIT), Ok(40));
    // Finishing cancels the decode in flight.
    let Rig {
        scrubber, registry, ..
    } = rig;
    let end = scrubber.finish();
    assert_eq!(end.frame, Some(40));
    assert_eq!(end.stats.requests, 2);
    assert_eq!(end.stats.superseded, 1);
    assert_eq!(registry.len(), 0);
}

#[test]
fn cached_frames_follow_inserts_and_removals() {
    let cached = CachedFrames::new();
    for frame in [5, 1, 9] {
        cached.insert(frame);
    }
    cached.remove(5);
    cached.retain(|f| *f < 9);
    assert_eq!(cached.len(), 1);
    assert!(cached.contains(1));
    let set: BTreeSet<i64> = [1].into();
    assert_eq!(
        engine::scrub::nearest(&set, 7, engine::scrub::Direction::Forward, 2.0),
        Some(1)
    );
    cached.clear();
    assert!(cached.is_empty());
}
