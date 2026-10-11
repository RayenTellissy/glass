//! Contract tests for nested sequences and compound clips (schema v8, Decision 69),
//! written against the contract rather than the implementation: cycles through chains of
//! sequences in edits, records and files, the order of refusals when several rules are
//! broken, the nested frame shown at differing frame rates and past the nested end,
//! nesting selections across tracks with gaps and fades, breaking apart (track mapping by
//! kind, trims at the shown range's edges, moving a compound's last clip versus copying a
//! shared one), independent copies of compounds, deleting nested sequences, transition
//! handles of nested clips, clip counts through nesting levels, reduced frame rates,
//! undo, redo, diffs, autosave and save of every nesting edit, refused files, and the v7
//! migration.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use project::autosave::{self, AutosaveWriter};
use project::diff::{Diff, Node, flatten, rebuild};
use project::params::effects;
use project::schema::{self, LoadError, SCHEMA_VERSION};
use project::{
    Alignment, Clip, ClipSource, ClipSpec, ColourLabel, EditError, EditLabel, HandleNeeds, History,
    Item, ItemId, ItemKind, MarkerOwner, MarkerSpec, MediaId, Project, Rational, Sequence,
    SequenceId, SpeedKeep, Time, TimeRange, TrackId, TrackKind, TransitionId, TransitionSpec,
    ValidationError,
};
use serde_json::{Map, Value, json};

const FPS: Rational = Rational::new(25, 1);

