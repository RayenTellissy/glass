//! Contract tests for transitions (schema v5), written from the transitions contract rather
//! than the implementation: the alignment split, which handle each alignment needs, the
//! order of `add_transition`'s refusals, the fit at exact boundaries, how every kind of clip
//! edit fits a track's transitions, the segment query at span edges, the tail-handle check,
//! diffs of every transition edit, and the refusals of loaded, migrated and recovered trees.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use project::autosave::{self, AutosaveError, AutosaveWriter};
use project::diff::{Diff, Node, ROOT_ID, flatten, rebuild};
use project::params::{ParamDef, ParamSchema, ParamValue, Range};
use project::schema::{self, LoadError};
use project::{
    Alignment, ClipSource, ClipSpec, CutSide, EditError, EditLabel, ItemId, MediaId, NodeId,
    Project, Rational, Segment, SequenceId, SpeedKeep, Time, TimeRange, TrackId, TrackKind,
    TransitionId, TransitionSpec, ValidationError,
};
use serde_json::Value;

const FPS: Rational = Rational::new(10, 1);

/// `n` frames at 10 fps.
fn f(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn secs(num: i64, den: i64) -> Time {
    Time::new(num, den).expect("time")
}

fn ms(n: i64) -> Time {
    secs(n, 1000)
}

fn at(start: i64, len: i64) -> TimeRange {
    TimeRange::new(f(start), f(len))
}

struct Fix {
    project: Project,
    seq: SequenceId,
    v1: TrackId,
    v2: TrackId,
    m: MediaId,
    m2: MediaId,
    /// V1: A 0..20 from 0, B 20..40 from 2 s (media m2), C 40..50 from 1 s, a gap,
    /// D 60..80 from 3 s. Every clip at speed 1.
    a: ItemId,
    b: ItemId,
    c: ItemId,
    d: ItemId,
    /// V2: E 0..20 from 0.
    e: ItemId,
    /// S1: two cues that meet at 10.
    cue1: ItemId,
    cue2: ItemId,
}

fn fixture() -> Fix {
    let p = Project::new("uid-transitions-contract", "Contract");
    let (p, m) = p.add_media("aa01", "/media/a.mov").expect("media");
    let (p, m2) = p.add_media("bb02", "/media/b.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).expect("v2");
    let (p, s1) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let clip = |media, start, len, source_start| ClipSpec {
        media,
        range: at(start, len),
        source_start,
    };
    let (p, a) = p.insert_clip(v1, clip(m, 0, 20, Time::ZERO)).expect("a");
    let (p, b) = p
        .insert_clip(v1, clip(m2, 20, 20, Time::seconds(2)))
        .expect("b");
    let (p, c) = p
        .insert_clip(v1, clip(m, 40, 10, Time::seconds(1)))
        .expect("c");
    let (p, d) = p
        .insert_clip(v1, clip(m, 60, 20, Time::seconds(3)))
        .expect("d");
    let (p, e) = p.insert_clip(v2, clip(m, 0, 20, Time::ZERO)).expect("e");
    let (p, cue1) = p.insert_subtitle(s1, at(0, 10), "One").expect("cue1");
    let (p, cue2) = p.insert_subtitle(s1, at(10, 10), "Two").expect("cue2");
    Fix {
        project: p,
        seq,
        v1,
        v2,
        m,
        m2,
        a,
        b,
        c,
        d,
        e,
        cue1,
        cue2,
    }
}

fn spec(o: Option<ItemId>, i: Option<ItemId>, n: i64, alignment: Alignment) -> TransitionSpec {
    TransitionSpec {
        alignment,
        ..TransitionSpec::new(o, i, f(n))
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

/// The span of `t`, or `None` once it has been removed.
fn span_of(p: &Project, t: TransitionId) -> Option<TimeRange> {
    p.find_transition(t)
        .map(|_| p.transition_span(t).expect("span"))
}

fn clip_at(f: &Fix, start: i64, len: i64, source_start: Time) -> ClipSpec {
    ClipSpec {
        media: f.m,
        range: at(start, len),
        source_start,
    }
}

#[test]
fn centre_split_puts_the_extra_odd_step_after_the_cut() {
    let grid = f(1);
    let cases = [
        (Alignment::Centre, 1, (0, 1)),
        (Alignment::Centre, 2, (1, 1)),
        (Alignment::Centre, 3, (1, 2)),
        (Alignment::Centre, 4, (2, 2)),
        (Alignment::Centre, 9, (4, 5)),
        (Alignment::Centre, 10, (5, 5)),
        (Alignment::Start, 1, (0, 1)),
        (Alignment::Start, 4, (0, 4)),
        (Alignment::End, 1, (1, 0)),
        (Alignment::End, 4, (4, 0)),
    ];
    for (alignment, n, (before, after)) in cases {
        assert_eq!(
            alignment.split(f(n), grid),
            Ok((f(before), f(after))),
            "{alignment:?} {n}"
        );
    }
    // On the audio sample grid too.
    let sample = secs(1, 48_000);
    assert_eq!(
        Alignment::Centre.split(secs(3, 48_000), sample),
        Ok((sample, secs(2, 48_000)))
    );
    assert_eq!(Alignment::default(), Alignment::Centre);
}

#[test]
fn start_needs_the_outgoing_tail_and_end_the_incoming_head_at_each_clips_speed() {
    let fx = fixture();
    // A at half speed shows source 0 to 1 s; B at double speed shows 2 s to 6 s, so its
    // 2 s of head handle is 1 s (10 frames) of timeline.
    let p = fx
        .project
        .set_clip_speed(fx.a, Rational::new(1, 2), SpeedKeep::Timeline)
        .expect("a speed")
        .set_clip_speed(fx.b, Rational::new(2, 1), SpeedKeep::Timeline)
        .expect("b speed");
    let needs = |n: i64, alignment| {
        let (q, t) = add(&p, Some(fx.a), Some(fx.b), n, alignment).expect("add");
        let needs = q.transition_needs(t).expect("needs");
        (span_of(&q, t), needs.outgoing, needs.incoming)
    };
    // Starting at the cut: all of it lies after, showing A past its end.
    assert_eq!(
        needs(6, Alignment::Start),
        (
            Some(at(20, 6)),
            Some((fx.a, ClipSource::Media(fx.m), secs(13, 10))),
            Some((fx.b, ClipSource::Media(fx.m2), Time::seconds(2))),
        )
    );
    // Ending at the cut: all of it lies before, showing B before its start.
    assert_eq!(
        needs(6, Alignment::End),
        (
            Some(at(14, 6)),
            Some((fx.a, ClipSource::Media(fx.m), Time::seconds(1))),
            Some((fx.b, ClipSource::Media(fx.m2), secs(4, 5))),
        )
    );
    // Centred on an odd count: 2 frames before, 3 after.
    assert_eq!(
        needs(5, Alignment::Centre),
        (
            Some(at(18, 5)),
            Some((fx.a, ClipSource::Media(fx.m), secs(23, 20))),
            Some((fx.b, ClipSource::Media(fx.m2), secs(8, 5))),
        )
    );
    // Only the side before the cut is limited by B's head handle.
    add(&p, Some(fx.a), Some(fx.b), 10, Alignment::End).expect("exactly the handle");
    assert_eq!(
        add(&p, Some(fx.a), Some(fx.b), 11, Alignment::End).map(|_| ()),
        Err(EditError::TransitionHandle {
            item: fx.b,
            needed: f(11),
            available: Time::seconds(1),
        })
    );
    add(&p, Some(fx.a), Some(fx.b), 11, Alignment::Start).expect("no head handle needed");
}

#[test]
fn add_transition_reports_the_first_broken_rule_when_several_are_broken() {
    let fx = fixture();
    let (p, ab) = add(&fx.project, Some(fx.a), Some(fx.b), 10, Alignment::Centre).expect("ab");
    let unknown = ItemId(999);
    let half_frame = secs(1, 20);
    let off_grid_huge = f(1000).checked_add(half_frame).expect("t");
    let off_grid = f(60).checked_add(half_frame).expect("t");
    let with = |o, i, duration, edit: &dyn Fn(&mut TransitionSpec)| {
        let mut s = TransitionSpec::new(o, i, duration);
        edit(&mut s);
        p.add_transition(s).map(|_| ())
    };
    let bad_ids = |s: &mut TransitionSpec| {
        s.kind = "Kind".into();
        s.direction = Some("Up".into());
        s.easing = "Ease".into();
    };
    let cases: Vec<(&str, Result<(), EditError>, EditError)> = vec![
        (
            "no clip at all, bad ids, empty duration",
            with(None, None, Time::ZERO, &bad_ids),
            EditError::TransitionNeedsClip,
        ),
        (
            "bad kind, direction and easing, empty duration, unknown item",
            with(Some(unknown), Some(fx.b), Time::ZERO, &bad_ids),
            EditError::InvalidTransitionKind("Kind".into()),
        ),
        (
            "bad direction and easing, empty duration, unknown item",
            with(Some(unknown), Some(fx.b), -f(1), &|s| {
                s.direction = Some("Up".into());
                s.easing = "Ease".into();
            }),
            EditError::InvalidDirection("Up".into()),
        ),
        (
            "bad easing, empty duration, unknown item",
            with(Some(unknown), Some(fx.b), Time::ZERO, &|s| {
                s.easing = "Ease".into();
            }),
            EditError::InvalidEasing("Ease".into()),
        ),
        (
            "empty duration, unknown item, not a clip",
            with(Some(unknown), Some(fx.cue1), Time::ZERO, &|_| {}),
            EditError::EmptyRange(Time::ZERO),
        ),
        (
            "unknown incoming beats an outgoing that is not a clip",
            with(Some(fx.cue1), Some(unknown), half_frame, &|_| {}),
            EditError::UnknownItem(unknown),
        ),
        (
            "neither is a clip: outgoing first",
            with(Some(fx.cue1), Some(fx.cue2), half_frame, &|_| {}),
            EditError::NotAClip(fx.cue1),
        ),
        (
            "clips that do not meet, on an edge that has a transition",
            with(Some(fx.a), Some(fx.c), off_grid_huge, &|_| {}),
            EditError::NotAdjacent {
                outgoing: fx.a,
                incoming: fx.c,
            },
        ),
        (
            "clips meeting in time on different tracks",
            with(Some(fx.e), Some(fx.b), off_grid_huge, &|_| {}),
            EditError::NotAdjacent {
                outgoing: fx.e,
                incoming: fx.b,
            },
        ),
        (
            "fade out of an edge that meets an item and has a transition",
            with(Some(fx.a), None, off_grid_huge, &|_| {}),
            EditError::CutNotFree {
                item: fx.a,
                other: fx.b,
            },
        ),
        (
            "fade in to an edge that meets an item and has a transition",
            with(None, Some(fx.b), off_grid_huge, &|_| {}),
            EditError::CutNotFree {
                item: fx.b,
                other: fx.a,
            },
        ),
        (
            "an existing transition beats a duration off the grid and too long",
            with(Some(fx.a), Some(fx.b), off_grid_huge, &|_| {}),
            EditError::TransitionExists(ab),
        ),
        (
            "off the grid and too long",
            with(Some(fx.b), Some(fx.c), off_grid, &|_| {}),
            EditError::OffGrid {
                time: off_grid,
                grid: f(1),
                kind: TrackKind::Video,
            },
        ),
        (
            // A–B (15..25) takes B's first 5 frames.
            "room before, head handle and room after all short",
            with(Some(fx.b), Some(fx.c), f(60), &|_| {}),
            EditError::TransitionRoom {
                side: CutSide::Before,
                needed: f(30),
                available: f(15),
            },
        ),
        (
            "head handle and room after short",
            with(Some(fx.b), Some(fx.c), f(24), &|_| {}),
            EditError::TransitionHandle {
                item: fx.c,
                needed: f(12),
                available: f(10),
            },
        ),
        (
            "room after short by one step",
            with(Some(fx.b), Some(fx.c), f(21), &|_| {}),
            EditError::TransitionRoom {
                side: CutSide::After,
                needed: f(11),
                available: f(10),
            },
        ),
    ];
    for (name, got, expected) in cases {
        assert_eq!(got, Err(expected), "{name}");
    }
    // Exactly the head handle before and exactly the clip after fits.
    let (q, bc) = add(&p, Some(fx.b), Some(fx.c), 20, Alignment::Centre).expect("bc");
    assert_eq!(span_of(&q, bc), Some(at(30, 20)));

    // The duration edit: an unknown transition before the grid, the grid before the fit.
    assert_eq!(
        p.set_transition_duration(TransitionId(999), half_frame)
            .map(|_| ()),
        Err(EditError::UnknownTransition(TransitionId(999)))
    );
    assert_eq!(
        p.set_transition_duration(ab, off_grid_huge).map(|_| ()),
        Err(EditError::OffGrid {
            time: off_grid_huge,
            grid: f(1),
            kind: TrackKind::Video,
        })
    );
}

#[test]
fn spans_may_touch_on_a_shared_clip_but_not_overlap_by_one_step() {
    let fx = fixture();
    // V2: E 0..20, F 20..26 (6 frames), G 26..46, both with 1 s of head handle.
    let (p, fc) = fx
        .project
        .insert_clip(fx.v2, clip_at(&fx, 20, 6, Time::seconds(1)))
        .expect("f");
    let (p, g) = p
        .insert_clip(fx.v2, clip_at(&fx, 26, 20, Time::seconds(1)))
        .expect("g");

    // E–F centred on 5: 2 before, 3 after, so 18..23.
    let (q, ef) = add(&p, Some(fx.e), Some(fc), 5, Alignment::Centre).expect("ef");
    assert_eq!(span_of(&q, ef), Some(at(18, 5)));
    // F–G centred on 7 takes the 3 frames from 23 to the cut: touching.
    let (r, fg) = add(&q, Some(fc), Some(g), 7, Alignment::Centre).expect("fg");
    assert_eq!(span_of(&r, fg), Some(at(23, 7)));
    r.validate().expect("touching spans are valid");
    // 8 needs 4 before the cut: one step into E–F.
    assert_eq!(
        add(&q, Some(fc), Some(g), 8, Alignment::Centre).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::Before,
            needed: f(4),
            available: f(3),
        })
    );

    // The other way round: F–G first, 22..30; E–F may then reach 22 but not 23.
    let (q, fg) = add(&p, Some(fc), Some(g), 8, Alignment::Centre).expect("fg");
    let (r, ef) = add(&q, Some(fx.e), Some(fc), 4, Alignment::Centre).expect("ef");
    assert_eq!(span_of(&r, ef), Some(at(18, 4)));
    assert_eq!(
        add(&q, Some(fx.e), Some(fc), 5, Alignment::Centre).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::After,
            needed: f(3),
            available: f(2),
        })
    );
    // Edits on a transition refuse a one-step overlap and accept touching.
    assert_eq!(
        r.set_transition_alignment(ef, Alignment::Start).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::After,
            needed: f(4),
            available: f(2),
        })
    );
    let s = r.set_transition_alignment(ef, Alignment::End).expect("end");
    assert_eq!(span_of(&s, ef), Some(at(16, 4)));
    let s = r.set_transition_duration(fg, f(9)).expect("touching");
    assert_eq!(span_of(&s, fg), Some(at(22, 9)));
    assert_eq!(
        r.set_transition_duration(fg, f(10)).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::Before,
            needed: f(5),
            available: f(4),
        })
    );
}

