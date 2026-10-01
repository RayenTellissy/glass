//! The master clock: what the audio callback publishes and what the video side reads.
//!
//! The callback is the only writer. It stores counters in atomics and the drift-corrected
//! line (see [`crate::drift`]) behind a sequence lock built from atomics, so publishing
//! never waits and a reader retries only if it raced a write. [`Clock`] is the cheap,
//! cloneable read handle the scheduler and render thread hold.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
use std::time::{Duration, Instant};

use crate::drift::Line;

/// How the clock turns a moment into a presentation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// A device (or a simulated one) with callback timestamps: the drift-corrected line.
    Device,
    /// Advanced by hand (tests, CI without audio devices, export): frames rendered so far.
    Manual,
    /// No device open: the monotonic clock since the clock was made stands in.
    System,
}

/// State shared between the callback (writer), the feeder and every [`Clock`] (readers).
#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) origin: Instant,
    pub(crate) sample_rate: u32,
    pub(crate) source: Source,
    /// Frames handed to the device (or rendered offline), silence included.
    pub(crate) frames: AtomicU64,
    /// Frames filled with silence because the ring ran dry while feeding.
    pub(crate) silent_frames: AtomicU64,
    /// Callbacks that ran dry while feeding.
    pub(crate) underruns: AtomicU64,
    /// Latest output latency (callback to audible), in nanoseconds.
    pub(crate) latency_ns: AtomicU64,
    /// Measured device rate over nominal, as `f64` bits.
    pub(crate) rate_ratio: AtomicU64,
    /// Errors reported by the device's error callback (disconnects, backend failures).
    pub(crate) errors: AtomicU64,
    /// Overloads (xruns) the device reported: its own glitches, not ring underruns.
    pub(crate) xruns: AtomicU64,
    /// Whether the feeder intends to keep the ring filled; a dry ring counts as an
    /// underrun only while this is set.
    pub(crate) feeding: AtomicBool,
    line: SeqLine,
}

impl Shared {
    pub(crate) fn new(sample_rate: u32, source: Source) -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            sample_rate: sample_rate.max(1),
            source,
            frames: AtomicU64::new(0),
            silent_frames: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            latency_ns: AtomicU64::new(0),
            rate_ratio: AtomicU64::new(1.0f64.to_bits()),
            errors: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            feeding: AtomicBool::new(true),
            line: SeqLine::default(),
        })
    }

    /// Seconds from the clock's origin to `t` (negative before it).
    pub(crate) fn secs(&self, t: Instant) -> f64 {
        match t.checked_duration_since(self.origin) {
            Some(d) => d.as_secs_f64(),
            None => -self.origin.duration_since(t).as_secs_f64(),
        }
    }

    /// Publishes a new line. Callback only (single writer).
    pub(crate) fn publish_line(&self, line: Line) {
        self.line.write(line);
    }
}

/// A sequence lock over the three `f64`s of a [`Line`], built from atomics only.
#[derive(Debug, Default)]
struct SeqLine {
    seq: AtomicU64,
    time: AtomicU64,
    frames: AtomicU64,
    slope: AtomicU64,
}

impl SeqLine {
    fn write(&self, line: Line) {
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.time.store(line.time.to_bits(), Ordering::Relaxed);
        self.frames.store(line.frames.to_bits(), Ordering::Relaxed);
        self.slope.store(line.slope.to_bits(), Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    fn read(&self) -> Line {
        loop {
            let before = self.seq.load(Ordering::Acquire);
            if before & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let line = Line {
                time: f64::from_bits(self.time.load(Ordering::Relaxed)),
                frames: f64::from_bits(self.frames.load(Ordering::Relaxed)),
                slope: f64::from_bits(self.slope.load(Ordering::Relaxed)),
            };
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return line;
            }
        }
    }
}

/// The read side of the master clock. Cheap to clone, `Send` and `Sync`; reading never
/// blocks the audio callback.
///
/// The presentation time is the timestamp, in seconds on the output stream's own timeline
/// (frame index over the sample rate), of the sample reaching the listener at a given
/// moment: frames played plus the device's output latency, drift-corrected against the
/// monotonic clock. It starts at 0 when the first frame becomes audible, never goes back
/// and never jumps. Mapping it to timeline time (start offset, pause, speed) is the
/// scheduler's job.
#[derive(Clone, Debug)]
pub struct Clock {
    shared: Arc<Shared>,
}

impl Clock {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// The stand-in for sessions with no audio device open: seconds of the monotonic
    /// clock since this call, with the same interface (no frames, no latency, rate ratio
    /// 1). Switching between it and a device clock is the scheduler's job: it re-bases
    /// the new clock on the current position.
    pub fn system(sample_rate: u32) -> Self {
        Self::new(Shared::new(sample_rate, Source::System))
    }

    /// Presentation time now, in seconds.
    pub fn now(&self) -> f64 {
        match self.shared.source {
            Source::Manual => self.manual_secs(),
            Source::System => self.shared.secs(Instant::now()).max(0.0),
            Source::Device => {
                // Read the line before taking the time, so `now` is never before its anchor.
                let line = self.shared.line.read();
                self.line_secs(line, Instant::now())
            }
        }
    }

    /// Presentation time at `instant` (for example the next display refresh), in seconds.
    /// A manually advanced clock ignores `instant`.
    pub fn at(&self, instant: Instant) -> f64 {
        match self.shared.source {
            Source::Manual => self.manual_secs(),
            Source::System => self.shared.secs(instant).max(0.0),
            Source::Device => self.line_secs(self.shared.line.read(), instant),
        }
    }

    fn manual_secs(&self) -> f64 {
        self.frames_written() as f64 / f64::from(self.shared.sample_rate)
    }

    fn line_secs(&self, line: Line, instant: Instant) -> f64 {
        let frames = line.frames_at(self.shared.secs(instant)).max(0.0);
        frames / f64::from(self.shared.sample_rate)
    }

    /// The stream's sample rate, in frames per second.
    pub fn sample_rate(&self) -> u32 {
        self.shared.sample_rate
    }

    /// Frames handed to the device (or rendered offline) so far, silence included.
    pub fn frames_written(&self) -> u64 {
        self.shared.frames.load(Ordering::Acquire)
    }

    /// The device's latest output latency: from the callback to the moment its first frame
    /// is audible. Zero for a manually advanced clock.
    pub fn latency(&self) -> Duration {
        Duration::from_nanos(self.shared.latency_ns.load(Ordering::Relaxed))
    }

    /// The device's measured rate over its nominal rate (1.001 for 0.1% fast). 1 for a
    /// manually advanced clock.
    pub fn rate_ratio(&self) -> f64 {
        f64::from_bits(self.shared.rate_ratio.load(Ordering::Relaxed))
    }

    /// Callbacks that found the ring short while the feeder was feeding.
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// Frames of silence written in place of missing audio while feeding. The scheduler
    /// subtracts them to know how far the fed audio (not the device) has got.
    pub fn silent_frames(&self) -> u64 {
        self.shared.silent_frames.load(Ordering::Relaxed)
    }
}
