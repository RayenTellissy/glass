//! The thread registry: every long-lived Glass thread, its name, its OS thread id and
//! how busy it is, for profilers, crash reports and the debug panel (Phase 6).
//!
//! **Spawning.** Long-lived threads start through [`spawn`] (or
//! [`ThreadRegistry::spawn`] on a registry of one's own, for tests). The helper names the
//! thread `glass-<role>` or `glass-<role>-<n>` from its [`Role`], registers it before the
//! OS thread exists (so it is listed as soon as `spawn` returns), records the OS thread id
//! when the thread starts, and removes the entry when the thread's closure returns or
//! unwinds, before its `JoinHandle` reports it finished. The UI thread, which the process
//! starts, registers itself with [`ThreadRegistry::register_current`].
//!
//! **Activity accounting.** A registered thread is `Busy` unless it says otherwise. The
//! places where it parks or waits wrap the wait in [`idle`] (waiting for work) or
//! [`blocked`] (waiting for room downstream: a full queue); both find the calling thread's
//! entry in a thread-local and do nothing on an unregistered thread. Each state change
//! adds the time spent in the previous state to that state's total and stamps the change.
//! Only the owning thread writes its entry, through a sequence counter (a single-writer
//! seqlock of atomics), so a reader on any thread gets a consistent copy of state, totals
//! and timestamps without taking a lock or stopping the writer.
//!
//! **Reading.** [`ThreadRegistry::snapshot_into`] copies every live entry into a
//! caller-kept `Vec` (reused once per UI frame, so it does not allocate once warm). The
//! membership list sits behind a mutex that only spawning, exiting and snapshots take,
//! each for a few pointer copies; the threads themselves never lock to account activity.
//! Load over an interval is the difference of two snapshots
//! ([`ThreadInfo::busy_fraction_since`]).
//!
//! **Audio callback.** The callback thread belongs to the audio backend, so it cannot be
//! spawned here. [`ThreadRegistry::register_audio_callback`] registers it from an
//! ordinary thread ahead of time and returns an [`AudioCallbackHook`] whose
//! [`enter`](AudioCallbackHook::enter) and [`exit`](AudioCallbackHook::exit) the callback
//! calls at its start and end: they store a few atomics and read the clock, never
//! allocate, lock or touch a thread-local (`tests/threads_audio_alloc.rs`). Dropping the
//! last clone only marks the entry exited; the registry prunes it on its next snapshot or
//! registration, so even a drop on the callback thread takes no lock.

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering, fence};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What a thread is for. Its name is `glass-<role>` or `glass-<role>-<n>`, except the UI
/// thread, which is the process main thread (`main`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The process main thread: egui, input, the project root and undo history.
    Ui,
    /// `glass-sched`: playback state and what to decode next.
    Scheduler,
    /// `glass-decode-<n>`: the decode pool.
    Decode,
    /// `glass-audio-decode`: audio decode and resampling into the feed rings.
    AudioDecode,
    /// `glass-render`: render graphs and the frames handed to present.
    Render,
    /// `glass-audio`: the audio device callback (owned by the backend).
    AudioCallback,
    /// `glass-analysis-<n>`: the analysis pool.
    Analysis,
    /// `glass-import`: an import scan or import.
    Import,
    /// `glass-io`: autosave and project save.
    Io,
    /// `glass-export`: a running export.
    Export,
}

impl Role {
    /// The thread name without a pool index.
    pub const fn base_name(self) -> &'static str {
        match self {
            Self::Ui => "main",
            Self::Scheduler => "glass-sched",
            Self::Decode => "glass-decode",
            Self::AudioDecode => "glass-audio-decode",
            Self::Render => "glass-render",
            Self::AudioCallback => "glass-audio",
            Self::Analysis => "glass-analysis",
            Self::Import => "glass-import",
            Self::Io => "glass-io",
            Self::Export => "glass-export",
        }
    }

    /// The thread name: the base name, then `-<n>` for a pool member.
    pub fn thread_name(self, index: Option<usize>) -> String {
        match index {
            Some(n) => format!("{}-{n}", self.base_name()),
            None => self.base_name().to_owned(),
        }
    }
}

