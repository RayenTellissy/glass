//! Undo history: gestures, coalescing, labels and sharing statistics.

use project::history::{EditLabel, History};
use project::{ClipSpec, Project, Rational, Time, TimeRange, TrackKind};

const FPS: Rational = Rational::new(30, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn setup(clips: i64) -> (Project, project::TrackId, Vec<project::ItemId>) {
    let p = Project::new("uid", "History");
    let (p, media) = p.add_media("aa", "/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (mut p, track) = p
        .add_track(seq, TrackKind::Video, "V1", None)
        .expect("track");
    let mut ids = Vec::new();
    for i in 0..clips {
        let spec = ClipSpec {
            media,
            range: TimeRange::new(frames(i * 20), frames(10)),
            source_start: Time::ZERO,
        };
        let (next, id) = p.insert_clip(track, spec).expect("clip");
        p = next;
        ids.push(id);
    }
    (p, track, ids)
}

fn start_of(p: &Project, id: project::ItemId) -> Time {
    p.find_item(id).expect("item").2.range.start
}

#[test]
fn undo_and_redo_move_between_roots() {
    let (p, track, ids) = setup(3);
    let mut h = History::new(p.clone());
    assert!(!h.can_undo());
    assert_eq!(h.undo(), None);

    let moved = p.move_item(ids[0], track, frames(5)).expect("move");
    h.commit(EditLabel::Move, moved.clone());
    let deleted = moved.remove_item(ids[1]).expect("delete");
    h.commit(EditLabel::Delete, deleted.clone());
    assert_eq!(h.undo_label(), Some(EditLabel::Delete));
    assert_eq!(
        h.undo_label().map(EditLabel::i18n_key),
        Some("edit.label.delete")
    );

    assert_eq!(h.undo(), Some(&moved));
    assert_eq!(h.redo_label(), Some(EditLabel::Delete));
    assert_eq!(h.undo_label(), Some(EditLabel::Move));
    assert_eq!(h.undo(), Some(&p));
    assert!(!h.can_undo());
    assert_eq!(h.redo(), Some(&moved));
    assert_eq!(h.redo(), Some(&deleted));
    assert_eq!(h.redo(), None);

    // A new edit after undo drops the redo steps.
    h.undo();
    let trimmed = moved
        .trim_item(ids[2], TimeRange::new(frames(40), frames(5)))
        .expect("trim");
    h.commit(EditLabel::Trim, trimmed.clone());
    assert!(!h.can_redo());
    assert_eq!(h.current(), &trimmed);
    assert_eq!(h.undo_depth(), 2);
}

#[test]
fn a_gesture_is_one_step() {
    let (p, track, ids) = setup(2);
    let mut h = History::new(p.clone());
    h.begin_gesture(EditLabel::Move);
    let mut last = p.clone();
    for f in 1..=9 {
        last = p.move_item(ids[0], track, frames(f)).expect("drag");
        h.update(last.clone());
        assert_eq!(h.current(), &last);
    }
    assert!(h.end_gesture());
    assert_eq!(h.undo_depth(), 1);
    assert_eq!(start_of(h.current(), ids[0]), frames(9));
    assert_eq!(h.undo(), Some(&p));
    assert_eq!(h.redo(), Some(&last));

    // Dragging back to where it started leaves no step.
    h.begin_gesture(EditLabel::Move);
    h.update(last.move_item(ids[0], track, frames(5)).expect("drag"));
    h.update(last.move_item(ids[0], track, frames(9)).expect("drag back"));
    assert!(!h.end_gesture());
    assert_eq!(h.undo_depth(), 1);
    assert_eq!(h.current(), &last);

    // Cancelling returns to the start.
    h.begin_gesture(EditLabel::Trim);
    h.update(
        last.trim_item(ids[0], TimeRange::new(frames(9), frames(3)))
            .expect("trim"),
    );
    h.cancel_gesture();
    assert_eq!(h.current(), &last);
    assert_eq!(h.undo_depth(), 1);
    assert!(!h.in_gesture());

    // Undo during a gesture ends it first.
    h.begin_gesture(EditLabel::Move);
    h.update(last.move_item(ids[0], track, frames(0)).expect("drag"));
    assert_eq!(h.undo(), Some(&last));
    assert!(!h.in_gesture());
    assert_eq!(h.redo_label(), Some(EditLabel::Move));
}

#[test]
fn nudges_coalesce_until_something_else_happens() {
    let (p, track, ids) = setup(2);
    let mut h = History::new(p.clone());
    let mut cur = p.clone();
    for f in 1..=5 {
        cur = cur.move_item(ids[1], track, frames(20 + f)).expect("nudge");
        h.commit_coalescing(EditLabel::Nudge, 7, cur.clone());
    }
    assert_eq!(h.undo_depth(), 1);
    // A different key starts a new step.
    cur = cur
        .move_item(ids[0], track, frames(1))
        .expect("nudge other");
    h.commit_coalescing(EditLabel::Nudge, 8, cur.clone());
    assert_eq!(h.undo_depth(), 2);
    // After an undo, the same key does not merge into the step before.
    h.undo();
    let again = h
        .current()
        .move_item(ids[0], track, frames(2))
        .expect("nudge");
    h.commit_coalescing(EditLabel::Nudge, 8, again);
    assert_eq!(h.undo_depth(), 2);
    h.undo();
    assert_eq!(start_of(h.current(), ids[1]), frames(25));
    assert_eq!(start_of(h.current(), ids[0]), frames(0));
    assert_eq!(h.undo(), Some(&p));
}

#[test]
fn no_op_edits_add_no_step() {
    let (p, track, ids) = setup(1);
    let mut h = History::new(p.clone());
    let same = p.move_item(ids[0], track, frames(0)).expect("no-op move");
    h.commit(EditLabel::Move, same);
    assert_eq!(h.undo_depth(), 0);
}

#[test]
fn memory_stats_count_shared_nodes_once() {
    let (p, track, ids) = setup(100);
    let mut h = History::new(p.clone());
    let mut cur = p;
    for step in 0..50 {
        let id = ids[step % ids.len()];
        let start = start_of(&cur, id);
        cur = cur
            .move_item(id, track, start.checked_add(frames(1)).expect("t"))
            .expect("move");
        h.commit(EditLabel::Move, cur.clone());
    }
    let stats = h.memory_stats();
    assert_eq!(stats.roots, 51);
    // 51 roots of 1 media + 1 sequence + 1 track + 100 items each would be 5253 nodes
    // unshared; each step adds one sequence, one track and one item.
    assert_eq!(stats.unique_nodes, 103 + 50 * 3);
    assert!(stats.node_refs > stats.unique_nodes);
}
