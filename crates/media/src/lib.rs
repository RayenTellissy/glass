//! Media I/O for Glass: a safe wrapper over FFmpeg for demux, decode, encode and probe,
//! hardware decode sessions, VFR maps, GOP indexes, stills, audio decode and resampling,
//! analysis algorithms and proxy generation. Produces platform frame handles or CPU
//! planes, never GPU objects. `unsafe` is allowed only in `media::ffi`.
//!
//! The building blocks are [`Demuxer`] (open a file, read [`Packet`]s, seek), [`Decoder`]
//! (packets in, [`Frame`]s out) and the exact time types [`Rational`] and [`Timestamp`].
//! None of the public types expose `ffmpeg-next`. Hardware decode ([`HwPreference`])
//! hands out [`HwFrame`] surfaces (a retained `CVPixelBuffer` on macOS) instead of CPU
//! planes; their platform calls live in `media::ffi` too.
//!
//! Threads: [`Demuxer`] and [`Decoder`] are `Send` (not `Sync`) and are driven from one
//! decode-pool thread at a time; [`Packet`] and the frame types are `Send + Sync` and can
//! be handed to other threads without copying.

pub mod cache;
pub mod colour;
mod convert;
mod decode;
mod demux;
mod error;
#[allow(unsafe_code)]
mod ffi;
mod format;
pub mod gop;
mod hw;
pub mod log;
mod probe;
mod reader;
mod scan;
mod time;
pub mod timemap;

pub use cache::{CacheDir, ContentHash};
pub use colour::{ChromaSiting, ColourSpace, Matrix, Primaries, Range, TransferFn};
pub use convert::RgbaImage;
pub use decode::{
    AudioFrame, Decoder, DecoderOptions, Frame, FrameStorage, Plane, Received, VideoFrame,
};
pub use demux::{Demuxer, Packet, StreamSummary};
pub use error::{FfmpegError, MediaError, Result};
pub use format::{ColourInfo, ColourRange, Hdr, MediaKind, PixelFormat, SampleFormat, SampleType};
pub use gop::{GopIndex, GopSource, Keyframe};
pub use hw::{
    DecodePath, FallbackReason, HwApi, HwFrame, HwPreference, HwSurfaceFormat,
    hardware_decoder_available,
};
pub use probe::{
    AudioInfo, CodecInfo, MediaInfo, StreamDetail, StreamInfo, VfrScan, VfrSource, VideoInfo, probe,
};
pub use reader::{FrameReader, StreamSelect};
pub use scan::PacketEntry;
pub use time::{Rational, Timestamp};
pub use timemap::{SourceTimeline, TimelineCache, TimestampMap};

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
