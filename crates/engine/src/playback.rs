//! The playback pipeline's threads: `glass-sched`, the `glass-decode-<n>` pool and
//! `glass-render`, wired with [`frame_queue`](crate::frame_queue) and a control channel.
//!
//! This is the skeleton later Phase 3 tasks fill in: the scheduler asks for frames in
//! order from the play position as fast as back-pressure allows (no clock, look-ahead
//! window or cache yet), each decoder turns a request into a frame through a [`Decode`]
//! implementation, and render puts the decoded frames back in order, turns each into an
//! output frame through a [`Render`] implementation and offers it to the presenter.
//! [`PassThrough`] does both with no media, for tests and until real decoding lands.
//!
//! ```text
//!             control (play, pause, seek, stop)
//!   Playback ────────────────────────────────▶ glass-sched
//!                                               │ request rings (spsc, one per decoder,
//!                                               │ frame index n goes to decoder n % N)
//!                                               ▼
//!                                         glass-decode-<n>
//!                                               │ decode queue (mpsc)
//!                                               ▼
//!                  segment starts (std mpsc)  glass-render ── present queue (spsc) ──▶ UI
//!   glass-sched ──────────────────────────────▶
//! ```
//!
//! **Seek and stop.** Every queue shares the pipeline's [`Generation`]. [`Playback::seek`]
//! advances it and then sends the scheduler the new generation and position, so frames
//! already in flight are dropped wherever they are and a scheduler or decoder blocked on
//! a full queue wakes with `Stale`. The generation only moves through the handle, always
//! followed by a message, so a scheduler whose push comes back `Stale` waits for that
//! message. Before it sends requests for a new generation the scheduler tells render
//! where the new segment starts, so render knows which frame index comes first.
//!
//! **Shutdown.** Dropping the handle (or [`Playback::shutdown`]) sends `Stop` and
//! advances the generation. The scheduler exits and drops its request rings, each
//! decoder then sees its ring disconnected and exits, dropping its decode producer, and
//! render sees the decode queue disconnected and exits; the handle joins all of them. If
//! the presenter drops its receiver first, render exits and the disconnect travels up
//! the same way.
//!
//! **Waiting.** Every wait parks (the frame queues' parkers, a channel receive) and is
//! marked in the thread registry: waiting for work as idle, waiting for room downstream
//! as blocked. The one timed wait is render's: the presenter never wakes render (render
//! must not wait on the UI), so while the present queue is full render offers its frame
//! again every [`PRESENT_RETRY`].
//!
//! **Seams.** The master clock (in `audio`) replaces "as fast as back-pressure allows" in
//! the scheduler, and its device callback is wrapped in the registry's
//! [`AudioCallbackHook`](crate::threads::AudioCallbackHook), handed to `audio` as a
//! closure. The rendered-frame cache (in `render`) sits behind a [`Render`]
//! implementation, with its index on the scheduler.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvError, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::Duration;

use media::{Rational, Timestamp};

use crate::frame_queue::{
    FrameStamp, Generation, MpscConsumer, MpscProducer, PopError, PushError, SpscConsumer,
    SpscProducer, Stamped, mpsc as mpsc_queue, spsc,
};
use crate::threads::{self, Role, ThreadRegistry};

/// Threads the rest of the app keeps busy during playback: UI, render, and audio decode
/// with the audio callback (a core's worth of work between them). The scheduler only
/// hands out requests and the analysis pool pauses during playback.
const RESERVED_THREADS: usize = 3;
/// At least two decoders, so a cut or transition can decode two streams at once.
const MIN_DECODERS: usize = 2;
/// Upper bound for this machine class (8 GB): a worker can hold a 4K software decoder
/// (about 100 to 150 MB of frames and references), so four stay near 0.6 GB.
const MAX_DECODERS: usize = 4;

/// How often render offers a frame again while the present queue is full.
pub const PRESENT_RETRY: Duration = Duration::from_millis(2);

/// Decode workers for this machine: logical cores minus [`RESERVED_THREADS`], from 2 to
/// 4 (4 on an 8-core M1 or M2).
pub fn default_decode_workers() -> usize {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    cores
        .saturating_sub(RESERVED_THREADS)
        .clamp(MIN_DECODERS, MAX_DECODERS)
}

/// Turns a frame request into a decoded frame, on a `glass-decode-<n>` thread. One
/// instance per worker.
pub trait Decode: Send + 'static {
    type Frame: Send + 'static;

    fn decode(&mut self, stamp: &FrameStamp) -> Self::Frame;
}

/// Turns decoded frames, in frame order, into frames for the presenter, on
/// `glass-render`.
pub trait Render: Send + 'static {
    type Input: Send + 'static;
    type Output: Send + 'static;

    fn render(&mut self, stamp: &FrameStamp, frame: Self::Input) -> Self::Output;
}

/// Decodes and renders nothing: every frame is its own stamp.
#[derive(Clone, Copy, Debug, Default)]
pub struct PassThrough;

impl Decode for PassThrough {
    type Frame = FrameStamp;

