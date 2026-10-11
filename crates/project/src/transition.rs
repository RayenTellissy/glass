//! Transitions (schema v5): first-class nodes on the cuts of a track, and the edits and
//! queries that use them.
//!
//! A transition sits on one cut of a video or audio track and names the clips it joins:
//! the outgoing clip ending at the cut and the incoming clip starting there. Either may be
//! absent next to a gap or the track's start (a fade from or to nothing), never both, and
//! such a one-sided transition is only allowed where its clip's edge does not meet another
//! item (two items that meet take a two-sided transition). Each clip edge holds at most one
//! transition, and the transitions of a track never overlap each other.
//!
//! The transition covers `duration` (on the track's grid) placed by its [`Alignment`]
//! relative to the cut: centred on it (with an odd number of grid steps, the extra step
//! after the cut), starting at it or ending at it. The part before the cut lies over the
//! outgoing clip (or the gap) and shows the incoming clip's media from before its start, so
//! it needs that much head handle: the incoming clip's source start divided by its speed.
//! The part after the cut lies over the incoming clip (or the gap) and shows the outgoing
//! clip's media from past its end. The project knows where media starts (source time 0) but
//! not how long a file is, so edits and [`Project::validate`] check head handles only;
//! [`Project::check_transition_media`] checks tail handles against lengths the caller
//! knows (the media pool's facts), and [`Project::source_used_until`] counts them, so
//! relinking refuses a file too short for them.
//!
//! Edits on a transition ([`Project::add_transition`],
//! [`Project::set_transition_duration`], [`Project::set_transition_alignment`]) refuse a
//! span that does not fit (past the far end of a clip, across a neighbouring transition or
//! item, before zero, or beyond a head handle) with [`EditError::TransitionRoom`] or
//! [`EditError::TransitionHandle`]; nothing is clamped. Edits on the clips instead keep the
//! tree valid by fitting the track's transitions afterwards: a transition whose clips no
//! longer meet at its cut (one was moved, deleted, trimmed at the cut or moved to another
//! track) or whose one-sided edge now meets an item is removed, and one that no longer fits
//! is shortened to the longest whole number of grid steps that fits, or removed when not
//! even one step does. Transitions are fitted in cut order, so when two compete for one
//! clip the earlier one keeps its length.
//!
//! The transition's `kind` is the id of a transition in the render-side registry
//! (`assets/transitions/<id>/`), and its `params` follow that transition's parameter
//! schema; a kind this build does not know is kept as it is. `easing` names the curve the
//! progress follows (`linear` until the easing editor adds others).
//!
//! [`Track::segment_at`] is the engine's query: what plays on a track at a time, one item
//! or a transition between two clips with its progress. Pure: `project` starts no threads.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::EditError;
use crate::id::{ItemId, MediaId, TrackId, TransitionId};
use crate::model::{Item, Project, Sequence, Track};
use crate::params::{ParamSchema, ParamValue, ParamValues};
use crate::time::{Time, TimeRange};

/// The transition a new one uses unless told otherwise.
pub const DEFAULT_KIND: &str = "cross-dissolve";
/// The easing a new transition uses unless told otherwise.
pub const DEFAULT_EASING: &str = "linear";

/// Where a transition lies relative to its cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    /// Centred on the cut; with an odd number of grid steps the extra step is after it.
    /// Needs handles on both clips.
    #[default]
    Centre,
    /// Starts at the cut, over the head of the incoming clip. Needs the outgoing clip's
    /// media past its end.
    Start,
    /// Ends at the cut, over the tail of the outgoing clip. Needs the incoming clip's media
    /// before its start.
    End,
}

impl Alignment {
    pub const ALL: [Alignment; 3] = [Alignment::Centre, Alignment::Start, Alignment::End];

    /// How much of `duration` (a whole number of `grid` steps) lies before and after the
    /// cut.
    pub fn split(self, duration: Time, grid: Time) -> Result<(Time, Time), EditError> {
        Ok(match self {
            Alignment::Start => (Time::ZERO, duration),
            Alignment::End => (duration, Time::ZERO),
            Alignment::Centre => {
                let steps = duration.floor_to_multiple(grid)?;
                let before = grid.mul_ratio(steps.div_euclid(2), 1)?;
                (before, duration.checked_sub(before)?)
            }
        })
    }

    /// The most grid steps a transition can last given `before` and `after` steps of room
    /// on each side of the cut (`None`: unbounded).
    fn max_steps(self, before: i64, after: Option<i64>) -> i64 {
        let after = after.unwrap_or(i64::MAX);
        match self {
            Alignment::Start => after,
            Alignment::End => before,
            // floor(n / 2) ≤ before and ceil(n / 2) ≤ after.
            Alignment::Centre => before
                .saturating_mul(2)
                .saturating_add(1)
                .min(after.saturating_mul(2)),
        }
    }
}

