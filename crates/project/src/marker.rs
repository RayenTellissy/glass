//! Markers: named, coloured points or spans on a sequence's ruler or on a clip.
//!
//! A sequence marker sits in sequence time on the sequence's frame grid. A clip marker
//! sits in the clip's source time (seconds from the start of its file), so trimming or
//! moving the clip never moves it off the frame it marks; a clip marker outside the
//! clip's visible source range is kept and simply not shown. Both lists are ordered by
//! start, then id, and markers may overlap.
//!
//! A marker's colour is one of the five label colours ([`ColourLabel`]), drawn in the
//! same theme roles as media pool labels, never an RGB value. `chapter` makes it a
//! chapter in exports; `todo` makes it a to-do marker, open or done.
//!
//! The edits here follow [`crate::edit`]: pure, validated, sharing everything they do not
//! touch. A clip marker edit copies the clip's item (items are whole nodes); a sequence
//! marker edit copies the sequence node but none of its tracks.

use std::sync::Arc;

use imbl::Vector;
use serde::{Deserialize, Serialize};

use crate::error::EditError;
use crate::id::{ItemId, MarkerId, SequenceId};
use crate::media_pool::ColourLabel;
use crate::model::{ItemKind, Project};
use crate::time::{Time, TimeRange};

/// A marker. See the module docs for its time base.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub start: Time,
    /// Zero for a point marker.
    pub duration: Time,
    pub name: String,
    pub note: String,
    pub colour: ColourLabel,
    /// Becomes a chapter in exports.
    pub chapter: bool,
    pub todo: TodoState,
}

impl Marker {
    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.start, self.duration)
    }

    /// Everything but the id.
    pub fn spec(&self) -> MarkerSpec {
        MarkerSpec {
            start: self.start,
            duration: self.duration,
            name: self.name.clone(),
            note: self.note.clone(),
            colour: self.colour,
            chapter: self.chapter,
            todo: self.todo,
        }
    }
}

/// Whether a marker is a to-do, and whether it is ticked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoState {
    /// A plain marker.
    #[default]
    Off,
    Open,
    Done,
}

/// A marker's content, for [`Project::add_marker`] and [`Project::update_marker`].
#[derive(Clone, Debug, PartialEq)]
pub struct MarkerSpec {
    pub start: Time,
    pub duration: Time,
    pub name: String,
    pub note: String,
    pub colour: ColourLabel,
    pub chapter: bool,
    pub todo: TodoState,
}

impl MarkerSpec {
    /// A point marker at `start` with no name or note.
    pub fn at(start: Time, colour: ColourLabel) -> Self {
        Self {
            start,
            duration: Time::ZERO,
            name: String::new(),
            note: String::new(),
            colour,
            chapter: false,
            todo: TodoState::Off,
        }
    }

    fn range(&self) -> TimeRange {
        TimeRange::new(self.start, self.duration)
    }
}

/// What a marker is attached to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkerOwner {
    Sequence(SequenceId),
    /// A clip item; other items have no markers.
    Clip(ItemId),
}

/// Where a marker is in the tree, by index.
#[derive(Clone, Copy, Debug)]
enum Location {
    Sequence {
        si: usize,
        mi: usize,
    },
    Clip {
        si: usize,
        ti: usize,
        ii: usize,
        mi: usize,
    },
}

impl Project {
    /// The owner of `marker`, and the marker.
    pub fn find_marker(&self, marker: MarkerId) -> Option<(MarkerOwner, &Arc<Marker>)> {
        self.sequences.iter().find_map(|s| {
            if let Some(m) = s.marker(marker) {
                return Some((MarkerOwner::Sequence(s.id), m));
            }
            s.tracks.iter().find_map(|t| {
                t.items.iter().find_map(|i| {
                    let m = i.clip()?.marker(marker)?;
                    Some((MarkerOwner::Clip(i.id), m))
                })
            })
        })
    }

    /// Adds a marker to a sequence or a clip.
    pub fn add_marker(
        &self,
        owner: MarkerOwner,
        spec: MarkerSpec,
    ) -> Result<(Project, MarkerId), EditError> {
        let mut next = self.clone();
        let id = MarkerId(next.alloc_id());
        let list = next.markers_mut(owner)?;
        list.check(&spec)?;
        insert_sorted(list.markers, Arc::new(build(id, spec)));
        Ok((self.unlocked(next)?, id))
    }

    /// Replaces a marker's content (moving, resizing, renaming, recolouring, ticking),
    /// keeping its id and owner.
    pub fn update_marker(&self, marker: MarkerId, spec: MarkerSpec) -> Result<Project, EditError> {
        let mut next = self.clone();
        let location = next.locate_marker(marker)?;
        let list = next.markers_at_mut(location)?;
        list.check(&spec)?;
        let mi = match location {
            Location::Sequence { mi, .. } | Location::Clip { mi, .. } => mi,
        };
        list.markers.remove(mi);
        insert_sorted(list.markers, Arc::new(build(marker, spec)));
        self.unlocked(next)
    }

