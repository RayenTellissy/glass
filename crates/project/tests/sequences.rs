//! Sequences, track kinds, implicit gaps and markers: per-kind item rules, structural
//! invariants under random edits, sharing, undo labels and autosave diffs.

use std::sync::Arc;

use project::diff::{Diff, Node, flatten, rebuild};
use project::history::{EditLabel, History};
use project::schema;
use project::{
    ClipSpec, ColourLabel, EditError, ItemId, ItemKind, MarkerOwner, MarkerSpec, MediaId, Project,
    Rational, SequenceId, Time, TimeRange, TodoState, TrackId, TrackKind,
};
use serde_json::Value;

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn range(start: i64, len: i64) -> TimeRange {
    TimeRange::new(frames(start), frames(len))
}

struct Fixture {
    project: Project,
    media: MediaId,
    seq: SequenceId,
    video: TrackId,
    audio: TrackId,
    subtitle: TrackId,
    adjustment: TrackId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-seq", "Sequences");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, adjustment) = p
        .add_track(seq, TrackKind::Adjustment, "FX", None)
        .expect("adjustment");
    let (p, subtitle) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("subtitle");
    let (p, video) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, audio) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    Fixture {
        project: p,
        media,
        seq,
        video,
        audio,
        subtitle,
        adjustment,
    }
}

fn clip(media: MediaId, start: i64, len: i64) -> ClipSpec {
    ClipSpec {
        media,
        range: range(start, len),
        source_start: Time::ZERO,
    }
}

fn marker(start: i64, colour: ColourLabel) -> MarkerSpec {
    MarkerSpec::at(frames(start), colour)
}

#[test]
fn each_track_kind_accepts_only_its_items() {
    let f = fixture();
    let p = &f.project;
    assert!(p.insert_clip(f.video, clip(f.media, 0, 10)).is_ok());
    assert!(p.insert_clip(f.audio, clip(f.media, 0, 10)).is_ok());
    assert!(p.insert_subtitle(f.subtitle, range(0, 10), "Hello").is_ok());
    assert!(p.insert_adjustment(f.adjustment, range(0, 10)).is_ok());

    let not_allowed = |kind, item| Err(EditError::ItemNotAllowed { kind, item });
    for track in [f.subtitle, f.adjustment] {
        let kind = p.find_track(track).expect("track").1.kind;
        assert_eq!(
            p.insert_clip(track, clip(f.media, 0, 10)).map(|_| ()),
            not_allowed(kind, "clip")
        );
    }
    for track in [f.video, f.audio, f.adjustment] {
        let kind = p.find_track(track).expect("track").1.kind;
        assert_eq!(
            p.insert_subtitle(track, range(0, 10), "x").map(|_| ()),
            not_allowed(kind, "subtitle")
        );
    }
    for track in [f.video, f.audio, f.subtitle] {
        let kind = p.find_track(track).expect("track").1.kind;
        assert_eq!(
            p.insert_adjustment(track, range(0, 10)).map(|_| ()),
            not_allowed(kind, "adjustment")
        );
    }
    for kind in TrackKind::ALL {
        let accepted = [
            ItemKind::Clip(project::Clip {
                media: f.media,
                source_start: Time::ZERO,
                markers: Default::default(),
            }),
            ItemKind::Subtitle(project::Subtitle { text: "x".into() }),
            ItemKind::Adjustment(project::Adjustment::default()),
        ]
        .iter()
        .filter(|item| kind.accepts(item))
        .count();
        assert_eq!(accepted, 1, "{kind:?} accepts exactly one item kind");
    }
}

#[test]
fn subtitle_and_adjustment_items_sit_on_the_frame_grid() {
    let f = fixture();
    let half_frame = TimeRange::new(Time::new(1, 50).expect("t"), frames(4));
    assert!(matches!(
        f.project.insert_subtitle(f.subtitle, half_frame, "x"),
        Err(EditError::OffGrid { .. })
    ));
    assert!(matches!(
        f.project.insert_adjustment(f.adjustment, half_frame),
        Err(EditError::OffGrid { .. })
    ));
    let (p, cue) = f
        .project
        .insert_subtitle(f.subtitle, range(10, 20), "One")
        .expect("cue");
    assert_eq!(
        p.insert_subtitle(f.subtitle, range(25, 10), "Two")
            .map(|_| ()),
        Err(EditError::Overlap {
            track: f.subtitle,
            other: cue
        })
    );
    // Trimming a cue changes only its range.
    let p = p.trim_item(cue, range(12, 8)).expect("trim");
    let (_, _, item) = p.find_item(cue).expect("cue");
    assert_eq!(item.range, range(12, 8));
    assert!(matches!(&item.kind, ItemKind::Subtitle(s) if s.text == "One"));
}