#[test]
fn a_head_handle_between_grid_steps_allows_the_whole_steps_below_it() {
    let fx = fixture();
    // 0.25 s of media before B's start: 2.5 frames.
    let p = fx.project.slip_clip(fx.b, secs(1, 4)).expect("slip");
    let (q, t) = add(&p, Some(fx.a), Some(fx.b), 2, Alignment::End).expect("2 frames");
    assert_eq!(span_of(&q, t), Some(at(18, 2)));
    add(&p, Some(fx.a), Some(fx.b), 5, Alignment::Centre).expect("2 frames before");
    for (n, alignment) in [(3, Alignment::End), (6, Alignment::Centre)] {
        assert_eq!(
            add(&p, Some(fx.a), Some(fx.b), n, alignment).map(|_| ()),
            Err(EditError::TransitionHandle {
                item: fx.b,
                needed: f(3),
                available: secs(1, 4),
            }),
            "{alignment:?} {n}"
        );
    }
}

#[test]
fn fades_fit_the_room_a_gap_or_the_track_start_leaves() {
    let fx = fixture();
    let (p, v3) = fx
        .project
        .add_track(fx.seq, TrackKind::Video, "V3", None)
        .expect("v3");
    // X 3..13 and Y 14..24, one frame apart, each with 1 s of head handle.
    let (p, x) = p
        .insert_clip(v3, clip_at(&fx, 3, 10, Time::seconds(1)))
        .expect("x");
    let (p, y) = p
        .insert_clip(v3, clip_at(&fx, 14, 10, Time::seconds(1)))
        .expect("y");
    // A fade in to the first clip has the room from zero.
    let (q, x_in) = add(&p, None, Some(x), 6, Alignment::Centre).expect("x in");
    assert_eq!(span_of(&q, x_in), Some(at(0, 6)));
    assert_eq!(
        add(&p, None, Some(x), 8, Alignment::Centre).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::Before,
            needed: f(4),
            available: f(3),
        })
    );
    // A one-frame gap holds one frame of fade.
    let (q, x_out) = add(&p, Some(x), None, 1, Alignment::Start).expect("x out");
    assert_eq!(span_of(&q, x_out), Some(at(13, 1)));
    assert_eq!(
        add(&p, Some(x), None, 2, Alignment::Start).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::After,
            needed: f(2),
            available: f(1),
        })
    );
    let (r, y_in) = add(&p, None, Some(y), 1, Alignment::End).expect("y in");
    assert_eq!(span_of(&r, y_in), Some(at(13, 1)));
    // With X fading out over the gap, Y can fade in only over its own head.
    assert_eq!(
        add(&q, None, Some(y), 1, Alignment::End).map(|_| ()),
        Err(EditError::TransitionRoom {
            side: CutSide::Before,
            needed: f(1),
            available: Time::ZERO,
        })
    );
    let (r, y_in) = add(&q, None, Some(y), 1, Alignment::Start).expect("y in");
    assert_eq!(span_of(&r, y_in), Some(at(14, 1)));
    r.validate().expect("valid");
}

