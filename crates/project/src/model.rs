//! The edit tree: a project holds media references and sequences, a sequence holds
//! tracks, a track holds items ordered by start time.
//!
//! Every child collection is an `im` persistent collection of `Arc` nodes. Cloning a
//! [`Project`] is O(1), and an edit (see [`crate::edit`]) copies only the path from the
//! root to the nodes it changes: untouched sequences, tracks and items stay the same
//! allocation in the old and the new root, so `Arc::ptr_eq` holds for them.
//!
//! Only [`Project`]'s own fields are private: a node taken out of a project can be cloned
//! and changed freely, but it only gets back into a project through an edit operation,
//! which validates it.

use std::sync::Arc;

use imbl::{OrdMap, Vector};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::id::{ItemId, MediaId, SequenceId, TrackId};
use crate::time::{Rational, Time, TimeError, TimeRange};

/// The document root. Cheap to clone; see the module docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Stable identity of the project (not of a file), used to name its autosave folder.
    pub(crate) uid: String,
    pub(crate) name: String,
    /// The next unused node id.
    pub(crate) next_id: u64,
    /// Source media the project refers to. The media pool (bins, metadata, relink state)
    /// will embed this set of references once it joins the tree.
    pub(crate) media: MediaRefs,
    pub(crate) sequences: Vector<Arc<Sequence>>,
}

impl Project {
    /// An empty project. `uid` should be unique per project (the app uses a random id).
    pub fn new(uid: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            name: name.into(),
            next_id: 1,
            media: MediaRefs::default(),
            sequences: Vector::new(),
        }
    }

    pub fn uid(&self) -> &str {
        &self.uid
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    pub fn media(&self) -> &MediaRefs {
        &self.media
    }

    pub fn sequences(&self) -> &Vector<Arc<Sequence>> {
        &self.sequences
    }

    pub fn sequence(&self, id: SequenceId) -> Option<&Arc<Sequence>> {
        self.sequences.iter().find(|s| s.id == id)
    }

    /// The sequence and track holding `item`, and the item itself.
    pub fn find_item(&self, item: ItemId) -> Option<(&Arc<Sequence>, &Arc<Track>, &Arc<Item>)> {
        self.sequences.iter().find_map(|s| {
            s.tracks
                .iter()
                .find_map(|t| t.item(item).map(|i| (s, t, i)))
        })
    }

    /// The sequence holding `track`, and the track.
    pub fn find_track(&self, track: TrackId) -> Option<(&Arc<Sequence>, &Arc<Track>)> {
        self.sequences
            .iter()
            .find_map(|s| s.track(track).map(|t| (s, t)))
    }

    /// True when `other` is this very version: nothing was edited between them (every
    /// child collection is the same shared tree).
    pub fn ptr_eq(&self, other: &Project) -> bool {
        self.next_id == other.next_id
            && self.uid == other.uid
            && self.name == other.name
            && self.media.ptr_eq(&other.media)
            && self.sequences.ptr_eq(&other.sequences)
    }

    /// Equal content, comparing shared nodes by pointer first, so two roots that differ
    /// in one item cost about one track's worth of pointer comparisons.
    pub fn content_eq(&self, other: &Project) -> bool {
        let Project {
            uid,
            name,
            next_id,
            media,
            sequences,
        } = self;
        self.ptr_eq(other)
            || (*next_id == other.next_id
                && *uid == other.uid
                && *name == other.name
                && (media.ptr_eq(&other.media) || *media == other.media)
                && all_eq(sequences, &other.sequences, |a, b| a.content_eq(b)))
    }

    pub(crate) fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// A reference to one source file: its content hash (`media::ContentHash`, hex) and the
/// path it was last found at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaRef {
    pub id: MediaId,
    pub content_hash: String,
    pub path: String,
}

/// The project's source references by id. Serialised as a list ordered by id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaRefs(pub(crate) OrdMap<MediaId, Arc<MediaRef>>);

impl MediaRefs {
    pub fn get(&self, id: MediaId) -> Option<&Arc<MediaRef>> {
        self.0.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<MediaRef>> {
        self.0.values()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// True when both share the same underlying tree (no copy was made).
    pub fn ptr_eq(&self, other: &MediaRefs) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl Serialize for MediaRefs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.values())
    }
}

impl<'de> Deserialize<'de> for MediaRefs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let list = Vec::<Arc<MediaRef>>::deserialize(deserializer)?;
        Ok(MediaRefs(list.into_iter().map(|m| (m.id, m)).collect()))
    }
}

/// A timeline: tracks at one frame rate and audio sample rate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    /// Frames per second; video, subtitle and adjustment items sit on its frame grid.
    pub frame_rate: Rational,
    /// Audio samples per second; audio items sit on its sample grid.
    pub sample_rate: u32,
    /// Top to bottom as shown in the timeline.
    pub tracks: Vector<Arc<Track>>,
}

impl Sequence {
    pub fn track(&self, id: TrackId) -> Option<&Arc<Track>> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Equal content, comparing shared tracks and items by pointer first.
    pub fn content_eq(&self, other: &Sequence) -> bool {
        let Sequence {
            id,
            name,
            frame_rate,
            sample_rate,
            tracks,
        } = self;
        *id == other.id
            && *name == other.name
            && *frame_rate == other.frame_rate
            && *sample_rate == other.sample_rate
            && all_eq(tracks, &other.tracks, |a, b| a.content_eq(b))
    }

    pub(crate) fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// The time grid items on a track of `kind` must sit on.
    pub fn grid(&self, kind: TrackKind) -> Result<Time, TimeError> {
        match kind {
            TrackKind::Audio => Time::from_samples(1, self.sample_rate),
            TrackKind::Video => self.frame_rate.frame_duration(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
}

/// A track: items ordered by start time, never overlapping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub items: Vector<Arc<Item>>,
}

impl Track {
    pub fn item(&self, id: ItemId) -> Option<&Arc<Item>> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Equal content, comparing shared items by pointer first.
    pub fn content_eq(&self, other: &Track) -> bool {
        let Track {
            id,
            kind,
            name,
            items,
        } = self;
        *id == other.id
            && *kind == other.kind
            && *name == other.name
            && all_eq(items, &other.items, |a, b| a == b)
    }

    pub(crate) fn item_index(&self, id: ItemId) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    /// The end of the last item, or zero for an empty track.
    pub fn end(&self) -> Result<Time, TimeError> {
        self.items.last().map_or(Ok(Time::ZERO), |i| i.range.end())
    }
}

/// Something that occupies a range of a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    /// Where the item sits on the timeline, in sequence time.
    pub range: TimeRange,
    #[serde(flatten)]
    pub kind: ItemKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemKind {
    Clip(Clip),
}

impl Item {
    pub fn clip(&self) -> Option<&Clip> {
        match &self.kind {
            ItemKind::Clip(c) => Some(c),
        }
    }
}

/// A piece of a source file placed on a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub media: MediaId,
    /// Source time (seconds from the start of the file) shown at the item's start.
    pub source_start: Time,
}

/// Element-wise equality of two node lists, by pointer first.
fn all_eq<T>(a: &Vector<Arc<T>>, b: &Vector<Arc<T>>, eq: impl Fn(&T, &T) -> bool) -> bool {
    a.ptr_eq(b)
        || (a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(x, y)| Arc::ptr_eq(x, y) || eq(x, y)))
}
