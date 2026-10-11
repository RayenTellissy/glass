//! 100,000 undo steps on a 1000-clip project stay small because the roots share every
//! node an edit did not touch. A counting global allocator measures the live heap, so
//! this test lives alone in its binary.

use std::sync::atomic::Ordering;

use project::history::{EditLabel, History};
use project::{ClipSpec, Project, Rational, Time, TimeRange, TrackKind};

/// The counting allocator: the only unsafe code of these tests, kept in an `ffi` module
/// as the workspace rule asks.
#[allow(unsafe_code)]
mod ffi {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicIsize, Ordering};

    pub struct Counting;

    /// Bytes currently allocated through this allocator.
    pub static LIVE: AtomicIsize = AtomicIsize::new(0);

    // SAFETY: forwards to the system allocator unchanged; only counts bytes.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
            // SAFETY: same contract as the caller's.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
            // SAFETY: same contract as the caller's.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;
}

const FPS: Rational = Rational::new(25, 1);
const STEPS: usize = 100_000;

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn live() -> isize {
    ffi::LIVE.load(Ordering::Relaxed)
}

#[test]
fn hundred_thousand_steps_share_memory() {
    let before_build = live();
    let p = Project::new("uid", "Memory");
    let (p, media) = p.add_media("aa", "/a.mov").expect("media");
    let (mut p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let mut tracks = Vec::new();
    for t in 0..4 {
        let (next, track) = p
            .add_track(seq, TrackKind::Video, format!("V{t}"), None)
            .expect("track");
        p = next;
        tracks.push(track);
    }
    let mut clips = Vec::new();
    for i in 0..1000i64 {
        let track = tracks[(i % 4) as usize];
        let base = (i / 4) * 20;
        let spec = ClipSpec {
            media,
            range: TimeRange::new(frames(base), frames(10)),
            source_start: Time::ZERO,
        };
        let (next, id) = p.insert_clip(track, spec).expect("clip");
        p = next;
        clips.push((id, track, base));
    }
    // A fresh deep copy of the tree for comparison: serialise and load it back.
    let json = serde_json::to_string(&p).expect("json");
    let before_copy = live();
    let copy: Project = serde_json::from_str(&json).expect("load");
    let full_tree = live() - before_copy;
    drop(copy);
    drop(json);

    let mut history = History::new(p.clone());
    let before_steps = live();
    let mut cur = p;
    for step in 0..STEPS {
        let (id, track, base) = clips[step % clips.len()];
        let offset = (step / clips.len()) as i64 % 9 + 1;
        cur = cur
            .move_item(id, track, frames(base + offset))
            .expect("move");
        history.commit(EditLabel::Move, cur.clone());
    }
    let per_step = (live() - before_steps) / STEPS as isize;
    let stats = history.memory_stats();
    println!(
        "tree built {} B, fresh copy {} B, per step {} B, {} steps: {} MB live; stats {:?}",
        before_steps - before_build,
        full_tree,
        per_step,
        STEPS,
        (live() - before_steps) / 1_000_000,
        stats
    );
    assert_eq!(history.undo_depth(), STEPS);
    assert_eq!(stats.roots, STEPS + 1);
    // Unshared, every step would hold a whole tree (about 92 KB here). Measured on M2:
    // about 3.9 KB per step, mostly the copied 64-slot chunks of the persistent vectors
    // on the path to the moved item, so 100,000 steps hold about 390 MB.
    assert!(
        per_step * 20 < full_tree,
        "per step {per_step} B vs tree {full_tree} B"
    );
    assert!(per_step < 8 * 1024, "per step {per_step} B");

    // Undo all the way back and redo all the way forward: only an index moves.
    let before_walk = live();
    while history.undo().is_some() {}
    assert_eq!(history.undo_depth(), 0);
    while history.redo().is_some() {}
    assert_eq!(history.current(), &cur);
    assert_eq!(live(), before_walk);
}
