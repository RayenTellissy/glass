//! The real-time shell around a [`Graph`]: [`Mixer`] runs inside the audio callback,
//! [`MixerControl`] is its non-real-time end.
//!
//! A graph arrives built (off the audio thread) together with the consumer end of its own
//! feed ring ([`Feed`]), whose frames carry every track's channels, interleaved, from a
//! known timeline frame on. Everything reaches the callback through one `rtrb` command
//! ring: a new graph ([`MixerControl::swap`]), play and pause, and setting changes
//! ([`MixerControl::set`]). The callback applies commands at the start of each callback
//! and hands every graph it retires back through a return ring, so graphs are dropped by
//! [`MixerControl::collect`], never on the audio thread.
//!
//! [`SwapAt::Now`] (start, seek) makes the new graph live at the next callback, at its
//! feed's start frame. [`SwapAt::Continue`] (an edit) keeps the live graph playing until
//! its position reaches the new feed's start, splitting a callback there if need be, so
//! the change is seamless; if the live graph is already past that frame, the new feed's
//! frames before it are dropped as they arrive.
//!
//! When a feed runs dry while playing, the missing frames are mixed as silence and the
//! timeline position moves on anyway (the clock and video keep going); the frames that
//! arrive late are dropped, so audio stays in sync. Those frames are reported as silent
//! frames by [`Mixer::render`] and counted in [`MixerStatus`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

use super::{Graph, Layout};

/// How to make a [`Mixer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixerConfig {
    pub sample_rate: u32,
    pub layout: Layout,
    /// Commands that may wait for the callback at once.
    pub commands: usize,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            layout: Layout::Stereo,
            commands: 64,
        }
    }
}

/// When a new graph takes over. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapAt {
    /// At the next callback, at the feed's start frame (start of playback, seek).
    Now,
    /// When the live graph reaches the feed's start frame (an edit).
    Continue,
}

/// A strip a setting change is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripRef {
    /// The track strip with this id.
    Track(u64),
    /// The bus with this id.
    Bus(u64),
    Master,
}

/// A setting change. The master takes only [`Param::Fader`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Param {
    Fader(f64),
    Pan(f64),
    Mute(bool),
    Solo(bool),
    SendLevel { send: usize, level_db: f64 },
}

/// Why a command was not sent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MixerError {
    #[error("the mixer's command ring is full")]
    Full,
    #[error("the graph does not fit the mixer: {0}")]
    Mismatch(&'static str),
}

/// The producer end of a graph's feed ring, for the audio decode thread. Never blocks.
pub struct Feed {
    producer: Producer<f32>,
    width: usize,
    position: i64,
}

/// The consumer end of a feed ring; goes to the callback with its graph.
pub struct FeedConsumer {
    consumer: Consumer<f32>,
    width: usize,
    start: i64,
}

impl Feed {
    /// A ring of `frames` frames of `width` samples (a graph's [`Graph::width`]) whose
    /// first frame is timeline frame `start`.
    pub fn new(width: usize, frames: usize, start: i64) -> (Feed, FeedConsumer) {
        let (producer, consumer) = RingBuffer::new((frames.max(1) * width).max(1));
        (
            Feed {
                producer,
                width,
                position: start,
            },
            FeedConsumer {
                consumer,
                width,
                start,
            },
        )
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// The timeline frame the next pushed frame is at.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// Whole frames that fit right now (unbounded for a graph without tracks).
    pub fn free_frames(&self) -> usize {
        match self.width {
            0 => usize::MAX,
            w => self.producer.slots() / w,
        }
    }

    /// Queues interleaved frames of [`width`](Self::width) samples and returns how many
    /// whole frames fit.
    pub fn push(&mut self, frames: &[f32]) -> usize {
        if self.width == 0 {
            return 0;
        }
        let n = (frames.len() / self.width).min(self.free_frames());
        let pushed = self.producer.push_partial_slice(&frames[..n * self.width]);
        debug_assert!(pushed.1.is_empty());
        self.position += n as i64;
        n
    }

    /// Whether the callback has let go of the other end (its graph was retired).
    pub fn is_abandoned(&self) -> bool {
        self.producer.is_abandoned()
    }
}

/// A graph on its way to, or inside, the callback.
struct Live {
    graph: Graph,
    feed: FeedConsumer,
    at: SwapAt,
    generation: u64,
    position: i64,
    /// Frames of the feed to drop when they arrive (mixed as silence already).
    debt: usize,
}

impl Live {
    /// Mixes `n` frames (at most a block) into `out`; returns the frames that were silence
    /// because the feed ran dry.
    fn render(&mut self, n: usize, out: &mut [f32]) -> usize {
        let width = self.feed.width;
        let mut short = 0;
        if width > 0 {
            let consumer = &mut self.feed.consumer;
            let frames_in = |c: &Consumer<f32>| c.slots().checked_div(width).unwrap_or(0);
            if self.debt > 0 {
                let drop = self.debt.min(frames_in(consumer));
                if drop > 0
                    && let Ok(chunk) = consumer.read_chunk(drop * width)
                {
                    chunk.commit_all();
                }
                self.debt -= drop;
            }
            let available = if self.debt > 0 {
                0
            } else {
                frames_in(consumer)
            };
            let take = available.min(n);
            let input = self.graph.input_mut(n);
            if take > 0
                && let Ok(chunk) = consumer.read_chunk(take * width)
            {
                let (a, b) = chunk.as_slices();
                input[..a.len()].copy_from_slice(a);
                input[a.len()..a.len() + b.len()].copy_from_slice(b);
                chunk.commit_all();
            }
            input[take * width..].fill(0.0);
            short = n - take;
            self.debt += short;
        }
        self.graph.process(self.position, n, out);
        self.position += n as i64;
        short
    }
}

enum Command {
    Swap(Box<Live>),
    Play,
    Pause,
    Set(StripRef, Param),
}

/// What the callback publishes for everyone else. Read with relaxed loads; values are
/// as of the end of the last callback.
#[derive(Debug, Default)]
pub struct MixerStatus {
    position: AtomicI64,
    generation: AtomicU64,
    playing: AtomicBool,
    underruns: AtomicU64,
    silent_frames: AtomicU64,
    leaked: AtomicU64,
}

impl MixerStatus {
    /// The live graph's timeline frame: the next frame it mixes.
    pub fn position(&self) -> i64 {
        self.position.load(Ordering::Acquire)
    }

