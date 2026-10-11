//! Nested sequences and compound clips (schema v8).
//!
//! A clip's source ([`ClipSource`]) is a media file or another sequence of the project. A
//! clip of a sequence (a nested clip) shows that sequence's timeline: the clip's source
//! time is the nested sequence's timeline time, so `source_start`, the speed, trims, slips,
//! clip markers and keyframes work exactly as for media, and the nested sequence's length
//! ([`Project::sequence_duration`], the end of its last item) plays the part of the
//! media's length. The two sequences keep their own formats: when their frame rates
//! differ, a parent frame shows the nested sequence's frame nearest to its source time
//! ([`Sequence::nearest_frame`], halves to the later frame); past the nested sequence's
//! end a clip shows nothing. A nested clip on a video track shows the nested sequence's
//! picture, on an audio track its sound.
//!
//! A compound clip is a nested clip whose sequence is marked
//! [`Sequence::compound`]: made from a selection by [`Project::nest_items`], hidden from
//! the browser's sequence list ([`Project::browser_sequences`]) and normally nested by
//! that one clip. [`Project::break_apart`] puts a nested sequence's contents back in the
//! parent, [`Project::duplicate_compound`] gives a clip its own copy of a compound shared
//! with other clips (after copy and paste), and [`Project::set_clip_source`] points any
//! clip at other media or another sequence.
//!
//! A sequence never contains itself, directly or through other sequences: edits refuse
//! it with [`EditError::NestingCycle`] and [`Project::validate`] refuses such a file. A
//! sequence that a clip nests cannot be deleted ([`EditError::SequenceInUse`]). A compound
//! that no clip nests any more (its clip was deleted) stays in the project, hidden, so the
//! clip can come back; only breaking it apart removes it.
//!
//! Relinking never touches nested clips (they have no media), and the clip counts the
//! media pool shows go through nested sequences ([`Project::clip_count`]).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use imbl::Vector;

use crate::edit::place;
use crate::error::EditError;
use crate::id::{EffectId, ItemId, MarkerId, SequenceId, TrackId, TransitionId};
use crate::marker::Marker;
use crate::model::{Clip, ClipSource, Item, ItemKind, Project, Sequence, Track, TrackKind};
use crate::time::{Rational, Time, TimeRange};
use crate::transition::{Transition, fit_track};

impl Sequence {
    /// The frame of this sequence nearest to `time` (halves to the later frame): the frame
    /// a clip nesting this sequence shows at source time `time` when the two sequences'
    /// frame rates differ.
    pub fn nearest_frame(&self, time: Time) -> Result<i64, EditError> {
        Ok(time.round_to_multiple(self.frame_rate.frame_duration()?)?)
    }

    /// The sequences this one's clips nest, directly, in track and time order (repeats
    /// included).
    pub fn nested(&self) -> impl Iterator<Item = SequenceId> + '_ {
        self.tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .filter_map(|i| i.clip()?.source.sequence())
    }
}

impl Project {
    /// The sequences the browser lists: every sequence but compound clips' contents.
    pub fn browser_sequences(&self) -> impl Iterator<Item = &Arc<Sequence>> {
        self.sequences.iter().filter(|s| !s.compound)
    }

    /// A sequence's length: the end of its last item on any track, zero when empty. This
    /// is the source length of a clip nesting it.
    pub fn sequence_duration(&self, sequence: SequenceId) -> Result<Time, EditError> {
        let seq = self
            .sequence(sequence)
            .ok_or(EditError::UnknownSequence(sequence))?;
        let mut end = Time::ZERO;
        for track in &seq.tracks {
            end = end.max(track.end()?);
        }
        Ok(end)
    }

    /// Every clip nesting `sequence`: its sequence and item, in sequence, track and time
    /// order.
    pub fn sequence_users(&self, sequence: SequenceId) -> Vec<(SequenceId, ItemId)> {
        let mut out = Vec::new();
        for seq in &self.sequences {
            for track in &seq.tracks {
                for item in &track.items {
                    if item
                        .clip()
                        .is_some_and(|c| c.source == ClipSource::Sequence(sequence))
                    {
                        out.push((seq.id, item.id));
                    }
                }
            }
        }
        out
    }

