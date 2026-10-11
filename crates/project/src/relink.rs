//! Relinking: pointing media references at the files they moved to.
//!
//! A relink replaces only [`MediaRef`] nodes (path and content hash). Every sequence,
//! track and clip stays the very same node, so trims, moves and effects made while the
//! media was offline are kept as they are, and one batch is one undo step
//! ([`crate::EditLabel::Relink`]). Finding the new files is the engine's job
//! (`engine::media_mgmt::relink`); this module only validates and applies the choice.
//! Clips of nested sequences have no media and are never relinked.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::error::EditError;
use crate::id::{MediaId, SequenceId};
use crate::model::{ClipSource, MediaRef, Project};
use crate::time::Time;
use crate::transition::needs;

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
    /// The end of the furthest source time any clip of `media` shows, the tail handles of
    /// transitions out of its clips included: the least length a file must have to stand
    /// in for it. `None` when no clip uses it.
    pub fn source_used_until(&self, media: MediaId) -> Result<Option<Time>, EditError> {
        let mut end: Option<Time> = None;
        let mut extend = |e: Time| end = Some(end.map_or(e, |x: Time| x.max(e)));
        for sequence in &self.sequences {
            for track in &sequence.tracks {
                for item in &track.items {
                    if let Some(clip) = item.clip()
                        && clip.source == ClipSource::Media(media)
                    {
                        extend(clip.source_range(item.range)?.end()?);
                    }
                }
                if track.transitions.is_empty() {
                    continue;
                }
                let grid = sequence.grid(track.kind)?;
                for transition in &track.transitions {
                    if let Some((_, m, e)) = needs(track, grid, transition)?.outgoing
                        && m == ClipSource::Media(media)
                    {
                        extend(e);
                    }
                }
            }
        }
        Ok(end)
    }

    /// How many times clips of `media` play in the browser's sequences (every sequence
    /// but compounds' contents): the "in 3 edits" the relink dialog shows. Nested
    /// sequences count through every clip that nests them, so a clip inside a compound
    /// counts once per clip of that compound, and a clip in a sequence nested in another
    /// counts in both. A compound no clip nests counts nowhere.
    pub fn clip_count(&self, media: MediaId) -> usize {
        let mut memo = HashMap::new();
        self.browser_sequences()
            .map(|s| uses(self, s.id, media, &mut memo))
            .sum()
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

/// How many clips of `media` sequence `id` plays, through its nested sequences;
/// memoised per sequence. A cycle (never in a valid tree) counts as nothing.
fn uses(
    project: &Project,
    id: SequenceId,
    media: MediaId,
    memo: &mut HashMap<SequenceId, usize>,
) -> usize {
    if let Some(&n) = memo.get(&id) {
        return n;
    }
    memo.insert(id, 0);
    let mut n = 0;
    if let Some(seq) = project.sequence(id) {
        for clip in seq
            .tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .filter_map(|i| i.clip())
        {
            n += match clip.source {
                ClipSource::Media(m) => usize::from(m == media),
                ClipSource::Sequence(s) => uses(project, s, media, memo),
            };
        }
    }
    memo.insert(id, n);
    n
}

/// How much shorter than the used source range a new file may probe and still be
/// accepted: one millisecond, because container durations are rounded and a millisecond
/// is far below a frame.
pub fn relink_tolerance() -> Time {
    Time::new(1, 1000).unwrap_or(Time::ZERO)
}