    /// The generation [`MixerControl::swap`] returned for the live graph (0 before any).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Callbacks in which a feed ran dry while playing.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Frames mixed as silence because a feed ran dry while playing.
    pub fn silent_frames(&self) -> u64 {
        self.silent_frames.load(Ordering::Relaxed)
    }

    /// Retired graphs the return ring had no room for, kept alive rather than freed on
    /// the audio thread. Zero unless [`MixerControl::collect`] is never called.
    pub fn leaked(&self) -> u64 {
        self.leaked.load(Ordering::Relaxed)
    }
}

/// The callback side. Owned by the audio thread (moved into the device callback, or driven
/// by [`crate::OfflineOutput`]).
pub struct Mixer {
    config: MixerConfig,
    commands: Consumer<Command>,
    garbage: Producer<Box<Live>>,
    status: Arc<MixerStatus>,
    live: Option<Box<Live>>,
    pending: Option<Box<Live>>,
    playing: bool,
}

/// The non-real-time end of a [`Mixer`]: sends graphs and commands, drops retired graphs.
pub struct MixerControl {
    config: MixerConfig,
    commands: Producer<Command>,
    garbage: Consumer<Box<Live>>,
    status: Arc<MixerStatus>,
    generation: u64,
}

impl Mixer {
    /// A paused mixer without a graph (it renders silence) and its control end.
    pub fn new(config: MixerConfig) -> (Mixer, MixerControl) {
        let capacity = config.commands.max(4);
        let (command_tx, command_rx) = RingBuffer::new(capacity);
        // Every swap retires at most one graph, and the control end collects before each
        // command it sends, so the return ring never holds more than this.
        let (garbage_tx, garbage_rx) = RingBuffer::new(capacity + 2);
        let status = Arc::new(MixerStatus::default());
        (
            Mixer {
                config,
                commands: command_rx,
                garbage: garbage_tx,
                status: Arc::clone(&status),
                live: None,
                pending: None,
                playing: false,
            },
            MixerControl {
                config,
                commands: command_tx,
                garbage: garbage_rx,
                status,
                generation: 0,
            },
        )
    }

    pub fn config(&self) -> MixerConfig {
        self.config
    }

    /// Output channels per frame.
    pub fn channels(&self) -> usize {
        self.config.layout.channels()
    }

    pub fn status(&self) -> &Arc<MixerStatus> {
        &self.status
    }

    fn retire(&mut self, live: Box<Live>) {
        if let Err(rtrb::PushError::Full(live)) = self.garbage.push(live) {
            self.status.leaked.fetch_add(1, Ordering::Relaxed);
            std::mem::forget(live);
        }
    }