    /// True when `outer` is `inner` or nests it, directly or through other sequences.
    pub fn contains_sequence(&self, outer: SequenceId, inner: SequenceId) -> bool {
        let mut stack = vec![outer];
        let mut seen = HashSet::new();
        while let Some(s) = stack.pop() {
            if s == inner {
                return true;
            }
            if !seen.insert(s) {
                continue;
            }
            if let Some(seq) = self.sequence(s) {
                stack.extend(seq.nested());
            }
        }
        false
    }

    /// Refuses nesting `child` in `parent` when `child` contains `parent`.
    fn check_no_cycle(&self, parent: SequenceId, child: SequenceId) -> Result<(), EditError> {
        if self.contains_sequence(child, parent) {
            return Err(EditError::NestingCycle { parent, child });
        }
        Ok(())
    }

    /// Places a clip of `sequence` on a video or audio track, showing it from
    /// `source_start` (its timeline time) with every property at its default. Error
    /// order: [`EditError::UnknownSequence`], [`EditError::SourceBeforeStart`],
    /// [`EditError::UnknownTrack`], [`EditError::NestingCycle`] (the track's own sequence,
    /// or one nesting it), then the placement as for [`Project::insert_clip`].
    pub fn insert_sequence_clip(
        &self,
        track: TrackId,
        sequence: SequenceId,
        range: TimeRange,
        source_start: Time,
    ) -> Result<(Project, ItemId), EditError> {
        if self.sequence(sequence).is_none() {
            return Err(EditError::UnknownSequence(sequence));
        }
        if source_start.is_negative() {
            return Err(EditError::SourceBeforeStart(source_start));
        }
        let (si, _) = self.locate_track(track)?;
        self.check_no_cycle(self.sequences[si].id, sequence)?;
        let clip = Clip::new(sequence, source_start);
        self.insert_item(track, range, ItemKind::Clip(clip))
    }

    /// Points a clip at other media or another sequence, keeping its range, source start,
    /// speed and every property. Error order: [`EditError::UnknownItem`],
    /// [`EditError::NotAClip`], [`EditError::UnknownMedia`] or
    /// [`EditError::UnknownSequence`], [`EditError::NestingCycle`]. The same source returns
    /// this root.
    pub fn set_clip_source(&self, item: ItemId, source: ClipSource) -> Result<Project, EditError> {
        let (si, ti, ii) = self.locate_item(item)?;
        if self.sequences[si].tracks[ti].items[ii].clip().is_none() {
            return Err(EditError::NotAClip(item));
        }
        match source {
            ClipSource::Media(media) => {
                if self.media.get(media).is_none() {
                    return Err(EditError::UnknownMedia(media));
                }
            }
            ClipSource::Sequence(sequence) => {
                if self.sequence(sequence).is_none() {
                    return Err(EditError::UnknownSequence(sequence));
                }
                self.check_no_cycle(self.sequences[si].id, sequence)?;
            }
        }
        self.change_clip(item, |clip| {
            clip.source = source;
            Ok(())
        })
    }