/// A side of a cut, in [`EditError::TransitionRoom`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CutSide {
    Before,
    After,
}

impl fmt::Display for CutSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CutSide::Before => "before",
            CutSide::After => "after",
        })
    }
}

/// A transition on a cut. See the module docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub id: TransitionId,
    /// The clip ending at the cut; `None` for a fade in from nothing.
    pub outgoing: Option<ItemId>,
    /// The clip starting at the cut; `None` for a fade out to nothing.
    pub incoming: Option<ItemId>,
    /// The render-side transition id (`cross-dissolve`): lowercase ASCII letters, digits
    /// and `-`.
    pub kind: String,
    /// Positive, a whole number of the track's grid steps.
    pub duration: Time,
    pub alignment: Alignment,
    /// The id of the curve progress follows (`linear`), same characters as `kind`.
    pub easing: String,
    /// Values for the kind's parameter schema; an unset parameter reads as its default.
    pub params: ParamValues,
}

/// A new transition: the clips it joins and how it looks. [`TransitionSpec::new`] fills
/// in the defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionSpec {
    pub outgoing: Option<ItemId>,
    pub incoming: Option<ItemId>,
    pub kind: String,
    pub duration: Time,
    pub alignment: Alignment,
    pub easing: String,
    pub params: ParamValues,
}

impl TransitionSpec {
    /// A [`DEFAULT_KIND`] transition, centred, [`DEFAULT_EASING`], every parameter at its
    /// default.
    pub fn new(outgoing: Option<ItemId>, incoming: Option<ItemId>, duration: Time) -> Self {
        Self {
            outgoing,
            incoming,
            kind: DEFAULT_KIND.to_owned(),
            duration,
            alignment: Alignment::Centre,
            easing: DEFAULT_EASING.to_owned(),
            params: ParamValues::new(),
        }
    }
}

/// What plays on a track at one time ([`Track::segment_at`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Segment<'a> {
    /// One item alone.
    Item(&'a Arc<Item>),
    /// A transition between two clips, or between a clip and nothing.
    Transition(TransitionAt<'a>),
}

/// A transition in progress at the queried time.
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionAt<'a> {
    pub transition: &'a Arc<Transition>,
    /// The clip ending at the cut, shown past its end over the part after the cut.
    pub outgoing: Option<&'a Arc<Item>>,
    /// The clip starting at the cut, shown before its start over the part before the cut.
    pub incoming: Option<&'a Arc<Item>>,
    /// The span the transition covers on the timeline.
    pub span: TimeRange,
    /// `(time − span.start) / duration`: 0 at the span's first instant, below 1 at its
    /// last, linear (the transition's easing is not applied).
    pub progress: f64,
}

/// What a transition needs of its clips' media.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandleNeeds {
    /// The outgoing clip, its media and the source time it is shown up to (exclusive): its
    /// own source end plus the part after the cut times its speed.
    pub outgoing: Option<(ItemId, MediaId, Time)>,
    /// The incoming clip, its media and the source time it is shown from: its source start
    /// less the part before the cut times its speed (never negative in a valid tree).
    pub incoming: Option<(ItemId, MediaId, Time)>,
}

/// An id a project stores for a render-side asset: lowercase ASCII letters, digits and `-`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn check_kind(kind: &str) -> Result<(), EditError> {
    if valid_id(kind) {
        Ok(())
    } else {
        Err(EditError::InvalidTransitionKind(kind.to_owned()))
    }
}

fn check_easing(easing: &str) -> Result<(), EditError> {
    if valid_id(easing) {
        Ok(())
    } else {
        Err(EditError::InvalidEasing(easing.to_owned()))
    }
}

/// The duration is positive and a whole number of grid steps.
fn check_duration(duration: Time, grid: Time, track: &Track) -> Result<(), EditError> {
    if !duration.is_positive() {
        return Err(EditError::EmptyRange(duration));
    }
    if !duration.is_multiple_of(grid) {
        return Err(EditError::OffGrid {
            time: duration,
            grid,
            kind: track.kind,
        });
    }
    Ok(())
}

/// A transition's cut resolved against its track's items.
struct Cut<'a> {
    at: Time,
    outgoing: Option<&'a Arc<Item>>,
    incoming: Option<&'a Arc<Item>>,
    /// The earliest time the part before the cut may reach: the outgoing clip's start, or
    /// the end of the item before the gap, or zero.
    floor: Time,
    /// The latest time the part after the cut may reach: the incoming clip's end, or the
    /// start of the item after the gap; `None` past the last item.
    ceiling: Option<Time>,
}

