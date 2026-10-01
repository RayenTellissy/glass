//! Relinking: pointing media references at the files they moved to.
//!
//! A relink replaces only [`MediaRef`] nodes (path and content hash). Every sequence,
//! track and clip stays the very same node, so trims, moves and effects made while the
//! media was offline are kept as they are, and one batch is one undo step
//! ([`crate::EditLabel::Relink`]). Finding the new files is the engine's job
//! (`engine::media_mgmt::relink`); this module only validates and applies the choice.

use std::collections::HashSet;
use std::sync::Arc;

use crate::error::EditError;
use crate::id::MediaId;
use crate::model::{ItemKind, MediaRef, Project};
use crate::time::Time;

/// One reference to point at a new file.
#[derive(Clone, Debug, PartialEq)]
pub struct Relink {
    pub media: MediaId,
    pub path: String,
    /// The new file's content hash (hex); it replaces the recorded one, so a later
    /// availability check accepts the new file.
    pub content_hash: String,
    /// The new file's length, `None` for stills and sequences without one. A file shorter
    /// than the source range the clips use is refused.
    pub duration: Option<Time>,
}

impl Project {
    /// The end of the furthest source time any clip of `media` shows: the least length a
    /// file must have to stand in for it. `None` when no clip uses it.
    pub fn source_used_until(&self, media: MediaId) -> Result<Option<Time>, EditError> {
        let mut end: Option<Time> = None;
        for sequence in &self.sequences {
            for track in &sequence.tracks {
                for item in &track.items {
                    match &item.kind {
                        ItemKind::Clip(clip) if clip.media == media => {
                            let e = clip.source_start.checked_add(item.range.duration)?;
                            end = Some(end.map_or(e, |x| x.max(e)));
                        }
                        ItemKind::Clip(_) => {}
                    }
                }
            }
        }
        Ok(end)
    }

    /// How many clips use `media`: the "in 3 edits" the relink dialog shows.
    pub fn clip_count(&self, media: MediaId) -> usize {
        self.sequences
            .iter()
            .flat_map(|s| s.tracks.iter())
            .flat_map(|t| t.items.iter())
            .filter(|i| i.clip().is_some_and(|c| c.media == media))
            .count()
    }

    /// Points each reference in `relinks` at its new file, all or nothing. Refuses an
    /// unknown media id, an id listed twice, an empty path and a file shorter than the
    /// source range its clips use (less [`relink_tolerance`]).
    pub fn relink_media(&self, relinks: &[Relink]) -> Result<Project, EditError> {
        let mut seen = HashSet::new();
        let mut next = self.clone();
        for relink in relinks {
            let Some(old) = self.media.get(relink.media) else {
                return Err(EditError::UnknownMedia(relink.media));
            };
            if !seen.insert(relink.media) {
                return Err(EditError::RelinkTwice(relink.media));
            }
            if relink.path.is_empty() {
                return Err(EditError::RelinkNoPath(relink.media));
            }
            if let (Some(found), Some(needed)) =
                (relink.duration, self.source_used_until(relink.media)?)
                && found.checked_add(relink_tolerance())? < needed
            {
                return Err(EditError::RelinkTooShort {
                    media: relink.media,
                    needed,
                    found,
                });
            }
            next.media.0.insert(
                relink.media,
                Arc::new(MediaRef {
                    id: old.id,
                    content_hash: relink.content_hash.clone(),
                    path: relink.path.clone(),
                }),
            );
        }
        Ok(next)
    }
}

/// How much shorter than the used source range a new file may probe and still be
/// accepted: one millisecond, because container durations are rounded and a millisecond
/// is far below a frame.
pub fn relink_tolerance() -> Time {
    Time::new(1, 1000).unwrap_or(Time::ZERO)
}
