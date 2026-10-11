//! Transitions (schema v5): adding them on cuts, the edits that change them, how clip
//! edits fit them, the engine's segment query, and the checks on loaded trees.

use std::sync::Arc;

use project::diff::{Diff, Node, flatten, rebuild};
use project::params::{ParamDef, ParamSchema, ParamValue, Range};
use project::schema::{self, LoadError};
use project::transition::{DEFAULT_EASING, DEFAULT_KIND};
use project::{
    Alignment, ClipSpec, CutSide, EditError, EditLabel, History, ItemId, Project, Rational,
    Segment, SequenceId, SpeedKeep, Time, TimeRange, TrackId, TrackKind, TransitionId,
    TransitionSpec, ValidationError,
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
    seq: SequenceId,
    v1: TrackId,
    v2: TrackId,
    /// V1: A 0..50 from 2 s, B 50..100 from 1 s, C 100..125 from 0, a gap, D 150..200
    /// from 4 s.
    a: ItemId,
    b: ItemId,
    c: ItemId,
    d: ItemId,
    /// A1: 0..2 s.
    sound: ItemId,
    cue: ItemId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-transitions", "Transitions");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).expect("v2");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, s1) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let spec = |range, source_start| ClipSpec {
        media,
        range,
        source_start,
    };
    let (p, a) = p
        .insert_clip(v1, spec(range(0, 50), Time::seconds(2)))
        .expect("a");
    let (p, b) = p
        .insert_clip(v1, spec(range(50, 50), Time::seconds(1)))
        .expect("b");
    let (p, c) = p
        .insert_clip(v1, spec(range(100, 25), Time::ZERO))
        .expect("c");
    let (p, d) = p
        .insert_clip(v1, spec(range(150, 50), Time::seconds(4)))
        .expect("d");
    let (p, sound) = p
        .insert_clip(
            a1,
            spec(TimeRange::new(Time::ZERO, Time::seconds(2)), Time::ZERO),
        )
        .expect("sound");
    let (p, cue) = p.insert_subtitle(s1, range(0, 10), "Hi").expect("cue");
    Fixture {
        project: p,
        seq,
        v1,
        v2,
        a,
        b,
        c,
        d,
        sound,
        cue,
    }
}

fn spec(o: Option<ItemId>, i: Option<ItemId>, n: i64, alignment: Alignment) -> TransitionSpec {
    TransitionSpec {
        alignment,
        ..TransitionSpec::new(o, i, frames(n))
    }
}

fn add(
    p: &Project,
    o: Option<ItemId>,
    i: Option<ItemId>,
    n: i64,
    alignment: Alignment,
) -> Result<(Project, TransitionId), EditError> {
    p.add_transition(spec(o, i, n, alignment))
}

fn span(p: &Project, t: TransitionId) -> TimeRange {
    p.transition_span(t).expect("span")
}

fn duration(p: &Project, t: TransitionId) -> Time {
    p.find_transition(t).expect("transition").2.duration
}

fn exists(p: &Project, t: TransitionId) -> bool {
    p.find_transition(t).is_some()
}

fn segment<'a>(p: &'a Project, f: &Fixture, time: Time) -> Option<Segment<'a>> {
    p.sequence(f.seq)
        .expect("seq")
        .segment_at(f.v1, time)
        .expect("query")
}

/// Commits `next` under `label`, checks undo restores `before` and redo `next`.
fn undo_redo(before: &Project, next: &Project, label: EditLabel) {
    let mut h = History::new(before.clone());
    h.commit(label, next.clone());
    assert_eq!(h.undo_label(), Some(label));
    assert!(h.undo().expect("undo").content_eq(before));
    assert!(h.redo().expect("redo").content_eq(next));
    next.validate().expect("valid");
}

