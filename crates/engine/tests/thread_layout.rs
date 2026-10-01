//! The thread layout: every long-lived thread is named and listed in the registry, the
//! registry's busy and idle accounting follows what the threads do, the playback
//! skeleton delivers frames in order across seeks, and shutdown joins every thread.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::analysis::{AnalysisPool, PoolConfig};
use engine::autosave::{Autosave, AutosaveConfig};
use engine::frame_queue::{FrameStamp, PopError};
use engine::playback::{Decode, PassThrough, Playback, PlaybackConfig, Presented};
use engine::threads::{self, Role, ThreadInfo, ThreadRegistry, ThreadState};
use media::CacheDir;

const WAIT: Duration = Duration::from_secs(10);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "glass-engine-threads-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(registry: &Arc<ThreadRegistry>, workers: usize) -> PlaybackConfig {
    PlaybackConfig {
        decode_workers: workers,
        registry: registry.clone(),
        ..PlaybackConfig::default()
    }
}

fn pass_through(
    registry: &Arc<ThreadRegistry>,
    workers: usize,
) -> (Playback, Presented<FrameStamp>) {
    Playback::start(config(registry, workers), |_| PassThrough, PassThrough).expect("playback")
}

fn find<'a>(threads: &'a [ThreadInfo], name: &str) -> &'a ThreadInfo {
    threads
        .iter()
        .find(|t| &*t.name == name)
        .unwrap_or_else(|| panic!("no thread named {name} in {threads:?}"))
}

