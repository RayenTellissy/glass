//! What an [`ExportQueue`](super::ExportQueue) tells the app: progress, pauses, the
//! outcome of each job, and notifications the app may show as OS notifications. Every
//! message is an i18n key with arguments; nothing here is English text.

use std::path::PathBuf;
use std::time::Duration;

use media::export::verify::VerifyReport;
use media::export::{EncoderFallback, ExportError};

/// A job's number in its queue, in submission order from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u64);

/// One message from the queue, about one job.
#[derive(Debug)]
pub struct ExportEvent {
    pub job: JobId,
    pub kind: EventKind,
}

#[derive(Debug)]
pub enum EventKind {
    /// Accepted; `ahead` jobs run before it (the running one included).
    Queued {
        ahead: usize,
    },
    /// The job's file is open and its frames start.
    Started {
        total_frames: u64,
    },
    /// At most every [`ExportQueueConfig::progress_interval`](super::ExportQueueConfig),
    /// at every stage change and after the last frame.
    Progress(Progress),
    /// The pipeline stopped at a frame boundary; the encoder stays open.
    Paused {
        frames_done: u64,
    },
    Resumed,
    /// The file is at its final path.
    Finished(Box<Finished>),
    /// Stopped; nothing is left at the final path and no temporary file remains.
    Cancelled,
    /// Stopped by an error; nothing is left at the final path and no temporary file
    /// remains.
    Failed {
        failure: ExportFailure,
        notification: Notification,
    },
}

/// Where a running job is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Building the plan, opening the encoder.
    Preparing,
    /// Frames are being rendered, read back and encoded.
    Rendering,
    /// Draining the encoders and closing the file.
    Finishing,
    /// Reading the file back ([`media::export::verify`]).
    Verifying,
}

impl Stage {
    pub fn key(self) -> &'static str {
        match self {
            Self::Preparing => "export.stage.preparing",
            Self::Rendering => "export.stage.rendering",
            Self::Finishing => "export.stage.finishing",
            Self::Verifying => "export.stage.verifying",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub stage: Stage,
    /// Frames encoded so far; never decreases.
    pub frames_done: u64,
    pub total_frames: u64,
    /// Frames encoded per second of running time (paused time left out).
    pub fps: f64,
    /// Running time left at the current rate; `None` until a frame is done.
    pub eta: Option<Duration>,
    /// Running time so far, paused time left out.
    pub elapsed: Duration,
}

impl Progress {
    /// Fraction of the frames done, 0 to 1.
    pub fn fraction(&self) -> f64 {
        if self.total_frames == 0 {
            return 0.0;
        }
        self.frames_done as f64 / self.total_frames as f64
    }
}

/// A finished job.
#[derive(Debug)]
pub struct Finished {
    /// The file (for an image sequence, the first frame's file).
    pub path: PathBuf,
    /// Every file written: the output (each frame of an image sequence) and the sidecar.
    pub files: Vec<PathBuf>,
    pub sidecar: Option<PathBuf>,
    pub frames: u64,
    /// Running time, paused time left out, verification included.
    pub elapsed: Duration,
    /// Frames per second of running time from the start to the closed file.
    pub fps: f64,
    /// `None` for image sequences, which are not verified.
    pub report: Option<VerifyReport>,
    pub video_encoder: Option<String>,
    pub fallbacks: Vec<EncoderFallback>,
    /// Clips whose media was offline when the export started; they render empty.
    pub offline_media: usize,
    /// The audio stream holds silence: no audio clip plays in the exported range.
    pub silent_audio: bool,
    pub notification: Notification,
}

/// Why a job stopped: an i18n key under `export.error.` and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportFailure {
    pub key: &'static str,
    pub args: Vec<(&'static str, String)>,
}

impl ExportFailure {
    pub fn new(key: &'static str) -> Self {
        Self {
            key,
            args: Vec::new(),
        }
    }

    pub fn with(mut self, name: &'static str, value: impl ToString) -> Self {
        self.args.push((name, value.to_string()));
        self
    }

