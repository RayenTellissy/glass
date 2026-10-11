//! The decode plan: which source frames each timeline frame shows, as segments.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use media::{Rational, Timestamp};
use render::frame_cache::SegmentHash;

/// Identifies a media source across segments, so a decoder that already has the file
/// open can reuse it. Whoever builds the plan picks the ids (a `MediaId` later).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(pub u64);

/// A media source a segment reads from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRef {
    pub id: SourceId,
    pub path: Arc<Path>,
}

impl SourceRef {
    pub fn new(id: SourceId, path: impl AsRef<Path>) -> Self {
        Self {
            id,
            path: Arc::from(path.as_ref()),
        }
    }
}

/// A run of timeline frames that show consecutive frames of one source: timeline frame
/// `t` in `timeline` shows the source frame on screen at
/// `source_start + (t - timeline.start) * frame_duration * speed` (the last frame
/// presented at or before that instant, as `media::timemap` defines it). A cut is one segment ending
/// where the next starts; a transition is two segments that overlap, the incoming one on
/// a higher `layer`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanSegment {
    pub source: SourceRef,
    /// Timeline frames, end exclusive.
    pub timeline: Range<i64>,
    /// The source time shown at `timeline.start`.
    pub source_start: Timestamp,
    /// Source seconds per timeline second: 1/1 plays at normal speed, 2/1 twice as fast.
    pub speed: Rational,
    /// Stacking order of overlapping segments: higher draws on top.
    pub layer: u32,
    /// The frame cache's segment key for the frames this segment renders.
    pub hash: SegmentHash,
}

impl PlanSegment {
    /// The source time one timeline frame of `frame_duration` advances: the frame
    /// duration times the speed, reduced (rounded to a microsecond in the rare case the
    /// exact product does not fit).
    pub fn source_step(&self, frame_duration: Rational) -> Rational {
        let (fn_, fd) = positive(frame_duration);
        let (sn, sd) = positive(self.speed);
        let (num, den) = (fn_ * sn, fd * sd);
        let g = gcd(num.unsigned_abs(), den.unsigned_abs()).max(1) as i128;
        let (num, den) = (num / g, den / g);
        match (i32::try_from(num), i32::try_from(den)) {
            (Ok(num), Ok(den)) => Rational::new(num, den),
            _ => {
                let micros = if den == 0 {
                    0.0
                } else {
                    num as f64 / den as f64 * 1e6
                };
                Rational::new(
                    micros.round().clamp(1.0, f64::from(i32::MAX)) as i32,
                    1_000_000,
                )
            }
        }
    }

    /// The source time shown at timeline frame `t`, rounded down to a tick of
    /// `source_start`'s base (exact whenever a source frame starts there, since frame
    /// times are whole ticks).
    pub fn source_time_at(&self, t: i64, frame_duration: Rational) -> Timestamp {
        let offset = Timestamp::new(t - self.timeline.start, self.source_step(frame_duration))
            .rescale_floor(self.source_start.time_base);
        Timestamp::new(
            self.source_start.value.saturating_add(offset.value),
            self.source_start.time_base,
        )
    }
}

/// Whether a source frame at `pts` is presented at or before the instant timeline frame
/// `start + offset` shows, `source_start + offset * step` (`step`:
/// [`PlanSegment::source_step`]), compared exactly.
pub(crate) fn at_or_before(
    pts: Timestamp,
    source_start: Timestamp,
    offset: i64,
    step: Rational,
) -> bool {
    let (pn, pd) = positive(pts.time_base);
    let (sn, sd) = positive(source_start.time_base);
    let (fn_, fd) = positive(step);
    // pts.value * pn / pd <= start.value * sn / sd + offset * fn / fd, times pd * sd * fd.
    let left = i128::from(pts.value) * pn * sd * fd;
    let right = i128::from(source_start.value) * sn * pd * fd + i128::from(offset) * fn_ * pd * sd;
    left <= right
}

