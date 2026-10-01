//! Relinking as an edit: only the references change, every clip and its edits stay the
//! same nodes, a too-short file is refused, and a batch is one undo step.

use std::sync::Arc;

use project::history::{EditLabel, History};
use project::{
    ClipSpec, EditError, MediaId, Project, Rational, Relink, Time, TimeRange, TrackKind,
};

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

/// Two offline sources with trimmed clips: `a` used up to 3.0 s (two clips), `b` up to
/// 1.6 s.
fn project() -> (Project, MediaId, MediaId) {
    let p = Project::new("uid-relink", "Harbour film");
    let (p, a) = p
        .add_media("aa11", "/Volumes/Card A/A014_C011.mov")
        .expect("a");
    let (p, b) = p
        .add_media("bb22", "/Volumes/Card B/B002_C004.mp4")
        .expect("b");
    let (p, seq) = p.add_sequence("Main sequence", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, first) = p
        .insert_clip(
            v1,
            ClipSpec {
                media: a,
                range: TimeRange::new(frames(0), frames(25)),
                source_start: frames(10),
            },
        )
        .expect("clip");
    let (p, _) = p
        .insert_clip(
            v1,
            ClipSpec {
                media: a,
                range: TimeRange::new(frames(50), frames(25)),
                source_start: frames(50),
            },
        )
        .expect("clip");
    let (p, _) = p
        .insert_clip(
            v1,
            ClipSpec {
                media: b,
                range: TimeRange::new(frames(100), frames(30)),
                source_start: frames(10),
            },
        )
        .expect("clip");
    // An edit made while offline: the first clip's head trimmed by 5 frames.
    let p = p
        .trim_item(first, TimeRange::new(frames(5), frames(20)))
        .expect("trim");
    (p, a, b)
}

fn relink(media: MediaId, path: &str, hash: &str, seconds: Option<f64>) -> Relink {
    Relink {
        media,
        path: path.into(),
        content_hash: hash.into(),
        duration: seconds.map(|s| Time::new((s * 1000.0).round() as i64, 1000).expect("t")),
    }
}

#[test]
fn used_source_range_is_the_furthest_clip_end() {
    let (p, a, b) = project();
    assert_eq!(p.source_used_until(a).expect("a"), Some(frames(75)));
    assert_eq!(p.source_used_until(b).expect("b"), Some(frames(40)));
    assert_eq!(p.clip_count(a), 2);
    assert_eq!(p.clip_count(b), 1);
    let (p, unused) = p.add_media("cc", "/x.mov").expect("c");
    assert_eq!(p.source_used_until(unused).expect("c"), None);
}

#[test]
fn a_batch_relink_changes_only_the_references() {
    let (p, a, b) = project();
    let next = p
        .relink_media(&[
            relink(
                a,
                "/Volumes/Studio SSD/Card A/A014_C011.mov",
                "aa11",
                Some(12.0),
            ),
            relink(b, "/Volumes/Studio SSD/Card B/gulls.mp4", "ff00", Some(7.0)),
        ])
        .expect("relink");
    assert_eq!(
        next.media().get(a).expect("a").path,
        "/Volumes/Studio SSD/Card A/A014_C011.mov"
    );
    let rb = next.media().get(b).expect("b");
    assert_eq!(rb.path, "/Volumes/Studio SSD/Card B/gulls.mp4");
    assert_eq!(rb.content_hash, "ff00");
    // Every sequence is the same node: trims and placements are untouched.
    assert!(
        Arc::ptr_eq(&p.sequences()[0], &next.sequences()[0]),
        "sequence copied"
    );
    next.validate().expect("valid");

    // One undo step for the whole batch.
    let mut history = History::new(p.clone());
    history.commit(EditLabel::Relink, next.clone());
    assert_eq!(history.undo_label(), Some(EditLabel::Relink));
    assert_eq!(EditLabel::Relink.i18n_key(), "edit.label.relink");
    history.undo();
    assert!(history.current().content_eq(&p));
    history.redo();
    assert!(history.current().content_eq(&next));
}

#[test]
fn a_file_shorter_than_the_used_range_is_refused() {
    let (p, a, b) = project();
    // `a` is used up to 3.0 s.
    let err = p
        .relink_media(&[
            relink(b, "/new/b.mp4", "bb22", Some(7.0)),
            relink(a, "/new/a.mov", "aa11", Some(2.5)),
        ])
        .expect_err("too short");
    assert_eq!(
        err,
        EditError::RelinkTooShort {
            media: a,
            needed: frames(75),
            found: Time::new(5, 2).expect("t"),
        }
    );
    // Exactly long enough, and a millisecond of container rounding, both pass; a still
    // (no duration) always does.
    p.relink_media(&[relink(a, "/new/a.mov", "aa11", Some(3.0))])
        .expect("exact");
    p.relink_media(&[relink(a, "/new/a.mov", "aa11", Some(2.999))])
        .expect("rounded");
    p.relink_media(&[relink(a, "/new/a.mov", "aa11", None)])
        .expect("no duration");
}

#[test]
fn bad_batches_are_refused_whole() {
    let (p, a, _) = project();
    assert_eq!(
        p.relink_media(&[relink(MediaId(999), "/x", "", None)]),
        Err(EditError::UnknownMedia(MediaId(999)))
    );
    assert_eq!(
        p.relink_media(&[relink(a, "/x.mov", "", None), relink(a, "/y.mov", "", None)]),
        Err(EditError::RelinkTwice(a))
    );
    assert_eq!(
        p.relink_media(&[relink(a, "", "", None)]),
        Err(EditError::RelinkNoPath(a))
    );
}
