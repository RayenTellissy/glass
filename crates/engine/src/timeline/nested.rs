//! Nested sequences in a plan (Decision 69): a clip of another sequence shows that
//! sequence's own plan, built in its own time, rate and canvas, as one layer.
//!
//! **Time.** At parent frame `t` the clip's source time `s` is the nested sequence's
//! timeline time (`Clip::source_time_at`, the clip's speed included), and the clip shows
//! the nested frame nearest to it (`Sequence::nearest_frame`, halves to the later frame)
//! while `0 <= s < ` the nested sequence's length; outside that it shows nothing.
//!
//! **Decoding.** The nested plan's decode segments are flattened into the parent's, so
//! nested media joins the same look-ahead: each becomes a parent segment over the parent
//! frames whose nested frame it covers, with its source time mapped through both clips
//! and its speed the product of both speeds. When every parent frame lands on a nested
//! frame (equal rates, a whole-frame source start, a speed that steps whole frames) the
//! mapping is exact; otherwise it samples each nested frame half a nested frame later,
//! which picks the nested frame's own media frame whenever the media is no finer than
//! the nested sequence's rate (a 60 fps file in a 24 fps compound shown at 30 fps may
//! show a media frame up to half a nested frame later than the compound does alone).

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use media::Rational as MediaRational;
use project::{ItemId, Rational, SequenceId, Time, TimeError};
use render::frame_cache::SegmentHash;

use super::animate::Animation;
use super::plan::{ClipLook, Hash128, TimelinePlan, timestamp};
use crate::lookahead::PlanSegment;

/// A clip nesting a sequence, as the parent plan shows it.
#[derive(Clone, Debug)]
pub struct NestedLook {
    pub item: ItemId,
    pub sequence: SequenceId,
    /// How the nested picture is placed and mixed: the clip's crop, transform, opacity,
    /// blend and effects, applied to the nested sequence's composite (its constants:
    /// [`Self::look_at`] evaluates animated ones).
    pub look: ClipLook,
    pub(super) plan: Arc<TimelinePlan>,
    /// The parent decode segment of each of the nested plan's segments (`None`: it shows
    /// in no parent frame).
    pub(super) segments: Vec<Option<usize>>,
    pub(super) time: NestedTime,
    pub(super) animation: Option<Arc<Animation>>,
    /// What the clip shows of the nested sequence, and its look when constant.
    pub(super) hash: SegmentHash,
}

impl NestedLook {
    /// The nested sequence's plan, in its own frames and canvas.
    pub fn plan(&self) -> &TimelinePlan {
        &self.plan
    }

    /// The nested frame parent frame `t` shows; `None` before the nested sequence's
    /// start or past its end.
    pub fn frame_at(&self, t: i64) -> Option<i64> {
        self.time.frame_at(t)
    }

    /// The parent decode segment that carries the nested plan's segment `segment`.
    pub fn segment(&self, segment: usize) -> Option<usize> {
        self.segments.get(segment).copied().flatten()
    }

    /// Whether the clip's own look is animated.
    pub fn is_animated(&self) -> bool {
        self.animation.is_some()
    }

    /// The clip's look at parent frame `t`.
    pub fn look_at(&self, t: i64) -> Cow<'_, ClipLook> {
        match &self.animation {
            Some(a) => Cow::Owned(a.look_at(&self.look, t)),
            None => Cow::Borrowed(&self.look),
        }
    }
}

/// Parent frames to nested time and frames.
#[derive(Clone, Debug)]
pub(super) struct NestedTime {
    /// The parent's frame rate.
    pub rate: Rational,
    pub item_start: Time,
    pub source_start: Time,
    pub speed: Rational,
    /// One nested frame.
    pub nested_frame: Time,
    /// The nested sequence's length.
    pub duration: Time,
}

impl NestedTime {
    /// The nested time at parent frame `t`.
    pub fn source_time(&self, t: i64) -> Option<Time> {
        let time = Time::from_frames(t, self.rate).ok()?;
        let offset = time.checked_sub(self.item_start).ok()?;
        self.source_start
            .checked_add(offset.mul_rational(self.speed).ok()?)
            .ok()
    }

    pub fn frame_at(&self, t: i64) -> Option<i64> {
        let s = self.source_time(t)?;
        if s.is_negative() || s >= self.duration {
            return None;
        }
        s.round_to_multiple(self.nested_frame).ok()
    }

    /// The part of `frames` where the clip shows a nested frame.
    pub fn visible(&self, frames: Range<i64>) -> Range<i64> {
        let start = first(frames.clone(), |t| {
            self.source_time(t).is_some_and(|s| !s.is_negative())
        });
        let end = first(start..frames.end, |t| {
            self.source_time(t).is_none_or(|s| s >= self.duration)
        });
        start..end
    }

    /// Nested time per parent frame.
    fn step(&self) -> Result<Time, TimeError> {
        Time::from_frames(1, self.rate)?.mul_rational(self.speed)
    }
}