impl Cut<'_> {
    /// The incoming clip's head handle in timeline time: its source start over its speed.
    fn head_handle(&self) -> Result<Option<Time>, EditError> {
        let Some(item) = self.incoming else {
            return Ok(None);
        };
        let clip = item.clip().ok_or(EditError::NotAClip(item.id))?;
        let speed = clip.speed.speed()?;
        Ok(Some(
            clip.source_start
                .mul_ratio(i64::from(speed.den), i64::from(speed.num))?,
        ))
    }

    fn span(&self, before: Time, after: Time) -> Result<TimeRange, EditError> {
        Ok(TimeRange::between(
            self.at.checked_sub(before)?,
            self.at.checked_add(after)?,
        )?)
    }

    /// Checks that `before` and `after` fit between `floor` and `ceiling` (narrowed by the
    /// neighbouring transitions' spans) and within the incoming clip's head handle. Error
    /// order: room before, head handle, room after.
    fn check_room(
        &self,
        before: Time,
        after: Time,
        prev_end: Option<Time>,
        next_start: Option<Time>,
    ) -> Result<(), EditError> {
        let floor = prev_end.map_or(self.floor, |e| e.max(self.floor));
        let room = self.at.checked_sub(floor)?;
        if before > room {
            return Err(EditError::TransitionRoom {
                side: CutSide::Before,
                needed: before,
                available: room,
            });
        }
        if before.is_positive()
            && let (Some(item), Some(handle)) = (self.incoming, self.head_handle()?)
            && before > handle
        {
            return Err(EditError::TransitionHandle {
                item: item.id,
                needed: before,
                available: handle,
            });
        }
        let ceiling = match (self.ceiling, next_start) {
            (Some(c), Some(n)) => Some(c.min(n)),
            (c, n) => c.or(n),
        };
        if let Some(ceiling) = ceiling {
            let room = ceiling.checked_sub(self.at)?;
            if after > room {
                return Err(EditError::TransitionRoom {
                    side: CutSide::After,
                    needed: after,
                    available: room,
                });
            }
        }
        Ok(())
    }

    /// The most grid steps the transition can last after `prev_end`.
    fn max_steps(
        &self,
        alignment: Alignment,
        grid: Time,
        prev_end: Option<Time>,
    ) -> Result<i64, EditError> {
        let floor = prev_end.map_or(self.floor, |e| e.max(self.floor));
        let mut before = self.at.checked_sub(floor)?;
        if let Some(handle) = self.head_handle()? {
            before = before.min(handle);
        }
        let before = before.floor_to_multiple(grid)?.max(0);
        let after = match self.ceiling {
            Some(c) => Some(c.checked_sub(self.at)?.floor_to_multiple(grid)?.max(0)),
            None => None,
        };
        Ok(alignment.max_steps(before, after))
    }
}

/// Finds a track's items by id.
trait ItemIndex {
    fn index_of(&self, track: &Track, id: ItemId) -> Option<usize>;
}

/// A linear search, for one lookup.
struct Scan;

impl ItemIndex for Scan {
    fn index_of(&self, track: &Track, id: ItemId) -> Option<usize> {
        track.item_index(id)
    }
}

/// A map built once, for a pass over every transition of a track.
struct Map(HashMap<ItemId, usize>);

impl Map {
    fn new(track: &Track) -> Self {
        Map(track
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| (it.id, i))
            .collect())
    }
}

/// Known positions of a few items, falling back to a linear search.
struct Near(Vec<(ItemId, usize)>);

impl ItemIndex for Near {
    fn index_of(&self, track: &Track, id: ItemId) -> Option<usize> {
        self.0
            .iter()
            .find(|(x, _)| *x == id)
            .map(|(_, i)| *i)
            .or_else(|| track.item_index(id))
    }
}

impl ItemIndex for Map {
    fn index_of(&self, _: &Track, id: ItemId) -> Option<usize> {
        self.0.get(&id).copied()
    }
}

