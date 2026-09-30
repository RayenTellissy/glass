//! Whole-tree invariants, checked when a tree comes from outside the edit operations
//! (loading a file, recovering an autosave).

use std::collections::HashSet;

use crate::edit::check_range;
use crate::error::EditError;
use crate::id::{NodeId, TrackId};
use crate::model::{ItemKind, Project};

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
    /// Checks every invariant the edit operations maintain: unique allocated ids, valid
    /// rates, items on their track's grid, sorted and not overlapping, clips pointing at
    /// known media.
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
        for (id, media) in self.media.0.iter() {
            if *id != media.id {
                return Err(ValidationError::Structure(format!(
                    "media {} stored under {id}",
                    media.id
                )));
            }
            claim(media.id.node())?;
        }
        for seq in &self.sequences {
            claim(seq.id.node())?;
            if !seq.frame_rate.is_positive_rate() {
                return Err(EditError::InvalidFrameRate {
                    num: seq.frame_rate.num,
                    den: seq.frame_rate.den,
                }
                .into());
            }
            if seq.sample_rate == 0 {
                return Err(EditError::InvalidSampleRate(seq.sample_rate).into());
            }
            for track in &seq.tracks {
                claim(track.id.node())?;
                let in_track = |error: EditError| ValidationError::Track {
                    track: track.id,
                    error,
                };
                let grid = seq.grid(track.kind).map_err(|e| in_track(e.into()))?;
                let mut previous: Option<&crate::model::Item> = None;
                for item in &track.items {
                    claim(item.id.node())?;
                    check_range(grid, track.kind, item.range).map_err(in_track)?;
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
                            if self.media.get(clip.media).is_none() {
                                return Err(in_track(EditError::UnknownMedia(clip.media)));
                            }
                        }
                    }
                    previous = Some(item);
                }
            }
        }
        Ok(())
    }
}
