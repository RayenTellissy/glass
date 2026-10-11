//! Track switches (schema v9) in the timeline: the plan composites only visible video
//! tracks, and dirty ranges follow what each switch changes on screen (visibility the
//! track's picture; mute, solo, lock and height nothing), while edits on hidden tracks and
//! media pool edits that keep a file's path and hash dirty nothing.

// Dirty frames are lists of ranges, often of one.
#![allow(clippy::single_range_in_vec_init)]

use std::ops::Range;

use engine::timeline::{DirtyFrames, DirtyRanges, TimelinePlan, TrackFrame};
use project::{
    ClipSpec, ColourLabel, ItemId, MediaId, Project, Rational, SequenceId, Time, TimeRange,
    TrackHeight, TrackId, TrackKind,
};

const FPS: Rational = Rational::new(30, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

struct F {
    p: Project,
    seq: SequenceId,
    media: MediaId,
    v2: TrackId,
    v1: TrackId,
    s1: TrackId,
    a1: TrackId,
    /// V1: 0..60.
    base: ItemId,
    /// V2: 20..40 and 70..80.
    top: ItemId,
    late: ItemId,
}

fn fixture() -> F {
    let p = Project::new("uid-switch-plan", "Switches");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, s1) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).expect("v2");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let clip = |start: i64, len: i64| ClipSpec {
        media,
        range: TimeRange::new(frames(start), frames(len)),
        source_start: Time::ZERO,
    };
    let (p, base) = p.insert_clip(v1, clip(0, 60)).expect("base");
    let (p, top) = p.insert_clip(v2, clip(20, 20)).expect("top");
    let (p, late) = p.insert_clip(v2, clip(70, 10)).expect("late");
    let (p, _) = p.insert_clip(a1, clip(0, 60)).expect("sound");
    let (p, _) = p
        .insert_subtitle(s1, TimeRange::new(frames(5), frames(10)), "Hello")
        .expect("cue");
    F {
        p,
        seq,
        media,
        v2,
        v1,
        s1,
        a1,
        base,
        top,
        late,
    }
}

fn dirty(old: &Project, new: &Project, seq: SequenceId) -> DirtyFrames {
    DirtyRanges::between(old, new).sequence(seq)
}

fn ranges(list: &[Range<i64>]) -> DirtyFrames {
    DirtyFrames::from_ranges(list.iter().cloned())
}

/// The items each frame composites, bottom track first.
fn layers(plan: &TimelinePlan, frame: i64) -> Vec<ItemId> {
    plan.frame(frame)
        .tracks
        .iter()
        .map(|t| match *t {
            TrackFrame::Clip(segment) => plan.look(segment).item,
            TrackFrame::Transition { .. } => panic!("no transitions here"),
        })
        .collect()
}

#[test]
fn the_plan_skips_hidden_video_tracks() {
    let f = fixture();
    let plan = TimelinePlan::build(&f.p, f.seq).expect("plan");
    assert_eq!(layers(&plan, 25), [f.base, f.top]);
    assert_eq!(layers(&plan, 75), [f.late]);
    assert_eq!(plan.segments().segments().len(), 3);

    let hidden = f.p.set_track_visible(f.v2, false).expect("hide");
    let plan = TimelinePlan::build(&hidden, f.seq).expect("plan");
    assert_eq!(layers(&plan, 25), [f.base]);
    assert!(layers(&plan, 75).is_empty());
    assert_eq!(plan.segments().segments().len(), 1, "no decoding for V2");
    assert_eq!(plan.end(), 60);

    let both = hidden.set_track_visible(f.v1, false).expect("hide v1");
    let plan = TimelinePlan::build(&both, f.seq).expect("plan");
    assert!(plan.segments().segments().is_empty());
    assert!(layers(&plan, 25).is_empty());

    // Mute, solo, lock and height leave the picture as it was.
    let p = f.p.set_track_locked(f.v2, true).expect("lock");
    let p = p
        .set_track_height(f.v1, TrackHeight::Large)
        .expect("height");
    let p = p.set_track_muted(f.a1, true).expect("mute");
    let p = p.set_track_solo(f.a1, true).expect("solo");
    let same = TimelinePlan::build(&p, f.seq).expect("plan");
    let before = TimelinePlan::build(&f.p, f.seq).expect("plan");
    assert_eq!(same.spans(), before.spans());
}