/// Resolves the cut between `outgoing` and `incoming` on `track`. Error order:
/// [`EditError::TransitionNeedsClip`], [`EditError::UnknownItem`] (not on this track,
/// outgoing first), [`EditError::NotAClip`] (outgoing first), [`EditError::NotAdjacent`],
/// [`EditError::CutNotFree`].
fn resolve<'a>(
    track: &'a Track,
    index: &impl ItemIndex,
    outgoing: Option<ItemId>,
    incoming: Option<ItemId>,
) -> Result<Cut<'a>, EditError> {
    if outgoing.is_none() && incoming.is_none() {
        return Err(EditError::TransitionNeedsClip);
    }
    let find = |id: Option<ItemId>| -> Result<Option<(usize, &'a Arc<Item>)>, EditError> {
        let Some(id) = id else { return Ok(None) };
        let i = index
            .index_of(track, id)
            .ok_or(EditError::UnknownItem(id))?;
        Ok(Some((i, &track.items[i])))
    };
    let out = find(outgoing)?;
    let inc = find(incoming)?;
    for (_, item) in out.iter().chain(inc.iter()) {
        if item.clip().is_none() {
            return Err(EditError::NotAClip(item.id));
        }
    }
    match (out, inc) {
        (Some((_, a)), Some((_, b))) => {
            let at = a.range.end()?;
            if at != b.range.start {
                return Err(EditError::NotAdjacent {
                    outgoing: a.id,
                    incoming: b.id,
                });
            }
            Ok(Cut {
                at,
                outgoing: Some(a),
                incoming: Some(b),
                floor: a.range.start,
                ceiling: Some(b.range.end()?),
            })
        }
        (Some((i, a)), None) => {
            let at = a.range.end()?;
            let next = track.items.get(i + 1);
            if let Some(next) = next
                && next.range.start == at
            {
                return Err(EditError::CutNotFree {
                    item: a.id,
                    other: next.id,
                });
            }
            Ok(Cut {
                at,
                outgoing: Some(a),
                incoming: None,
                floor: a.range.start,
                ceiling: next.map(|n| n.range.start),
            })
        }
        (None, Some((j, b))) => {
            let at = b.range.start;
            let prev = j.checked_sub(1).map(|p| &track.items[p]);
            let floor = match prev {
                Some(prev) => {
                    let end = prev.range.end()?;
                    if end == at {
                        return Err(EditError::CutNotFree {
                            item: b.id,
                            other: prev.id,
                        });
                    }
                    end
                }
                None => Time::ZERO,
            };
            Ok(Cut {
                at,
                outgoing: None,
                incoming: Some(b),
                floor,
                ceiling: Some(b.range.end()?),
            })
        }
        (None, None) => Err(EditError::TransitionNeedsClip),
    }
}

/// The transitions of `track` other than `skip` with their spans, resolved with `index`.
fn spans<'a>(
    track: &'a Track,
    grid: Time,
    index: &impl ItemIndex,
    skip: Option<TransitionId>,
) -> Result<Vec<(Time, TimeRange, &'a Arc<Transition>)>, EditError> {
    let mut out = Vec::with_capacity(track.transitions.len());
    for t in &track.transitions {
        if Some(t.id) == skip {
            continue;
        }
        let cut = resolve(track, index, t.outgoing, t.incoming)?;
        let (before, after) = t.alignment.split(t.duration, grid)?;
        out.push((cut.at, cut.span(before, after)?, t));
    }
    Ok(out)
}

/// The end of the last span before the cut at `at` and the start of the first after it.
fn neighbours(
    spans: &[(Time, TimeRange, &Arc<Transition>)],
    at: Time,
) -> (Option<Time>, Option<Time>) {
    let mut prev_end: Option<Time> = None;
    let mut next_start: Option<Time> = None;
    for (cut, span, _) in spans {
        if *cut < at {
            let end = span.end().unwrap_or(span.start);
            prev_end = Some(prev_end.map_or(end, |e| e.max(end)));
        } else if *cut > at {
            next_start = Some(next_start.map_or(span.start, |s| s.min(span.start)));
        }
    }
    (prev_end, next_start)
}

/// Checks that `transition` (in `track` or about to be) fits: its cut, its kind and easing
/// ids, its duration on the grid, and its span against its clips, the neighbouring items
/// and transitions, and the incoming clip's head handle.
fn check_fits(track: &Track, grid: Time, transition: &Transition) -> Result<(), EditError> {
    let cut = resolve(track, &Scan, transition.outgoing, transition.incoming)?;
    check_duration(transition.duration, grid, track)?;
    let (before, after) = transition.alignment.split(transition.duration, grid)?;
    let others = spans(track, grid, &Map::new(track), Some(transition.id))?;
    let (prev_end, next_start) = neighbours(&others, cut.at);
    cut.check_room(before, after, prev_end, next_start)
}