    fn decode(&mut self, stamp: &FrameStamp) -> FrameStamp {
        *stamp
    }
}

impl Render for PassThrough {
    type Input = FrameStamp;
    type Output = FrameStamp;

    fn render(&mut self, _stamp: &FrameStamp, frame: FrameStamp) -> FrameStamp {
        frame
    }
}

/// Queue depths, pool size and frame timing of a pipeline.
#[derive(Clone, Debug)]
pub struct PlaybackConfig {
    /// `glass-decode-<n>` workers.
    pub decode_workers: usize,
    /// Requests queued per decoder.
    pub request_depth: usize,
    /// Decoded frames queued for render.
    pub decode_depth: usize,
    /// Rendered frames queued for the presenter.
    pub present_depth: usize,
    /// One frame's duration: frame `n` is presented at `n` times this.
    pub frame_duration: Rational,
    /// Where the threads register.
    pub registry: Arc<ThreadRegistry>,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            decode_workers: default_decode_workers(),
            request_depth: 2,
            decode_depth: 8,
            present_depth: 3,
            frame_duration: Rational::new(1, 24),
            registry: threads::global().clone(),
        }
    }
}

enum Control {
    Play,
    Pause,
    Seek { index: i64, generation: u64 },
    Stop,
}

/// Where a generation's frames start, from the scheduler to render.
struct Segment {
    generation: u64,
    first: i64,
}

/// The running pipeline. Dropping it stops and joins every thread.
pub struct Playback {
    control: Sender<Control>,
    generation: Generation,
    threads: Vec<JoinHandle<()>>,
    decode_workers: usize,
}

/// The presenter's end: rendered frames in frame order.
pub type Presented<T> = SpscConsumer<T>;

impl Playback {
    /// Starts render, the decode pool (one decoder from `make_decoder(n)` per worker) and
    /// the scheduler, paused at frame 0. Returns the handle and the present queue's
    /// receiving end for the UI.
    pub fn start<D, R>(
        config: PlaybackConfig,
        mut make_decoder: impl FnMut(usize) -> D,
        renderer: R,
    ) -> std::io::Result<(Self, Presented<R::Output>)>
    where
        D: Decode,
        R: Render<Input = D::Frame>,
    {
        let workers = config.decode_workers.max(1);
        let generation = Generation::new();
        let (control, control_rx) = mpsc::channel();
        let (segments, segments_rx) = mpsc::channel();
        let (decoded_tx, decoded_rx) =
            mpsc_queue::<D::Frame>(config.decode_depth.max(1), &generation);
        let (present_tx, present_rx) = spsc::<R::Output>(config.present_depth.max(1), &generation);
        let mut playback = Self {
            control,
            generation: generation.clone(),
            threads: Vec::with_capacity(workers + 2),
            decode_workers: workers,
        };
        let registry = &config.registry;

        // Render holds at most every frame that can be in flight ahead of the one it
        // waits for.
        let reorder = workers * (config.request_depth.max(1) + 1) + config.decode_depth.max(1);
        let render = RenderLoop {
            renderer,
            input: decoded_rx,
            output: present_tx,
            segments: segments_rx,
            generation: 0,
            expected: 0,
            waiting: Vec::with_capacity(reorder),
            pending: None,
        };
        playback
            .threads
            .push(registry.spawn(Role::Render, None, move || render.run())?);

        let mut requests = Vec::with_capacity(workers);
        for n in 0..workers {
            let (request_tx, request_rx) = spsc::<()>(config.request_depth.max(1), &generation);
            requests.push(request_tx);
            let worker = DecodeLoop {
                decoder: make_decoder(n),
                requests: request_rx,
                output: decoded_tx.clone(),
            };
            playback
                .threads
                .push(registry.spawn(Role::Decode, Some(n), move || worker.run())?);
        }
        drop(decoded_tx);

        let scheduler = Scheduler {
            control: control_rx,
            requests,
            segments,
            frame_duration: config.frame_duration,
            generation: 0,
            playing: false,
            next: 0,
        };
        playback
            .threads
            .push(registry.spawn(Role::Scheduler, None, move || scheduler.run())?);
        Ok((playback, present_rx))
    }

    /// Plays from the current position.
    pub fn play(&self) {
        let _ = self.control.send(Control::Play);
    }

    /// Stops asking for new frames; frames already requested still arrive.
    pub fn pause(&self) {
        let _ = self.control.send(Control::Pause);
    }

    /// Drops every frame in flight and continues from frame `index` (playing or paused
    /// as before). Returns the new generation.
    pub fn seek(&self, index: i64) -> u64 {
        let generation = self.generation.advance();
        let _ = self.control.send(Control::Seek { index, generation });
        generation
    }

    /// The pipeline's current generation.
    pub fn generation(&self) -> u64 {
        self.generation.current()
    }

    pub fn decode_workers(&self) -> usize {
        self.decode_workers
    }

