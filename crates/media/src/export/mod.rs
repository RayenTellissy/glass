//! Export encoding: rendered pictures and mixed audio in, a finished file out.
//!
//! An [`ExportSession`] is created from [`ExportSettings`] (a [`Container`], optional
//! [`VideoSettings`] and [`AudioSettings`]) and fed in presentation order:
//! [`push_picture`](ExportSession::push_picture) takes a [`Picture`] in caller memory
//! (packed RGBA rows from the render readback ring, or YUV planes) and
//! [`push_frame`](ExportSession::push_frame) a decoded [`VideoFrame`](crate::VideoFrame);
//! [`push_audio`](ExportSession::push_audio) takes planar f32 at the settings' rate and
//! channel count. Frames are numbered at the constant frame rate, samples at the sample
//! rate. [`finish`](ExportSession::finish) drains the encoders and closes the file.
//!
//! RGB pictures become YUV with the matrix, range and chroma siting of the
//! [`ColourTags`] (swscale), and the same tags are written to the stream; the values are
//! taken as already encoded for that transfer (the render output node encodes them,
//! Decision 21). YUV pictures are copied value for value.
//!
//! Codecs and the encoders behind them:
//!
//! | Codec | Encoder | Containers |
//! | --- | --- | --- |
//! | H.264 | `libx264` | MP4, MOV, Matroska |
//! | HEVC | `libx265` (tagged `hvc1` in MP4 and MOV) | MP4, MOV, Matroska |
//! | ProRes | `prores_videotoolbox` on macOS, `prores_ks` with the `codec-prores-sw` feature | MOV, Matroska |
//! | AV1 | `libsvtav1`, `libaom-av1` or `librav1e` when the FFmpeg build has one | MP4, Matroska, WebM |
//! | VP9 | `libvpx-vp9` when the FFmpeg build has it | MP4, Matroska, WebM |
//! | PNG, TIFF | `png`, `tiff`, one file per frame | image sequence |
//! | GIF | the `gif` crate, a NeuQuant palette per frame | GIF |
//! | PCM | `pcm_s16le`, `pcm_s24le`, `pcm_f32le` | WAV, MOV, Matroska |
//! | AAC | FFmpeg's `aac` | M4A, MP4, MOV, Matroska |
//! | MP3 | `libmp3lame` when the FFmpeg build has it | MP3, MP4, Matroska |
//! | FLAC | `flac` (16 or 24-bit) | FLAC, Matroska |
//!
//! [`VideoSettings::encoder`] names another FFmpeg encoder for the same codec (the
//! hardware encoders of the next Phase 8 task go through it).

mod gif_writer;
mod picture;
mod session;
mod settings;

use std::path::PathBuf;

pub use picture::{Picture, PicturePlane};
pub use session::{ExportSession, ExportSummary};
pub use settings::{
    AudioCodec, AudioSettings, ColourTags, Container, ExportSettings, ProResProfile, RateControl,
    VideoCodec, VideoSettings, video_encoder_for,
};

use crate::error::MediaError;

/// Why an export could not start or stopped.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Media(#[from] MediaError),

    /// This build has no encoder for the codec. `tried` lists the FFmpeg encoders looked
    /// for.
    #[error("no {codec} encoder in this build (looked for {})", tried.join(", "))]
    Unavailable {
        codec: &'static str,
        tried: Vec<&'static str>,
    },

    /// The container cannot carry the codec.
    #[error("{container} cannot carry {codec}")]
    Incompatible {
        container: &'static str,
        codec: &'static str,
    },

    /// The settings contradict each other or the encoder (size, pixel format, colour,
    /// rate).
    #[error("export settings: {0}")]
    Settings(String),

    /// A picture or audio buffer does not match the session's settings.
    #[error("export input: {0}")]
    Input(String),

    /// Writing a file the session writes itself (image sequence frames, GIF) failed.
    #[error("{}: {message}", path.display())]
    Write { path: PathBuf, message: String },
}

pub type Result<T, E = ExportError> = std::result::Result<T, E>;