/// The checks [`Project::validate`] runs on a track's transitions (besides their order
/// and node ids): valid kind and easing ids, durations on the grid, cuts that resolve, one
/// transition per cut, and spans that fit without overlapping (checked in cut order).
pub(crate) fn check_track(track: &Track, grid: Time) -> Result<(), EditError> {
    if track.transitions.is_empty() {
        return Ok(());
    }
    let index = Map::new(track);
    let mut cuts = Vec::with_capacity(track.transitions.len());
    for t in &track.transitions {
        check_kind(&t.kind)?;
        check_easing(&t.easing)?;
        check_duration(t.duration, grid, track)?;
        cuts.push((resolve(track, &index, t.outgoing, t.incoming)?, t));
    }
    cuts.sort_by_key(|(cut, t)| (cut.at, t.id));
    let mut prev: Option<(Time, Time)> = None;
    for (cut, t) in &cuts {
        if let Some((at, _)) = prev
            && at == cut.at
        {
            return Err(EditError::TransitionExists(t.id));
        }
        let (before, after) = t.alignment.split(t.duration, grid)?;
        cut.check_room(before, after, prev.map(|(_, end)| end), None)?;
        prev = Some((cut.at, cut.at.checked_add(after)?));
    }
    Ok(())
}

/// Brings a track's transitions back in line with its items after an edit on them: removes
/// those whose cut no longer resolves and shortens (or removes) those that no longer fit,
/// in cut order. Leaves `track.transitions` untouched when nothing changes.
pub(crate) fn fit_track(track: &mut Track, grid: Time) {
    if track.transitions.is_empty() {
        return;
    }
    // (index in the list, new duration in steps or None to remove).
    let mut changes: Vec<(usize, Option<i64>)> = Vec::new();
    {
        let index = Map::new(track);
        let mut cuts = Vec::with_capacity(track.transitions.len());
        for (i, t) in track.transitions.iter().enumerate() {
            match resolve(track, &index, t.outgoing, t.incoming) {
                Ok(cut) => cuts.push((cut, i, t)),
                Err(_) => changes.push((i, None)),
            }
        }
        cuts.sort_by_key(|(cut, _, t)| (cut.at, t.id));
        let mut prev: Option<(Time, Time)> = None;
        for (cut, i, t) in &cuts {
            if prev.is_some_and(|(at, _)| at == cut.at) {
                changes.push((*i, None));
                continue;
            }
            let fitted = (|| -> Result<Option<(i64, Time)>, EditError> {
                let steps = t.duration.floor_to_multiple(grid)?;
                let max = cut.max_steps(t.alignment, grid, prev.map(|(_, end)| end))?;
                let steps = steps.min(max);
                if steps < 1 {
                    return Ok(None);
                }
                let duration = grid.mul_ratio(steps, 1)?;
                let (_, after) = t.alignment.split(duration, grid)?;
                Ok(Some((steps, cut.at.checked_add(after)?)))
            })();
            match fitted {
                Ok(Some((steps, end))) => {
                    if grid.mul_ratio(steps, 1).ok() != Some(t.duration) {
                        changes.push((*i, Some(steps)));
                    }
                    prev = Some((cut.at, end));
                }
                Ok(None) | Err(_) => changes.push((*i, None)),
            }
        }
    }
    if changes.is_empty() {
        return;
    }
    changes.sort_by_key(|(i, _)| std::cmp::Reverse(*i));
    for (i, steps) in changes {
        match steps.and_then(|s| grid.mul_ratio(s, 1).ok()) {
            Some(duration) => Arc::make_mut(&mut track.transitions[i]).duration = duration,
            None => {
                track.transitions.remove(i);
            }
        }
    }
}

impl Track {
    pub fn transition(&self, id: TransitionId) -> Option<&Arc<Transition>> {
        self.transitions.iter().find(|t| t.id == id)
    }

    /// The transitions on `item`'s cuts: the one into its head and the one out of its tail.
    pub fn transitions_of(
        &self,
        item: ItemId,
    ) -> (Option<&Arc<Transition>>, Option<&Arc<Transition>>) {
        let head = self.transitions.iter().find(|t| t.incoming == Some(item));
        let tail = self.transitions.iter().find(|t| t.outgoing == Some(item));
        (head, tail)
    }

    /// The span a transition of this track covers, given the track's grid.
    pub fn transition_span(
        &self,
        grid: Time,
        transition: &Transition,
    ) -> Result<TimeRange, EditError> {
        let cut = resolve(self, &Scan, transition.outgoing, transition.incoming)?;
        let (before, after) = transition.alignment.split(transition.duration, grid)?;
        cut.span(before, after)
    }

