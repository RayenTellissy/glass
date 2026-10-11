//! [`TimelinePlayback`]: a sequence played through the look-ahead, the timeline
//! renderer and the frame cache, paced by the master clock (see the module docs of
//! [`crate::timeline`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use audio::Clock;
use media::{Rational, VideoFrame};
use render::frame_cache::FrameCacheStats;
use render::wgpu;

use super::dirty::DirtyFrames;
use super::plan::TimelinePlan;
use super::render::{RenderStats, RendererConfig, TimelineRenderer};
use crate::frame_queue::{
    FrameStamp, Generation, PushError, SpscConsumer, SpscProducer, Stamped, spsc,
};
use crate::lookahead::{
    FramesError, GopIndexes, Layer, Lookahead, LookaheadConfig, LookaheadFrames, LookaheadStats,
    MediaDecoder, MediaDecoderOptions, SegmentDecoder, default_decode_workers, default_depth,
};
use crate::quality::{
    Admit, FrameTiming, GovernorConfig, QualityGovernor, QualityLevel, QualityStats,
};
use crate::scrub::{CachedFrames, ScrubRender};
use crate::threads::{self, Role, ThreadRegistry};

/// How far past its due time a frame may finish and still count as on time: the clock
/// is read in floating point.
const LATE_SLACK: f64 = 1e-4;

/// How often render offers a frame again while the present queue is full.
const PRESENT_RETRY: Duration = Duration::from_millis(2);

/// What `glass-render` does with a timeline frame: [`TimelineRenderer`] in the app, a
/// stand-in where tests drive the pipeline without a GPU or media files.
pub trait FrameRender: Send + 'static {
    /// The decoded pictures the look-ahead's layers carry.
    type Frame: Send + Sync + 'static;
    /// What the presenter gets.
    type Picture: Send + 'static;

    /// Timeline frame `t` of `plan` at `quality`: from the frame cache when it holds it,
    /// else rendered from `layers` (bottom first, any order) and cached. Returns once the
    /// picture is finished, with whether it came from the cache.
    fn render_frame(
        &mut self,
        plan: &TimelinePlan,
        t: i64,
        layers: &[Layer<Self::Frame>],
        quality: QualityLevel,
    ) -> (Self::Picture, bool);

    /// The cached frame `t` of `plan` at `quality`, if the cache holds it.
    fn lookup_frame(
        &mut self,
        plan: &TimelinePlan,
        t: i64,
        quality: QualityLevel,
    ) -> Option<Self::Picture>;

    /// `plan` replaced the plan the cache index followed; the keys of frames outside
    /// `dirty` did not change.
    fn replan(&mut self, plan: &TimelinePlan, dirty: &DirtyFrames);

    /// The full-quality frames the cache holds under the current plan: the frames the
    /// look-ahead skips and the scrubber answers from.
    fn cached_frames(&self) -> CachedFrames;

    /// Counters for [`TimelinePlayback::render_stats`].
    fn counters(&self) -> (RenderStats, FrameCacheStats);
}

/// Pool size, queue depths, decoder and canvas of a [`TimelinePlayback`].
#[derive(Clone, Debug)]
pub struct TimelineConfig {
    pub renderer: RendererConfig,
    /// `glass-decode-<n>` workers of the look-ahead (at least 2).
    pub decode_workers: usize,
    /// Decoded frames each look-ahead lane holds.
    pub depth: usize,
    /// Rendered frames queued for the presenter.
    pub present_depth: usize,
    /// How the look-ahead opens sources (hardware decode by default).
    pub decoder: MediaDecoderOptions,
    /// Adaptive quality thresholds; `None` renders every frame at full resolution.
    pub quality: Option<GovernorConfig>,
    pub registry: Arc<ThreadRegistry>,
}

impl TimelineConfig {
    /// Defaults for a project canvas of `canvas` pixels.
    pub fn new(canvas: (u32, u32)) -> Self {
        Self {
            renderer: RendererConfig::new(canvas),
            decode_workers: default_decode_workers(),
            depth: default_depth(canvas.0, canvas.1, 8),
            present_depth: 3,
            decoder: MediaDecoderOptions::default(),
            quality: Some(GovernorConfig::default()),
            registry: threads::global().clone(),
        }
    }
}

