//! The analysis pool: priority order, re-prioritisation, cancellation, dedup, the disk
//! cache (hits and version bumps), progress event order, pause and resume, and a
//! concurrent submit stress test. Jobs here are synthetic; they record what ran.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use engine::analysis::{
    AnalysisJob, AnalysisPool, Artifact, JobContext, JobError, JobEvent, JobEventKind, Outcome,
    PoolConfig, Priority,
};
use media::{CacheDir, CacheEntry, ContentHash};

const LIMIT: Duration = Duration::from_secs(20);

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("glass-analysis-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn cache(&self) -> CacheDir {
        CacheDir::new(&self.0)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn pool(dir: &TempDir, workers: usize) -> (AnalysisPool, Receiver<JobEvent>) {
    AnalysisPool::new(PoolConfig {
        workers,
        cache: dir.cache(),
    })
    .expect("pool")
}

#[derive(Debug, PartialEq, Eq)]
struct Number(u64);

impl Artifact for Number {
    const EXTENSION: &'static str = "txt";

    fn encode(&self) -> std::io::Result<Vec<u8>> {
        Ok(self.0.to_string().into_bytes())
    }

    fn decode(bytes: &[u8], _: &CacheEntry) -> Option<Self> {
        std::str::from_utf8(bytes).ok()?.parse().ok().map(Number)
    }
}

/// Records its name when it runs, then returns `value`. `V` is the format version.
#[derive(Clone)]
struct Probe<const V: u32 = 1> {
    source: u8,
    name: &'static str,
    value: u64,
    log: Arc<Mutex<Vec<&'static str>>>,
    runs: Arc<AtomicUsize>,
    steps: u32,
    step_time: Duration,
}

impl<const V: u32> Probe<V> {
    fn new(source: u8, name: &'static str, log: &Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            source,
            name,
            value: u64::from(source),
            log: log.clone(),
            runs: Arc::new(AtomicUsize::new(0)),
            steps: 1,
            step_time: Duration::ZERO,
        }
    }
}

impl<const V: u32> AnalysisJob for Probe<V> {
    type Output = Number;
    const KIND: &'static str = "probe";
    const VERSION: u32 = V;

    fn source_hash(&self) -> ContentHash {
        ContentHash([self.source; 32])
    }

    fn params(&self) -> String {
        format!("name={}", self.name)
    }

    fn run(&self, ctx: &JobContext) -> Result<Number, JobError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.log.lock().expect("log").push(self.name);
        for step in 0..self.steps {
            ctx.checkpoint()?;
            std::thread::sleep(self.step_time);
            ctx.progress((step + 1) as f32 / self.steps as f32);
        }
        Ok(Number(self.value))
    }
}

fn log() -> Arc<Mutex<Vec<&'static str>>> {
    Arc::new(Mutex::new(Vec::new()))
}

#[test]
fn runs_by_priority_then_submission_order() {
    let dir = TempDir::new("priority");
    let (pool, _events) = pool(&dir, 1);
    let log = log();
    pool.pause();
    let jobs = [
        ("bg1", Priority::Background),
        ("sel1", Priority::Selected),
        ("vis1", Priority::Visible),
        ("bg2", Priority::Background),
        ("vis2", Priority::Visible),
        ("sel2", Priority::Selected),
    ];
    let handles: Vec<_> = jobs
        .iter()
        .enumerate()
        .map(|(i, (name, priority))| pool.submit(Probe::<1>::new(i as u8, name, &log), *priority))
        .collect();
    pool.resume();
    for handle in &handles {
        handle.wait_timeout(LIMIT).expect("finished").expect("ok");
    }
    assert_eq!(
        *log.lock().expect("log"),
        ["vis1", "vis2", "sel1", "sel2", "bg1", "bg2"]
    );
}

#[test]
fn reprioritising_moves_a_queued_job() {
    let dir = TempDir::new("reprioritise");
    let (pool, _events) = pool(&dir, 1);
    let log = log();
    pool.pause();
    let a = pool.submit(Probe::<1>::new(1, "a", &log), Priority::Background);
    let b = pool.submit(Probe::<1>::new(2, "b", &log), Priority::Background);
    let c = pool.submit(Probe::<1>::new(3, "c", &log), Priority::Background);
    let d = pool.submit(Probe::<1>::new(4, "d", &log), Priority::Selected);
    // c scrolls into view; d is deselected.
    c.set_priority(Priority::Visible);
    pool.set_priority(d.key(), Priority::Background);
    // Resubmitting a lower priority never lowers a queued job.
    let c_again = pool.submit(Probe::<1>::new(3, "c", &log), Priority::Background);
    pool.resume();
    for handle in [&a, &b, &c, &d, &c_again] {
        handle.wait_timeout(LIMIT).expect("finished").expect("ok");
    }
    assert_eq!(*log.lock().expect("log"), ["c", "a", "b", "d"]);
}