    /// Nests the selected items into a new compound clip. The items (listed in any order,
    /// repeats ignored) must all be in one sequence and hold either sound (only audio
    /// tracks) or picture (video tracks, with subtitle and adjustment tracks beside them).
    /// They move, keeping their ids and their places relative to the selection's start,
    /// into a new sequence marked compound, named `name`, in the parent's format, with the
    /// parent's track layout (a track of the same kind and name for each of its tracks,
    /// top to bottom). Transitions whose clips are all selected move with them (fitted
    /// in their new tracks as any edit fits them); transitions joining a selected clip to
    /// one left behind are removed. One clip of the new sequence, from source time 0 at
    /// speed 1, then covers the selection's span, from the earliest start to the latest
    /// end, on the topmost selected video track (audio track for sound). Ids come from the
    /// counter in this order: the sequence, its tracks top to bottom, the clip.
    ///
    /// Error order: [`EditError::NothingToNest`], [`EditError::UnknownItem`],
    /// [`EditError::NestAcrossSequences`], [`EditError::NestMixedKinds`],
    /// [`EditError::NestNeedsClipTrack`], then placing the clip on its track
    /// ([`EditError::OffGrid`] when the span is off the track's grid,
    /// [`EditError::Overlap`] when an item left on that track lies within it).
    pub fn nest_items(
        &self,
        items: &[ItemId],
        name: impl Into<String>,
    ) -> Result<(Project, SequenceId, ItemId), EditError> {
        if items.is_empty() {
            return Err(EditError::NothingToNest);
        }
        let mut located = Vec::with_capacity(items.len());
        for &id in items {
            located.push((id, self.locate_item(id)?));
        }
        let si = located[0].1.0;
        if let Some((id, _)) = located.iter().find(|(_, (s, _, _))| *s != si) {
            return Err(EditError::NestAcrossSequences(*id));
        }
        let parent = &self.sequences[si];
        let selected: HashSet<ItemId> = items.iter().copied().collect();
        let kind_of = |ti: usize| parent.tracks[ti].kind;
        let sound = located
            .iter()
            .any(|(_, (_, ti, _))| kind_of(*ti) == TrackKind::Audio);
        let picture = located
            .iter()
            .any(|(_, (_, ti, _))| kind_of(*ti) != TrackKind::Audio);
        if sound && picture {
            return Err(EditError::NestMixedKinds);
        }
        let host_kind = if sound {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        let host = located
            .iter()
            .map(|(_, (_, ti, _))| *ti)
            .filter(|&ti| kind_of(ti) == host_kind)
            .min()
            .ok_or(EditError::NestNeedsClipTrack)?;
        let mut start: Option<Time> = None;
        let mut end: Option<Time> = None;
        for (_, (_, ti, ii)) in &located {
            let range = parent.tracks[*ti].items[*ii].range;
            start = Some(start.map_or(range.start, |s| s.min(range.start)));
            let e = range.end()?;
            end = Some(end.map_or(e, |x| x.max(e)));
        }
        let (Some(start), Some(end)) = (start, end) else {
            return Err(EditError::NothingToNest);
        };

        let mut next = self.clone();
        let child_id = SequenceId(next.alloc_id());
        let mut child = Sequence {
            id: child_id,
            name: name.into(),
            compound: true,
            markers: Vector::new(),
            tracks: Vector::new(),
            ..Sequence::clone(parent)
        };
        for track in &parent.tracks {
            let id = TrackId(next.alloc_id());
            let grid = parent.grid(track.kind)?;
            let mut items = Vector::new();
            for item in track.items.iter().filter(|i| selected.contains(&i.id)) {
                let mut moved = Item::clone(item);
                moved.range.start = item.range.start.checked_sub(start)?;
                items.push_back(Arc::new(moved));
            }
            let transitions = track
                .transitions
                .iter()
                .filter(|t| {
                    t.outgoing
                        .iter()
                        .chain(t.incoming.iter())
                        .all(|i| selected.contains(i))
                })
                .cloned()
                .collect();
            let mut inner = Track {
                items,
                transitions,
                ..Track::new(id, track.kind, track.name.clone())
            };
            fit_track(&mut inner, grid);
            child.tracks.push_back(Arc::new(inner));
        }
        let clip_id = ItemId(next.alloc_id());
        let touched: HashSet<usize> = located.iter().map(|(_, (_, ti, _))| *ti).collect();
        let parent = next.sequence_at_mut(si);
        for &ti in &touched {
            let grid = parent.grid(parent.tracks[ti].kind)?;
            let track = Arc::make_mut(&mut parent.tracks[ti]);
            track.items.retain(|i| !selected.contains(&i.id));
            fit_track(track, grid);
        }
        let grid = parent.grid(host_kind)?;
        let track = Arc::make_mut(&mut parent.tracks[host]);
        let clip = Item {
            id: clip_id,
            range: TimeRange::between(start, end)?,
            kind: ItemKind::Clip(Clip::new(child_id, Time::ZERO)),
        };
        place(grid, track, Arc::new(clip))?;
        fit_track(track, grid);
        next.sequences.push_back(Arc::new(child));
        Ok((self.unlocked(next)?, child_id, clip_id))
    }

    /// Puts the contents of the sequence a clip nests back in the clip's sequence, in
    /// place of the clip, and removes the clip. What the clip shows (its source range)
    /// comes out at the same timeline times: items crossing its edges are trimmed to them
    /// (a clip's source start follows its head trim), items outside it are left out, and
    /// transitions come along when all their clips do (then fitted as any edit fits
    /// them). The n-th track of each kind in the nested sequence goes to the n-th track
    /// of that kind in the clip's sequence, top to bottom. The clip's own properties
    /// (effects, transform, opacity, keyframes, markers) are dropped with it.
    ///
    /// A compound nested by no other clip moves out, keeping its node ids, and is removed
    /// from the project, so nesting a selection and breaking the result apart gives back
    /// the same tree (with a higher id counter). Any other nested sequence stays and its
    /// contents are copied with new ids (compounds inside them copied too, so every copy
    /// owns its compounds).
    ///
    /// Error order: [`EditError::UnknownItem`], [`EditError::NotAClip`],
    /// [`EditError::NotNested`], [`EditError::BreakApartSpeed`] (the clip must play at
    /// speed 1), [`EditError::BreakApartNoTrack`], then placing each item
    /// ([`EditError::OffGrid`], [`EditError::Overlap`]), tracks top to bottom, items in
    /// time order.
    pub fn break_apart(&self, item: ItemId) -> Result<Project, EditError> {
        let (si, ti, ii) = self.locate_item(item)?;
        let parent = &self.sequences[si];
        let node = &parent.tracks[ti].items[ii];
        let clip = node.clip().ok_or(EditError::NotAClip(item))?;
        let child_id = clip.source.sequence().ok_or(EditError::NotNested(item))?;
        let speed = clip.speed.speed()?;
        if speed != Rational::ONE {
            return Err(EditError::BreakApartSpeed { item, speed });
        }
        let child = self
            .sequence(child_id)
            .ok_or(EditError::UnknownSequence(child_id))?
            .clone();
        // Child time t shows at parent time t - from + at.
        let from = clip.source_start;
        let until = from.checked_add(node.range.duration)?;
        let at = node.range.start;
        let moving = child.compound && self.sequence_users(child_id) == [(parent.id, item)];

        // The parent track each child track with content goes to.
        let mut targets = Vec::with_capacity(child.tracks.len());
        let mut seen: HashMap<TrackKind, usize> = HashMap::new();
        for track in &child.tracks {
            let n = seen.entry(track.kind).or_default();
            let index = *n;
            *n += 1;
            if track.items.is_empty() {
                targets.push(None);
                continue;
            }
            let target = parent
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.kind == track.kind)
                .nth(index)
                .map(|(i, _)| i)
                .ok_or(EditError::BreakApartNoTrack {
                    kind: track.kind,
                    index,
                })?;
            targets.push(Some(target));
        }

        let mut next = self.clone();
        let mut touched = vec![ti];
        Arc::make_mut(&mut next.sequence_at_mut(si).tracks[ti])
            .items
            .remove(ii);
        for (track, target) in child.tracks.iter().zip(targets) {
            let Some(target) = target else { continue };
            let mut ids: HashMap<ItemId, ItemId> = HashMap::new();
            let mut placed = Vec::new();
            for old in &track.items {
                let old_end = old.range.end()?;
                if old_end <= from || old.range.start >= until {
                    continue;
                }
                let mut out = Item::clone(old);
                let head = from.checked_sub(old.range.start)?.max(Time::ZERO);
                if head.is_positive()
                    && let ItemKind::Clip(c) = &mut out.kind
                {
                    c.source_start = c.source_start.checked_add(c.speed.source_duration(head)?)?;
                }
                let start = old.range.start.max(from);
                let end = old_end.min(until);
                out.range = TimeRange::between(
                    start.checked_sub(from)?.checked_add(at)?,
                    end.checked_sub(from)?.checked_add(at)?,
                )?;
                if !moving {
                    out = copy_item(&mut next, &out)?;
                }
                ids.insert(old.id, out.id);
                placed.push(Arc::new(out));
            }
            let mut transitions = Vec::new();
            for t in &track.transitions {
                let sides = [t.outgoing, t.incoming];
                if sides.iter().flatten().all(|i| ids.contains_key(i)) {
                    let mut out = Transition::clone(t);
                    out.outgoing = t.outgoing.map(|i| ids[&i]);
                    out.incoming = t.incoming.map(|i| ids[&i]);
                    if !moving {
                        out.id = TransitionId(next.alloc_id());
                    }
                    transitions.push(Arc::new(out));
                }
            }
            let seq = next.sequence_at_mut(si);
            let grid = seq.grid(seq.tracks[target].kind)?;
            let dest = Arc::make_mut(&mut seq.tracks[target]);
            for item in placed {
                place(grid, dest, item)?;
            }
            for t in transitions {
                let (Ok(at) | Err(at)) = dest.transitions.binary_search_by(|x| x.id.cmp(&t.id));
                dest.transitions.insert(at, t);
            }
            touched.push(target);
        }
        let seq = next.sequence_at_mut(si);
        touched.sort_unstable();
        touched.dedup();
        for ti in touched {
            let grid = seq.grid(seq.tracks[ti].kind)?;
            fit_track(Arc::make_mut(&mut seq.tracks[ti]), grid);
        }
        if moving {
            next.sequences.retain(|s| s.id != child_id);
        }
        self.unlocked(next)
    }