/// A frame for the presenter.
#[derive(Debug)]
pub struct PresentedFrame<P = wgpu::Texture> {
    pub frame: i64,
    /// [`PREVIEW_FORMAT`](super::PREVIEW_FORMAT), at the quality's preview size (the
    /// stand-in renderer's picture in tests).
    pub texture: P,
    pub quality: QualityLevel,
    /// When it is due on screen, in seconds of the master clock; `None` for a frame shown
    /// while paused (after a seek, a pause at reduced quality, or a scrub).
    pub due: Option<f64>,
    /// Render finished after `due`.
    pub late: bool,
    /// When render finished it, in seconds of the master clock.
    pub ready: f64,
    pub from_cache: bool,
    /// How long render waited for the frame's decoded layers.
    pub decode_wait: Duration,
    /// How long rendering took, the GPU included.
    pub render: Duration,
}

/// Counters across the pipeline, for tests, benchmarks and the debug panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackStats {
    /// Frames handed to the presenter while playing.
    pub presented: u64,
    /// Of those, frames that finished after they were due.
    pub late: u64,
    /// Frames the governor dropped.
    pub dropped: u64,
    /// Frames answered from the frame cache while playing.
    pub cache_hits: u64,
    /// Plans swapped in by [`TimelinePlayback::set_plan`].
    pub replans: u64,
    /// Skipped frames the cache had let go of by the time render took them, decoded
    /// again through [`LookaheadFrames::refill`].
    pub refills: u64,
    pub render_total: Duration,
    pub render_max: Duration,
    pub decode_wait_total: Duration,
    /// The longest decode wait plus render of a frame after the first of a run.
    pub cost_max: Duration,
    /// The last timeline frame render took from the look-ahead.
    pub position: i64,
    /// Playback reached the end of the timeline.
    pub ended: bool,
}

#[derive(Default)]
struct SharedStats {
    presented: AtomicU64,
    late: AtomicU64,
    dropped: AtomicU64,
    cache_hits: AtomicU64,
    replans: AtomicU64,
    refills: AtomicU64,
    render_nanos: AtomicU64,
    render_max: AtomicU64,
    wait_nanos: AtomicU64,
    cost_max: AtomicU64,
    position: AtomicI64,
    ended: AtomicBool,
}

impl SharedStats {
    fn snapshot(&self) -> PlaybackStats {
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let nanos = |c: &AtomicU64| Duration::from_nanos(load(c));
        PlaybackStats {
            presented: load(&self.presented),
            late: load(&self.late),
            dropped: load(&self.dropped),
            cache_hits: load(&self.cache_hits),
            replans: load(&self.replans),
            refills: load(&self.refills),
            render_total: nanos(&self.render_nanos),
            render_max: nanos(&self.render_max),
            decode_wait_total: nanos(&self.wait_nanos),
            cost_max: nanos(&self.cost_max),
            position: self.position.load(Ordering::Relaxed),
            ended: self.ended.load(Ordering::Relaxed),
        }
    }
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// The governor's counters, published by render after every change.
#[derive(Default)]
struct SharedQuality {
    level: AtomicU8,
    frames: AtomicU64,
    misses: AtomicU64,
    steps_down: AtomicU64,
    steps_up: AtomicU64,
    restores: AtomicU64,
    drops: AtomicU64,
}

impl SharedQuality {
    fn publish(&self, stats: &QualityStats) {
        let store = |c: &AtomicU64, v: u64| c.store(v, Ordering::Relaxed);
        store(&self.frames, stats.frames);
        store(&self.misses, stats.misses);
        store(&self.steps_down, stats.steps_down);
        store(&self.steps_up, stats.steps_up);
        store(&self.restores, stats.restores);
        store(&self.drops, stats.drops);
        self.level.store(stats.level as u8, Ordering::Relaxed);
    }