#[test]
fn a_centred_transition_sits_across_the_cut_with_the_defaults() {
    let f = fixture();
    let (p, t) = add(&f.project, Some(f.a), Some(f.b), 10, Alignment::Centre).expect("add");
    undo_redo(&f.project, &p, EditLabel::AddTransition);
    let (_, track, transition) = p.find_transition(t).expect("transition");
    assert_eq!(track.id, f.v1);
    assert_eq!(transition.kind, DEFAULT_KIND);
    assert_eq!(transition.kind, "cross-dissolve");
    assert_eq!(transition.easing, DEFAULT_EASING);
    assert_eq!(transition.alignment, Alignment::Centre);
    assert_eq!(transition.outgoing, Some(f.a));
    assert_eq!(transition.incoming, Some(f.b));
    assert_eq!(t.0, f.project.next_id());
    assert_eq!(p.next_id(), f.project.next_id() + 1);
    assert_eq!(span(&p, t), range(45, 10));
    assert_eq!(track.transitions_of(f.b).0.map(|x| x.id), Some(t));
    assert_eq!(track.transitions_of(f.a).1.map(|x| x.id), Some(t));
    assert_eq!(track.transitions_of(f.c), (None, None));
    // The items and the other tracks are untouched.
    let old = f.project.sequence(f.seq).expect("seq");
    let new = p.sequence(f.seq).expect("seq");
    let (old_items, new_items) = (&old.tracks[0].items, &new.tracks[0].items);
    assert_eq!(old_items.len(), new_items.len());
    assert!(
        old_items
            .iter()
            .zip(new_items)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
    for i in 1..old.tracks.len() {
        assert!(Arc::ptr_eq(&new.tracks[i], &old.tracks[i]));
    }
    // Media shown: A past its end to 4.2 s, B from 0.8 s.
    let needs = p.transition_needs(t).expect("needs");
    let media = p.media().iter().next().expect("media").id;
    assert_eq!(
        needs.outgoing,
        Some((f.a, media, Time::new(21, 5).expect("t")))
    );
    assert_eq!(
        needs.incoming,
        Some((f.b, media, Time::new(4, 5).expect("t")))
    );
}

#[test]
fn alignment_places_the_span_relative_to_the_cut() {
    let f = fixture();
    let cases = [
        (Alignment::Centre, 10, range(45, 10)),
        // Odd: the extra frame goes after the cut.
        (Alignment::Centre, 5, range(48, 5)),
        (Alignment::Centre, 1, range(50, 1)),
        (Alignment::Start, 10, range(50, 10)),
        (Alignment::End, 10, range(40, 10)),
    ];
    for (alignment, n, expected) in cases {
        let (p, t) = add(&f.project, Some(f.a), Some(f.b), n, alignment).expect("add");
        assert_eq!(span(&p, t), expected, "{alignment:?} {n}");
    }
    let grid = frames(1);
    assert_eq!(
        Alignment::Centre.split(frames(7), grid),
        Ok((frames(3), frames(4)))
    );
    assert_eq!(
        Alignment::Start.split(frames(7), grid),
        Ok((Time::ZERO, frames(7)))
    );
    assert_eq!(
        Alignment::End.split(frames(7), grid),
        Ok((frames(7), Time::ZERO))
    );
}

#[test]
fn a_transition_needing_more_head_handle_than_the_media_has_is_refused() {
    let f = fixture();
    // C starts at source 0: no media before its start.
    assert_eq!(
        add(&f.project, Some(f.b), Some(f.c), 4, Alignment::Centre).map(|_| ()),
        Err(EditError::TransitionHandle {
            item: f.c,
            needed: frames(2),
            available: Time::ZERO,
        })
    );
    assert_eq!(
        add(&f.project, Some(f.b), Some(f.c), 4, Alignment::End).map(|_| ()),
        Err(EditError::TransitionHandle {
            item: f.c,
            needed: frames(4),
            available: Time::ZERO,
        })
    );
    // Starting at the cut needs only the outgoing clip's tail, which `project` cannot see.
    add(&f.project, Some(f.b), Some(f.c), 4, Alignment::Start).expect("start at the cut");
    // B has 1 s (25 frames) of media before its start.
    assert_eq!(
        add(&f.project, Some(f.a), Some(f.b), 30, Alignment::End).map(|_| ()),
        Err(EditError::TransitionHandle {
            item: f.b,
            needed: frames(30),
            available: frames(25),
        })
    );
    add(&f.project, Some(f.a), Some(f.b), 25, Alignment::End).expect("exactly the handle");
    // At double speed the 1 s of source is half a second of timeline.
    let p = f
        .project
        .set_clip_speed(f.b, Rational::new(2, 1), SpeedKeep::Timeline)
        .expect("speed");
    assert_eq!(
        add(&p, Some(f.a), Some(f.b), 13, Alignment::End).map(|_| ()),
        Err(EditError::TransitionHandle {
            item: f.b,
            needed: frames(13),
            available: Time::new(1, 2).expect("t"),
        })
    );
}

#[test]
fn a_transition_must_stay_within_its_clips_and_off_its_neighbours() {
    let f = fixture();
    assert_eq!(
        add(&f.project, Some(f.a), Some(f.b), 60, Alignment::Start).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::After,
            needed: frames(60),
            available: frames(50),
        })
    );
    // Room before the cut is checked before the handle.
    assert_eq!(
        add(&f.project, Some(f.a), Some(f.b), 120, Alignment::Centre).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::Before,
            needed: frames(60),
            available: frames(50),
        })
    );
    // Two transitions sharing clip B may touch but not overlap.
    let p = f.project.slip_clip(f.c, Time::seconds(2)).expect("slip");
    let (p, ab) = add(&p, Some(f.a), Some(f.b), 30, Alignment::Start).expect("ab");
    assert_eq!(
        add(&p, Some(f.b), Some(f.c), 25, Alignment::End).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::Before,
            needed: frames(25),
            available: frames(20),
        })
    );
    let (p, bc) = add(&p, Some(f.b), Some(f.c), 20, Alignment::End).expect("bc");
    assert_eq!(span(&p, bc), range(80, 20));
    assert_eq!(
        p.set_transition_duration(ab, frames(31)).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::After,
            needed: frames(31),
            available: frames(30),
        })
    );
    // A transition cannot reach before zero.
    assert_eq!(
        add(&f.project, None, Some(f.a), 10, Alignment::Centre).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::Before,
            needed: frames(5),
            available: Time::ZERO,
        })
    );
}

