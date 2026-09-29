//! The edit tree: pure edits, validation, and structural sharing between roots.

use std::sync::Arc;

use project::{ClipSpec, EditError, Project, Rational, Time, TimeRange, TrackKind};

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn range(start: i64, len: i64) -> TimeRange {
    TimeRange::new(frames(start), frames(len))
}

struct Fixture {
    project: Project,
    media: project::MediaId,
    seq: project::SequenceId,
    v1: project::TrackId,
    v2: project::TrackId,
    a1: project::TrackId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-test", "Test");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).expect("v2");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    Fixture {
        project: p,
        media,
        seq,
        v1,
        v2,
        a1,
    }
}

fn clip(media: project::MediaId, start: i64, len: i64) -> ClipSpec {
    ClipSpec {
        media,
        range: range(start, len),
        source_start: Time::ZERO,
    }
}

#[test]
fn edits_return_new_roots_and_leave_the_old_one_alone() {
    let f = fixture();
    let before = f.project.clone();
    let (after, id) = f
        .project
        .insert_clip(f.v1, clip(f.media, 0, 50))
        .expect("insert");
    assert_eq!(f.project, before);
    assert!(before.find_item(id).is_none());
    let (_, track, item) = after.find_item(id).expect("placed");
    assert_eq!(track.id, f.v1);
    assert_eq!(item.range, range(0, 50));
    assert!(after.next_id() > before.next_id());
}

#[test]
fn untouched_subtrees_are_shared() {
    let f = fixture();
    let mut p = f.project.clone();
    let mut ids = Vec::new();
    for i in 0..200 {
        let (next, id) = p.insert_clip(f.v1, clip(f.media, i * 10, 10)).expect("v1");
        let (next, _) = next
            .insert_clip(f.v2, clip(f.media, i * 10, 10))
            .expect("v2");
        p = next;
        ids.push(id);
    }
    let (p, other_seq) = p.add_sequence("Other", FPS, 48_000).expect("seq");
    let (p, _) = p
        .add_track(other_seq, TrackKind::Video, "V1", None)
        .expect("track");

    let target = ids[100];
    let edited = p
        .move_item(target, f.v1, frames(100 * 10))
        .expect("same place");
    let edited = edited.trim_item(target, range(1000, 5)).expect("trim tail");

    let seq_before = p.sequence(f.seq).expect("seq");
    let seq_after = edited.sequence(f.seq).expect("seq");
    assert!(
        !Arc::ptr_eq(seq_before, seq_after),
        "the edited path is copied"
    );
    assert!(p.media().ptr_eq(edited.media()), "media map shared");
    assert!(Arc::ptr_eq(
        p.sequence(other_seq).expect("other"),
        edited.sequence(other_seq).expect("other")
    ));
    assert!(Arc::ptr_eq(
        seq_before.track(f.v2).expect("v2"),
        seq_after.track(f.v2).expect("v2")
    ));
    assert!(Arc::ptr_eq(
        seq_before.track(f.a1).expect("a1"),
        seq_after.track(f.a1).expect("a1")
    ));
    let (v1_before, v1_after) = (
        seq_before.track(f.v1).expect("v1"),
        seq_after.track(f.v1).expect("v1"),
    );
    assert!(!Arc::ptr_eq(v1_before, v1_after));
    let mut shared = 0;
    for (a, b) in v1_before.items.iter().zip(v1_after.items.iter()) {
        if a.id == target {
            assert!(!Arc::ptr_eq(a, b));
            assert_eq!(b.range, range(1000, 5));
        } else {
            assert!(Arc::ptr_eq(a, b), "item {} copied", a.id);
            shared += 1;
        }
    }
    assert_eq!(shared, 199);
}

