//! Track switches (schema v9): mute, solo, visibility, lock and height, kept on
//! [`Track`] and changed by edits with undo like any other.
//!
//! Audio tracks have mute and solo; the other kinds (video, subtitle, adjustment) a
//! visibility switch; every track a lock and a height. Setting a switch a track's kind
//! does not have is refused ([`EditError::NoSuchSwitch`]), so a non-audio track is never
//! muted or soloed and an audio track never hidden ([`Project::validate`] holds this for
//! files too); changing a track's kind puts the switches the new kind lacks back to
//! their defaults.
//!
//! **Lock.** A locked track refuses every edit to itself and to anything on it
//! ([`EditError::TrackLocked`]): its items (insert, delete, move in or out, trim, slip,
//! speed, clip properties, effects, keyframes, clip markers, nesting and breaking apart),
//! its transitions, its name, kind and switches, and its removal. Only unlocking it and
//! changing its height (a view setting) go through. The check is one guard after each
//! edit ([`Project::unlocked`]): an edit whose result changes a track that is locked in
//! the root it started from is refused, whatever path the change took (a transition
//! refitted next to a trim, an item moved onto the track). So an edit's own errors
//! (unknown ids, overlaps, ranges) come first and the lock after them. Tracks of a
//! sequence the edit removes as a whole are not checked: deleting a sequence is not an
//! edit to its tracks. Edits outside tracks (media pool, relinking, settings, sequence
//! markers and formats) never touch a lock.
//!
//! Solo: while any audio track of a sequence is soloed, its audio tracks that are not
//! soloed are silent too; mute wins over solo ([`crate::Sequence::audible`]).

use std::sync::Arc;

use crate::error::EditError;
use crate::id::TrackId;
use crate::model::{Project, Track, TrackHeight, TrackKind};

impl Project {
    /// Mutes or unmutes an audio track. Refused on other kinds and on a locked track;
    /// setting the value it already has returns the project as it was.
    pub fn set_track_muted(&self, track: TrackId, muted: bool) -> Result<Project, EditError> {
        self.switch(track, "mute", is_audio, |t| {
            std::mem::replace(&mut t.muted, muted) != muted
        })
    }

    /// Solos or unsolos an audio track. Refused on other kinds and on a locked track.
    pub fn set_track_solo(&self, track: TrackId, solo: bool) -> Result<Project, EditError> {
        self.switch(track, "solo", is_audio, |t| {
            std::mem::replace(&mut t.solo, solo) != solo
        })
    }

    /// Shows or hides a picture track (video, subtitle, adjustment). Refused on audio
    /// tracks and on a locked track.
    pub fn set_track_visible(&self, track: TrackId, visible: bool) -> Result<Project, EditError> {
        self.switch(
            track,
            "visibility",
            |k| !is_audio(k),
            |t| std::mem::replace(&mut t.visible, visible) != visible,
        )
    }

    /// Locks or unlocks a track (any kind). Allowed on a locked track: it is how the
    /// track is unlocked.
    pub fn set_track_locked(&self, track: TrackId, locked: bool) -> Result<Project, EditError> {
        self.switch_unguarded(track, |t| std::mem::replace(&mut t.locked, locked) != locked)
    }

    /// Sets a track's height preset (any kind, locked or not: it only changes the view).
    pub fn set_track_height(
        &self,
        track: TrackId,
        height: TrackHeight,
    ) -> Result<Project, EditError> {
        self.switch_unguarded(track, |t| std::mem::replace(&mut t.height, height) != height)
    }

    /// Refuses `next`, the result of an edit of this root, when it changes or removes a
    /// track that is locked here, unless the edit removed the track's whole sequence.
    /// Every edit but the lock and height switches ends with it (see the module docs).
    pub(crate) fn unlocked(&self, next: Project) -> Result<Project, EditError> {
        if self.sequences.ptr_eq(&next.sequences) {
            return Ok(next);
        }
        for seq in &self.sequences {
            let Some(after) = next.sequence(seq.id) else {
                continue;
            };
            if Arc::ptr_eq(seq, after) || seq.tracks.ptr_eq(&after.tracks) {
                continue;
            }
            for track in seq.tracks.iter().filter(|t| t.locked) {
                match after.track(track.id) {
                    Some(t) if Arc::ptr_eq(t, track) || t.content_eq(track) => {}
                    _ => return Err(EditError::TrackLocked(track.id)),
                }
            }
        }
        Ok(next)
    }

    fn switch(
        &self,
        track: TrackId,
        name: &'static str,
        has: impl Fn(TrackKind) -> bool,
        set: impl FnOnce(&mut Track) -> bool,
    ) -> Result<Project, EditError> {
        let (_, found) = self
            .find_track(track)
            .ok_or(EditError::UnknownTrack(track))?;
        if !has(found.kind) {
            return Err(EditError::NoSuchSwitch {
                track,
                kind: found.kind,
                switch: name,
            });
        }
        let next = self.switch_unguarded(track, set)?;
        self.unlocked(next)
    }

    /// Applies `set` (which says whether it changed anything) to a copy of `track`.
    fn switch_unguarded(
        &self,
        track: TrackId,
        set: impl FnOnce(&mut Track) -> bool,
    ) -> Result<Project, EditError> {
        let (si, ti) = self.locate_track(track)?;
        let mut copy = Track::clone(&self.sequences[si].tracks[ti]);
        if !set(&mut copy) {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.sequence_at_mut(si).tracks[ti] = Arc::new(copy);
        Ok(next)
    }
}

fn is_audio(kind: TrackKind) -> bool {
    kind == TrackKind::Audio
}

/// Puts the switches `track`'s kind does not have back to their defaults (after a kind
/// change): mute and solo off on picture tracks, visible on audio tracks.
pub(crate) fn fit_switches(track: &mut Track) {
    if is_audio(track.kind) {
        track.visible = true;
    } else {
        track.muted = false;
        track.solo = false;
    }
}

/// The switch a track of its kind cannot have, if it has one set (for validation).
pub(crate) fn check_switches(track: &Track) -> Result<(), EditError> {
    let wrong = if is_audio(track.kind) {
        (!track.visible).then_some("visibility")
    } else if track.muted {
        Some("mute")
    } else if track.solo {
        Some("solo")
    } else {
        None
    };
    match wrong {
        Some(switch) => Err(EditError::NoSuchSwitch {
            track: track.id,
            kind: track.kind,
            switch,
        }),
        None => Ok(()),
    }
}
