//! Dirty ranges: the timeline frames of each sequence an edit can change on screen.
//!
//! [`DirtyRanges::between`] compares two project roots through `project::diff` (one record
//! per node, items compared by pointer first) and turns every changed record into frames
//! of the sequence holding it, at that root's frame rate:
//!
//! - an item on a picture track (video, subtitle, adjustment): its old and new extents,
//!   plus the spans of the transitions on its track that name it in either root (an
//!   incoming clip shows before its start there); a clip whose markers alone changed
//!   changes nothing
//! - a transition: its old and new spans (an item edit that refits it changes its record
//!   too)
//! - a media reference (relink, new content hash): every clip that uses it, as above
//! - a track added, removed or changed in anything but its name (its kind, and whatever
//!   later schemas add: visibility, mute, blend): its whole content in both roots; when
//!   the picture tracks change order, the content of each track whose place among them
//!   moved
//! - a sequence added, removed or changed in anything but its name, drop-frame flag,
//!   sample rate and channel layout (its canvas, its frame rate): every frame
//! - the project settings, changed in anything but the format new sequences start with
//!   and the cache folder (the colour space): every frame of every sequence
//! - markers, names and audio tracks: nothing
//!
//! Fields a later schema adds to a track, a sequence or the settings count as changing
//! everything they cover until they are listed above, so a new kind of edit is never
//! missed. Ranges are frames `floor(start)..ceil(end)` at each root's rate, sorted and
//! merged. They are a superset of what renders again: the frame cache's keys
//! ([`TimelinePlan::span_hash`](super::TimelinePlan::span_hash)) decide that, so the part
//! a trim keeps of a clip, for one, is listed but stays cached.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use project::diff::{self, Diff, Node};
use project::{
    ItemId, ItemKind, MediaId, Project, ProjectSettings, Sequence, SequenceId, TimeRange, Track,
    TrackId, TrackKind, Transition,
};

/// The frames of one sequence an edit can change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirtyFrames {
    /// Every frame (a new canvas, frame rate or colour space, a new or removed sequence).
    All,
    /// These frames, end exclusive: sorted, merged, none empty.
    Ranges(Vec<Range<i64>>),
}

impl Default for DirtyFrames {
    fn default() -> Self {
        Self::none()
    }
}

impl DirtyFrames {
    /// No frame.
    pub fn none() -> Self {
        Self::Ranges(Vec::new())
    }

    /// `ranges` sorted and merged, empty ones left out.
    pub fn from_ranges(ranges: impl IntoIterator<Item = Range<i64>>) -> Self {
        let mut ranges: Vec<Range<i64>> = ranges.into_iter().filter(|r| r.start < r.end).collect();
        ranges.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<i64>> = Vec::with_capacity(ranges.len());
        for range in ranges {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        Self::Ranges(merged)
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Ranges(r) if r.is_empty())
    }

    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }

    /// The ranges; `None` for every frame.
    pub fn ranges(&self) -> Option<&[Range<i64>]> {
        match self {
            Self::All => None,
            Self::Ranges(r) => Some(r),
        }
    }

    pub fn contains(&self, frame: i64) -> bool {
        match self {
            Self::All => true,
            Self::Ranges(r) => {
                let at = r.partition_point(|range| range.end <= frame);
                r.get(at).is_some_and(|range| range.start <= frame)
            }
        }
    }

    /// Whether any of `frames` is dirty.
    pub fn intersects(&self, frames: &Range<i64>) -> bool {
        if frames.start >= frames.end {
            return false;
        }
        match self {
            Self::All => true,
            Self::Ranges(r) => {
                let at = r.partition_point(|range| range.end <= frames.start);
                r.get(at).is_some_and(|range| range.start < frames.end)
            }
        }
    }

    /// The union of both.
    pub fn union(&self, other: &DirtyFrames) -> DirtyFrames {
        match (self, other) {
            (Self::Ranges(a), Self::Ranges(b)) => Self::from_ranges(a.iter().chain(b).cloned()),
            _ => Self::All,
        }
    }
}

/// The dirty frames of every sequence an edit touched (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirtyRanges {
    sequences: BTreeMap<SequenceId, DirtyFrames>,
}

