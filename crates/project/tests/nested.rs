//! Nested sequences and compound clips (schema v8): clips of sequences, nesting a
//! selection into a compound and breaking it apart, replacing a clip's source, copying a
//! compound, deleting sequences, cycles, nested lengths in transition checks, clip counts,
//! and the checks on loaded trees.

use project::diff::{Diff, flatten, rebuild};
use project::schema::{self, LoadError};
use project::{
    Alignment, ClipSource, ClipSpec, EditError, EditLabel, History, ItemId, MediaId, Project,
    Rational, SequenceFormat, SequenceId, SpeedKeep, Time, TimeRange, TrackId, TrackKind,
    TransitionId, TransitionSpec, ValidationError,
};
use serde_json::Value;

const FPS: Rational = Rational::new(25, 1);
const TITLES_FPS: Rational = Rational::new(24, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn range(start: i64, len: i64) -> TimeRange {
    TimeRange::new(frames(start), frames(len))
}

fn secs(num: i64, den: i64) -> Time {
    Time::new(num, den).expect("time")
}

struct Fixture {
    project: Project,
    main: SequenceId,
    titles: SequenceId,
    v1: TrackId,
    v2: TrackId,
    a1: TrackId,
    m: MediaId,
    m2: MediaId,
    /// Main V1: A 0..50 from 2 s, B 50..100 from 1 s, C 100..125 from 1 s (`m2`), a
    /// gap, D 150..200 from 4 s; a centred 4-frame transition on the B/C cut.
    a: ItemId,
    b: ItemId,
    c: ItemId,
    d: ItemId,
    bc: TransitionId,
    /// Main V2: E 60..90 from 0 (`m2`).
    e: ItemId,
    /// Main A1: 0..2 s.
    sound: ItemId,
    /// Main S1: 50..60.
    cue: ItemId,
    /// Titles (24 fps), V1: T 0..2 s of `m2`.
    t: ItemId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-nested", "Nested");
    let (p, m) = p.add_media("00ff", "/media/a.mov").expect("m");
    let (p, m2) = p.add_media("11ee", "/media/b.mov").expect("m2");
    let (p, main) = p.add_sequence("Main", FPS, 48_000).expect("main");
    let (p, v1) = p.add_track(main, TrackKind::Video, "V1", None).expect("v1");
    let (p, v2) = p.add_track(main, TrackKind::Video, "V2", None).expect("v2");
    let (p, a1) = p.add_track(main, TrackKind::Audio, "A1", None).expect("a1");
    let (p, s1) = p
        .add_track(main, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let spec = |media, range, source_start| ClipSpec {
        media,
        range,
        source_start,
    };
    let (p, a) = p
        .insert_clip(v1, spec(m, range(0, 50), Time::seconds(2)))
        .expect("a");
    let (p, b) = p
        .insert_clip(v1, spec(m, range(50, 50), Time::seconds(1)))
        .expect("b");
    let (p, c) = p
        .insert_clip(v1, spec(m2, range(100, 25), Time::seconds(1)))
        .expect("c");
    let (p, d) = p
        .insert_clip(v1, spec(m, range(150, 50), Time::seconds(4)))
        .expect("d");
    let (p, e) = p
        .insert_clip(v2, spec(m2, range(60, 30), Time::ZERO))
        .expect("e");
    let (p, sound) = p
        .insert_clip(
            a1,
            spec(m, TimeRange::new(Time::ZERO, Time::seconds(2)), Time::ZERO),
        )
        .expect("sound");
    let (p, cue) = p.insert_subtitle(s1, range(50, 10), "Hi").expect("cue");
    let (p, bc) = p
        .add_transition(TransitionSpec::new(Some(b), Some(c), frames(4)))
        .expect("bc");
    let (p, titles) = p
        .add_sequence("Titles", TITLES_FPS, 48_000)
        .expect("titles");
    let (p, tv1) = p
        .add_track(titles, TrackKind::Video, "V1", None)
        .expect("tv1");
    let (p, t) = p
        .insert_clip(
            tv1,
            spec(m2, TimeRange::new(Time::ZERO, Time::seconds(2)), Time::ZERO),
        )
        .expect("t");
    Fixture {
        project: p,
        main,
        titles,
        v1,
        v2,
        a1,
        m,
        m2,
        a,
        b,
        c,
        d,
        bc,
        e,
        sound,
        cue,
        t,
    }
}

/// `after` is one undo step from `before` under `label`, valid, and survives saving and
/// loading.
fn undoable(before: &Project, after: &Project, label: EditLabel) {
    after.validate().expect("valid");
    let mut h = History::new(before.clone());
    h.commit(label, after.clone());
    assert_eq!(h.undo_label(), Some(label));
    assert!(h.undo().expect("undo").content_eq(before));
    assert!(h.redo().expect("redo").content_eq(after));
    let loaded = schema::load(&schema::save(after)).expect("loads");
    assert!(loaded.content_eq(after));
}

fn clip_source(p: &Project, item: ItemId) -> ClipSource {
    p.find_item(item)
        .and_then(|(_, _, i)| i.clip().map(|c| c.source))
        .expect("clip")
}

fn item_range(p: &Project, item: ItemId) -> TimeRange {
    p.find_item(item).expect("item").2.range
}

fn track_items(p: &Project, track: TrackId) -> Vec<ItemId> {
    let (_, t) = p.find_track(track).expect("track");
    t.items.iter().map(|i| i.id).collect()
}

/// Equal sequences and media, in order: the whole tree but the id counter.
fn same_tree(a: &Project, b: &Project) {
    assert_eq!(a.media(), b.media());
    assert_eq!(a.sequences().len(), b.sequences().len());
    for (x, y) in a.sequences().iter().zip(b.sequences().iter()) {
        assert!(x.content_eq(y), "sequence {} differs", x.id);
    }
}

#[test]
fn nesting_a_selection_makes_a_compound_with_the_parents_layout() {
    let f = fixture();
    let p = &f.project;
    let next = p.next_id();
    let (q, compound, clip) = p
        .nest_items(&[f.c, f.e, f.b, f.cue, f.b], "Opening")
        .expect("nest");
    undoable(p, &q, EditLabel::Nest);
    // Ids in order: the sequence, its four tracks, the clip.
    assert_eq!(compound, SequenceId(next));
    assert_eq!(clip, ItemId(next + 5));
    assert_eq!(q.next_id(), next + 6);

    let seq = q.sequence(compound).expect("compound");
    assert!(seq.compound);
    assert_eq!(seq.name, "Opening");
    assert_eq!(seq.format(), q.sequence(f.main).expect("main").format());
    let layout: Vec<(TrackKind, &str)> = seq
        .tracks
        .iter()
        .map(|t| (t.kind, t.name.as_str()))
        .collect();
    assert_eq!(
        layout,
        [
            (TrackKind::Video, "V1"),
            (TrackKind::Video, "V2"),
            (TrackKind::Audio, "A1"),
            (TrackKind::Subtitle, "S1"),
        ]
    );
    let ids: Vec<TrackId> = seq.tracks.iter().map(|t| t.id).collect();
    assert_eq!(
        ids,
        (next + 1..next + 5).map(TrackId).collect::<Vec<_>>(),
        "new track ids, top to bottom"
    );
    // The items keep their ids and their places relative to the selection's start (50).
    assert_eq!(item_range(&q, f.b), range(0, 50));
    assert_eq!(item_range(&q, f.c), range(50, 25));
    assert_eq!(item_range(&q, f.e), range(10, 30));
    assert_eq!(item_range(&q, f.cue), range(0, 10));
    for item in [f.b, f.c, f.e, f.cue] {
        assert_eq!(q.find_item(item).expect("item").0.id, compound);
    }
    // The B/C transition moved with its clips.
    let (holder, _, _) = q.find_transition(f.bc).expect("bc");
    assert_eq!(holder.id, compound);
    // The parent: the clip on the topmost selected video track over the span 50..125.
    assert_eq!(track_items(&q, f.v1), [f.a, clip, f.d]);
    assert!(track_items(&q, f.v2).is_empty());
    assert_eq!(item_range(&q, clip), range(50, 75));
    let (_, _, node) = q.find_item(clip).expect("clip");
    let c = node.clip().expect("a clip");
    assert_eq!(c.source, ClipSource::Sequence(compound));
    assert_eq!(c.source_start, Time::ZERO);
    assert_eq!(q.sequence_duration(compound), Ok(frames(75)));
    // Hidden from the browser.
    let listed: Vec<SequenceId> = q.browser_sequences().map(|s| s.id).collect();
    assert_eq!(listed, [f.main, f.titles]);
    assert_eq!(q.sequence_users(compound), [(f.main, clip)]);
}

#[test]
fn nesting_then_breaking_apart_gives_back_the_same_tree() {
    let f = fixture();
    let p = &f.project;
    let (q, compound, clip) = p
        .nest_items(&[f.b, f.c, f.e, f.cue], "Opening")
        .expect("nest");
    let r = q.break_apart(clip).expect("break apart");
    undoable(&q, &r, EditLabel::BreakApart);
    same_tree(p, &r);
    assert!(r.sequence(compound).is_none(), "the compound is removed");
    assert!(r.next_id() > p.next_id(), "ids are never reused");
    // An audio selection round-trips the same way, onto its audio track.
    let (q, _, clip) = p.nest_items(&[f.sound], "Sound").expect("nest sound");
    assert_eq!(track_items(&q, f.a1), [clip]);
    same_tree(p, &q.break_apart(clip).expect("break apart"));
}

#[test]
fn transitions_joining_the_selection_to_what_stays_are_removed() {
    let f = fixture();
    let (p, ab) = f
        .project
        .add_transition(TransitionSpec::new(Some(f.a), Some(f.b), frames(10)))
        .expect("ab");
    let (q, compound, clip) = p.nest_items(&[f.b, f.c], "Middle").expect("nest");
    assert!(q.find_transition(ab).is_none(), "A stays, B leaves");
    assert_eq!(q.find_transition(f.bc).expect("bc").0.id, compound);
    // Breaking apart does not bring it back.
    let r = q.break_apart(clip).expect("break apart");
    assert!(r.find_transition(ab).is_none());
    assert_eq!(r.find_transition(f.bc).expect("bc").0.id, f.main);
}

#[test]
fn nesting_refuses_what_cannot_become_one_clip() {
    let f = fixture();
    let p = &f.project;
    assert_eq!(p.nest_items(&[], "x"), Err(EditError::NothingToNest));
    assert_eq!(
        p.nest_items(&[f.b, ItemId(999)], "x"),
        Err(EditError::UnknownItem(ItemId(999)))
    );
    assert_eq!(
        p.nest_items(&[f.b, f.t], "x"),
        Err(EditError::NestAcrossSequences(f.t))
    );
    assert_eq!(
        p.nest_items(&[f.b, f.sound], "x"),
        Err(EditError::NestMixedKinds)
    );
    assert_eq!(
        p.nest_items(&[f.cue], "x"),
        Err(EditError::NestNeedsClipTrack)
    );
    // C and E span 60..125 on V1, where B (50..100) stays.
    assert_eq!(
        p.nest_items(&[f.c, f.e], "x"),
        Err(EditError::Overlap {
            track: f.v1,
            other: f.b
        })
    );
}

#[test]
fn a_sequence_can_be_placed_as_a_clip_but_never_inside_itself() {
    let f = fixture();
    let p = &f.project;
    let (q, nested) = p
        .insert_sequence_clip(f.v2, f.titles, range(125, 25), secs(1, 2))
        .expect("nest titles");
    undoable(p, &q, EditLabel::Insert);
    let (_, _, node) = q.find_item(nested).expect("clip");
    let clip = node.clip().expect("a clip");
    assert_eq!(clip.source, ClipSource::Sequence(f.titles));
    assert_eq!(clip.source_start, secs(1, 2));
    assert_eq!(q.sequence_users(f.titles), [(f.main, nested)]);
    assert!(q.contains_sequence(f.main, f.titles));
    assert!(!q.contains_sequence(f.titles, f.main));
    // Titles cannot hold Main, which holds Titles; nor can a sequence hold itself.
    let (_, tv1) = q
        .sequence(f.titles)
        .map(|s| (s.id, s.tracks[0].id))
        .expect("titles");
    assert_eq!(
        q.insert_sequence_clip(
            tv1,
            f.main,
            TimeRange::new(Time::seconds(3), Time::seconds(1)),
            Time::ZERO
        ),
        Err(EditError::NestingCycle {
            parent: f.titles,
            child: f.main
        })
    );
    assert_eq!(
        q.insert_sequence_clip(f.v2, f.main, range(150, 5), Time::ZERO),
        Err(EditError::NestingCycle {
            parent: f.main,
            child: f.main
        })
    );
    // Through a compound: Main nests Opening, which nests Titles.
    let (r, _, _) = q.nest_items(&[nested], "Wrap").expect("nest");
    assert_eq!(
        r.set_clip_source(f.t, ClipSource::Sequence(f.main)),
        Err(EditError::NestingCycle {
            parent: f.titles,
            child: f.main
        })
    );
    // Argument checks come first.
    assert_eq!(
        p.insert_sequence_clip(f.v2, SequenceId(999), range(125, 5), Time::ZERO),
        Err(EditError::UnknownSequence(SequenceId(999)))
    );
    assert_eq!(
        p.insert_sequence_clip(f.v2, f.titles, range(125, 5), -Time::seconds(1)),
        Err(EditError::SourceBeforeStart(-Time::seconds(1)))
    );
    assert_eq!(
        p.insert_sequence_clip(TrackId(999), f.titles, range(125, 5), Time::ZERO),
        Err(EditError::UnknownTrack(TrackId(999)))
    );
    assert_eq!(
        p.insert_sequence_clip(f.v2, f.titles, range(60, 5), Time::ZERO),
        Err(EditError::Overlap {
            track: f.v2,
            other: f.e
        })
    );
}

#[test]
fn a_clips_source_can_be_replaced_by_a_sequence_and_back() {
    let f = fixture();
    let p = &f.project;
    let q = p
        .set_clip_source(f.d, ClipSource::Sequence(f.titles))
        .expect("to titles");
    undoable(p, &q, EditLabel::ReplaceSource);
    assert_eq!(clip_source(&q, f.d), ClipSource::Sequence(f.titles));
    // Everything else stays: range, source start, properties.
    assert_eq!(item_range(&q, f.d), range(150, 50));
    let r = q
        .set_clip_source(f.d, ClipSource::Media(f.m))
        .expect("back");
    assert!(r.content_eq(p));
    // The same source changes nothing: every sequence node is shared.
    let same = p
        .set_clip_source(f.d, ClipSource::Media(f.m))
        .expect("same");
    assert!(
        same.sequences()
            .iter()
            .zip(p.sequences().iter())
            .all(|(a, b)| std::sync::Arc::ptr_eq(a, b))
    );
    assert_eq!(
        p.set_clip_source(f.cue, ClipSource::Media(f.m)),
        Err(EditError::NotAClip(f.cue))
    );
    assert_eq!(
        p.set_clip_source(f.d, ClipSource::Media(MediaId(999))),
        Err(EditError::UnknownMedia(MediaId(999)))
    );
    assert_eq!(
        p.set_clip_source(f.d, ClipSource::Sequence(SequenceId(999))),
        Err(EditError::UnknownSequence(SequenceId(999)))
    );
    assert_eq!(
        p.set_clip_source(f.d, ClipSource::Sequence(f.main)),
        Err(EditError::NestingCycle {
            parent: f.main,
            child: f.main
        })
    );
}

#[test]
fn breaking_apart_a_shared_sequence_copies_what_the_clip_shows() {
    let f = fixture();
    // Titles shows 0.5 s to 1.5 s at 125..150.
    let (p, nested) = f
        .project
        .insert_sequence_clip(f.v2, f.titles, range(125, 25), secs(1, 2))
        .expect("nest titles");
    let next = p.next_id();
    let q = p.break_apart(nested).expect("break apart");
    undoable(&p, &q, EditLabel::BreakApart);
    // Titles stays as it was; its V1 content is copied onto Main's first video track.
    assert!(
        q.sequence(f.titles)
            .expect("titles")
            .content_eq(p.sequence(f.titles).expect("titles"))
    );
    assert!(q.find_item(nested).is_none());
    let copy = ItemId(next);
    assert_eq!(track_items(&q, f.v1), [f.a, f.b, f.c, copy, f.d]);
    // T, trimmed to the shown half second either side: from source 0.5 s at 125..150.
    assert_eq!(item_range(&q, copy), range(125, 25));
    let (_, _, node) = q.find_item(copy).expect("copy");
    let clip = node.clip().expect("a clip");
    assert_eq!(clip.source, ClipSource::Media(f.m2));
    assert_eq!(clip.source_start, secs(1, 2));
    assert_eq!(q.next_id(), next + 1);
}

#[test]
fn breaking_apart_refuses_what_cannot_come_out_in_place() {
    let f = fixture();
    let p = &f.project;
    assert_eq!(p.break_apart(f.d), Err(EditError::NotNested(f.d)));
    assert_eq!(p.break_apart(f.cue), Err(EditError::NotAClip(f.cue)));
    assert_eq!(
        p.break_apart(ItemId(999)),
        Err(EditError::UnknownItem(ItemId(999)))
    );
    let (q, _, clip) = p.nest_items(&[f.b, f.c], "Middle").expect("nest");
    let fast = q
        .set_clip_speed(clip, Rational::new(2, 1), SpeedKeep::Timeline)
        .expect("speed");
    assert_eq!(
        fast.break_apart(clip),
        Err(EditError::BreakApartSpeed {
            item: clip,
            speed: Rational::new(2, 1)
        })
    );
    // Main's content into Titles, which has one video track: Main's second one has E.
    let (r, deck) = p.add_sequence("Deck", FPS, 48_000).expect("deck");
    let (r, dv1) = r
        .add_track(deck, TrackKind::Video, "V1", None)
        .expect("dv1");
    let (r, whole) = r
        .insert_sequence_clip(dv1, f.main, range(0, 200), Time::ZERO)
        .expect("nest main");
    assert_eq!(
        r.break_apart(whole),
        Err(EditError::BreakApartNoTrack {
            kind: TrackKind::Video,
            index: 1
        })
    );
    // An edge inside the shown range that lands off the parent's grid: T, shortened to
    // end at 1 s and shown from 1/24 s, would last 23/24 s at 25 fps.
    let s = p
        .trim_item(f.t, TimeRange::new(Time::ZERO, Time::seconds(1)))
        .expect("trim t");
    let (s, nested) = s
        .insert_sequence_clip(f.v2, f.titles, range(125, 25), secs(1, 24))
        .expect("nest");
    assert!(matches!(
        s.break_apart(nested),
        Err(EditError::OffGrid {
            kind: TrackKind::Video,
            ..
        })
    ));
}

#[test]
fn a_shared_compound_can_be_made_independent() {
    let f = fixture();
    let (p, compound, first) = f.project.nest_items(&[f.b, f.c], "Middle").expect("nest");
    // A second clip of the same compound (as after copy and paste).
    let (p, second) = p
        .insert_sequence_clip(f.v2, compound, range(125, 25), Time::ZERO)
        .expect("second");
    let next = p.next_id();
    let (q, copy) = p.duplicate_compound(second).expect("duplicate");
    undoable(&p, &q, EditLabel::DuplicateCompound);
    assert_eq!(copy, SequenceId(next));
    assert_eq!(clip_source(&q, second), ClipSource::Sequence(copy));
    assert_eq!(clip_source(&q, first), ClipSource::Sequence(compound));
    let old = q.sequence(compound).expect("old");
    let new = q.sequence(copy).expect("copy");
    assert!(new.compound);
    assert_eq!(new.name, old.name);
    assert_eq!(new.tracks.len(), old.tracks.len());
    for (a, b) in old.tracks.iter().zip(new.tracks.iter()) {
        assert_ne!(a.id, b.id);
        assert_eq!(a.items.len(), b.items.len());
        for (x, y) in a.items.iter().zip(b.items.iter()) {
            assert_ne!(x.id, y.id, "new item ids");
            assert_eq!((x.range, &x.kind), (y.range, &y.kind));
        }
        for (x, y) in a.transitions.iter().zip(b.transitions.iter()) {
            assert_ne!(x.id, y.id);
            // The copy's transition joins the copy's clips.
            assert_eq!(y.outgoing.map(|i| b.item(i).is_some()), Some(true));
            assert_eq!(y.incoming.map(|i| b.item(i).is_some()), Some(true));
            assert_eq!((x.duration, x.alignment), (y.duration, y.alignment));
        }
    }
    // Editing the copy leaves the original alone.
    let copied_b = new.tracks[0].items[0].id;
    let r = q.remove_item(copied_b).expect("edit copy");
    assert!(
        r.sequence(compound)
            .expect("old")
            .content_eq(q.sequence(compound).expect("old"))
    );
    assert_eq!(p.duplicate_compound(f.d), Err(EditError::NotNested(f.d)));
    assert_eq!(p.duplicate_compound(f.cue), Err(EditError::NotAClip(f.cue)));
    let (s, titles_clip) = p
        .insert_sequence_clip(f.v2, f.titles, range(160, 10), Time::ZERO)
        .expect("titles clip");
    assert_eq!(
        s.duplicate_compound(titles_clip),
        Err(EditError::NotACompound(titles_clip))
    );
}

#[test]
fn copying_a_compound_copies_the_compounds_inside_it() {
    let f = fixture();
    let (p, inner, _) = f.project.nest_items(&[f.b, f.c], "Inner").expect("inner");
    let (p, outer, outer_clip) = p
        .nest_items(&track_items(&p, f.v1)[1..2], "Outer")
        .expect("outer");
    let (p, again) = p
        .insert_sequence_clip(f.v2, outer, range(125, 25), Time::ZERO)
        .expect("again");
    let (q, copy) = p.duplicate_compound(again).expect("duplicate");
    let inner_copy = q
        .sequence(copy)
        .expect("copy")
        .nested()
        .next()
        .expect("nested");
    assert_ne!(inner_copy, inner, "a compound inside is copied too");
    assert!(q.sequence(inner_copy).expect("inner copy").compound);
    assert_eq!(clip_source(&q, outer_clip), ClipSource::Sequence(outer));
    q.validate().expect("valid");
}

#[test]
fn a_nested_sequence_cannot_be_deleted() {
    let f = fixture();
    let (p, nested) = f
        .project
        .insert_sequence_clip(f.v2, f.titles, range(125, 25), Time::ZERO)
        .expect("nest");
    assert_eq!(
        p.remove_sequence(f.titles),
        Err(EditError::SequenceInUse {
            sequence: f.titles,
            item: nested
        })
    );
    assert_eq!(
        p.remove_sequence(SequenceId(999)),
        Err(EditError::UnknownSequence(SequenceId(999)))
    );
    let q = p.remove_item(nested).expect("unnest");
    let r = q.remove_sequence(f.titles).expect("delete");
    undoable(&q, &r, EditLabel::RemoveSequence);
    assert!(r.sequence(f.titles).is_none());
    assert!(r.find_item(f.t).is_none());
    // Main nests nothing, so it can go too.
    r.remove_sequence(f.main).expect("delete main");
}

#[test]
fn a_compound_whose_clip_is_deleted_stays_hidden() {
    let f = fixture();
    let (p, compound, clip) = f.project.nest_items(&[f.b, f.c], "Middle").expect("nest");
    let q = p.remove_item(clip).expect("delete clip");
    assert!(q.sequence(compound).expect("kept").compound);
    assert!(q.sequence_users(compound).is_empty());
    assert!(q.browser_sequences().all(|s| s.id != compound));
    q.validate().expect("valid");
}

#[test]
fn transition_handles_on_nested_clips_use_the_sequences_length() {
    let f = fixture();
    // Titles is 2 s long; the clip shows 0 to 1 s of it at 125..150.
    let (p, nested) = f
        .project
        .insert_sequence_clip(f.v2, f.titles, range(125, 25), Time::ZERO)
        .expect("nest");
    assert_eq!(p.sequence_duration(f.titles), Ok(Time::seconds(2)));
    let fade = |n| {
        p.add_transition(TransitionSpec {
            alignment: Alignment::Start,
            ..TransitionSpec::new(Some(nested), None, frames(n))
        })
        .expect("fade out")
    };
    let no_media = |m: MediaId| -> Option<Time> { panic!("asked for media {m}") };
    let (q, t) = fade(25);
    assert_eq!(
        q.transition_needs(t).expect("needs").outgoing,
        Some((nested, ClipSource::Sequence(f.titles), Time::seconds(2)))
    );
    q.check_transition_media(t, no_media)
        .expect("exactly enough");
    let (q, t) = fade(50);
    assert_eq!(
        q.check_transition_media(t, no_media),
        Err(EditError::TransitionHandle {
            item: nested,
            needed: frames(50),
            available: Time::seconds(1),
        })
    );
    assert_eq!(
        p.sequence_duration(SequenceId(999)),
        Err(EditError::UnknownSequence(SequenceId(999)))
    );
}

#[test]
fn clip_counts_go_through_nested_sequences_and_relinking_skips_them() {
    let f = fixture();
    let p = &f.project;
    // m: A, B, D, sound; m2: C, E, T (Titles is a browser sequence).
    assert_eq!(p.clip_count(f.m), 4);
    assert_eq!(p.clip_count(f.m2), 3);
    let (q, compound, _) = p.nest_items(&[f.b, f.c], "Middle").expect("nest");
    assert_eq!(q.clip_count(f.m), 4, "a compound counts through its clip");
    let (q, _) = q
        .insert_sequence_clip(f.v2, compound, range(125, 25), Time::ZERO)
        .expect("again");
    assert_eq!(q.clip_count(f.m), 5, "and once more per clip of it");
    let (q, _) = q
        .insert_sequence_clip(f.v2, f.titles, range(160, 10), Time::ZERO)
        .expect("titles");
    assert_eq!(q.clip_count(f.m2), 5, "T in Titles, and through Main");
    // Clips in compounds still count for relinking; nested clips are never relinked.
    assert_eq!(
        q.source_used_until(f.m2).expect("used"),
        p.source_used_until(f.m2).expect("used")
    );
    let relinked = q
        .relink_media(&[project::Relink {
            media: f.m2,
            path: "/moved/b.mov".into(),
            content_hash: "11ee".into(),
            duration: None,
        }])
        .expect("relink");
    for seq in relinked.sequences() {
        for (a, b) in seq
            .tracks
            .iter()
            .zip(q.sequence(seq.id).expect("seq").tracks.iter())
        {
            assert!(a.content_eq(b));
        }
    }
    // A compound no clip nests counts nowhere.
    let (r, _, clip) = p.nest_items(&[f.d], "End").expect("nest d");
    let r = r.remove_item(clip).expect("delete");
    assert_eq!(r.clip_count(f.m), 3);
}

#[test]
fn a_nested_sequence_shows_its_nearest_frame() {
    let f = fixture();
    let titles = f.project.sequence(f.titles).expect("titles");
    // Main's frame 1 (1/25 s) is 0.96 of a 24 fps frame: frame 1.
    assert_eq!(titles.nearest_frame(frames(1)), Ok(1));
    // Main's frame 12 (0.48 s) is 11.52 frames: 12; frame 25 (1 s) is exactly 24.
    assert_eq!(titles.nearest_frame(frames(12)), Ok(12));
    assert_eq!(titles.nearest_frame(frames(25)), Ok(24));
    // Halves go to the later frame.
    assert_eq!(titles.nearest_frame(secs(1, 48)), Ok(1));
    assert_eq!(titles.nearest_frame(secs(3, 48)), Ok(2));
}

#[test]
fn nesting_edits_travel_as_diffs() {
    let f = fixture();
    let p = &f.project;
    let (q, _, clip) = p.nest_items(&[f.b, f.c, f.e], "Opening").expect("nest");
    for (a, b) in [(p, &q), (&q, &q.break_apart(clip).expect("break apart"))] {
        let (old, new) = (flatten(a), flatten(b));
        let mut flat = old.clone();
        Diff::between(&old, &new).apply(&mut flat);
        assert!(rebuild(&flat).expect("rebuilds").content_eq(b));
    }
}

fn saved(p: &Project) -> Value {
    serde_json::from_slice(&schema::save(p)).expect("JSON")
}

/// Titles' clip T in a saved fixture project.
fn titles_clip(v: &mut Value) -> &mut serde_json::Map<String, Value> {
    v["project"]["sequences"][1]["tracks"][0]["items"][0]
        .as_object_mut()
        .expect("T")
}

fn load(v: &Value) -> Result<Project, LoadError> {
    schema::load(&serde_json::to_vec(v).expect("JSON"))
}

#[test]
fn files_name_a_clips_sequence_and_refuse_cycles() {
    let f = fixture();
    let (p, nested) = f
        .project
        .insert_sequence_clip(f.v2, f.titles, range(125, 25), Time::ZERO)
        .expect("nest");
    let (p, _, _) = p.nest_items(&[f.a], "Start").expect("compound");
    let json = saved(&p);
    let seqs = &json["project"]["sequences"];
    assert_eq!(seqs[0]["compound"], Value::Bool(false));
    assert_eq!(seqs[2]["compound"], Value::Bool(true));
    let v2 = &seqs[0]["tracks"][1]["items"];
    let clip = v2
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["id"] == nested.0)
        .expect("nested clip");
    assert_eq!(clip["sequence"], f.titles.0);
    assert!(clip.get("media").is_none());
    assert!(load(&json).expect("loads").content_eq(&p));

    // T nests Main, which nests Titles.
    let mut cycle = json.clone();
    let t = titles_clip(&mut cycle);
    t.remove("media");
    t.insert("sequence".into(), f.main.0.into());
    assert!(matches!(
        load(&cycle),
        Err(LoadError::Invalid(ValidationError::Edit(
            EditError::NestingCycle { .. }
        )))
    ));
    // Titles nests itself.
    let mut own = json.clone();
    let t = titles_clip(&mut own);
    t.remove("media");
    t.insert("sequence".into(), f.titles.0.into());
    assert!(matches!(
        load(&own),
        Err(LoadError::Invalid(ValidationError::Edit(EditError::NestingCycle {
            parent,
            child
        }))) if parent == f.titles && child == f.titles
    ));
    // A sequence that does not exist.
    let mut unknown = json.clone();
    let t = titles_clip(&mut unknown);
    t.remove("media");
    t.insert("sequence".into(), 999.into());
    assert!(matches!(
        load(&unknown),
        Err(LoadError::Invalid(ValidationError::Track {
            error: EditError::UnknownSequence(SequenceId(999)),
            ..
        }))
    ));
    // A clip with no source at all does not parse.
    let mut none = json.clone();
    titles_clip(&mut none).remove("media");
    assert!(matches!(load(&none), Err(LoadError::Model { .. })));
    // Nor does a sequence without its compound flag.
    let mut flagless = json;
    flagless["project"]["sequences"][0]
        .as_object_mut()
        .expect("main")
        .remove("compound");
    assert!(matches!(load(&flagless), Err(LoadError::Model { .. })));
}

#[test]
fn a_nested_clips_format_may_differ_from_its_parents() {
    let f = fixture();
    // Titles stays at 24 fps inside Main at 25: nothing is conformed.
    let (p, _) = f
        .project
        .insert_sequence_clip(f.v2, f.titles, range(125, 25), Time::ZERO)
        .expect("nest");
    assert_eq!(p.sequence(f.titles).expect("titles").frame_rate, TITLES_FPS);
    let format = SequenceFormat {
        frame_rate: Rational::new(50, 1),
        ..p.sequence(f.main).expect("main").format()
    };
    p.set_sequence_format(f.main, format).expect("main to 50");
}