fn f(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn at(start: i64, len: i64) -> TimeRange {
    TimeRange::new(f(start), f(len))
}

fn secs(num: i64, den: i64) -> Time {
    Time::new(num, den).expect("time")
}

fn plus(a: Time, b: Time) -> Time {
    a.checked_add(b).expect("add")
}

fn minus(a: Time, b: Time) -> Time {
    a.checked_sub(b).expect("sub")
}

fn spec(media: MediaId, range: TimeRange, source_start: Time) -> ClipSpec {
    ClipSpec {
        media,
        range,
        source_start,
    }
}

fn clip_of(p: &Project, item: ItemId) -> &Clip {
    p.find_item(item).expect("item").2.clip().expect("clip")
}

/// The sequence and track holding `item`, and its range.
fn place_of(p: &Project, item: ItemId) -> (SequenceId, TrackId, TimeRange) {
    let (s, t, i) = p.find_item(item).expect("item");
    (s.id, t.id, i.range)
}

fn items_on(p: &Project, track: TrackId) -> Vec<ItemId> {
    p.find_track(track)
        .expect("track")
        .1
        .items
        .iter()
        .map(|i| i.id)
        .collect()
}

/// Each clip on `track`: its range, source and source start.
fn clips_on(p: &Project, track: TrackId) -> Vec<(TimeRange, ClipSource, Time)> {
    p.find_track(track)
        .expect("track")
        .1
        .items
        .iter()
        .map(|i| {
            let c = i.clip().expect("clip");
            (i.range, c.source, c.source_start)
        })
        .collect()
}

fn holder_of(p: &Project, transition: TransitionId) -> Option<SequenceId> {
    p.find_transition(transition).map(|(s, _, _)| s.id)
}

/// The whole tree but the id counter: media and every sequence, in order.
fn same_tree(a: &Project, b: &Project) {
    assert_eq!(a.media(), b.media());
    let ids = |p: &Project| p.sequences().iter().map(|s| s.id).collect::<Vec<_>>();
    assert_eq!(ids(a), ids(b));
    for (x, y) in a.sequences().iter().zip(b.sequences().iter()) {
        assert!(x.content_eq(y), "sequence {} differs", x.id);
    }
}

fn saved(p: &Project) -> Value {
    serde_json::from_slice(&schema::save(p)).expect("JSON")
}

fn load(v: &Value) -> Result<Project, LoadError> {
    schema::load(&serde_json::to_vec(v).expect("JSON"))
}

/// Three sequences at 25 fps, each with a video track: A nests B and C, B nests C, and C
/// holds a clip of media. A also has a subtitle track with a cue; C an empty one.
struct Chain {
    project: Project,
    m: MediaId,
    a: SequenceId,
    b: SequenceId,
    c: SequenceId,
    av1: TrackId,
    as1: TrackId,
    bv1: TrackId,
    cv1: TrackId,
    cs1: TrackId,
    /// A V1: B at 0..25, C at 25..50.
    a_b: ItemId,
    a_c: ItemId,
    /// A S1: 60..65.
    cue: ItemId,
    /// B V1: C at 0..25.
    b_c: ItemId,
    /// C V1: media 0..25.
    c_m: ItemId,
}

fn chain() -> Chain {
    let p = Project::new("uid-chain", "Chain");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("m");
    let (p, a) = p.add_sequence("A", FPS, 48_000).expect("a");
    let (p, b) = p.add_sequence("B", FPS, 48_000).expect("b");
    let (p, c) = p.add_sequence("C", FPS, 48_000).expect("c");
    let (p, av1) = p.add_track(a, TrackKind::Video, "V1", None).expect("av1");
    let (p, as1) = p
        .add_track(a, TrackKind::Subtitle, "S1", None)
        .expect("as1");
    let (p, bv1) = p.add_track(b, TrackKind::Video, "V1", None).expect("bv1");
    let (p, cv1) = p.add_track(c, TrackKind::Video, "V1", None).expect("cv1");
    let (p, cs1) = p
        .add_track(c, TrackKind::Subtitle, "S1", None)
        .expect("cs1");
    let (p, c_m) = p
        .insert_clip(cv1, spec(m, at(0, 25), Time::ZERO))
        .expect("c_m");
    let (p, b_c) = p
        .insert_sequence_clip(bv1, c, at(0, 25), Time::ZERO)
        .expect("b_c");
    let (p, a_b) = p
        .insert_sequence_clip(av1, b, at(0, 25), Time::ZERO)
        .expect("a_b");
    let (p, a_c) = p
        .insert_sequence_clip(av1, c, at(25, 25), Time::ZERO)
        .expect("a_c");
    let (p, cue) = p.insert_subtitle(as1, at(60, 5), "Hi").expect("cue");
    Chain {
        project: p,
        m,
        a,
        b,
        c,
        av1,
        as1,
        bv1,
        cv1,
        cs1,
        a_b,
        a_c,
        cue,
        b_c,
        c_m,
    }
}

/// The error of a refused edit.
fn refused<T: std::fmt::Debug>(result: Result<T, EditError>) -> EditError {
    result.expect_err("refused")
}

#[test]
fn cycles_are_refused_directly_and_through_every_level_by_both_edits() {
    let c = chain();
    // Without A's own clip of C, A reaches C only through B.
    let p = &c.project.remove_item(c.a_c).expect("A drops C");
    let cycle = |parent, child| EditError::NestingCycle { parent, child };
    // A ⊃ B ⊃ A.
    assert_eq!(
        refused(p.insert_sequence_clip(c.bv1, c.a, at(50, 10), Time::ZERO)),
        cycle(c.b, c.a)
    );
    assert_eq!(
        refused(p.set_clip_source(c.b_c, ClipSource::Sequence(c.a))),
        cycle(c.b, c.a)
    );
    // A ⊃ B ⊃ C ⊃ A, and B ⊃ C ⊃ B.
    assert_eq!(
        refused(p.insert_sequence_clip(c.cv1, c.a, at(50, 10), Time::ZERO)),
        cycle(c.c, c.a)
    );
    assert_eq!(
        refused(p.insert_sequence_clip(c.cv1, c.b, at(50, 10), Time::ZERO)),
        cycle(c.c, c.b)
    );
    assert_eq!(
        refused(p.set_clip_source(c.c_m, ClipSource::Sequence(c.a))),
        cycle(c.c, c.a)
    );
    assert_eq!(
        refused(p.set_clip_source(c.c_m, ClipSource::Sequence(c.b))),
        cycle(c.c, c.b)
    );
    // Nesting what is already nested elsewhere, or twice, is no cycle.
    c.project
        .validate()
        .expect("A nesting C directly and through B is no cycle");
    let (q, _) = c
        .project
        .insert_sequence_clip(c.av1, c.c, at(50, 25), Time::ZERO)
        .expect("C again in A");
    q.validate().expect("valid");
    p.set_clip_source(c.a_b, ClipSource::Sequence(c.c))
        .expect("A's clip of B to C");
    // The check reads the current tree: once B no longer nests C, C may nest A.
    let unchained = p
        .set_clip_source(c.b_c, ClipSource::Media(c.m))
        .expect("B to media");
    let (r, _) = unchained
        .insert_sequence_clip(c.cv1, c.a, at(50, 10), Time::ZERO)
        .expect("C nests A");
    r.validate().expect("valid");
}

#[test]
fn inserting_a_sequence_clip_reports_the_first_broken_rule_in_contract_order() {
    let c = chain();
    let p = &c.project;
    let early = -Time::seconds(1);
    // UnknownSequence, then SourceBeforeStart, then UnknownTrack.
    assert_eq!(
        p.insert_sequence_clip(TrackId(999), SequenceId(999), at(0, 5), early),
        Err(EditError::UnknownSequence(SequenceId(999)))
    );
    assert_eq!(
        p.insert_sequence_clip(TrackId(999), c.a, at(0, 5), early),
        Err(EditError::SourceBeforeStart(early))
    );
    assert_eq!(
        p.insert_sequence_clip(TrackId(999), c.a, at(0, 5), Time::ZERO),
        Err(EditError::UnknownTrack(TrackId(999)))
    );
    // NestingCycle comes before every placement error: a subtitle track, off the grid,
    // over C's clip.
    let cycle = Err(EditError::NestingCycle {
        parent: c.c,
        child: c.a,
    });
    assert_eq!(
        p.insert_sequence_clip(c.cs1, c.a, at(0, 5), Time::ZERO),
        cycle
    );
    assert_eq!(
        p.insert_sequence_clip(c.cv1, c.a, TimeRange::new(secs(1, 50), f(5)), Time::ZERO),
        cycle
    );
    // Then placement, as for media clips.
    assert_eq!(
        p.insert_sequence_clip(c.as1, c.c, at(70, 5), Time::ZERO),
        Err(EditError::ItemNotAllowed {
            kind: TrackKind::Subtitle,
            item: "clip"
        })
    );
    assert_eq!(
        p.insert_sequence_clip(c.av1, c.c, TimeRange::new(secs(1, 50), f(5)), Time::ZERO),
        Err(EditError::OffGrid {
            time: secs(1, 50),
            grid: f(1),
            kind: TrackKind::Video
        })
    );
    assert_eq!(
        p.insert_sequence_clip(c.av1, c.c, at(10, 5), Time::ZERO),
        Err(EditError::Overlap {
            track: c.av1,
            other: c.a_b
        })
    );
}

#[test]
fn replacing_a_source_reports_the_first_broken_rule_in_contract_order() {
    let c = chain();
    let p = &c.project;
    assert_eq!(
        p.set_clip_source(ItemId(999), ClipSource::Sequence(SequenceId(999))),
        Err(EditError::UnknownItem(ItemId(999)))
    );
    assert_eq!(
        p.set_clip_source(c.cue, ClipSource::Sequence(SequenceId(999))),
        Err(EditError::NotAClip(c.cue))
    );
    assert_eq!(
        p.set_clip_source(c.cue, ClipSource::Media(MediaId(999))),
        Err(EditError::NotAClip(c.cue))
    );
    assert_eq!(
        p.set_clip_source(c.c_m, ClipSource::Media(MediaId(999))),
        Err(EditError::UnknownMedia(MediaId(999)))
    );
    // A sequence's id is no media id, nor the other way round.
    assert_eq!(
        p.set_clip_source(c.c_m, ClipSource::Media(MediaId(c.b.0))),
        Err(EditError::UnknownMedia(MediaId(c.b.0)))
    );
    assert_eq!(
        p.set_clip_source(c.c_m, ClipSource::Sequence(SequenceId(c.m.0))),
        Err(EditError::UnknownSequence(SequenceId(c.m.0)))
    );
}

/// Sets the source of the first item of the first track of sequence `seq` in a saved
/// project to `key` (`"media"` or `"sequence"`) `id`.
fn set_source(v: &mut Value, seq: usize, key: &str, id: u64) {
    let item = first_item(v, seq);
    item.remove("media");
    item.remove("sequence");
    item.insert(key.into(), id.into());
}

fn first_item(v: &mut Value, seq: usize) -> &mut Map<String, Value> {
    v["project"]["sequences"][seq]["tracks"][0]["items"][0]
        .as_object_mut()
        .expect("item")
}

#[test]
fn loading_refuses_cycles_through_several_sequences_and_dangling_sources() {
    let c = chain();
    let json = saved(&c.project);
    assert!(load(&json).expect("a diamond loads").content_eq(&c.project));
    let chain_ids = [c.a, c.b, c.c];
    // C's media clip nests A: A ⊃ B ⊃ C ⊃ A, A's own clip of C removed.
    let mut cycle = saved(&c.project.remove_item(c.a_c).expect("A drops C"));
    set_source(&mut cycle, 2, "sequence", c.a.0);
    match load(&cycle) {
        Err(LoadError::Invalid(ValidationError::Edit(EditError::NestingCycle {
            parent,
            child,
        }))) => assert!(chain_ids.contains(&parent) && chain_ids.contains(&child)),
        other => panic!("{other:?}"),
    }
    // B's clip of C nests B itself.
    let mut own = json.clone();
    set_source(&mut own, 1, "sequence", c.b.0);
    assert!(matches!(
        load(&own),
        Err(LoadError::Invalid(ValidationError::Edit(EditError::NestingCycle { parent, child })))
            if parent == c.b && child == c.b
    ));
    // A sequence that is not there, and ids of the other kind.
    let track_error = |v: &Value| match load(v) {
        Err(LoadError::Invalid(ValidationError::Track { track, error })) => (track, error),
        other => panic!("{other:?}"),
    };
    let mut dangling = json.clone();
    set_source(&mut dangling, 1, "sequence", 999);
    assert_eq!(
        track_error(&dangling),
        (c.bv1, EditError::UnknownSequence(SequenceId(999)))
    );
    let mut media_as_sequence = json.clone();
    set_source(&mut media_as_sequence, 1, "sequence", c.m.0);
    assert_eq!(
        track_error(&media_as_sequence),
        (c.bv1, EditError::UnknownSequence(SequenceId(c.m.0)))
    );
    let mut sequence_as_media = json;
    set_source(&mut sequence_as_media, 2, "media", c.b.0);
    assert_eq!(
        track_error(&sequence_as_media),
        (c.cv1, EditError::UnknownMedia(MediaId(c.b.0)))
    );

    // Autosave records rebuild through the same checks.
    let mut flat = flatten(&c.project.remove_item(c.a_c).expect("A drops C"));
    let Some(Node::Item { track, item }) = flat.get(&c.c_m.node()).cloned() else {
        panic!("C's clip record");
    };
    let mut item = Item::clone(&item);
    let ItemKind::Clip(clip) = &mut item.kind else {
        panic!("a clip");
    };
    clip.source = ClipSource::Sequence(c.a);
    flat.insert(
        c.c_m.node(),
        Node::Item {
            track,
            item: Arc::new(item),
        },
    );
    assert!(matches!(
        rebuild(&flat),
        Err(ValidationError::Edit(EditError::NestingCycle { .. }))
    ));
}

#[test]
fn loading_refuses_malformed_sources_and_compound_flags_as_model_errors() {
    let c = chain();
    let json = saved(&c.project);
    let model = |v: &Value| matches!(load(v), Err(LoadError::Model { .. }));
    let mut none = json.clone();
    first_item(&mut none, 2).remove("media");
    assert!(model(&none), "a clip with no source");
    let mut text = json.clone();
    first_item(&mut text, 1).insert("sequence".into(), json!("C"));
    assert!(model(&text), "a sequence named by text");
    let mut flag = json.clone();
    flag["project"]["sequences"][0]["compound"] = json!("no");
    assert!(model(&flag), "a compound flag that is not a boolean");
    let mut flagless = json;
    flagless["project"]["sequences"][2]
        .as_object_mut()
        .expect("C")
        .remove("compound");
    assert!(model(&flagless), "a sequence without its compound flag");
}

#[test]
#[ignore = "suspected bug: a clip naming both \"media\" and \"sequence\" loads, silently taking one of them"]
fn loading_refuses_a_clip_that_names_both_a_media_and_a_sequence() {
    let c = chain();
    let mut both = saved(&c.project);
    // C's clip keeps its media and also names B.
    first_item(&mut both, 2).insert("sequence".into(), c.b.0.into());
    assert!(
        matches!(load(&both), Err(LoadError::Model { .. })),
        "{:?}",
        load(&both).map(|p| clip_of(&p, c.c_m).source)
    );
}

/// The project the nesting tests start from. Main at 25 fps, tracks top to bottom V1, V2,
/// V3, A1, S1 and FX (adjustment), and Other with one clip.
struct World {
    project: Project,
    main: SequenceId,
    v1: TrackId,
    v2: TrackId,
    v3: TrackId,
    a1: TrackId,
    /// V1: 12..18, inside the span of `p1` and `q`.
    x: ItemId,
    /// V2: 0..10 from 3 s, then `p1` 10..20 from 1 s (with an effect and a marker), a
    /// gap, then `u2` 40..50 from 5 s.
    u1: ItemId,
    p1: ItemId,
    u2: ItemId,
    /// V3: 15..40 from 1 s.
    q: ItemId,
    /// A1: 0..2 s.
    sound: ItemId,
    /// S1: 10..15.
    cue: ItemId,
    /// FX: 0..5.
    adj: ItemId,
    /// A centred 4-frame cross on the u1/p1 cut, `p1` fading out over its last 4 frames
    /// and `q` fading in over its first 5.
    u1_p1: TransitionId,
    p1_out: TransitionId,
    q_in: TransitionId,
    other: SequenceId,
    /// Other V1: 0..10.
    o: ItemId,
}

fn world() -> World {
    let p = Project::new("uid-nested-contract", "Nested contract");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("m");
    let (p, main) = p.add_sequence("Main", FPS, 48_000).expect("main");
    let (p, v1) = p.add_track(main, TrackKind::Video, "V1", None).expect("v1");
    let (p, v2) = p.add_track(main, TrackKind::Video, "V2", None).expect("v2");
    let (p, v3) = p.add_track(main, TrackKind::Video, "V3", None).expect("v3");
    let (p, a1) = p.add_track(main, TrackKind::Audio, "A1", None).expect("a1");
    let (p, s1) = p
        .add_track(main, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let (p, fx) = p
        .add_track(main, TrackKind::Adjustment, "FX", None)
        .expect("fx");
    let (p, x) = p
        .insert_clip(v1, spec(m, at(12, 6), Time::ZERO))
        .expect("x");
    let (p, u1) = p
        .insert_clip(v2, spec(m, at(0, 10), Time::seconds(3)))
        .expect("u1");
    let (p, p1) = p
        .insert_clip(v2, spec(m, at(10, 10), Time::seconds(1)))
        .expect("p1");
    let (p, u2) = p
        .insert_clip(v2, spec(m, at(40, 10), Time::seconds(5)))
        .expect("u2");
    let (p, q) = p
        .insert_clip(v3, spec(m, at(15, 25), Time::seconds(1)))
        .expect("q");
    let (p, sound) = p
        .insert_clip(
            a1,
            spec(m, TimeRange::new(Time::ZERO, Time::seconds(2)), Time::ZERO),
        )
        .expect("sound");
    let (p, cue) = p.insert_subtitle(s1, at(10, 5), "Hi").expect("cue");
    let (p, adj) = p.insert_adjustment(fx, at(0, 5)).expect("adj");
    let (p, _) = p.add_effect(p1, &effects::lut(), None).expect("lut");
    let (p, _) = p
        .add_marker(
            MarkerOwner::Clip(p1),
            MarkerSpec::at(secs(6, 5), ColourLabel::Blue),
        )
        .expect("marker");
    let (p, u1_p1) = p
        .add_transition(TransitionSpec::new(Some(u1), Some(p1), f(4)))
        .expect("u1_p1");
    let (p, p1_out) = p
        .add_transition(TransitionSpec {
            alignment: Alignment::End,
            ..TransitionSpec::new(Some(p1), None, f(4))
        })
        .expect("p1_out");
    let (p, q_in) = p
        .add_transition(TransitionSpec {
            alignment: Alignment::Start,
            ..TransitionSpec::new(None, Some(q), f(5))
        })
        .expect("q_in");
    let (p, other) = p.add_sequence("Other", FPS, 48_000).expect("other");
    let (p, ov1) = p
        .add_track(other, TrackKind::Video, "V1", None)
        .expect("ov1");
    let (p, o) = p
        .insert_clip(ov1, spec(m, at(0, 10), Time::ZERO))
        .expect("o");
    World {
        project: p,
        main,
        v1,
        v2,
        v3,
        a1,
        x,
        u1,
        p1,
        u2,
        q,
        sound,
        cue,
        adj,
        u1_p1,
        p1_out,
        q_in,
        other,
        o,
    }
}

#[test]
fn nesting_spans_the_selection_across_tracks_and_gaps_on_the_topmost_selected_track() {
    let w = world();
    let p = &w.project;
    let next = p.next_id();
    let (q, k, clip) = p.nest_items(&[w.q, w.p1, w.q], "Opening").expect("nest");
    // Ids in order: the sequence, its six tracks top to bottom, the clip.
    assert_eq!(k, SequenceId(next));
    let seq = q.sequence(k).expect("compound");
    let track_ids: Vec<TrackId> = seq.tracks.iter().map(|t| t.id).collect();
    assert_eq!(
        track_ids,
        (next + 1..next + 7).map(TrackId).collect::<Vec<_>>()
    );
    assert_eq!(clip, ItemId(next + 7));
    assert_eq!(q.next_id(), next + 8);
    // The span 10..40 lands on V2, the topmost selected video track, touching u1 and u2.
    assert_eq!(items_on(&q, w.v2), [w.u1, clip, w.u2]);
    assert_eq!(place_of(&q, clip).2, at(10, 30));
    assert_eq!(clip_of(&q, clip).source, ClipSource::Sequence(k));
    assert_eq!(clip_of(&q, clip).source_start, Time::ZERO);
    assert!(items_on(&q, w.v3).is_empty());
    // An item inside the span on an unselected track stays where it was.
    assert_eq!(place_of(&q, w.x), (w.main, w.v1, at(12, 6)));
    // Inside, the items keep their places relative to the span's start, gaps included,
    // and their properties.
    assert_eq!(place_of(&q, w.p1), (k, track_ids[1], at(0, 10)));
    assert_eq!(place_of(&q, w.q), (k, track_ids[2], at(5, 25)));
    assert_eq!(clip_of(&q, w.p1), clip_of(p, w.p1));
    assert_eq!(q.sequence_duration(k), Ok(f(30)));
    // Fades whose only clip moved go inside unchanged; the cut joining u1 to p1 is gone.
    for t in [w.p1_out, w.q_in] {
        assert_eq!(holder_of(&q, t), Some(k));
        assert_eq!(
            q.find_transition(t).expect("moved").2,
            p.find_transition(t).expect("before").2
        );
    }
    assert_eq!(holder_of(&q, w.u1_p1), None);
    q.validate().expect("valid");
}

#[test]
fn nesting_refuses_a_span_reaching_one_frame_into_an_item_that_stays() {
    let w = world();
    // q one frame longer: the span 10..41 reaches into u2 at 40 on V2.
    let long = w.project.trim_item(w.q, at(15, 26)).expect("longer q");
    assert_eq!(
        long.nest_items(&[w.p1, w.q], "x"),
        Err(EditError::Overlap {
            track: w.v2,
            other: w.u2
        })
    );
    // q starting at 9: the span 9..40 reaches into u1, which ends at 10.
    let early = w.project.trim_item(w.q, at(9, 31)).expect("earlier q");
    assert_eq!(
        early.nest_items(&[w.p1, w.q], "x"),
        Err(EditError::Overlap {
            track: w.v2,
            other: w.u1
        })
    );
}

#[test]
fn nesting_reports_the_first_broken_rule_in_contract_order() {
    let w = world();
    let p = &w.project;
    assert_eq!(p.nest_items(&[], "x"), Err(EditError::NothingToNest));
    // Unknown before across sequences, across before mixed kinds.
    assert_eq!(
        p.nest_items(&[w.p1, w.o, ItemId(999)], "x"),
        Err(EditError::UnknownItem(ItemId(999)))
    );
    assert_eq!(
        p.nest_items(&[w.p1, w.sound, w.o], "x"),
        Err(EditError::NestAcrossSequences(w.o))
    );
    // Subtitles and adjustment items are picture, so they do not mix with sound.
    assert_eq!(
        p.nest_items(&[w.cue, w.sound], "x"),
        Err(EditError::NestMixedKinds)
    );
    assert_eq!(
        p.nest_items(&[w.adj, w.sound], "x"),
        Err(EditError::NestMixedKinds)
    );
    assert_eq!(
        p.nest_items(&[w.cue, w.adj], "x"),
        Err(EditError::NestNeedsClipTrack)
    );
    // Sound alone goes on its audio track.
    let (q, _, clip) = p.nest_items(&[w.sound], "Sound").expect("sound");
    assert_eq!(place_of(&q, clip).1, w.a1);
}

#[test]
fn breaking_apart_the_only_clip_of_a_compound_restores_the_tree_it_was_nested_from() {
    let w = world();
    let p0 = w
        .project
        .remove_transition(w.u1_p1)
        .expect("no straddling cut");
    let (q, k, clip) = p0.nest_items(&[w.p1, w.q, w.cue], "Opening").expect("nest");
    let r = q.break_apart(clip).expect("break apart");
    same_tree(&p0, &r);
    assert!(r.sequence(k).is_none());
    assert!(r.next_id() >= q.next_id());
}

#[test]
fn breaking_apart_a_shared_compound_copies_it_and_its_last_clip_moves_it_out() {
    let w = world();
    let p0 = w
        .project
        .remove_transition(w.u1_p1)
        .expect("no straddling cut");
    let (p, k, k1) = p0.nest_items(&[w.p1, w.q], "Opening").expect("nest");
    let (p, k2) = p
        .insert_sequence_clip(w.v1, k, at(60, 30), Time::ZERO)
        .expect("second clip");
    let next = p.next_id();
    let r = p.break_apart(k2).expect("break apart a shared compound");
    // The compound stays as it was, nested by the other clip.
    assert!(
        r.sequence(k)
            .expect("kept")
            .content_eq(p.sequence(k).expect("compound"))
    );
    assert_eq!(r.sequence_users(k), [(w.main, k1)]);
    assert_eq!(place_of(&r, w.p1).0, k);
    // Its contents come out at k2's place, on the same-numbered video tracks, as copies
    // with new ids.
    let new_on = |track| -> Vec<ItemId> {
        let before = items_on(&p, track);
        items_on(&r, track)
            .into_iter()
            .filter(|i| !before.contains(i))
            .collect()
    };
    let [p1_copy] = new_on(w.v2)[..] else {
        panic!("one copy on V2: {:?}", new_on(w.v2));
    };
    let [q_copy] = new_on(w.v3)[..] else {
        panic!("one copy on V3: {:?}", new_on(w.v3));
    };
    assert!(p1_copy.0 >= next && q_copy.0 >= next, "new ids");
    assert_eq!(place_of(&r, p1_copy).2, at(60, 10));
    assert_eq!(place_of(&r, q_copy).2, at(65, 25));
    let (copy, original) = (clip_of(&r, p1_copy), clip_of(&p, w.p1));
    assert_eq!(
        (copy.source, copy.source_start),
        (original.source, original.source_start)
    );
    assert_eq!(copy.effects.len(), 1);
    assert_ne!(copy.effects[0].id, original.effects[0].id, "new effect id");
    assert_ne!(copy.markers[0].id, original.markers[0].id, "new marker id");
    // The fades came along as copies on the copies.
    let fades: Vec<_> = r
        .sequence(w.main)
        .expect("main")
        .tracks
        .iter()
        .flat_map(|t| t.transitions.iter())
        .map(|t| (t.id.0 >= next, t.outgoing, t.incoming))
        .collect();
    assert_eq!(
        fades,
        [(true, Some(p1_copy), None), (true, None, Some(q_copy))]
    );

    // Now k1 is the compound's only clip: breaking it apart moves the contents out with
    // their own ids and removes the compound.
    let s = r.break_apart(k1).expect("break apart the last clip");
    assert!(s.sequence(k).is_none());
    assert_eq!(place_of(&s, w.p1), (w.main, w.v2, at(10, 10)));
    assert_eq!(place_of(&s, w.q), (w.main, w.v3, at(15, 25)));
    assert_eq!(
        clip_of(&s, w.p1),
        clip_of(&p0, w.p1),
        "same effect and marker ids"
    );
    assert_eq!(holder_of(&s, w.p1_out), Some(w.main));
    assert_eq!(holder_of(&s, w.q_in), Some(w.main));
    s.validate().expect("valid");
}

#[test]
fn broken_apart_tracks_go_to_the_same_numbered_track_of_their_kind() {
    let p = Project::new("uid-kinds", "Kinds");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("m");
    let (p, kit) = p.add_sequence("Kit", FPS, 48_000).expect("kit");
    let (p, kv1) = p.add_track(kit, TrackKind::Video, "V1", None).expect("kv1");
    let (p, ka1) = p.add_track(kit, TrackKind::Audio, "A1", None).expect("ka1");
    let (p, kv2) = p.add_track(kit, TrackKind::Video, "V2", None).expect("kv2");
    let (p, ks1) = p
        .add_track(kit, TrackKind::Subtitle, "S1", None)
        .expect("ks1");
    let (p, _) = p
        .insert_clip(kv1, spec(m, at(0, 10), Time::ZERO))
        .expect("k1");
    let (p, _) = p
        .insert_clip(
            ka1,
            spec(m, TimeRange::new(Time::ZERO, Time::seconds(1)), Time::ZERO),
        )
        .expect("k2");
    let (p, _) = p
        .insert_clip(kv2, spec(m, at(2, 6), Time::seconds(1)))
        .expect("k3");
    let (p, _) = p.insert_subtitle(ks1, at(0, 5), "Hi").expect("cue");
    // The parent interleaves its kinds differently.
    let (p, host) = p.add_sequence("Host", FPS, 48_000).expect("host");
    let (p, ha1) = p
        .add_track(host, TrackKind::Audio, "A1", None)
        .expect("ha1");
    let (p, hv1) = p
        .add_track(host, TrackKind::Video, "V1", None)
        .expect("hv1");
    let (p, hs1) = p
        .add_track(host, TrackKind::Subtitle, "S1", None)
        .expect("hs1");
    let (p, ha2) = p
        .add_track(host, TrackKind::Audio, "A2", None)
        .expect("ha2");
    let (p, hv2) = p
        .add_track(host, TrackKind::Video, "V2", None)
        .expect("hv2");
    let (p, h) = p
        .insert_sequence_clip(hv1, kit, at(100, 10), Time::ZERO)
        .expect("kit clip");
    let r = p.break_apart(h).expect("break apart");
    let media = ClipSource::Media(m);
    assert_eq!(clips_on(&r, hv1), [(at(100, 10), media, Time::ZERO)]);
    assert_eq!(clips_on(&r, hv2), [(at(102, 6), media, Time::seconds(1))]);
    assert_eq!(clips_on(&r, ha1), [(at(100, 10), media, Time::ZERO)]);
    assert!(items_on(&r, ha2).is_empty());
    let cues = r.find_track(hs1).expect("hs1").1.items.clone();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].range, at(100, 5));
    assert!(matches!(&cues[0].kind, ItemKind::Subtitle(s) if s.text == "Hi"));

    // A parent with no audio track cannot take the nested sound.
    let (p, bare) = p.add_sequence("Bare", FPS, 48_000).expect("bare");
    let (p, bv1) = p
        .add_track(bare, TrackKind::Video, "V1", None)
        .expect("bv1");
    let (p, _) = p
        .add_track(bare, TrackKind::Video, "V2", None)
        .expect("bv2");
    let (p, _) = p
        .add_track(bare, TrackKind::Subtitle, "S1", None)
        .expect("bs1");
    let (p, b) = p
        .insert_sequence_clip(bv1, kit, at(0, 10), Time::ZERO)
        .expect("kit in bare");
    assert_eq!(
        p.break_apart(b),
        Err(EditError::BreakApartNoTrack {
            kind: TrackKind::Audio,
            index: 0
        })
    );
    // The speed is checked first, slower than 1 as well as faster.
    let half = Rational::new(1, 2);
    let slow = p
        .set_clip_speed(b, half, SpeedKeep::Timeline)
        .expect("slow");
    assert_eq!(
        slow.break_apart(b),
        Err(EditError::BreakApartSpeed {
            item: b,
            speed: half
        })
    );
    // A clip of media is not nested, whatever its speed.
    let (p, mc) = p
        .insert_clip(bv1, spec(m, at(50, 10), Time::ZERO))
        .expect("mc");
    let fast = p
        .set_clip_speed(mc, Rational::new(2, 1), SpeedKeep::Timeline)
        .expect("fast");
    assert_eq!(fast.break_apart(mc), Err(EditError::NotNested(mc)));
}

