//! The `glass-io` autosave thread: ticks only when the tree changed, never makes the UI
//! wait, and a crash (no clean close) leaves an autosave the next launch restores.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use engine::autosave::{self, Autosave, AutosaveConfig, AutosaveEvent};
use project::autosave as files;
use project::history::{EditLabel, History};
use project::{ClipSpec, MediaId, Project, Rational, Time, TimeRange, TrackId, TrackKind};

const FPS: Rational = Rational::new(25, 1);
const WAIT: Duration = Duration::from_secs(20);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn temp_cache(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "glass-engine-autosave-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn project(clips: usize) -> (Project, MediaId, TrackId) {
    let p = Project::new(format!("uid-{clips}"), "Harbour film");
    let (p, media) = p.add_media("abcd", "/footage/harbour.mov").expect("media");
    let (p, seq) = p.add_sequence("Main sequence", FPS, 48_000).expect("seq");
    let (mut p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    for c in 0..clips as i64 {
        p = p.insert_clip(v1, clip(media, c * 30, 24)).expect("clip").0;
    }
    (p, media, v1)
}

fn clip(media: MediaId, start: i64, len: i64) -> ClipSpec {
    ClipSpec {
        media,
        range: TimeRange::new(frames(start), frames(len)),
        source_start: Time::ZERO,
    }
}

fn config(cache: &std::path::Path, p: &Project, interval_ms: u64) -> AutosaveConfig {
    AutosaveConfig {
        interval: Duration::from_millis(interval_ms),
        ..AutosaveConfig::new(autosave::project_dir(cache, p))
    }
}

/// Waits for the next event other than `Started`.
fn next_event(events: &Receiver<AutosaveEvent>) -> AutosaveEvent {
    loop {
        match events.recv_timeout(WAIT).expect("autosave event") {
            AutosaveEvent::Started { .. } => {}
            event => return event,
        }
    }
}

#[test]
fn a_crash_leaves_the_last_autosave_to_restore() {
    let cache = temp_cache("crash");
    let (p, media, v1) = project(3);
    let mut history = History::new(p.clone());
    let (handle, events) =
        Autosave::start(config(&cache, &p, 30), p, history.changes()).expect("start");
    for i in 0..4 {
        let next = history
            .current()
            .insert_clip(v1, clip(media, 1000 + i * 30, 10))
            .expect("clip")
            .0;
        history.commit(EditLabel::Insert, next);
        handle.submit(history.current(), history.changes());
        assert!(matches!(next_event(&events), AutosaveEvent::Written { .. }));
    }
    // One more edit that only reaches the thread on an explicit flush.
    let next = history
        .current()
        .remove_item(history.current().sequences()[0].tracks[0].items[0].id);
    history.commit(EditLabel::Delete, next.expect("remove"));
    handle.submit(history.current(), history.changes());
    handle.flush();
    assert!(matches!(next_event(&events), AutosaveEvent::Written { .. }));
    handle.abandon(); // the crash: no clean close

    let found = files::pending(&cache).expect("pending");
    assert_eq!(found.len(), 1);
    let info = found[0].1.as_ref().expect("recoverable");
    assert_eq!(info.edits_since_save, 5);
    assert_eq!(info.project_name, "Harbour film");
    let recovered = files::recover(&info.dir).expect("recover");
    assert_eq!(&recovered.project, history.current());
    std::fs::remove_dir_all(&cache).expect("cleanup");
}

#[test]
fn ticks_only_when_the_tree_changed() {
    let cache = temp_cache("idle");
    let (p, media, v1) = project(2);
    let (handle, events) = Autosave::start(config(&cache, &p, 20), p.clone(), 0).expect("start");
    assert!(matches!(
        events.recv_timeout(WAIT).expect("event"),
        AutosaveEvent::Started { .. }
    ));
    // Idle, and a root equal to the recorded one: nothing is written.
    handle.submit(&p, 0);
    std::thread::sleep(Duration::from_millis(150));
    assert!(events.try_recv().is_err(), "no tick without changes");
    let (edited, _) = p.insert_clip(v1, clip(media, 500, 5)).expect("clip");
    // Many submits between two ticks become one record.
    for _ in 0..50 {
        handle.submit(&edited, 1);
    }
    assert!(matches!(
        next_event(&events),
        AutosaveEvent::Written {
            tick: files::Tick::Appended { upserts: 1, .. },
            ..
        }
    ));
    std::thread::sleep(Duration::from_millis(100));
    assert!(events.try_recv().is_err(), "one record for fifty submits");
    handle.close();
    assert!(!autosave::project_dir(&cache, &p).exists());
    assert!(files::pending(&cache).expect("pending").is_empty());
    let _ = std::fs::remove_dir_all(&cache);
}

#[test]
fn saving_resets_the_edit_count() {
    let cache = temp_cache("saved");
    let (p, media, v1) = project(2);
    let (handle, events) = Autosave::start(config(&cache, &p, 20), p.clone(), 0).expect("start");
    let (a, _) = p.insert_clip(v1, clip(media, 500, 5)).expect("clip");
    handle.submit(&a, 1);
    assert!(matches!(next_event(&events), AutosaveEvent::Written { .. }));
    handle.saved(&a, 1);
    assert_eq!(next_event(&events), AutosaveEvent::Saved);
    let (b, _) = a.insert_clip(v1, clip(media, 600, 5)).expect("clip");
    handle.submit(&b, 3);
    assert!(matches!(next_event(&events), AutosaveEvent::Written { .. }));
    handle.abandon();
    let info = files::find_recovery(&autosave::project_dir(&cache, &p))
        .expect("find")
        .expect("recoverable");
    assert_eq!(info.edits_since_save, 2);
    assert!(info.saved_at_ms.is_some());
    std::fs::remove_dir_all(&cache).expect("cleanup");
}

#[test]
fn submitting_never_waits_for_a_write() {
    let cache = temp_cache("busy");
    let (p, media, v1) = project(1000);
    // The first snapshot of 1000 clips is written on the thread while we submit.
    let (handle, events) = Autosave::start(config(&cache, &p, 1), p.clone(), 0).expect("start");
    let mut worst = Duration::ZERO;
    let mut root = p;
    for i in 0..200 {
        root = root
            .insert_clip(v1, clip(media, 40_000 + i * 30, 10))
            .expect("clip")
            .0;
        let started = Instant::now();
        handle.submit(&root, i as u64 + 1);
        worst = worst.max(started.elapsed());
    }
    let mut writes = 0;
    let mut slowest = Duration::ZERO;
    handle.flush();
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(200)) {
            Ok(AutosaveEvent::Written { took, .. }) => {
                writes += 1;
                slowest = slowest.max(took);
            }
            Ok(AutosaveEvent::Started { took }) => {
                eprintln!("first snapshot of 1000 clips: {took:?}")
            }
            Ok(other) => panic!("{other:?}"),
            Err(_) if writes > 0 => break,
            Err(_) => {}
        }
    }
    eprintln!("200 submits: slowest {worst:?}; {writes} writes on glass-io, slowest {slowest:?}");
    assert!(writes > 0);
    // A send on an unbounded channel: microseconds even while a write is in progress.
    assert!(worst < Duration::from_millis(2), "submit took {worst:?}");
    handle.abandon();
    assert_eq!(
        files::recover(&autosave::project_dir(&cache, &root))
            .expect("recover")
            .project,
        root
    );
    std::fs::remove_dir_all(&cache).expect("cleanup");
}

#[test]
fn a_folder_that_cannot_be_written_reports_failure() {
    let cache = temp_cache("fail");
    std::fs::create_dir_all(&cache).expect("cache");
    // A file where the autosave folder should be.
    let blocker = cache.join("not-a-folder");
    std::fs::write(&blocker, b"x").expect("file");
    let (p, _, _) = project(1);
    let config = AutosaveConfig::new(blocker.join("autosave"));
    let (_handle, events) = Autosave::start(config, p, 0).expect("start");
    assert!(matches!(
        events.recv_timeout(WAIT).expect("event"),
        AutosaveEvent::Failed(_)
    ));
    std::fs::remove_dir_all(&cache).expect("cleanup");
}
