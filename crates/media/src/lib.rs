//! Media I/O for Glass: a safe wrapper over FFmpeg for demux, decode, encode and probe,
//! hardware decode sessions, VFR maps, GOP indexes, stills, audio decode and resampling,
//! analysis algorithms and proxy generation. Produces platform frame handles or CPU
//! planes, never GPU objects. `unsafe` is allowed only in `media::ffi`.
//!
//! The building blocks are [`Demuxer`] (open a file, read [`Packet`]s, seek), [`Decoder`]
//! (packets in, [`Frame`]s out) and the exact time types [`Rational`] and [`Timestamp`].
//! None of the public types expose `ffmpeg-next`.
//!
//! Threads: [`Demuxer`] and [`Decoder`] are `Send` (not `Sync`) and are driven from one
//! decode-pool thread at a time; [`Packet`] and the frame types are `Send + Sync` and can
//! be handed to other threads without copying.

mod decode;
mod demux;
mod error;
#[allow(unsafe_code)]
mod ffi;
mod format;
pub mod log;
mod probe;
mod time;

pub use decode::{AudioFrame, Decoder, DecoderOptions, Frame, Plane, Received, VideoFrame};
pub use demux::{Demuxer, Packet, StreamSummary};
pub use error::{FfmpegError, MediaError, Result};
pub use format::{ColourInfo, ColourRange, Hdr, MediaKind, PixelFormat, SampleFormat, SampleType};
pub use probe::{
    AudioInfo, CodecInfo, MediaInfo, StreamDetail, StreamInfo, VfrScan, VfrSource, VideoInfo, probe,
};
pub use time::{Rational, Timestamp};

/// Initialises FFmpeg once per process and routes its log into `tracing` (see
/// [`log`]). Called by [`Demuxer::open`]; calling it earlier is harmless.
pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Err(error) = ffmpeg_next::init() {
            tracing::error!(%error, "FFmpeg initialisation failed");
        }
        ffi::install_log_bridge();
    });
}

/// Names of the optional codec features compiled into this build.
pub fn enabled_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "codec-av1") {
        features.push("codec-av1");
    }
    if cfg!(feature = "codec-prores-sw") {
        features.push("codec-prores-sw");
    }
    features
}
