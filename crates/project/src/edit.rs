//! Edit operations: pure functions from a project to a new project.
//!
//! Each operation takes `&self`, validates, and returns a new root (plus the id of any
//! node it created) or an [`EditError`]. The old root is never modified, so it can stay in
//! the undo history, and the new root shares every node the edit did not touch.

use std::sync::Arc;

use crate::error::EditError;
use crate::id::{ItemId, MediaId, SequenceId, TrackId};
use crate::model::{Clip, Item, ItemKind, MediaRef, Project, Sequence, Track, TrackKind};
use crate::time::{Rational, Time, TimeRange};

/// A new clip: which media, where it goes on the timeline, and the source time at its
/// start.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipSpec {
    pub media: MediaId,
    pub range: TimeRange,
    pub source_start: Time,
}

impl Project {
    /// Adds a reference to a source file.
    pub fn add_media(
        &self,
        content_hash: impl Into<String>,
        path: impl Into<String>,
    ) -> Result<(Project, MediaId), EditError> {
        let mut next = self.clone();
        let id = MediaId(next.alloc_id());
        let media = MediaRef {
            id,
            content_hash: content_hash.into(),
            path: path.into(),
        };
        next.media.0.insert(id, Arc::new(media));
        Ok((next, id))
    }

    /// Adds an empty sequence after the existing ones.
    pub fn add_sequence(
        &self,
        name: impl Into<String>,
        frame_rate: Rational,
        sample_rate: u32,
    ) -> Result<(Project, SequenceId), EditError> {
        if !frame_rate.is_positive_rate() {
            return Err(EditError::InvalidFrameRate {
                num: frame_rate.num,
                den: frame_rate.den,
            });
        }
        if sample_rate == 0 {
            return Err(EditError::InvalidSampleRate(sample_rate));
        }
        let mut next = self.clone();
        let id = SequenceId(next.alloc_id());
        next.sequences.push_back(Arc::new(Sequence {
            id,
            name: name.into(),
            frame_rate,
            sample_rate,
            tracks: imbl::Vector::new(),
        }));
        Ok((next, id))
    }

    /// Inserts an empty track at `index` (0 is the top), or at the bottom when `None`.
    pub fn add_track(
        &self,
        sequence: SequenceId,
        kind: TrackKind,
        name: impl Into<String>,
        index: Option<usize>,
    ) -> Result<(Project, TrackId), EditError> {
        let mut next = self.clone();
        let id = TrackId(next.alloc_id());
        let seq = next.sequence_mut(sequence)?;
        let len = seq.tracks.len();
        let index = index.unwrap_or(len);
        if index > len {
            return Err(EditError::TrackIndex { index, len });
        }
        seq.tracks.insert(
            index,
            Arc::new(Track {
                id,
                kind,
                name: name.into(),
                items: imbl::Vector::new(),
            }),
        );
        Ok((next, id))
    }

    /// Removes a track and everything on it.
    pub fn remove_track(&self, track: TrackId) -> Result<Project, EditError> {
        let mut next = self.clone();
        let (si, ti) = next.locate_track(track)?;
        next.sequence_at_mut(si).tracks.remove(ti);
        Ok(next)
    }

    /// Places a clip on a track. Fails if it would overlap another item.
    pub fn insert_clip(
        &self,
        track: TrackId,
        spec: ClipSpec,
    ) -> Result<(Project, ItemId), EditError> {
        if self.media.get(spec.media).is_none() {
            return Err(EditError::UnknownMedia(spec.media));
        }
        if spec.source_start.is_negative() {
            return Err(EditError::SourceBeforeStart(spec.source_start));
        }
        let mut next = self.clone();
        let id = ItemId(next.alloc_id());
        let item = Item {
            id,
            range: spec.range,
            kind: ItemKind::Clip(Clip {
                media: spec.media,
                source_start: spec.source_start,
            }),
        };
        let (si, ti) = next.locate_track(track)?;
        let (seq, track) = next.track_at_mut(si, ti);
        place(seq_grid(seq, track.kind)?, track, Arc::new(item))?;
        Ok((next, id))
    }

    /// Removes an item, leaving empty space.
    pub fn remove_item(&self, item: ItemId) -> Result<Project, EditError> {
        let mut next = self.clone();
        let (si, ti, ii) = next.locate_item(item)?;
        next.track_at_mut(si, ti).1.items.remove(ii);
        Ok(next)
    }

    /// Moves an item to `start` on `to_track` (which may be its own track). The target
    /// track must be in the same sequence and of the same kind.
    pub fn move_item(
        &self,
        item: ItemId,
        to_track: TrackId,
        start: Time,
    ) -> Result<Project, EditError> {
        let mut next = self.clone();
        let (si, ti, ii) = next.locate_item(item)?;
        let (_, from) = next.track_at_mut(si, ti);
        let from_kind = from.kind;
        let mut moved = from.items.remove(ii);
        let dest = next
            .sequence_at_mut(si)
            .track_index(to_track)
            .ok_or(EditError::UnknownTrack(to_track))?;
        let (seq, dest_track) = next.track_at_mut(si, dest);
        if dest_track.kind != from_kind {
            return Err(EditError::TrackKindMismatch {
                from: from_kind,
                to: dest_track.kind,
            });
        }
        Arc::make_mut(&mut moved).range.start = start;
        place(seq_grid(seq, dest_track.kind)?, dest_track, moved)?;
        Ok(next)
    }

