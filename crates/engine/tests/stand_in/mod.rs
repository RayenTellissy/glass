//! A stand-in timeline for tests of the playback pipeline that need neither a GPU nor
//! media files: a sequence with one long clip per video track over a file nobody opens,
//! a decoder that makes its frames up (optionally spinning for each, like a software
//! decoder), and a renderer that hands the timeline frame number to the presenter.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::lookahead::{Layer, SegmentDecoder, SourceRef};
use engine::quality::QualityLevel;
use engine::scrub::CachedFrames;
use engine::threads::ThreadRegistry;
use engine::timeline::{
    DirtyFrames, FrameRender, RenderStats, TimelineConfig, TimelinePlan, TimelinePlayback,
};
use media::{MediaError, Rational, Timestamp};
use project::{ClipSpec, Project, Time, TimeRange, TrackKind};
use render::frame_cache::FrameCacheStats;

/// A sequence at `fps` with `tracks` video tracks, each holding one clip over frames
/// `0..frames` (so `tracks` segments overlap everywhere).
pub fn plan(fps: i32, tracks: usize, frames: i64) -> Arc<TimelinePlan> {
    let rate = project::Rational::new(fps, 1);
    let p = Project::new("stand-in", "stand-in");
    let (p, media) = p
        .add_media("stand-in", "/nowhere/stand-in.mov")
        .expect("media");
    let (mut p, seq) = p.add_sequence("main", rate, 48_000).expect("sequence");
    for n in 0..tracks {
        let (q, track) = p
            .add_track(seq, TrackKind::Video, format!("V{}", n + 1), None)
            .expect("track");
        let range = TimeRange::between(Time::ZERO, Time::from_frames(frames, rate).expect("end"))
            .expect("range");
        let (q, _) = q
            .insert_clip(
                track,
                ClipSpec {
                    media,
                    range,
                    source_start: Time::ZERO,
                },
            )
            .expect("clip");
        p = q;
    }
    Arc::new(TimelinePlan::build(&p, seq).expect("plan"))
}

/// Playback settings for the stand-in: `workers` decoders registering in `registry`.
pub fn config(registry: &Arc<ThreadRegistry>, workers: usize) -> TimelineConfig {
    let mut config = TimelineConfig::new((64, 36));
    config.decode_workers = workers;
    config.registry = registry.clone();
    config
}

/// Starts a playback of `plan` with made-up decoders spinning `spin` per frame and
/// `renderer`, on the system clock.
pub fn start<R: FrameRender<Frame = ()>>(
    plan: Arc<TimelinePlan>,
    renderer: R,
    config: TimelineConfig,
    spin: Duration,
) -> (TimelinePlayback<R>, engine::timeline::Presented<R::Picture>) {
    let fps = plan.rate();
    TimelinePlayback::start_with(
        plan,
        renderer,
        audio::Clock::system(48_000),
        config,
        move |_| MadeUp::new(Rational::new(fps.den, fps.num), spin),
    )
    .expect("playback")
}

/// Makes up one source frame per `frame` of source time, spinning `spin` for each.
pub struct MadeUp {
    frame: Rational,
    next: i64,
    spin: Duration,
}

impl MadeUp {
    pub fn new(frame: Rational, spin: Duration) -> Self {
        Self {
            frame,
            next: 0,
            spin,
        }
    }
}

impl SegmentDecoder for MadeUp {
    type Frame = ();

    fn seek(&mut self, _: &SourceRef, at: Timestamp) -> Result<(), MediaError> {
        self.next = at.rescale_floor(self.frame).value.max(0);
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<(Timestamp, ())>, MediaError> {
        let start = Instant::now();
        while start.elapsed() < self.spin {
            std::hint::spin_loop();
        }
        let pts = Timestamp::new(self.next, self.frame);
        self.next += 1;
        Ok(Some((pts, ())))
    }
}

/// Renders nothing: presents the timeline frame number at once, caches nothing.
#[derive(Default)]
pub struct Immediate {
    cached: CachedFrames,
}

impl FrameRender for Immediate {
    type Frame = ();
    type Picture = i64;

    fn render_frame(
        &mut self,
        _: &TimelinePlan,
        t: i64,
        _: &[Layer<()>],
        _: QualityLevel,
    ) -> (i64, bool) {
        (t, false)
    }

    fn lookup_frame(&mut self, _: &TimelinePlan, _: i64, _: QualityLevel) -> Option<i64> {
        None
    }

    fn replan(&mut self, _: &TimelinePlan, _: &DirtyFrames) {}

    fn cached_frames(&self) -> CachedFrames {
        self.cached.clone()
    }

    fn counters(&self) -> (RenderStats, FrameCacheStats) {
        Default::default()
    }
}