#[test]
fn breaking_apart_keeps_only_what_the_clip_shows_trimming_items_across_its_edges() {
    let p = Project::new("uid-edges", "Edges");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("m");
    let (p, kit) = p.add_sequence("Kit", FPS, 48_000).expect("kit");
    let (p, kv1) = p.add_track(kit, TrackKind::Video, "V1", None).expect("kv1");
    let (p, kv2) = p.add_track(kit, TrackKind::Video, "V2", None).expect("kv2");
    // The clip shows 6..36. On V1, one item ends at 6 and one starts at 36; the two
    // between lie within. On V2, one item crosses 6 and one crosses 36.
    let (p, _) = p
        .insert_clip(kv1, spec(m, at(0, 6), Time::ZERO))
        .expect("i0");
    let (p, _) = p
        .insert_clip(kv1, spec(m, at(6, 14), Time::seconds(1)))
        .expect("i1");
    let (p, _) = p
        .insert_clip(kv1, spec(m, at(30, 6), Time::seconds(2)))
        .expect("i2");
    let (p, _) = p
        .insert_clip(kv1, spec(m, at(36, 9), Time::seconds(3)))
        .expect("i3");
    let (p, _) = p
        .insert_clip(kv2, spec(m, at(2, 8), Time::seconds(4)))
        .expect("j1");
    let (p, _) = p
        .insert_clip(kv2, spec(m, at(32, 8), Time::seconds(5)))
        .expect("j2");
    let (p, host) = p.add_sequence("Host", FPS, 48_000).expect("host");
    let (p, hv1) = p
        .add_track(host, TrackKind::Video, "V1", None)
        .expect("hv1");
    let (p, hv2) = p
        .add_track(host, TrackKind::Video, "V2", None)
        .expect("hv2");
    let (p, h) = p
        .insert_sequence_clip(hv1, kit, at(100, 30), f(6))
        .expect("kit clip");
    let r = p.break_apart(h).expect("break apart");
    let media = ClipSource::Media(m);
    assert_eq!(
        clips_on(&r, hv1),
        [
            (at(100, 14), media, Time::seconds(1)),
            (at(124, 6), media, Time::seconds(2)),
        ]
    );
    // The head trim moves the source start by the frames cut; the tail trim does not.
    assert_eq!(
        clips_on(&r, hv2),
        [
            (at(100, 4), media, plus(Time::seconds(4), f(4))),
            (at(126, 4), media, Time::seconds(5)),
        ]
    );
}

