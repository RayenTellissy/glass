//! Clip edges on an audio track, in mix samples: the fades a track's transitions give its
//! clips (a crossfade is the outgoing clip fading out and the incoming one fading in over
//! the same samples; a transition next to a gap or the track's start fades one clip from
//! or to nothing) and, when asked, short declick ramps at hard cuts.
//!
//! **Positions.** Every edge is a sample index at the mix rate: a time `t` on the
//! sequence's timeline plays at sample `round(t × rate)`, halves to the later sample
//! ([`super::sample_of`], `Time::round_to_multiple`, the same rounding the project uses
//! for durations on its grids). Audio tracks sit on the sequence's sample grid, so at the
//! sequence's own rate every clip edge and transition span is already an exact sample and
//! nothing rounds; at another mix rate both clips of a cut round the same time to the same
//! sample. Either way a cut at sample `N` plays the outgoing clip up to sample `N − 1` and
//! the incoming clip from sample `N`: nothing is played twice or skipped.
//!
//! **Fades.** A transition's span `from..to` (its duration placed by its alignment, see
//! `project::transition`) is where the outgoing clip, playing on past its end into its tail
//! handle, fades out and the incoming clip, starting early from its head handle, fades in;
//! the curve comes from the transition (`project::audio_transition::curve_of`) and its
//! progress is bent by the transition's easing, the same ids as pictures. The gains are
//! `audio::mixer::fade`'s, per sample at the sample's centre.
//!
//! **Declick** (off by default, [`super::MixSettings::declick`]): a hard cut, a clip edge
//! without a transition, makes the waveform jump unless the clip on the other side
//! continues it seamlessly (the same source, speed and gain, its source start where the
//! other's source ends). With declick on, every edge that would jump fades over
//! [`DECLICK_SECONDS`] (an S-curve inside the clip, at most half the clip), so a cut
//! between unrelated clips or into a gap does not click. Off by default because it
//! changes the samples next to every cut, which sample-accurate editing must not do unless
//! asked.

use std::collections::HashMap;

use audio::mixer::fade::{FadeCurve, fade_in_at, fade_out_at};
use project::{ClipParam, Item, ItemId, Time, Track, Transition, audio_transition};
use render::transition::Easing;

use super::sample_of;

/// The length of a declick ramp: 1 ms.
pub const DECLICK_SECONDS: f64 = 0.001;

/// A fade over mix samples `from..to`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fade {
    pub from: i64,
    pub to: i64,
    pub curve: FadeCurve,
    pub easing: Easing,
}

/// What shapes one clip's edges. Fades take precedence: an edge with a fade never has a
/// declick ramp.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Edges {
    /// Fading in: the clip is the incoming side of a transition.
    pub head: Option<Fade>,
    /// Fading out: the clip is the outgoing side of a transition.
    pub tail: Option<Fade>,
    /// Declick ramp lengths in samples at the clip's start and end (0: none).
    pub declick_head: i64,
    pub declick_tail: i64,
}

impl Edges {
    /// The mix frames the clip sounds over, given its own `start..end`: stretched over
    /// its transitions' spans.
    pub fn play_range(&self, start: i64, end: i64) -> (i64, i64) {
        (
            self.head.map_or(start, |f| f.from.min(start)),
            self.tail.map_or(end, |f| f.to.max(end)),
        )
    }

    /// Whether the gain differs from 1 anywhere in mix frames `from..to` of a clip over
    /// `start..end`.
    pub fn shapes(&self, start: i64, end: i64, from: i64, to: i64) -> bool {
        let meets = |a: i64, b: i64| a < to && b > from;
        let (head, tail) = self.declick(start, end);
        self.head.is_some_and(|f| meets(f.from, f.to))
            || self.tail.is_some_and(|f| meets(f.from, f.to))
            || (head > 0 && meets(start, start + head))
            || (tail > 0 && meets(end - tail, end))
    }

    /// The gain at mix frame `k` of a clip over `start..end`.
    pub fn gain(&self, start: i64, end: i64, k: i64) -> f64 {
        let mut gain = 1.0;
        if let Some(f) = self.head {
            gain *= fade_in_at(f.curve, f.from, f.to, k, |p| f.easing.apply(p));
        }
        if let Some(f) = self.tail {
            gain *= fade_out_at(f.curve, f.from, f.to, k, |p| f.easing.apply(p));
        }
        let (head, tail) = self.declick(start, end);
        if head > 0 {
            gain *= fade_in_at(FadeCurve::SCurve, start, start + head, k, |p| p);
        }
        if tail > 0 {
            gain *= fade_out_at(FadeCurve::SCurve, end - tail, end, k, |p| p);
        }
        gain
    }

