//! GOP index: where each keyframe of a video stream is, so an accurate seek can start
//! decoding at the right one. Built from the container's index when that gives
//! presentation times for keyframes (every frame listed and no reordering), otherwise
//! from a packet scan (no decoding), and cached per file on disk under the project cache
//! folder, keyed by [`ContentHash`].

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cache::{CacheDir, ContentHash};
use crate::demux::Demuxer;
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::MediaKind;
use crate::reader::StreamSelect;
use crate::scan::scan_stream;
use crate::time::{Rational, Timestamp};

/// Bumped when the index format or how it is built changes; older files are rebuilt.
const GOP_INDEX_VERSION: u32 = 1;

/// A keyframe: a packet decoding can start from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Presentation time, stream ticks.
    pub pts: i64,
    /// Decode time, stream ticks, when known.
    pub dts: Option<i64>,
    /// Byte offset in the file, when known.
    pub pos: Option<u64>,
}

/// How a [`GopIndex`] was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GopSource {
    ContainerIndex,
    PacketScan,
    /// Loaded from the project cache folder.
    Disk,
}

/// Keyframes of one video stream, sorted by presentation time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GopIndex {
    pub version: u32,
    pub stream_index: usize,
    pub time_base: Rational,
    pub keyframes: Vec<Keyframe>,
    /// Packets (frames) in the stream.
    pub frames: u64,
}

impl GopIndex {
    /// Builds the index of a file's video stream.
    pub fn build(path: impl AsRef<Path>, select: StreamSelect) -> Result<(Self, GopSource)> {
        let path = path.as_ref();
        let mut demuxer = Demuxer::open(path)?;
        let index = video_stream(&demuxer, select)?;
        let summary = demuxer.stream(index)?.clone();
        let (entries, in_order) = match demuxer.input().stream(index) {
            Some(s) => (
                ffi::index_entries(&s),
                ffi::stream_params(&s).video_delay == 0,
            ),
            None => (Vec::new(), false),
        };
        let complete = summary
            .frames
            .is_some_and(|n| n > 0 && entries.len() as u64 == n);
        let mut gop = if in_order && complete {
            let keyframes = entries
                .iter()
                .filter(|e| e.key)
                .map(|e| Keyframe {
                    pts: e.timestamp,
                    dts: Some(e.timestamp),
                    pos: u64::try_from(e.pos).ok(),
                })
                .collect();
            (
                Self {
                    version: GOP_INDEX_VERSION,
                    stream_index: index,
                    time_base: summary.time_base,
                    keyframes,
                    frames: entries.len() as u64,
                },
                GopSource::ContainerIndex,
            )
        } else {
            let packets = scan_stream(&mut demuxer, index)?;
            let keyframes = packets
                .iter()
                .filter(|p| p.key)
                .filter_map(|p| {
                    p.pts.or(p.dts).map(|pts| Keyframe {
                        pts,
                        dts: p.dts,
                        pos: p.pos,
                    })
                })
                .collect();
            (
                Self {
                    version: GOP_INDEX_VERSION,
                    stream_index: index,
                    time_base: summary.time_base,
                    keyframes,
                    frames: packets.len() as u64,
                },
                GopSource::PacketScan,
            )
        };
        gop.0.keyframes.sort_by_key(|k| k.pts);
        gop.0.keyframes.dedup_by_key(|k| k.pts);
        Ok(gop)
    }

    /// The index from the cache folder, or built and written there. A cache that cannot
    /// be written is logged and skipped; the index is still returned.
    pub fn load_or_build(
        cache: &CacheDir,
        path: impl AsRef<Path>,
        select: StreamSelect,
    ) -> Result<(Self, GopSource)> {
        let path = path.as_ref();
        let io_error = |e: std::io::Error| MediaError::Open {
            path: path.to_path_buf(),
            source: crate::error::FfmpegError {
                code: -e.raw_os_error().unwrap_or(5),
                message: e.to_string(),
            },
        };
        let hash = ContentHash::of_file(path).map_err(io_error)?;
        let name = cache_name(select);
        if let Ok(Some(index)) = cache.read_json::<GopIndex>(&hash, &name)
            && index.version == GOP_INDEX_VERSION
        {
            return Ok((index, GopSource::Disk));
        }
        let (index, source) = Self::build(path, select)?;
        if let Err(error) = cache.write_json(&hash, &name, &index) {
            tracing::warn!(path = %path.display(), %error, "could not write the GOP index to the cache");
        }
        Ok((index, source))
    }

    /// The last keyframe presented at or before `at`, or `None` before the first.
    pub fn keyframe_at_or_before(&self, at: Timestamp) -> Option<usize> {
        let t = at.rescale_floor(self.time_base).value;
        self.keyframes
            .partition_point(|k| k.pts <= t)
            .checked_sub(1)
    }

    pub fn keyframe(&self, i: usize) -> Option<&Keyframe> {
        self.keyframes.get(i)
    }

    pub fn keyframe_pts(&self, i: usize) -> Option<Timestamp> {
        self.keyframes
            .get(i)
            .map(|k| Timestamp::new(k.pts, self.time_base))
    }
}

fn cache_name(select: StreamSelect) -> String {
    match select {
        StreamSelect::Index(i) => format!("gop-{i}.json"),
        _ => "gop-video.json".to_string(),
    }
}

fn video_stream(demuxer: &Demuxer, select: StreamSelect) -> Result<usize> {
    match select {
        StreamSelect::Index(i) => Ok(i),
        _ => demuxer
            .best_stream(MediaKind::Video)
            .ok_or_else(|| MediaError::NoStream {
                path: demuxer.path().to_path_buf(),
                kind: MediaKind::Video,
            }),
    }
}
