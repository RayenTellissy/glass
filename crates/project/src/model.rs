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
use serde::{Deserialize, Serialize};

use crate::clip::{BlendMode, Crop, Effect, SpeedMap, Transform};
use crate::error::EditError;
use crate::id::{EffectId, ItemId, MarkerId, MediaId, SequenceId, TrackId};
use crate::keyframe::{ClipParam, Curve};
use crate::marker::Marker;
use crate::media_pool::{MediaItem, MediaPool};
use crate::settings::{ChannelLayout, ProjectSettings, Resolution, SequenceFormat};
use crate::time::{Rational, Time, TimeError, TimeRange};
use crate::transition::Transition;

/// The document root. Cheap to clone; see the module docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Stable identity of the project (not of a file), used to name its autosave folder.
    pub(crate) uid: String,
    pub(crate) name: String,
    /// The next unused node id.
    pub(crate) next_id: u64,
    /// Project settings (schema v6; see [`crate::settings`]). Shared, so cloning the root
    /// stays O(1).
    pub(crate) settings: Arc<ProjectSettings>,
    /// The media pool (schema v9; see [`crate::media_pool`]): every source file the
    /// project refers to, with its bins, colour labels, tags and import facts. Whether a
    /// file is available is not stored (Decision 29).
    pub(crate) media: MediaPool,
    pub(crate) sequences: Vector<Arc<Sequence>>,
}

impl Project {
    /// An empty project with default settings ([`ProjectSettings::default`]) and an
    /// empty media pool whose root bin carries the project's name. `uid` should be unique
    /// per project (the app uses a random id).
    pub fn new(uid: impl Into<String>, name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            uid: uid.into(),
            media: MediaPool::new(name.clone()),
            name,
            next_id: 1,
            settings: Arc::new(ProjectSettings::default()),
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

    /// The media pool: the project's source references with their bins and labels.
    pub fn media(&self) -> &MediaPool {
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
            && Arc::ptr_eq(&self.settings, &other.settings)
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
            settings,
            media,
            sequences,
        } = self;
        self.ptr_eq(other)
            || (*next_id == other.next_id
                && *uid == other.uid
                && *name == other.name
                && (Arc::ptr_eq(settings, &other.settings) || *settings == other.settings)
                && (media.ptr_eq(&other.media) || *media == other.media)
                && all_eq(sequences, &other.sequences, |a, b| a.content_eq(b)))
    }

    pub(crate) fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// A reference to one source file: the media pool item (schema v9) holding its path,
/// content hash, name, bin, labels and the facts read at import. The name is kept for the
/// timeline code that only reads `path` and `content_hash`.
pub type MediaRef = MediaItem;

/// A timeline: tracks on one canvas at one frame rate and audio sample rate. Its format
/// fields ([`Sequence::format`]) change only through
/// [`Project::set_sequence_format`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    /// A compound clip's contents: made from a selection, shown only through the clips
    /// that nest it, not in the browser's sequence list (schema v8; see
    /// [`crate::nest`]).
    pub compound: bool,
    /// The canvas size in pixels (schema v6).
    pub resolution: Resolution,
    /// Frames per second; video, subtitle and adjustment items sit on its frame grid.
    pub frame_rate: Rational,
    /// Timecode counts drop-frame (29.97 and 59.94 only; schema v6).
    pub drop_frame: bool,
    /// Audio samples per second; audio items sit on its sample grid.
    pub sample_rate: u32,
    /// The speaker layout the sequence mixes to (schema v6).
    pub channel_layout: ChannelLayout,
    /// Markers on the sequence's ruler, in sequence time, ordered by start then id
    /// (schema v3).
    pub markers: Vector<Arc<Marker>>,
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
            compound,
            resolution,
            frame_rate,
            drop_frame,
            sample_rate,
            channel_layout,
            markers,
            tracks,
        } = self;
        *id == other.id
            && *name == other.name
            && *compound == other.compound
            && *resolution == other.resolution
            && *frame_rate == other.frame_rate
            && *drop_frame == other.drop_frame
            && *sample_rate == other.sample_rate
            && *channel_layout == other.channel_layout
            && all_eq(markers, &other.markers, |a, b| a == b)
            && all_eq(tracks, &other.tracks, |a, b| a.content_eq(b))
    }

    pub(crate) fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// The sequence's format: canvas, frame rate, drop-frame, sample rate, channels.
    pub fn format(&self) -> SequenceFormat {
        SequenceFormat {
            resolution: self.resolution,
            frame_rate: self.frame_rate,
            drop_frame: self.drop_frame,
            sample_rate: self.sample_rate,
            channel_layout: self.channel_layout,
        }
    }

    pub(crate) fn set_format(&mut self, format: SequenceFormat) {
        let SequenceFormat {
            resolution,
            frame_rate,
            drop_frame,
            sample_rate,
            channel_layout,
        } = format;
        self.resolution = resolution;
        self.frame_rate = frame_rate;
        self.drop_frame = drop_frame;
        self.sample_rate = sample_rate;
        self.channel_layout = channel_layout;
    }

    /// The time grid items on a track of `kind` must sit on.
    pub fn grid(&self, kind: TrackKind) -> Result<Time, TimeError> {
        match kind {
            TrackKind::Audio => Time::from_samples(1, self.sample_rate),
            TrackKind::Video | TrackKind::Subtitle | TrackKind::Adjustment => {
                self.frame_rate.frame_duration()
            }
        }
    }

    pub fn marker(&self, id: MarkerId) -> Option<&Arc<Marker>> {
        self.markers.iter().find(|m| m.id == id)
    }

    /// Whether any audio track of the sequence is soloed.
    pub fn any_solo(&self) -> bool {
        self.tracks
            .iter()
            .any(|t| t.kind == TrackKind::Audio && t.solo)
    }

    /// Whether `track` is heard: an audio track of this sequence, not muted, and soloed
    /// whenever any audio track of the sequence is ([`Track::audible`]).
    pub fn audible(&self, track: TrackId) -> bool {
        self.track(track)
            .is_some_and(|t| t.audible(self.any_solo()))
    }
}

