//! [`TimelinePlayback`]: a sequence played through the look-ahead, the timeline
//! renderer and the frame cache, paced by the master clock (see the module docs of
//! [`crate::timeline`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use audio::Clock;
use media::{Rational, VideoFrame};
use render::frame_cache::FrameCacheStats;
use render::wgpu;

use super::plan::TimelinePlan;
use super::render::{RenderStats, RenderedFrame, RendererConfig, TimelineRenderer};
use crate::frame_queue::{
    FrameStamp, Generation, PushError, SpscConsumer, SpscProducer, Stamped, spsc,
};
use crate::lookahead::{
    FramesError, GopIndexes, Layer, Lookahead, LookaheadConfig, LookaheadFrames, LookaheadStats,
    MediaDecoder, MediaDecoderOptions, default_depth,
};
use crate::playback::{PRESENT_RETRY, SharedQuality, default_decode_workers};
use crate::quality::{
    Admit, FrameTiming, GovernorConfig, QualityGovernor, QualityLevel, QualityStats,
};
use crate::scrub::{CachedFrames, ScrubRender};
use crate::threads::{self, Role, ThreadRegistry};

/// How far past its due time a frame may finish and still count as on time: the clock
/// is read in floating point.
const LATE_SLACK: f64 = 1e-4;

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
pub struct PresentedFrame {
    pub frame: i64,
    /// [`PREVIEW_FORMAT`](super::PREVIEW_FORMAT), at the quality's preview size.
    pub texture: wgpu::Texture,
    pub quality: QualityLevel,
    /// When it is due on screen, in seconds of the master clock; `None` for a frame shown
    /// while paused (after a seek, a pause at reduced quality, or a scrub).
    pub due: Option<f64>,
    /// Render finished after `due`.
    pub late: bool,
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

enum Control {
    Play,
    Pause,
    Seek,
    Show(i64),
    ScrubRender {
        frame: i64,
        layers: Vec<Layer<VideoFrame>>,
        done: SyncSender<()>,
    },
    Stop,
}

/// The presenter's end: rendered frames in timeline order.
pub type Presented = SpscConsumer<PresentedFrame>;

/// A playing (or paused) sequence. Dropping it stops and joins every thread.
pub struct TimelinePlayback {
    control: Sender<Control>,
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
        let (lookahead, frames) = Lookahead::start(
            plan.segments().clone(),
            LookaheadConfig {
                decode_workers: config.decode_workers,
                depth: config.depth,
                frame_duration: plan.frame_duration(),
                registry: config.registry.clone(),
            },
            |_| MediaDecoder::new(options, gops.clone()),
        )?;
        let present = Generation::new();
        let (output, presented) = spsc(config.present_depth.max(1), &present);
        let renderer = TimelineRenderer::new(device, queue, config.renderer);
        let cached = renderer.cached_frames();
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