#[test]
fn transitions_from_or_to_nothing_need_a_gap_or_the_track_edge() {
    let f = fixture();
    // Fade in at the very start of the track, over A's head.
    let (p, fade_in) = add(&f.project, None, Some(f.a), 10, Alignment::Start).expect("fade in");
    assert_eq!(span(&p, fade_in), range(0, 10));
    // Fade C out into the gap before D.
    let (p, out) = add(&p, Some(f.c), None, 25, Alignment::Start).expect("fade out");
    assert_eq!(span(&p, out), range(125, 25));
    assert_eq!(
        add(&f.project, Some(f.c), None, 26, Alignment::Start).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::After,
            needed: frames(26),
            available: frames(25),
        })
    );
    // A fade out past the last item has unlimited room after the cut.
    let (p, last) = add(&p, Some(f.d), None, 500, Alignment::Start).expect("last");
    assert_eq!(span(&p, last), range(200, 500));
    // D fades in from the gap, which the fade out of C partly takes.
    let (q, _) = add(&f.project, Some(f.c), None, 15, Alignment::Start).expect("out");
    assert_eq!(
        add(&q, None, Some(f.d), 15, Alignment::End).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::Before,
            needed: frames(15),
            available: frames(10),
        })
    );
    let (q, d_in) = add(&q, None, Some(f.d), 10, Alignment::End).expect("in");
    assert_eq!(span(&q, d_in), range(140, 10));
    // Edges that meet another item take a two-sided transition.
    assert_eq!(
        add(&f.project, Some(f.a), None, 5, Alignment::End).map(|_| ()),
        Err(EditError::CutNotFree {
            item: f.a,
            other: f.b,
        })
    );
    assert_eq!(
        add(&f.project, None, Some(f.b), 5, Alignment::Start).map(|_| ()),
        Err(EditError::CutNotFree {
            item: f.b,
            other: f.a,
        })
    );
}

#[test]
fn adding_refuses_bad_arguments_and_cuts_in_order() {
    let f = fixture();
    let p = &f.project;
    let unknown = ItemId(999);
    let bad = |mut s: TransitionSpec, edit: &dyn Fn(&mut TransitionSpec)| {
        edit(&mut s);
        p.add_transition(s).map(|_| ())
    };
    let ab = spec(Some(f.a), Some(f.b), 10, Alignment::Centre);
    assert_eq!(
        bad(ab.clone(), &|s| {
            s.outgoing = None;
            s.incoming = None;
            s.kind = "Bad".into();
        }),
        Err(EditError::TransitionNeedsClip)
    );
    assert_eq!(
        bad(ab.clone(), &|s| {
            s.kind = "Cross Dissolve".into();
            s.easing = String::new();
        }),
        Err(EditError::InvalidTransitionKind("Cross Dissolve".into()))
    );
    assert_eq!(
        bad(ab.clone(), &|s| s.kind = String::new()),
        Err(EditError::InvalidTransitionKind(String::new()))
    );
    assert_eq!(
        bad(ab.clone(), &|s| {
            s.easing = "ease_in".into();
            s.duration = Time::ZERO;
        }),
        Err(EditError::InvalidEasing("ease_in".into()))
    );
    // Arguments come before the lookup.
    assert_eq!(
        bad(ab.clone(), &|s| {
            s.duration = -frames(1);
            s.outgoing = Some(unknown);
        }),
        Err(EditError::EmptyRange(-frames(1)))
    );
    assert_eq!(
        bad(ab.clone(), &|s| s.outgoing = Some(unknown)),
        Err(EditError::UnknownItem(unknown))
    );
    assert_eq!(
        bad(ab.clone(), &|s| {
            s.outgoing = Some(ItemId(998));
            s.incoming = Some(unknown);
        }),
        Err(EditError::UnknownItem(ItemId(998)))
    );
    assert_eq!(
        bad(ab.clone(), &|s| s.incoming = Some(f.cue)),
        Err(EditError::NotAClip(f.cue))
    );
    assert_eq!(
        bad(ab.clone(), &|s| {
            s.outgoing = Some(f.cue);
            s.incoming = None;
        }),
        Err(EditError::NotAClip(f.cue))
    );
    assert_eq!(
        bad(ab.clone(), &|s| s.incoming = Some(f.c)),
        Err(EditError::NotAdjacent {
            outgoing: f.a,
            incoming: f.c,
        })
    );
    assert_eq!(
        bad(ab.clone(), &|s| s.incoming = Some(f.sound)),
        Err(EditError::NotAdjacent {
            outgoing: f.a,
            incoming: f.sound,
        })
    );
    assert_eq!(
        bad(ab.clone(), &|s| {
            s.outgoing = Some(f.c);
            s.incoming = Some(f.d);
        }),
        Err(EditError::NotAdjacent {
            outgoing: f.c,
            incoming: f.d,
        })
    );
    let half_frame = Time::new(1, 50).expect("t");
    assert_eq!(
        bad(ab.clone(), &|s| s.duration = half_frame),
        Err(EditError::OffGrid {
            time: half_frame,
            grid: frames(1),
            kind: TrackKind::Video,
        })
    );
    // One transition per edge, refused before the duration's grid.
    let (q, t) = p.add_transition(ab.clone()).expect("add");
    let mut again = ab.clone();
    again.duration = half_frame;
    assert_eq!(
        q.add_transition(again).map(|_| ()),
        Err(EditError::TransitionExists(t))
    );
    // Audio tracks take transitions on their sample grid.
    let (q, sound_out) = add(p, Some(f.sound), None, 5, Alignment::Start).expect("audio");
    assert_eq!(span(&q, sound_out), range(50, 5));
    let sample = Time::new(1, 48_000).expect("t");
    let mut s = spec(Some(f.sound), None, 0, Alignment::Start);
    s.duration = sample;
    let (q, short) = p.add_transition(s).expect("one sample");
    assert_eq!(duration(&q, short), sample);
}