#[test]
fn a_copied_compound_and_its_original_change_independently() {
    let w = world();
    let p0 = w
        .project
        .remove_transition(w.u1_p1)
        .expect("no straddling cut");
    let (p, k, k1) = p0.nest_items(&[w.p1, w.q], "Opening").expect("nest");
    let (p, k2) = p
        .insert_sequence_clip(w.v1, k, at(60, 30), Time::ZERO)
        .expect("second clip");
    let (d, copy) = p.duplicate_compound(k2).expect("duplicate");
    assert_ne!(copy, k);
    assert_eq!(d.sequence_users(k), [(w.main, k1)]);
    assert_eq!(d.sequence_users(copy), [(w.main, k2)]);
    let copied = d.sequence(copy).expect("copy");
    assert!(copied.compound);
    assert!(d.browser_sequences().all(|s| s.id != copy));
    assert!(
        d.sequence(k)
            .expect("original")
            .content_eq(p.sequence(k).expect("original"))
    );
    let p1_copy = copied.tracks[1].items[0].id;
    let fade_copy = copied.tracks[1].transitions[0].id;
    assert_ne!(p1_copy, w.p1);
    assert_ne!(fade_copy, w.p1_out);

    // Editing the copy leaves the original alone...
    let e = d
        .set_clip_opacity(p1_copy, 0.5)
        .expect("opacity")
        .remove_transition(fade_copy)
        .expect("no fade");
    assert!(
        e.sequence(k)
            .expect("original")
            .content_eq(d.sequence(k).expect("original"))
    );
    assert_eq!(holder_of(&e, w.p1_out), Some(k));
    // ...and editing the original leaves the copy alone.
    let e = d
        .set_clip_opacity(w.p1, 0.25)
        .expect("opacity")
        .remove_item(w.q)
        .expect("no q");
    assert!(
        e.sequence(copy)
            .expect("copy")
            .content_eq(d.sequence(copy).expect("copy"))
    );
    // The copy is k2's own now, so breaking k2 apart moves it out and keeps the original.
    let r = d.break_apart(k2).expect("break apart");
    assert!(r.sequence(copy).is_none());
    assert_eq!(place_of(&r, p1_copy), (w.main, w.v2, at(60, 10)));
    assert!(
        r.sequence(k)
            .expect("original")
            .content_eq(d.sequence(k).expect("original"))
    );
    r.validate().expect("valid");
}