    fn snapshot(&self) -> QualityStats {
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        QualityStats {
            level: QualityLevel::from_index(self.level.load(Ordering::Relaxed)),
            frames: load(&self.frames),
            misses: load(&self.misses),
            steps_down: load(&self.steps_down),
            steps_up: load(&self.steps_up),
            restores: load(&self.restores),
            drops: load(&self.drops),
        }
    }
}

enum Control<F> {
    Play,
    Pause,
    Seek,
    Show(i64),
    ScrubRender {
        frame: i64,
        layers: Vec<Layer<F>>,
        done: SyncSender<()>,
    },
    Plan {
        plan: Arc<TimelinePlan>,
        dirty: DirtyFrames,
    },
    Stop,
}

/// The presenter's end: rendered frames in timeline order.
pub type Presented<P = wgpu::Texture> = SpscConsumer<PresentedFrame<P>>;

/// [`TimelinePlayback::set_plan`] refused a plan at another canvas size or frame rate:
/// the renderer and the look-ahead are set up for one of each, so the app starts a new
/// playback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the sequence's canvas or frame rate changed: start a new playback")]
pub struct FormatChanged;

/// A playing (or paused) sequence. Dropping it stops and joins every thread.
pub struct TimelinePlayback<R: FrameRender = TimelineRenderer> {
    control: Sender<Control<R::Frame>>,
    lookahead: Option<Lookahead>,
    /// The present queue's generation, advanced with the look-ahead's on every seek so
    /// the two stay equal.
    present: Generation,
    thread: Option<JoinHandle<()>>,
    stats: Arc<SharedStats>,
    quality: Arc<SharedQuality>,
    render_stats: Arc<std::sync::Mutex<(RenderStats, FrameCacheStats)>>,
    plan: Arc<TimelinePlan>,
    cached: CachedFrames,
}

impl TimelinePlayback {
    /// Starts the look-ahead (decoding from frame 0 at once) and `glass-render` on
    /// `device`, paused at frame 0; `clock` is the master clock (an audio device's, or
    /// `Clock::system` without one). Returns the handle and the present queue's consumer.
    pub fn start(
        plan: Arc<TimelinePlan>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        clock: Clock,
        config: TimelineConfig,
    ) -> std::io::Result<(Self, Presented)> {
        let rate = plan.rate();
        let options = MediaDecoderOptions {
            project_rate: Rational::new(rate.num, rate.den),
            ..config.decoder
        };
        let gops = GopIndexes::new();
        let renderer = TimelineRenderer::new(device, queue, config.renderer);
        Self::start_with(plan, renderer, clock, config, move |_| {
            MediaDecoder::new(options, gops.clone())
        })
    }
}

impl<R: FrameRender> TimelinePlayback<R> {
    /// [`start`](TimelinePlayback::start) with any renderer and decoders: one from
    /// `make_decoder(n)` per look-ahead worker. `config.renderer` and `config.decoder`
    /// are not read.
    pub fn start_with<D>(
        plan: Arc<TimelinePlan>,
        renderer: R,
        clock: Clock,
        config: TimelineConfig,
        make_decoder: impl FnMut(usize) -> D,
    ) -> std::io::Result<(Self, Presented<R::Picture>)>
    where
        D: SegmentDecoder<Frame = R::Frame>,
    {
        let cached = renderer.cached_frames();
        let (lookahead, frames) = Lookahead::start_skipping(
            plan.segments().clone(),
            LookaheadConfig {
                decode_workers: config.decode_workers,
                depth: config.depth,
                frame_duration: plan.frame_duration(),
                registry: config.registry.clone(),
            },
            Some(cached.clone()),
            make_decoder,
        )?;
        let present = Generation::new();
        let (output, presented) = spsc(config.present_depth.max(1), &present);
        let (control, control_rx) = mpsc::channel();
        let stats = Arc::new(SharedStats::default());
        let quality = Arc::new(SharedQuality::default());
        let render_stats = Arc::new(std::sync::Mutex::new((
            RenderStats::default(),
            FrameCacheStats::default(),
        )));
        let interval = plan.frame_duration().to_f64().unwrap_or(0.0).max(0.0);
        let render = RenderLoop {
            renderer,
            plan: plan.clone(),
            frames,
            layers: Vec::new(),
            last_layers: Vec::new(),
            last: None,
            control: control_rx,
            output,
            pending: None,
            clock,
            interval,
            governor: config.quality.map(QualityGovernor::new),
            stats: stats.clone(),
            quality: quality.clone(),
            render_stats: render_stats.clone(),
            playing: false,
            show_next: true,
            anchor: None,
            generation: 0,
            waiting_since: None,
        };
        let thread = config
            .registry
            .spawn(Role::Render, None, move || render.run())?;
        Ok((
            Self {
                control,
                lookahead: Some(lookahead),
                present,
                thread: Some(thread),
                stats,
                quality,
                render_stats,
                plan,
                cached,
            },
            presented,
        ))
    }

