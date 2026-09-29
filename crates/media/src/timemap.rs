//! Frame-rate normalisation: which source frame each frame of a sequence shows.
//!
//! A [`SourceTimeline`] describes when a clip's frames are presented, once per file:
//! closed form for constant-rate streams (start, rate, frame count) and the sorted list
//! of presentation timestamps for variable-rate ones. It is built from the container's
//! index when that lists presentation times for every frame, or from a packet scan (no
//! decoding), and is serialisable for the disk cache. [`TimelineCache`] keeps them in
//! memory per file.
//!
//! A [`TimestampMap`] puts a timeline on a sequence's frame rate. Sequence frame `n` is
//! the instant `n / rate` seconds after the clip's first frame, and it shows the source
//! frame on screen at that instant: the **nearest earlier frame**, the last one whose
//! presentation time is at or before the instant. A source slower than the sequence
//! repeats frames; a faster one drops them. The map covers the clip's whole duration
//! (first frame to the end of the last frame): `len()` is `ceil(duration * rate)`, so
//! every sequence frame whose instant falls inside the clip maps to a frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::demux::Demuxer;
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::MediaKind;
use crate::probe::{StreamDetail, probe};
use crate::reader::StreamSelect;
use crate::scan::scan_stream;
use crate::time::{Rational, Timestamp};

/// When a clip's frames are presented, independent of any sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTimeline {
    pub stream_index: usize,
    pub time_base: Rational,
    pub frames: FrameTimes,
}

/// The frame times of a [`SourceTimeline`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameTimes {
    /// Frame `k` is presented at `start + k / rate` seconds (start in stream ticks).
    Constant {
        start: i64,
        rate: Rational,
        count: u64,
    },
    /// Sorted presentation times in stream ticks, and the end of the last frame.
    Variable { pts: Vec<i64>, end: i64 },
}

/// How [`SourceTimeline::scan`] learned the frame times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineSource {
    /// Constant rate from the probe; no per-frame times needed.
    ClosedForm,
    /// The container's index (every frame listed, no reordering).
    ContainerIndex,
    /// Every packet read, none decoded.
    PacketScan,
}

impl SourceTimeline {
    /// Builds the timeline of a video stream: closed form when `probe` finds a constant
    /// rate, otherwise every presentation time from the container index or a packet scan.
    pub fn scan(path: impl AsRef<Path>, select: StreamSelect) -> Result<(Self, TimelineSource)> {
        let path = path.as_ref();
        let info = probe(path)?;
        let mut demuxer = Demuxer::open(path)?;
        let index = match select {
            StreamSelect::Index(i) => i,
            StreamSelect::BestVideo | StreamSelect::BestAudio => demuxer
                .best_stream(MediaKind::Video)
                .ok_or_else(|| MediaError::NoStream {
                    path: path.to_path_buf(),
                    kind: MediaKind::Video,
                })?,
        };
        let summary = demuxer.stream(index)?.clone();
        let stream = info
            .streams
            .iter()
            .find(|s| s.index == index)
            .ok_or_else(|| MediaError::NoSuchStream {
                path: path.to_path_buf(),
                index,
            })?;
        let StreamDetail::Video(video) = &stream.detail else {
            return Err(MediaError::NotDecodable {
                path: path.to_path_buf(),
                index,
                kind: summary.kind,
            });
        };
        let time_base = summary.time_base;
        if !video.vfr.is_vfr
            && let (Some(rate), Some(start)) = (video.frame_rate, summary.start)
            && rate.is_valid()
        {
            let count = summary.frames.or_else(|| {
                summary.duration.map(|d| {
                    let seconds = d.seconds() * rate.to_f64().unwrap_or(0.0);
                    seconds.round().max(1.0) as u64
                })
            });
            if let Some(count) = count {
                return Ok((
                    Self {
                        stream_index: index,
                        time_base,
                        frames: FrameTimes::Constant {
                            start: start.value,
                            rate: rate.reduced(),
                            count,
                        },
                    },
                    TimelineSource::ClosedForm,
                ));
            }
        }

        let params = demuxer
            .input()
            .stream(index)
            .map(|s| ffi::stream_params(&s));
        let entries = demuxer
            .input()
            .stream(index)
            .map(|s| ffi::index_entries(&s))
            .unwrap_or_default();
        let in_order = params.is_some_and(|p| p.video_delay == 0);
        // The end of the last frame: the container's stream duration when it has one.
        // Packet durations are decode-time deltas in MP4, which are not the last
        // presented frame's duration once frames are reordered.
        let container_end = summary
            .start
            .zip(summary.duration)
            .map(|(s, d)| s.value + d.value);
        let complete = summary
            .frames
            .is_some_and(|n| entries.len() as u64 == n && n > 0);
        if in_order && complete {
            let mut pts: Vec<i64> = entries.iter().map(|e| e.timestamp).collect();
            pts.sort_unstable();
            let end = container_end
                .filter(|end| pts.last().is_some_and(|last| end > last))
                .unwrap_or_else(|| guess_end(&pts));
            return Ok((
                Self {
                    stream_index: index,
                    time_base,
                    frames: FrameTimes::Variable { pts, end },
                },
                TimelineSource::ContainerIndex,
            ));
        }

        let packets = scan_stream(&mut demuxer, index)?;
        Ok((
            Self::from_packets(index, time_base, &packets, container_end),
            TimelineSource::PacketScan,
        ))
    }