/// What a thread is doing now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThreadState {
    /// Registered; the OS thread has not started yet (or the audio callback has not run).
    Starting,
    /// Working.
    Busy,
    /// Waiting for work.
    Idle,
    /// Waiting for room downstream (a full queue).
    Blocked,
    /// Gone; pruned from the registry.
    Exited,
}

impl ThreadState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Starting,
            1 => Self::Busy,
            2 => Self::Idle,
            3 => Self::Blocked,
            _ => Self::Exited,
        }
    }
}

/// One thread as a snapshot saw it.
#[derive(Clone, Debug)]
pub struct ThreadInfo {
    /// Unique within its registry, never reused.
    pub id: u64,
    pub role: Role,
    /// The pool index of a pool member.
    pub index: Option<usize>,
    pub name: Arc<str>,
    /// The OS thread id (`pthread_threadid_np` on macOS, `GetCurrentThreadId` on
    /// Windows), once the thread has run.
    pub os_id: Option<u64>,
    pub state: ThreadState,
    /// Time spent busy, idle and blocked since the thread started, up to the snapshot.
    pub busy: Duration,
    pub idle: Duration,
    pub blocked: Duration,
    /// Busy spans started: how often the thread woke up to work.
    pub wakeups: u64,
    /// When the thread was last busy (the snapshot time if it is busy now).
    pub last_active: Option<Instant>,
    /// When the snapshot was taken.
    pub taken: Instant,
}

impl ThreadInfo {
    /// The share of time spent busy between an earlier snapshot of this thread and this
    /// one, from 0 to 1 (0 when no time passed).
    pub fn busy_fraction_since(&self, earlier: &Self) -> f64 {
        let busy = self.busy.saturating_sub(earlier.busy).as_secs_f64();
        let waiting = (self.idle + self.blocked)
            .saturating_sub(earlier.idle + earlier.blocked)
            .as_secs_f64();
        let total = busy + waiting;
        if total > 0.0 { busy / total } else { 0.0 }
    }

    /// The share of time spent busy since the thread started.
    pub fn busy_fraction(&self) -> f64 {
        let busy = self.busy.as_secs_f64();
        let total = busy + (self.idle + self.blocked).as_secs_f64();
        if total > 0.0 { busy / total } else { 0.0 }
    }
}

const NEVER: u64 = u64::MAX;

/// A registered thread's shared record. Every field but the membership data is written
/// by the owning thread only.
struct Entry {
    id: u64,
    role: Role,
    index: Option<usize>,
    name: Arc<str>,
    epoch: Instant,
    /// 0 until the thread has run.
    os_id: AtomicU64,
    /// Odd while the owner is changing the fields below.
    seq: AtomicU64,
    state: AtomicU8,
    /// Nanoseconds since the epoch when the current state began.
    since: AtomicU64,
    busy: AtomicU64,
    idle: AtomicU64,
    blocked: AtomicU64,
    wakeups: AtomicU64,
    /// Nanoseconds since the epoch when the thread last stopped being busy, or `NEVER`.
    last_active: AtomicU64,
}

