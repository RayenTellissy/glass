//! The analysis job system: background work on source files (timelines, thumbnails,
//! waveforms, later proxies, scene cuts, silence, beats, loudness) on a small pool of
//! low-priority `glass-analysis-<n>` threads, with results cached on disk under the
//! project cache folder.
//!
//! - **Jobs** implement [`AnalysisJob`]: a [`JobKey`] (the source's `ContentHash`, the
//!   job kind, a hash of its parameters, its format version), `run(ctx)`, and an output
//!   that implements [`Artifact`] (bytes on disk). Kinds need no registration beyond the
//!   type: the pool erases the type when a job is submitted.
//! - **Priority** ([`Priority`]): visible clips first, then selected, then background;
//!   first come, first served within a priority. [`AnalysisPool::set_priority`] moves a
//!   queued job, for example when a clip scrolls into view.
//! - **Dedup**: submitting a key that is already queued or running returns a handle to
//!   the same job (raising its priority when the new request is higher). Nothing runs
//!   twice.
//! - **Cache**: before running, a worker looks for the result on disk
//!   (`media::CacheDir::entry`: `<cache>/analysis/<hash>/<kind>-<params>-v<version>.<ext>`).
//!   A hit decodes it and does no work. A miss runs the job and writes the main file
//!   last, so its presence means the result is complete; files of other versions of the
//!   same kind are deleted then.
//! - **Cancellation** is cooperative: a job calls [`JobContext::checkpoint`] between
//!   units of work, which returns [`JobError::Cancelled`] once cancelled. Cancelling a
//!   queued job removes it at once.
//! - **Pause**: [`AnalysisPool::pause`] (playback, scrubbing, battery mode) stops workers
//!   from starting jobs and parks running jobs at their next checkpoint until
//!   [`AnalysisPool::resume`].
//! - **Progress** reaches the UI as [`JobEvent`]s on an `std::sync::mpsc` channel (the
//!   lock-free list channel ported from crossbeam) that the UI drains with `try_recv`
//!   once per frame; it never waits. Results are read with [`JobHandle::try_result`],
//!   which never blocks either.
//!
//! The queue itself is a mutex-protected ordered set: submit, cancel and re-prioritise
//! hold it for a few map operations, never across I/O or a job.

mod jobs;
mod pool;

pub use jobs::{PeaksJob, ThumbnailJob, TimelineJob};
pub use pool::{
    AnalysisJob, AnalysisPool, Artifact, JobContext, JobError, JobEvent, JobEventKind, JobHandle,
    JobKey, Outcome, PoolConfig, Priority, default_workers,
};