    /// A variable-rate timeline from scanned packets: their presentation times (decode
    /// times where a packet has none), sorted, and the end of the last one: `end` when
    /// it is after the last time (the container's stream end), else the last packet's
    /// duration or the median interval.
    pub fn from_packets(
        stream_index: usize,
        time_base: Rational,
        packets: &[crate::scan::PacketEntry],
        end: Option<i64>,
    ) -> Self {
        let mut timed: Vec<(i64, i64)> = packets
            .iter()
            .filter_map(|p| p.pts.or(p.dts).map(|t| (t, p.duration)))
            .collect();
        timed.sort_unstable();
        timed.dedup_by_key(|(t, _)| *t);
        let pts: Vec<i64> = timed.iter().map(|(t, _)| *t).collect();
        let end = match (timed.last(), end) {
            (Some((last, _)), Some(end)) if end > *last => end,
            (Some((last, duration)), _) if *duration > 0 => last + duration,
            _ => guess_end(&pts),
        };
        Self {
            stream_index,
            time_base,
            frames: FrameTimes::Variable { pts, end },
        }
    }

    /// Number of source frames.
    pub fn frame_count(&self) -> u64 {
        match &self.frames {
            FrameTimes::Constant { count, .. } => *count,
            FrameTimes::Variable { pts, .. } => pts.len() as u64,
        }
    }

    /// Presentation time of the first frame, in stream ticks.
    pub fn start(&self) -> i64 {
        match &self.frames {
            FrameTimes::Constant { start, .. } => *start,
            FrameTimes::Variable { pts, .. } => pts.first().copied().unwrap_or(0),
        }
    }

    /// End of the last frame, in stream ticks.
    pub fn end(&self) -> i64 {
        match &self.frames {
            FrameTimes::Constant { start, rate, count } => {
                start + ticks_for_frames(*count, *rate, self.time_base)
            }
            FrameTimes::Variable { end, .. } => *end,
        }
    }

    pub fn duration(&self) -> Timestamp {
        Timestamp::new(self.end() - self.start(), self.time_base)
    }

    /// Presentation time of source frame `k`.
    pub fn frame_pts(&self, k: u64) -> Option<Timestamp> {
        if k >= self.frame_count() {
            return None;
        }
        let value = match &self.frames {
            FrameTimes::Constant { start, rate, .. } => {
                start + ticks_for_frames(k, *rate, self.time_base)
            }
            FrameTimes::Variable { pts, .. } => pts[k as usize],
        };
        Some(Timestamp::new(value, self.time_base))
    }

