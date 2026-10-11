//! Nested sequences in a timeline plan (Decisions 69 and 75): a clip of a sequence shows
//! that sequence's frames, its media decoded through segments flattened into the
//! parent's plan; transitions mix it like any clip, and nesting deeper than
//! `MAX_NESTING_DEPTH` leaves the deeper clips out, counted.

use engine::timeline::{MAX_NESTING_DEPTH, TimelinePlan, TrackFrame};
use project::{Alignment, ClipSpec, Project, Rational, Time, TimeRange, TrackKind, TransitionSpec};

const FPS: Rational = Rational::new(30, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

#[test]
fn nested_clips_show_their_sequences_frames() {
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
    assert_eq!(plan.nested_clips(), 0, "nothing left out");
    assert_eq!(plan.missing_media(), 0);
    assert_eq!(plan.nested_looks().len(), 1);
    let look = plan.nested(0);
    assert_eq!((look.item, look.sequence), (nested, inner));
    assert_eq!(look.plan().rate(), Rational::new(24, 1));
    // The media clip and the inner clip, carried by a parent segment.
    assert_eq!(plan.segments().segments().len(), 2);
    let carried = look.segment(0).expect("inner segment carried");
    assert_eq!(plan.segment(carried).timeline, 30..60);
    // The dissolve mixes the shot into the nested sequence.
    let dissolve = &plan.transitions()[0];
    assert!(dissolve.outgoing.is_some());
    assert_eq!(dissolve.incoming, None);
    assert_eq!(dissolve.incoming_nested, Some(0));
    assert_eq!(dissolve.span, 30..40);
    for t in 0..plan.end() {
        let desc = plan.frame(t);
        plan.span_hash(t);
        let segments = desc.segments(&plan);
        match t {
            0..30 => assert_eq!(desc.tracks, [TrackFrame::Clip(segments[0])], "frame {t}"),
            30..40 => {
                assert!(
                    matches!(desc.tracks[..], [TrackFrame::Transition { .. }]),
                    "frame {t}"
                );
                assert!(
                    segments.contains(&carried),
                    "frame {t}: reads the nested media"
                );
            }
            _ => {
                // 30 fps showing 24 fps: frame 30 + k is nested time k / 30 s, the nested
                // frame nearest 0.8 k.
                let k = t - 30;
                let expected = (k * 8 + 5).div_euclid(10);
                assert_eq!(
                    desc.tracks,
                    [TrackFrame::Nested {
                        nested: 0,
                        frame: expected
                    }],
                    "frame {t}"
                );
                assert_eq!(segments, [carried], "frame {t}");
            }
        }
    }
    // Frames inside the nested clip are keyed one by one.
    assert_ne!(plan.span_hash(45), plan.span_hash(46));
    assert_eq!(plan.end(), 60);
}

#[test]
fn nesting_deeper_than_the_limit_leaves_the_deeper_clips_out() {
    let p = Project::new("uid-deep", "Deep");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (mut p, mut inner) = p.add_sequence("Level 0", FPS, 48_000).expect("level 0");
    let (q, track) = p
        .add_track(inner, TrackKind::Video, "V1", None)
        .expect("track");
    let (q, _) = q
        .insert_clip(
            track,
            ClipSpec {
                media,
                range: TimeRange::new(Time::ZERO, frames(30)),
                source_start: Time::ZERO,
            },
        )
        .expect("clip");
    p = q;
    // Sequence k nests sequence k - 1; the outermost is `MAX_NESTING_DEPTH` levels above
    // the media.
    let mut outer = inner;
    for level in 1..=MAX_NESTING_DEPTH {
        let (q, seq) = p
            .add_sequence(format!("Level {level}"), FPS, 48_000)
            .expect("level");
        let (q, track) = q
            .add_track(seq, TrackKind::Video, "V1", None)
            .expect("track");
        let (q, _) = q
            .insert_sequence_clip(
                track,
                inner,
                TimeRange::new(Time::ZERO, frames(30)),
                Time::ZERO,
            )
            .expect("nest");
        p = q;
        inner = seq;
        outer = seq;
    }
    let plan = TimelinePlan::build(&p, outer).expect("plan");
    // The chain stops one level short of the media: no segment, one clip counted.
    let mut look = &plan;
    let mut depth = 0;
    while let Some(n) = look.nested_looks().first() {
        look = n.plan();
        depth += 1;
    }
    assert_eq!(depth, MAX_NESTING_DEPTH - 1);
    assert_eq!(look.nested_clips(), 1);
    assert!(plan.segments().segments().is_empty());
    for t in 0..30 {
        plan.frame(t).segments(&plan);
        plan.span_hash(t);
    }
}
