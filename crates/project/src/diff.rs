//! Diffs between two project roots, by node id.
//!
//! A root is flattened into one record per node ([`Node`]): the project header, each
//! media reference, each sequence and track without its children (plus the order of its
//! children), each sequence marker, and each item whole (a clip's markers travel with
//! it). A [`Diff`] lists the records added or changed and the
//! ids removed; applying it to the flat form of the old root gives the flat form of the
//! new root, which rebuilds into an equal tree.
//!
//! Items are compared by pointer first (consecutive roots share them), so a diff costs one
//! pass over the ids plus the nodes that changed.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::id::{NodeId, SequenceId, TrackId};
use crate::marker::Marker;
use crate::model::{Item, MediaRef, MediaRefs, Project, Sequence, Track};
use crate::validate::ValidationError;

/// The id of the project header record; node ids start at 1.
pub const ROOT_ID: NodeId = NodeId(0);

/// One node of a flattened project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum Node {
    Root {
        uid: String,
        name: String,
        next_id: u64,
        sequences: Vec<SequenceId>,
    },
    Media {
        media: Arc<MediaRef>,
    },
    Sequence {
        /// The sequence with its markers and tracks left out.
        sequence: Arc<Sequence>,
        tracks: Vec<TrackId>,
    },
    Track {
        sequence: SequenceId,
        /// The track with its items left out.
        track: Arc<Track>,
    },
    Item {
        track: TrackId,
        item: Arc<Item>,
    },
    /// A marker on a sequence's ruler (schema v3; autosave format 2).
    Marker {
        sequence: SequenceId,
        marker: Arc<Marker>,
    },
}

impl Node {
    pub fn id(&self) -> NodeId {
        match self {
            Node::Root { .. } => ROOT_ID,
            Node::Media { media } => media.id.node(),
            Node::Sequence { sequence, .. } => sequence.id.node(),
            Node::Track { track, .. } => track.id.node(),
            Node::Item { item, .. } => item.id.node(),
            Node::Marker { marker, .. } => marker.id.node(),
        }
    }

    /// Equal content, checking shared pointers first.
    fn same(&self, other: &Node) -> bool {
        match (self, other) {
            (Node::Media { media: a }, Node::Media { media: b }) => Arc::ptr_eq(a, b) || a == b,
            (Node::Item { track: ta, item: a }, Node::Item { track: tb, item: b }) => {
                ta == tb && (Arc::ptr_eq(a, b) || a == b)
            }
            (
                Node::Marker {
                    sequence: sa,
                    marker: a,
                },
                Node::Marker {
                    sequence: sb,
                    marker: b,
                },
            ) => sa == sb && (Arc::ptr_eq(a, b) || a == b),
            _ => self == other,
        }
    }
}

/// A project as one record per node, keyed by id.
pub type Flat = BTreeMap<NodeId, Node>;

/// Flattens a root. Items keep their `Arc`, so this copies no item.
pub fn flatten(project: &Project) -> Flat {
    let mut flat = Flat::new();
    flat.insert(
        ROOT_ID,
        Node::Root {
            uid: project.uid.clone(),
            name: project.name.clone(),
            next_id: project.next_id,
            sequences: project.sequences.iter().map(|s| s.id).collect(),
        },
    );
    for media in project.media.iter() {
        flat.insert(
            media.id.node(),
            Node::Media {
                media: media.clone(),
            },
        );
    }
    for seq in &project.sequences {
        let mut bare = Sequence::clone(seq);
        bare.markers = imbl::Vector::new();
        bare.tracks = imbl::Vector::new();
        flat.insert(
            seq.id.node(),
            Node::Sequence {
                sequence: Arc::new(bare),
                tracks: seq.tracks.iter().map(|t| t.id).collect(),
            },
        );
        for marker in &seq.markers {
            flat.insert(
                marker.id.node(),
                Node::Marker {
                    sequence: seq.id,
                    marker: marker.clone(),
                },
            );
        }
        for track in &seq.tracks {
            let mut bare = Track::clone(track);
            bare.items = imbl::Vector::new();
            flat.insert(
                track.id.node(),
                Node::Track {
                    sequence: seq.id,
                    track: Arc::new(bare),
                },
            );
            for item in &track.items {
                flat.insert(
                    item.id.node(),
                    Node::Item {
                        track: track.id,
                        item: item.clone(),
                    },
                );
            }
        }
    }
    flat
}