#[test]
fn a_sequence_nested_anywhere_cannot_be_deleted_not_even_from_a_hidden_compound() {
    let w = world();
    let (p, oc) = w
        .project
        .insert_sequence_clip(w.v1, w.other, at(60, 10), Time::ZERO)
        .expect("other in main");
    let (p, wrap, wc) = p.nest_items(&[oc], "Wrap").expect("wrap");
    assert_eq!(
        p.remove_sequence(w.other),
        Err(EditError::SequenceInUse {
            sequence: w.other,
            item: oc
        })
    );
    assert_eq!(
        p.remove_sequence(wrap),
        Err(EditError::SequenceInUse {
            sequence: wrap,
            item: wc
        })
    );
    // Deleting the compound's clip keeps the compound, which still nests Other.
    let q = p.remove_item(wc).expect("delete the clip");
    assert!(q.sequence(wrap).expect("kept").compound);
    assert_eq!(
        q.remove_sequence(w.other),
        Err(EditError::SequenceInUse {
            sequence: w.other,
            item: oc
        })
    );
    let r = q.remove_sequence(wrap).expect("an unused compound");
    let r = r.remove_sequence(w.other).expect("nested by nothing now");
    assert!(r.sequence(w.other).is_none());
    r.validate().expect("valid");
    // The sequence holding a nested clip can go; what it nested stays.
    let s = p.remove_sequence(w.main).expect("main");
    assert!(s.sequence(wrap).is_some());
    s.validate().expect("valid");
}