impl Entry {
    fn now(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX - 1)
    }

    /// Records the OS thread id of the calling thread.
    fn attach_os_thread(&self) {
        let os_id = thread_id::get() as u64;
        if self.os_id.load(Ordering::Relaxed) != os_id {
            self.os_id.store(os_id, Ordering::Relaxed);
        }
    }

    /// Moves to `new` and returns the previous state. Owner only: allocation-free and
    /// lock-free.
    fn set_state(&self, new: ThreadState) -> ThreadState {
        let now = self.now();
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        let old = ThreadState::from_u8(self.state.load(Ordering::Relaxed));
        let span = now.saturating_sub(self.since.load(Ordering::Relaxed));
        let total = match old {
            ThreadState::Busy => Some(&self.busy),
            ThreadState::Idle => Some(&self.idle),
            ThreadState::Blocked => Some(&self.blocked),
            ThreadState::Starting | ThreadState::Exited => None,
        };
        if let Some(total) = total {
            total.store(
                total.load(Ordering::Relaxed).saturating_add(span),
                Ordering::Relaxed,
            );
        }
        if old == ThreadState::Busy {
            self.last_active.store(now, Ordering::Relaxed);
        }
        if new == ThreadState::Busy && old != ThreadState::Busy {
            self.wakeups
                .store(self.wakeups.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
        }
        self.state.store(new as u8, Ordering::Relaxed);
        self.since.store(now, Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
        old
    }

    fn state(&self) -> ThreadState {
        ThreadState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// A consistent copy of the entry, from any thread.
    fn read(&self) -> ThreadInfo {
        loop {
            let before = self.seq.load(Ordering::Acquire);
            let now = self.now();
            let taken = Instant::now();
            let state = ThreadState::from_u8(self.state.load(Ordering::Relaxed));
            let since = self.since.load(Ordering::Relaxed);
            let mut busy = self.busy.load(Ordering::Relaxed);
            let mut idle = self.idle.load(Ordering::Relaxed);
            let mut blocked = self.blocked.load(Ordering::Relaxed);
            let wakeups = self.wakeups.load(Ordering::Relaxed);
            let last_active = self.last_active.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            let after = self.seq.load(Ordering::Relaxed);
            if before % 2 == 1 || before != after {
                std::hint::spin_loop();
                continue;
            }
            // The open span of the current state counts up to now.
            let open = now.saturating_sub(since);
            match state {
                ThreadState::Busy => busy += open,
                ThreadState::Idle => idle += open,
                ThreadState::Blocked => blocked += open,
                ThreadState::Starting | ThreadState::Exited => {}
            }
            let last_active = match (state, last_active) {
                (ThreadState::Busy, _) => Some(taken),
                (_, NEVER) => None,
                (_, at) => Some(self.epoch + Duration::from_nanos(at)),
            };
            let os_id = self.os_id.load(Ordering::Relaxed);
            return ThreadInfo {
                id: self.id,
                role: self.role,
                index: self.index,
                name: self.name.clone(),
                os_id: (os_id != 0).then_some(os_id),
                state,
                busy: Duration::from_nanos(busy),
                idle: Duration::from_nanos(idle),
                blocked: Duration::from_nanos(blocked),
                wakeups,
                last_active,
                taken,
            };
        }
    }
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("state", &self.state())
            .finish()
    }
}

/// The list of registered threads (see the module docs).
pub struct ThreadRegistry {
    epoch: Instant,
    entries: Mutex<Vec<Arc<Entry>>>,
    next_id: AtomicU64,
}

static GLOBAL: LazyLock<Arc<ThreadRegistry>> = LazyLock::new(ThreadRegistry::new);

/// The process-wide registry the app's threads use and the debug panel reads.
pub fn global() -> &'static Arc<ThreadRegistry> {
    &GLOBAL
}

/// Spawns a thread registered in the [`global`] registry (see
/// [`ThreadRegistry::spawn`]).
pub fn spawn<F, T>(role: Role, index: Option<usize>, work: F) -> std::io::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    global().spawn(role, index, work)
}

thread_local! {
    /// The calling thread's entry, if it was spawned or registered through a registry.
    static CURRENT: RefCell<Option<Arc<Entry>>> = const { RefCell::new(None) };
}

/// Runs `wait` with the calling thread marked idle (waiting for work), then restores its
/// previous state. Wrap the places where a thread parks for new work in it.
pub fn idle<R>(wait: impl FnOnce() -> R) -> R {
    in_state(ThreadState::Idle, wait)
}

/// Runs `wait` with the calling thread marked blocked (waiting for room downstream),
/// then restores its previous state.
pub fn blocked<R>(wait: impl FnOnce() -> R) -> R {
    in_state(ThreadState::Blocked, wait)
}

fn in_state<R>(state: ThreadState, work: impl FnOnce() -> R) -> R {
    let entry = CURRENT.with_borrow(|current| current.clone());
    let Some(entry) = entry else {
        return work();
    };
    let previous = entry.set_state(state);
    let result = work();
    entry.set_state(previous);
    result
}