    /// The declick ramps' lengths, at most half the clip each.
    fn declick(&self, start: i64, end: i64) -> (i64, i64) {
        let half = (end - start) / 2;
        (self.declick_head.min(half), self.declick_tail.min(half))
    }
}

/// The curve of a transition on an audio track.
pub(crate) fn fade_curve(transition: &Transition) -> FadeCurve {
    match audio_transition::curve_of(transition) {
        audio_transition::LINEAR => FadeCurve::Linear,
        audio_transition::S_CURVE => FadeCurve::SCurve,
        _ => FadeCurve::EqualPower,
    }
}

/// The easing a transition names; ids this build does not know play linear (the same ids
/// the timeline plan reads for pictures).
fn easing(id: &str) -> Easing {
    match id {
        "ease-in" => Easing::EaseIn,
        "ease-out" => Easing::EaseOut,
        "ease-in-out" => Easing::EaseInOut,
        _ => Easing::Linear,
    }
}

/// The edges of every clip of `track` that has a fade or a declick ramp, at mix `rate`;
/// `grid` is the track's grid. A transition whose cut no longer resolves (a damaged tree;
/// edits never leave one) is skipped with a warning, and its clips cut hard.
pub(crate) fn track_edges(
    track: &Track,
    grid: Time,
    rate: u32,
    declick: bool,
) -> HashMap<ItemId, Edges> {
    let mut edges: HashMap<ItemId, Edges> = HashMap::new();
    for transition in &track.transitions {
        let span = track
            .transition_span(grid, transition)
            .map_err(|e| e.to_string())
            .and_then(|span| {
                let from = sample_of(span.start, rate).map_err(|e| e.to_string())?;
                let to = span
                    .end()
                    .map_err(|e| e.to_string())
                    .and_then(|end| sample_of(end, rate).map_err(|e| e.to_string()))?;
                Ok((from, to))
            });
        let (from, to) = match span {
            Ok(span) => span,
            Err(error) => {
                tracing::warn!(transition = %transition.id, %error, "audio transition skipped");
                continue;
            }
        };
        let fade = Fade {
            from,
            to,
            curve: fade_curve(transition),
            easing: easing(&transition.easing),
        };
        if let Some(item) = transition.outgoing {
            edges.entry(item).or_default().tail = Some(fade);
        }
        if let Some(item) = transition.incoming {
            edges.entry(item).or_default().head = Some(fade);
        }
    }
    if declick {
        let ramp = ((DECLICK_SECONDS * f64::from(rate)).round() as i64).max(1);
        let items: Vec<&std::sync::Arc<Item>> = track.items.iter().collect();
        for (i, item) in items.iter().enumerate() {
            if !item.clip().is_some_and(|c| c.enabled) {
                continue;
            }
            let before = i.checked_sub(1).map(|j| items[j].as_ref());
            let after = items.get(i + 1).map(|a| a.as_ref());
            let e = edges.entry(item.id).or_default();
            if e.head.is_none() && !before.is_some_and(|b| continues(b, item)) {
                e.declick_head = ramp;
            }
            if e.tail.is_none() && !after.is_some_and(|a| continues(item, a)) {
                e.declick_tail = ramp;
            }
        }
    }
    edges
}

/// Whether `b` continues `a` without a jump in the waveform: they meet, both are enabled
/// clips of the same source at the same constant speed and gain, and `b` starts in its
/// source where `a` ends.
fn continues(a: &Item, b: &Item) -> bool {
    let (Some(ca), Some(cb)) = (a.clip(), b.clip()) else {
        return false;
    };
    let meet = a.range.end().is_ok_and(|end| end == b.range.start);
    let source_end = ca.source_range(a.range).and_then(|r| Ok(r.end()?));
    meet && ca.enabled
        && cb.enabled
        && ca.source == cb.source
        && ca.speed.constant_speed().is_some()
        && ca.speed.constant_speed() == cb.speed.constant_speed()
        && source_end.is_ok_and(|end| end == cb.source_start)
        && ca.gain_db == cb.gain_db
        && ca.curve(ClipParam::GainDb) == cb.curve(ClipParam::GainDb)
}