fn sequence_at(rate: Rational) -> Arc<Sequence> {
    let (p, s) = Project::new("uid-rate", "Rate")
        .add_sequence("Nested", rate, 48_000)
        .expect("sequence");
    p.sequence(s).expect("sequence").clone()
}

#[test]
fn a_parent_frame_shows_the_nearest_nested_frame_with_halves_going_to_the_later_one() {
    let ntsc = Rational::new(30_000, 1001);
    let r = |n| Rational::new(n, 1);
    // (nested rate, parent rate, parent frame, nested frame shown)
    let cases = [
        // 24 in 30: parent frame n is 0.8 n nested frames.
        (r(24), r(30), 1, 1),
        (r(24), r(30), 2, 2),
        (r(24), r(30), 3, 2),
        (r(24), r(30), 5, 4),
        (r(24), r(30), 30, 24),
        // 30 in 24: 1.25 n, so every other frame is a half.
        (r(30), r(24), 1, 1),
        (r(30), r(24), 2, 3),
        (r(30), r(24), 3, 4),
        (r(30), r(24), 10, 13),
        // 25 in 29.97: n × 1001 / 1200.
        (r(25), ntsc, 1, 1),
        (r(25), ntsc, 6, 5),
        (r(25), ntsc, 599, 500),
        (r(25), ntsc, 600, 501),
        (r(25), ntsc, 1200, 1001),
    ];
    for (nested, parent, n, expected) in cases {
        let time = Time::from_frames(n, parent).expect("time");
        assert_eq!(
            sequence_at(nested).nearest_frame(time),
            Ok(expected),
            "{nested} fps in {parent} fps, parent frame {n}"
        );
    }
    // Source times between frames, as a slip leaves them: exactly half a 24 fps frame
    // past frame 2 shows frame 3.
    assert_eq!(sequence_at(r(24)).nearest_frame(secs(5, 48)), Ok(3));
    assert_eq!(sequence_at(r(24)).nearest_frame(secs(9, 96)), Ok(2));
}