    /// Plays from the current position. The first frame is due when it is ready; every
    /// later frame one frame interval of the clock after the one before.
    pub fn play(&self) {
        let _ = self.control.send(Control::Play);
    }

    /// Stops taking frames. At a reduced quality the frame on screen is rendered again at
    /// full resolution. The look-ahead's workers park once their lanes are full, so a
    /// paused (or scrubbing) playback decodes nothing.
    pub fn pause(&self) {
        let _ = self.control.send(Control::Pause);
    }

    /// Drops every frame in flight and continues from timeline frame `frame`; while
    /// paused, that frame is rendered and presented. Returns the new generation.
    pub fn seek(&self, frame: i64) -> u64 {
        let generation = match &self.lookahead {
            Some(lookahead) => lookahead.seek(frame),
            None => return self.present.current(),
        };
        while self.present.current() < generation {
            self.present.advance();
        }
        let _ = self.control.send(Control::Seek);
        generation
    }

    /// Presents the cached full-quality frame `frame` (what the scrubber answered), if
    /// the cache still holds it.
    pub fn show(&self, frame: i64) {
        let _ = self.control.send(Control::Show(frame));
    }

    /// Swaps in `plan`, the sequence after an edit, at the next frame boundary of
    /// `glass-render` (see the module docs of [`crate::timeline`]): `dirty` holds every
    /// frame whose picture may have changed ([`DirtyRanges`](super::DirtyRanges)).
    /// Look-ahead lanes of unchanged segments keep decoding, frames whose cache keys did
    /// not change stay cached, and while paused a dirty frame on screen is presented
    /// again. A scrubber started earlier keeps the old segments; start a new one.
    pub fn set_plan(
        &mut self,
        plan: Arc<TimelinePlan>,
        dirty: DirtyFrames,
    ) -> Result<(), FormatChanged> {
        if plan.canvas() != self.plan.canvas() || plan.rate() != self.plan.rate() {
            return Err(FormatChanged);
        }
        self.plan = plan.clone();
        let _ = self.control.send(Control::Plan { plan, dirty });
        Ok(())
    }

    /// The scrubber's renderer: renders scrubbed frames on `glass-render` into this
    /// playback's frame cache.
    pub fn scrub_render(&self) -> TimelineScrubRender<R::Frame> {
        TimelineScrubRender {
            control: self.control.clone(),
        }
    }

    /// The index of full-quality frames in the frame cache, for the scrubber.
    pub fn cached_frames(&self) -> CachedFrames {
        self.cached.clone()
    }

    pub fn plan(&self) -> &Arc<TimelinePlan> {
        &self.plan
    }

    pub fn stats(&self) -> PlaybackStats {
        self.stats.snapshot()
    }

    pub fn quality(&self) -> QualityStats {
        self.quality.snapshot()
    }

    pub fn lookahead_stats(&self) -> LookaheadStats {
        self.lookahead
            .as_ref()
            .map(Lookahead::stats)
            .unwrap_or_default()
    }

