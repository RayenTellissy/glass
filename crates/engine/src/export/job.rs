//! [`ExportJob`]: what one export renders and where it goes.

use std::ops::Range;
use std::path::{Path, PathBuf};

use media::Primaries;
use media::export::presets::{Preset, TimelineFormat};
use media::export::{ColourTags, ExportSettings};
use project::{ColourSpace, Project, Sequence, SequenceId, TrackId};
use render::nodes::OutputTransfer;
use render::subtitle::SubtitleStyle;

use super::events::ExportFailure;

/// The sidecar subtitle file formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SidecarFormat {
    /// SubRip, `.srt`.
    Srt,
    /// WebVTT, `.vtt`.
    Vtt,
}

impl SidecarFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }
}

/// What happens to a subtitle track of the sequence.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SubtitleExport {
    #[default]
    None,
    /// A `.srt` or `.vtt` file next to the output, with the output's name.
    Sidecar {
        track: TrackId,
        format: SidecarFormat,
    },
    /// A text stream in the output (`mov_text` in MP4 and MOV, WebVTT in Matroska and
    /// WebM), with an ISO 639-2 language code.
    Embedded {
        track: TrackId,
        language: String,
        title: Option<String>,
    },
    /// Drawn into every frame the cue covers. `fonts` pins the font files (empty: the
    /// system's fonts).
    BurnedIn {
        track: TrackId,
        style: SubtitleStyle,
        fonts: Vec<PathBuf>,
    },
}

impl SubtitleExport {
    pub fn track(&self) -> Option<TrackId> {
        match self {
            Self::None => None,
            Self::Sidecar { track, .. }
            | Self::Embedded { track, .. }
            | Self::BurnedIn { track, .. } => Some(*track),
        }
    }
}

/// One export: a sequence of a project snapshot, encoded with `settings` into `output`.
///
/// `project` is an immutable root (cloning it is O(1)), so edits made while the job
/// waits or runs do not reach it.
#[derive(Clone, Debug)]
pub struct ExportJob {
    pub project: Project,
    pub sequence: SequenceId,
    pub settings: ExportSettings,
    /// The preset the settings came from: its loudness target and platform limits are
    /// checked after the export.
    pub preset: Option<&'static Preset>,
    /// Timeline frames to export, end exclusive; `None` for the whole sequence.
    pub range: Option<Range<i64>>,
    /// The final path (for an image sequence, a printf pattern or a plain name, as
    /// [`ExportSession::create`](media::export::ExportSession::create) takes). An
    /// existing file there is replaced when the export succeeds.
    pub output: PathBuf,
    pub subtitles: SubtitleExport,
}

impl ExportJob {
    pub fn new(
        project: Project,
        sequence: SequenceId,
        settings: ExportSettings,
        output: impl Into<PathBuf>,
    ) -> Self {
        Self {
            project,
            sequence,
            settings,
            preset: None,
            range: None,
            output: output.into(),
            subtitles: SubtitleExport::None,
        }
    }

    /// A job with `preset` resolved against the sequence's format (size, frame rate,
    /// channels). The settings' encoder preference stays `Auto`; change any field before
    /// submitting.
    pub fn from_preset(
        project: Project,
        sequence: SequenceId,
        preset: &'static Preset,
        output: impl Into<PathBuf>,
    ) -> Result<Self, ExportFailure> {
        let seq = project
            .sequence(sequence)
            .ok_or_else(|| ExportFailure::new("export.error.sequence"))?;
        let settings = preset
            .resolve(&timeline_format(seq))
            .map_err(|e| ExportFailure::from_export(&e, true))?;
        let mut job = Self::new(project, sequence, settings, output);
        job.preset = Some(preset);
        Ok(job)
    }

    pub fn with_range(mut self, range: Range<i64>) -> Self {
        self.range = Some(range);
        self
    }

    pub fn with_subtitles(mut self, subtitles: SubtitleExport) -> Self {
        self.subtitles = subtitles;
        self
    }

    /// The output's file name, for notifications.
    pub fn display_name(&self) -> String {
        self.output
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.output.to_string_lossy().into_owned())
    }
}

/// The format presets resolve against: the sequence's canvas, frame rate and channels.
pub fn timeline_format(sequence: &Sequence) -> TimelineFormat {
    TimelineFormat {
        width: sequence.resolution.width,
        height: sequence.resolution.height,
        frame_rate: media::Rational::new(sequence.frame_rate.num, sequence.frame_rate.den),
        audio_channels: sequence.channel_layout.channels(),
    }
}

/// The colour tags that deliver a project's colour space as it is (for settings that
/// follow the project rather than a preset).
pub fn colour_tags(space: ColourSpace) -> ColourTags {
    match space {
        ColourSpace::Rec709 => ColourTags::Bt709,
        ColourSpace::Rec2020Pq => ColourTags::Bt2020Pq,
        ColourSpace::Rec2020Hlg => ColourTags::Bt2020Hlg,
    }
}

/// The output node's primaries and transfer for a stream tagged `tags`: the values the
/// stream says it holds (Decision 21).
pub fn output_encoding(tags: ColourTags) -> (Primaries, OutputTransfer) {
    match tags {
        ColourTags::Bt709 => (Primaries::Bt709, OutputTransfer::Bt1886),
        ColourTags::Bt2020Pq => (Primaries::Bt2020, OutputTransfer::Pq),
        ColourTags::Bt2020Hlg => (Primaries::Bt2020, OutputTransfer::Hlg),
        ColourTags::Srgb => (Primaries::Bt709, OutputTransfer::Srgb),
    }
}

/// Where a sidecar of `format` goes: next to `output`, with its name and the format's
/// extension.
pub fn sidecar_path(output: &Path, format: SidecarFormat) -> PathBuf {
    output.with_extension(format.extension())
}