#[test]
fn past_the_nested_end_a_clip_shows_nothing_rather_than_the_last_frame() {
    let rate = Rational::new(24, 1);
    let p = Project::new("uid-end", "End");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("m");
    let (p, card) = p.add_sequence("Card", rate, 48_000).expect("card");
    let (p, cv1) = p
        .add_track(card, TrackKind::Video, "V1", None)
        .expect("cv1");
    let (p, shot) = p
        .insert_clip(
            cv1,
            spec(m, TimeRange::new(Time::ZERO, Time::seconds(1)), Time::ZERO),
        )
        .expect("shot");
    let end = p.sequence_duration(card).expect("duration");
    assert_eq!(end, Time::seconds(1));
    let seq = p.sequence(card).expect("card");
    let shown = |t: Time| -> (i64, Option<ItemId>) {
        let frame = seq.nearest_frame(t).expect("frame");
        let at = Time::from_frames(frame, rate).expect("time");
        let item = seq.tracks[0].item_at(at).expect("item at").map(|i| i.id);
        (frame, item)
    };
    // The last frame (23) shows the clip; from the end on, frames past it show nothing.
    assert_eq!(shown(secs(23, 24)), (23, Some(shot)));
    assert_eq!(shown(end), (24, None));
    assert_eq!(shown(secs(31, 30)), (25, None));
    assert_eq!(shown(Time::seconds(5)), (120, None));
}

#[test]
fn transition_handles_of_nested_clips_use_the_sequences_length_and_never_ask_for_media() {
    let p = Project::new("uid-handles", "Handles");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("m");
    // Long is 4 s: its audio runs past its picture.
    let (p, long) = p.add_sequence("Long", FPS, 48_000).expect("long");
    let (p, lv1) = p
        .add_track(long, TrackKind::Video, "V1", None)
        .expect("lv1");
    let (p, la1) = p
        .add_track(long, TrackKind::Audio, "A1", None)
        .expect("la1");
    let (p, _) = p
        .insert_clip(lv1, spec(m, at(0, 50), Time::ZERO))
        .expect("l1");
    let (p, _) = p
        .insert_clip(
            la1,
            spec(m, TimeRange::new(Time::ZERO, Time::seconds(4)), Time::ZERO),
        )
        .expect("l2");
    assert_eq!(p.sequence_duration(long), Ok(Time::seconds(4)));
    let (p, main) = p.add_sequence("Main", FPS, 48_000).expect("main");
    let (p, mv1) = p
        .add_track(main, TrackKind::Video, "V1", None)
        .expect("mv1");
    let (p, mv2) = p
        .add_track(main, TrackKind::Video, "V2", None)
        .expect("mv2");
    // n shows 0..2 s of Long at speed 2 over 0..25: 2 s of Long, 1 s of timeline, remain.
    let (p, n) = p
        .insert_sequence_clip(mv1, long, at(0, 25), Time::ZERO)
        .expect("n");
    let p = p
        .set_clip_speed(n, Rational::new(2, 1), SpeedKeep::Timeline)
        .expect("speed");
    let never = |media: MediaId| -> Option<Time> { panic!("asked for the length of {media}") };
    let fade = |frames| {
        p.add_transition(TransitionSpec {
            alignment: Alignment::Start,
            ..TransitionSpec::new(Some(n), None, f(frames))
        })
        .expect("fade out")
    };
    let (q, t) = fade(25);
    q.check_transition_media(t, never).expect("exactly enough");
    let (q, t) = fade(26);
    assert_eq!(
        q.check_transition_media(t, never),
        Err(EditError::TransitionHandle {
            item: n,
            needed: f(26),
            available: Time::seconds(1),
        })
    );

    // A cross from n into a media clip: each side names its source.
    let (q, mm) = p
        .insert_clip(mv1, spec(m, at(25, 25), Time::seconds(1)))
        .expect("mm");
    let (q, cross) = q
        .add_transition(TransitionSpec::new(Some(n), Some(mm), f(10)))
        .expect("cross");
    assert_eq!(
        q.transition_needs(cross),
        Ok(HandleNeeds {
            outgoing: Some((n, ClipSource::Sequence(long), plus(Time::seconds(2), f(10)))),
            incoming: Some((mm, ClipSource::Media(m), minus(Time::seconds(1), f(5)))),
        })
    );
    q.check_transition_media(cross, never)
        .expect("the nested tail fits");
    // From media into a nested clip: only the media's length is asked for.
    let (q, mo) = q
        .insert_clip(mv2, spec(m, at(0, 25), Time::ZERO))
        .expect("mo");
    let (q, ni) = q
        .insert_sequence_clip(mv2, long, at(25, 25), Time::seconds(1))
        .expect("ni");
    let (q, into) = q
        .add_transition(TransitionSpec::new(Some(mo), Some(ni), f(10)))
        .expect("into");
    assert_eq!(
        q.transition_needs(into).expect("needs").incoming,
        Some((
            ni,
            ClipSource::Sequence(long),
            minus(Time::seconds(1), f(5))
        ))
    );
    let asked = RefCell::new(Vec::new());
    q.check_transition_media(into, |media| {
        asked.borrow_mut().push(media);
        Some(Time::seconds(60))
    })
    .expect("fits");
    assert_eq!(asked.into_inner(), [m]);
}

#[test]
fn clip_counts_multiply_through_every_level_of_nesting() {
    let p = Project::new("uid-counts", "Counts");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("m");
    let (p, a) = p.add_sequence("A", FPS, 48_000).expect("a");
    let (p, b) = p.add_sequence("B", FPS, 48_000).expect("b");
    let (p, c) = p.add_sequence("C", FPS, 48_000).expect("c");
    let (p, av1) = p.add_track(a, TrackKind::Video, "V1", None).expect("av1");
    let (p, bv1) = p.add_track(b, TrackKind::Video, "V1", None).expect("bv1");
    let (p, cv1) = p.add_track(c, TrackKind::Video, "V1", None).expect("cv1");
    // C plays m once; B nests C twice; A nests B three times.
    let (p, _) = p
        .insert_clip(cv1, spec(m, at(0, 25), Time::ZERO))
        .expect("c1");
    let (p, _) = p
        .insert_sequence_clip(bv1, c, at(0, 25), Time::ZERO)
        .expect("b1");
    let (p, _) = p
        .insert_sequence_clip(bv1, c, at(25, 25), Time::ZERO)
        .expect("b2");
    let (p, a1) = p
        .insert_sequence_clip(av1, b, at(0, 50), Time::ZERO)
        .expect("a1");
    let (p, _) = p
        .insert_sequence_clip(av1, b, at(50, 50), Time::ZERO)
        .expect("a2");
    let (p, _) = p
        .insert_sequence_clip(av1, b, at(100, 50), Time::ZERO)
        .expect("a3");
    // C: 1, B: 2, A: 6.
    assert_eq!(p.clip_count(m), 9);
    // A compound counts only through its clips, once per clip.
    let (q, k, _) = p.nest_items(&[a1], "Wrap").expect("nest");
    assert_eq!(q.clip_count(m), 9);
    let (q, _) = q
        .insert_sequence_clip(av1, k, at(150, 50), Time::ZERO)
        .expect("again");
    assert_eq!(q.clip_count(m), 11);
}