/// Adds `base` in order, applies `edit`, and checks every transition's span afterwards
/// (`None`: removed). The result must be a valid tree.
fn check_refit(
    name: &str,
    fx: &Fix,
    base: &[(Option<ItemId>, Option<ItemId>, i64, Alignment)],
    edit: impl FnOnce(&Project) -> Result<Project, EditError>,
    expected: &[Option<TimeRange>],
) {
    let mut p = fx.project.clone();
    let mut ids = Vec::new();
    for &(o, i, n, alignment) in base {
        let (q, t) = add(&p, o, i, n, alignment).unwrap_or_else(|e| panic!("{name}: {e}"));
        p = q;
        ids.push(t);
    }
    let q = edit(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
    let got: Vec<_> = ids.iter().map(|&t| span_of(&q, t)).collect();
    assert_eq!(got, expected, "{name}");
    q.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
}

#[test]
fn clip_edits_shrink_transitions_to_the_most_whole_steps_that_fit() {
    use Alignment::{Centre, End, Start};
    let fx = fixture();
    let (a, b, c, d) = (Some(fx.a), Some(fx.b), Some(fx.c), Some(fx.d));

    // A–B centred on 20 is 10..30. B trimmed to 4 frames: ceil(n/2) ≤ 4, so 8.
    check_refit(
        "trim the incoming tail: room after binds",
        &fx,
        &[(a, b, 20, Centre)],
        |p| p.trim_item(fx.b, at(20, 4)),
        &[Some(at(16, 8))],
    );
    check_refit(
        "trim the incoming tail: start alignment",
        &fx,
        &[(a, b, 15, Start)],
        |p| p.trim_item(fx.b, at(20, 8)),
        &[Some(at(20, 8))],
    );
    check_refit(
        "trim the outgoing head: end alignment",
        &fx,
        &[(a, b, 15, End)],
        |p| p.trim_item(fx.a, at(12, 8)),
        &[Some(at(12, 8))],
    );
    check_refit(
        "trim the incoming head moves the cut",
        &fx,
        &[(a, b, 10, Centre)],
        |p| p.trim_item(fx.b, at(22, 18)),
        &[None],
    );
    // 3 frames of head handle: floor(n/2) ≤ 3, so 7.
    check_refit(
        "slip the incoming: the head handle caps the room before",
        &fx,
        &[(a, b, 20, Centre)],
        |p| p.slip_clip(fx.b, secs(3, 10)),
        &[Some(at(17, 7))],
    );
    // Half a frame of head handle: no whole step before the cut. Centred keeps the one
    // step after it; ending at the cut has nothing left.
    check_refit(
        "slip below one step: centred keeps one step",
        &fx,
        &[(a, b, 20, Centre)],
        |p| p.slip_clip(fx.b, secs(1, 20)),
        &[Some(at(20, 1))],
    );
    check_refit(
        "slip below one step: ending at the cut is removed",
        &fx,
        &[(a, b, 10, End)],
        |p| p.slip_clip(fx.b, secs(1, 20)),
        &[None],
    );
    // At 4x B's 2 s of handle is 5 frames: 11 centred.
    check_refit(
        "speed keeping the timeline shrinks the head handle",
        &fx,
        &[(a, b, 30, Centre)],
        |p| p.set_clip_speed(fx.b, Rational::new(4, 1), SpeedKeep::Timeline),
        &[Some(at(15, 11))],
    );
    // At 2x keeping the source, B lasts 10 frames with 10 frames of handle: 20 centred;
    // its tail cut moves away from C.
    check_refit(
        "speed keeping the source shortens the incoming",
        &fx,
        &[(a, b, 30, Centre), (b, c, 5, Start)],
        |p| p.set_clip_speed(fx.b, Rational::new(2, 1), SpeedKeep::Source),
        &[Some(at(10, 20)), None],
    );
    // B shows 0.4 s to 1.4 s: 10 frames long, 4 frames of handle, so 9 centred.
    check_refit(
        "source range of the incoming",
        &fx,
        &[(a, b, 20, Centre)],
        |p| p.set_clip_source_range(fx.b, TimeRange::new(secs(2, 5), Time::seconds(1))),
        &[Some(at(16, 9))],
    );
    check_refit(
        "source range of the outgoing moves the cut",
        &fx,
        &[(a, b, 10, Centre)],
        |p| p.set_clip_source_range(fx.a, TimeRange::new(Time::ZERO, secs(3, 2))),
        &[None],
    );
    // D fades in centred on 8 (56..64); an item put at 52..57 leaves 3 before the cut.
    check_refit(
        "insert into the gap",
        &fx,
        &[(None, d, 8, Centre)],
        |p| {
            p.insert_clip(fx.v1, clip_at(&fx, 52, 5, Time::ZERO))
                .map(|(q, _)| q)
        },
        &[Some(at(57, 7))],
    );
    check_refit(
        "removing the item that shortened a fade does not lengthen it again",
        &fx,
        &[(c, None, 10, Start)],
        |p| {
            let (q, x) = p.insert_clip(fx.v1, clip_at(&fx, 55, 5, Time::ZERO))?;
            q.remove_item(x)
        },
        &[Some(at(50, 5))],
    );
}

#[test]
fn the_earlier_cut_keeps_its_length_and_the_later_fits_after_its_span() {
    use Alignment::{Centre, End, Start};
    let fx = fixture();
    let (c, d) = (Some(fx.c), Some(fx.d));
    // D's fade in (centred on 8, 56..64) is added before C's fade out (50..56), so the
    // earlier cut has the later id.
    let base = [(None, d, 8, Centre), (c, None, 6, Start)];
    check_refit(
        "D at 58: two frames before its cut",
        &fx,
        &base,
        |p| p.move_item(fx.d, fx.v1, f(58)),
        &[Some(at(56, 5)), Some(at(50, 6))],
    );
    check_refit(
        "D at 56: no frame before its cut, one after",
        &fx,
        &base,
        |p| p.move_item(fx.d, fx.v1, f(56)),
        &[Some(at(56, 1)), Some(at(50, 6))],
    );
    check_refit(
        "D at 55: C's fade shrinks to the gap, D's starts where it ends",
        &fx,
        &base,
        |p| p.move_item(fx.d, fx.v1, f(55)),
        &[Some(at(55, 1)), Some(at(50, 5))],
    );
    check_refit(
        "D meets C: neither edge is free",
        &fx,
        &base,
        |p| p.move_item(fx.d, fx.v1, f(50)),
        &[None, None],
    );
    check_refit(
        "ending at the cut with no room before is removed",
        &fx,
        &[(None, d, 4, End), (c, None, 6, Start)],
        |p| p.move_item(fx.d, fx.v1, f(56)),
        &[None, Some(at(50, 6))],
    );
}

#[test]
fn moving_a_clip_to_another_track_fits_the_transitions_of_both() {
    use Alignment::{End, Start};
    let fx = fixture();
    // E fades out past the end of V2 (20..50); D fades in on V1.
    let base = [(Some(fx.e), None, 30, Start), (None, Some(fx.d), 4, End)];
    check_refit(
        "D lands in E's fade",
        &fx,
        &base,
        |p| p.move_item(fx.d, fx.v2, f(35)),
        &[Some(at(20, 15)), None],
    );
    check_refit(
        "D lands against E",
        &fx,
        &base,
        |p| p.move_item(fx.d, fx.v2, f(20)),
        &[None, None],
    );
}

#[test]
fn the_segment_query_at_span_edges() {
    use Alignment::{Centre, End, Start};
    let fx = fixture();
    let p = fx.project.clone();
    let (p, a_in) = add(&p, None, Some(fx.a), 2, Start).expect("a in");
    let (p, ab) = add(&p, Some(fx.a), Some(fx.b), 5, Centre).expect("ab");
    let p = p.set_transition_easing(ab, "ease-in").expect("easing");
    let (p, bc) = add(&p, Some(fx.b), Some(fx.c), 4, End).expect("bc");
    let (p, c_out) = add(&p, Some(fx.c), None, 6, Start).expect("c out");
    let (p, d_in) = add(&p, None, Some(fx.d), 4, End).expect("d in");
    let (p, d_out) = add(&p, Some(fx.d), None, 10, Start).expect("d out");
    let seq = p.sequence(fx.seq).expect("seq");
    let at_time = |t: Time| seq.segment_at(fx.v1, t).expect("query");
    let item = |t: Time| match at_time(t) {
        Some(Segment::Item(i)) => Some(i.id),
        other => panic!("{t}: {other:?}"),
    };
    let progress = |t: Time| match at_time(t) {
        Some(Segment::Transition(x)) => (x.transition.id, x.progress),
        other => panic!("{t}: {other:?}"),
    };
    let before = |n: i64| f(n).checked_sub(ms(1)).expect("t");

    assert_eq!(
        at_time(-ms(1)),
        None,
        "before zero, even with a fade in at 0"
    );
    assert_eq!(progress(Time::ZERO), (a_in, 0.0));
    assert_eq!(item(f(2)), Some(fx.a));
    // A–B: 18..23, 5 frames; easing is not applied.
    assert_eq!(item(before(18)), Some(fx.a));
    assert_eq!(progress(f(18)), (ab, 0.0));
    assert_eq!(progress(f(20)), (ab, 0.4));
    assert_eq!(progress(f(22)), (ab, 0.8), "the last frame is (n − 1) / n");
    assert_eq!(progress(before(23)), (ab, 0.998));
    assert_eq!(
        item(f(23)),
        Some(fx.b),
        "the span's end is the incoming clip"
    );
    // B–C ends at the cut: 36..40.
    assert_eq!(progress(f(36)), (bc, 0.0));
    assert_eq!(progress(f(39)), (bc, 0.75));
    assert_eq!(item(f(40)), Some(fx.c));
    // C's fade out (50..56) and D's fade in (56..60) touch in the gap: the later one
    // holds the shared instant.
    assert_eq!(progress(f(50)), (c_out, 0.0));
    assert_eq!(progress(f(55)), (c_out, 5.0 / 6.0));
    match at_time(before(56)) {
        Some(Segment::Transition(x)) => {
            assert_eq!(x.transition.id, c_out);
            assert_eq!(x.progress, 599.0 / 600.0);
            assert_eq!(x.outgoing.map(|i| i.id), Some(fx.c));
            assert_eq!(x.incoming, None);
            assert_eq!(x.span, at(50, 6));
        }
        other => panic!("{other:?}"),
    }
    match at_time(f(56)) {
        Some(Segment::Transition(x)) => {
            assert_eq!(x.transition.id, d_in);
            assert_eq!(x.progress, 0.0);
            assert_eq!(x.outgoing, None);
            assert_eq!(x.incoming.map(|i| i.id), Some(fx.d));
            assert_eq!(x.span, at(56, 4));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(progress(f(59)), (d_in, 0.75));
    assert_eq!(item(f(60)), Some(fx.d));
    // D fades out past the last item: 80..90, then nothing.
    assert_eq!(progress(f(89)), (d_out, 0.9));
    assert_eq!(at_time(f(90)), None);
    assert_eq!(at_time(f(1000)), None);
}

#[test]
fn the_tail_handle_check_measures_the_outgoing_media_past_its_end_over_its_speed() {
    use Alignment::{Centre, End, Start};
    let fx = fixture();
    // A at double speed shows source 0 to 4 s of media m; B uses media m2.
    let p = fx
        .project
        .set_clip_speed(fx.a, Rational::new(2, 1), SpeedKeep::Timeline)
        .expect("speed");
    let lengths = |m: Option<Time>, m2: Option<Time>| {
        let (fm, fm2) = (fx.m, fx.m2);
        move |id: MediaId| {
            if id == fm {
                m
            } else if id == fm2 {
                m2
            } else {
                panic!("asked for {id}")
            }
        }
    };
    // Starting at the cut, 10 frames (1 s) past A's end: 2 s of source.
    let (q, t) = add(&p, Some(fx.a), Some(fx.b), 10, Start).expect("start");
    assert_eq!(
        q.transition_needs(t).expect("needs").outgoing,
        Some((fx.a, ClipSource::Media(fx.m), Time::seconds(6)))
    );
    q.check_transition_media(t, lengths(Some(Time::seconds(6)), Some(Time::ZERO)))
        .expect("exactly enough; B's media does not count");
    assert_eq!(
        q.check_transition_media(t, lengths(Some(Time::seconds(5)), None)),
        Err(EditError::TransitionHandle {
            item: fx.a,
            needed: Time::seconds(1),
            available: secs(1, 2),
        })
    );
    q.check_transition_media(t, lengths(None, Some(Time::ZERO)))
        .expect("A's length unknown");
    // Centred on 10: 5 frames after the cut.
    let (q, t) = add(&p, Some(fx.a), Some(fx.b), 10, Centre).expect("centre");
    q.check_transition_media(t, lengths(Some(Time::seconds(5)), None))
        .expect("exactly enough");
    assert_eq!(
        q.check_transition_media(t, lengths(Some(secs(49, 10)), None)),
        Err(EditError::TransitionHandle {
            item: fx.a,
            needed: f(5),
            available: secs(9, 20),
        })
    );
    // Nothing after the cut: skipped, even when the file is shorter than A itself.
    let (q, t) = add(&p, Some(fx.a), Some(fx.b), 10, End).expect("end");
    q.check_transition_media(t, lengths(Some(Time::seconds(3)), Some(Time::ZERO)))
        .expect("nothing past A's end");
    // No outgoing clip: skipped.
    let (q, t) = add(&p, None, Some(fx.a), 5, Start).expect("fade in");
    q.check_transition_media(t, lengths(Some(Time::ZERO), Some(Time::ZERO)))
        .expect("nothing goes out");
}

fn wipe() -> ParamSchema {
    ParamSchema::new(
        "wipe",
        "transition.name.wipe",
        vec![ParamDef::float(
            "feather",
            "transition.wipe.feather",
            0.1,
            Range::new(0.0, 1.0),
        )],
    )
}

/// An edit: its name, the roots before and after, and the ids the diff upserts and removes.
type DiffCase<'a> = (&'static str, &'a Project, Project, Vec<NodeId>, Vec<NodeId>);

fn ids(nodes: &[Node]) -> Vec<NodeId> {
    nodes.iter().map(Node::id).collect()
}

#[test]
fn every_transition_edit_diffs_and_rebuilds_both_ways() {
    let fx = fixture();
    // C's fade out is added before A–B, whose cut is earlier: the list stays in id order.
    let (p, c_out) = add(&fx.project, Some(fx.c), None, 5, Alignment::Start).expect("c out");
    let (p, ab) = add(&p, Some(fx.a), Some(fx.b), 10, Alignment::Centre).expect("ab");
    let listed: Vec<_> = p.sequence(fx.seq).expect("seq").tracks[0]
        .transitions
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(listed, vec![c_out, ab]);
    p.validate().expect("valid");
    // Track records leave their transitions out.
    for node in flatten(&p).values() {
        if let Node::Track { track, .. } = node {
            assert!(track.transitions.is_empty(), "track {}", track.id);
        }
    }

    let schema = wipe();
    let wiped = p.set_transition_kind(ab, "wipe").expect("kind");
    let (added, bc) = add(&p, Some(fx.b), Some(fx.c), 4, Alignment::Start).expect("bc");
    let edits: Vec<DiffCase> = vec![
        ("add", &p, added, vec![ROOT_ID, bc.node()], vec![]),
        (
            "remove",
            &p,
            p.remove_transition(ab).expect("remove"),
            vec![],
            vec![ab.node()],
        ),
        (
            "duration",
            &p,
            p.set_transition_duration(ab, f(12)).expect("duration"),
            vec![ab.node()],
            vec![],
        ),
        (
            "alignment",
            &p,
            p.set_transition_alignment(ab, Alignment::End)
                .expect("alignment"),
            vec![ab.node()],
            vec![],
        ),
        ("kind", &p, wiped.clone(), vec![ab.node()], vec![]),
        (
            "direction",
            &p,
            p.set_transition_direction(ab, Some("left"))
                .expect("direction"),
            vec![ab.node()],
            vec![],
        ),
        (
            "easing",
            &p,
            p.set_transition_easing(ab, "ease-out").expect("easing"),
            vec![ab.node()],
            vec![],
        ),
        (
            "param",
            &wiped,
            wiped
                .set_transition_param(ab, &schema, "feather", ParamValue::Float(0.5))
                .expect("param"),
            vec![ab.node()],
            vec![],
        ),
        (
            "a trim that shortens A–B",
            &p,
            p.trim_item(fx.b, at(20, 4)).expect("trim"),
            vec![fx.b.node(), ab.node()],
            vec![],
        ),
        (
            "deleting C takes its fade",
            &p,
            p.remove_item(fx.c).expect("remove"),
            vec![],
            vec![fx.c.node(), c_out.node()],
        ),
    ];
    for (name, before, after, upserts, removals) in edits {
        let (old, new) = (flatten(before), flatten(&after));
        let forward = Diff::between(&old, &new);
        assert_eq!(ids(&forward.upserts), upserts, "{name}");
        assert_eq!(forward.removals, removals, "{name}");
        let mut flat = old.clone();
        forward.apply(&mut flat);
        assert!(rebuild(&flat).expect(name).content_eq(&after), "{name}");
        let back = Diff::between(&new, &old);
        let mut flat = new.clone();
        back.apply(&mut flat);
        assert!(
            rebuild(&flat).expect(name).content_eq(before),
            "{name} back"
        );
    }
}

#[test]
fn a_same_kind_keeps_its_parameters_and_no_ops_share_the_track() {
    let fx = fixture();
    let schema = wipe();
    let (p, t) = add(&fx.project, Some(fx.a), Some(fx.b), 10, Alignment::Centre).expect("ab");
    let p = p
        .set_transition_kind(t, "wipe")
        .expect("kind")
        .set_transition_param(t, &schema, "feather", ParamValue::Float(0.5))
        .expect("param");
    let track = |p: &Project| p.sequence(fx.seq).expect("seq").tracks[0].clone();
    for (name, same) in [
        ("same kind", p.set_transition_kind(t, "wipe")),
        (
            "same value",
            p.set_transition_param(t, &schema, "feather", ParamValue::Float(0.5)),
        ),
    ] {
        let same = same.expect(name);
        assert_eq!(
            same.find_transition(t)
                .expect("t")
                .2
                .params
                .float(&schema, "feather"),
            Some(0.5),
            "{name}"
        );
        assert!(Arc::ptr_eq(&track(&same), &track(&p)), "{name}");
    }
}

#[test]
fn the_generic_transition_edit_reads_change_transition() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/i18n/en.json");
    let en: Value = serde_json::from_slice(&fs::read(path).expect("en.json")).expect("JSON");
    assert_eq!(
        en["strings"][EditLabel::Transition.i18n_key()],
        "Change Transition"
    );
}

/// A–B centred on 10 (cut 20), B–C ending at the cut (cut 40), C fading out (cut 50),
/// added latest cut first so that list order (by id) is the reverse of cut order:
/// `[c_out, bc, ab]`.
fn reversed() -> (Project, Fix) {
    let fx = fixture();
    let (p, _) = add(&fx.project, Some(fx.c), None, 5, Alignment::Start).expect("c out");
    let (p, _) = add(&p, Some(fx.b), Some(fx.c), 4, Alignment::End).expect("bc");
    let (p, _) = add(&p, Some(fx.a), Some(fx.b), 10, Alignment::Centre).expect("ab");
    (p, fx)
}

fn load_with(p: &Project, edit: impl FnOnce(&mut Value)) -> Result<Project, LoadError> {
    let mut json: Value = serde_json::from_slice(&schema::save(p)).expect("JSON");
    edit(&mut json["project"]["sequences"][0]["tracks"][0]["transitions"]);
    schema::load(&serde_json::to_vec(&json).expect("JSON"))
}

/// A change to the transitions list of a saved file.
type JsonFix<'a> = &'a dyn Fn(&mut Value);

/// The error of a load refused by one track's checks, with that track.
fn track_error(result: Result<Project, LoadError>) -> (TrackId, EditError) {
    match result {
        Err(LoadError::Invalid(ValidationError::Track { track, error })) => (track, error),
        other => panic!("{other:?}"),
    }
}

#[test]
fn loading_refuses_transitions_in_the_contract_order() {
    let (p, fx) = reversed();
    let v1 = fx.v1;
    // Indices: 0 is C's fade out, 1 is B–C, 2 is A–B.
    assert_eq!(
        track_error(load_with(&p, |t| t[0]["outgoing"] = fx.a.0.into())),
        (
            v1,
            EditError::CutNotFree {
                item: fx.a,
                other: fx.b,
            }
        )
    );
    assert_eq!(
        track_error(load_with(&p, |t| {
            t[1]["alignment"] = "start".into();
            t[1]["duration"] = "11/10".into();
        })),
        (
            v1,
            EditError::TransitionRoom {
                side: CutSide::After,
                needed: f(11),
                available: f(10),
            }
        )
    );
    // Two misfits: the earlier cut is reported, though its id is later.
    assert_eq!(
        track_error(load_with(&p, |t| {
            t[0]["duration"] = "11/10".into();
            t[2]["alignment"] = "start".into();
            t[2]["duration"] = "21/10".into();
        })),
        (
            v1,
            EditError::TransitionRoom {
                side: CutSide::After,
                needed: f(21),
                available: f(20),
            }
        )
    );
    // A cut that does not resolve is reported before any misfit.
    assert_eq!(
        track_error(load_with(&p, |t| {
            t[0]["duration"] = "11/10".into();
            t[2]["incoming"] = 999.into();
        })),
        (v1, EditError::UnknownItem(ItemId(999)))
    );
    // Within one transition: kind, direction, easing, duration, then its cut.
    let faults = |t: &mut Value| {
        t[2]["kind"] = "Kind".into();
        t[2]["direction"] = "Up".into();
        t[2]["easing"] = "Ease".into();
        t[2]["duration"] = "0".into();
        t[2]["incoming"] = 999.into();
    };
    let steps: [(JsonFix, EditError); 6] = [
        (&|_| {}, EditError::InvalidTransitionKind("Kind".into())),
        (
            &|t| t[2]["kind"] = "wipe".into(),
            EditError::InvalidDirection("Up".into()),
        ),
        (
            &|t| t[2]["direction"] = "up".into(),
            EditError::InvalidEasing("Ease".into()),
        ),
        (
            &|t| t[2]["easing"] = "linear".into(),
            EditError::EmptyRange(Time::ZERO),
        ),
        (
            &|t| t[2]["duration"] = "-1/10".into(),
            EditError::EmptyRange(-f(1)),
        ),
        (
            &|t| t[2]["duration"] = "1/20".into(),
            EditError::OffGrid {
                time: secs(1, 20),
                grid: f(1),
                kind: TrackKind::Video,
            },
        ),
    ];
    let mut fixes: Vec<JsonFix> = Vec::new();
    for (fix, expected) in steps {
        fixes.push(fix);
        let got = track_error(load_with(&p, |t| {
            faults(t);
            for fix in &fixes {
                fix(t);
            }
        }));
        assert_eq!(got, (v1, expected));
    }
    assert_eq!(
        track_error(load_with(&p, |t| {
            faults(t);
            for fix in &fixes {
                fix(t);
            }
            t[2]["duration"] = "1".into();
        })),
        (v1, EditError::UnknownItem(ItemId(999)))
    );
    // Ids out of order are refused before a track's own checks.
    assert!(matches!(
        load_with(&p, |t| {
            t.as_array_mut().expect("list").swap(0, 1);
            t[0]["kind"] = "Kind".into();
        }),
        Err(LoadError::Invalid(ValidationError::Structure(_)))
    ));
}

#[test]
fn rebuilding_from_records_refuses_a_bad_transition_like_loading() {
    let (p, fx) = reversed();
    let ab = p.sequence(fx.seq).expect("seq").tracks[0].transitions[2].clone();
    let mut flat = flatten(&p);
    let mut bad = (*ab).clone();
    bad.kind = "Kind".into();
    flat.insert(
        ab.id.node(),
        Node::Transition {
            track: fx.v1,
            transition: Arc::new(bad),
        },
    );
    assert_eq!(
        rebuild(&flat).map(|_| ()),
        Err(ValidationError::Track {
            track: fx.v1,
            error: EditError::InvalidTransitionKind("Kind".into()),
        })
    );
}

fn fixture_file(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/projects")
        .join(name);
    serde_json::from_slice(&fs::read(&path).expect("fixture")).expect("JSON")
}

fn load_edited(name: &str, edit: impl FnOnce(&mut Value)) -> Result<Project, LoadError> {
    let mut json = fixture_file(name);
    edit(&mut json["project"]["sequences"][0]["tracks"]);
    schema::load(&serde_json::to_vec(&json).expect("JSON"))
}

fn migration_error(result: Result<Project, LoadError>) -> (u32, String) {
    match result {
        Err(LoadError::Migration { from, message }) => (from, message),
        other => panic!("{other:?}"),
    }
}

#[test]
fn old_files_with_malformed_tracks_fail_their_migration() {
    let loaded = load_edited("v4-basic.json", |_| {}).expect("an untouched v4 file loads");
    for track in &loaded.sequences()[0].tracks {
        assert!(track.transitions.is_empty());
    }
    assert_eq!(
        migration_error(load_edited("v4-basic.json", |t| {
            t[1]["transitions"] = Value::Array(Vec::new());
        })),
        (4, "a v4 track already has transitions".to_string())
    );
    assert_eq!(
        migration_error(load_edited("v4-basic.json", |t| t[1] = 5.into())),
        (4, "a track is not an object".to_string())
    );
    assert_eq!(
        migration_error(load_edited("v3-basic.json", |t| t[0] = "V1".into())),
        (3, "a track is not an object".to_string())
    );
    assert_eq!(
        migration_error(load_edited("v2-basic.json", |t| {
            t[0] = Value::Array(Vec::new());
        })),
        (2, "a track is not an object".to_string())
    );
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "glass-transitions-contract-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn transition_edits_recover_from_the_autosave_log() {
    let fx = fixture();
    let dir = temp_dir("recover");
    let p = fx.project.clone();
    let mut writer = AutosaveWriter::open(&dir, &p, None, 0, 0).expect("open");
    let (p, ab) = add(&p, Some(fx.a), Some(fx.b), 10, Alignment::Centre).expect("ab");
    let (p, c_out) = add(&p, Some(fx.c), None, 5, Alignment::Start).expect("c out");
    writer.tick(&p, 2, 10).expect("tick");
    // Shortens A–B to 8, then takes C's fade with C.
    let p = p.trim_item(fx.b, at(20, 4)).expect("trim");
    writer.tick(&p, 1, 20).expect("tick");
    let p = p.remove_item(fx.c).expect("remove");
    writer.tick(&p, 1, 30).expect("tick");
    drop(writer);
    let recovered = autosave::recover(&dir).expect("recovers");
    assert!(recovered.project.content_eq(&p));
    assert_eq!(span_of(&recovered.project, ab), Some(at(16, 8)));
    assert_eq!(span_of(&recovered.project, c_out), None);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn a_format_4_autosave_is_refused_by_its_version_even_with_a_current_body() {
    let fx = fixture();
    let dir = temp_dir("format-4");
    let (p, _) = add(&fx.project, Some(fx.a), Some(fx.b), 10, Alignment::Centre).expect("ab");
    let mut writer = AutosaveWriter::open(&dir, &p, None, 0, 0).expect("open");
    writer.tick(&p, 1, 10).expect("tick");
    drop(writer);
    assert!(
        autosave::recover(&dir)
            .expect("the current format recovers")
            .project
            .content_eq(&p)
    );
    let snapshot = dir.join("snapshot-00000001.json");
    let mut json: Value =
        serde_json::from_slice(&fs::read(&snapshot).expect("snapshot")).expect("JSON");
    assert_eq!(json["autosave_format"], autosave::FORMAT_VERSION);
    json["autosave_format"] = 4.into();
    fs::write(&snapshot, serde_json::to_vec(&json).expect("JSON")).expect("write");
    assert!(matches!(
        autosave::recover(&dir),
        Err(AutosaveError::Format(4))
    ));
    fs::remove_dir_all(&dir).expect("cleanup");
}