#[test]
fn every_transition_edit_is_undoable_and_no_ops_share_the_tree() {
    let f = fixture();
    let (p, t) = add(&f.project, Some(f.a), Some(f.b), 10, Alignment::Centre).expect("add");

    let q = p.set_transition_duration(t, frames(20)).expect("duration");
    assert_eq!(span(&q, t), range(40, 20));
    undo_redo(&p, &q, EditLabel::TransitionDuration);

    let q = p
        .set_transition_alignment(t, Alignment::Start)
        .expect("align");
    assert_eq!(span(&q, t), range(50, 10));
    undo_redo(&p, &q, EditLabel::TransitionAlignment);

    let q = p.set_transition_kind(t, "iris").expect("kind");
    assert_eq!(q.find_transition(t).expect("t").2.kind, "iris");
    undo_redo(&p, &q, EditLabel::Transition);

    let q = p.set_transition_easing(t, "ease-in-out").expect("easing");
    assert_eq!(q.find_transition(t).expect("t").2.easing, "ease-in-out");
    undo_redo(&p, &q, EditLabel::Transition);

    let q = p.remove_transition(t).expect("remove");
    assert!(!exists(&q, t));
    assert!(
        q.sequence(f.seq).expect("seq").tracks[0]
            .transitions
            .is_empty()
    );
    undo_redo(&p, &q, EditLabel::RemoveTransition);

    let track_of = |p: &Project| p.sequence(f.seq).expect("seq").tracks[0].clone();
    for same in [
        p.set_transition_duration(t, frames(10)),
        p.set_transition_alignment(t, Alignment::Centre),
        p.set_transition_kind(t, DEFAULT_KIND),
        p.set_transition_easing(t, DEFAULT_EASING),
    ] {
        let same = same.expect("no-op");
        assert!(same.content_eq(&p));
        assert!(Arc::ptr_eq(&track_of(&same), &track_of(&p)));
    }

    let unknown = TransitionId(999);
    assert_eq!(
        p.remove_transition(unknown).map(|_| ()),
        Err(EditError::UnknownTransition(unknown))
    );
    assert_eq!(
        p.set_transition_duration(unknown, Time::ZERO).map(|_| ()),
        Err(EditError::EmptyRange(Time::ZERO))
    );
    assert_eq!(
        p.set_transition_duration(unknown, frames(4)).map(|_| ()),
        Err(EditError::UnknownTransition(unknown))
    );
    assert_eq!(
        p.set_transition_duration(t, Time::new(1, 50).expect("t"))
            .map(|_| ()),
        Err(EditError::OffGrid {
            time: Time::new(1, 50).expect("t"),
            grid: frames(1),
            kind: TrackKind::Video,
        })
    );
    assert_eq!(
        p.set_transition_alignment(unknown, Alignment::End)
            .map(|_| ()),
        Err(EditError::UnknownTransition(unknown))
    );
    assert_eq!(
        p.set_transition_kind(unknown, "No").map(|_| ()),
        Err(EditError::InvalidTransitionKind("No".into()))
    );
    assert_eq!(
        p.set_transition_kind(unknown, "iris").map(|_| ()),
        Err(EditError::UnknownTransition(unknown))
    );
    assert_eq!(
        p.set_transition_easing(unknown, "a b").map(|_| ()),
        Err(EditError::InvalidEasing("a b".into()))
    );
    assert_eq!(
        p.set_transition_easing(unknown, "linear").map(|_| ()),
        Err(EditError::UnknownTransition(unknown))
    );
    // Alignment is refused, not clamped, when the new place does not fit: C has no head
    // handle.
    let (r, bc) = add(&p, Some(f.b), Some(f.c), 4, Alignment::Start).expect("bc");
    assert_eq!(
        r.set_transition_alignment(bc, Alignment::Centre)
            .map(|_| ()),
        Err(EditError::TransitionHandle {
            item: f.c,
            needed: frames(2),
            available: Time::ZERO,
        })
    );
}

fn iris() -> ParamSchema {
    ParamSchema::new(
        "iris",
        "transition.name.iris",
        vec![ParamDef::float(
            "softness",
            "transition.iris.softness",
            0.2,
            Range::new(0.0, 1.0),
        )],
    )
}

#[test]
fn parameters_follow_the_kind_and_are_cleared_when_it_changes() {
    let f = fixture();
    let (p, t) = add(&f.project, Some(f.a), Some(f.b), 10, Alignment::Centre).expect("add");
    let schema = iris();
    assert_eq!(
        p.set_transition_param(t, &schema, "softness", ParamValue::Float(0.5))
            .map(|_| ()),
        Err(EditError::TransitionSchemaMismatch {
            transition: t,
            expected: DEFAULT_KIND.into(),
            found: "iris".into(),
        })
    );
    let p = p.set_transition_kind(t, "iris").expect("kind");
    let q = p
        .set_transition_param(t, &schema, "softness", ParamValue::Float(2.0))
        .expect("param");
    let params = &q.find_transition(t).expect("t").2.params;
    assert_eq!(params.float(&schema, "softness"), Some(1.0), "clamped");
    undo_redo(&p, &q, EditLabel::Properties);
    assert_eq!(
        q.set_transition_param(t, &schema, "radius", ParamValue::Float(1.0))
            .map(|_| ()),
        Err(EditError::UnknownParam {
            schema: "iris".into(),
            param: "radius".into(),
        })
    );
    let unknown = TransitionId(999);
    assert_eq!(
        q.set_transition_param(unknown, &schema, "radius", ParamValue::Float(1.0))
            .map(|_| ()),
        Err(EditError::UnknownTransition(unknown))
    );
    // Setting the stored value again changes nothing.
    let same = q
        .set_transition_param(t, &schema, "softness", ParamValue::Float(1.0))
        .expect("same");
    assert!(same.content_eq(&q));
    // Another kind starts from its own defaults.
    let r = q.set_transition_kind(t, "cross-dissolve").expect("kind");
    assert_eq!(
        r.find_transition(t).expect("t").2.params,
        project::params::ParamValues::new()
    );
    // Values given when adding are kept as they are.
    let mut values = project::params::ParamValues::new();
    values.set(&schema, "softness", ParamValue::Float(0.7));
    let mut s = spec(Some(f.b), Some(f.c), 4, Alignment::Start);
    s.kind = "iris".into();
    s.params = values.clone();
    let (r, bc) = f.project.add_transition(s).expect("add");
    assert_eq!(r.find_transition(bc).expect("t").2.params, values);
}