/// What a track holds. Each kind accepts one kind of item ([`TrackKind::accepts`]):
/// video and audio tracks hold clips, subtitle tracks subtitles, adjustment tracks
/// adjustment items. Audio items sit on the sample grid, all others on the frame grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
    Adjustment,
}

impl TrackKind {
    pub const ALL: [TrackKind; 4] = [
        TrackKind::Video,
        TrackKind::Audio,
        TrackKind::Subtitle,
        TrackKind::Adjustment,
    ];

    /// Whether an item of `kind` may sit on a track of this kind.
    pub fn accepts(self, kind: &ItemKind) -> bool {
        matches!(
            (self, kind),
            (TrackKind::Video | TrackKind::Audio, ItemKind::Clip(_))
                | (TrackKind::Subtitle, ItemKind::Subtitle(_))
                | (TrackKind::Adjustment, ItemKind::Adjustment(_))
        )
    }
}

/// A track's height preset in the timeline (schema v9). The UI maps each to its row
/// height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackHeight {
    /// One control row: name and switches.
    Small,
    /// Adds a caption line under the name.
    #[default]
    Medium,
    /// Room for thumbnails and waveforms.
    Large,
}

impl TrackHeight {
    pub const ALL: [TrackHeight; 3] = [TrackHeight::Small, TrackHeight::Medium, TrackHeight::Large];
}

/// A track: items ordered by start time, never overlapping.
///
/// Gaps are implicit: the empty space between items is not stored (no gap items), so no
/// edit has to keep gap nodes in step with the items around them. [`Track::gaps`] lists
/// them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    /// Audio tracks: silenced (schema v9; see [`crate::switches`]). Never set on the
    /// other kinds.
    pub muted: bool,
    /// Audio tracks: soloed; while any audio track of a sequence is, the audio tracks
    /// that are not are silent too ([`Sequence::audible`]). Never set on the other kinds.
    pub solo: bool,
    /// Picture tracks (video, subtitle, adjustment): shown in the picture. Always true on
    /// audio tracks.
    pub visible: bool,
    /// Edits to the track and everything on it are refused, all but unlocking it and
    /// changing its height ([`EditError::TrackLocked`]).
    pub locked: bool,
    /// The track's height in the timeline (a view setting kept with the project).
    pub height: TrackHeight,
    pub items: Vector<Arc<Item>>,
    /// Transitions on the cuts of this track's clips, ordered by id (schema v5; see
    /// [`crate::transition`]).
    pub transitions: Vector<Arc<Transition>>,
}