    /// Changes an item's timeline range. For a clip, moving the start trims its head:
    /// the source start moves by the same amount, so the frames that stay on the
    /// timeline do not move.
    pub fn trim_item(&self, item: ItemId, range: TimeRange) -> Result<Project, EditError> {
        let mut next = self.clone();
        let (si, ti, ii) = next.locate_item(item)?;
        let (seq, track) = next.track_at_mut(si, ti);
        let grid = seq_grid(seq, track.kind)?;
        let mut trimmed = track.items.remove(ii);
        let node = Arc::make_mut(&mut trimmed);
        let head = range.start.checked_sub(node.range.start)?;
        match &mut node.kind {
            ItemKind::Clip(clip) => {
                let source_start = clip.source_start.checked_add(head)?;
                if source_start.is_negative() {
                    return Err(EditError::SourceBeforeStart(source_start));
                }
                clip.source_start = source_start;
            }
        }
        node.range = range;
        place(grid, track, trimmed)?;
        Ok(next)
    }

    fn sequence_mut(&mut self, id: SequenceId) -> Result<&mut Sequence, EditError> {
        let index = self
            .sequences
            .iter()
            .position(|s| s.id == id)
            .ok_or(EditError::UnknownSequence(id))?;
        Ok(self.sequence_at_mut(index))
    }

    pub(crate) fn sequence_at_mut(&mut self, index: usize) -> &mut Sequence {
        Arc::make_mut(&mut self.sequences[index])
    }

    /// The sequence (without its tracks borrowed) and one of its tracks, both writable.
    pub(crate) fn track_at_mut(&mut self, si: usize, ti: usize) -> (SequenceView, &mut Track) {
        let seq = self.sequence_at_mut(si);
        let view = SequenceView {
            frame_rate: seq.frame_rate,
            sample_rate: seq.sample_rate,
        };
        (view, Arc::make_mut(&mut seq.tracks[ti]))
    }

    pub(crate) fn locate_track(&self, track: TrackId) -> Result<(usize, usize), EditError> {
        self.sequences
            .iter()
            .enumerate()
            .find_map(|(si, s)| s.track_index(track).map(|ti| (si, ti)))
            .ok_or(EditError::UnknownTrack(track))
    }

    pub(crate) fn locate_item(&self, item: ItemId) -> Result<(usize, usize, usize), EditError> {
        self.sequences
            .iter()
            .enumerate()
            .find_map(|(si, s)| {
                s.tracks
                    .iter()
                    .enumerate()
                    .find_map(|(ti, t)| t.item_index(item).map(|ii| (si, ti, ii)))
            })
            .ok_or(EditError::UnknownItem(item))
    }
}

/// The parts of a sequence an edit on one of its tracks needs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SequenceView {
    pub frame_rate: Rational,
    pub sample_rate: u32,
}

fn seq_grid(seq: SequenceView, kind: TrackKind) -> Result<Time, EditError> {
    Ok(match kind {
        TrackKind::Audio => Time::from_samples(1, seq.sample_rate)?,
        TrackKind::Video => seq.frame_rate.frame_duration()?,
    })
}

/// Validates `item` against `track` and inserts it in start order.
pub(crate) fn place(grid: Time, track: &mut Track, item: Arc<Item>) -> Result<(), EditError> {
    check_range(grid, track.kind, item.range)?;
    let index = slot(track, item.range, None)?;
    track.items.insert(index, item);
    Ok(())
}

/// The range is non-empty, starts at or after zero and sits on the track's grid.
pub(crate) fn check_range(grid: Time, kind: TrackKind, range: TimeRange) -> Result<(), EditError> {
    if !range.duration.is_positive() {
        return Err(EditError::EmptyRange(range.duration));
    }
    if range.start.is_negative() {
        return Err(EditError::NegativeStart(range.start));
    }
    for time in [range.start, range.duration] {
        if !time.is_multiple_of(grid) {
            return Err(EditError::OffGrid { time, grid, kind });
        }
    }
    Ok(())
}

/// The index at which an item with `range` keeps the track sorted, or the item it would
/// overlap. `ignore` skips one item (the one being changed in place).
pub(crate) fn slot(
    track: &Track,
    range: TimeRange,
    ignore: Option<ItemId>,
) -> Result<usize, EditError> {
    let index = match track
        .items
        .binary_search_by(|i| i.range.start.cmp(&range.start))
    {
        Ok(i) | Err(i) => i,
    };
    let overlap = |i: usize| -> Result<Option<ItemId>, EditError> {
        match track.items.get(i) {
            Some(other) if Some(other.id) != ignore && other.range.overlaps(range)? => {
                Ok(Some(other.id))
            }
            _ => Ok(None),
        }
    };
    let neighbours = [index.checked_sub(1), Some(index), Some(index + 1)];
    for i in neighbours.into_iter().flatten() {
        if let Some(other) = overlap(i)? {
            return Err(EditError::Overlap {
                track: track.id,
                other,
            });
        }
    }
    Ok(index)
}
