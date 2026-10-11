//! Real-time audio for Glass: device I/O through `cpal`, the audio callback, the mixer
//! graph, audio effects, meters, the master clock and voice-over recording. Receives f32
//! blocks through ring buffers and depends on no other Glass crate.
//!
//! Output today: [`OutputStream`] (a `cpal` device) or [`OfflineOutput`] (no device, the
//! clock advanced by hand), both returning a [`Feeder`] for the audio decode thread and a
//! [`Clock`] for the scheduler and render thread. The callback that fills the device is
//! the master clock; [`drift`] keeps its presentation time smooth against the monotonic
//! clock. [`video_sync`] picks the video frame for each display refresh from it.
//!
//! Mixing: [`mixer`] holds the mixer graph (track strips, buses and sends, the master,
//! meters, automation), run inside the callback by a [`mixer::Mixer`]
//! ([`OutputStream::open_with_mixer`], [`OfflineOutput::with_mixer`]) or offline for
//! export.

mod callback;
mod clock;
mod device;
pub mod drift;
pub mod mixer;
mod offline;
pub mod video_sync;

pub use callback::Feeder;
pub use clock::Clock;
pub use device::{OutputError, OutputStream};
pub use offline::{DEFAULT_OFFLINE_RATE, OfflineOutput};

/// How to open an output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputConfig {
    /// Frames per second: the project rate. `None` takes the device's own rate (for a
    /// device) or [`DEFAULT_OFFLINE_RATE`] (offline); read the result from
    /// [`Clock::sample_rate`].
    pub sample_rate: Option<u32>,
    /// Channels per frame the feeder writes.
    pub channels: u16,
    /// Ring capacity in frames: how far the feeder may run ahead of the callback.
    pub ring_frames: usize,
    /// Device buffer size in frames; `None` takes the device default.
    pub buffer_frames: Option<u32>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            sample_rate: None,
            channels: 2,
            // About 340 ms at 48 kHz.
            ring_frames: 16_384,
            buffer_frames: None,
        }
    }
}