#[test]
fn cancelling_stops_a_running_job_and_removes_a_queued_one() {
    let dir = TempDir::new("cancel");
    let (pool, events) = pool(&dir, 1);
    let log = log();
    let mut long = Probe::<1>::new(1, "long", &log);
    long.steps = 10_000;
    long.step_time = Duration::from_millis(1);
    let running = pool.submit(long, Priority::Visible);
    let queued = pool.submit(Probe::<1>::new(2, "queued", &log), Priority::Background);
    // Wait until the long job reports progress, so it is mid-run.
    let deadline = Instant::now() + LIMIT;
    loop {
        let event = events.recv_timeout(LIMIT).expect("event");
        if event.key == *running.key() && matches!(event.kind, JobEventKind::Progress(_)) {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    queued.cancel();
    running.cancel();
    let started = Instant::now();
    assert_eq!(
        running.wait_timeout(LIMIT).expect("finished"),
        Err(JobError::Cancelled)
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cancelled promptly"
    );
    assert_eq!(
        queued.wait_timeout(LIMIT).expect("finished"),
        Err(JobError::Cancelled)
    );
    assert_eq!(
        *log.lock().expect("log"),
        ["long"],
        "the queued job never ran"
    );
    // Nothing was written for either.
    let entry = dir
        .cache()
        .entry(&ContentHash([1; 32]), "probe", "name=long", 1);
    assert_eq!(entry.read("txt").expect("read"), None);
    let finished: Vec<Outcome> = events
        .try_iter()
        .chain(std::iter::from_fn(|| {
            events.recv_timeout(Duration::from_millis(200)).ok()
        }))
        .filter_map(|e| match e.kind {
            JobEventKind::Finished(outcome) => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(finished, [Outcome::Cancelled, Outcome::Cancelled]);
}

#[test]
fn the_same_key_twice_is_one_job() {
    let dir = TempDir::new("dedup");
    let (pool, _events) = pool(&dir, 2);
    let log = log();
    pool.pause();
    let first = Probe::<1>::new(9, "same", &log);
    let runs = first.runs.clone();
    let a = pool.submit(first.clone(), Priority::Background);
    let b = pool.submit(first.clone(), Priority::Visible);
    assert_eq!(pool.pending(), (1, 0));
    pool.resume();
    let ra = a.wait_timeout(LIMIT).expect("finished").expect("ok");
    let rb = b.wait_timeout(LIMIT).expect("finished").expect("ok");
    assert!(Arc::ptr_eq(&ra, &rb));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    // Different parameters are a different job.
    let other = pool.submit(Probe::<1>::new(9, "other", &log), Priority::Visible);
    other.wait_timeout(LIMIT).expect("finished").expect("ok");
    assert_eq!(log.lock().expect("log").len(), 2);
}

#[test]
fn a_second_run_is_a_cache_hit_and_a_version_bump_invalidates() {
    let dir = TempDir::new("cache");
    let log = log();
    let job = Probe::<1>::new(5, "cached", &log);
    let runs = job.runs.clone();
    {
        let (pool, _events) = pool(&dir, 1);
        let value = pool
            .submit(job.clone(), Priority::Visible)
            .wait_timeout(LIMIT)
            .expect("finished")
            .expect("ok");
        assert_eq!(*value, Number(5));
    }
    // A new pool (a new session) on the same folder: no work, same value, Cached.
    let (pool, events) = pool(&dir, 1);
    let handle = pool.submit(job.clone(), Priority::Visible);
    assert_eq!(
        *handle.wait_timeout(LIMIT).expect("finished").expect("ok"),
        Number(5)
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1, "no work on a hit");
    let outcome = events
        .iter()
        .find_map(|e| match e.kind {
            JobEventKind::Finished(o) => Some(o),
            _ => None,
        })
        .expect("finished event");
    assert_eq!(outcome, Outcome::Cached);

    // Version 2 of the same kind misses, runs, and deletes the version 1 file.
    let mut v2: Probe<2> = Probe::new(5, "cached", &log);
    v2.runs = runs.clone();
    v2.value = 50;
    let value = pool
        .submit(v2.clone(), Priority::Visible)
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    assert_eq!(*value, Number(50));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    let old = dir
        .cache()
        .entry(&ContentHash([5; 32]), "probe", "name=cached", 1);
    assert_eq!(old.read("txt").expect("read"), None, "old version removed");
    let new = dir
        .cache()
        .entry(&ContentHash([5; 32]), "probe", "name=cached", 2);
    assert_eq!(new.read("txt").expect("read"), Some(b"50".to_vec()));
    // A corrupt main file runs the job again instead of failing.
    new.write("txt", b"garbage").expect("write");
    let value = pool
        .submit(v2, Priority::Visible)
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    assert_eq!(*value, Number(50));
    assert_eq!(runs.load(Ordering::SeqCst), 3);
}

#[test]
fn progress_events_arrive_in_order() {
    let dir = TempDir::new("events");
    let (pool, events) = pool(&dir, 1);
    let log = log();
    let mut job = Probe::<1>::new(7, "progress", &log);
    job.steps = 400;
    let handle = pool.submit(job, Priority::Visible);
    handle.wait_timeout(LIMIT).expect("finished").expect("ok");
    let mine: Vec<JobEventKind> = std::iter::from_fn(|| events.recv_timeout(LIMIT).ok())
        .filter(|e| e.key == *handle.key())
        .map(|e| e.kind)
        .take_while(|k| !matches!(k, JobEventKind::Finished(_)))
        .collect();
    assert_eq!(mine[0], JobEventKind::Queued(Priority::Visible));
    assert_eq!(mine[1], JobEventKind::Started);
    let progress: Vec<f32> = mine[2..]
        .iter()
        .map(|k| match k {
            JobEventKind::Progress(p) => *p,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert!(progress.windows(2).all(|w| w[0] < w[1]), "increasing");
    assert_eq!(progress.last().copied(), Some(1.0));
    // 400 steps coalesced to at most one event per percent.
    assert!(progress.len() <= 101, "{} events", progress.len());
}

#[test]
fn pause_holds_new_and_running_jobs_until_resume() {
    let dir = TempDir::new("pause");
    let (pool, events) = pool(&dir, 2);
    let log = log();
    let mut long = Probe::<1>::new(1, "long", &log);
    long.steps = 200;
    long.step_time = Duration::from_millis(2);
    let long_runs = long.runs.clone();
    let running = pool.submit(long, Priority::Visible);
    // Mid-run, then pause.
    loop {
        let e = events.recv_timeout(LIMIT).expect("event");
        if matches!(e.kind, JobEventKind::Progress(p) if p > 0.1) {
            break;
        }
    }
    pool.pause();
    assert!(pool.is_paused());
    let waiting = pool.submit(Probe::<1>::new(2, "waiting", &log), Priority::Visible);
    // Drain what the running job sent before it parked, then expect silence.
    std::thread::sleep(Duration::from_millis(100));
    while events.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(300));
    let during: Vec<JobEvent> = events.try_iter().collect();
    assert!(during.is_empty(), "no progress while paused: {during:?}");
    assert!(running.try_result().is_none());
    assert!(waiting.try_result().is_none());
    assert_eq!(log.lock().expect("log").as_slice(), ["long"]);
    pool.resume();
    running.wait_timeout(LIMIT).expect("finished").expect("ok");
    waiting.wait_timeout(LIMIT).expect("finished").expect("ok");
    assert_eq!(
        long_runs.load(Ordering::SeqCst),
        1,
        "resumed, not restarted"
    );
}

#[test]
fn cancelling_a_paused_job_wakes_it() {
    let dir = TempDir::new("pause-cancel");
    let (pool, events) = pool(&dir, 1);
    let log = log();
    let mut long = Probe::<1>::new(1, "long", &log);
    long.steps = 1000;
    long.step_time = Duration::from_millis(1);
    let running = pool.submit(long, Priority::Visible);
    loop {
        let e = events.recv_timeout(LIMIT).expect("event");
        if matches!(e.kind, JobEventKind::Progress(_)) {
            break;
        }
    }
    pool.pause();
    running.cancel();
    assert_eq!(
        running.wait_timeout(LIMIT).expect("finished"),
        Err(JobError::Cancelled)
    );
}

#[test]
fn concurrent_submits_cancels_and_moves_do_not_deadlock() {
    let dir = TempDir::new("stress");
    let (pool, events) = pool(&dir, 3);
    let pool = Arc::new(pool);
    let log = log();
    let threads = 8;
    let per_thread = 200;
    let barrier = Arc::new(Barrier::new(threads));
    let started = Instant::now();
    let workers: Vec<_> = (0..threads)
        .map(|t| {
            let pool = pool.clone();
            let log = log.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut handles = Vec::new();
                for i in 0..per_thread {
                    // 40 distinct keys across all threads, so most submits dedup.
                    let n = ((t * 7 + i * 13) % 40) as u8;
                    let names = ["a", "b"];
                    let mut job = Probe::<1>::new(n, names[i % 2], &log);
                    job.steps = 3;
                    let priority =
                        [Priority::Background, Priority::Selected, Priority::Visible][(i + t) % 3];
                    let handle = pool.submit(job, priority);
                    match i % 10 {
                        3 => handle.cancel(),
                        5 => handle.set_priority(Priority::Visible),
                        7 => pool.pause(),
                        8 => pool.resume(),
                        _ => {}
                    }
                    handles.push(handle);
                }
                handles
            })
        })
        .collect();
    let handles: Vec<_> = workers
        .into_iter()
        .flat_map(|w| w.join().expect("thread"))
        .collect();
    pool.resume();
    for handle in &handles {
        let result = handle.wait_timeout(LIMIT).expect("every job finishes");
        assert!(
            matches!(result, Ok(_) | Err(JobError::Cancelled)),
            "{result:?}"
        );
    }
    // Workers finish their bookkeeping just after the result is published.
    while pool.pending() != (0, 0) {
        assert!(started.elapsed() < LIMIT, "pool drains");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Every job that was queued reported exactly one Finished.
    let all: Vec<JobEvent> = events.try_iter().collect();
    let queued = all
        .iter()
        .filter(|e| matches!(e.kind, JobEventKind::Queued(_)))
        .count();
    let finished = all
        .iter()
        .filter(|e| matches!(e.kind, JobEventKind::Finished(_)))
        .count();
    assert_eq!(queued, finished);
}
