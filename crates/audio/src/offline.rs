//! The null backend: the same callback, ring, [`Feeder`] and [`Clock`] as a device stream,
//! driven by hand. Tests, CI machines without audio devices and export use it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::OutputConfig;
use crate::callback::{self, Callback, Feeder, Timing};
use crate::clock::{Clock, Shared, Source};

/// Sample rate of an offline stream whose config leaves it unset.
pub const DEFAULT_OFFLINE_RATE: u32 = 48_000;

/// Frames [`OfflineOutput::advance`] renders per internal block.
const BLOCK_FRAMES: usize = 512;

/// An output with no device behind it.
///
/// Built with [`OfflineOutput::new`], its clock is advanced by hand: every rendered frame
/// moves it by exactly one sample period and the latency is zero. Built with
/// [`OfflineOutput::with_device_timing`], each render carries the callback timing a device
/// would report, and the clock runs the same drift correction as a device stream (tests
/// simulate a device running fast or slow this way).
pub struct OfflineOutput {
    callback: Callback,
    shared: Arc<Shared>,
    channels: usize,
    scratch: Vec<f32>,
}

impl OfflineOutput {
    /// A manually advanced output. Returns the output, its feeder and its clock.
    pub fn new(config: &OutputConfig) -> (Self, Feeder, Clock) {
        Self::build(config, Source::Manual)
    }

    /// An output whose clock follows the callback timing passed to
    /// [`OfflineOutput::render_at`], like a device's.
    pub fn with_device_timing(config: &OutputConfig) -> (Self, Feeder, Clock) {
        Self::build(config, Source::Device)
    }

    fn build(config: &OutputConfig, source: Source) -> (Self, Feeder, Clock) {
        let rate = config.sample_rate.unwrap_or(DEFAULT_OFFLINE_RATE);
        let channels = usize::from(config.channels.max(1));
        let shared = Shared::new(rate, source);
        let (callback, feeder) =
            callback::channel(Arc::clone(&shared), channels, channels, config.ring_frames);
        let clock = Clock::new(Arc::clone(&shared));
        let output = Self {
            callback,
            shared,
            channels,
            scratch: vec![0.0; BLOCK_FRAMES * channels],
        };
        (output, feeder, clock)
    }

    /// Channels per frame of the rendered output.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Renders `out.len() / channels()` frames of interleaved audio from the ring (silence
    /// where it runs dry) and advances the clock by as many frames. Allocation-free.
    pub fn render(&mut self, out: &mut [f32]) {
        self.callback.process(out, None);
    }

    /// Renders like a device callback that runs at `callback_at` and whose first frame is
    /// audible `latency` later. Only a [`OfflineOutput::with_device_timing`] clock reads
    /// the timing; a manual clock ignores it.
    pub fn render_at(&mut self, out: &mut [f32], callback_at: Instant, latency: Duration) {
        let timing = Timing {
            now: self.shared.secs(callback_at),
            latency: latency.as_secs_f64(),
        };
        self.callback.process(out, Some(timing));
    }

    /// Renders `frames` frames into internal scratch space and discards them, advancing
    /// the clock by `frames`.
    pub fn advance(&mut self, frames: usize) {
        let mut left = frames;
        while left > 0 {
            let n = left.min(BLOCK_FRAMES);
            self.callback
                .process(&mut self.scratch[..n * self.channels], None);
            left -= n;
        }
    }
}