    /// The source frame on screen at `at` (the last one presented at or before it), or
    /// `None` before the first frame or from the end of the last one.
    pub fn frame_at(&self, at: Timestamp) -> Option<u64> {
        let t = at.rescale_floor(self.time_base).value;
        if t < self.start() || t >= self.end() {
            return None;
        }
        let k = match &self.frames {
            FrameTimes::Constant { start, rate, count } => {
                // floor((t - start) * rate * tb), exactly.
                let num =
                    i128::from(t - start) * i128::from(rate.num) * i128::from(self.time_base.num);
                let den = i128::from(rate.den) * i128::from(self.time_base.den);
                ((num.div_euclid(den)) as u64).min(count.saturating_sub(1))
            }
            FrameTimes::Variable { pts, .. } => (pts.partition_point(|p| *p <= t) as u64) - 1,
        };
        Some(k)
    }
}

/// Stream ticks of `frames` frames at `rate`, rounded to the nearest tick (as muxers
/// write constant-rate timestamps).
fn ticks_for_frames(frames: u64, rate: Rational, time_base: Rational) -> i64 {
    // frames / rate seconds = frames * rate.den / rate.num; in ticks divide by tb.
    let num = i128::from(frames) * i128::from(rate.den) * i128::from(time_base.den);
    let den = i128::from(rate.num) * i128::from(time_base.num);
    if den == 0 {
        return 0;
    }
    ((num + den / 2) / den) as i64
}

/// The end of the last frame when no duration is known: the last time plus the median
/// interval.
fn guess_end(pts: &[i64]) -> i64 {
    let Some(last) = pts.last() else {
        return 0;
    };
    let mut intervals: Vec<i64> = pts.windows(2).map(|w| w[1] - w[0]).collect();
    intervals.sort_unstable();
    last + intervals.get(intervals.len() / 2).copied().unwrap_or(1)
}

/// A clip's frames on a sequence's frame rate (see the module docs for the rule).
#[derive(Clone, Debug)]
pub struct TimestampMap {
    source: Arc<SourceTimeline>,
    rate: Rational,
    len: u64,
}

impl TimestampMap {
    /// The map of `source` on a sequence running at `sequence_rate` frames per second.
    /// An invalid rate gives an empty map.
    pub fn new(source: Arc<SourceTimeline>, sequence_rate: Rational) -> Self {
        let rate = sequence_rate.reduced();
        let len = if rate.is_valid() && rate.num > 0 {
            // ceil(duration * rate), duration = ticks * tb.
            let ticks = i128::from((source.end() - source.start()).max(0));
            let num = ticks * i128::from(source.time_base.num) * i128::from(rate.num);
            let den = i128::from(source.time_base.den) * i128::from(rate.den);
            if den > 0 {
                ((num + den - 1) / den) as u64
            } else {
                0
            }
        } else {
            0
        };
        Self { source, rate, len }
    }

    pub fn source(&self) -> &SourceTimeline {
        &self.source
    }

    pub fn sequence_rate(&self) -> Rational {
        self.rate
    }

    /// Sequence frames covering the clip.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The instant of sequence frame `n` as a timestamp in the source's time base,
    /// rounded down to a tick.
    pub fn instant(&self, n: u64) -> Timestamp {
        let tb = self.source.time_base;
        // n / rate seconds from the start, in ticks, rounded down.
        let num = i128::from(n) * i128::from(self.rate.den) * i128::from(tb.den);
        let den = i128::from(self.rate.num) * i128::from(tb.num);
        let offset = if den > 0 { (num / den) as i64 } else { 0 };
        Timestamp::new(self.source.start() + offset, tb)
    }

