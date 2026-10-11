//! Live mixing: the `glass-audio-decode` thread renders the clip side of the mix ahead of
//! the playhead into the live graph's feed ring, and the audio callback's
//! [`audio::mixer::Mixer`] mixes it.
//!
//! The thread owns the project snapshot, the mix settings, the clip readers and the
//! [`MixerControl`]. It keeps the feed ring full (about 170 ms ahead at 48 kHz by default),
//! even while paused, so playback starts at once. Every structural change builds a new
//! graph and feed here, off the audio thread: a seek swaps it in at the next callback
//! ([`SwapAt::Now`]), an edit or new settings where the old feed ends ([`SwapAt::Continue`],
//! seamless, heard after what is already queued). Fader, pan, mute, solo and send changes
//! go straight to the callback as commands and ramp in within a control period, and are
//! kept in the settings for the next graph.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::thread::JoinHandle;
use std::time::Duration;

use audio::mixer::{Feed, Graph, MeterBank, MixerControl, MixerStatus, Param, StripRef, SwapAt};
use crossbeam_queue::ArrayQueue;
use project::{Project, SequenceId, TrackId};

use super::source::SequenceSource;
use super::{MIX_BLOCK, MixError, MixFormat, MixSettings, graph_spec};
use crate::threads::{self, Role};

/// How the audio decode thread feeds the mixer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioPlaybackConfig {
    /// Feed ring capacity in frames: how far ahead of the playhead clips are decoded.
    pub ring_frames: usize,
    /// Frames rendered per step.
    pub chunk_frames: usize,
    /// How long before a clip starts its file is opened.
    pub preopen: Duration,
    /// How often the thread looks for room in the ring when no message arrives.
    pub poll: Duration,
}

impl Default for AudioPlaybackConfig {
    fn default() -> Self {
        Self {
            ring_frames: 8192,
            chunk_frames: 512,
            preopen: Duration::from_secs(1),
            poll: Duration::from_millis(2),
        }
    }
}

/// Counters of the audio decode thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedStats {
    /// Feed frames rendered.
    pub rendered_frames: u64,
    /// Graphs sent to the callback.
    pub swaps: u64,
    /// Clips that went silent: missing media, unsupported speed, unopenable or unreadable
    /// files, nested sequences that cannot be mixed (`SourceStats::silent`).
    pub silent_clips: u64,
}

#[derive(Debug, Default)]
struct SharedStats {
    rendered_frames: AtomicU64,
    swaps: AtomicU64,
    silent_clips: AtomicU64,
    feed_position: AtomicU64,
}

enum Message {
    Play,
    Pause,
    Seek(i64),
    Update(Box<(Project, MixSettings)>),
    Set(StripRef, Param),
    Stop,
}

/// A running live mix. Dropping it stops the thread (the callback keeps its last graph
/// and plays silence once the feed runs dry, unless paused first).
pub struct AudioPlayback {
    tx: mpsc::Sender<Message>,
    worker: Option<JoinHandle<()>>,
    status: Arc<MixerStatus>,
    meters: Arc<ArrayQueue<Arc<MeterBank>>>,
    stats: Arc<SharedStats>,
}

impl AudioPlayback {
    /// Starts mixing `sequence` from mix frame `start`, paused, through `control` (the
    /// format is the mixer's rate and layout): builds the first graph here, then spawns
    /// `glass-audio-decode` to fill its feed.
    pub fn start(
        project: Project,
        sequence: SequenceId,
        settings: MixSettings,
        mut control: MixerControl,
        start: i64,
        config: AudioPlaybackConfig,
    ) -> Result<Self, MixError> {
        let mixer = control.config();
        let format = MixFormat {
            sample_rate: mixer.sample_rate,
            layout: mixer.layout,
        };
        let source = SequenceSource::build(&project, sequence, format, settings.declick, None)?;
        let meters = Arc::new(ArrayQueue::new(1));
        let stats = Arc::new(SharedStats::default());
        let feed = send_graph(
            &project,
            sequence,
            &settings,
            format,
            &mut control,
            &meters,
            &stats,
            &config,
            start,
            SwapAt::Now,
        )?;
        let status = Arc::clone(control.status());
        let (tx, rx) = mpsc::channel();
        let mut feeder = Feeder {
            project,
            sequence,
            settings,
            format,
            source,
            feed,
            control,
            config,
            buffer: Vec::new(),
            meters: Arc::clone(&meters),
            stats: Arc::clone(&stats),
        };
        let worker = threads::spawn(Role::AudioDecode, None, move || feeder.run(&rx))?;
        Ok(Self {
            tx,
            worker: Some(worker),
            status,
            meters,
            stats,
        })
    }

