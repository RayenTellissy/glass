//! Look-ahead decode across cuts and transitions.
//!
//! A [`DecodePlan`] lists the timeline as [`PlanSegment`]s: a run of timeline frames that
//! show consecutive frames of one source. [`Lookahead`] decodes them ahead of the
//! playhead, and its consumer, [`LookaheadFrames`] (render's end), takes every timeline
//! frame in order with one [`Layer`] per segment that covers it: one at a cut, two inside
//! a transition, none in a gap.
//!
//! ```text
//!            seek, stop                 jobs (one segment each)
//!   Lookahead ─────────▶ glass-sched ──────────────────────────▶ glass-decode-<n>
//!                            ▲  │ lanes (segment + the consumer of its queue)  │
//!            idle, closed    │  ▼                                              │ lane queue
//!   ─────────────────────────┴─ LookaheadFrames ◀──────────────────────────────┘ (spsc, depth)
//! ```
//!
//! **Lanes.** The scheduler (`glass-sched`) walks the plan from the play position and
//! hands each segment, in order of timeline start, to an idle `glass-decode-<n>` worker as
//! a job, together with a new single-producer queue of [`LookaheadConfig::depth`] frames
//! (Decision 27's [`spsc`](crate::frame_queue::spsc)); the queue's consumer goes to
//! [`LookaheadFrames`] as a lane. The worker opens and seeks the source (the pre-roll),
//! then pushes one frame per timeline frame and parks while the queue is full. Because
//! the next segment goes to another worker as soon as one is idle and a lane is free, its
//! decoder is opened, seeked and filled while the current segment still plays: the cut is
//! ready before the playhead reaches it. At most `decode_workers + 1` lanes are open at
//! once, so decoded frames ahead of the playhead stay under `(decode_workers + 1) *
//! depth`; a worker that finished its segment takes the next while its lane drains.
//!
//! **Transitions** are segments that overlap: each has its own worker and lane, and both
//! decode. At most `decode_workers` segments may overlap at any frame (more could never
//! all be decoding at once); the scheduler drops a segment that would exceed that, with
//! a warning, rather than stall.
//!
//! **Horizon.** The consumer must know it has every lane that covers a frame before it
//! hands that frame out. After each hand-out the scheduler sends the start of the next
//! segment it has not handed out yet: every segment starting before it is already a lane.
//!
//! **Seek and stop.** As in [`playback`](crate::playback), [`Lookahead::seek`] advances
//! the shared [`Generation`] and then tells the scheduler, which restarts the plan at the
//! new frame and sends the consumer a reset. Workers blocked on a full queue wake with
//! `Stale` and drop their job; the consumer drops its old lanes when it sees the new
//! generation. Dropping the handle stops the scheduler, which drops the job channels, and
//! every worker exits after its current job; the handle joins them all.
//!
//! **Source frames.** Timeline frame `t` of a segment shows the last source frame
//! presented at or before `source_start + (t - start) * frame_duration`, compared
//! exactly, so a 25 fps source on a 30 fps timeline repeats every fifth frame (the same
//! decoded frame, shared through an `Arc`) and past its end a source holds its last frame.
//! A source that fails to open or seek yields layers with no frame (render draws the
//! offline slate), one that fails mid-segment holds its last frame, and both count in
//! [`LookaheadStats::decode_errors`].
//!
//! The decoder behind each worker is a [`SegmentDecoder`]: [`MediaDecoder`] for files
//! (hardware decode when the file allows it, with an optional software pre-roll), or
//! anything else in tests.

mod media_decoder;
mod plan;

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use media::{MediaError, Rational, Timestamp};
use render::frame_cache::SegmentHash;

use crate::frame_queue::{
    FrameStamp, Generation, PopError, PushError, SpscConsumer, SpscProducer, Stamped, spsc,
};
use crate::playback::default_decode_workers;
use crate::threads::{self, Role, ThreadRegistry};