#[test]
fn overlaps_are_rejected() {
    let f = fixture();
    let (p, first) = f
        .project
        .insert_clip(f.v1, clip(f.media, 10, 10))
        .expect("insert");
    for (start, len) in [(5, 10), (15, 10), (10, 10), (12, 2), (0, 40)] {
        assert_eq!(
            p.insert_clip(f.v1, clip(f.media, start, len)),
            Err(EditError::Overlap {
                track: f.v1,
                other: first
            }),
            "{start}+{len}"
        );
    }
    let (p, _) = p
        .insert_clip(f.v1, clip(f.media, 0, 10))
        .expect("touching before");
    let (p, second) = p
        .insert_clip(f.v1, clip(f.media, 20, 10))
        .expect("touching after");
    assert!(matches!(
        p.move_item(second, f.v1, frames(15)),
        Err(EditError::Overlap { .. })
    ));
    assert!(matches!(
        p.trim_item(first, range(10, 11)),
        Err(EditError::Overlap { .. })
    ));
    let order: Vec<_> = p
        .find_track(f.v1)
        .expect("v1")
        .1
        .items
        .iter()
        .map(|i| i.range.start)
        .collect();
    assert_eq!(order, vec![frames(0), frames(10), frames(20)]);
}

#[test]
fn grids_are_enforced() {
    let f = fixture();
    let half_frame = Time::new(1, 50).expect("t");
    let off = ClipSpec {
        media: f.media,
        range: TimeRange::new(half_frame, frames(10)),
        source_start: Time::ZERO,
    };
    assert!(matches!(
        f.project.insert_clip(f.v1, off.clone()),
        Err(EditError::OffGrid { .. })
    ));
    // 1/50 s is 960 samples at 48 kHz, so it is fine on an audio track.
    let (p, a) = f.project.insert_clip(f.a1, off).expect("audio");
    let one_sample = Time::from_samples(1, 48_000).expect("t");
    let p = p.move_item(a, f.a1, one_sample).expect("sample-exact");
    assert_eq!(p.find_item(a).expect("a").2.range.start, one_sample);
    assert_eq!(
        p.move_item(a, f.v1, frames(0)),
        Err(EditError::TrackKindMismatch {
            from: TrackKind::Audio,
            to: TrackKind::Video
        })
    );
    assert!(matches!(
        f.project.insert_clip(f.v1, clip(f.media, 0, 0)),
        Err(EditError::EmptyRange(_))
    ));
    assert!(matches!(
        f.project.insert_clip(f.v1, clip(f.media, -1, 5)),
        Err(EditError::NegativeStart(_))
    ));
    assert!(matches!(
        f.project.add_sequence("x", Rational::new(0, 1), 48_000),
        Err(EditError::InvalidFrameRate { .. })
    ));
}

#[test]
fn head_trim_moves_the_source_start() {
    let f = fixture();
    let spec = ClipSpec {
        media: f.media,
        range: range(10, 20),
        source_start: frames(100),
    };
    let (p, id) = f.project.insert_clip(f.v1, spec).expect("insert");
    let p = p.trim_item(id, range(15, 15)).expect("trim head");
    let item = p.find_item(id).expect("item").2.clone();
    assert_eq!(item.clip().expect("clip").source_start, frames(105));
    let p = p.trim_item(id, range(0, 30)).expect("extend head");
    assert_eq!(
        p.find_item(id)
            .expect("item")
            .2
            .clip()
            .expect("clip")
            .source_start,
        frames(90)
    );
    let spec = ClipSpec {
        media: f.media,
        range: range(100, 10),
        source_start: frames(2),
    };
    let (p, other) = p.insert_clip(f.v1, spec).expect("insert");
    assert_eq!(
        p.trim_item(other, range(97, 13)),
        Err(EditError::SourceBeforeStart(frames(-1)))
    );
}

#[test]
fn unknown_ids_are_typed_errors() {
    let f = fixture();
    let missing = project::ItemId(9999);
    assert_eq!(
        f.project.remove_item(missing),
        Err(EditError::UnknownItem(missing))
    );
    assert_eq!(
        f.project
            .insert_clip(project::TrackId(9999), clip(f.media, 0, 1)),
        Err(EditError::UnknownTrack(project::TrackId(9999)))
    );
    assert_eq!(
        f.project
            .insert_clip(f.v1, clip(project::MediaId(9999), 0, 1)),
        Err(EditError::UnknownMedia(project::MediaId(9999)))
    );
    let p = f.project.remove_track(f.v2).expect("remove");
    assert!(p.find_track(f.v2).is_none());
    assert!(f.project.find_track(f.v2).is_some());
}