    pub fn play(&self) {
        let _ = self.tx.send(Message::Play);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Message::Pause);
    }

    /// Moves the playhead to mix frame `frame`: the feed restarts there and the callback
    /// switches to it at its next block.
    pub fn seek(&self, frame: i64) {
        let _ = self.tx.send(Message::Seek(frame));
    }

    /// A new project snapshot or new mix settings: the next graph plays it, seamlessly,
    /// after the audio already queued.
    pub fn update(&self, project: Project, settings: MixSettings) {
        let _ = self.tx.send(Message::Update(Box::new((project, settings))));
    }

    /// Changes one strip setting (a fader move, a mute) at once.
    pub fn set(&self, strip: StripRef, param: Param) {
        let _ = self.tx.send(Message::Set(strip, param));
    }

    /// The callback's position, play state and underruns.
    pub fn status(&self) -> &Arc<MixerStatus> {
        &self.status
    }

    /// The meters of the newest graph, once per graph: `None` when the bank the caller
    /// already holds is still current.
    pub fn take_meters(&self) -> Option<Arc<MeterBank>> {
        self.meters.pop()
    }

    /// Frames decoded ahead of the callback's position.
    pub fn buffered_frames(&self) -> i64 {
        self.stats.feed_position.load(Ordering::Relaxed) as i64 - self.status.position()
    }

    pub fn stats(&self) -> FeedStats {
        FeedStats {
            rendered_frames: self.stats.rendered_frames.load(Ordering::Relaxed),
            swaps: self.stats.swaps.load(Ordering::Relaxed),
            silent_clips: self.stats.silent_clips.load(Ordering::Relaxed),
        }
    }

    /// Stops the thread and waits for it.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.tx.send(Message::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Builds a graph for the current state, sends it with a new feed starting at `start`,
/// and returns the feed.
#[allow(clippy::too_many_arguments)]
fn send_graph(
    project: &Project,
    sequence: SequenceId,
    settings: &MixSettings,
    format: MixFormat,
    control: &mut MixerControl,
    meters: &ArrayQueue<Arc<MeterBank>>,
    stats: &SharedStats,
    config: &AudioPlaybackConfig,
    start: i64,
    at: SwapAt,
) -> Result<Feed, MixError> {
    let spec = graph_spec(project, sequence, settings, format, MIX_BLOCK)?;
    let graph = Graph::build(&spec)?;
    let bank = graph.meters();
    let (feed, consumer) = Feed::new(graph.width(), config.ring_frames, start);
    control.swap(graph, consumer, at)?;
    meters.force_push(bank);
    stats.swaps.fetch_add(1, Ordering::Relaxed);
    stats
        .feed_position
        .store(start.max(0) as u64, Ordering::Relaxed);
    Ok(feed)
}

/// The audio decode thread's state.
struct Feeder {
    project: Project,
    sequence: SequenceId,
    settings: MixSettings,
    format: MixFormat,
    source: SequenceSource,
    feed: Feed,
    control: MixerControl,
    config: AudioPlaybackConfig,
    buffer: Vec<f32>,
    meters: Arc<ArrayQueue<Arc<MeterBank>>>,
    stats: Arc<SharedStats>,
}