#[test]
fn deleting_or_moving_a_clip_removes_the_transitions_on_its_cuts() {
    let f = fixture();
    let (p, ab) = add(&f.project, Some(f.a), Some(f.b), 10, Alignment::Centre).expect("ab");
    let (p, bc) = add(&p, Some(f.b), Some(f.c), 10, Alignment::Start).expect("bc");
    let (p, d_in) = add(&p, None, Some(f.d), 10, Alignment::End).expect("d in");

    let q = p.remove_item(f.b).expect("remove b");
    assert!(!exists(&q, ab) && !exists(&q, bc) && exists(&q, d_in));
    undo_redo(&p, &q, EditLabel::Delete);
    let q = p.remove_item(f.c).expect("remove c");
    assert!(exists(&q, ab) && !exists(&q, bc));

    // A moved away from its cut loses it.
    let q = p.move_item(f.a, f.v1, frames(300)).expect("move a");
    assert!(!exists(&q, ab) && exists(&q, bc));
    undo_redo(&p, &q, EditLabel::Move);

    // D's fade in travels with it while its edge stays free.
    let q = p.move_item(f.d, f.v1, frames(160)).expect("move d");
    assert_eq!(span(&q, d_in), range(150, 10));
    // ...and goes once D meets C.
    let q = p.move_item(f.d, f.v1, frames(125)).expect("move d");
    assert!(!exists(&q, d_in));
    // Or once D leaves the track.
    let q = p.move_item(f.d, f.v2, frames(150)).expect("move d to v2");
    assert!(!exists(&q, d_in));
    assert!(
        q.sequence(f.seq)
            .expect("seq")
            .track(f.v2)
            .expect("v2")
            .transitions
            .is_empty()
    );
    q.validate().expect("valid");

    // An item put into the gap shortens C's fade out, or removes it when it meets C.
    let (p, c_out) = add(&f.project, Some(f.c), None, 20, Alignment::Start).expect("c out");
    let media = p.media().iter().next().expect("media").id;
    let clip = |start, len| ClipSpec {
        media,
        range: range(start, len),
        source_start: Time::ZERO,
    };
    let (q, _) = p.insert_clip(f.v1, clip(140, 10)).expect("insert");
    assert_eq!(span(&q, c_out), range(125, 15));
    undo_redo(&p, &q, EditLabel::Insert);
    let (q, _) = p.insert_clip(f.v1, clip(125, 5)).expect("insert");
    assert!(!exists(&q, c_out));
    q.validate().expect("valid");
}

#[test]
fn trimming_a_clip_shortens_or_removes_its_transitions() {
    let f = fixture();
    let (p, ab) = add(&f.project, Some(f.a), Some(f.b), 20, Alignment::Centre).expect("ab");
    assert_eq!(span(&p, ab), range(40, 20));
    // Trimming A's head to 45 leaves 5 frames before the cut: 11 frames centred fit.
    let q = p.trim_item(f.a, range(45, 5)).expect("trim head");
    assert_eq!(duration(&q, ab), frames(11));
    assert_eq!(span(&q, ab), range(45, 11));
    undo_redo(&p, &q, EditLabel::Trim);
    // Trimming at the cut moves it: the transition goes.
    let q = p.trim_item(f.a, range(0, 45)).expect("trim tail");
    assert!(!exists(&q, ab));
    q.validate().expect("valid");
    // A one-frame clip leaves room for one frame centred (it lies after the cut).
    let q = p.trim_item(f.a, range(49, 1)).expect("trim");
    assert_eq!(span(&q, ab), range(49, 3));
}

#[test]
fn clip_edits_that_reduce_the_head_handle_shorten_the_transition() {
    let f = fixture();
    let (p, ab) = add(&f.project, Some(f.a), Some(f.b), 20, Alignment::End).expect("ab");
    assert_eq!(span(&p, ab), range(30, 20));
    // 10 frames of media before B's start leaves 10 frames.
    let q = p.slip_clip(f.b, frames(10)).expect("slip");
    assert_eq!(span(&q, ab), range(40, 10));
    undo_redo(&p, &q, EditLabel::Slip);
    let q = p.slip_clip(f.b, Time::ZERO).expect("slip");
    assert!(!exists(&q, ab));
    // At double speed B's 1 s of source is 12.5 frames: 12 whole frames fit.
    let q = p
        .set_clip_speed(f.b, Rational::new(2, 1), SpeedKeep::Timeline)
        .expect("speed");
    assert_eq!(duration(&q, ab), frames(12));
    undo_redo(&p, &q, EditLabel::Speed);
    // A speed change that moves A's end removes the transition.
    let q = p
        .set_clip_speed(f.a, Rational::new(2, 1), SpeedKeep::Source)
        .expect("speed");
    assert!(!exists(&q, ab));
    // Edits that leave the range, source start and speed alone keep the transition node.
    let q = p.set_clip_gain(f.b, -6.0).expect("gain");
    let tr = |p: &Project| {
        p.sequence(f.seq).expect("seq").tracks[0]
            .transitions
            .clone()
    };
    let (before, after) = (tr(&p), tr(&q));
    assert_eq!(before.len(), 1);
    assert!(Arc::ptr_eq(&before[0], &after[0]));
}

