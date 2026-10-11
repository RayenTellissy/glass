//! Background export: a queue of jobs, each a sequence of a project snapshot rendered
//! through the timeline renderer at full quality, read back and encoded into a file.
//!
//! ```text
//!   ExportQueue::submit(ExportJob) ── glass-export (one job at a time, FIFO):
//!     TimelinePlan ── Lookahead (glass-sched, glass-decode-<n>) ── LookaheadFrames
//!       ──▶ glass-export-render: TimelineRenderer::render_output (full quality, never the
//!           governor; burned-in subtitle over the composite) ──▶ ReadbackRing (lossless,
//!           several frames in flight)
//!       ──▶ glass-export: ExportSession (push_picture, silence, embedded cues)
//!       ──▶ finish ──▶ verify ──▶ sidecar ──▶ rename to the final path
//!   events (Queued, Started, Progress, Paused, Resumed, Finished, Cancelled, Failed) ──▶ UI
//! ```
//!
//! **Jobs.** An [`ExportJob`] holds an immutable project root, the sequence, the
//! [`ExportSettings`](media::export::ExportSettings) (usually from a preset, which also
//! sets the loudness target and limits the result is checked against), a range of
//! timeline frames, the output path and what to do with a subtitle track
//! ([`SubtitleExport`]). Jobs run one at a time in submission order.
//!
//! **Pipeline.** The look-ahead decodes the range; `glass-export-render` renders each
//! output frame with [`TimelineRenderer::render_output`](crate::timeline::TimelineRenderer)
//! at the settings' size (the canvas fitted inside), encoded by the output node for the
//! settings' colour tags into an 8-bit, 10-bit or 16-bit texture matching the encoder's
//! depth, and records a copy into a lossless [`ReadbackRing`](render::readback); finished
//! frames go to `glass-export` through a short bounded queue. So the GPU renders and
//! copies frames ahead while the encoder converts and encodes earlier ones, and neither
//! thread waits on the UI. A frame rate other than the timeline's shows the timeline frame
//! at each output frame's start.
//!
//! **Audio.** The engine has no sequence audio mix yet: an audio stream gets silence of
//! the right length, and its loudness target is not checked.
//!
//! **Pause, cancel, errors.** Pause stops both threads at the next frame boundary (the
//! encoder and the file stay open, the look-ahead fills its lanes and parks); resume
//! continues with the next frame. Cancel stops them, drops the session and deletes every
//! temporary file. The session writes a hidden temporary file next to the output (an
//! image sequence a hidden folder); the finished, verified file is renamed onto the
//! output path, so the final path never holds a partial file. Failures are i18n keys
//! under `export.error.` ([`ExportFailure`]); finished and failed jobs carry a
//! [`Notification`] the app can show.

mod events;
mod job;
mod queue;
mod run;

pub use events::{
    EventKind, ExportEvent, ExportFailure, Finished, JobId, MESSAGE_KEYS, Notification,
    NotificationKind, Progress, Stage,
};
pub use job::{
    ExportJob, SidecarFormat, SubtitleExport, colour_tags, output_encoding, sidecar_path,
    timeline_format,
};
pub use queue::{ExportQueue, ExportQueueConfig};