/// The changes from one root to the next.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Diff {
    /// Records added or changed, in id order.
    pub upserts: Vec<Node>,
    /// Ids removed, ascending.
    pub removals: Vec<NodeId>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.removals.is_empty()
    }

    /// The changes from `old` to `new`.
    pub fn between(old: &Flat, new: &Flat) -> Diff {
        let upserts = new
            .iter()
            .filter(|(id, node)| old.get(id).is_none_or(|o| !o.same(node)))
            .map(|(_, node)| node.clone())
            .collect();
        let removals = old
            .keys()
            .filter(|id| !new.contains_key(id))
            .copied()
            .collect();
        Diff { upserts, removals }
    }

    /// Applies this diff to `flat` in place: removals first, then upserts.
    pub fn apply(&self, flat: &mut Flat) {
        for id in &self.removals {
            flat.remove(id);
        }
        for node in &self.upserts {
            flat.insert(node.id(), node.clone());
        }
    }
}

/// Rebuilds a root from its flat form and validates it.
pub fn rebuild(flat: &Flat) -> Result<Project, ValidationError> {
    let Some(Node::Root {
        uid,
        name,
        next_id,
        sequences,
    }) = flat.get(&ROOT_ID)
    else {
        return Err(ValidationError::Structure("no project header".into()));
    };
    let mut media = MediaRefs::default();
    let mut items: BTreeMap<TrackId, Vec<Arc<Item>>> = BTreeMap::new();
    let mut markers: BTreeMap<SequenceId, Vec<Arc<Marker>>> = BTreeMap::new();
    for node in flat.values() {
        match node {
            Node::Media { media: m } => {
                media.0.insert(m.id, m.clone());
            }
            Node::Item { track, item } => items.entry(*track).or_default().push(item.clone()),
            Node::Marker { sequence, marker } => {
                markers.entry(*sequence).or_default().push(marker.clone())
            }
            _ => {}
        }
    }
    let missing = |id: NodeId| ValidationError::Structure(format!("missing node {id}"));
    let mut project = Project {
        uid: uid.clone(),
        name: name.clone(),
        next_id: *next_id,
        media,
        sequences: imbl::Vector::new(),
    };
    for seq_id in sequences {
        let Some(Node::Sequence { sequence, tracks }) = flat.get(&seq_id.node()) else {
            return Err(missing(seq_id.node()));
        };
        let mut seq = Sequence::clone(sequence);
        let mut list = markers.remove(seq_id).unwrap_or_default();
        list.sort_by_key(|m| (m.start, m.id));
        seq.markers = list.into_iter().collect();
        for track_id in tracks {
            let Some(Node::Track { track, .. }) = flat.get(&track_id.node()) else {
                return Err(missing(track_id.node()));
            };
            let mut track = Track::clone(track);
            let mut list = items.remove(track_id).unwrap_or_default();
            list.sort_by_key(|a| a.range.start);
            track.items = list.into_iter().collect();
            seq.tracks.push_back(Arc::new(track));
        }
        project.sequences.push_back(Arc::new(seq));
    }
    if let Some((track, _)) = items.into_iter().next() {
        return Err(ValidationError::Structure(format!(
            "items on track {track}, which is not in any sequence"
        )));
    }
    if let Some((sequence, _)) = markers.into_iter().next() {
        return Err(ValidationError::Structure(format!(
            "markers on sequence {sequence}, which is not in the project"
        )));
    }
    project.validate()?;
    Ok(project)
}
