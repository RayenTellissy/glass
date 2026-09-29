//! The pool, its queue and the job trait (see the module docs of `analysis`).

use std::any::Any;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use media::{CacheDir, CacheEntry, ContentHash};

/// Threads the rest of the app keeps busy: UI, render, audio (decode and callback share
/// a core's worth of work) and two for video decode during playback.
const RESERVED_THREADS: usize = 5;
/// Upper bound for this machine class (8 GB): a worker can hold a 4K software decoder
/// (about 100 to 150 MB of frames and references), so three stay under 0.5 GB.
const MAX_WORKERS: usize = 3;

/// Analysis workers for this machine: logical cores minus [`RESERVED_THREADS`], at least
/// one and at most [`MAX_WORKERS`] (3 on an 8-core M1 or M2).
pub fn default_workers() -> usize {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    cores.saturating_sub(RESERVED_THREADS).clamp(1, MAX_WORKERS)
}

/// How urgent a job is. Higher runs first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Media not on screen (import, idle prefetch).
    Background,
    /// Selected in the media pool or timeline.
    Selected,
    /// On screen now.
    Visible,
}

/// Identity of a result: which file, which analysis, which settings, which format.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobKey {
    pub hash: ContentHash,
    pub kind: &'static str,
    /// [`media::cache::params_hash`] of the parameter string.
    pub params: String,
    pub version: u32,
}

impl JobKey {
    pub fn new(hash: ContentHash, kind: &'static str, params: &str, version: u32) -> Self {
        Self {
            hash,
            kind,
            params: media::cache::params_hash(params),
            version,
        }
    }
}

impl std::fmt::Display for JobKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex = self.hash.to_hex();
        write!(
            f,
            "{}:{}-{}-v{}",
            &hex[..12],
            self.kind,
            self.params,
            self.version
        )
    }
}

/// Why a job produced no result.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
}

impl From<media::MediaError> for JobError {
    fn from(error: media::MediaError) -> Self {
        Self::Failed(error.to_string())
    }
}

impl From<std::io::Error> for JobError {
    fn from(error: std::io::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

/// A job's result as bytes on disk: the main file of its [`CacheEntry`].
pub trait Artifact: Sized + Send + Sync + 'static {
    /// Extension of the main file (`json`, `bin`).
    const EXTENSION: &'static str;
    fn encode(&self) -> std::io::Result<Vec<u8>>;
    /// `None` when the bytes are not a valid result (the job then runs again).
    fn decode(bytes: &[u8], entry: &CacheEntry) -> Option<Self>;
}

/// One kind of analysis. The type is the registration: submitting a value of it is all
/// the pool needs.
pub trait AnalysisJob: Send + 'static {
    type Output: Artifact;
    /// Stable name, part of the cache file names: lower-case letters, digits and `_`.
    const KIND: &'static str;
    /// Bumped whenever the output format or the algorithm changes; results of other
    /// versions stop matching and are deleted when the new one is written.
    const VERSION: u32;

    fn source_hash(&self) -> ContentHash;

    /// The settings the output depends on, as a canonical string (hashed into the key).
    fn params(&self) -> String;

    fn run(&self, ctx: &JobContext) -> Result<Self::Output, JobError>;

    fn key(&self) -> JobKey {
        JobKey::new(
            self.source_hash(),
            Self::KIND,
            &self.params(),
            Self::VERSION,
        )
    }
}

/// How a finished job ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Ran and wrote its result.
    Computed,
    /// Found on disk; no work done.
    Cached,
    Cancelled,
    Failed(String),
}