fn positive(r: Rational) -> (i128, i128) {
    if r.den < 0 {
        (-i128::from(r.num), -i128::from(r.den))
    } else {
        (i128::from(r.num), i128::from(r.den))
    }
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Where segments come from. A later task builds one from a sequence's tracks and clips;
/// [`SegmentList`] holds a fixed list.
pub trait DecodePlan: Send + 'static {
    type Segments: Iterator<Item = PlanSegment> + Send + 'static;

    /// Every segment that ends after timeline frame `frame`, in order of timeline start.
    fn segments_from(&self, frame: i64) -> Self::Segments;
}

/// A fixed decode plan.
#[derive(Clone, Debug)]
pub struct SegmentList {
    segments: Arc<[PlanSegment]>,
}

impl SegmentList {
    /// Sorts the segments by timeline start (stable, so equal starts keep their order) and
    /// leaves out empty ones.
    pub fn new(segments: impl IntoIterator<Item = PlanSegment>) -> Self {
        let mut segments: Vec<_> = segments
            .into_iter()
            .filter(|s| s.timeline.start < s.timeline.end)
            .collect();
        segments.sort_by_key(|s| s.timeline.start);
        Self {
            segments: segments.into(),
        }
    }

    pub fn segments(&self) -> &[PlanSegment] {
        &self.segments
    }
}

impl DecodePlan for SegmentList {
    type Segments = SegmentListIter;

    fn segments_from(&self, frame: i64) -> SegmentListIter {
        SegmentListIter {
            segments: self.segments.clone(),
            next: 0,
            from: frame,
        }
    }
}

/// [`SegmentList`]'s segments from a frame on.
#[derive(Clone, Debug)]
pub struct SegmentListIter {
    segments: Arc<[PlanSegment]>,
    next: usize,
    from: i64,
}

impl Iterator for SegmentListIter {
    type Item = PlanSegment;

    fn next(&mut self) -> Option<PlanSegment> {
        while let Some(segment) = self.segments.get(self.next) {
            self.next += 1;
            if segment.timeline.end > self.from {
                return Some(segment.clone());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(start: i64, end: i64) -> PlanSegment {
        PlanSegment {
            source: SourceRef::new(SourceId(1), "a.mp4"),
            timeline: start..end,
            source_start: Timestamp::new(0, Rational::new(1, 12800)),
            speed: Rational::new(1, 1),
            layer: 0,
            hash: SegmentHash::from(start as u64),
        }
    }

    #[test]
    fn segments_from_skips_what_ended() {
        let plan = SegmentList::new([segment(30, 60), segment(0, 30), segment(50, 50)]);
        let starts: Vec<_> = plan.segments_from(0).map(|s| s.timeline.start).collect();
        assert_eq!(starts, [0, 30]);
        let starts: Vec<_> = plan.segments_from(30).map(|s| s.timeline.start).collect();
        assert_eq!(starts, [30]);
        assert_eq!(plan.segments_from(60).count(), 0);
    }

    #[test]
    fn source_frames_follow_the_nearest_earlier_rule() {
        // 25 fps source (512 ticks of 1/12800 per frame) on a 30 fps timeline.
        let s = segment(0, 30);
        let fd = Rational::new(1, 30);
        let frame = |k: i64| Timestamp::new(512 * k, Rational::new(1, 12800));
        // Timeline frame 6 is 0.2 s: source frame 5 starts exactly there.
        assert!(at_or_before(frame(5), s.source_start, 6, fd));
        assert!(!at_or_before(frame(5), s.source_start, 5, fd));
        assert!(at_or_before(frame(4), s.source_start, 5, fd));
        assert_eq!(s.source_time_at(6, fd), frame(5));
        assert_eq!(s.source_time_at(1, fd).value, 426);
    }

    #[test]
    fn speed_scales_the_source_step() {
        let fd = Rational::new(1, 30);
        let mut s = segment(0, 30);
        s.speed = Rational::new(2, 1);
        assert_eq!(s.source_step(fd), Rational::new(1, 15));
        // Twice as fast: timeline frame 3 shows source time 0.2 s, 2560 ticks.
        assert_eq!(s.source_time_at(3, fd).value, 2560);
        s.speed = Rational::new(1, 2);
        assert_eq!(
            s.source_step(Rational::new(1001, 30000)),
            Rational::new(1001, 60000)
        );
    }
}