#[test]
fn track_kinds_change_only_when_every_item_fits() {
    let f = fixture();
    // An empty track becomes anything.
    let p = f
        .project
        .set_track_kind(f.subtitle, TrackKind::Video)
        .expect("empty");
    assert_eq!(
        p.find_track(f.subtitle).expect("t").1.kind,
        TrackKind::Video
    );
    // Same kind: nothing changes.
    let same = f
        .project
        .set_track_kind(f.video, TrackKind::Video)
        .expect("same");
    assert!(same.content_eq(&f.project));

    let (p, _) = f
        .project
        .insert_clip(f.video, clip(f.media, 0, 10))
        .expect("clip");
    assert_eq!(
        p.set_track_kind(f.video, TrackKind::Subtitle),
        Err(EditError::ItemNotAllowed {
            kind: TrackKind::Subtitle,
            item: "clip"
        })
    );
    // 25 fps frames are whole sample counts at 48 kHz, so video clips fit an audio track.
    let audio = p.set_track_kind(f.video, TrackKind::Audio).expect("audio");
    audio.validate().expect("valid");
    // A clip one sample long does not fit the frame grid.
    let one_sample = ClipSpec {
        media: f.media,
        range: TimeRange::new(Time::ZERO, Time::from_samples(1, 48_000).expect("t")),
        source_start: Time::ZERO,
    };
    let (p, _) = f
        .project
        .insert_clip(f.audio, one_sample)
        .expect("one sample");
    assert!(matches!(
        p.set_track_kind(f.audio, TrackKind::Video),
        Err(EditError::OffGrid { .. })
    ));
    assert_eq!(
        p.set_track_kind(TrackId(999), TrackKind::Video),
        Err(EditError::UnknownTrack(TrackId(999)))
    );
}

#[test]
fn gaps_are_the_empty_space_between_items() {
    let f = fixture();
    let track = |p: &Project| p.find_track(f.video).expect("v").1.clone();
    assert!(track(&f.project).gaps().expect("gaps").is_empty());

    let (p, a) = f
        .project
        .insert_clip(f.video, clip(f.media, 10, 10))
        .expect("a");
    let (p, b) = p.insert_clip(f.video, clip(f.media, 20, 5)).expect("b");
    let (p, _) = p.insert_clip(f.video, clip(f.media, 40, 5)).expect("c");
    assert_eq!(
        track(&p).gaps().expect("gaps"),
        vec![range(0, 10), range(25, 15)]
    );
    let t = track(&p);
    assert_eq!(t.item_at(frames(10)).expect("at").map(|i| i.id), Some(a));
    assert_eq!(t.item_at(frames(24)).expect("at").map(|i| i.id), Some(b));
    assert_eq!(t.item_at(frames(25)).expect("at").map(|i| i.id), None);
    assert_eq!(t.item_at(frames(5)).expect("at").map(|i| i.id), None);
    assert_eq!(t.item_at(frames(99)).expect("at").map(|i| i.id), None);

    // Removing an item leaves a gap; no gap node appears in the tree.
    let p = p.remove_item(b).expect("remove");
    assert_eq!(
        track(&p).gaps().expect("gaps"),
        vec![range(0, 10), range(20, 20)]
    );
    assert_eq!(track(&p).items.len(), 2);
}

