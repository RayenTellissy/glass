//! Nested sequences in a timeline plan (Decision 69): not rendered yet, so their clips are
//! left out and counted, and a plan with them (and transitions on them) still builds.

use engine::timeline::TimelinePlan;
use project::{Alignment, ClipSpec, Project, Rational, Time, TimeRange, TrackKind, TransitionSpec};

const FPS: Rational = Rational::new(30, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

#[test]
fn nested_clips_are_left_out_and_counted() {
    let p = Project::new("uid-nested-plan", "Nested");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, inner) = p
        .add_sequence("Inner", Rational::new(24, 1), 48_000)
        .expect("inner");
    let (p, iv1) = p
        .add_track(inner, TrackKind::Video, "V1", None)
        .expect("iv1");
    let (p, _) = p
        .insert_clip(
            iv1,
            ClipSpec {
                media,
                range: TimeRange::new(Time::ZERO, Time::seconds(2)),
                source_start: Time::ZERO,
            },
        )
        .expect("inner clip");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("main");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, shot) = p
        .insert_clip(
            v1,
            ClipSpec {
                media,
                range: TimeRange::new(Time::ZERO, frames(30)),
                source_start: Time::seconds(1),
            },
        )
        .expect("shot");
    let (p, nested) = p
        .insert_sequence_clip(
            v1,
            inner,
            TimeRange::new(frames(30), frames(30)),
            Time::ZERO,
        )
        .expect("nested");
    let (p, _) = p
        .add_transition(TransitionSpec {
            alignment: Alignment::Start,
            ..TransitionSpec::new(Some(shot), Some(nested), frames(10))
        })
        .expect("dissolve");
    let plan = TimelinePlan::build(&p, seq).expect("plan");
    assert_eq!(plan.nested_clips(), 1);
    assert_eq!(plan.missing_media(), 0);
    assert_eq!(
        plan.segments().segments().len(),
        1,
        "only the media clip decodes"
    );
    // The dissolve has nothing to mix in from the nested side.
    let dissolve = &plan.transitions()[0];
    assert!(dissolve.outgoing.is_some());
    assert_eq!(dissolve.incoming, None);
    for t in 0..plan.end() {
        plan.frame(t);
        plan.span_hash(t);
    }
}