    /// What plays on this track at `time` (`grid` is the track's grid): the transition
    /// whose span holds `time` with its clips and progress, else the item covering `time`,
    /// else `None` (a gap, before zero, past the end). Costs a binary search over the
    /// items plus a pass over the transitions.
    pub fn segment_at(&self, grid: Time, time: Time) -> Result<Option<Segment<'_>>, EditError> {
        // The last item starting at or before `time`.
        let at_or_before = match self.items.binary_search_by(|i| i.range.start.cmp(&time)) {
            Ok(i) => Some(i),
            Err(0) => None,
            Err(i) => Some(i - 1),
        };
        let covering = match at_or_before {
            Some(i) if self.items[i].range.contains(time)? => Some(i),
            _ => None,
        };
        // A transition holding `time` sits on the head or tail of the covering item, or, in
        // a gap, is a one-sided one on the gap's edges.
        let is_candidate = |t: &Transition| match covering {
            Some(i) => {
                let x = Some(self.items[i].id);
                t.incoming == x || t.outgoing == x
            }
            None => {
                let prev = at_or_before.map(|i| self.items[i].id);
                let next = self
                    .items
                    .get(at_or_before.map_or(0, |i| i + 1))
                    .map(|i| i.id);
                (t.incoming.is_none() && prev.is_some() && t.outgoing == prev)
                    || (t.outgoing.is_none() && next.is_some() && t.incoming == next)
            }
        };
        let base = covering.or(at_or_before).unwrap_or(0);
        let near = Near(
            (base.saturating_sub(1)..=base + 1)
                .filter_map(|i| self.items.get(i).map(|it| (it.id, i)))
                .collect(),
        );
        for t in self.transitions.iter().filter(|t| is_candidate(t)) {
            let cut = resolve(self, &near, t.outgoing, t.incoming)?;
            let (before, after) = t.alignment.split(t.duration, grid)?;
            let span = cut.span(before, after)?;
            if span.contains(time)? {
                let progress = time
                    .checked_sub(span.start)?
                    .mul_div(Time::seconds(1), t.duration)?
                    .as_secs_f64();
                return Ok(Some(Segment::Transition(TransitionAt {
                    transition: t,
                    outgoing: cut.outgoing,
                    incoming: cut.incoming,
                    span,
                    progress,
                })));
            }
        }
        Ok(covering.map(|i| Segment::Item(&self.items[i])))
    }
}

impl Sequence {
    /// [`Track::segment_at`] for one of this sequence's tracks, on its grid.
    pub fn segment_at(&self, track: TrackId, time: Time) -> Result<Option<Segment<'_>>, EditError> {
        let track = self.track(track).ok_or(EditError::UnknownTrack(track))?;
        track.segment_at(self.grid(track.kind)?, time)
    }
}

impl Project {
    /// The sequence and track holding `transition`, and the transition itself.
    pub fn find_transition(
        &self,
        transition: TransitionId,
    ) -> Option<(&Arc<Sequence>, &Arc<Track>, &Arc<Transition>)> {
        self.sequences.iter().find_map(|s| {
            s.tracks
                .iter()
                .find_map(|t| t.transition(transition).map(|x| (s, t, x)))
        })
    }

    /// The span `transition` covers on its track.
    pub fn transition_span(&self, transition: TransitionId) -> Result<TimeRange, EditError> {
        let (seq, track, t) = self
            .find_transition(transition)
            .ok_or(EditError::UnknownTransition(transition))?;
        track.transition_span(seq.grid(track.kind)?, t)
    }

    /// The source times `transition` shows of its clips' media.
    pub fn transition_needs(&self, transition: TransitionId) -> Result<HandleNeeds, EditError> {
        let (seq, track, t) = self
            .find_transition(transition)
            .ok_or(EditError::UnknownTransition(transition))?;
        needs(track, seq.grid(track.kind)?, t)
    }

    /// Checks the outgoing clip's tail handle against its media's length, where `length`
    /// knows it (the media pool's facts; `None` skips the check, as for stills). The head
    /// handle is always checked by the edits. Fails with [`EditError::TransitionHandle`],
    /// in timeline time: the part after the cut against the media left past the clip's
    /// end over its speed.
    pub fn check_transition_media(
        &self,
        transition: TransitionId,
        length: impl Fn(MediaId) -> Option<Time>,
    ) -> Result<(), EditError> {
        let (seq, track, t) = self
            .find_transition(transition)
            .ok_or(EditError::UnknownTransition(transition))?;
        let grid = seq.grid(track.kind)?;
        let cut = resolve(track, &Scan, t.outgoing, t.incoming)?;
        let (_, after) = t.alignment.split(t.duration, grid)?;
        let Some(item) = cut.outgoing else {
            return Ok(());
        };
        if !after.is_positive() {
            return Ok(());
        }
        let clip = item.clip().ok_or(EditError::NotAClip(item.id))?;
        let Some(len) = length(clip.media) else {
            return Ok(());
        };
        let speed = clip.speed.speed()?;
        let end = clip.source_range(item.range)?.end()?;
        let available = len
            .checked_sub(end)?
            .mul_ratio(i64::from(speed.den), i64::from(speed.num))?;
        if after > available {
            return Err(EditError::TransitionHandle {
                item: item.id,
                needed: after,
                available,
            });
        }
        Ok(())
    }

