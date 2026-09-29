//! Error types. Every error that comes from a file names the file, and every error that
//! comes from FFmpeg keeps its numeric code and message.

use std::path::PathBuf;

use crate::format::MediaKind;
use crate::hw::FallbackReason;
use crate::source::Unsupported;
use crate::time::Timestamp;

/// An FFmpeg return code with its message (`av_strerror`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message} (FFmpeg error {code})")]
pub struct FfmpegError {
    /// The negative `AVERROR` value FFmpeg returned.
    pub code: i32,
    pub message: String,
}

impl From<ffmpeg_next::Error> for FfmpegError {
    fn from(error: ffmpeg_next::Error) -> Self {
        Self {
            message: error.to_string(),
            code: error.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("{}: cannot open: {source}", path.display())]
    Open { path: PathBuf, source: FfmpegError },

    #[error("{}: no {kind} stream", path.display())]
    NoStream { path: PathBuf, kind: MediaKind },

    #[error("{}: stream {index} does not exist", path.display())]
    NoSuchStream { path: PathBuf, index: usize },

    #[error("{}: stream {index} is {kind}, which cannot be decoded here", path.display())]
    NotDecodable {
        path: PathBuf,
        index: usize,
        kind: MediaKind,
    },

    /// This FFmpeg build has no software decoder for the codec. `reason` says what is
    /// missing.
    #[error("{}: stream {index}: no software decoder for {codec}: {reason}", path.display())]
    NoDecoder {
        path: PathBuf,
        index: usize,
        codec: String,
        reason: &'static str,
    },

    /// Hardware decoding was required ([`HwPreference::Require`]) but is not possible for
    /// this stream, or stopped working.
    ///
    /// [`HwPreference::Require`]: crate::HwPreference::Require
    #[error("{}: stream {index}: hardware decoding unavailable: {reason}", path.display())]
    Hardware {
        path: PathBuf,
        index: usize,
        reason: FallbackReason,
    },

    /// Opening, feeding or draining a decoder failed. `action` says which.
    #[error("{}: stream {index}: {action}: {source}", path.display())]
    Codec {
        path: PathBuf,
        index: usize,
        action: &'static str,
        source: FfmpegError,
    },

    #[error("{}: reading packets: {source}", path.display())]
    Read { path: PathBuf, source: FfmpegError },

    #[error("{}: seeking to {target}: {source}", path.display())]
    Seek {
        path: PathBuf,
        target: Timestamp,
        source: FfmpegError,
    },

    /// A still image, animation or sequence frame could not be read or decoded.
    #[error("{}: cannot decode the image: {message}", path.display())]
    Image { path: PathBuf, message: String },

    /// The file is a kind of picture this build or platform cannot decode; `reason` has
    /// the message for the UI.
    #[error("{}: {reason}", path.display())]
    Unsupported { path: PathBuf, reason: Unsupported },

    /// Converting a decoded frame (for example to RGBA with swscale) failed.
    #[error("converting a {format} {width}x{height} frame to {to}: {source}")]
    Convert {
        format: &'static str,
        width: u32,
        height: u32,
        to: &'static str,
        source: FfmpegError,
    },
}

impl MediaError {
    /// The FFmpeg error behind this one, when there is one.
    pub fn ffmpeg(&self) -> Option<&FfmpegError> {
        match self {
            Self::Open { source, .. }
            | Self::Codec { source, .. }
            | Self::Read { source, .. }
            | Self::Seek { source, .. }
            | Self::Convert { source, .. } => Some(source),
            Self::NoStream { .. }
            | Self::NoSuchStream { .. }
            | Self::NotDecodable { .. }
            | Self::NoDecoder { .. }
            | Self::Hardware { .. }
            | Self::Image { .. }
            | Self::Unsupported { .. } => None,
        }
    }
}

pub type Result<T, E = MediaError> = std::result::Result<T, E>;