#[test]
fn sequence_markers_stay_sorted_and_on_the_frame_grid() {
    let f = fixture();
    let owner = MarkerOwner::Sequence(f.seq);
    let (p, late) = f
        .project
        .add_marker(owner, marker(50, ColourLabel::Red))
        .expect("late");
    let (p, early) = p
        .add_marker(owner, marker(10, ColourLabel::Blue))
        .expect("early");
    let (p, same) = p
        .add_marker(owner, marker(10, ColourLabel::Green))
        .expect("same start");
    let order = |p: &Project| -> Vec<_> {
        let seq = p.sequence(f.seq).expect("seq");
        seq.markers.iter().map(|m| m.id).collect()
    };
    assert_eq!(order(&p), vec![early, same, late]);

    let off_grid = MarkerSpec::at(Time::new(1, 50).expect("t"), ColourLabel::Amber);
    assert!(matches!(
        p.add_marker(owner, off_grid),
        Err(EditError::MarkerOffGrid { .. })
    ));
    let before_zero = MarkerSpec::at(-frames(1), ColourLabel::Amber);
    assert!(matches!(
        p.add_marker(owner, before_zero),
        Err(EditError::MarkerBeforeZero(_))
    ));
    let mut negative = marker(5, ColourLabel::Amber);
    negative.duration = -frames(2);
    assert!(matches!(
        p.add_marker(owner, negative),
        Err(EditError::NegativeDuration(_))
    ));
    assert_eq!(
        p.add_marker(
            MarkerOwner::Sequence(SequenceId(999)),
            marker(0, ColourLabel::Red)
        )
        .map(|_| ()),
        Err(EditError::UnknownSequence(SequenceId(999)))
    );

    // Moving the late marker first reorders the list and keeps its id and owner.
    let mut spec = p.find_marker(late).expect("late").1.spec();
    spec.start = frames(0);
    spec.duration = frames(25);
    spec.name = "Opening line".into();
    spec.note = "Wind on the last word".into();
    spec.chapter = true;
    spec.todo = TodoState::Open;
    let p = p.update_marker(late, spec.clone()).expect("update");
    assert_eq!(order(&p), vec![late, early, same]);
    let (found_owner, m) = p.find_marker(late).expect("late");
    assert_eq!(found_owner, owner);
    assert_eq!(m.spec(), spec);
    p.validate().expect("valid");

    let p = p.remove_marker(early).expect("remove");
    assert_eq!(order(&p), vec![late, same]);
    assert_eq!(p.remove_marker(early), Err(EditError::UnknownMarker(early)));
    assert_eq!(
        p.update_marker(early, marker(0, ColourLabel::Red)),
        Err(EditError::UnknownMarker(early))
    );
}

#[test]
fn clip_markers_are_in_source_time_and_survive_trims() {
    let f = fixture();
    let (p, item) = f
        .project
        .insert_clip(f.video, clip(f.media, 100, 50))
        .expect("clip");
    // Source time: frame 30 of the file, shown at timeline frame 130.
    let (p, m) = p
        .add_marker(MarkerOwner::Clip(item), marker(30, ColourLabel::Violet))
        .expect("marker");
    let p = p.trim_item(item, range(110, 40)).expect("trim head");
    let p = p.move_item(item, f.video, frames(0)).expect("move");
    let (owner, found) = p.find_marker(m).expect("marker");
    assert_eq!(owner, MarkerOwner::Clip(item));
    assert_eq!(found.start, frames(30));
    let clip = p
        .find_item(item)
        .expect("item")
        .2
        .clip()
        .expect("clip")
        .clone();
    assert_eq!(clip.source_range(range(0, 40)), range(10, 40));

    // A clip marker need not sit on the sequence grid (the source rate is unknown here).
    let odd = MarkerSpec::at(Time::new(1, 3).expect("t"), ColourLabel::Amber);
    assert!(p.add_marker(MarkerOwner::Clip(item), odd).is_ok());

    let (p, cue) = p
        .insert_subtitle(f.subtitle, range(0, 10), "Hi")
        .expect("cue");
    assert_eq!(
        p.add_marker(MarkerOwner::Clip(cue), marker(0, ColourLabel::Red))
            .map(|_| ()),
        Err(EditError::NotAClip(cue))
    );
    assert_eq!(
        p.add_marker(MarkerOwner::Clip(ItemId(999)), marker(0, ColourLabel::Red))
            .map(|_| ()),
        Err(EditError::UnknownItem(ItemId(999)))
    );
    // Removing the clip removes its markers.
    let p = p.remove_item(item).expect("remove");
    assert!(p.find_marker(m).is_none());
    p.validate().expect("valid");
}