    /// The scrubber's renderer: renders scrubbed frames on `glass-render` into this
    /// playback's frame cache.
    pub fn scrub_render(&self) -> TimelineScrubRender {
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

impl Drop for TimelinePlayback {
    fn drop(&mut self) {
        self.stop();
    }
}

/// [`ScrubRender`] for a [`TimelinePlayback`]: hands the scrubbed layers to
/// `glass-render`, which renders them at full quality into the frame cache and the
/// scrubber's index, and returns once that is done.
pub struct TimelineScrubRender {
    control: Sender<Control>,
}

impl ScrubRender<VideoFrame> for TimelineScrubRender {
    fn render(&mut self, frame: i64, layers: &[Layer<VideoFrame>]) {
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
    }
}

struct RenderLoop {
    renderer: TimelineRenderer,
    plan: Arc<TimelinePlan>,
    frames: LookaheadFrames<VideoFrame>,
    layers: Vec<Layer<VideoFrame>>,
    /// The layers of the last frame rendered, kept to render it again on pause.
    last_layers: Vec<Layer<VideoFrame>>,
    last: Option<FrameStamp>,
    control: Receiver<Control>,
    output: SpscProducer<PresentedFrame>,
    /// A frame the present queue had no room for.
    pending: Option<Stamped<PresentedFrame>>,
    clock: Clock,
    /// One frame interval, in seconds.
    interval: f64,
    governor: Option<QualityGovernor>,
    stats: Arc<SharedStats>,
    quality: Arc<SharedQuality>,
    render_stats: Arc<std::sync::Mutex<(RenderStats, FrameCacheStats)>>,
    playing: bool,
    /// Paused: present the next frame the look-ahead hands out (the start position, or
    /// a seek's).
    show_next: bool,
    /// Clock time and timeline frame of the first frame of the current run.
    anchor: Option<(f64, i64)>,
    generation: u64,
    /// Since when render has waited for the next frame: instant and clock time.
    waiting_since: Option<(Instant, f64)>,
}

impl RenderLoop {
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
        let clock = &self.clock;
        let (since, since_clock) = *self
            .waiting_since
            .get_or_insert_with(|| (Instant::now(), clock.now()));
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
            self.show_next = false;
            let rendered = self.render(t, QualityLevel::Full, false);
            self.queue(stamp, rendered.0, None, false, decode_wait, rendered.1);
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
        let (rendered, render) = self.render(t, level, false);
        let now = self.clock.now();
        let first = self.anchor.is_none();
        let (c0, f0) = *self.anchor.get_or_insert((now, t));
        let due = c0 + (t - f0) as f64 * self.interval;
        let late = now > due + LATE_SLACK;
        if !first && let Some(governor) = &mut self.governor {
            governor.record(FrameTiming {
                budget: Duration::from_secs_f64((due - since_clock).clamp(0.0, self.interval)),
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
            .fetch_add(u64::from(rendered.from_cache), Ordering::Relaxed);
        s.render_nanos.fetch_add(nanos(render), Ordering::Relaxed);
        s.render_max.fetch_max(nanos(render), Ordering::Relaxed);
        s.wait_nanos
            .fetch_add(nanos(decode_wait), Ordering::Relaxed);
        if !first {
            s.cost_max
                .fetch_max(nanos(decode_wait + render), Ordering::Relaxed);
        }
        self.queue(stamp, rendered, Some(due), late, decode_wait, render);
        self.keep_last(stamp);
        true
    }

    /// Renders timeline frame `t` from the current (or, for `again`, the last) layers
    /// and waits for the GPU. Returns the frame and how long it took.
    fn render(&mut self, t: i64, level: QualityLevel, again: bool) -> (RenderedFrame, Duration) {
        let started = Instant::now();
        let layers = if again {
            &self.last_layers
        } else {
            &self.layers
        };
        let rendered = self.renderer.render(&self.plan, t, layers, level);
        if let Some(submission) = &rendered.submission {
            let _ = self.renderer.device().poll(wgpu::PollType::Wait {
                submission_index: Some(submission.clone()),
                timeout: None,
            });
        }
        let took = started.elapsed();
        *self
            .render_stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            (self.renderer.stats(), self.renderer.cache_stats());
        (rendered, took)
    }

    fn queue(
        &mut self,
        stamp: FrameStamp,
        rendered: RenderedFrame,
        due: Option<f64>,
        late: bool,
        decode_wait: Duration,
        render: Duration,
    ) {
        self.pending = Some(Stamped::new(
            stamp,
            PresentedFrame {
                frame: rendered.frame,
                texture: rendered.texture,
                quality: rendered.quality,
                due,
                late,
                from_cache: rendered.from_cache,
                decode_wait,
                render,
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
    fn apply(&mut self, control: Control) -> bool {
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
                self.pending = None;
                self.show_next = !self.playing;
                self.stats.ended.store(false, Ordering::Relaxed);
                self.new_run();
            }
            Control::Show(frame) => {
                if let Some(texture) = self.renderer.lookup(&self.plan, frame, QualityLevel::Full) {
                    let stamp = FrameStamp {
                        generation: self.output.generation(),
                        index: frame,
                        pts: media::Timestamp::new(frame, self.plan.frame_duration()),
                    };
                    let rendered = RenderedFrame {
                        frame,
                        texture,
                        quality: QualityLevel::Full,
                        from_cache: true,
                        submission: None,
                    };
                    self.queue(stamp, rendered, None, false, Duration::ZERO, Duration::ZERO);
                }
            }
            Control::ScrubRender {
                frame,
                layers,
                done,
            } => {
                let rendered = self
                    .renderer
                    .render(&self.plan, frame, &layers, QualityLevel::Full);
                if let Some(submission) = rendered.submission {
                    let _ = self.renderer.device().poll(wgpu::PollType::Wait {
                        submission_index: Some(submission),
                        timeout: None,
                    });
                }
                let _ = done.send(());
            }
            Control::Stop => return false,
        }
        true
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
        let (rendered, render) = self.render(stamp.index, QualityLevel::Full, true);
        self.queue(stamp, rendered, None, false, Duration::ZERO, render);
    }
}
