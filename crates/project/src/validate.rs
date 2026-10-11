//! Whole-tree invariants, checked when a tree comes from outside the edit operations
//! (loading a file, recovering an autosave).

use std::collections::HashSet;

use std::sync::Arc;

use imbl::Vector;

use crate::clip::check_clip;
use crate::edit::check_item;
use crate::error::EditError;
use crate::id::{NodeId, TrackId};
use crate::marker::{Marker, check_marker, is_sorted};
use crate::model::{ClipSource, ItemKind, Project};
use crate::switches::check_switches;
use crate::time::Time;
use crate::transition::check_track;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("{0}")]
    Structure(String),
    #[error("id {0} is used twice")]
    DuplicateId(NodeId),
    #[error("id {id} was never allocated (next id is {next_id})")]
    Unallocated { id: NodeId, next_id: u64 },
    #[error("track {track}: {error}")]
    Track { track: TrackId, error: EditError },
    #[error(transparent)]
    Edit(#[from] EditError),
}

impl Project {
    /// Checks every invariant the edit operations maintain: unique allocated ids (media
    /// pool bins and items included; the root bin is id 0), a well-formed media pool
    /// ([`crate::media_pool::MediaPool::check`]), valid project settings and sequence
    /// formats ([`crate::settings`]), track switches the track's kind has
    /// ([`crate::switches`]), items of a kind their track accepts, on its grid, sorted
    /// and not overlapping, clips pointing at known media or sequences with their
    /// properties in range (a constant speed map, finite numbers within their limits,
    /// valid effect schema ids,
    /// keyframe curves sorted and finite with valid handles, [`crate::keyframe`]),
    /// markers sorted, not before zero and (on sequences) on the frame grid, and
    /// transitions ordered by id on cuts that resolve, fitting their clips and head
    /// handles without overlapping ([`crate::transition`]), and no sequence nesting
    /// itself, directly or through others ([`crate::nest`]).
    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut ids = HashSet::new();
        let mut claim = |id: NodeId| -> Result<(), ValidationError> {
            if id.0 == 0 || id.0 >= self.next_id {
                return Err(ValidationError::Unallocated {
                    id,
                    next_id: self.next_id,
                });
            }
            if !ids.insert(id) {
                return Err(ValidationError::DuplicateId(id));
            }
            Ok(())
        };
        self.media.check().map_err(ValidationError::Structure)?;
        for bin in self.media.bins() {
            if bin.id != self.media.root() {
                claim(bin.id.node())?;
            }
        }
        for media in self.media.items() {
            claim(media.id.node())?;
        }
        self.settings.check()?;
        for seq in &self.sequences {
            claim(seq.id.node())?;
            seq.format().check()?;
            let frame = seq.frame_rate.frame_duration().map_err(EditError::from)?;
            check_markers(&seq.markers, Some(frame), &mut claim)?;
            for track in &seq.tracks {
                claim(track.id.node())?;
                let in_track = |error: EditError| ValidationError::Track {
                    track: track.id,
                    error,
                };
                check_switches(track).map_err(in_track)?;
                let grid = seq.grid(track.kind).map_err(|e| in_track(e.into()))?;
                let mut previous: Option<&crate::model::Item> = None;
                for item in &track.items {
                    claim(item.id.node())?;
                    check_item(grid, track.kind, item).map_err(in_track)?;
                    if let Some(prev) = previous {
                        let prev_end = prev.range.end().map_err(|e| in_track(e.into()))?;
                        if prev_end > item.range.start {
                            return Err(in_track(EditError::Overlap {
                                track: track.id,
                                other: prev.id,
                            }));
                        }
                    }
                    match &item.kind {
                        ItemKind::Clip(clip) => {
                            match clip.source {
                                ClipSource::Media(media) if self.media.get(media).is_none() => {
                                    return Err(in_track(EditError::UnknownMedia(media)));
                                }
                                ClipSource::Sequence(s) if self.sequence(s).is_none() => {
                                    return Err(in_track(EditError::UnknownSequence(s)));
                                }
                                _ => {}
                            }
                            check_clip(clip).map_err(in_track)?;
                            for effect in &clip.effects {
                                claim(effect.id.node())?;
                            }
                            check_markers(&clip.markers, None, &mut claim)?;
                        }
                        ItemKind::Subtitle(_) | ItemKind::Adjustment(_) => {}
                    }
                    previous = Some(item);
                }
                let mut last = None;
                for transition in &track.transitions {
                    claim(transition.id.node())?;
                    if last.is_some_and(|id| id > transition.id) {
                        return Err(ValidationError::Structure(format!(
                            "transitions on track {} are not ordered by id",
                            track.id
                        )));
                    }
                    last = Some(transition.id);
                }
                check_track(track, grid).map_err(in_track)?;
            }
        }
        self.check_nesting()?;
        Ok(())
    }
}

/// Claims each marker's id and checks its span and the list's order.
fn check_markers(
    markers: &Vector<Arc<Marker>>,
    grid: Option<Time>,
    claim: &mut impl FnMut(NodeId) -> Result<(), ValidationError>,
) -> Result<(), ValidationError> {
    for marker in markers {
        claim(marker.id.node())?;
        check_marker(grid, marker.range())?;
    }
    if !is_sorted(markers) {
        return Err(ValidationError::Structure(
            "markers are not ordered by start".into(),
        ));
    }
    Ok(())
}