    /// Adds a transition on the cut `spec` names. Error order: argument checks
    /// ([`EditError::TransitionNeedsClip`], [`EditError::InvalidTransitionKind`],
    /// [`EditError::InvalidEasing`], [`EditError::EmptyRange`] for a duration ≤ 0), then
    /// the clips ([`EditError::UnknownItem`], [`EditError::NotAClip`], outgoing first;
    /// [`EditError::NotAdjacent`] for clips on different tracks or not meeting;
    /// [`EditError::CutNotFree`]), then [`EditError::TransitionExists`] for an edge that
    /// already has one, [`EditError::OffGrid`] for the duration, and the fit
    /// ([`EditError::TransitionRoom`] before the cut, [`EditError::TransitionHandle`],
    /// [`EditError::TransitionRoom`] after the cut).
    pub fn add_transition(
        &self,
        spec: TransitionSpec,
    ) -> Result<(Project, TransitionId), EditError> {
        if spec.outgoing.is_none() && spec.incoming.is_none() {
            return Err(EditError::TransitionNeedsClip);
        }
        check_kind(&spec.kind)?;
        check_easing(&spec.easing)?;
        if !spec.duration.is_positive() {
            return Err(EditError::EmptyRange(spec.duration));
        }
        let mut located = Vec::with_capacity(2);
        for id in [spec.outgoing, spec.incoming].into_iter().flatten() {
            let (si, ti, ii) = self.locate_item(id)?;
            located.push((si, ti, ii));
        }
        for &(si, ti, ii) in &located {
            let item = &self.sequences[si].tracks[ti].items[ii];
            if item.clip().is_none() {
                return Err(EditError::NotAClip(item.id));
            }
        }
        let (si, ti, _) = located[0];
        if let (Some(o), Some(i)) = (spec.outgoing, spec.incoming)
            && (located[1].0, located[1].1) != (si, ti)
        {
            return Err(EditError::NotAdjacent {
                outgoing: o,
                incoming: i,
            });
        }
        let seq = &self.sequences[si];
        let track = &seq.tracks[ti];
        resolve(track, &Scan, spec.outgoing, spec.incoming)?;
        if let Some(t) = track.transitions.iter().find(|t| {
            spec.outgoing.is_some() && t.outgoing == spec.outgoing
                || spec.incoming.is_some() && t.incoming == spec.incoming
        }) {
            return Err(EditError::TransitionExists(t.id));
        }
        let grid = seq.grid(track.kind)?;
        let mut next = self.clone();
        let id = TransitionId(next.alloc_id());
        let transition = Transition {
            id,
            outgoing: spec.outgoing,
            incoming: spec.incoming,
            kind: spec.kind,
            duration: spec.duration,
            alignment: spec.alignment,
            easing: spec.easing,
            params: spec.params,
        };
        check_fits(track, grid, &transition)?;
        let (_, track) = next.track_at_mut(si, ti);
        track.transitions.push_back(Arc::new(transition));
        Ok((next, id))
    }

    /// Removes a transition: the cut becomes a plain cut again.
    pub fn remove_transition(&self, transition: TransitionId) -> Result<Project, EditError> {
        let (si, ti, xi) = self.locate_transition(transition)?;
        let mut next = self.clone();
        next.track_at_mut(si, ti).1.transitions.remove(xi);
        Ok(next)
    }

    /// Changes how long a transition lasts. Error order: [`EditError::EmptyRange`],
    /// [`EditError::UnknownTransition`], [`EditError::OffGrid`], then the fit as for
    /// [`Project::add_transition`]. Nothing is clamped.
    pub fn set_transition_duration(
        &self,
        transition: TransitionId,
        duration: Time,
    ) -> Result<Project, EditError> {
        if !duration.is_positive() {
            return Err(EditError::EmptyRange(duration));
        }
        self.change_transition(transition, true, |t| {
            t.duration = duration;
            Ok(())
        })
    }

    /// Places a transition centred on, starting at or ending at its cut. Error order:
    /// [`EditError::UnknownTransition`], then the fit.
    pub fn set_transition_alignment(
        &self,
        transition: TransitionId,
        alignment: Alignment,
    ) -> Result<Project, EditError> {
        self.change_transition(transition, true, |t| {
            t.alignment = alignment;
            Ok(())
        })
    }