impl Track {
    /// An empty track of `kind` with its switches at their defaults: heard, shown,
    /// unlocked, medium height.
    pub fn new(id: TrackId, kind: TrackKind, name: impl Into<String>) -> Self {
        Self {
            id,
            kind,
            name: name.into(),
            muted: false,
            solo: false,
            visible: true,
            locked: false,
            height: TrackHeight::default(),
            items: Vector::new(),
            transitions: Vector::new(),
        }
    }

    pub fn item(&self, id: ItemId) -> Option<&Arc<Item>> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Whether the track's items show in the picture: a picture track (not audio) that
    /// is visible.
    pub fn shows_picture(&self) -> bool {
        self.kind != TrackKind::Audio && self.visible
    }

    /// Whether an audio track is heard: not muted, and soloed whenever `any_solo` (any
    /// audio track of its sequence is soloed). Mute wins over solo. Other kinds are
    /// never heard.
    pub fn audible(&self, any_solo: bool) -> bool {
        self.kind == TrackKind::Audio && !self.muted && (self.solo || !any_solo)
    }

    /// Equal content, comparing shared items by pointer first.
    pub fn content_eq(&self, other: &Track) -> bool {
        let Track {
            id,
            kind,
            name,
            muted,
            solo,
            visible,
            locked,
            height,
            items,
            transitions,
        } = self;
        *id == other.id
            && *kind == other.kind
            && *name == other.name
            && *muted == other.muted
            && *solo == other.solo
            && *visible == other.visible
            && *locked == other.locked
            && *height == other.height
            && all_eq(items, &other.items, |a, b| a == b)
            && all_eq(transitions, &other.transitions, |a, b| a == b)
    }

    pub(crate) fn item_index(&self, id: ItemId) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    /// The end of the last item, or zero for an empty track.
    pub fn end(&self) -> Result<Time, TimeError> {
        self.items.last().map_or(Ok(Time::ZERO), |i| i.range.end())
    }

    /// The item covering `time`, if any.
    pub fn item_at(&self, time: Time) -> Result<Option<&Arc<Item>>, TimeError> {
        let index = match self.items.binary_search_by(|i| i.range.start.cmp(&time)) {
            Ok(i) => return Ok(self.items.get(i)),
            Err(0) => return Ok(None),
            Err(i) => i - 1,
        };
        let item = &self.items[index];
        Ok(item.range.contains(time)?.then_some(item))
    }

    /// The empty spans between zero and [`Track::end`], in time order.
    pub fn gaps(&self) -> Result<Vec<TimeRange>, TimeError> {
        let mut gaps = Vec::new();
        let mut at = Time::ZERO;
        for item in &self.items {
            if item.range.start > at {
                gaps.push(TimeRange::between(at, item.range.start)?);
            }
            at = at.max(item.range.end()?);
        }
        Ok(gaps)
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

// Every item is its own `Arc` node, so the enum is never stored by value in a collection;
// boxing the clip would only add a second allocation per clip.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemKind {
    Clip(Clip),
    Subtitle(Subtitle),
    Adjustment(Adjustment),
}

impl ItemKind {
    /// The kind's name in files and messages (`clip`, `subtitle`, `adjustment`).
    pub fn name(&self) -> &'static str {
        match self {
            ItemKind::Clip(_) => "clip",
            ItemKind::Subtitle(_) => "subtitle",
            ItemKind::Adjustment(_) => "adjustment",
        }
    }
}

impl Item {
    pub fn clip(&self) -> Option<&Clip> {
        match &self.kind {
            ItemKind::Clip(c) => Some(c),
            _ => None,
        }
    }
}

/// What a clip shows: a source file, or another sequence of the project (a nested
/// sequence; schema v8, see [`crate::nest`]). Serialised flat inside the clip, as
/// `"media": 3` or `"sequence": 7`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipSource {
    Media(MediaId),
    /// The sequence's timeline time is the clip's source time.
    Sequence(SequenceId),
}

impl ClipSource {
    pub fn media(self) -> Option<MediaId> {
        match self {
            ClipSource::Media(m) => Some(m),
            ClipSource::Sequence(_) => None,
        }
    }

    pub fn sequence(self) -> Option<SequenceId> {
        match self {
            ClipSource::Sequence(s) => Some(s),
            ClipSource::Media(_) => None,
        }
    }
}

impl From<MediaId> for ClipSource {
    fn from(media: MediaId) -> Self {
        ClipSource::Media(media)
    }
}