    pub fn arg(&self, name: &str) -> Option<&str> {
        self.args
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    /// An encoder or muxer error while creating the file (`opening`) or writing it.
    pub(crate) fn from_export(error: &ExportError, opening: bool) -> Self {
        match error {
            ExportError::Unavailable { codec, .. } => {
                Self::new("export.error.unavailable").with("codec", codec)
            }
            ExportError::Incompatible { container, codec } => {
                Self::new("export.error.incompatible")
                    .with("container", container)
                    .with("codec", codec)
            }
            ExportError::Settings(detail) => {
                Self::new("export.error.settings").with("detail", detail)
            }
            other if opening => Self::new("export.error.open").with("detail", other),
            other => Self::new("export.error.encode").with("detail", other),
        }
    }
}

/// What a notification says about a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NotificationKind {
    Finished,
    /// Finished, and verification found warnings or failures.
    FinishedWithFindings,
    Failed,
}

/// An event the app can turn into an OS notification: a title and a body as i18n keys,
/// the body's arguments (`name`, the output's file name; `count`, findings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub kind: NotificationKind,
    pub title_key: &'static str,
    pub body_key: &'static str,
    pub args: Vec<(&'static str, String)>,
}

impl Notification {
    pub(crate) fn finished(name: &str, findings: usize) -> Self {
        if findings == 0 {
            Self {
                kind: NotificationKind::Finished,
                title_key: "export.notify.finished.title",
                body_key: "export.notify.finished.body",
                args: vec![("name", name.to_owned())],
            }
        } else {
            Self {
                kind: NotificationKind::FinishedWithFindings,
                title_key: "export.notify.findings.title",
                body_key: "export.notify.findings.body",
                args: vec![("name", name.to_owned()), ("count", findings.to_string())],
            }
        }
    }

    pub(crate) fn failed(name: &str) -> Self {
        Self {
            kind: NotificationKind::Failed,
            title_key: "export.notify.failed.title",
            body_key: "export.notify.failed.body",
            args: vec![("name", name.to_owned())],
        }
    }
}

/// Every message key the export queue's events carry, for the i18n check.
pub const MESSAGE_KEYS: &[&str] = &[
    "export.stage.preparing",
    "export.stage.rendering",
    "export.stage.finishing",
    "export.stage.verifying",
    "export.error.sequence",
    "export.error.plan",
    "export.error.range",
    "export.error.subtitle_track",
    "export.error.subtitle_container",
    "export.error.unavailable",
    "export.error.incompatible",
    "export.error.settings",
    "export.error.open",
    "export.error.encode",
    "export.error.render",
    "export.error.write",
    "export.error.rename",
    "export.error.thread",
    "export.notify.finished.title",
    "export.notify.finished.body",
    "export.notify.findings.title",
    "export.notify.findings.body",
    "export.notify.failed.title",
    "export.notify.failed.body",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_keys_are_english_strings() {
        let english = assets::i18n::Locale::english();
        for key in MESSAGE_KEYS {
            assert!(english.get(key).is_some(), "{key} missing from en.json");
        }
    }

    #[test]
    fn arguments_match_the_placeholders() {
        let english = assets::i18n::Locale::english();
        let check = |key: &str, args: &[(&'static str, String)]| {
            let text = english.get(key).expect("key");
            let mut names = assets::i18n::placeholders(text).expect("placeholders");
            names.sort_unstable();
            let mut given: Vec<&str> = args.iter().map(|(n, _)| *n).collect();
            given.sort_unstable();
            assert_eq!(names, given, "{key}");
        };
        for n in [
            Notification::finished("a.mp4", 0),
            Notification::finished("a.mp4", 2),
            Notification::failed("a.mp4"),
        ] {
            check(n.title_key, &[]);
            check(n.body_key, &n.args);
        }
        let failures = [
            ExportFailure::from_export(
                &ExportError::Unavailable {
                    codec: "AV1",
                    tried: Vec::new(),
                },
                true,
            ),
            ExportFailure::from_export(
                &ExportError::Incompatible {
                    container: "WAV",
                    codec: "H.264",
                },
                true,
            ),
            ExportFailure::from_export(&ExportError::Settings("odd width".into()), true),
            ExportFailure::from_export(&ExportError::Input("short".into()), true),
            ExportFailure::from_export(&ExportError::Input("short".into()), false),
        ];
        for f in &failures {
            check(f.key, &f.args);
        }
    }
}