    /// Gives a compound clip its own copy of its compound, so editing one no longer
    /// changes the other clips that shared it. Everything in the copy gets new ids from the
    /// counter (the sequence first, then each track with its items and their effects and
    /// markers, its transitions, then the sequence markers); compounds nested inside are
    /// copied too, other nested sequences are shared. The copy is appended to the
    /// project's sequences; the old compound stays, even when no clip nests it any more.
    /// Error order: [`EditError::UnknownItem`], [`EditError::NotAClip`],
    /// [`EditError::NotNested`], [`EditError::NotACompound`].
    pub fn duplicate_compound(&self, item: ItemId) -> Result<(Project, SequenceId), EditError> {
        let (si, ti, ii) = self.locate_item(item)?;
        let node = &self.sequences[si].tracks[ti].items[ii];
        let clip = node.clip().ok_or(EditError::NotAClip(item))?;
        let compound = clip.source.sequence().ok_or(EditError::NotNested(item))?;
        if !self.sequence(compound).is_some_and(|s| s.compound) {
            return Err(EditError::NotACompound(item));
        }
        let mut next = self.clone();
        let copy = copy_sequence(&mut next, compound)?;
        let (_, track) = next.track_at_mut(si, ti);
        if let ItemKind::Clip(c) = &mut Arc::make_mut(&mut track.items[ii]).kind {
            c.source = ClipSource::Sequence(copy);
        }
        Ok((self.unlocked(next)?, copy))
    }