impl DirtyRanges {
    /// What changes on screen from `old` to `new`. Pure: costs one flattening of each
    /// root plus the records that changed.
    pub fn between(old: &Project, new: &Project) -> Self {
        if old.ptr_eq(new) {
            return Self::default();
        }
        let (old_flat, new_flat) = (diff::flatten(old), diff::flatten(new));
        let changes = Diff::between(&old_flat, &new_flat);
        let mut collect = Collect::default();
        let roots = Roots { old, new };
        for node in &changes.upserts {
            roots.changed(&mut collect, old_flat.get(&node.id()), Some(node));
        }
        for id in &changes.removals {
            roots.changed(&mut collect, old_flat.get(id), None);
        }
        collect.finish()
    }

    /// The dirty frames of `sequence` (none when the edit did not touch it).
    pub fn sequence(&self, sequence: SequenceId) -> DirtyFrames {
        self.sequences.get(&sequence).cloned().unwrap_or_default()
    }

    /// No sequence changed on screen.
    pub fn is_empty(&self) -> bool {
        self.sequences.is_empty()
    }

    /// The sequences with dirty frames, by id.
    pub fn iter(&self) -> impl Iterator<Item = (SequenceId, &DirtyFrames)> {
        self.sequences.iter().map(|(id, frames)| (*id, frames))
    }
}

/// Whether a track's items show in the picture.
fn shows_picture(kind: TrackKind) -> bool {
    !matches!(kind, TrackKind::Audio)
}

#[derive(Default)]
struct Collect {
    all: BTreeSet<SequenceId>,
    ranges: BTreeMap<SequenceId, Vec<Range<i64>>>,
}

impl Collect {
    fn all(&mut self, sequence: SequenceId) {
        self.all.insert(sequence);
    }

    /// `range` of `seq` as frames; a time that does not resolve marks every frame.
    fn time(&mut self, seq: &Sequence, range: Option<TimeRange>) {
        let Some((range, end)) = range.and_then(|r| r.end().ok().map(|end| (r, end))) else {
            self.all(seq.id);
            return;
        };
        let rate = seq.frame_rate;
        let start = range.start.frames_floor(rate);
        let end = (-end).frames_floor(rate).saturating_neg();
        self.ranges.entry(seq.id).or_default().push(start..end);
    }

    fn transition(&mut self, seq: &Sequence, track: &Track, transition: &Transition) {
        let span = seq
            .grid(track.kind)
            .ok()
            .and_then(|grid| track.transition_span(grid, transition).ok());
        self.time(seq, span);
    }

    /// Item `item` of `track` and the transitions naming it.
    fn item(&mut self, seq: &Sequence, track: &Track, item: ItemId) {
        if !shows_picture(track.kind) {
            return;
        }
        if let Some(item) = track.item(item) {
            self.time(seq, Some(item.range));
        }
        for t in track
            .transitions
            .iter()
            .filter(|t| t.outgoing == Some(item) || t.incoming == Some(item))
        {
            self.transition(seq, track, t);
        }
    }

    /// Everything on `track`.
    fn track(&mut self, seq: &Sequence, track: &Track) {
        if !shows_picture(track.kind) {
            return;
        }
        for item in &track.items {
            self.time(seq, Some(item.range));
        }
        for t in &track.transitions {
            self.transition(seq, track, t);
        }
    }

    /// Every clip of `media` in `project`.
    fn media(&mut self, project: &Project, media: MediaId) {
        for seq in project.sequences() {
            for track in &seq.tracks {
                for item in &track.items {
                    if item.clip().is_some_and(|c| c.media == media) {
                        self.item(seq, track, item.id);
                    }
                }
            }
        }
    }

    fn finish(self) -> DirtyRanges {
        let mut sequences: BTreeMap<SequenceId, DirtyFrames> = self
            .ranges
            .into_iter()
            .map(|(id, ranges)| (id, DirtyFrames::from_ranges(ranges)))
            .filter(|(_, frames)| !frames.is_empty())
            .collect();
        for id in self.all {
            sequences.insert(id, DirtyFrames::All);
        }
        DirtyRanges { sequences }
    }
}

struct Roots<'a> {
    old: &'a Project,
    new: &'a Project,
}

