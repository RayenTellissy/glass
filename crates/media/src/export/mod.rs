//! Export encoding: rendered pictures and mixed audio in, a finished file out.
//!
//! An [`ExportSession`] is created from [`ExportSettings`] (a [`Container`], optional
//! [`VideoSettings`] and [`AudioSettings`]) and fed in presentation order:
//! [`push_picture`](ExportSession::push_picture) takes a [`Picture`] in caller memory
//! (packed RGBA rows from the render readback ring, or YUV planes) and
//! [`push_frame`](ExportSession::push_frame) a decoded [`VideoFrame`](crate::VideoFrame);
//! [`push_audio`](ExportSession::push_audio) takes planar f32 at the settings' rate and
//! channel count. Frames are numbered at the constant frame rate, samples at the sample
//! rate. [`push_subtitle`](ExportSession::push_subtitle) takes a
//! [`SubtitleCue`](crate::SubtitleCue) for one of the settings' [`SubtitleSettings`]
//! streams, in time order and not overlapping the stream's previous cue; cues may come
//! at any point before finishing. [`finish`](ExportSession::finish) drains the encoders
//! and closes the file.
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
//! | H.264 | `h264_videotoolbox` on macOS; `h264_nvenc`, `h264_qsv`, `h264_amf` on Windows and Linux; `libx264` | MP4, MOV, Matroska |
//! | HEVC | `hevc_videotoolbox` (Main 10 from 10-bit settings) on macOS; `hevc_nvenc`, `hevc_qsv`, `hevc_amf` on Windows and Linux; `libx265` (tagged `hvc1` in MP4 and MOV) | MP4, MOV, Matroska |
//! | ProRes | `prores_videotoolbox` on macOS, `prores_ks` with the `codec-prores-sw` feature | MOV, Matroska |
//! | AV1 | `libsvtav1`, `libaom-av1` or `librav1e` when the FFmpeg build has one | MP4, Matroska, WebM |
//! | VP9 | `libvpx-vp9` when the FFmpeg build has it | MP4, Matroska, WebM |
//! | PNG, TIFF | `png`, `tiff`, one file per frame | image sequence |
//! | GIF | the `gif` crate, a NeuQuant palette per frame | GIF |
//! | PCM | `pcm_s16le`, `pcm_s24le`, `pcm_f32le` | WAV, MOV, Matroska |
//! | AAC | FFmpeg's `aac` | M4A, MP4, MOV, Matroska |
//! | MP3 | `libmp3lame` when the FFmpeg build has it | MP3, MP4, Matroska |
//! | FLAC | `flac` (16 or 24-bit) | FLAC, Matroska |
//! | Text subtitles | `mov_text`; `webvtt` | MP4, MOV; Matroska, WebM |
//!
//! [`VideoSettings::encoder_preference`] chooses between them: [`EncoderPreference::Auto`]
//! (the default) tries the hardware encoders in the table's order and falls back to the
//! software one when a hardware encoder fails to open or fails on the first frames
//! (they are kept until it produces its first packet and replayed into the fallback, so
//! nothing of the failed encoder reaches the file); `Hardware` never falls back;
//! `Software` skips them. Hardware encoders get 4:2:0 as NV12 or P010 and a constant
//! quality mapped from the CRF. [`ExportSummary::video_encoder`] names the encoder that
//! wrote the file and [`ExportSummary::fallbacks`] the ones given up on.
//! [`VideoSettings::encoder`] names one FFmpeg encoder for the codec instead, without
//! fallback.
//!
//! [`presets`] holds the built-in platform and deliverable presets, which resolve
//! against a timeline's format into [`ExportSettings`]; [`verify`] reads a finished file
//! back against its settings and preset and reveals it in the file manager.

mod gif_writer;
mod picture;
pub mod presets;
mod session;
mod settings;
pub mod verify;

use std::path::PathBuf;

pub use picture::{Picture, PicturePlane};
pub use session::{EncoderFallback, ExportSession, ExportSummary};
pub use settings::{
    AudioCodec, AudioSettings, ColourTags, Container, ContentLight, EncoderPreference,
    ExportSettings, HdrMetadata, MasteringDisplay, ProResProfile, RateControl, SubtitleSettings,
    VideoCodec, VideoSettings, is_hardware_encoder, video_encoder_for,
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