    fn apply_commands(&mut self) {
        while let Ok(command) = self.commands.pop() {
            match command {
                Command::Play => self.playing = true,
                Command::Pause => self.playing = false,
                Command::Set(strip, param) => {
                    if let Some(live) = &mut self.live {
                        live.graph.set(strip, param);
                    }
                    if let Some(pending) = &mut self.pending {
                        pending.graph.set(strip, param);
                    }
                }
                Command::Swap(new) => match (new.at, self.live.is_some()) {
                    (SwapAt::Continue, true) => {
                        if let Some(old) = self.pending.replace(new) {
                            self.retire(old);
                        }
                    }
                    _ => {
                        if let Some(old) = self.pending.take() {
                            self.retire(old);
                        }
                        if let Some(old) = self.live.replace(new) {
                            self.retire(old);
                        }
                    }
                },
            }
        }
    }

    /// Makes the pending graph live where the live one stands.
    fn promote(&mut self) {
        let Some(mut new) = self.pending.take() else {
            return;
        };
        if let Some(old) = self.live.take() {
            new.debt = (old.position - new.feed.start).max(0) as usize;
            new.position = old.position.max(new.feed.start);
            self.retire(old);
        }
        self.live = Some(new);
    }

    /// Fills `out` (interleaved, [`channels`](Self::channels) per frame): applies waiting
    /// commands, then mixes the live graph, or writes silence while paused or without a
    /// graph. Returns the frames mixed as silence because a feed ran dry while playing.
    /// Real-time safe.
    pub fn render(&mut self, out: &mut [f32]) -> usize {
        self.apply_commands();
        let channels = self.channels();
        let frames = out.len() / channels;
        let mut done = 0;
        let mut silent = 0;
        while done < frames {
            if let Some(pending) = &self.pending
                && self
                    .live
                    .as_ref()
                    .is_none_or(|live| live.position >= pending.feed.start)
            {
                self.promote();
            }
            let Some(live) = self.live.as_mut().filter(|_| self.playing) else {
                out[done * channels..].fill(0.0);
                if let Some(live) = &mut self.live {
                    live.graph.idle(frames - done);
                }
                break;
            };
            let mut n = (frames - done).min(live.graph.max_block());
            if let Some(pending) = &self.pending {
                n = n.min((pending.feed.start - live.position) as usize);
            }
            silent += live.render(n, &mut out[done * channels..(done + n) * channels]);
            done += n;
        }
        out[frames * channels..].fill(0.0);
        if let Some(live) = &self.live {
            self.status.position.store(live.position, Ordering::Release);
            self.status
                .generation
                .store(live.generation, Ordering::Release);
        }
        self.status.playing.store(self.playing, Ordering::Relaxed);
        if silent > 0 {
            self.status.underruns.fetch_add(1, Ordering::Relaxed);
            self.status
                .silent_frames
                .fetch_add(silent as u64, Ordering::Relaxed);
        }
        silent
    }
}

impl MixerControl {
    pub fn config(&self) -> MixerConfig {
        self.config
    }

    pub fn status(&self) -> &Arc<MixerStatus> {
        &self.status
    }

    /// Drops the graphs the callback retired. Returns how many.
    pub fn collect(&mut self) -> usize {
        let mut n = 0;
        while self.garbage.pop().is_ok() {
            n += 1;
        }
        n
    }

    fn send(&mut self, command: Command) -> Result<(), MixerError> {
        self.collect();
        self.commands.push(command).map_err(|_| MixerError::Full)
    }

    /// Sends a built graph and its feed (see [`SwapAt`]). Returns the generation the
    /// graph will report through [`MixerStatus::generation`] once live.
    pub fn swap(
        &mut self,
        graph: Graph,
        feed: FeedConsumer,
        at: SwapAt,
    ) -> Result<u64, MixerError> {
        if graph.layout() != self.config.layout {
            return Err(MixerError::Mismatch("layout"));
        }
        if graph.sample_rate() != self.config.sample_rate {
            return Err(MixerError::Mismatch("sample rate"));
        }
        if graph.width() != feed.width {
            return Err(MixerError::Mismatch("feed width"));
        }
        self.generation += 1;
        let live = Box::new(Live {
            position: feed.start,
            graph,
            feed,
            at,
            generation: self.generation,
            debt: 0,
        });
        self.send(Command::Swap(live))?;
        Ok(self.generation)
    }

    pub fn play(&mut self) -> Result<(), MixerError> {
        self.send(Command::Play)
    }

    pub fn pause(&mut self) -> Result<(), MixerError> {
        self.send(Command::Pause)
    }

    /// Changes a setting on the live graph and on one waiting to go live.
    pub fn set(&mut self, strip: StripRef, param: Param) -> Result<(), MixerError> {
        self.send(Command::Set(strip, param))
    }
}