impl From<SequenceId> for ClipSource {
    fn from(sequence: SequenceId) -> Self {
        ClipSource::Sequence(sequence)
    }
}

/// A piece of a source file, or of a nested sequence, placed on a track, with its
/// properties (schema v4; see [`crate::clip`] for how they relate and the edits that set
/// them).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// The media or sequence it shows (schema v8; `"media"` or `"sequence"` in files).
    #[serde(flatten)]
    pub source: ClipSource,
    /// Source time (seconds from the start of the file, or of the nested sequence) shown
    /// at the item's start.
    pub source_start: Time,
    /// How fast the source plays: the source duration is the timeline duration times the
    /// speed.
    pub speed: SpeedMap,
    /// A disabled clip keeps its place but shows and plays nothing.
    pub enabled: bool,
    /// Audio gain in dB (clips on audio tracks).
    pub gain_db: f64,
    /// Placement on the canvas (clips on video tracks, as are the crop, opacity, blend mode
    /// and effects).
    pub transform: Transform,
    pub crop: Crop,
    /// 0 to 1.
    pub opacity: f64,
    pub blend: BlendMode,
    /// The effect stack, applied first to last.
    pub effects: Vector<Arc<Effect>>,
    /// Curves of the animated properties, by property (schema v7; see
    /// [`crate::keyframe`]). While a property has a curve, its constant field above is
    /// ignored: read the values through [`Clip::param_at`], [`Clip::transform_at`] and the
    /// other `*_at` readers.
    #[serde(serialize_with = "crate::keyframe::serialize_clip_curves")]
    pub keyframes: OrdMap<ClipParam, Arc<Curve>>,
    /// Markers on the clip in source time, so they stay on their frame when the clip is
    /// trimmed or moved; ordered by start then id (schema v3).
    pub markers: Vector<Arc<Marker>>,
}

impl Clip {
    /// A clip of `source` (a [`MediaId`] or a [`SequenceId`]) from `source_start` with
    /// every property at its default: speed 1, enabled, 0 dB, centred at 100%, uncropped,
    /// opaque, normal blend, no effects, nothing animated, no markers.
    pub fn new(source: impl Into<ClipSource>, source_start: Time) -> Self {
        Self {
            source: source.into(),
            source_start,
            speed: SpeedMap::default(),
            enabled: true,
            gain_db: 0.0,
            transform: Transform::IDENTITY,
            crop: Crop::NONE,
            opacity: 1.0,
            blend: BlendMode::Normal,
            effects: Vector::new(),
            keyframes: OrdMap::new(),
            markers: Vector::new(),
        }
    }

    /// The source span an item with timeline `range` shows: from `source_start`, for the
    /// timeline duration times the speed.
    pub fn source_range(&self, range: TimeRange) -> Result<TimeRange, EditError> {
        Ok(TimeRange::new(
            self.source_start,
            self.speed.source_duration(range.duration)?,
        ))
    }

    /// The source time an item starting at `item_start` shows at timeline `time`:
    /// `source_start + (time − item_start) × speed`. Outside the item's range it is a
    /// handle: before the start for the incoming clip of a transition, past the end for
    /// the outgoing one.
    pub fn source_time_at(&self, item_start: Time, time: Time) -> Result<Time, EditError> {
        let offset = self.speed.source_duration(time.checked_sub(item_start)?)?;
        Ok(self.source_start.checked_add(offset)?)
    }

    pub fn marker(&self, id: MarkerId) -> Option<&Arc<Marker>> {
        self.markers.iter().find(|m| m.id == id)
    }

    pub fn effect(&self, id: EffectId) -> Option<&Arc<Effect>> {
        self.effects.iter().find(|e| e.id == id)
    }
}

/// A subtitle cue on a subtitle track. Caption styles join it in Phase 10.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subtitle {
    pub text: String,
}

/// An adjustment item: its effects apply to every track below it over its range. The
/// effect stack joins it with the adjustment layers task.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Adjustment {}

/// Element-wise equality of two node lists, by pointer first.
fn all_eq<T>(a: &Vector<Arc<T>>, b: &Vector<Arc<T>>, eq: impl Fn(&T, &T) -> bool) -> bool {
    a.ptr_eq(b)
        || (a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(x, y)| Arc::ptr_eq(x, y) || eq(x, y)))
}