    /// Deletes a sequence and everything in it. Refused with
    /// [`EditError::SequenceInUse`] (naming the first nesting clip in sequence, track and
    /// time order) while a clip nests it; compounds its own clips nest stay.
    pub fn remove_sequence(&self, sequence: SequenceId) -> Result<Project, EditError> {
        let index = self
            .sequences
            .iter()
            .position(|s| s.id == sequence)
            .ok_or(EditError::UnknownSequence(sequence))?;
        if let Some(&(_, item)) = self.sequence_users(sequence).first() {
            return Err(EditError::SequenceInUse { sequence, item });
        }
        let mut next = self.clone();
        next.sequences.remove(index);
        Ok(next)
    }

    /// The checks [`Project::validate`] runs on nesting: every nested sequence exists and
    /// no sequence contains itself.
    pub(crate) fn check_nesting(&self) -> Result<(), EditError> {
        let index: HashMap<SequenceId, usize> = self
            .sequences
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, i))
            .collect();
        let mut children = Vec::with_capacity(self.sequences.len());
        for seq in &self.sequences {
            let mut out = Vec::new();
            for child in seq.nested() {
                out.push(*index.get(&child).ok_or(EditError::UnknownSequence(child))?);
            }
            children.push(out);
        }
        // Depth first: 1 on the current path, 2 done.
        let mut state = vec![0u8; children.len()];
        for root in 0..children.len() {
            if state[root] != 0 {
                continue;
            }
            let mut stack = vec![(root, 0usize)];
            state[root] = 1;
            while let Some((node, next_child)) = stack.last_mut() {
                let node = *node;
                match children[node].get(*next_child) {
                    Some(&child) => {
                        *next_child += 1;
                        match state[child] {
                            0 => {
                                state[child] = 1;
                                stack.push((child, 0));
                            }
                            1 => {
                                return Err(EditError::NestingCycle {
                                    parent: self.sequences[node].id,
                                    child: self.sequences[child].id,
                                });
                            }
                            _ => {}
                        }
                    }
                    None => {
                        state[node] = 2;
                        stack.pop();
                    }
                }
            }
        }
        Ok(())
    }
}