pub use media_decoder::{GopIndexes, MediaDecoder, MediaDecoderOptions, PreRoll};
pub use plan::{DecodePlan, PlanSegment, SegmentList, SegmentListIter, SourceId, SourceRef};

/// The fewest frames a lane holds: the frame being shown and the next.
pub const MIN_DEPTH: usize = 2;
/// The most frames a lane holds; 8 frames is a quarter second at 30 fps, longer than a
/// VideoToolbox 4K seek on the M2 (median 122 ms).
pub const MAX_DEPTH: usize = 8;
/// Decoded bytes one lane may hold ahead of the playhead: 8 frames at 1080p, 5 at 4K
/// 8-bit, 2 at 4K 10-bit.
pub const LANE_BUDGET: u64 = 64 << 20;

/// The default look-ahead depth for frames of `width` x `height` at `bit_depth` bits:
/// as many 4:2:0 frames as fit in [`LANE_BUDGET`], from [`MIN_DEPTH`] to [`MAX_DEPTH`].
pub fn default_depth(width: u32, height: u32, bit_depth: u8) -> usize {
    let bytes_per_sample = if bit_depth > 8 { 2 } else { 1 };
    let frame = (u64::from(width) * u64::from(height) * 3 / 2 * bytes_per_sample).max(1);
    usize::try_from(LANE_BUDGET / frame)
        .unwrap_or(MAX_DEPTH)
        .clamp(MIN_DEPTH, MAX_DEPTH)
}

/// A decoder one worker owns: opens and seeks a segment's source, then hands out its
/// frames in presentation order.
pub trait SegmentDecoder: Send + 'static {
    type Frame: Send + Sync + 'static;

    /// Gets ready to hand out frames of `source` from `at` on: opens it (or reuses the
    /// open one) and seeks. The first frame handed out afterwards is the one on screen at
    /// `at` (the first frame for an earlier `at`).
    fn seek(&mut self, source: &SourceRef, at: Timestamp) -> Result<(), MediaError>;

    /// The next frame and its presentation time, or `None` after the last.
    fn next_frame(&mut self) -> Result<Option<(Timestamp, Self::Frame)>, MediaError>;
}

/// Pool size, depth and frame timing of a look-ahead pipeline.
#[derive(Clone, Debug)]
pub struct LookaheadConfig {
    /// `glass-decode-<n>` workers, each decoding one segment at a time (at least 2, so a
    /// transition's two segments decode at once).
    pub decode_workers: usize,
    /// Frames each lane holds ahead of the playhead.
    pub depth: usize,
    /// One timeline frame's duration.
    pub frame_duration: Rational,
    /// Where the threads register.
    pub registry: Arc<ThreadRegistry>,
}

impl Default for LookaheadConfig {
    fn default() -> Self {
        Self {
            decode_workers: default_decode_workers(),
            depth: default_depth(1920, 1080, 8),
            frame_duration: Rational::new(1, 30),
            registry: threads::global().clone(),
        }
    }
}

/// One segment's frame at a timeline frame.
#[derive(Debug)]
pub struct Layer<F> {
    pub layer: u32,
    /// With the timeline frame index, the frame cache key.
    pub segment: SegmentHash,
    pub source: SourceId,
    /// The source frame's presentation time; `None` when the source failed.
    pub source_pts: Option<Timestamp>,
    /// `None` when the source failed to open or decode.
    pub frame: Option<Arc<F>>,
}

/// Why [`LookaheadFrames::next_into`] returned no frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramesError {
    /// Not ready before the deadline (`next_into_timeout` only); try again.
    Timeout,
    /// The plan has no frames from here on.
    Finished,
    /// The pipeline stopped.
    Disconnected,
}

/// Counters across the pipeline, for tests, benchmarks and the debug panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LookaheadStats {
    /// Segment pre-rolls (open and seek).
    pub seeks: u64,
    pub seek_total: Duration,
    pub seek_max: Duration,
    /// Source frames decoded.
    pub frames_decoded: u64,
    /// Frames pushed into lanes (repeats included).
    pub frames_queued: u64,
    /// Segments whose source failed to open, seek or decode.
    pub decode_errors: u64,
    /// Segments dropped because more than `decode_workers` would overlap.
    pub segments_dropped: u64,
    /// The most frames any lane held at once.
    pub max_lane_queued: usize,
}

