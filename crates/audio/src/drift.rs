//! Drift correction between the audio device clock and the monotonic clock.
//!
//! The device plays frames at its own crystal's rate, which differs from the nominal rate
//! by up to a few hundred ppm when measured against the monotonic clock, and each callback
//! reports its timing with some jitter. [`DriftEstimator`] turns the stream of callback
//! observations ("frame `n` becomes audible at monotonic time `t`") into a smooth mapping
//! from monotonic time to frames:
//!
//! 1. A second-order delay-locked loop (F. Adriaensen, "Using a DLL to filter time", 2005)
//!    filters the observations into a line: the audible time of a reference frame and the
//!    measured seconds per frame. Its bandwidth starts wide so the rate locks within
//!    seconds and narrows so callback jitter is rejected once it has.
//! 2. The published line never jumps to the loop's estimate. At each update it starts
//!    from where the previous published line is at that moment and steers towards the
//!    loop's estimate by changing its slope, by at most [`MAX_SLEW`] of the rate, so the
//!    reported position is continuous and strictly increasing.
//!
//! Only a gap larger than [`RESYNC`] (a stalled or restarted device) re-seeds the loop
//! and moves the published line in one step.
//!
//! Pure arithmetic with no allocation: it runs inside the audio callback.

use std::f64::consts::{PI, SQRT_2};

/// The loop bandwidth, in Hz, when the first observation arrives.
pub const START_BANDWIDTH: f64 = 1.5;
/// The loop bandwidth, in Hz, once locked.
pub const LOCKED_BANDWIDTH: f64 = 0.1;
/// How fast, in seconds of observations, the bandwidth narrows from start to locked.
pub const BANDWIDTH_DECAY: f64 = 2.0;
/// The published line closes its distance to the loop's estimate with this time constant,
/// in seconds.
pub const SLEW_TIME: f64 = 0.5;
/// The largest slope correction of the published line, as a fraction of the rate (0.5%).
pub const MAX_SLEW: f64 = 0.005;
/// An observation further than this, in seconds, from the loop's prediction re-seeds it.
pub const RESYNC: f64 = 0.1;

/// A line mapping monotonic time to frames: `frames(t) = frames + (t - time) * slope`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Line {
    /// Seconds on the monotonic clock where the line is anchored.
    pub time: f64,
    /// Frames (fractional) at `time`.
    pub frames: f64,
    /// Frames per second of monotonic time.
    pub slope: f64,
}

impl Line {
    /// Frames at monotonic time `t`, in seconds.
    pub fn frames_at(&self, t: f64) -> f64 {
        self.frames + (t - self.time) * self.slope
    }
}

/// The delay-locked loop plus the slewed output line. See the module documentation.
#[derive(Clone, Debug)]
pub struct DriftEstimator {
    nominal_rate: f64,
    started: bool,
    /// The loop's filtered audible time of frame `ref_frames`.
    ref_time: f64,
    ref_frames: f64,
    /// The loop's measured seconds per frame.
    secs_per_frame: f64,
    /// Seconds of audio observed since the loop was seeded, for the bandwidth schedule.
    observed: f64,
    output: Line,
    resyncs: u64,
}

impl DriftEstimator {
    pub fn new(nominal_rate: u32) -> Self {
        let nominal_rate = f64::from(nominal_rate.max(1));
        Self {
            nominal_rate,
            started: false,
            ref_time: 0.0,
            ref_frames: 0.0,
            secs_per_frame: 1.0 / nominal_rate,
            observed: 0.0,
            output: Line::default(),
            resyncs: 0,
        }
    }

    /// Feeds one callback: frame `frames` becomes audible at `audible_at`, and the callback
    /// runs at `now` (both seconds on the monotonic clock). The published line changes at
    /// `now`, so readers see no step at or after it.
    pub fn observe(&mut self, audible_at: f64, frames: u64, now: f64) {
        let frames = frames as f64;
        if !self.started {
            self.seed(audible_at, frames, now);
            return;
        }
        let n = frames - self.ref_frames;
        if n <= 0.0 {
            return;
        }
        let predicted = self.ref_time + n * self.secs_per_frame;
        let error = audible_at - predicted;
        if error.abs() > RESYNC {
            self.resyncs += 1;
            self.seed(audible_at, frames, now);
            return;
        }
        let dt = n / self.nominal_rate;
        let bandwidth = LOCKED_BANDWIDTH
            + (START_BANDWIDTH - LOCKED_BANDWIDTH) * (-self.observed / BANDWIDTH_DECAY).exp();
        let omega = (2.0 * PI * bandwidth * dt).min(0.5);
        self.ref_time = predicted + SQRT_2 * omega * error;
        self.ref_frames = frames;
        self.secs_per_frame += omega * omega * error / n;
        self.observed += dt;

        let target_slope = 1.0 / self.secs_per_frame;
        let target = self.ref_frames + (now - self.ref_time) * target_slope;
        let current = self.output.frames_at(now);
        let limit = MAX_SLEW * target_slope;
        let correction = ((target - current) / SLEW_TIME).clamp(-limit, limit);
        self.output = Line {
            time: now,
            frames: current,
            slope: target_slope + correction,
        };
    }

    /// Starts the loop at the nominal rate and the published line on this observation.
    fn seed(&mut self, audible_at: f64, frames: f64, now: f64) {
        self.started = true;
        self.ref_time = audible_at;
        self.ref_frames = frames;
        self.secs_per_frame = 1.0 / self.nominal_rate;
        self.observed = 0.0;
        self.output = Line {
            time: now,
            frames: frames + (now - audible_at) * self.nominal_rate,
            slope: self.nominal_rate,
        };
    }

    /// The published line (all zero before the first observation).
    pub fn line(&self) -> Line {
        self.output
    }

    /// The device's measured rate over its nominal rate (1.001 for a device 0.1% fast).
    pub fn rate_ratio(&self) -> f64 {
        1.0 / (self.secs_per_frame * self.nominal_rate)
    }

    /// How many times a gap larger than [`RESYNC`] re-seeded the loop.
    pub fn resyncs(&self) -> u64 {
        self.resyncs
    }
}