/// A copy of `item` with new ids for it, its effects and its markers; a clip of a compound
/// gets its own copy of the compound ([`copy_sequence`]).
fn copy_item(next: &mut Project, item: &Item) -> Result<Item, EditError> {
    let mut out = item.clone();
    out.id = ItemId(next.alloc_id());
    if let ItemKind::Clip(clip) = &mut out.kind {
        for effect in clip.effects.iter_mut() {
            Arc::make_mut(effect).id = EffectId(next.alloc_id());
        }
        for marker in clip.markers.iter_mut() {
            Arc::make_mut(marker).id = MarkerId(next.alloc_id());
        }
        if let ClipSource::Sequence(s) = clip.source
            && next.sequence(s).is_some_and(|s| s.compound)
        {
            clip.source = ClipSource::Sequence(copy_sequence(next, s)?);
        }
    }
    Ok(out)
}

/// Appends a copy of sequence `id` with new ids throughout (see
/// [`Project::duplicate_compound`]) and returns the copy's id.
fn copy_sequence(next: &mut Project, id: SequenceId) -> Result<SequenceId, EditError> {
    let source = next
        .sequence(id)
        .cloned()
        .ok_or(EditError::UnknownSequence(id))?;
    let copy_id = SequenceId(next.alloc_id());
    let mut copy = Sequence {
        id: copy_id,
        markers: Vector::new(),
        tracks: Vector::new(),
        ..Sequence::clone(&source)
    };
    for track in &source.tracks {
        let track_id = TrackId(next.alloc_id());
        let mut ids = HashMap::new();
        let mut items = Vector::new();
        for item in &track.items {
            let out = copy_item(next, item)?;
            ids.insert(item.id, out.id);
            items.push_back(Arc::new(out));
        }
        let mut transitions = Vector::new();
        for t in &track.transitions {
            let mut out = Transition::clone(t);
            out.id = TransitionId(next.alloc_id());
            out.outgoing = t.outgoing.and_then(|i| ids.get(&i).copied());
            out.incoming = t.incoming.and_then(|i| ids.get(&i).copied());
            transitions.push_back(Arc::new(out));
        }
        copy.tracks.push_back(Arc::new(Track {
            id: track_id,
            items,
            transitions,
            ..Track::clone(track)
        }));
    }
    for marker in &source.markers {
        let mut out = Marker::clone(marker);
        out.id = MarkerId(next.alloc_id());
        copy.markers.push_back(Arc::new(out));
    }
    next.sequences.push_back(Arc::new(copy));
    Ok(copy_id)
}