    /// Stops every thread and waits for them.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        let _ = self.control.send(Control::Stop);
        // Wakes a scheduler or decoder blocked on a full queue.
        self.generation.advance();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Scheduler {
    control: Receiver<Control>,
    requests: Vec<SpscProducer<()>>,
    segments: Sender<Segment>,
    frame_duration: Rational,
    generation: u64,
    playing: bool,
    next: i64,
}

impl Scheduler {
    fn run(mut self) {
        let _ = self.segments.send(Segment {
            generation: self.generation,
            first: self.next,
        });
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
            if !self.playing {
                if !self.wait_for_control() {
                    return;
                }
                continue;
            }
            let index = self.next;
            let worker = index.rem_euclid(self.requests.len() as i64) as usize;
            let request = Stamped::new(
                FrameStamp {
                    generation: self.generation,
                    index,
                    pts: Timestamp::new(index, self.frame_duration),
                },
                (),
            );
            let ring = &mut self.requests[worker];
            match threads::blocked(|| ring.push(request)) {
                Ok(()) => self.next += 1,
                // The handle moved the generation on and its message is on the way.
                Err(PushError::Stale(_)) => {
                    if !self.wait_for_control() {
                        return;
                    }
                }
                Err(PushError::Disconnected(_)) => return,
                Err(PushError::Full(_) | PushError::Timeout(_)) => {}
            }
        }
    }

    fn wait_for_control(&mut self) -> bool {
        match threads::idle(|| self.control.recv()) {
            Ok(control) => self.apply(control),
            Err(RecvError) => false,
        }
    }

    /// Applies a control message; false means stop.
    fn apply(&mut self, control: Control) -> bool {
        match control {
            Control::Play => self.playing = true,
            Control::Pause => self.playing = false,
            // Two seeks can race to the channel; the later generation wins.
            Control::Seek { index, generation } if generation > self.generation => {
                self.generation = generation;
                self.next = index;
                let _ = self.segments.send(Segment {
                    generation,
                    first: index,
                });
            }
            Control::Seek { .. } => {}
            Control::Stop => return false,
        }
        true
    }
}

struct DecodeLoop<D: Decode> {
    decoder: D,
    requests: SpscConsumer<()>,
    output: MpscProducer<D::Frame>,
}

impl<D: Decode> DecodeLoop<D> {
    fn run(mut self) {
        loop {
            let request = match threads::idle(|| self.requests.pop()) {
                Ok(request) => request,
                Err(PopError::Disconnected) => return,
                Err(PopError::Empty | PopError::Timeout) => continue,
            };
            if self.output.is_stale(request.stamp.generation) {
                continue;
            }
            let frame = self.decoder.decode(&request.stamp);
            let output = &mut self.output;
            match threads::blocked(|| output.push(Stamped::new(request.stamp, frame))) {
                Ok(()) | Err(PushError::Stale(_)) => {}
                Err(PushError::Disconnected(_)) => return,
                Err(PushError::Full(_) | PushError::Timeout(_)) => {}
            }
        }
    }
}

struct RenderLoop<R: Render> {
    renderer: R,
    input: MpscConsumer<R::Input>,
    output: SpscProducer<R::Output>,
    segments: Receiver<Segment>,
    /// The segment render is putting in order.
    generation: u64,
    expected: i64,
    /// Decoded frames that arrived ahead of `expected`.
    waiting: Vec<Stamped<R::Input>>,
    /// A rendered frame the present queue had no room for.
    pending: Option<Stamped<R::Output>>,
}

impl<R: Render> RenderLoop<R> {
    fn run(mut self) {
        loop {
            if let Some(frame) = self.pending.take() {
                match self.output.try_push(frame) {
                    Ok(()) | Err(PushError::Stale(_)) => {}
                    Err(PushError::Full(frame)) => {
                        self.pending = Some(frame);
                        threads::blocked(|| std::thread::park_timeout(PRESENT_RETRY));
                        continue;
                    }
                    Err(PushError::Disconnected(_)) => return,
                    Err(PushError::Timeout(_)) => {}
                }
            }
            if let Some(at) = self
                .waiting
                .iter()
                .position(|f| f.stamp.index == self.expected)
            {
                let frame = self.waiting.swap_remove(at);
                self.expected += 1;
                let rendered = self.renderer.render(&frame.stamp, frame.frame);
                self.pending = Some(Stamped::new(frame.stamp, rendered));
                continue;
            }
            match threads::idle(|| self.input.pop()) {
                Ok(frame) => {
                    if frame.stamp.generation > self.generation {
                        self.start_segment(frame.stamp.generation);
                    }
                    if frame.stamp.generation == self.generation {
                        self.waiting.push(frame);
                    }
                }
                Err(PopError::Disconnected) => return,
                Err(PopError::Empty | PopError::Timeout) => {}
            }
        }
    }

    /// Moves to the segment of `generation`: the scheduler sent its start before any of
    /// its requests.
    fn start_segment(&mut self, generation: u64) {
        while let Ok(segment) = self.segments.try_recv() {
            if segment.generation != self.generation {
                self.generation = segment.generation;
                self.expected = segment.first;
                self.waiting.clear();
            }
            if segment.generation >= generation {
                break;
            }
        }
    }
}