#[derive(Default)]
struct Counters {
    seeks: AtomicU64,
    seek_nanos: AtomicU64,
    seek_max_nanos: AtomicU64,
    frames_decoded: AtomicU64,
    frames_queued: AtomicU64,
    decode_errors: AtomicU64,
    segments_dropped: AtomicU64,
    max_lane_queued: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> LookaheadStats {
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        LookaheadStats {
            seeks: load(&self.seeks),
            seek_total: Duration::from_nanos(load(&self.seek_nanos)),
            seek_max: Duration::from_nanos(load(&self.seek_max_nanos)),
            frames_decoded: load(&self.frames_decoded),
            frames_queued: load(&self.frames_queued),
            decode_errors: load(&self.decode_errors),
            segments_dropped: load(&self.segments_dropped),
            max_lane_queued: load(&self.max_lane_queued) as usize,
        }
    }

    fn add_seek(&self, took: Duration) {
        let nanos = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
        self.seeks.fetch_add(1, Ordering::Relaxed);
        self.seek_nanos.fetch_add(nanos, Ordering::Relaxed);
        self.seek_max_nanos.fetch_max(nanos, Ordering::Relaxed);
    }
}

/// What a lane's queue carries per timeline frame.
struct LaneFrame<F> {
    source_pts: Option<Timestamp>,
    frame: Option<Arc<F>>,
}

/// A segment handed to a worker.
struct Job<F> {
    segment: PlanSegment,
    /// The first timeline frame to decode: the segment's start, or the seek position
    /// inside it.
    first: i64,
    generation: u64,
    output: SpscProducer<LaneFrame<F>>,
}

/// Messages to the scheduler.
enum ToScheduler {
    Seek {
        frame: i64,
        generation: u64,
    },
    Stop,
    /// Worker `n` finished its job.
    Idle(usize),
    /// The consumer is done with a lane.
    Closed {
        generation: u64,
        lane: u64,
    },
}

/// Messages to the consumer.
enum ToConsumer<F> {
    /// Frames of `generation` start at `frame`.
    Reset {
        generation: u64,
        frame: i64,
    },
    Lane(Lane<F>),
    /// Every segment starting before `frame` is already a lane (`i64::MAX`: all are).
    Horizon {
        generation: u64,
        frame: i64,
    },
}

struct Lane<F> {
    id: u64,
    generation: u64,
    layer: u32,
    hash: SegmentHash,
    source: SourceId,
    /// Timeline frames still to come from this lane.
    frames: Range<i64>,
    queue: SpscConsumer<LaneFrame<F>>,
    /// The frame popped for the timeline frame being assembled.
    ready: Option<LaneFrame<F>>,
    /// The worker dropped the lane early (its job went stale or the pipeline stopped).
    gone: bool,
}

/// The running look-ahead pipeline. Dropping it stops and joins every thread.
pub struct Lookahead {
    control: Sender<ToScheduler>,
    generation: Generation,
    counters: Arc<Counters>,
    threads: Vec<JoinHandle<()>>,
}