#[test]
fn marker_edits_share_everything_they_do_not_touch() {
    let f = fixture();
    let mut p = f.project.clone();
    let mut clips = Vec::new();
    for i in 0..50 {
        let (next, id) = p
            .insert_clip(f.video, clip(f.media, i * 10, 10))
            .expect("v");
        let (next, _) = next
            .insert_clip(f.audio, clip(f.media, i * 10, 10))
            .expect("a");
        p = next;
        clips.push(id);
    }
    let (p, seq_marker) = p
        .add_marker(MarkerOwner::Sequence(f.seq), marker(0, ColourLabel::Blue))
        .expect("seq marker");

    // A sequence marker edit copies the sequence node but no track.
    let mut spec = p.find_marker(seq_marker).expect("m").1.spec();
    spec.name = "Chorus".into();
    let edited = p.update_marker(seq_marker, spec).expect("rename");
    let (before, after) = (
        p.sequence(f.seq).expect("seq"),
        edited.sequence(f.seq).expect("seq"),
    );
    assert!(!Arc::ptr_eq(before, after));
    for (a, b) in before.tracks.iter().zip(after.tracks.iter()) {
        assert!(Arc::ptr_eq(a, b), "track {} copied", a.id);
    }
    assert!(p.media().ptr_eq(edited.media()));

    // A clip marker edit copies that clip and its track only.
    let target = clips[25];
    let (edited, _) = p
        .add_marker(MarkerOwner::Clip(target), marker(3, ColourLabel::Green))
        .expect("clip marker");
    let (before, after) = (
        p.sequence(f.seq).expect("seq"),
        edited.sequence(f.seq).expect("seq"),
    );
    assert!(
        Arc::ptr_eq(&before.markers[0], &after.markers[0]),
        "sequence marker copied"
    );
    for track in [f.audio, f.subtitle, f.adjustment] {
        assert!(Arc::ptr_eq(
            before.track(track).expect("t"),
            after.track(track).expect("t")
        ));
    }
    let (v_before, v_after) = (
        before.track(f.video).expect("v"),
        after.track(f.video).expect("v"),
    );
    let mut shared = 0;
    for (a, b) in v_before.items.iter().zip(v_after.items.iter()) {
        if a.id == target {
            assert!(!Arc::ptr_eq(a, b));
        } else {
            assert!(Arc::ptr_eq(a, b), "item {} copied", a.id);
            shared += 1;
        }
    }
    assert_eq!(shared, 49);

    // Changing a track's kind copies that track's node, not its items.
    let changed = p.set_track_kind(f.video, TrackKind::Audio).expect("kind");
    let (before, after) = (
        p.sequence(f.seq).expect("seq"),
        changed.sequence(f.seq).expect("seq"),
    );
    let (tb, ta) = (
        before.track(f.video).expect("v"),
        after.track(f.video).expect("v"),
    );
    assert_eq!(tb.items.len(), ta.items.len());
    for (a, b) in tb.items.iter().zip(ta.items.iter()) {
        assert!(Arc::ptr_eq(a, b), "item {} copied", a.id);
    }
    assert!(Arc::ptr_eq(
        before.track(f.audio).expect("a"),
        after.track(f.audio).expect("a")
    ));
}

/// A small deterministic generator, so a failure replays.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

/// The invariants every root must keep, checked directly rather than through
/// `validate`, so the two cannot share a bug.
fn assert_structure(p: &Project) {
    p.validate().expect("valid");
    for seq in p.sequences() {
        for pair in seq.markers.iter().collect::<Vec<_>>().windows(2) {
            assert!((pair[0].start, pair[0].id) < (pair[1].start, pair[1].id));
        }
        for track in &seq.tracks {
            for item in &track.items {
                assert!(
                    track.kind.accepts(&item.kind),
                    "{:?} on {:?}",
                    item.kind,
                    track.kind
                );
            }
            for pair in track.items.iter().collect::<Vec<_>>().windows(2) {
                let end = pair[0].range.end().expect("end");
                assert!(end <= pair[1].range.start, "overlap on {}", track.id);
            }
        }
    }
}