#[test]
fn when_two_transitions_compete_for_a_gap_the_earlier_keeps_its_length() {
    let f = fixture();
    let (p, c_out) = add(&f.project, Some(f.c), None, 15, Alignment::Start).expect("out");
    let (p, d_in) = add(&p, None, Some(f.d), 10, Alignment::End).expect("in");
    assert_eq!(span(&p, d_in), range(140, 10));
    let q = p.move_item(f.d, f.v1, frames(145)).expect("move");
    assert_eq!(span(&q, c_out), range(125, 15));
    assert_eq!(span(&q, d_in), range(140, 5));
    q.validate().expect("valid");
}

#[test]
fn a_track_kind_change_needs_the_transitions_on_the_new_grid() {
    let p = Project::new("uid-kind", "Kind");
    let ntsc = Rational::new(30000, 1001);
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", ntsc, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let five = Time::from_frames(5, ntsc).expect("t");
    let spec = |start| ClipSpec {
        media,
        range: TimeRange::new(start, five),
        source_start: Time::ZERO,
    };
    let (p, a) = p.insert_clip(v1, spec(Time::ZERO)).expect("a");
    let (p, b) = p.insert_clip(v1, spec(five)).expect("b");
    let one = Time::from_frames(1, ntsc).expect("t");
    let (q, _) = p
        .add_transition(TransitionSpec {
            alignment: Alignment::Start,
            ..TransitionSpec::new(Some(a), Some(b), one)
        })
        .expect("add");
    // One NTSC frame is 1601.6 samples.
    assert_eq!(
        q.set_track_kind(v1, TrackKind::Audio).map(|_| ()),
        Err(EditError::OffGrid {
            time: one,
            grid: Time::new(1, 48_000).expect("t"),
            kind: TrackKind::Audio,
        })
    );
    p.set_track_kind(v1, TrackKind::Audio)
        .expect("no transitions");
    // At 25 fps a frame is 1920 samples.
    let f = fixture();
    let (q, t) = add(&f.project, Some(f.a), Some(f.b), 10, Alignment::Centre).expect("add");
    let q = q.set_track_kind(f.v1, TrackKind::Audio).expect("kind");
    assert_eq!(span(&q, t), range(45, 10));
}

#[test]
fn the_segment_query_returns_the_clip_or_the_transition_and_its_progress() {
    let f = fixture();
    let (p, ab) = add(&f.project, Some(f.a), Some(f.b), 10, Alignment::Centre).expect("ab");
    let item = |s: Option<Segment>| match s {
        Some(Segment::Item(i)) => Some(i.id),
        _ => None,
    };
    let progress = |s: Option<Segment>| match s {
        Some(Segment::Transition(t)) => Some((t.transition.id, t.progress)),
        _ => None,
    };
    assert_eq!(item(segment(&p, &f, frames(44))), Some(f.a));
    assert_eq!(progress(segment(&p, &f, frames(45))), Some((ab, 0.0)));
    assert_eq!(progress(segment(&p, &f, frames(50))), Some((ab, 0.5)));
    assert_eq!(progress(segment(&p, &f, frames(54))), Some((ab, 0.9)));
    let mid = frames(45)
        .checked_add(Time::new(1, 50).expect("t"))
        .expect("t");
    assert_eq!(progress(segment(&p, &f, mid)), Some((ab, 0.05)));
    assert_eq!(item(segment(&p, &f, frames(55))), Some(f.b));
    match segment(&p, &f, frames(47)) {
        Some(Segment::Transition(t)) => {
            assert_eq!(t.outgoing.map(|i| i.id), Some(f.a));
            assert_eq!(t.incoming.map(|i| i.id), Some(f.b));
            assert_eq!(t.span, range(45, 10));
        }
        other => panic!("{other:?}"),
    }
    // Start and end alignments.
    let q = p
        .set_transition_alignment(ab, Alignment::Start)
        .expect("start");
    assert_eq!(item(segment(&q, &f, frames(49))), Some(f.a));
    assert_eq!(progress(segment(&q, &f, frames(50))), Some((ab, 0.0)));
    assert_eq!(progress(segment(&q, &f, frames(59))), Some((ab, 0.9)));
    assert_eq!(item(segment(&q, &f, frames(60))), Some(f.b));
    let q = p.set_transition_alignment(ab, Alignment::End).expect("end");
    assert_eq!(item(segment(&q, &f, frames(39))), Some(f.a));
    assert_eq!(progress(segment(&q, &f, frames(40))), Some((ab, 0.0)));
    assert_eq!(progress(segment(&q, &f, frames(49))), Some((ab, 0.9)));
    assert_eq!(item(segment(&q, &f, frames(50))), Some(f.b));
    // Plain cuts, gaps and outside the track.
    assert_eq!(item(segment(&p, &f, frames(100))), Some(f.c));
    assert_eq!(item(segment(&p, &f, frames(99))), Some(f.b));
    assert_eq!(segment(&p, &f, frames(130)), None);
    assert_eq!(segment(&p, &f, frames(200)), None);
    assert_eq!(segment(&p, &f, -frames(1)), None);
    assert_eq!(item(segment(&p, &f, Time::ZERO)), Some(f.a));

    // One-sided transitions in the gap: C fades out, D fades in centred.
    let (p, c_out) = add(&p, Some(f.c), None, 10, Alignment::Start).expect("out");
    let (p, d_in) = add(&p, None, Some(f.d), 10, Alignment::Centre).expect("in");
    assert_eq!(progress(segment(&p, &f, frames(125))), Some((c_out, 0.0)));
    assert_eq!(progress(segment(&p, &f, frames(134))), Some((c_out, 0.9)));
    assert_eq!(segment(&p, &f, frames(135)), None);
    assert_eq!(segment(&p, &f, frames(144)), None);
    assert_eq!(progress(segment(&p, &f, frames(145))), Some((d_in, 0.0)));
    assert_eq!(progress(segment(&p, &f, frames(150))), Some((d_in, 0.5)));
    assert_eq!(progress(segment(&p, &f, frames(154))), Some((d_in, 0.9)));
    assert_eq!(item(segment(&p, &f, frames(155))), Some(f.d));
    match segment(&p, &f, frames(146)) {
        Some(Segment::Transition(t)) => {
            assert_eq!(t.outgoing, None);
            assert_eq!(t.incoming.map(|i| i.id), Some(f.d));
        }
        other => panic!("{other:?}"),
    }
    match segment(&p, &f, frames(126)) {
        Some(Segment::Transition(t)) => {
            assert_eq!(t.outgoing.map(|i| i.id), Some(f.c));
            assert_eq!(t.incoming, None);
        }
        other => panic!("{other:?}"),
    }
    // A fade in at zero.
    let (p, a_in) = add(&p, None, Some(f.a), 5, Alignment::Start).expect("a in");
    assert_eq!(progress(segment(&p, &f, Time::ZERO)), Some((a_in, 0.0)));
    assert_eq!(item(segment(&p, &f, frames(5))), Some(f.a));

    let seq = p.sequence(f.seq).expect("seq");
    assert_eq!(
        seq.segment_at(TrackId(999), Time::ZERO).map(|_| ()),
        Err(EditError::UnknownTrack(TrackId(999)))
    );
    // The source time a clip shows inside a transition, past its own range.
    let (_, track, _) = p.find_transition(ab).expect("ab");
    let a = track.item(f.a).expect("a");
    let b = track.item(f.b).expect("b");
    let clip_a = a.clip().expect("clip");
    let clip_b = b.clip().expect("clip");
    assert_eq!(
        clip_a.source_time_at(a.range.start, frames(54)),
        Ok(Time::seconds(2).checked_add(frames(54)).expect("t"))
    );
    assert_eq!(
        clip_b.source_time_at(b.range.start, frames(45)),
        Ok(Time::seconds(1).checked_sub(frames(5)).expect("t"))
    );
}