impl ThreadRegistry {
    /// An empty registry. The app uses [`global`]; tests may keep their own.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: Instant::now(),
            entries: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Arc<Entry>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn register(&self, role: Role, index: Option<usize>) -> Arc<Entry> {
        let entry = Arc::new(Entry {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            role,
            index,
            name: role.thread_name(index).into(),
            epoch: self.epoch,
            os_id: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            state: AtomicU8::new(ThreadState::Starting as u8),
            since: AtomicU64::new(0),
            busy: AtomicU64::new(0),
            idle: AtomicU64::new(0),
            blocked: AtomicU64::new(0),
            wakeups: AtomicU64::new(0),
            last_active: AtomicU64::new(NEVER),
        });
        entry.since.store(entry.now(), Ordering::Relaxed);
        let mut entries = self.lock();
        entries.retain(|e| e.state() != ThreadState::Exited);
        entries.push(entry.clone());
        entry
    }

    fn deregister(&self, id: u64) {
        self.lock().retain(|e| e.id != id);
    }

    /// Spawns a thread named for `role` (and `index` in a pool), registered here for its
    /// whole life. The entry is listed when this returns and removed when `work` returns
    /// or unwinds, before the `JoinHandle` reports the thread finished.
    pub fn spawn<F, T>(
        self: &Arc<Self>,
        role: Role,
        index: Option<usize>,
        work: F,
    ) -> std::io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let entry = self.register(role, index);
        let attached = Attached {
            registry: self.clone(),
            entry: entry.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name(entry.name.to_string())
            .spawn(move || {
                let _attached = attached.enter();
                work()
            });
        if spawned.is_err() {
            self.deregister(entry.id);
        }
        spawned
    }

    /// Registers the calling thread (the UI thread, or any thread this registry did not
    /// spawn) until the returned guard is dropped on it.
    pub fn register_current(self: &Arc<Self>, role: Role, index: Option<usize>) -> Registered {
        let entry = self.register(role, index);
        Registered {
            attached: Some(
                Attached {
                    registry: self.clone(),
                    entry,
                }
                .enter(),
            ),
            _not_send: std::marker::PhantomData,
        }
    }

    /// Registers the audio device callback thread; call from an ordinary thread before
    /// starting the stream, and move the hook into the callback.
    pub fn register_audio_callback(&self) -> AudioCallbackHook {
        AudioCallbackHook {
            inner: Arc::new(HookInner {
                entry: self.register(Role::AudioCallback, None),
            }),
        }
    }

    /// Replaces `out` with a copy of every live thread, in registration order. Reuses
    /// `out`'s allocation.
    pub fn snapshot_into(&self, out: &mut Vec<ThreadInfo>) {
        out.clear();
        let mut entries = self.lock();
        entries.retain(|e| e.state() != ThreadState::Exited);
        out.extend(entries.iter().map(|e| e.read()));
    }

    /// Every live thread, in registration order.
    pub fn snapshot(&self) -> Vec<ThreadInfo> {
        let mut out = Vec::new();
        self.snapshot_into(&mut out);
        out
    }

    /// Live threads.
    pub fn len(&self) -> usize {
        self.lock()
            .iter()
            .filter(|e| e.state() != ThreadState::Exited)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for ThreadRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.lock().iter()).finish()
    }
}

/// A thread's link to its entry while it runs: installed in the thread-local, removed
/// with the entry on drop (also when the thread unwinds).
struct Attached {
    registry: Arc<ThreadRegistry>,
    entry: Arc<Entry>,
}

impl Attached {
    /// Called on the thread itself.
    fn enter(self) -> Self {
        self.entry.attach_os_thread();
        self.entry.set_state(ThreadState::Busy);
        CURRENT.with_borrow_mut(|current| *current = Some(self.entry.clone()));
        self
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        CURRENT.with_borrow_mut(|current| {
            if current.as_ref().is_some_and(|e| e.id == self.entry.id) {
                *current = None;
            }
        });
        self.entry.set_state(ThreadState::Exited);
        self.registry.deregister(self.entry.id);
    }
}