    /// The source frame sequence frame `n` shows, or `None` past the end.
    pub fn source_frame(&self, n: u64) -> Option<u64> {
        if n >= self.len {
            return None;
        }
        match &self.source.frames {
            FrameTimes::Constant { rate, count, .. } => {
                // floor(n / seq_rate * src_rate), exactly.
                let num = i128::from(n) * i128::from(self.rate.den) * i128::from(rate.num);
                let den = i128::from(self.rate.num) * i128::from(rate.den);
                Some(((num / den) as u64).min(count.saturating_sub(1)))
            }
            // The rounded-down instant keeps the comparison exact: a frame at or before
            // the true instant is also at or before its floor, since times are whole ticks.
            FrameTimes::Variable { .. } => self.source.frame_at(self.instant(n)),
        }
    }

    /// The presentation time of the source frame sequence frame `n` shows.
    pub fn source_pts(&self, n: u64) -> Option<Timestamp> {
        self.source_frame(n).and_then(|k| self.source.frame_pts(k))
    }

    /// The sequence frame whose interval `[n / rate, (n + 1) / rate)` contains `at`, or
    /// `None` outside the clip.
    pub fn sequence_frame(&self, at: Timestamp) -> Option<u64> {
        let tb = self.source.time_base;
        let t = at.rescale_floor(tb).value;
        if t < self.source.start() || t >= self.source.end() {
            return None;
        }
        let num =
            i128::from(t - self.source.start()) * i128::from(tb.num) * i128::from(self.rate.num);
        let den = i128::from(tb.den) * i128::from(self.rate.den);
        let n = (num / den) as u64;
        (n < self.len).then_some(n)
    }

    /// The first sequence frame that shows source frame `k`, or `None` when the sequence
    /// drops it.
    pub fn first_sequence_frame_of(&self, k: u64) -> Option<u64> {
        let pts = self.source.frame_pts(k)?;
        // The first instant at or after the frame's presentation time.
        let tb = self.source.time_base;
        let num = i128::from(pts.value - self.source.start())
            * i128::from(tb.num)
            * i128::from(self.rate.num);
        let den = i128::from(tb.den) * i128::from(self.rate.den);
        let n = ((num + den - 1) / den) as u64;
        (self.source_frame(n) == Some(k)).then_some(n)
    }
}

/// What identifies a file for [`TimelineCache`]: its path, size and modification time.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FileKey {
    path: PathBuf,
    stream: StreamKey,
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum StreamKey {
    Best,
    Index(usize),
}