impl Roots<'_> {
    /// One changed record: `before` in the old root, `after` in the new (`None` where it
    /// is missing).
    fn changed(&self, c: &mut Collect, before: Option<&Node>, after: Option<&Node>) {
        match (before, after) {
            (Some(Node::Root { settings: a, .. }), Some(Node::Root { settings: b, .. })) => {
                if !same_picture_settings(a, b) {
                    for seq in self.old.sequences().iter().chain(self.new.sequences()) {
                        c.all(seq.id);
                    }
                }
            }
            (Some(Node::Media { media }), _) | (None, Some(Node::Media { media })) => {
                c.media(self.old, media.id);
                c.media(self.new, media.id);
            }
            (
                Some(Node::Sequence { sequence: a, .. }),
                Some(Node::Sequence { sequence: b, .. }),
            ) => {
                if same_picture_format(a, b) {
                    self.track_order(c, a.id);
                } else {
                    c.all(a.id);
                }
            }
            (Some(Node::Sequence { sequence, .. }), _)
            | (None, Some(Node::Sequence { sequence, .. })) => c.all(sequence.id),
            (
                Some(Node::Track {
                    sequence: sa,
                    track: a,
                }),
                Some(Node::Track {
                    sequence: sb,
                    track: b,
                }),
            ) if sa == sb && same_but_name(a, b) => {}
            (Some(Node::Track { track, .. }), _) | (None, Some(Node::Track { track, .. })) => {
                for project in [self.old, self.new] {
                    if let Some((seq, t)) = project.find_track(track.id) {
                        c.track(seq, t);
                    }
                }
            }
            (Some(Node::Item { track: ta, item: a }), Some(Node::Item { track: tb, item: b }))
                if ta == tb && same_picture_item(a, b) => {}
            (Some(Node::Item { track, item }), after) => {
                self.item(c, self.old, *track, item.id);
                if let Some(Node::Item { track, item }) = after {
                    self.item(c, self.new, *track, item.id);
                }
            }
            (None, Some(Node::Item { track, item })) => self.item(c, self.new, *track, item.id),
            (Some(Node::Transition { track, transition }), after) => {
                self.transition(c, self.old, *track, transition);
                if let Some(Node::Transition { track, transition }) = after {
                    self.transition(c, self.new, *track, transition);
                }
            }
            (None, Some(Node::Transition { track, transition })) => {
                self.transition(c, self.new, *track, transition)
            }
            // Markers show nothing; a record that changed kind under one id cannot occur.
            _ => {}
        }
    }

    fn item(&self, c: &mut Collect, project: &Project, track: TrackId, item: ItemId) {
        if let Some((seq, track)) = project.find_track(track) {
            c.item(seq, track, item);
        }
    }

    fn transition(&self, c: &mut Collect, project: &Project, track: TrackId, t: &Transition) {
        if let Some((seq, track)) = project.find_track(track)
            && shows_picture(track.kind)
        {
            c.transition(seq, track, t);
        }
    }

    /// The picture tracks in both roots, in each root's order: every track whose place
    /// among them moved composites in another order wherever it shows.
    fn track_order(&self, c: &mut Collect, sequence: SequenceId) {
        let (Some(a), Some(b)) = (self.old.sequence(sequence), self.new.sequence(sequence)) else {
            return;
        };
        let picture =
            |seq: &Sequence, id: TrackId| seq.track(id).is_some_and(|t| shows_picture(t.kind));
        let order = |seq: &Sequence, other: &Sequence| -> Vec<TrackId> {
            seq.tracks
                .iter()
                .map(|t| t.id)
                .filter(|id| picture(seq, *id) && picture(other, *id))
                .collect()
        };
        let (before, after) = (order(a, b), order(b, a));
        for (i, id) in before.iter().enumerate() {
            if after.get(i) != Some(id) {
                for seq in [a, b] {
                    if let Some(track) = seq.track(*id) {
                        c.track(seq, track);
                    }
                }
            }
        }
    }
}

/// Settings equal in everything the picture depends on.
fn same_picture_settings(a: &ProjectSettings, b: &ProjectSettings) -> bool {
    let mut a = a.clone();
    a.sequence = b.sequence;
    a.cache_folder = b.cache_folder.clone();
    a == *b
}

