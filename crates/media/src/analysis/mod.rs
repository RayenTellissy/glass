//! Analysis algorithms that run on the analysis pool (`engine::analysis`): thumbnail
//! strips, waveform peaks, scene cuts, silences and proxies. They take a [`Control`] for
//! cancellation, pause and progress, and write their files through a
//! [`CacheEntry`](crate::CacheEntry).

pub mod peaks;
pub mod proxy;
pub mod scenes;
pub mod silence;
pub mod thumbnails;

use crate::error::MediaError;

/// The analysis pool's side of a running analysis.
pub trait Control {
    /// Between units of work: `Err(Stopped)` once cancelled; may wait while paused.
    fn checkpoint(&self) -> Result<(), Stopped>;
    /// Fraction done, 0 to 1.
    fn progress(&self, fraction: f32);
    /// Part `n` of the result is on disk (a thumbnail level).
    fn partial(&self, n: u32);
}

/// A [`Control`] that never stops and reports nothing (tests, benches, tools).
#[derive(Clone, Copy, Debug, Default)]
pub struct Unattended;

impl Control for Unattended {
    fn checkpoint(&self) -> Result<(), Stopped> {
        Ok(())
    }

    fn progress(&self, _: f32) {}

    fn partial(&self, _: u32) {}
}

/// The analysis was cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stopped;

/// Why an analysis produced no result.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("cancelled")]
    Stopped,
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error("cache: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Encode(String),
}

impl From<Stopped> for AnalysisError {
    fn from(_: Stopped) -> Self {
        Self::Stopped
    }
}