fn file_key(path: &Path, select: StreamSelect) -> Result<FileKey> {
    let meta = std::fs::metadata(path).map_err(|e| MediaError::Open {
        path: path.to_path_buf(),
        source: crate::error::FfmpegError {
            code: -e.raw_os_error().unwrap_or(2),
            message: e.to_string(),
        },
    })?;
    Ok(FileKey {
        path: path.to_path_buf(),
        stream: match select {
            StreamSelect::Index(i) => StreamKey::Index(i),
            _ => StreamKey::Best,
        },
        size: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// Source timelines per file, in memory. A file whose size or modification time changed
/// is scanned again. Shared between threads.
#[derive(Debug, Default)]
pub struct TimelineCache {
    entries: Mutex<HashMap<FileKey, Arc<SourceTimeline>>>,
}

impl TimelineCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached timeline of a file's video stream, scanning it on the first request.
    pub fn get_or_scan(
        &self,
        path: impl AsRef<Path>,
        select: StreamSelect,
    ) -> Result<Arc<SourceTimeline>> {
        let path = path.as_ref();
        let key = file_key(path, select)?;
        if let Some(hit) = self.lock().get(&key) {
            return Ok(hit.clone());
        }
        let (timeline, _) = SourceTimeline::scan(path, select)?;
        let timeline = Arc::new(timeline);
        self.lock().insert(key, timeline.clone());
        Ok(timeline)
    }

    /// Adds a timeline loaded elsewhere (the disk cache) for the file as it is now.
    pub fn insert(
        &self,
        path: impl AsRef<Path>,
        select: StreamSelect,
        timeline: Arc<SourceTimeline>,
    ) -> Result<()> {
        let key = file_key(path.as_ref(), select)?;
        self.lock().insert(key, timeline);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<FileKey, Arc<SourceTimeline>>> {
        // A panic while holding the lock cannot leave the map half-updated (inserts are
        // single calls), so a poisoned lock is still usable.
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfr(rate: Rational, count: u64) -> Arc<SourceTimeline> {
        Arc::new(SourceTimeline {
            stream_index: 0,
            time_base: Rational::new(1, 90_000),
            frames: FrameTimes::Constant {
                start: 0,
                rate,
                count,
            },
        })
    }

    fn map(source: &Arc<SourceTimeline>, rate: Rational) -> Vec<u64> {
        let m = TimestampMap::new(source.clone(), rate);
        (0..m.len())
            .map(|n| m.source_frame(n).expect("inside"))
            .collect()
    }

    #[test]
    fn same_rate_is_identity() {
        let s = cfr(Rational::new(30, 1), 90);
        assert_eq!(map(&s, Rational::new(30, 1)), (0..90).collect::<Vec<_>>());
    }

    #[test]
    fn thirty_in_twenty_four_drops_one_in_five() {
        let got = map(&cfr(Rational::new(30, 1), 90), Rational::new(24, 1));
        assert_eq!(got.len(), 72);
        assert_eq!(&got[..10], &[0, 1, 2, 3, 5, 6, 7, 8, 10, 11]);
        assert_eq!(got[71], 88);
    }

    #[test]
    fn sixty_in_thirty_takes_every_other() {
        let got = map(&cfr(Rational::new(60, 1), 180), Rational::new(30, 1));
        assert_eq!(got, (0..90).map(|n| n * 2).collect::<Vec<_>>());
    }

    #[test]
    fn ntsc_film_in_pal_repeats_one_in_twenty_four() {
        let got = map(&cfr(Rational::new(24000, 1001), 48), Rational::new(25, 1));
        assert_eq!(got.len(), 51);
        // Instants n/25 against frames at k*1001/24000: frame 0 twice, then one each
        // until the next repeat.
        assert_eq!(&got[..4], &[0, 0, 1, 2]);
        assert_eq!(got[25], 23);
        assert_eq!(got[50], 47);
    }

    #[test]
    fn inverse_lookups() {
        let s = cfr(Rational::new(30, 1), 90);
        let m = TimestampMap::new(s.clone(), Rational::new(24, 1));
        // Frame 4 (at 4/30 s) is dropped in 24; frame 5 first shows at n = 4.
        assert_eq!(m.first_sequence_frame_of(4), None);
        assert_eq!(m.first_sequence_frame_of(5), Some(4));
        let at = s.frame_pts(5).expect("frame 5");
        assert_eq!(m.sequence_frame(at), Some(4));
        assert_eq!(m.sequence_frame(Timestamp::new(-1, s.time_base)), None);
        assert_eq!(m.source_frame(m.len()), None);
    }

    #[test]
    fn variable_rate_uses_the_nearest_earlier_frame() {
        let tb = Rational::new(1, 1000);
        let s = Arc::new(SourceTimeline {
            stream_index: 0,
            time_base: tb,
            frames: FrameTimes::Variable {
                pts: vec![0, 100, 400, 500],
                end: 600,
            },
        });
        // 10 fps: instants 0, 100, ..., 500.
        let got = map(&s, Rational::new(10, 1));
        assert_eq!(got, vec![0, 1, 1, 1, 2, 3]);
        assert_eq!(s.frame_at(Timestamp::new(399, tb)), Some(1));
        assert_eq!(s.frame_at(Timestamp::new(600, tb)), None);
    }

    #[test]
    fn serde_round_trip() {
        let s = cfr(Rational::new(24000, 1001), 48);
        let json = serde_json::to_string(&*s).expect("serialise");
        let back: SourceTimeline = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, *s);
    }
}