#[test]
fn tail_handles_are_checked_against_known_media_lengths_and_count_for_relinking() {
    let f = fixture();
    let (p, ab) = add(&f.project, Some(f.a), Some(f.b), 10, Alignment::Start).expect("ab");
    let media = p.media().iter().next().expect("media").id;
    // A shows source 2 s to 4 s; the transition needs 10 more frames, to 4.4 s.
    let needed = Time::new(22, 5).expect("t");
    assert_eq!(
        p.transition_needs(ab).expect("needs").outgoing,
        Some((f.a, media, needed))
    );
    p.check_transition_media(ab, |_| Some(needed))
        .expect("exactly enough");
    p.check_transition_media(ab, |_| None).expect("unknown");
    assert_eq!(
        p.check_transition_media(ab, |_| Some(Time::new(43, 10).expect("t"))),
        Err(EditError::TransitionHandle {
            item: f.a,
            needed: frames(10),
            available: Time::new(3, 10).expect("t"),
        })
    );
    assert_eq!(
        p.check_transition_media(TransitionId(999), |_| None),
        Err(EditError::UnknownTransition(TransitionId(999)))
    );
    // D, the last clip, ends at source 6 s; so the tail handle of A's transition is what
    // relinking measures only when it reaches further.
    let used = f.project.source_used_until(media).expect("used");
    assert_eq!(used, Some(Time::seconds(6)));
    let (q, d_out) = add(&p, Some(f.d), None, 25, Alignment::Start).expect("d out");
    assert_eq!(
        q.source_used_until(media).expect("used"),
        Some(Time::seconds(7))
    );
    assert_eq!(
        q.transition_needs(d_out).expect("needs").incoming,
        None,
        "nothing comes in"
    );
}

#[test]
fn transitions_travel_as_their_own_autosave_records() {
    let f = fixture();
    let (p, t) = add(&f.project, Some(f.a), Some(f.b), 10, Alignment::Centre).expect("ab");
    let (p, out) = add(&p, Some(f.c), None, 5, Alignment::End).expect("out");
    let base = flatten(&p);
    assert!(matches!(base.get(&t.node()), Some(Node::Transition { track, .. }) if *track == f.v1));
    let q = p.set_transition_duration(t, frames(4)).expect("duration");
    let diff = Diff::between(&base, &flatten(&q));
    assert!(diff.removals.is_empty());
    assert_eq!(diff.upserts.len(), 1);
    assert_eq!(diff.upserts[0].id(), t.node());
    let mut flat = base.clone();
    diff.apply(&mut flat);
    assert!(rebuild(&flat).expect("rebuild").content_eq(&q));
    let r = q.remove_transition(out).expect("remove");
    let diff = Diff::between(&flatten(&q), &flatten(&r));
    assert_eq!(diff.removals, vec![out.node()]);
    assert!(diff.upserts.is_empty());
    // A transition record on a track no sequence holds is refused.
    let mut flat = flatten(&r);
    let Some(Node::Transition { transition, .. }) = flat.get(&t.node()).cloned() else {
        panic!("no record");
    };
    flat.insert(
        t.node(),
        Node::Transition {
            track: TrackId(777),
            transition,
        },
    );
    assert!(matches!(
        rebuild(&flat),
        Err(ValidationError::Structure(message)) if message.contains("#777")
    ));
}