impl Lookahead {
    /// Starts the scheduler and the decode pool (one decoder from `make_decoder(n)` per
    /// worker) at timeline frame 0. Returns the handle and the consumer for render.
    pub fn start<P, D>(
        plan: P,
        config: LookaheadConfig,
        mut make_decoder: impl FnMut(usize) -> D,
    ) -> std::io::Result<(Self, LookaheadFrames<D::Frame>)>
    where
        P: DecodePlan,
        D: SegmentDecoder,
    {
        let workers = config.decode_workers.max(2);
        let depth = config.depth.max(1);
        let generation = Generation::new();
        let counters = Arc::new(Counters::default());
        let (control, control_rx) = mpsc::channel();
        let (to_consumer, consumer_rx) = mpsc::channel();
        let mut lookahead = Self {
            control: control.clone(),
            generation: generation.clone(),
            counters: counters.clone(),
            threads: Vec::with_capacity(workers + 1),
        };
        let registry = &config.registry;

        let mut jobs = Vec::with_capacity(workers);
        for n in 0..workers {
            let (job_tx, job_rx) = mpsc::channel();
            jobs.push(job_tx);
            let worker = Worker {
                index: n,
                decoder: make_decoder(n),
                jobs: job_rx,
                events: control.clone(),
                frame_duration: config.frame_duration,
                counters: counters.clone(),
            };
            lookahead
                .threads
                .push(registry.spawn(Role::Decode, Some(n), move || worker.run())?);
        }

        let scheduler = Scheduler {
            segments: plan.segments_from(0),
            plan,
            peeked: None,
            generation: 0,
            from: 0,
            control: control_rx,
            jobs,
            idle: (0..workers).rev().collect(),
            open: Vec::with_capacity(workers + 1),
            max_lanes: workers + 1,
            next_lane: 0,
            horizon: i64::MIN,
            depth,
            shared: generation.clone(),
            to_consumer,
            counters: counters.clone(),
        };
        lookahead
            .threads
            .push(registry.spawn(Role::Scheduler, None, move || scheduler.run())?);

        let frames = LookaheadFrames {
            messages: consumer_rx,
            control,
            shared: generation,
            counters,
            frame_duration: config.frame_duration,
            generation: None,
            next: 0,
            horizon: i64::MIN,
            lanes: Vec::with_capacity(workers + 2),
            max_total_queued: 0,
        };
        Ok((lookahead, frames))
    }

    /// Drops every frame in flight and continues from timeline frame `frame`. Returns the
    /// new generation.
    pub fn seek(&self, frame: i64) -> u64 {
        let generation = self.generation.advance();
        let _ = self.control.send(ToScheduler::Seek { frame, generation });
        generation
    }

    /// The pipeline's current generation.
    pub fn generation(&self) -> u64 {
        self.generation.current()
    }

    pub fn stats(&self) -> LookaheadStats {
        self.counters.snapshot()
    }