/// Keeps the calling thread registered (see [`ThreadRegistry::register_current`]). Drop
/// it on the thread that created it.
pub struct Registered {
    attached: Option<Attached>,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Registered {
    /// The registry id of this thread.
    pub fn id(&self) -> u64 {
        self.attached.as_ref().map_or(0, |a| a.entry.id)
    }
}

impl fmt::Debug for Registered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Registered").field(&self.id()).finish()
    }
}

struct HookInner {
    entry: Arc<Entry>,
}

impl Drop for HookInner {
    fn drop(&mut self) {
        // No lock here: the registry prunes the entry on its next snapshot.
        self.entry.set_state(ThreadState::Exited);
    }
}

/// The audio callback's link to its registry entry (see the module docs). Cheap to
/// clone; the entry leaves the registry when the last clone is dropped. `enter` and
/// `exit` must not run on two threads at once (one callback at a time, as audio
/// backends guarantee).
#[derive(Clone)]
pub struct AudioCallbackHook {
    inner: Arc<HookInner>,
}

impl AudioCallbackHook {
    /// At the start of each callback: records the calling OS thread and marks it busy.
    /// Allocation-free and lock-free.
    pub fn enter(&self) {
        let entry = &self.inner.entry;
        entry.attach_os_thread();
        entry.set_state(ThreadState::Busy);
    }

    /// At the end of each callback: marks the thread idle until the next one.
    pub fn exit(&self) {
        self.inner.entry.set_state(ThreadState::Idle);
    }

    /// Runs one callback body between [`enter`](Self::enter) and [`exit`](Self::exit).
    pub fn run<R>(&self, callback: impl FnOnce() -> R) -> R {
        self.enter();
        let result = callback();
        self.exit();
        result
    }

    /// The registry id of the callback's entry.
    pub fn id(&self) -> u64 {
        self.inner.entry.id
    }
}

impl fmt::Debug for AudioCallbackHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AudioCallbackHook")
            .field(&self.inner.entry)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_role() {
        assert_eq!(Role::Decode.thread_name(Some(2)), "glass-decode-2");
        assert_eq!(Role::Scheduler.thread_name(None), "glass-sched");
        assert_eq!(Role::Ui.thread_name(None), "main");
    }

    #[test]
    fn a_spawned_thread_is_listed_until_it_returns() {
        let registry = ThreadRegistry::new();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let handle = registry
            .spawn(Role::Io, None, move || {
                let name = std::thread::current().name().map(str::to_owned);
                idle(|| wait.recv().ok());
                name
            })
            .expect("spawn");
        let listed = registry.snapshot();
        assert_eq!(listed.len(), 1);
        assert_eq!(&*listed[0].name, "glass-io");
        go.send(()).expect("send");
        assert_eq!(handle.join().expect("join").as_deref(), Some("glass-io"));
        assert!(registry.is_empty());
    }

    #[test]
    fn a_panicking_thread_leaves_the_registry() {
        let registry = ThreadRegistry::new();
        let handle = registry
            .spawn(Role::Export, None, || panic!("deliberate"))
            .expect("spawn");
        assert!(handle.join().is_err());
        assert!(registry.is_empty());
    }

    #[test]
    fn the_current_thread_registers_and_leaves() {
        let registry = ThreadRegistry::new();
        let guard = registry.register_current(Role::Ui, None);
        let listed = registry.snapshot();
        assert_eq!(listed[0].role, Role::Ui);
        assert_eq!(listed[0].state, ThreadState::Busy);
        assert!(listed[0].os_id.is_some());
        blocked(|| std::thread::sleep(Duration::from_millis(5)));
        let after = registry.snapshot();
        assert!(after[0].blocked >= Duration::from_millis(5));
        drop(guard);
        assert!(registry.is_empty());
    }

    #[test]
    fn waits_on_an_unregistered_thread_do_nothing() {
        let value = std::thread::spawn(|| idle(|| blocked(|| 7)))
            .join()
            .expect("join");
        assert_eq!(value, 7);
    }
}
