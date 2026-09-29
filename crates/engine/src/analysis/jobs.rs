//! The analysis kinds: each is a type implementing [`AnalysisJob`].

use media::analysis::peaks::{self, PEAKS_VERSION, PeakSettings, Peaks};
use media::analysis::scenes::{self, SCENES_VERSION, SceneCuts, SceneSettings};
use media::analysis::thumbnails::{self, THUMBNAILS_VERSION, ThumbnailSettings, ThumbnailStrips};
use media::analysis::{AnalysisError, Control, Stopped};
use media::timemap::{TIMELINE_CACHE_KIND, TIMELINE_CACHE_VERSION, timeline_cache_params};
use media::{CacheEntry, ContentHash, SourceFile, SourceTimeline, StreamSelect};

use super::pool::{AnalysisJob, Artifact, JobContext, JobError};

/// A JSON main file.
fn json_encode<T: serde::Serialize>(value: &T) -> std::io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(std::io::Error::other)
}

/// The frame times of a video stream ([`SourceTimeline::scan`]). Shares its cache file
/// with [`media::TimelineCache`] and [`SourceTimeline::load_or_scan`].
#[derive(Clone, Debug)]
pub struct TimelineJob {
    pub source: SourceFile,
    pub stream: StreamSelect,
}

impl Artifact for SourceTimeline {
    const EXTENSION: &'static str = "json";

    fn encode(&self) -> std::io::Result<Vec<u8>> {
        json_encode(self)
    }

    fn decode(bytes: &[u8], _: &CacheEntry) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

impl AnalysisJob for TimelineJob {
    type Output = SourceTimeline;
    const KIND: &'static str = TIMELINE_CACHE_KIND;
    const VERSION: u32 = TIMELINE_CACHE_VERSION;

    fn source_hash(&self) -> ContentHash {
        self.source.hash
    }

    fn params(&self) -> String {
        timeline_cache_params(self.stream)
    }

    fn run(&self, ctx: &JobContext) -> Result<SourceTimeline, JobError> {
        ctx.checkpoint()?;
        Ok(SourceTimeline::scan(&self.source.path, self.stream)?.0)
    }
}

impl Control for JobContext {
    fn checkpoint(&self) -> Result<(), Stopped> {
        JobContext::checkpoint(self).map_err(|_| Stopped)
    }

    fn progress(&self, fraction: f32) {
        JobContext::progress(self, fraction);
    }

    fn partial(&self, n: u32) {
        JobContext::partial(self, n);
    }
}

impl From<AnalysisError> for JobError {
    fn from(error: AnalysisError) -> Self {
        match error {
            AnalysisError::Stopped => Self::Cancelled,
            other => Self::Failed(other.to_string()),
        }
    }
}

/// Thumbnail strips at three zoom levels ([`media::analysis::thumbnails`]). Levels land
/// coarse first; each is announced with `JobEventKind::Partial(level)` and can be read
/// with [`ThumbnailStrips::load_levels`] before the job finishes.
#[derive(Clone, Debug)]
pub struct ThumbnailJob {
    pub source: SourceFile,
    pub settings: ThumbnailSettings,
}

impl Artifact for ThumbnailStrips {
    const EXTENSION: &'static str = "json";

    fn encode(&self) -> std::io::Result<Vec<u8>> {
        json_encode(self)
    }

    /// Valid only while every atlas it names is still on disk.
    fn decode(bytes: &[u8], entry: &CacheEntry) -> Option<Self> {
        let strips: ThumbnailStrips = serde_json::from_slice(bytes).ok()?;
        let complete = strips
            .levels
            .iter()
            .flat_map(|l| &l.atlases)
            .all(|a| entry.dir().join(a).is_file());
        complete.then_some(strips)
    }
}

impl AnalysisJob for ThumbnailJob {
    type Output = ThumbnailStrips;
    const KIND: &'static str = "thumbs";
    const VERSION: u32 = THUMBNAILS_VERSION;

    fn source_hash(&self) -> ContentHash {
        self.source.hash
    }

    fn params(&self) -> String {
        self.settings.params()
    }

    fn run(&self, ctx: &JobContext) -> Result<ThumbnailStrips, JobError> {
        Ok(thumbnails::generate(
            &self.source.path,
            &self.settings,
            ctx.entry(),
            Some(ctx.cache()),
            ctx,
        )?)
    }
}

/// Waveform peaks at five zoom levels ([`media::analysis::peaks`]), one binary file.
#[derive(Clone, Debug)]
pub struct PeaksJob {
    pub source: SourceFile,
    pub settings: PeakSettings,
}

impl Artifact for Peaks {
    const EXTENSION: &'static str = "bin";

    fn encode(&self) -> std::io::Result<Vec<u8>> {
        Ok(self.to_bytes())
    }

    fn decode(bytes: &[u8], _: &CacheEntry) -> Option<Self> {
        Peaks::from_bytes(bytes)
    }
}

impl AnalysisJob for PeaksJob {
    type Output = Peaks;
    const KIND: &'static str = "peaks";
    const VERSION: u32 = PEAKS_VERSION;

    fn source_hash(&self) -> ContentHash {
        self.source.hash
    }

    fn params(&self) -> String {
        self.settings.params()
    }

    fn run(&self, ctx: &JobContext) -> Result<Peaks, JobError> {
        Ok(peaks::generate(&self.source.path, &self.settings, ctx)?)
    }
}

/// Scene cuts ([`media::analysis::scenes`]), one JSON file.
#[derive(Clone, Debug)]
pub struct SceneJob {
    pub source: SourceFile,
    pub settings: SceneSettings,
}

impl Artifact for SceneCuts {
    const EXTENSION: &'static str = "json";

    fn encode(&self) -> std::io::Result<Vec<u8>> {
        json_encode(self)
    }

    fn decode(bytes: &[u8], _: &CacheEntry) -> Option<Self> {
        serde_json::from_slice::<SceneCuts>(bytes)
            .ok()
            .filter(|s| s.version == SCENES_VERSION)
    }
}

impl AnalysisJob for SceneJob {
    type Output = SceneCuts;
    const KIND: &'static str = "scenes";
    const VERSION: u32 = SCENES_VERSION;

    fn source_hash(&self) -> ContentHash {
        self.source.hash
    }

    fn params(&self) -> String {
        self.settings.params()
    }

    fn run(&self, ctx: &JobContext) -> Result<SceneCuts, JobError> {
        Ok(scenes::detect(&self.source.path, &self.settings, ctx)?)
    }
}