/// Three transitions: A–B centred, B–C ending at the cut, C fading out into the gap.
fn saved() -> (Project, Fixture) {
    let f = fixture();
    let p = f.project.slip_clip(f.c, Time::seconds(1)).expect("slip");
    let (p, _) = add(&p, Some(f.a), Some(f.b), 10, Alignment::Centre).expect("ab");
    let (p, _) = add(&p, Some(f.b), Some(f.c), 6, Alignment::End).expect("bc");
    let (p, _) = add(&p, Some(f.c), None, 10, Alignment::Start).expect("c out");
    (p, f)
}

fn load_with(p: &Project, edit: impl FnOnce(&mut Value)) -> Result<Project, LoadError> {
    let mut json: Value = serde_json::from_slice(&schema::save(p)).expect("JSON");
    edit(&mut json["project"]["sequences"][0]["tracks"][0]["transitions"]);
    schema::load(&serde_json::to_vec(&json).expect("JSON"))
}

fn invalid(result: Result<Project, LoadError>) -> EditError {
    match result {
        Err(LoadError::Invalid(ValidationError::Track { error, .. })) => error,
        other => panic!("{other:?}"),
    }
}

#[test]
fn loading_refuses_transitions_that_break_the_invariants() {
    let (p, f) = saved();
    let loaded = schema::load(&schema::save(&p)).expect("loads");
    assert!(loaded.content_eq(&p));
    assert_eq!(
        invalid(load_with(&p, |t| t[0]["duration"] = "1/50".into())),
        EditError::OffGrid {
            time: Time::new(1, 50).expect("t"),
            grid: frames(1),
            kind: TrackKind::Video,
        }
    );
    assert_eq!(
        invalid(load_with(&p, |t| t[0]["duration"] = "0".into())),
        EditError::EmptyRange(Time::ZERO)
    );
    assert_eq!(
        invalid(load_with(&p, |t| t[0]["kind"] = "Cross".into())),
        EditError::InvalidTransitionKind("Cross".into())
    );
    assert_eq!(
        invalid(load_with(&p, |t| t[0]["easing"] = "".into())),
        EditError::InvalidEasing(String::new())
    );
    assert_eq!(
        invalid(load_with(&p, |t| t[0]["incoming"] = 999.into())),
        EditError::UnknownItem(ItemId(999))
    );
    assert_eq!(
        invalid(load_with(&p, |t| t[0]["incoming"] = f.c.0.into())),
        EditError::NotAdjacent {
            outgoing: f.a,
            incoming: f.c,
        }
    );
    assert_eq!(
        invalid(load_with(&p, |t| {
            t[0]["outgoing"] = Value::Null;
            t[0]["incoming"] = Value::Null;
        })),
        EditError::TransitionNeedsClip
    );
    assert_eq!(
        invalid(load_with(&p, |t| t[2]["incoming"] = f.d.0.into())),
        EditError::NotAdjacent {
            outgoing: f.c,
            incoming: f.d,
        }
    );
    // C (source 1 s) cannot give 30 frames before its start.
    assert_eq!(
        invalid(load_with(&p, |t| t[1]["duration"] = "6/5".into())),
        EditError::TransitionHandle {
            item: f.c,
            needed: frames(30),
            available: frames(25),
        }
    );
    // A–B starting at the cut for 46 frames reaches into B–C's span, 94 to 100.
    assert_eq!(
        invalid(load_with(&p, |t| {
            t[0]["alignment"] = "start".into();
            t[0]["duration"] = "46/25".into();
        })),
        EditError::TransitionRoom {
            side: CutSide::Before,
            needed: frames(6),
            available: frames(4),
        }
    );
    // Two transitions on one cut.
    let second = p.sequences()[0].tracks[0].transitions[1].id;
    assert_eq!(
        invalid(load_with(&p, |t| {
            t[1]["outgoing"] = f.a.0.into();
            t[1]["incoming"] = f.b.0.into();
        })),
        EditError::TransitionExists(second)
    );
    assert!(matches!(
        load_with(&p, |t| t[0]["alignment"] = "middle".into()),
        Err(LoadError::Model { .. })
    ));
    // Ids out of order, used twice, or never allocated.
    assert!(matches!(
        load_with(&p, |t| t.as_array_mut().expect("list").swap(0, 1)),
        Err(LoadError::Invalid(ValidationError::Structure(_)))
    ));
    assert!(matches!(
        load_with(&p, |t| t[1]["id"] = t[0]["id"].clone()),
        Err(LoadError::Invalid(ValidationError::DuplicateId(_)))
    ));
    assert!(matches!(
        load_with(&p, |t| t[2]["id"] = 5000.into()),
        Err(LoadError::Invalid(ValidationError::Unallocated { .. }))
    ));
    let err = load_with(&p, |t| t[0]["kind"] = "Cross".into()).expect_err("refused");
    assert_eq!(err.i18n_key(), "project.open_error.damaged");
}