#[test]
fn random_edits_keep_every_structural_invariant() {
    let f = fixture();
    let tracks = [f.video, f.audio, f.subtitle, f.adjustment];
    let mut rng = Lcg(0x5eed);
    let mut p = f.project.clone();
    let mut items: Vec<ItemId> = Vec::new();
    let mut markers = Vec::new();
    let (mut accepted, mut rejected) = (0, 0);
    for _ in 0..2000 {
        let track = tracks[rng.next(4) as usize];
        let r = range(rng.next(200) as i64, 1 + rng.next(30) as i64);
        let result: Result<Project, EditError> = match rng.next(10) {
            0 => p
                .insert_clip(track, clip(f.media, r.start.frames_floor(FPS), 5))
                .map(|(n, id)| {
                    items.push(id);
                    n
                }),
            1 => p.insert_subtitle(track, r, "cue").map(|(n, id)| {
                items.push(id);
                n
            }),
            2 => p.insert_adjustment(track, r).map(|(n, id)| {
                items.push(id);
                n
            }),
            3 if !items.is_empty() => {
                let item = items[rng.next(items.len() as u64) as usize];
                p.move_item(item, track, r.start)
            }
            4 if !items.is_empty() => {
                let item = items[rng.next(items.len() as u64) as usize];
                p.trim_item(item, r)
            }
            5 if !items.is_empty() => {
                let item = items[rng.next(items.len() as u64) as usize];
                p.remove_item(item)
            }
            6 => {
                let kind = TrackKind::ALL[rng.next(4) as usize];
                p.set_track_kind(track, kind)
            }
            7 if !items.is_empty() => {
                let item = items[rng.next(items.len() as u64) as usize];
                let spec = marker(rng.next(40) as i64, ColourLabel::Red);
                p.add_marker(MarkerOwner::Clip(item), spec).map(|(n, id)| {
                    markers.push(id);
                    n
                })
            }
            8 => {
                let spec = marker(rng.next(300) as i64, ColourLabel::Amber);
                p.add_marker(MarkerOwner::Sequence(f.seq), spec)
                    .map(|(n, id)| {
                        markers.push(id);
                        n
                    })
            }
            _ if !markers.is_empty() => {
                let m = markers[rng.next(markers.len() as u64) as usize];
                if rng.next(2) == 0 {
                    p.remove_marker(m)
                } else {
                    p.update_marker(m, marker(rng.next(300) as i64, ColourLabel::Green))
                }
            }
            _ => continue,
        };
        match result {
            Ok(next) => {
                accepted += 1;
                p = next;
                assert_structure(&p);
            }
            Err(_) => rejected += 1,
        }
    }
    assert!(accepted > 300 && rejected > 300, "{accepted} / {rejected}");
    // The final tree survives saving and loading.
    let loaded = schema::load(&schema::save(&p)).expect("loads");
    assert!(loaded.content_eq(&p));
}

#[test]
fn marker_and_track_kind_edits_undo_with_their_labels() {
    let f = fixture();
    let mut h = History::new(f.project.clone());
    let (p, m) = h
        .current()
        .add_marker(MarkerOwner::Sequence(f.seq), marker(5, ColourLabel::Blue))
        .expect("add");
    h.commit(EditLabel::AddMarker, p);
    let mut spec = h.current().find_marker(m).expect("m").1.spec();
    spec.colour = ColourLabel::Red;
    let p = h.current().update_marker(m, spec).expect("update");
    h.commit(EditLabel::ChangeMarker, p);
    let p = h.current().remove_marker(m).expect("remove");
    h.commit(EditLabel::DeleteMarker, p);
    let p = h
        .current()
        .set_track_kind(f.subtitle, TrackKind::Adjustment)
        .expect("kind");
    h.commit(EditLabel::ChangeTrackKind, p);

    let mut labels = Vec::new();
    while let Some(label) = h.undo_label() {
        labels.push(label.i18n_key());
        h.undo();
    }
    assert_eq!(
        labels,
        [
            "edit.label.change_track_kind",
            "edit.label.delete_marker",
            "edit.label.change_marker",
            "edit.label.add_marker",
        ]
    );
    assert!(h.current().content_eq(&f.project));
    h.redo();
    h.redo();
    assert_eq!(
        h.current().find_marker(m).expect("m").1.colour,
        ColourLabel::Red
    );
}

#[test]
fn every_edit_label_has_an_english_name() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/i18n/en.json");
    let en: Value = serde_json::from_slice(&std::fs::read(path).expect("en.json")).expect("JSON");
    for label in EditLabel::ALL {
        assert!(
            en["strings"][label.i18n_key()].is_string(),
            "{} missing from en.json",
            label.i18n_key()
        );
    }
}