/// The first frame of `frames` where `pred` holds (`frames.end` if none), for a `pred`
/// that holds from some frame on.
fn first(frames: Range<i64>, pred: impl Fn(i64) -> bool) -> i64 {
    let (mut lo, mut hi) = (frames.start, frames.end.max(frames.start));
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// The nested plan's decode segments as parent segments over parent frames `frames` (the
/// clip's extent with its transition handles), each with the index of the nested segment
/// it carries. See the module docs.
pub(super) fn flatten(
    item: ItemId,
    layer: u32,
    time: &NestedTime,
    frames: Range<i64>,
    nested: &TimelinePlan,
    frame_duration: MediaRational,
) -> Result<Vec<(usize, PlanSegment)>, TimeError> {
    let visible = time.visible(frames);
    let step = time.step()?;
    let mut out = Vec::new();
    for (j, segment) in nested.segments().segments().iter().enumerate() {
        let n = |t: i64| time.frame_at(t).unwrap_or(i64::MAX);
        let start = first(visible.clone(), |t| n(t) >= segment.timeline.start);
        let end = first(start..visible.end, |t| n(t) >= segment.timeline.end);
        if start >= end {
            continue;
        }
        let Some(s) = time.source_time(start) else {
            continue;
        };
        let aligned = s.is_multiple_of(time.nested_frame) && step.is_multiple_of(time.nested_frame);
        let shift = if aligned {
            Time::ZERO
        } else {
            time.nested_frame.mul_ratio(1, 2)?
        };
        // The nested segment's source time at nested time `s + shift`.
        let base = &segment.source_start;
        let segment_source = Time::new(
            base.value * i64::from(base.time_base.num),
            i64::from(base.time_base.den),
        )?;
        let offset = s
            .checked_add(shift)?
            .checked_sub(Time::from_frames(segment.timeline.start, nested.rate())?)?
            .mul_ratio(i64::from(segment.speed.num), i64::from(segment.speed.den))?;
        let source = segment_source.checked_add(offset)?;
        let speed = product(time.speed, segment.speed);
        let source_start = timestamp(source, speed, frame_duration);
        let mut h = Hash128::new("nested segment");
        h.add(&item.0);
        h.add(&segment.hash.0);
        h.add(&(start, end));
        h.add(&(source.num(), source.den()));
        h.add(&(speed.num, speed.den));
        out.push((
            j,
            PlanSegment {
                source: segment.source.clone(),
                timeline: start..end,
                source_start,
                speed,
                layer,
                hash: h.finish(),
            },
        ));
    }
    Ok(out)
}

/// `a * b`, reduced, as the media speed (rounded to a millionth in the rare case it does
/// not fit).
fn product(a: Rational, b: MediaRational) -> MediaRational {
    let num = i64::from(a.num) * i64::from(b.num);
    let den = i64::from(a.den) * i64::from(b.den);
    let g = gcd(num.unsigned_abs(), den.unsigned_abs()).max(1) as i64;
    match (i32::try_from(num / g), i32::try_from(den / g)) {
        (Ok(num), Ok(den)) => MediaRational::new(num, den),
        _ => MediaRational::new(
            (num as f64 / den as f64 * 1e6)
                .round()
                .clamp(1.0, f64::from(i32::MAX)) as i32,
            1_000_000,
        ),
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(rate: (i32, i32), nested: i32, speed: Rational, source_start: Time) -> NestedTime {
        NestedTime {
            rate: Rational::new(rate.0, rate.1),
            item_start: Time::ZERO,
            source_start,
            speed,
            nested_frame: Rational::new(nested, 1).frame_duration().unwrap(),
            duration: Time::seconds(2),
        }
    }

    #[test]
    fn nearest_nested_frames_halves_later_and_nothing_outside() {
        // 30 fps showing 24 fps: frame t is 0.8 t nested frames.
        let t = time((30, 1), 24, Rational::ONE, Time::ZERO);
        let shown: Vec<_> = (0..6).map(|f| t.frame_at(f)).collect();
        assert_eq!(
            shown,
            [Some(0), Some(1), Some(2), Some(2), Some(3), Some(4)]
        );
        // 48 fps showing 24 fps: odd frames are halves and go to the later frame.
        let t = time((48, 1), 24, Rational::ONE, Time::ZERO);
        assert_eq!(t.frame_at(1), Some(1));
        assert_eq!(t.frame_at(3), Some(2));
        // Two seconds of nested sequence: 60 parent frames at 30 fps.
        let t = time((30, 1), 24, Rational::ONE, Time::ZERO);
        assert_eq!(t.visible(-5..100), 0..60);
        assert_eq!(t.frame_at(60), None);
        assert_eq!(t.frame_at(-1), None);
        // Twice as fast from 1 s: 30 parent frames left.
        let t = time((30, 1), 24, Rational::new(2, 1), Time::seconds(1));
        assert_eq!(t.visible(0..100), 0..15);
    }

    #[test]
    fn products_reduce() {
        assert_eq!(
            product(Rational::new(2, 1), MediaRational::new(1, 4)),
            MediaRational::new(1, 2)
        );
    }
}