    /// Removes a marker.
    pub fn remove_marker(&self, marker: MarkerId) -> Result<Project, EditError> {
        let mut next = self.clone();
        let location = next.locate_marker(marker)?;
        let mi = match location {
            Location::Sequence { mi, .. } | Location::Clip { mi, .. } => mi,
        };
        next.markers_at_mut(location)?.markers.remove(mi);
        self.unlocked(next)
    }

    fn locate_marker(&self, marker: MarkerId) -> Result<Location, EditError> {
        self.sequences
            .iter()
            .enumerate()
            .find_map(|(si, s)| {
                if let Some(mi) = s.markers.iter().position(|m| m.id == marker) {
                    return Some(Location::Sequence { si, mi });
                }
                s.tracks.iter().enumerate().find_map(|(ti, t)| {
                    t.items.iter().enumerate().find_map(|(ii, i)| {
                        let mi = i.clip()?.markers.iter().position(|m| m.id == marker)?;
                        Some(Location::Clip { si, ti, ii, mi })
                    })
                })
            })
            .ok_or(EditError::UnknownMarker(marker))
    }

    fn markers_mut(&mut self, owner: MarkerOwner) -> Result<MarkerList<'_>, EditError> {
        match owner {
            MarkerOwner::Sequence(id) => {
                let si = self
                    .sequences
                    .iter()
                    .position(|s| s.id == id)
                    .ok_or(EditError::UnknownSequence(id))?;
                self.markers_at_mut(Location::Sequence { si, mi: 0 })
            }
            MarkerOwner::Clip(item) => {
                let (si, ti, ii) = self.locate_item(item)?;
                if self.sequences[si].tracks[ti].items[ii].clip().is_none() {
                    return Err(EditError::NotAClip(item));
                }
                self.markers_at_mut(Location::Clip { si, ti, ii, mi: 0 })
            }
        }
    }

    /// The writable marker list at `location` (its `mi` is ignored). The location must
    /// come from this project, and a clip location must hold a clip.
    fn markers_at_mut(&mut self, location: Location) -> Result<MarkerList<'_>, EditError> {
        Ok(match location {
            Location::Sequence { si, .. } => {
                let seq = self.sequence_at_mut(si);
                MarkerList {
                    grid: Some(seq.frame_rate.frame_duration()?),
                    markers: &mut seq.markers,
                }
            }
            Location::Clip { si, ti, ii, .. } => {
                let (_, track) = self.track_at_mut(si, ti);
                match &mut Arc::make_mut(&mut track.items[ii]).kind {
                    ItemKind::Clip(clip) => MarkerList {
                        grid: None,
                        markers: &mut clip.markers,
                    },
                    _ => unreachable!("clip locations hold clips"),
                }
            }
        })
    }
}

/// One owner's markers and the grid they must sit on (none for clip markers, whose
/// source rate the project does not know).
struct MarkerList<'a> {
    grid: Option<Time>,
    markers: &'a mut Vector<Arc<Marker>>,
}

impl MarkerList<'_> {
    fn check(&self, spec: &MarkerSpec) -> Result<(), EditError> {
        check_marker(self.grid, spec.range())
    }
}

/// A marker's span starts at or after zero, has no negative duration and, on a sequence,
/// sits on the frame grid.
pub(crate) fn check_marker(grid: Option<Time>, range: TimeRange) -> Result<(), EditError> {
    if range.start.is_negative() {
        return Err(EditError::MarkerBeforeZero(range.start));
    }
    if range.duration.is_negative() {
        return Err(EditError::NegativeDuration(range.duration));
    }
    if let Some(grid) = grid {
        for time in [range.start, range.duration] {
            if !time.is_multiple_of(grid) {
                return Err(EditError::MarkerOffGrid { time, grid });
            }
        }
    }
    Ok(())
}

fn build(id: MarkerId, spec: MarkerSpec) -> Marker {
    let MarkerSpec {
        start,
        duration,
        name,
        note,
        colour,
        chapter,
        todo,
    } = spec;
    Marker {
        id,
        start,
        duration,
        name,
        note,
        colour,
        chapter,
        todo,
    }
}

/// Inserts keeping the list ordered by start, then id.
fn insert_sorted(list: &mut Vector<Arc<Marker>>, marker: Arc<Marker>) {
    let key = (marker.start, marker.id);
    let index = match list.binary_search_by(|m| (m.start, m.id).cmp(&key)) {
        Ok(i) | Err(i) => i,
    };
    list.insert(index, marker);
}

/// Whether a list is ordered by start, then id.
pub(crate) fn is_sorted(list: &Vector<Arc<Marker>>) -> bool {
    list.iter()
        .zip(list.iter().skip(1))
        .all(|(a, b)| (a.start, a.id) < (b.start, b.id))
}