/// Sequences (without tracks and markers) equal in everything the picture depends on.
fn same_picture_format(a: &Sequence, b: &Sequence) -> bool {
    let mut a = a.clone();
    a.name = b.name.clone();
    a.drop_frame = b.drop_frame;
    a.sample_rate = b.sample_rate;
    a.channel_layout = b.channel_layout;
    a.markers = b.markers.clone();
    a.tracks = b.tracks.clone();
    a == *b
}

/// Tracks (without items and transitions) equal but for their names.
fn same_but_name(a: &Track, b: &Track) -> bool {
    let mut a = a.clone();
    a.name = b.name.clone();
    a == *b
}

/// Items that show the same picture: equal, or clips differing in their markers only.
fn same_picture_item(a: &project::Item, b: &project::Item) -> bool {
    if a == b {
        return true;
    }
    match (&a.kind, &b.kind) {
        (ItemKind::Clip(x), ItemKind::Clip(y)) if a.id == b.id && a.range == b.range => {
            let (mut x, mut y) = (x.clone(), y.clone());
            x.markers = Default::default();
            y.markers = Default::default();
            x == y
        }
        _ => false,
    }
}

#[cfg(test)]
// Dirty frames are lists of ranges, often of one.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use project::params::effects;
    use project::{
        ClipSpec, ColourLabel, ColourSpace, Crop, MarkerOwner, MarkerSpec, Rational, Relink,
        Resolution, SequenceFormat, SpeedKeep, Time, TransitionSpec,
    };

    const FPS: Rational = Rational::new(30, 1);

    fn at(frames: i64) -> Time {
        Time::from_frames(frames, FPS).unwrap()
    }

    fn range(start: i64, end: i64) -> TimeRange {
        TimeRange::between(at(start), at(end)).unwrap()
    }

    struct F {
        p: Project,
        seq: SequenceId,
        v1: TrackId,
        v2: TrackId,
        a1: TrackId,
        m1: MediaId,
        /// V1: 0..30, 30..60, 60..90 (sources 0, 60, 0 s), a cut-free gap after.
        c1: ItemId,
        c2: ItemId,
        c3: ItemId,
        /// V2: 40..70.
        top: ItemId,
        /// A1: 0..90.
        sound: ItemId,
    }

    fn fixture() -> F {
        let p = Project::new("uid", "dirty");
        let (p, m1) = p.add_media("h1", "/media/one.mp4").unwrap();
        let (p, m2) = p.add_media("h2", "/media/two.mov").unwrap();
        let (p, seq) = p.add_sequence("main", FPS, 48_000).unwrap();
        let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).unwrap();
        let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).unwrap();
        let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).unwrap();
        let clip = |media, start, end, source| ClipSpec {
            media,
            range: range(start, end),
            source_start: at(source),
        };
        let (p, c1) = p.insert_clip(v1, clip(m1, 0, 30, 0)).unwrap();
        let (p, c2) = p.insert_clip(v1, clip(m1, 30, 60, 60)).unwrap();
        let (p, c3) = p.insert_clip(v1, clip(m2, 60, 90, 0)).unwrap();
        let (p, top) = p.insert_clip(v2, clip(m2, 40, 70, 0)).unwrap();
        let (p, sound) = p.insert_clip(a1, clip(m1, 0, 90, 0)).unwrap();
        F {
            p,
            seq,
            v1,
            v2,
            a1,
            m1,
            c1,
            c2,
            c3,
            top,
            sound,
        }
    }

    fn frames(old: &Project, new: &Project, seq: SequenceId) -> DirtyFrames {
        DirtyRanges::between(old, new).sequence(seq)
    }

    fn ranges(r: &[Range<i64>]) -> DirtyFrames {
        DirtyFrames::Ranges(r.to_vec())
    }

    #[test]
    fn nothing_changed() {
        let f = fixture();
        assert!(DirtyRanges::between(&f.p, &f.p).is_empty());
        // An equal root built separately changes nothing either.
        let same = f.p.set_clip_opacity(f.c1, 1.0).unwrap();
        assert!(DirtyRanges::between(&f.p, &same).is_empty());
    }

    #[test]
    fn clip_looks_dirty_the_clip() {
        let f = fixture();
        let edits: Vec<Project> = vec![
            f.p.set_clip_opacity(f.c2, 0.5).unwrap(),
            f.p.set_clip_crop(
                f.c2,
                Crop {
                    left: 0.1,
                    ..Crop::default()
                },
            )
            .unwrap(),
            f.p.set_clip_blend(f.c2, project::BlendMode::Multiply)
                .unwrap(),
            f.p.set_clip_transform(
                f.c2,
                project::Transform {
                    rotation: 5.0,
                    ..project::Transform::IDENTITY
                },
            )
            .unwrap(),
            f.p.set_clip_enabled(f.c2, false).unwrap(),
            f.p.add_effect(f.c2, &effects::lut(), None).unwrap().0,
            f.p.slip_clip(f.c2, at(45)).unwrap(),
            f.p.set_clip_speed(f.c2, Rational::new(2, 1), SpeedKeep::Timeline)
                .unwrap(),
        ];
        for (i, edited) in edits.iter().enumerate() {
            assert_eq!(frames(&f.p, edited, f.seq), ranges(&[30..60]), "edit {i}");
        }
    }

    #[test]
    fn trims_and_moves_dirty_old_and_new_extents() {
        let f = fixture();
        let tail = f.p.trim_item(f.c3, range(60, 75)).unwrap();
        assert_eq!(frames(&f.p, &tail, f.seq), ranges(&[60..90]));
        let head = f.p.trim_item(f.top, range(50, 70)).unwrap();
        assert_eq!(frames(&f.p, &head, f.seq), ranges(&[40..70]));
        let moved = f.p.move_item(f.top, f.v2, at(100)).unwrap();
        assert_eq!(frames(&f.p, &moved, f.seq), ranges(&[40..70, 100..130]));
        // Across tracks: the item's record names another track.
        let (p, _) = f.p.remove_item(f.c3).map(|p| (p, ())).unwrap();
        let across = p.move_item(f.top, f.v1, at(60)).unwrap();
        assert_eq!(frames(&p, &across, f.seq), ranges(&[40..90]));
    }

    #[test]
    fn inserted_and_removed_items_dirty_their_extents() {
        let f = fixture();
        let removed = f.p.remove_item(f.c2).unwrap();
        assert_eq!(frames(&f.p, &removed, f.seq), ranges(&[30..60]));
        let (inserted, _) =
            f.p.insert_clip(
                f.v2,
                ClipSpec {
                    media: f.m1,
                    range: range(5, 20),
                    source_start: Time::ZERO,
                },
            )
            .unwrap();
        assert_eq!(frames(&f.p, &inserted, f.seq), ranges(&[5..20]));
        let (subtitle, _) = {
            let (p, track) =
                f.p.add_track(f.seq, TrackKind::Subtitle, "S1", None)
                    .unwrap();
            let (q, id) = p.insert_subtitle(track, range(10, 25), "hello").unwrap();
            ((p, q), id)
        };
        assert_eq!(frames(&subtitle.0, &subtitle.1, f.seq), ranges(&[10..25]));
    }

    #[test]
    fn sound_and_markers_dirty_nothing() {
        let f = fixture();
        let gain = f.p.set_clip_gain(f.sound, -6.0).unwrap();
        assert!(DirtyRanges::between(&f.p, &gain).is_empty());
        let trimmed = f.p.trim_item(f.sound, range(0, 45)).unwrap();
        assert!(DirtyRanges::between(&f.p, &trimmed).is_empty());
        let (marked, _) =
            f.p.add_marker(
                MarkerOwner::Sequence(f.seq),
                MarkerSpec::at(at(12), ColourLabel::Blue),
            )
            .unwrap();
        assert!(DirtyRanges::between(&f.p, &marked).is_empty());
        let (clip_marked, _) =
            f.p.add_marker(
                MarkerOwner::Clip(f.c1),
                MarkerSpec::at(at(3), ColourLabel::Blue),
            )
            .unwrap();
        assert!(DirtyRanges::between(&f.p, &clip_marked).is_empty());
        // An empty track shows nothing; neither does removing the audio track.
        let (added, _) =
            f.p.add_track(f.seq, TrackKind::Video, "V3", Some(0))
                .unwrap();
        assert!(DirtyRanges::between(&f.p, &added).is_empty());
        let removed = f.p.remove_track(f.a1).unwrap();
        assert!(DirtyRanges::between(&f.p, &removed).is_empty());
    }

    #[test]
    fn transitions_dirty_their_spans() {
        let f = fixture();
        let (with, t) =
            f.p.add_transition(TransitionSpec::new(Some(f.c1), Some(f.c2), at(10)))
                .unwrap();
        assert_eq!(frames(&f.p, &with, f.seq), ranges(&[25..35]));
        let longer = with.set_transition_duration(t, at(20)).unwrap();
        assert_eq!(frames(&with, &longer, f.seq), ranges(&[20..40]));
        let kind = with.set_transition_kind(t, "dip-to-black").unwrap();
        assert_eq!(frames(&with, &kind, f.seq), ranges(&[25..35]));
        let easing = with.set_transition_easing(t, "ease-in").unwrap();
        assert_eq!(frames(&with, &easing, f.seq), ranges(&[25..35]));
        let removed = with.remove_transition(t).unwrap();
        assert_eq!(frames(&with, &removed, f.seq), ranges(&[25..35]));
        // A clip under a transition: its extent and the span, which reaches before the
        // incoming clip's start.
        let faded = with.set_clip_opacity(f.c2, 0.5).unwrap();
        assert_eq!(frames(&with, &faded, f.seq), ranges(&[25..60]));
        // An item edit that refits the transition: the trim and the old and new spans.
        let trimmed = with.trim_item(f.c1, range(0, 28)).unwrap();
        let dirty = frames(&with, &trimmed, f.seq);
        assert!(dirty.contains(0) && dirty.contains(34), "{dirty:?}");
        assert!(!dirty.contains(40), "{dirty:?}");
    }

    #[test]
    fn track_changes_dirty_the_whole_track() {
        let f = fixture();
        let hidden = f.p.set_track_kind(f.v2, TrackKind::Audio).unwrap();
        assert_eq!(frames(&f.p, &hidden, f.seq), ranges(&[40..70]));
        let shown = hidden.set_track_kind(f.v2, TrackKind::Video).unwrap();
        assert_eq!(frames(&hidden, &shown, f.seq), ranges(&[40..70]));
        let removed = f.p.remove_track(f.v1).unwrap();
        assert_eq!(frames(&f.p, &removed, f.seq), ranges(&[0..90]));
        // Renaming changes nothing on screen (through the flat form: no edit renames).
        let mut flat = diff::flatten(&f.p);
        if let Some(Node::Track { track, .. }) = flat.get_mut(&f.v2.node()) {
            std::sync::Arc::make_mut(track).name = "Titles".into();
        }
        let renamed = diff::rebuild(&flat).unwrap();
        assert!(DirtyRanges::between(&f.p, &renamed).is_empty());
    }

    #[test]
    fn reordered_tracks_dirty_the_tracks_that_moved() {
        let f = fixture();
        let mut flat = diff::flatten(&f.p);
        if let Some(Node::Sequence { tracks, .. }) = flat.get_mut(&f.seq.node()) {
            // V2, V1, A1 -> A1, V1, V2: the audio track's move is invisible.
            tracks.reverse();
        }
        let reordered = diff::rebuild(&flat).unwrap();
        assert_eq!(frames(&f.p, &reordered, f.seq), ranges(&[0..90]));
        let mut flat = diff::flatten(&f.p);
        if let Some(Node::Sequence { tracks, .. }) = flat.get_mut(&f.seq.node()) {
            // V2, V1, A1 -> V2, A1, V1: the picture tracks keep their order.
            tracks.swap(1, 2);
        }
        let audio_moved = diff::rebuild(&flat).unwrap();
        assert!(DirtyRanges::between(&f.p, &audio_moved).is_empty());
    }

    #[test]
    fn relinked_media_dirties_its_clips() {
        let f = fixture();
        let relinked =
            f.p.relink_media(&[Relink {
                media: f.m1,
                path: "/media/moved.mp4".into(),
                content_hash: "h1-new".into(),
                duration: None,
            }])
            .unwrap();
        // c1 and c2 on V1; the audio clip of the same media shows nothing.
        assert_eq!(frames(&f.p, &relinked, f.seq), ranges(&[0..60]));
        let (unused, _) = f.p.add_media("h3", "/media/three.mp4").unwrap();
        assert!(DirtyRanges::between(&f.p, &unused).is_empty());
    }

    #[test]
    fn sequence_and_settings_changes_dirty_everything() {
        let f = fixture();
        let format = f.p.sequence(f.seq).unwrap().format();
        let bigger =
            f.p.set_sequence_format(
                f.seq,
                SequenceFormat {
                    resolution: Resolution::new(3840, 2160),
                    ..format
                },
            )
            .unwrap();
        assert_eq!(frames(&f.p, &bigger, f.seq), DirtyFrames::All);
        let faster =
            f.p.set_sequence_format(
                f.seq,
                SequenceFormat {
                    frame_rate: Rational::new(60, 1),
                    ..format
                },
            )
            .unwrap();
        assert_eq!(frames(&f.p, &faster, f.seq), DirtyFrames::All);
        let sound =
            f.p.set_sequence_format(
                f.seq,
                SequenceFormat {
                    sample_rate: 96_000,
                    ..format
                },
            )
            .unwrap();
        assert!(DirtyRanges::between(&f.p, &sound).is_empty());

        let mut settings = f.p.settings().clone();
        settings.colour_space = ColourSpace::Rec2020Pq;
        let hdr = f.p.set_project_settings(settings).unwrap();
        assert_eq!(frames(&f.p, &hdr, f.seq), DirtyFrames::All);
        let mut settings = f.p.settings().clone();
        settings.sequence.resolution = Resolution::new(1280, 720);
        let defaults = f.p.set_project_settings(settings).unwrap();
        assert!(DirtyRanges::between(&f.p, &defaults).is_empty());

        let (two, other) = f.p.add_sequence("second", FPS, 48_000).unwrap();
        let dirty = DirtyRanges::between(&f.p, &two);
        assert_eq!(dirty.sequence(other), DirtyFrames::All);
        assert!(
            dirty.sequence(f.seq).is_empty(),
            "the first sequence is untouched"
        );
    }

    #[test]
    fn an_edit_dirties_only_its_own_sequence() {
        let f = fixture();
        let (p, other) = f.p.add_sequence("second", FPS, 48_000).unwrap();
        let (p, v) = p.add_track(other, TrackKind::Video, "V1", None).unwrap();
        let (p, clip) = p
            .insert_clip(
                v,
                ClipSpec {
                    media: f.m1,
                    range: range(0, 10),
                    source_start: Time::ZERO,
                },
            )
            .unwrap();
        let faded = p.set_clip_opacity(clip, 0.2).unwrap();
        let dirty = DirtyRanges::between(&p, &faded);
        assert_eq!(dirty.sequence(other), ranges(&[0..10]));
        assert!(dirty.sequence(f.seq).is_empty());
        assert_eq!(dirty.iter().count(), 1);
    }

    #[test]
    fn frames_round_outward_and_merge() {
        let f = fixture();
        // An item off the frame grid cannot be made, but a range is floored and ceiled.
        let mut c = Collect::default();
        let seq = f.p.sequence(f.seq).unwrap();
        let odd = TimeRange::between(Time::new(1, 60).unwrap(), Time::new(61, 60).unwrap());
        c.time(seq, odd.ok());
        assert_eq!(c.finish().sequence(f.seq), ranges(&[0..31]));
        let merged = DirtyFrames::from_ranges([10..20, 0..5, 5..8, 15..30, 40..40]);
        assert_eq!(merged, ranges(&[0..8, 10..30]));
        assert!(merged.contains(7) && !merged.contains(8) && merged.contains(29));
        assert!(merged.intersects(&(8..11)) && !merged.intersects(&(8..10)));
        assert_eq!(merged.union(&ranges(&[8..10])), ranges(&[0..30]));
        assert!(merged.union(&DirtyFrames::All).is_all());
        let _ = (f.c1, f.c3, f.top, f.v1);
    }
}