    /// The renderer's and the frame cache's counters as of the last frame rendered.
    pub fn render_stats(&self) -> (RenderStats, FrameCacheStats) {
        *self
            .render_stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Stops every thread and waits for them.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.control.send(Control::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(lookahead) = self.lookahead.take() {
            lookahead.shutdown();
        }
    }
}

impl<R: FrameRender> Drop for TimelinePlayback<R> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// [`ScrubRender`] for a [`TimelinePlayback`]: hands the scrubbed layers to
/// `glass-render`, which renders them at full quality into the frame cache and the
/// scrubber's index, and returns once that is done.
pub struct TimelineScrubRender<F = VideoFrame> {
    control: Sender<Control<F>>,
}

impl<F: Send + Sync + 'static> ScrubRender<F> for TimelineScrubRender<F> {
    fn render(&mut self, frame: i64, layers: &[Layer<F>]) {
        let (done, wait) = mpsc::sync_channel(1);
        let layers = layers.iter().map(copy_layer).collect();
        if self
            .control
            .send(Control::ScrubRender {
                frame,
                layers,
                done,
            })
            .is_ok()
        {
            let _ = threads::blocked(|| wait.recv());
        }
    }
}

fn copy_layer<F>(layer: &Layer<F>) -> Layer<F> {
    Layer {
        layer: layer.layer,
        segment: layer.segment,
        source: layer.source,
        source_pts: layer.source_pts,
        frame: layer.frame.clone(),
        skipped: layer.skipped,
    }
}

/// A frame render finished: its picture, the level it shows, whether the cache had it
/// and how long it took.
struct Done<P> {
    picture: P,
    quality: QualityLevel,
    from_cache: bool,
    took: Duration,
}

struct RenderLoop<R: FrameRender> {
    renderer: R,
    plan: Arc<TimelinePlan>,
    frames: LookaheadFrames<R::Frame>,
    layers: Vec<Layer<R::Frame>>,
    /// The layers of the last frame rendered, kept to render it again on pause.
    last_layers: Vec<Layer<R::Frame>>,
    last: Option<FrameStamp>,
    control: Receiver<Control<R::Frame>>,
    output: SpscProducer<PresentedFrame<R::Picture>>,
    /// A frame the present queue had no room for.
    pending: Option<Stamped<PresentedFrame<R::Picture>>>,
    clock: Clock,
    /// One frame interval, in seconds.
    interval: f64,
    governor: Option<QualityGovernor>,
    stats: Arc<SharedStats>,
    quality: Arc<SharedQuality>,
    render_stats: Arc<std::sync::Mutex<(RenderStats, FrameCacheStats)>>,
    playing: bool,
    /// Paused: present the next frame the look-ahead hands out (the start position, a
    /// seek's, or a frame on screen an edit changed).
    show_next: bool,
    /// Clock time and timeline frame of the first frame of the current run.
    anchor: Option<(f64, i64)>,
    generation: u64,
    /// Since when render has waited for the next frame.
    waiting_since: Option<Instant>,
}

impl<R: FrameRender> RenderLoop<R> {
    fn run(mut self) {
        loop {
            loop {
                match self.control.try_recv() {
                    Ok(control) => {
                        if !self.apply(control) {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            if let Some(frame) = self.pending.take() {
                match self.output.try_push(frame) {
                    Ok(()) | Err(PushError::Stale(_) | PushError::Timeout(_)) => {}
                    Err(PushError::Full(frame)) => {
                        self.pending = Some(frame);
                        threads::blocked(|| std::thread::park_timeout(PRESENT_RETRY));
                        continue;
                    }
                    Err(PushError::Disconnected(_)) => return,
                }
            }
            if !self.playing && !self.show_next {
                match threads::idle(|| self.control.recv()) {
                    Ok(control) => {
                        if !self.apply(control) {
                            return;
                        }
                    }
                    Err(_) => return,
                }
                continue;
            }
            if !self.step() {
                return;
            }
        }
    }

    /// Takes, renders and queues the next frame. False when the look-ahead stopped.
    fn step(&mut self) -> bool {
        let since = *self.waiting_since.get_or_insert_with(Instant::now);
        let timeout = Duration::from_secs_f64(self.interval.clamp(0.005, 0.1));
        let frames = &mut self.frames;
        let layers = &mut self.layers;
        let stamp = match threads::idle(|| frames.next_into_timeout(layers, timeout)) {
            Ok(stamp) => stamp,
            // Back to the control channel, still waiting for the same frame.
            Err(FramesError::Timeout) => return true,
            Err(FramesError::Finished) => {
                self.waiting_since = None;
                self.playing = false;
                self.show_next = false;
                self.stats.ended.store(true, Ordering::Relaxed);
                return true;
            }
            Err(FramesError::Disconnected) => return false,
        };
        self.waiting_since = None;
        let decode_wait = since.elapsed();
        if stamp.generation != self.generation {
            self.generation = stamp.generation;
            self.new_run();
        }
        let t = stamp.index;
        self.stats.position.store(t, Ordering::Relaxed);

        if !self.playing {
            // A paused frame: the start position or a seek's, at full resolution.
            let Some(done) = self.render(t, QualityLevel::Full, false) else {
                self.refill(t);
                return true;
            };
            self.show_next = false;
            self.queue(stamp, done, None, false, decode_wait);
            self.keep_last(stamp);
            return true;
        }

        let level = match self.governor.as_mut().map(QualityGovernor::admit) {
            Some(Admit::Drop) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                if let Some(g) = &self.governor {
                    self.quality.publish(&g.stats());
                }
                return true;
            }
            Some(Admit::Render(level)) => level,
            None => QualityLevel::Full,
        };
        let Some(done) = self.render(t, level, false) else {
            // Waiting for the frame decoded again counts as decode lateness.
            self.refill(t);
            self.waiting_since = Some(since);
            return true;
        };
        let render = done.took;
        let now = self.clock.now();
        let first = self.anchor.is_none();
        let (c0, f0) = *self.anchor.get_or_insert((now, t));
        let due = c0 + (t - f0) as f64 * self.interval;
        let late = now > due + LATE_SLACK;
        if !first && let Some(governor) = &mut self.governor {
            governor.record(FrameTiming {
                // One interval per frame (Decision 52), so the governor's lag adds up
                // to how far behind the clock render is and a drop catches up an
                // interval; a budget cut to the time left before the deadline was zero
                // once render fell behind, and the governor never dropped (Decision 67).
                budget: Duration::from_secs_f64(self.interval),
                decode_late: decode_wait,
                render,
            });
        }
        if let Some(g) = &self.governor {
            self.quality.publish(&g.stats());
        }
        let s = &self.stats;
        s.presented.fetch_add(1, Ordering::Relaxed);
        s.late.fetch_add(u64::from(late), Ordering::Relaxed);
        s.cache_hits
            .fetch_add(u64::from(done.from_cache), Ordering::Relaxed);
        s.render_nanos.fetch_add(nanos(render), Ordering::Relaxed);
        s.render_max.fetch_max(nanos(render), Ordering::Relaxed);
        s.wait_nanos
            .fetch_add(nanos(decode_wait), Ordering::Relaxed);
        if !first {
            s.cost_max
                .fetch_max(nanos(decode_wait + render), Ordering::Relaxed);
        }
        self.queue(stamp, done, Some(due), late, decode_wait);
        self.keep_last(stamp);
        true
    }

    /// Timeline frame `t` from the cache or rendered from the current (or, for `again`,
    /// the last) layers, finished. `None` when a layer was skipped because the cache
    /// held the frame and the cache no longer holds it at any level.
    fn render(&mut self, t: i64, level: QualityLevel, again: bool) -> Option<Done<R::Picture>> {
        let started = Instant::now();
        let layers = if again {
            &self.last_layers
        } else {
            &self.layers
        };
        let done = if layers.iter().any(|l| l.skipped) {
            let cached = |renderer: &mut R, quality| {
                renderer
                    .lookup_frame(&self.plan, t, quality)
                    .map(|picture| (picture, quality))
            };
            let (picture, quality) = cached(&mut self.renderer, level).or_else(|| {
                (level != QualityLevel::Full)
                    .then(|| cached(&mut self.renderer, QualityLevel::Full))
                    .flatten()
            })?;
            Done {
                picture,
                quality,
                from_cache: true,
                took: started.elapsed(),
            }
        } else {
            let (picture, from_cache) = self.renderer.render_frame(&self.plan, t, layers, level);
            Done {
                picture,
                quality: level,
                from_cache,
                took: started.elapsed(),
            }
        };
        *self
            .render_stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.renderer.counters();
        Some(done)
    }

    /// Frame `t` came skipped but the cache let go of it: drop it from the index (so the
    /// look-ahead decodes it) and have the look-ahead hand it out again.
    fn refill(&mut self, t: i64) {
        self.renderer.cached_frames().remove(t);
        self.frames.refill(t);
        self.stats.refills.fetch_add(1, Ordering::Relaxed);
    }

    fn queue(
        &mut self,
        stamp: FrameStamp,
        done: Done<R::Picture>,
        due: Option<f64>,
        late: bool,
        decode_wait: Duration,
    ) {
        self.pending = Some(Stamped::new(
            stamp,
            PresentedFrame {
                frame: stamp.index,
                texture: done.picture,
                quality: done.quality,
                due,
                late,
                ready: self.clock.now(),
                from_cache: done.from_cache,
                decode_wait,
                render: done.took,
            },
        ));
    }

    fn keep_last(&mut self, stamp: FrameStamp) {
        std::mem::swap(&mut self.layers, &mut self.last_layers);
        self.last = Some(stamp);
    }

    /// A seek or a play: the next frame starts a new run, due when it is ready.
    fn new_run(&mut self) {
        self.anchor = None;
        self.waiting_since = None;
        if let Some(governor) = &mut self.governor {
            governor.reset_timing();
        }
    }

    /// Applies a control message; false means stop.
    fn apply(&mut self, control: Control<R::Frame>) -> bool {
        match control {
            Control::Play => {
                self.playing = true;
                self.show_next = false;
                self.stats.ended.store(false, Ordering::Relaxed);
                self.new_run();
            }
            Control::Pause => {
                self.playing = false;
                self.new_run();
                self.restore_full();
            }
            Control::Seek => {
                // Render may already hold the new position's first frame.
                let current = self.output.generation();
                self.pending = self
                    .pending
                    .take()
                    .filter(|p| p.stamp.generation >= current);
                self.show_next = !self.playing;
                self.stats.ended.store(false, Ordering::Relaxed);
                self.new_run();
            }
            Control::Show(frame) => {
                if let Some(picture) =
                    self.renderer
                        .lookup_frame(&self.plan, frame, QualityLevel::Full)
                {
                    let stamp = FrameStamp {
                        generation: self.output.generation(),
                        index: frame,
                        pts: media::Timestamp::new(frame, self.plan.frame_duration()),
                    };
                    let done = Done {
                        picture,
                        quality: QualityLevel::Full,
                        from_cache: true,
                        took: Duration::ZERO,
                    };
                    self.queue(stamp, done, None, false, Duration::ZERO);
                }
            }
            Control::ScrubRender {
                frame,
                layers,
                done,
            } => {
                self.renderer
                    .render_frame(&self.plan, frame, &layers, QualityLevel::Full);
                let _ = done.send(());
            }
            Control::Plan { plan, dirty } => self.replan(plan, &dirty),
            Control::Stop => return false,
        }
        true
    }

    /// Swaps in an edited sequence's plan between two frames (see
    /// [`TimelinePlayback::set_plan`]).
    fn replan(&mut self, plan: Arc<TimelinePlan>, dirty: &DirtyFrames) {
        self.renderer.replan(&plan, dirty);
        self.plan = plan;
        self.stats.replans.fetch_add(1, Ordering::Relaxed);
        let stale = match dirty {
            DirtyFrames::All => std::iter::once(i64::MIN..i64::MAX).collect(),
            DirtyFrames::Ranges(ranges) => ranges.clone(),
        };
        let mut from = self.frames.position();
        let shown = self.last.map(|s| s.index);
        if !self.playing
            && !self.show_next
            && let Some(t) = shown.filter(|t| dirty.contains(*t))
        {
            // The frame on screen changed: present it again, from the layers it was
            // made of when the new plan reads the same segments, else decoded anew.
            if self.layers_cover(t) {
                if let Some(done) = self.render(t, QualityLevel::Full, true)
                    && let Some(stamp) = self.last
                {
                    self.queue(stamp, done, None, false, Duration::ZERO);
                }
            } else {
                from = t;
                self.show_next = true;
            }
        }
        let segments = self.plan.segments().clone();
        self.frames.replan(segments, from, stale);
    }

    /// Whether the last frame's layers hold a picture of every segment `t` reads in the
    /// current plan.
    fn layers_cover(&self, t: i64) -> bool {
        self.plan.frame(t).segments(&self.plan).iter().all(|&s| {
            let hash = self.plan.segment(s).hash;
            self.last_layers
                .iter()
                .any(|l| l.segment == hash && !l.skipped)
        })
    }

    /// Playback stopped: back to full resolution, and the frame on screen rendered again
    /// at it when it was reduced.
    fn restore_full(&mut self) {
        let Some(governor) = &mut self.governor else {
            return;
        };
        let change = governor.idle();
        self.quality.publish(&governor.stats());
        if change.is_none() {
            return;
        }
        let Some(stamp) = self.last else {
            return;
        };
        let t = stamp.index;
        // After an edit the last layers may no longer be the frame's segments.
        if !self.last_layers.iter().any(|l| l.skipped) && !self.layers_cover(t) {
            self.show_again(t);
            return;
        }
        match self.render(t, QualityLevel::Full, true) {
            Some(done) => self.queue(stamp, done, None, false, Duration::ZERO),
            None => self.show_again(t),
        }
    }

    /// Presents frame `t` again, decoded anew: the look-ahead hands it out once more and
    /// render shows it as paused.
    fn show_again(&mut self, t: i64) {
        self.frames.refill(t);
        self.show_next = true;
    }
}