    /// Stops every thread and waits for them.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        let _ = self.control.send(ToScheduler::Stop);
        // Wakes workers blocked on a full lane.
        self.generation.advance();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Lookahead {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Scheduler<P: DecodePlan, F> {
    plan: P,
    segments: P::Segments,
    peeked: Option<PlanSegment>,
    generation: u64,
    from: i64,
    control: Receiver<ToScheduler>,
    jobs: Vec<Sender<Job<F>>>,
    idle: Vec<usize>,
    /// Lanes the consumer has not closed: id and timeline range.
    open: Vec<(u64, Range<i64>)>,
    max_lanes: usize,
    next_lane: u64,
    /// The last horizon sent for this generation.
    horizon: i64,
    depth: usize,
    shared: Generation,
    to_consumer: Sender<ToConsumer<F>>,
    counters: Arc<Counters>,
}

impl<P: DecodePlan, F: Send + Sync + 'static> Scheduler<P, F> {
    fn run(mut self) {
        let _ = self.to_consumer.send(ToConsumer::Reset {
            generation: 0,
            frame: 0,
        });
        loop {
            self.hand_out();
            match threads::idle(|| self.control.recv()) {
                Ok(message) => {
                    if !self.apply(message) {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }

    /// Applies a message; false means stop.
    fn apply(&mut self, message: ToScheduler) -> bool {
        match message {
            // Two seeks can race to the channel; the later generation wins.
            ToScheduler::Seek { frame, generation } if generation > self.generation => {
                self.generation = generation;
                self.from = frame;
                self.segments = self.plan.segments_from(frame);
                self.peeked = None;
                self.open.clear();
                self.horizon = i64::MIN;
                let _ = self
                    .to_consumer
                    .send(ToConsumer::Reset { generation, frame });
            }
            ToScheduler::Seek { .. } => {}
            ToScheduler::Stop => return false,
            ToScheduler::Idle(worker) => self.idle.push(worker),
            ToScheduler::Closed { generation, lane } => {
                if generation == self.generation {
                    self.open.retain(|(id, _)| *id != lane);
                }
            }
        }
        true
    }

    /// Hands segments to idle workers while lanes are free, then tells the consumer how
    /// far the plan has been handed out.
    fn hand_out(&mut self) {
        loop {
            if self.peeked.is_none() {
                self.peeked = self.segments.next();
            }
            let Some(segment) = &self.peeked else {
                self.send_horizon(i64::MAX);
                return;
            };
            let start = segment.timeline.start;
            let overlapping = self.open.iter().filter(|(_, r)| r.end > start).count();
            if overlapping >= self.jobs.len() {
                tracing::warn!(
                    "look-ahead: dropped the segment at frame {start}: more than {} segments overlap",
                    self.jobs.len()
                );
                self.counters
                    .segments_dropped
                    .fetch_add(1, Ordering::Relaxed);
                self.peeked = None;
                continue;
            }
            if self.open.len() >= self.max_lanes || self.idle.is_empty() {
                self.send_horizon(start);
                return;
            }
            let Some(segment) = self.peeked.take() else {
                return;
            };
            let worker = self.idle.pop().unwrap_or_default();
            let first = segment.timeline.start.max(self.from);
            let (output, queue) = spsc(self.depth, &self.shared);
            let id = self.next_lane;
            self.next_lane += 1;
            let lane = Lane {
                id,
                generation: self.generation,
                layer: segment.layer,
                hash: segment.hash,
                source: segment.source.id,
                frames: first..segment.timeline.end,
                queue,
                ready: None,
                gone: false,
            };
            self.open.push((id, segment.timeline.clone()));
            let job = Job {
                segment,
                first,
                generation: self.generation,
                output,
            };
            if self.jobs[worker].send(job).is_err() {
                return;
            }
            let _ = self.to_consumer.send(ToConsumer::Lane(lane));
        }
    }

    fn send_horizon(&mut self, frame: i64) {
        if frame > self.horizon {
            self.horizon = frame;
            let _ = self.to_consumer.send(ToConsumer::Horizon {
                generation: self.generation,
                frame,
            });
        }
    }
}

struct Worker<D: SegmentDecoder> {
    index: usize,
    decoder: D,
    jobs: Receiver<Job<D::Frame>>,
    events: Sender<ToScheduler>,
    frame_duration: Rational,
    counters: Arc<Counters>,
}

impl<D: SegmentDecoder> Worker<D> {
    fn run(mut self) {
        while let Ok(job) = threads::idle(|| self.jobs.recv()) {
            self.decode(job);
            if self.events.send(ToScheduler::Idle(self.index)).is_err() {
                return;
            }
        }
    }

    /// Pre-rolls the segment and fills its lane, one frame per timeline frame, until the
    /// segment ends or the lane goes stale or away.
    fn decode(&mut self, job: Job<D::Frame>) {
        let Job {
            segment,
            first,
            generation,
            mut output,
        } = job;
        if output.is_stale(generation) || output.is_disconnected() {
            return;
        }
        let started = Instant::now();
        let target = segment.source_time_at(first, self.frame_duration);
        let mut failed = match self.decoder.seek(&segment.source, target) {
            Ok(()) => false,
            Err(error) => {
                self.fail(&segment, &error);
                true
            }
        };
        self.counters.add_seek(started.elapsed());

        let mut current: Option<(Timestamp, Arc<D::Frame>)> = None;
        let mut upcoming: Option<(Timestamp, Arc<D::Frame>)> = None;
        let mut ended = false;
        for t in first..segment.timeline.end {
            let offset = t - segment.timeline.start;
            while !failed {
                if upcoming.is_none() && !ended {
                    match self.decoder.next_frame() {
                        Ok(Some((pts, frame))) => {
                            self.counters.frames_decoded.fetch_add(1, Ordering::Relaxed);
                            upcoming = Some((pts, Arc::new(frame)));
                        }
                        Ok(None) => ended = true,
                        Err(error) => {
                            self.fail(&segment, &error);
                            failed = true;
                        }
                    }
                }
                let take = upcoming.as_ref().is_some_and(|(pts, _)| {
                    current.is_none()
                        || plan::at_or_before(
                            *pts,
                            segment.source_start,
                            offset,
                            self.frame_duration,
                        )
                });
                if !take {
                    break;
                }
                current = upcoming.take();
            }
            let item = LaneFrame {
                source_pts: current.as_ref().map(|(pts, _)| *pts),
                frame: current.as_ref().map(|(_, frame)| frame.clone()),
            };
            let stamp = FrameStamp {
                generation,
                index: t,
                pts: Timestamp::new(t, self.frame_duration),
            };
            match threads::blocked(|| output.push(Stamped::new(stamp, item))) {
                Ok(()) => {
                    self.counters.frames_queued.fetch_add(1, Ordering::Relaxed);
                    let queued = output.capacity() - output.slots();
                    self.counters
                        .max_lane_queued
                        .fetch_max(queued as u64, Ordering::Relaxed);
                }
                Err(PushError::Stale(_) | PushError::Disconnected(_)) => return,
                Err(PushError::Full(_) | PushError::Timeout(_)) => return,
            }
        }
    }

    fn fail(&self, segment: &PlanSegment, error: &MediaError) {
        self.counters.decode_errors.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            "look-ahead: {} (frames {}..{}) has no picture: {error}",
            segment.source.path.display(),
            segment.timeline.start,
            segment.timeline.end
        );
    }
}

/// Render's end of the pipeline: every timeline frame in order, with its layers.
pub struct LookaheadFrames<F> {
    messages: Receiver<ToConsumer<F>>,
    control: Sender<ToScheduler>,
    shared: Generation,
    counters: Arc<Counters>,
    frame_duration: Rational,
    /// The generation being delivered; `None` until its reset arrives.
    generation: Option<u64>,
    next: i64,
    horizon: i64,
    lanes: Vec<Lane<F>>,
    max_total_queued: usize,
}

impl<F> LookaheadFrames<F> {
    /// Waits for the next timeline frame and replaces `layers` with its layers, bottom
    /// first (empty in a gap). Returns the frame's stamp. Reuses `layers`' allocation.
    pub fn next_into(&mut self, layers: &mut Vec<Layer<F>>) -> Result<FrameStamp, FramesError> {
        self.next_with(layers, None)
    }

    /// [`next_into`](Self::next_into) that gives up with [`FramesError::Timeout`] after
    /// `timeout`; call again to continue with the same frame.
    pub fn next_into_timeout(
        &mut self,
        layers: &mut Vec<Layer<F>>,
        timeout: Duration,
    ) -> Result<FrameStamp, FramesError> {
        self.next_with(layers, Some(Instant::now() + timeout))
    }

    /// The timeline frame the next call returns.
    pub fn position(&self) -> i64 {
        self.next
    }

    pub fn stats(&self) -> LookaheadStats {
        self.counters.snapshot()
    }

    /// The most decoded frames queued across all lanes at once, as this consumer saw it
    /// at each frame.
    pub fn max_total_queued(&self) -> usize {
        self.max_total_queued
    }

    /// Lanes open now.
    pub fn open_lanes(&self) -> usize {
        self.lanes.len()
    }

    fn next_with(
        &mut self,
        layers: &mut Vec<Layer<F>>,
        deadline: Option<Instant>,
    ) -> Result<FrameStamp, FramesError> {
        layers.clear();
        loop {
            if self.generation.is_some_and(|g| g < self.shared.current()) {
                self.drop_lanes();
                self.generation = None;
            }
            loop {
                match self.messages.try_recv() {
                    Ok(message) => self.apply(message),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if self.generation.is_none() || self.horizon <= self.next {
                            return Err(FramesError::Disconnected);
                        }
                        break;
                    }
                }
            }
            let Some(generation) = self.generation else {
                self.wait_for_message(deadline)?;
                continue;
            };
            let next = self.next;
            self.close_lanes_before(next);
            if self.horizon <= next {
                self.wait_for_message(deadline)?;
                continue;
            }
            if self.horizon == i64::MAX && self.lanes.is_empty() {
                return Err(FramesError::Finished);
            }
            let total: usize = self.lanes.iter().map(|lane| lane.queue.len()).sum();
            self.max_total_queued = self.max_total_queued.max(total);

            let mut retry = false;
            for lane in &mut self.lanes {
                if !lane.frames.contains(&next) || lane.ready.is_some() || lane.gone {
                    continue;
                }
                match pop_until(lane, next, deadline) {
                    Ok(()) => {}
                    Err(PopError::Timeout) => return Err(FramesError::Timeout),
                    Err(PopError::Disconnected | PopError::Empty) => {
                        // A stale job's worker let go of the lane; the next generation
                        // replaces it. Otherwise the pipeline is stopping.
                        lane.gone = true;
                        retry = true;
                    }
                }
            }
            if retry {
                if self.shared.current() > generation {
                    continue;
                }
                return Err(FramesError::Disconnected);
            }

            for lane in &mut self.lanes {
                if let Some(frame) = lane.ready.take() {
                    layers.push(Layer {
                        layer: lane.layer,
                        segment: lane.hash,
                        source: lane.source,
                        source_pts: frame.source_pts,
                        frame: frame.frame,
                    });
                }
            }
            layers.sort_unstable_by_key(|layer| layer.layer);
            self.next += 1;
            return Ok(FrameStamp {
                generation,
                index: next,
                pts: Timestamp::new(next, self.frame_duration),
            });
        }
    }

    fn apply(&mut self, message: ToConsumer<F>) {
        match message {
            ToConsumer::Reset { generation, frame } => {
                if generation >= self.shared.current() {
                    self.drop_lanes();
                    self.generation = Some(generation);
                    self.next = frame;
                    self.horizon = i64::MIN;
                }
            }
            ToConsumer::Lane(lane) => {
                if Some(lane.generation) == self.generation {
                    self.lanes.push(lane);
                }
            }
            ToConsumer::Horizon { generation, frame } => {
                if Some(generation) == self.generation {
                    self.horizon = self.horizon.max(frame);
                }
            }
        }
    }

    fn wait_for_message(&mut self, deadline: Option<Instant>) -> Result<(), FramesError> {
        let message = match deadline {
            None => threads::idle(|| self.messages.recv()).map_err(|_| FramesError::Disconnected),
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                threads::idle(|| self.messages.recv_timeout(left)).map_err(|e| match e {
                    RecvTimeoutError::Timeout => FramesError::Timeout,
                    RecvTimeoutError::Disconnected => FramesError::Disconnected,
                })
            }
        }?;
        self.apply(message);
        Ok(())
    }

    /// Closes lanes whose frames are all before `frame`.
    fn close_lanes_before(&mut self, frame: i64) {
        let control = &self.control;
        self.lanes.retain(|lane| {
            let done = lane.frames.end <= frame;
            if done {
                let _ = control.send(ToScheduler::Closed {
                    generation: lane.generation,
                    lane: lane.id,
                });
            }
            !done
        });
    }

    fn drop_lanes(&mut self) {
        self.lanes.clear();
    }
}

/// Pops `lane`'s frame for timeline frame `index` into `lane.ready`, dropping earlier
/// frames.
fn pop_until<F>(lane: &mut Lane<F>, index: i64, deadline: Option<Instant>) -> Result<(), PopError> {
    loop {
        let item = match deadline {
            None => threads::idle(|| lane.queue.pop())?,
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                threads::idle(|| lane.queue.pop_timeout(left))?
            }
        };
        if item.stamp.index >= index {
            lane.ready = Some(item.frame);
            return Ok(());
        }
    }
}