#[test]
fn autosave_diffs_carry_marker_and_track_kind_edits() {
    let f = fixture();
    let (p, item) = f
        .project
        .insert_clip(f.video, clip(f.media, 0, 50))
        .expect("clip");
    let (p, seq_marker) = p
        .add_marker(MarkerOwner::Sequence(f.seq), marker(5, ColourLabel::Blue))
        .expect("marker");
    let base = flatten(&p);

    // A sequence marker is its own record: renaming it rewrites only that record.
    let mut spec = p.find_marker(seq_marker).expect("m").1.spec();
    spec.note = "Gulls cue".into();
    let renamed = p.update_marker(seq_marker, spec).expect("update");
    let diff = Diff::between(&base, &flatten(&renamed));
    assert!(diff.removals.is_empty());
    assert!(
        matches!(diff.upserts.as_slice(), [Node::Marker { sequence, marker }]
            if *sequence == f.seq && marker.note == "Gulls cue"),
        "{diff:?}"
    );

    // A clip marker travels inside its item; the header carries the new id counter.
    let (with_clip_marker, _) = renamed
        .add_marker(MarkerOwner::Clip(item), marker(2, ColourLabel::Amber))
        .expect("clip marker");
    let diff2 = Diff::between(&flatten(&renamed), &flatten(&with_clip_marker));
    assert_eq!(diff2.upserts.len(), 2, "{diff2:?}");
    assert!(diff2.upserts.iter().any(|n| matches!(n, Node::Root { .. })));
    assert!(
        diff2
            .upserts
            .iter()
            .any(|n| matches!(n, Node::Item { item: i, .. } if i.id == item))
    );

    // A track kind change rewrites the bare track record only.
    let changed = with_clip_marker
        .set_track_kind(f.subtitle, TrackKind::Adjustment)
        .expect("kind");
    let diff3 = Diff::between(&flatten(&with_clip_marker), &flatten(&changed));
    assert!(
        matches!(diff3.upserts.as_slice(), [Node::Track { track, .. }]
            if track.kind == TrackKind::Adjustment),
        "{diff3:?}"
    );

    // Removing the sequence marker removes its record.
    let removed = changed.remove_marker(seq_marker).expect("remove");
    let diff4 = Diff::between(&flatten(&changed), &flatten(&removed));
    assert_eq!(diff4.removals, vec![seq_marker.node()]);

    // Applying the diffs in order to the first flat form rebuilds the last root.
    let mut flat = base;
    for d in [&diff, &diff2, &diff3, &diff4] {
        d.apply(&mut flat);
    }
    let rebuilt = rebuild(&flat).expect("rebuilds");
    assert!(rebuilt.content_eq(&removed));
    // Diffs round-trip through the autosave log's JSON.
    let line = serde_json::to_string(&diff).expect("JSON");
    assert_eq!(serde_json::from_str::<Diff>(&line).expect("parse"), diff);
}

#[test]
fn validation_rejects_items_on_the_wrong_kind_of_track_and_unsorted_markers() {
    let f = fixture();
    let (p, _) = f
        .project
        .insert_subtitle(f.subtitle, range(0, 10), "Hi")
        .expect("cue");
    let (p, _) = p
        .add_marker(MarkerOwner::Sequence(f.seq), marker(0, ColourLabel::Blue))
        .expect("m");
    let (p, _) = p
        .add_marker(MarkerOwner::Sequence(f.seq), marker(5, ColourLabel::Blue))
        .expect("m");
    let saved: Value = serde_json::from_slice(&schema::save(&p)).expect("JSON");

    // The subtitle track declared as video.
    let mut wrong_kind = saved.clone();
    let tracks = wrong_kind["project"]["sequences"][0]["tracks"]
        .as_array_mut()
        .expect("tracks");
    let subtitle = tracks
        .iter_mut()
        .find(|t| t["kind"] == "subtitle")
        .expect("subtitle track");
    subtitle["kind"] = Value::from("video");
    let err = schema::load(&serde_json::to_vec(&wrong_kind).expect("JSON")).expect_err("invalid");
    assert!(err.to_string().contains("cannot hold a subtitle"), "{err}");

    // Markers out of order.
    let mut unsorted = saved;
    unsorted["project"]["sequences"][0]["markers"]
        .as_array_mut()
        .expect("markers")
        .reverse();
    let err = schema::load(&serde_json::to_vec(&unsorted).expect("JSON")).expect_err("invalid");
    assert!(err.to_string().contains("markers are not ordered"), "{err}");
}
