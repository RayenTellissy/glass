//! The analysis kinds: each is a type implementing [`AnalysisJob`].

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