/// What happened to a job, for the UI.
#[derive(Clone, Debug, PartialEq)]
pub struct JobEvent {
    pub key: JobKey,
    /// Distinguishes a job from an earlier one with the same key.
    pub id: u64,
    pub kind: JobEventKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobEventKind {
    Queued(Priority),
    Started,
    /// Fraction done, 0 to 1, increasing; sent at most every 1% (and at 1).
    Progress(f32),
    /// Part of the result is on disk (for example a coarse thumbnail level); the number
    /// is job-specific.
    Partial(u32),
    Finished(Outcome),
}

/// Pool settings.
#[derive(Clone, Debug)]
pub struct PoolConfig {
    pub workers: usize,
    pub cache: CacheDir,
}

impl PoolConfig {
    /// [`default_workers`] threads on this cache folder.
    pub fn new(cache: CacheDir) -> Self {
        Self {
            workers: default_workers(),
            cache,
        }
    }
}

/// Handed to [`AnalysisJob::run`]: cancellation, pause, progress and the job's files.
pub struct JobContext {
    shared: Arc<Shared>,
    key: JobKey,
    id: u64,
    cancel: Arc<AtomicBool>,
    entry: CacheEntry,
    last_progress: AtomicU32,
}

impl JobContext {
    /// Whether the job was cancelled (or the pool is shutting down).
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire) || self.shared.shutdown.load(Ordering::Acquire)
    }

    /// Call between units of work: returns `Err(Cancelled)` once the job is cancelled,
    /// and waits here while the pool is paused.
    pub fn checkpoint(&self) -> Result<(), JobError> {
        if self.shared.paused.load(Ordering::Acquire) {
            let mut state = self.shared.lock();
            while state.paused && !self.is_cancelled() {
                state = self
                    .shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
        if self.is_cancelled() {
            Err(JobError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Reports the fraction done (0 to 1). Coalesced to 1% steps; never goes back.
    pub fn progress(&self, fraction: f32) {
        let fraction = fraction.clamp(0.0, 1.0);
        // Steps are stored plus one, so 0 means nothing sent yet.
        let step = (fraction * 100.0).floor() as u32 + 1;
        if self.last_progress.fetch_max(step, Ordering::Relaxed) < step {
            self.send(JobEventKind::Progress(fraction));
        }
    }

    /// Announces that part of the result is on disk.
    pub fn partial(&self, part: u32) {
        self.send(JobEventKind::Partial(part));
    }

    /// The job's files in the cache folder (side files such as atlases go here; the pool
    /// writes the main file).
    pub fn entry(&self) -> &CacheEntry {
        &self.entry
    }

    pub fn cache(&self) -> &CacheDir {
        &self.shared.cache
    }

    fn send(&self, kind: JobEventKind) {
        self.shared.send(&self.key, self.id, kind);
    }
}

/// The caller's view of a submitted job.
pub struct JobHandle<T> {
    key: JobKey,
    slot: Arc<Slot<T>>,
    pool: Weak<Shared>,
}

impl<T> Clone for JobHandle<T> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            slot: self.slot.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl<T: Send + Sync + 'static> JobHandle<T> {
    pub fn key(&self) -> &JobKey {
        &self.key
    }

    /// The result once the job finished; never blocks.
    pub fn try_result(&self) -> Option<Result<Arc<T>, JobError>> {
        self.slot.value.get().cloned()
    }

    /// Waits for the result (tests, benches and worker threads; never the UI thread).
    pub fn wait(&self) -> Result<Arc<T>, JobError> {
        let mut done = self
            .slot
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while !*done {
            done = self
                .slot
                .cv
                .wait(done)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(done);
        self.try_result()
            .unwrap_or_else(|| Err(JobError::Failed("no result".to_string())))
    }

    /// [`Self::wait`] with a limit; `None` on timeout.
    pub fn wait_timeout(&self, limit: Duration) -> Option<Result<Arc<T>, JobError>> {
        let deadline = Instant::now() + limit;
        let mut done = self
            .slot
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while !*done {
            let left = deadline.checked_duration_since(Instant::now())?;
            done = self
                .slot
                .cv
                .wait_timeout(done, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        drop(done);
        self.try_result()
    }

    /// Cancels the job for every holder of a handle to it.
    pub fn cancel(&self) {
        if let Some(shared) = self.pool.upgrade() {
            Shared::cancel(&shared, &self.key);
        }
    }

    /// Moves the job in the queue (no effect once it runs).
    pub fn set_priority(&self, priority: Priority) {
        if let Some(shared) = self.pool.upgrade() {
            shared.set_priority(&self.key, priority);
        }
    }
}

struct Slot<T> {
    value: OnceLock<Result<Arc<T>, JobError>>,
    done: Mutex<bool>,
    cv: Condvar,
}

impl<T> Slot<T> {
    fn new() -> Self {
        Self {
            value: OnceLock::new(),
            done: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    fn finish(&self, result: Result<Arc<T>, JobError>) {
        let _ = self.value.set(result);
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_all();
    }
}

/// A job with its type erased, as the queue holds it.
trait ErasedJob: Send {
    fn execute(self: Box<Self>, ctx: &JobContext) -> Outcome;
    /// Finishes the result slot without running (cancelled while queued).
    fn abandon(self: Box<Self>, error: JobError);
}

struct Typed<J: AnalysisJob> {
    job: J,
    slot: Arc<Slot<J::Output>>,
}

impl<J: AnalysisJob> ErasedJob for Typed<J> {
    fn execute(self: Box<Self>, ctx: &JobContext) -> Outcome {
        let entry = ctx.entry();
        let ext = <J::Output as Artifact>::EXTENSION;
        match entry.read(ext) {
            Ok(Some(bytes)) => {
                if let Some(output) = J::Output::decode(&bytes, entry) {
                    self.slot.finish(Ok(Arc::new(output)));
                    return Outcome::Cached;
                }
                tracing::warn!(
                    entry = entry.stem(),
                    "cached analysis result unreadable, running again"
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(entry = entry.stem(), %error, "cannot read the analysis cache")
            }
        }
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.job.run(ctx)))
            .unwrap_or_else(|_| Err(JobError::Failed("the analysis job panicked".to_string())));
        let result = run.and_then(|output| {
            ctx.checkpoint()?;
            let bytes = output.encode()?;
            match entry.write(ext, &bytes) {
                Ok(_) => {
                    if let Err(error) = entry.remove_other_versions() {
                        tracing::debug!(entry = entry.stem(), %error, "could not remove old analysis versions");
                    }
                }
                Err(error) => {
                    tracing::warn!(entry = entry.stem(), %error, "could not write the analysis result");
                }
            }
            Ok(output)
        });
        let outcome = match &result {
            Ok(_) => Outcome::Computed,
            Err(JobError::Cancelled) => Outcome::Cancelled,
            Err(JobError::Failed(message)) => Outcome::Failed(message.clone()),
        };
        self.slot.finish(result.map(Arc::new));
        outcome
    }

    fn abandon(self: Box<Self>, error: JobError) {
        self.slot.finish(Err(error));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct QueuePos {
    /// Reversed so the highest priority sorts first.
    rank: std::cmp::Reverse<Priority>,
    seq: u64,
}

enum Status {
    Queued(Box<dyn ErasedJob>),
    Running,
}

struct Tracked {
    id: u64,
    priority: Priority,
    seq: u64,
    status: Status,
    cancel: Arc<AtomicBool>,
    /// `Arc<Slot<Output>>` for handing out handles to later submitters.
    slot: Arc<dyn Any + Send + Sync>,
}

#[derive(Default)]
struct State {
    queue: BTreeSet<(QueuePos, JobKey)>,
    jobs: HashMap<JobKey, Tracked>,
    paused: bool,
    next_id: u64,
    running: usize,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled on any change a waiting worker or paused job cares about.
    changed: Condvar,
    paused: AtomicBool,
    shutdown: AtomicBool,
    cache: CacheDir,
    events: Sender<JobEvent>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Every critical section leaves the state consistent before anything can panic.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn send(&self, key: &JobKey, id: u64, kind: JobEventKind) {
        // The receiver may be gone (UI closed); events are then dropped.
        let _ = self.events.send(JobEvent {
            key: key.clone(),
            id,
            kind,
        });
    }

    fn cancel(shared: &Arc<Shared>, key: &JobKey) {
        let mut state = shared.lock();
        let Some(tracked) = state.jobs.get(key) else {
            return;
        };
        tracked.cancel.store(true, Ordering::Release);
        let pos = QueuePos {
            rank: std::cmp::Reverse(tracked.priority),
            seq: tracked.seq,
        };
        let id = tracked.id;
        if matches!(tracked.status, Status::Queued(_)) {
            state.queue.remove(&(pos, key.clone()));
            if let Some(Tracked {
                status: Status::Queued(job),
                ..
            }) = state.jobs.remove(key)
            {
                drop(state);
                job.abandon(JobError::Cancelled);
                shared.send(key, id, JobEventKind::Finished(Outcome::Cancelled));
                return;
            }
        } else {
            // Running: the worker reports when the job reaches a checkpoint. A new submit
            // of the key starts a fresh job.
            state.jobs.remove(key);
        }
        drop(state);
        shared.changed.notify_all();
    }

    fn set_priority(&self, key: &JobKey, priority: Priority) {
        let mut state = self.lock();
        Self::reprioritise(&mut state, key, priority, false);
    }

    /// Moves a queued job; with `only_up`, only to a higher priority.
    fn reprioritise(state: &mut State, key: &JobKey, priority: Priority, only_up: bool) {
        let Some(tracked) = state.jobs.get_mut(key) else {
            return;
        };
        if !matches!(tracked.status, Status::Queued(_))
            || tracked.priority == priority
            || (only_up && priority < tracked.priority)
        {
            return;
        }
        let old = QueuePos {
            rank: std::cmp::Reverse(tracked.priority),
            seq: tracked.seq,
        };
        tracked.priority = priority;
        let new = QueuePos {
            rank: std::cmp::Reverse(priority),
            seq: tracked.seq,
        };
        state.queue.remove(&(old, key.clone()));
        state.queue.insert((new, key.clone()));
    }
}

/// The analysis worker pool (see the module docs).
pub struct AnalysisPool {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl AnalysisPool {
    /// Starts `config.workers` threads and returns the pool with the receiving end of its
    /// event channel, for the UI to drain with `try_recv`.
    pub fn new(config: PoolConfig) -> std::io::Result<(Self, Receiver<JobEvent>)> {
        let (events, receiver) = channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            paused: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            cache: config.cache,
            events,
        });
        let mut workers = Vec::new();
        for n in 0..config.workers.max(1) {
            let worker_shared = shared.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("glass-analysis-{n}"))
                .spawn(move || worker(&worker_shared));
            match spawned {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    let pool = Self { shared, workers };
                    drop(pool);
                    return Err(error);
                }
            }
        }
        Ok((Self { shared, workers }, receiver))
    }

    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    pub fn cache(&self) -> &CacheDir {
        &self.shared.cache
    }

    /// Queues a job, or joins the queued or running job with the same key (raising its
    /// priority if this request is higher).
    pub fn submit<J: AnalysisJob>(&self, job: J, priority: Priority) -> JobHandle<J::Output> {
        let key = job.key();
        let handle = |slot: Arc<Slot<J::Output>>| JobHandle {
            key: key.clone(),
            slot,
            pool: Arc::downgrade(&self.shared),
        };
        let mut state = self.shared.lock();
        if let Some(tracked) = state.jobs.get(&key) {
            match tracked.slot.clone().downcast::<Slot<J::Output>>() {
                Ok(slot) => {
                    Shared::reprioritise(&mut state, &key, priority, true);
                    return handle(slot);
                }
                Err(_) => {
                    // Two job types claim the same kind: a programming error.
                    tracing::error!(kind = J::KIND, "two analysis job types share a kind");
                    let slot = Arc::new(Slot::new());
                    slot.finish(Err(JobError::Failed(format!(
                        "analysis kind {} is used by two job types",
                        J::KIND
                    ))));
                    return handle(slot);
                }
            }
        }
        let slot: Arc<Slot<J::Output>> = Arc::new(Slot::new());
        let id = state.next_id;
        state.next_id += 1;
        let pos = QueuePos {
            rank: std::cmp::Reverse(priority),
            seq: id,
        };
        state.queue.insert((pos, key.clone()));
        state.jobs.insert(
            key.clone(),
            Tracked {
                id,
                priority,
                seq: id,
                status: Status::Queued(Box::new(Typed {
                    job,
                    slot: slot.clone(),
                })),
                cancel: Arc::new(AtomicBool::new(false)),
                slot: slot.clone(),
            },
        );
        // Sent under the lock so Queued always precedes the worker's Started.
        self.shared.send(&key, id, JobEventKind::Queued(priority));
        drop(state);
        self.shared.changed.notify_all();
        handle(slot)
    }

    /// Moves a queued job to another priority (no effect once it runs).
    pub fn set_priority(&self, key: &JobKey, priority: Priority) {
        self.shared.set_priority(key, priority);
    }

    /// Cancels a queued or running job.
    pub fn cancel(&self, key: &JobKey) {
        Shared::cancel(&self.shared, key);
    }

    /// Stops starting jobs and parks running ones at their next checkpoint.
    pub fn pause(&self) {
        let mut state = self.shared.lock();
        state.paused = true;
        self.shared.paused.store(true, Ordering::Release);
    }

    pub fn resume(&self) {
        let mut state = self.shared.lock();
        state.paused = false;
        self.shared.paused.store(false, Ordering::Release);
        drop(state);
        self.shared.changed.notify_all();
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Acquire)
    }

    /// Jobs queued and running.
    pub fn pending(&self) -> (usize, usize) {
        let state = self.shared.lock();
        (state.queue.len(), state.running)
    }
}

impl Drop for AnalysisPool {
    fn drop(&mut self) {
        let abandoned: Vec<(JobKey, u64, Box<dyn ErasedJob>)> = {
            let mut state = self.shared.lock();
            self.shared.shutdown.store(true, Ordering::Release);
            state.queue.clear();
            let keys: Vec<JobKey> = state.jobs.keys().cloned().collect();
            let mut out = Vec::new();
            for key in keys {
                if let Some(tracked) = state.jobs.get(&key) {
                    tracked.cancel.store(true, Ordering::Release);
                    if matches!(tracked.status, Status::Queued(_))
                        && let Some(Tracked {
                            id,
                            status: Status::Queued(job),
                            ..
                        }) = state.jobs.remove(&key)
                    {
                        out.push((key, id, job));
                    }
                }
            }
            out
        };
        self.shared.changed.notify_all();
        for (key, id, job) in abandoned {
            job.abandon(JobError::Cancelled);
            self.shared
                .send(&key, id, JobEventKind::Finished(Outcome::Cancelled));
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn worker(shared: &Arc<Shared>) {
    loop {
        let (key, id, job, cancel) = {
            let mut state = shared.lock();
            loop {
                if shared.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if !state.paused
                    && let Some(first) = state.queue.pop_first()
                {
                    let key = first.1;
                    let Some(tracked) = state.jobs.get_mut(&key) else {
                        continue;
                    };
                    let Status::Queued(job) =
                        std::mem::replace(&mut tracked.status, Status::Running)
                    else {
                        continue;
                    };
                    let (id, cancel) = (tracked.id, tracked.cancel.clone());
                    state.running += 1;
                    // Under the lock, so a cancel cannot report before Started.
                    shared.send(&key, id, JobEventKind::Started);
                    break (key, id, job, cancel);
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let ctx = JobContext {
            shared: shared.clone(),
            entry: entry_for(&shared.cache, &key),
            key: key.clone(),
            id,
            cancel,
            last_progress: AtomicU32::new(0),
        };
        let outcome = job.execute(&ctx);
        if outcome == Outcome::Computed {
            ctx.progress(1.0);
        }
        let mut state = shared.lock();
        state.running -= 1;
        if state.jobs.get(&key).is_some_and(|t| t.id == id) {
            state.jobs.remove(&key);
        }
        shared.send(&key, id, JobEventKind::Finished(outcome));
        drop(state);
        shared.changed.notify_all();
    }
}

/// The cache entry of a key (its params are already hashed).
fn entry_for(cache: &CacheDir, key: &JobKey) -> CacheEntry {
    cache.entry_hashed(&key.hash, key.kind, &key.params, key.version)
}