#[test]
fn edits_store_frame_rates_in_lowest_terms_for_new_sequences_and_compounds() {
    let p = Project::new("uid-reduced", "Reduced");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("m");
    let (p, fifty) = p
        .add_sequence("Fifty halves", Rational::new(50, 2), 48_000)
        .expect("sequence");
    assert_eq!(
        p.sequence(fifty).expect("seq").frame_rate,
        Rational::new(25, 1)
    );
    let (p, v1) = p
        .add_track(fifty, TrackKind::Video, "V1", None)
        .expect("v1");
    let (p, shot) = p
        .insert_clip(v1, spec(m, at(0, 10), Time::ZERO))
        .expect("shot");
    let (q, k, _) = p.nest_items(&[shot], "Wrap").expect("nest");
    assert_eq!(q.sequence(k).expect("k").frame_rate, Rational::new(25, 1));
}

#[test]
#[ignore = "suspected bug: nest_items copies a loaded parent's unreduced frame rate (50/2) into the compound"]
fn a_compound_nested_from_a_file_with_an_unreduced_rate_stores_it_reduced() {
    let p = Project::new("uid-reduced", "Reduced");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("m");
    let (p, main) = p.add_sequence("Main", FPS, 48_000).expect("main");
    let (p, v1) = p.add_track(main, TrackKind::Video, "V1", None).expect("v1");
    let (p, shot) = p
        .insert_clip(v1, spec(m, at(0, 10), Time::ZERO))
        .expect("shot");
    let mut json = saved(&p);
    json["project"]["sequences"][0]["frame_rate"] = json!({ "num": 50, "den": 2 });
    let loaded = load(&json).expect("a file may hold any form");
    let (q, k, _) = loaded.nest_items(&[shot], "Wrap").expect("nest");
    assert_eq!(q.sequence(k).expect("k").frame_rate, Rational::new(25, 1));
}

#[test]
fn every_nesting_edit_undoes_redoes_diffs_and_saves_both_ways() {
    let w = world();
    let p0 = &w.project;
    let (inserted, _) = p0
        .insert_sequence_clip(w.v1, w.other, at(60, 10), Time::ZERO)
        .expect("insert");
    let replaced = p0
        .set_clip_source(w.u2, ClipSource::Sequence(w.other))
        .expect("replace");
    let (nested, k, k1) = p0.nest_items(&[w.p1, w.q], "Opening").expect("nest");
    let moved = nested.break_apart(k1).expect("break apart, moving");
    let (shared, k2) = nested
        .insert_sequence_clip(w.v1, k, at(60, 30), Time::ZERO)
        .expect("second clip");
    let copied = shared.break_apart(k2).expect("break apart, copying");
    let (duplicated, _) = shared.duplicate_compound(k2).expect("duplicate");
    let orphaned = nested.remove_item(k1).expect("delete the clip");
    let removed = orphaned.remove_sequence(k).expect("remove sequence");
    let cases = [
        ("insert", EditLabel::Insert, p0, &inserted),
        ("replace source", EditLabel::ReplaceSource, p0, &replaced),
        ("nest", EditLabel::Nest, p0, &nested),
        (
            "break apart, moving",
            EditLabel::BreakApart,
            &nested,
            &moved,
        ),
        (
            "break apart, copying",
            EditLabel::BreakApart,
            &shared,
            &copied,
        ),
        (
            "duplicate",
            EditLabel::DuplicateCompound,
            &shared,
            &duplicated,
        ),
        (
            "remove sequence",
            EditLabel::RemoveSequence,
            &orphaned,
            &removed,
        ),
    ];
    for (name, label, before, after) in cases {
        after.validate().expect(name);
        assert!(!after.content_eq(before), "{name} changes the tree");
        let mut h = History::new(before.clone());
        h.commit(label, after.clone());
        assert_eq!(h.undo_label(), Some(label), "{name}");
        assert!(h.undo().expect(name).content_eq(before), "{name} undo");
        assert!(h.redo().expect(name).content_eq(after), "{name} redo");
        let (old, new) = (flatten(before), flatten(after));
        let mut flat = old.clone();
        Diff::between(&old, &new).apply(&mut flat);
        assert!(rebuild(&flat).expect(name).content_eq(after), "{name}");
        let mut flat = new.clone();
        Diff::between(&new, &old).apply(&mut flat);
        assert!(
            rebuild(&flat).expect(name).content_eq(before),
            "{name} back"
        );
        let loaded = schema::load(&schema::save(after)).expect(name);
        assert!(loaded.content_eq(after), "{name} saved");
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "glass-nested-contract-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_session_of_nesting_edits_recovers_from_the_autosave_log() {
    let w = world();
    let dir = temp_dir("session");
    let mut p = w.project.clone();
    let mut writer = AutosaveWriter::open(&dir, &p, None, 0, 0).expect("open");
    let mut now = 0;
    let mut tick = |p: &Project| {
        now += 10;
        writer.tick(p, 1, now).expect("tick");
    };
    let (q, oc) = p
        .insert_sequence_clip(w.v1, w.other, at(100, 10), Time::ZERO)
        .expect("insert");
    p = q;
    tick(&p);
    let (q, k, k1) = p.nest_items(&[w.p1, w.q], "Opening").expect("nest");
    p = q;
    tick(&p);
    let (q, k2) = p
        .insert_sequence_clip(w.v1, k, at(60, 30), Time::ZERO)
        .expect("second clip");
    p = q;
    tick(&p);
    let (q, copy) = p.duplicate_compound(k2).expect("duplicate");
    p = q;
    tick(&p);
    p = p
        .set_clip_source(w.x, ClipSource::Sequence(copy))
        .expect("x shows the copy too");
    tick(&p);
    p = p.break_apart(k1).expect("break apart the original");
    tick(&p);
    p = p.remove_item(oc).expect("unnest other");
    tick(&p);
    p = p.remove_sequence(w.other).expect("remove other");
    tick(&p);
    drop(writer);
    let recovered = autosave::recover(&dir).expect("recovers");
    assert!(recovered.project.content_eq(&p));
    assert!(recovered.project.sequence(k).is_none());
    let mut users = recovered.project.sequence_users(copy);
    users.sort();
    assert_eq!(users, [(w.main, w.x), (w.main, k2)]);
    fs::remove_dir_all(&dir).expect("cleanup");
}

fn v7_basic() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/projects/v7-basic.json");
    serde_json::from_slice(&fs::read(&path).expect("fixture")).expect("JSON")
}

#[test]
fn a_v7_file_migrates_to_the_current_schema_unless_a_sequence_already_has_compound() {
    let v7 = v7_basic();
    assert_eq!(v7["schema_version"], 7);
    let loaded = load(&v7).expect("v7 loads");
    let resaved = saved(&loaded);
    assert_eq!(resaved["schema_version"], SCHEMA_VERSION);
    let sequences = resaved["project"]["sequences"].as_array().expect("list");
    assert!(!sequences.is_empty());
    for seq in sequences {
        assert_eq!(seq["compound"], Value::Bool(false));
    }
    // A v7 sequence cannot hold the flag yet, whatever its value.
    for value in [false, true] {
        let mut bad = v7.clone();
        bad["project"]["sequences"][0]["compound"] = Value::Bool(value);
        match load(&bad) {
            Err(LoadError::Migration { from, message }) => {
                assert_eq!(from, 7);
                assert!(message.contains("compound"), "{message}");
            }
            other => panic!("compound {value}: {other:?}"),
        }
    }
}
