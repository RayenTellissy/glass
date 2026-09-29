//! Error types. Every error that comes from a file names the file, and every error that
//! comes from FFmpeg keeps its numeric code and message.

use std::path::PathBuf;

use crate::format::MediaKind;
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

    #[error("{}: stream {index}: no decoder for codec {codec}", path.display())]
    NoDecoder {
        path: PathBuf,
        index: usize,
        codec: String,
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
            | Self::NoDecoder { .. } => None,
        }
    }
}

pub type Result<T, E = MediaError> = std::result::Result<T, E>;