/// Waits until `done` holds for a snapshot of `registry`.
fn wait_until(registry: &ThreadRegistry, mut done: impl FnMut(&[ThreadInfo]) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        if done(&registry.snapshot()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out: {registry:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn every_thread_of_the_layout_is_named_and_listed() {
    let registry = threads::global();
    let ui = registry.register_current(Role::Ui, None);
    let (playback, _presented) = Playback::start(
        PlaybackConfig {
            decode_workers: 3,
            ..PlaybackConfig::default()
        },
        |_| PassThrough,
        PassThrough,
    )
    .expect("playback");
    let audio = registry.register_audio_callback();
    let dir = TempDir::new("layout");
    let (pool, _events) = AnalysisPool::new(PoolConfig {
        workers: 2,
        cache: CacheDir::new(&dir.0),
    })
    .expect("analysis pool");
    let (autosave, _autosave_events) = Autosave::start(
        AutosaveConfig::new(dir.0.join("autosave")),
        project::Project::new("uid-threads", "Threads"),
        0,
    )
    .expect("autosave");

    let listed = registry.snapshot();
    let expected = [
        ("main", Role::Ui),
        ("glass-sched", Role::Scheduler),
        ("glass-decode-0", Role::Decode),
        ("glass-decode-1", Role::Decode),
        ("glass-decode-2", Role::Decode),
        ("glass-render", Role::Render),
        ("glass-audio", Role::AudioCallback),
        ("glass-analysis-0", Role::Analysis),
        ("glass-analysis-1", Role::Analysis),
        ("glass-io", Role::Io),
    ];
    for (name, role) in expected {
        assert_eq!(find(&listed, name).role, role, "{name}");
    }
    // Every spawned thread records its OS thread id once it runs; the ids differ.
    wait_until(registry, |threads| {
        expected
            .iter()
            .filter(|(name, _)| *name != "glass-audio")
            .all(|(name, _)| find(threads, name).os_id.is_some())
    });
    let listed = registry.snapshot();
    let os_ids: HashSet<u64> = expected
        .iter()
        .filter_map(|(name, _)| find(&listed, name).os_id)
        .collect();
    assert_eq!(os_ids.len(), expected.len() - 1);
    // The callback thread is known once the callback has run.
    assert_eq!(find(&listed, "glass-audio").state, ThreadState::Starting);
    std::thread::spawn({
        let audio = audio.clone();
        move || audio.run(|| ())
    })
    .join()
    .expect("callback");
    let listed = registry.snapshot();
    let callback = find(&listed, "glass-audio");
    assert!(callback.os_id.is_some());
    assert_eq!(callback.state, ThreadState::Idle);

    let ids: Vec<u64> = listed
        .iter()
        .filter(|t| expected.iter().any(|(name, _)| *name == &*t.name))
        .map(|t| t.id)
        .collect();
    playback.shutdown();
    drop(pool);
    autosave.close();
    drop(audio);
    drop(ui);
    let left: Vec<_> = registry
        .snapshot()
        .into_iter()
        .filter(|t| ids.contains(&t.id))
        .collect();
    assert!(left.is_empty(), "still registered: {left:?}");
}

#[test]
fn accounting_splits_busy_idle_and_blocked_time() {
    let registry = ThreadRegistry::new();
    let (go, start) = std::sync::mpsc::channel::<()>();
    let handle = registry
        .spawn(Role::Export, None, move || {
            threads::idle(|| start.recv().ok());
            let spin = Instant::now();
            while spin.elapsed() < Duration::from_millis(60) {
                std::hint::spin_loop();
            }
            threads::blocked(|| std::thread::sleep(Duration::from_millis(40)));
            threads::idle(|| std::thread::sleep(Duration::from_secs(60)));
        })
        .expect("spawn");
    std::thread::sleep(Duration::from_millis(50));
    let before = registry.snapshot()[0].clone();
    assert_eq!(before.state, ThreadState::Idle);
    assert!(before.idle >= Duration::from_millis(50));
    assert!(before.busy < Duration::from_millis(50));
    go.send(()).expect("go");
    wait_until(&registry, |t| {
        t[0].state == ThreadState::Idle && t[0].blocked > Duration::ZERO
    });
    let after = registry.snapshot()[0].clone();
    assert!(after.busy >= Duration::from_millis(60), "{after:?}");
    assert!(after.blocked >= Duration::from_millis(40), "{after:?}");
    assert_eq!(after.wakeups, 3);
    assert!(after.last_active.is_some());
    let fraction = after.busy_fraction_since(&before);
    assert!(fraction > 0.05 && fraction < 0.95, "{fraction}");
    // Waiting time keeps counting while idle, busy time does not.
    std::thread::sleep(Duration::from_millis(30));
    let later = registry.snapshot()[0].clone();
    assert_eq!(later.busy, after.busy);
    assert!(later.idle >= after.idle + Duration::from_millis(30));
    drop(handle);
}

/// Spins for a fixed time per frame, like a software decoder.
struct Spin(Duration);

impl Decode for Spin {
    type Frame = FrameStamp;

    fn decode(&mut self, stamp: &FrameStamp) -> FrameStamp {
        let start = Instant::now();
        while start.elapsed() < self.0 {
            std::hint::spin_loop();
        }
        *stamp
    }
}

#[test]
fn decode_bound_playback_shows_busy_decoders_and_waiting_scheduler() {
    let registry = ThreadRegistry::new();
    let (playback, mut presented) = Playback::start(
        config(&registry, 2),
        |_| Spin(Duration::from_millis(2)),
        PassThrough,
    )
    .expect("playback");
    playback.play();
    let before = registry.snapshot();
    let mut last = -1;
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(400) {
        if let Ok(frame) = presented.pop_timeout(Duration::from_millis(50)) {
            assert_eq!(frame.stamp.index, last + 1);
            last = frame.stamp.index;
        }
    }
    let during = registry.snapshot();
    assert!(last > 20, "only {last} frames");
    let fraction = |name: &str| find(&during, name).busy_fraction_since(find(&before, name));
    for decoder in ["glass-decode-0", "glass-decode-1"] {
        assert!(
            fraction(decoder) > 0.5,
            "{decoder} busy {}",
            fraction(decoder)
        );
        assert!(find(&during, decoder).wakeups > 1);
    }
    assert!(
        fraction("glass-sched") < 0.5,
        "scheduler busy {}",
        fraction("glass-sched")
    );
    assert!(
        fraction("glass-render") < 0.5,
        "render busy {}",
        fraction("glass-render")
    );
    let scheduler = find(&during, "glass-sched");
    assert!(scheduler.blocked > scheduler.busy, "{scheduler:?}");

    // Paused, with the presenter still draining: everything goes idle.
    playback.pause();
    let drain = Instant::now();
    while drain.elapsed() < Duration::from_millis(200) {
        let _ = presented.pop_timeout(Duration::from_millis(20));
    }
    let paused = registry.snapshot();
    std::thread::sleep(Duration::from_millis(100));
    let idle = registry.snapshot();
    for thread in &idle {
        assert_eq!(thread.state, ThreadState::Idle, "{thread:?}");
        let earlier = find(&paused, &thread.name);
        assert!(thread.busy_fraction_since(earlier) < 0.2, "{thread:?}");
    }
    playback.shutdown();
    assert!(registry.is_empty(), "{registry:?}");
}

#[test]
fn frames_arrive_in_order_and_a_seek_starts_a_new_run() {
    let registry = ThreadRegistry::new();
    let (playback, mut presented) = pass_through(&registry, 3);
    playback.play();
    for index in 0..200 {
        let frame = presented.pop_timeout(WAIT).expect("frame");
        assert_eq!((frame.stamp.generation, frame.stamp.index), (0, index));
        assert_eq!(frame.frame, frame.stamp);
    }
    let generation = playback.seek(1_000);
    assert_eq!(playback.generation(), generation);
    // Frames of the old generation still queued are dropped as the presenter meets them.
    let first = presented.pop_timeout(WAIT).expect("frame after seek");
    assert_eq!(
        (first.stamp.generation, first.stamp.index),
        (generation, 1_000)
    );
    for index in 1_001..1_100 {
        let frame = presented.pop_timeout(WAIT).expect("frame");
        assert_eq!(
            (frame.stamp.generation, frame.stamp.index),
            (generation, index)
        );
    }
    // A seek while paused lands on the new position when play resumes.
    playback.pause();
    let generation = playback.seek(-24);
    playback.play();
    let first = loop {
        let frame = presented.pop_timeout(WAIT).expect("frame");
        if frame.stamp.generation == generation {
            break frame;
        }
    };
    assert_eq!(first.stamp.index, -24);
    assert_eq!(presented.pop_timeout(WAIT).expect("frame").stamp.index, -23);
    playback.shutdown();
    assert!(registry.is_empty());
}

#[test]
fn shutdown_joins_every_thread_with_full_queues() {
    let registry = ThreadRegistry::new();
    let (playback, presented) = pass_through(&registry, 4);
    assert_eq!(registry.len(), 6);
    playback.play();
    // Nobody presents, so the queues fill and the scheduler and decoders end up waiting.
    wait_until(&registry, |threads| {
        let blocked = |role| {
            threads
                .iter()
                .any(|t| t.role == role && t.state == ThreadState::Blocked)
        };
        blocked(Role::Scheduler) && blocked(Role::Decode)
    });
    let (done, joined) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        playback.shutdown();
        let _ = done.send(());
    });
    joined.recv_timeout(WAIT).expect("shutdown joined");
    assert!(registry.is_empty(), "{registry:?}");
    drop(presented);
}

#[test]
fn a_dropped_presenter_stops_the_pipeline() {
    let registry = ThreadRegistry::new();
    let (playback, mut presented) = pass_through(&registry, 2);
    playback.play();
    presented.pop_timeout(WAIT).expect("frame");
    drop(presented);
    wait_until(&registry, <[ThreadInfo]>::is_empty);
    drop(playback);
}

#[test]
fn start_and_stop_repeatedly_leaks_no_threads() {
    let registry = ThreadRegistry::new();
    for round in 0..20 {
        let (playback, mut presented) = pass_through(&registry, 1 + round % 4);
        if round % 2 == 0 {
            playback.play();
            let _ = presented.pop_timeout(WAIT);
        }
        if round % 3 == 0 {
            playback.seek(round as i64);
        }
        drop(playback);
        assert!(registry.is_empty(), "round {round}: {registry:?}");
        // Whatever was still queued is stale; then the queue reports its producer gone.
        let end = loop {
            if let Err(error) = presented.try_pop() {
                break error;
            }
        };
        assert_eq!(end, PopError::Disconnected);
    }
}