#[test]
fn hiding_or_showing_a_track_dirties_its_picture_only() {
    let f = fixture();
    let hidden = f.p.set_track_visible(f.v2, false).expect("hide");
    assert_eq!(dirty(&f.p, &hidden, f.seq), ranges(&[20..40, 70..80]));
    assert_eq!(dirty(&hidden, &f.p, f.seq), ranges(&[20..40, 70..80]));
    let no_titles = f.p.set_track_visible(f.s1, false).expect("hide s1");
    assert_eq!(dirty(&f.p, &no_titles, f.seq), ranges(&[5..15]));
}

#[test]
fn mute_solo_lock_and_height_dirty_nothing() {
    let f = fixture();
    let edits = [
        f.p.set_track_muted(f.a1, true).expect("mute"),
        f.p.set_track_solo(f.a1, true).expect("solo"),
        f.p.set_track_locked(f.v1, true).expect("lock"),
        f.p.set_track_locked(f.a1, true).expect("lock audio"),
        f.p.set_track_height(f.v2, TrackHeight::Small)
            .expect("height"),
        f.p.rename_track(f.v1, "Picture").expect("rename"),
    ];
    for (i, edited) in edits.iter().enumerate() {
        assert!(DirtyRanges::between(&f.p, edited).is_empty(), "edit {i}");
        assert!(DirtyRanges::between(edited, &f.p).is_empty(), "undo {i}");
    }
}

#[test]
fn edits_on_a_hidden_track_dirty_nothing() {
    let f = fixture();
    let hidden = f.p.set_track_visible(f.v2, false).expect("hide");
    let moved = hidden
        .move_item(f.top, f.v2, frames(45))
        .expect("move on the hidden track");
    assert!(dirty(&hidden, &moved, f.seq).is_empty());
    let faded = hidden.set_clip_opacity(f.late, 0.5).expect("opacity");
    assert!(dirty(&hidden, &faded, f.seq).is_empty());
    // Moving a clip from the hidden track onto a visible one shows it there.
    let (with_v3, v3) = hidden
        .add_track(f.seq, TrackKind::Video, "V3", Some(0))
        .expect("v3");
    let shown = with_v3.move_item(f.top, v3, frames(90)).expect("move");
    assert_eq!(dirty(&with_v3, &shown, f.seq), ranges(&[90..110]));
}

#[test]
fn pool_edits_dirty_nothing_but_a_new_file_dirties_its_clips() {
    let f = fixture();
    let renamed = f.p.rename_media(f.media, "Harbour").expect("rename");
    let labelled = renamed
        .set_media_label(&[f.media], Some(ColourLabel::Amber))
        .expect("label");
    let tagged = labelled.add_media_tag(&[f.media], "Wide").expect("tag");
    let (with_bin, bin) = tagged
        .add_bin(tagged.media().root(), "Selects")
        .expect("bin");
    let moved = with_bin.move_media(&[f.media], bin).expect("move");
    assert!(DirtyRanges::between(&f.p, &moved).is_empty());
    let relinked = moved
        .relink_media(&[project::Relink {
            media: f.media,
            path: "/moved/a.mov".into(),
            content_hash: "00ff".into(),
            duration: None,
        }])
        .expect("relink");
    // Every picture clip of the media: V1 and V2 (and the subtitle cue is not a clip).
    assert_eq!(dirty(&moved, &relinked, f.seq), ranges(&[0..60, 70..80]));
}