impl Feeder {
    fn run(&mut self, rx: &mpsc::Receiver<Message>) {
        loop {
            loop {
                match rx.try_recv() {
                    Ok(message) => {
                        if !self.handle(message) {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            self.control.collect();
            self.fill();
            match threads::idle(|| rx.recv_timeout(self.config.poll)) {
                Ok(message) => {
                    if !self.handle(message) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Applies a message; false to stop.
    fn handle(&mut self, message: Message) -> bool {
        let result = match message {
            Message::Stop => return false,
            Message::Play => self.control.play().map_err(MixError::from),
            Message::Pause => self.control.pause().map_err(MixError::from),
            Message::Set(strip, param) => {
                self.remember(strip, param);
                self.control.set(strip, param).map_err(MixError::from)
            }
            Message::Seek(frame) => self.resend(frame, SwapAt::Now),
            Message::Update(update) => {
                let (project, settings) = *update;
                let previous =
                    std::mem::replace(&mut self.source, SequenceSource::empty(self.format));
                match SequenceSource::build(
                    &project,
                    self.sequence,
                    self.format,
                    settings.declick,
                    Some(previous),
                ) {
                    Ok(source) => {
                        self.source = source;
                        self.project = project;
                        self.settings = settings;
                        self.resend(self.feed.position(), SwapAt::Continue)
                    }
                    Err(error) => {
                        // Keep playing the snapshot before (its readers reopen).
                        if let Ok(source) = SequenceSource::build(
                            &self.project,
                            self.sequence,
                            self.format,
                            self.settings.declick,
                            None,
                        ) {
                            self.source = source;
                        }
                        Err(error)
                    }
                }
            }
        };
        if let Err(error) = result {
            tracing::warn!(%error, "audio mix update failed");
        }
        true
    }

    /// Keeps a setting change for graphs built later.
    fn remember(&mut self, strip: StripRef, param: Param) {
        let apply =
            |fader: &mut f64, pan: &mut f64, mute: &mut bool, sends: &mut Vec<_>| match param {
                Param::Fader(db) => *fader = db,
                Param::Pan(p) => *pan = p,
                Param::Mute(m) => *mute = m,
                Param::Solo(_) => {}
                Param::SendLevel { send, level_db } => {
                    if let Some(s) = sends.get_mut(send) {
                        let s: &mut audio::mixer::SendSpec = s;
                        s.level_db = level_db;
                    }
                }
            };
        match strip {
            StripRef::Track(id) => {
                let track = self.settings.track_mut(TrackId(id));
                if let Param::Solo(solo) = param {
                    track.solo = solo;
                }
                apply(
                    &mut track.fader_db,
                    &mut track.pan,
                    &mut track.mute,
                    &mut track.sends,
                );
            }
            StripRef::Bus(id) => {
                if let Some(bus) = self.settings.buses.iter_mut().find(|b| b.id == id) {
                    apply(
                        &mut bus.fader_db,
                        &mut bus.pan,
                        &mut bus.mute,
                        &mut bus.sends,
                    );
                }
            }
            StripRef::Master => {
                if let Param::Fader(db) = param {
                    self.settings.master.fader_db = db;
                }
            }
        }
    }

    fn resend(&mut self, start: i64, at: SwapAt) -> Result<(), MixError> {
        self.feed = send_graph(
            &self.project,
            self.sequence,
            &self.settings,
            self.format,
            &mut self.control,
            &self.meters,
            &self.stats,
            &self.config,
            start,
            at,
        )?;
        let silent = self.source.stats().silent();
        self.stats
            .silent_clips
            .store(silent as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Renders clips into the feed until it is full.
    fn fill(&mut self) {
        let width = self.feed.width();
        if width == 0 {
            return;
        }
        let chunk = self.config.chunk_frames.max(1);
        self.buffer.resize(chunk * width, 0.0);
        let preopen =
            (self.config.preopen.as_secs_f64() * f64::from(self.format.sample_rate)) as i64;
        while self.feed.free_frames() >= chunk {
            let at = self.feed.position();
            self.source.prepare(at, at + preopen);
            self.source.render(at, chunk, &mut self.buffer);
            self.feed.push(&self.buffer);
            self.stats
                .rendered_frames
                .fetch_add(chunk as u64, Ordering::Relaxed);
            self.stats
                .feed_position
                .store(self.feed.position().max(0) as u64, Ordering::Relaxed);
        }
        let silent = self.source.stats().silent();
        self.stats
            .silent_clips
            .store(silent as u64, Ordering::Relaxed);
    }
}