    /// Changes which transition plays. The parameter values belonged to the old kind, so
    /// they are cleared (every parameter of the new kind at its default). Error order:
    /// [`EditError::InvalidTransitionKind`], [`EditError::UnknownTransition`].
    pub fn set_transition_kind(
        &self,
        transition: TransitionId,
        kind: &str,
    ) -> Result<Project, EditError> {
        check_kind(kind)?;
        self.change_transition(transition, false, |t| {
            if t.kind != kind {
                t.kind = kind.to_owned();
                t.params = ParamValues::new();
            }
            Ok(())
        })
    }

    /// Changes the curve a transition's progress follows. Error order:
    /// [`EditError::InvalidEasing`], [`EditError::UnknownTransition`].
    pub fn set_transition_easing(
        &self,
        transition: TransitionId,
        easing: &str,
    ) -> Result<Project, EditError> {
        check_easing(easing)?;
        self.change_transition(transition, false, |t| {
            easing.clone_into(&mut t.easing);
            Ok(())
        })
    }

    /// Sets one parameter of a transition, made valid by [`ParamValues::set`]. `schema`
    /// must be the transition kind's own (its id equal to the kind). Error order:
    /// [`EditError::UnknownTransition`], [`EditError::TransitionSchemaMismatch`],
    /// [`EditError::UnknownParam`].
    pub fn set_transition_param(
        &self,
        transition: TransitionId,
        schema: &ParamSchema,
        param: &str,
        value: ParamValue,
    ) -> Result<Project, EditError> {
        self.change_transition(transition, false, |t| {
            if t.kind != schema.id {
                return Err(EditError::TransitionSchemaMismatch {
                    transition,
                    expected: t.kind.clone(),
                    found: schema.id.clone(),
                });
            }
            t.params
                .set(schema, param, value)
                .ok_or_else(|| EditError::UnknownParam {
                    schema: schema.id.clone(),
                    param: param.to_owned(),
                })?;
            Ok(())
        })
    }

    pub(crate) fn locate_transition(
        &self,
        transition: TransitionId,
    ) -> Result<(usize, usize, usize), EditError> {
        self.sequences
            .iter()
            .enumerate()
            .find_map(|(si, s)| {
                s.tracks.iter().enumerate().find_map(|(ti, t)| {
                    t.transitions
                        .iter()
                        .position(|x| x.id == transition)
                        .map(|xi| (si, ti, xi))
                })
            })
            .ok_or(EditError::UnknownTransition(transition))
    }

    /// Applies `change` to a copy of `transition`; when `refit`, checks the result still
    /// fits. A change that leaves it equal returns this root.
    fn change_transition(
        &self,
        transition: TransitionId,
        refit: bool,
        change: impl FnOnce(&mut Transition) -> Result<(), EditError>,
    ) -> Result<Project, EditError> {
        let (si, ti, xi) = self.locate_transition(transition)?;
        let seq = &self.sequences[si];
        let track = &seq.tracks[ti];
        let old = &track.transitions[xi];
        let mut changed = Transition::clone(old);
        change(&mut changed)?;
        if changed == **old {
            return Ok(self.clone());
        }
        if refit {
            check_fits(track, seq.grid(track.kind)?, &changed)?;
        }
        let mut next = self.clone();
        next.track_at_mut(si, ti).1.transitions[xi] = Arc::new(changed);
        Ok(next)
    }
}

/// See [`Project::transition_needs`].
pub(crate) fn needs(
    track: &Track,
    grid: Time,
    transition: &Transition,
) -> Result<HandleNeeds, EditError> {
    let cut = resolve(track, &Scan, transition.outgoing, transition.incoming)?;
    let (before, after) = transition.alignment.split(transition.duration, grid)?;
    let outgoing = match cut.outgoing {
        Some(item) => {
            let clip = item.clip().ok_or(EditError::NotAClip(item.id))?;
            let end = clip.source_range(item.range)?.end()?;
            let extra = clip.speed.source_duration(after)?;
            Some((item.id, clip.media, end.checked_add(extra)?))
        }
        None => None,
    };
    let incoming = match cut.incoming {
        Some(item) => {
            let clip = item.clip().ok_or(EditError::NotAClip(item.id))?;
            let extra = clip.speed.source_duration(before)?;
            Some((item.id, clip.media, clip.source_start.checked_sub(extra)?))
        }
        None => None,
    };
    Ok(HandleNeeds { outgoing, incoming })
}
