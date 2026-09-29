//! Logging: `tracing` with per-crate filters, written to stderr and kept in an in-memory
//! ring of the most recent events that the debug panel reads.
//!
//! `init` is called once by each binary (`glass`, `smoke`). The filter comes from the
//! `GLASS_LOG` environment variable in `EnvFilter` syntax (`GLASS_LOG=engine=debug,info`),
//! or [`DEFAULT_DIRECTIVES`] when it is unset or invalid. `log` crate records (wgpu,
//! winit, eframe) are routed into `tracing`, so the same filter covers them.
//!
//! The audio callback must never log: recording an event formats strings, allocates and
//! briefly takes a slot lock. Audio code reports through its lock-free rings to another
//! thread, which may log on its behalf. Every other thread may log; writers only contend
//! when two of them land on the same ring slot, so logging never waits on the UI or on a
//! reader for longer than one event copy.

use std::fmt::{self, Write as _};
use std::io::IsTerminal as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer};

/// The environment variable holding the log filter.
pub const ENV_VAR: &str = "GLASS_LOG";

/// Filter used when `GLASS_LOG` is unset: `info` for Glass, quieter for chatty
/// dependencies.
pub const DEFAULT_DIRECTIVES: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn,winit=warn";

/// Events kept in memory by [`init`].
pub const DEFAULT_RING_CAPACITY: usize = 10_000;

/// One recorded event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEvent {
    /// Position in the stream of all events recorded by this ring, starting at 0.
    pub seq: u64,
    pub time: SystemTime,
    pub level: Level,
    /// The module path or explicit `target:` of the event, for example `engine::sched`.
    pub target: &'static str,
    pub thread: Option<String>,
    pub message: String,
    /// The other fields as `name=value` pairs separated by spaces.
    pub fields: String,
}

/// Which events a read returns. The default returns everything.
#[derive(Clone, Debug, Default)]
pub struct LogFilter {
    /// Keep events at this level or more severe (`WARN` keeps warnings and errors).
    pub min_level: Option<Level>,
    /// Keep events whose target is this module or one of its children
    /// (`engine` matches `engine` and `engine::sched`, not `engine_x`).
    pub target: Option<String>,
}

impl LogFilter {
    pub fn matches(&self, event: &LogEvent) -> bool {
        // tracing orders levels by verbosity: ERROR < WARN < INFO < DEBUG < TRACE.
        let level_ok = self.min_level.is_none_or(|min| event.level <= min);
        let target_ok = self.target.as_deref().is_none_or(|prefix| {
            event.target == prefix
                || event
                    .target
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with("::"))
        });
        level_ok && target_ok
    }
}

/// A fixed-capacity ring of recent events. When full, each new event replaces the
/// oldest. Writers reserve a sequence number atomically and lock only the one slot they
/// write, so concurrent writers and readers rarely touch the same lock.
pub struct LogRing {
    slots: Box<[Mutex<Option<LogEvent>>]>,
    next: AtomicU64,
}

impl LogRing {
    /// # Panics
    /// If `capacity` is 0.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "log ring capacity must be at least 1");
        Self {
            slots: (0..capacity).map(|_| Mutex::new(None)).collect(),
            next: AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Events recorded since creation, including those already overwritten.
    pub fn total(&self) -> u64 {
        self.next.load(Ordering::Acquire)
    }

    /// Events overwritten because the ring was full.
    pub fn dropped(&self) -> u64 {
        self.total().saturating_sub(self.slots.len() as u64)
    }

    /// Events currently held, oldest first, that match `filter`.
    pub fn snapshot(&self, filter: &LogFilter) -> Vec<LogEvent> {
        self.since(0, filter)
    }

    /// Held events with `seq >= from` that match `filter`, oldest first. A reader that
    /// polls keeps the last `seq` it saw plus one and passes it here, so each poll only
    /// copies new events.
    pub fn since(&self, from: u64, filter: &LogFilter) -> Vec<LogEvent> {
        let end = self.total();
        let start = end.saturating_sub(self.slots.len() as u64).max(from);
        let mut out = Vec::with_capacity((end.saturating_sub(start)) as usize);
        for seq in start..end {
            let slot = self
                .slot(seq)
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // A slot can hold a newer event (a writer lapped us) or none yet (its writer
            // has reserved the number but not stored it); both are skipped.
            if let Some(event) = slot.as_ref().filter(|e| e.seq == seq)
                && filter.matches(event)
            {
                out.push(event.clone());
            }
        }
        out
    }

    fn push(&self, mut event: LogEvent) {
        let seq = self.next.fetch_add(1, Ordering::AcqRel);
        event.seq = seq;
        let mut slot = self
            .slot(seq)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // A writer that stalled for a whole lap must not overwrite a newer event.
        if slot.as_ref().is_none_or(|old| old.seq < seq) {
            *slot = Some(event);
        }
    }

    fn slot(&self, seq: u64) -> &Mutex<Option<LogEvent>> {
        &self.slots[(seq % self.slots.len() as u64) as usize]
    }
}

impl fmt::Debug for LogRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogRing")
            .field("capacity", &self.capacity())
            .field("total", &self.total())
            .finish()
    }
}

/// A `tracing` layer that records every event it sees into a [`LogRing`].
#[derive(Clone, Debug)]
pub struct RingLayer {
    ring: Arc<LogRing>,
}

impl RingLayer {
    pub fn new(ring: Arc<LogRing>) -> Self {
        Self { ring }
    }
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.ring.push(LogEvent {
            seq: 0,
            time: SystemTime::now(),
            level: *meta.level(),
            target: meta.target(),
            thread: std::thread::current().name().map(str::to_owned),
            message: visitor.message,
            fields: visitor.fields,
        });
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: String,
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.push_field(field, format_args!("{value}"));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            self.push_field(field, format_args!("{value:?}"));
        }
    }
}

impl FieldVisitor {
    fn push_field(&mut self, field: &Field, value: fmt::Arguments<'_>) {
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        let _ = write!(self.fields, "{}={}", field.name(), value);
    }
}

/// Why [`init`] failed.
#[derive(Debug)]
pub enum InitError {
    /// A global subscriber was already installed (by an earlier `init` or a test harness).
    AlreadyInitialised,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialised => f.write_str("a global tracing subscriber is already set"),
        }
    }
}

impl std::error::Error for InitError {}

static RING: OnceLock<Arc<LogRing>> = OnceLock::new();

/// Installs the global subscriber: the `GLASS_LOG` filter, a stderr writer and the
/// in-memory ring. Returns the ring, which [`ring`] also returns from anywhere later.
pub fn init() -> Result<Arc<LogRing>, InitError> {
    let (filter, bad_filter) = filter_from(std::env::var(ENV_VAR).ok().as_deref());
    let ring = Arc::new(LogRing::new(DEFAULT_RING_CAPACITY));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(std::io::stderr().is_terminal()),
        )
        .with(RingLayer::new(ring.clone()))
        .try_init()
        .map_err(|_| InitError::AlreadyInitialised)?;
    let ring = RING.get_or_init(|| ring).clone();
    if let Some(error) = bad_filter {
        tracing::warn!(%error, "invalid {ENV_VAR}, using the default filter");
    }
    Ok(ring)
}

/// The ring installed by [`init`], if it has run.
pub fn ring() -> Option<Arc<LogRing>> {
    RING.get().cloned()
}

/// The filter for a `GLASS_LOG` value, falling back to [`DEFAULT_DIRECTIVES`] when it is
/// unset, empty or invalid; the parse error is returned so it can be logged.
fn filter_from(value: Option<&str>) -> (EnvFilter, Option<String>) {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(directives) => match EnvFilter::try_new(directives) {
            Ok(filter) => (filter, None),
            Err(error) => (EnvFilter::new(DEFAULT_DIRECTIVES), Some(error.to_string())),
        },
        None => (EnvFilter::new(DEFAULT_DIRECTIVES), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::dispatcher::{self, Dispatch};
    use tracing_subscriber::Registry;

    fn dispatch(ring: &Arc<LogRing>) -> Dispatch {
        Dispatch::new(Registry::default().with(RingLayer::new(ring.clone())))
    }

    fn messages(events: &[LogEvent]) -> Vec<&str> {
        events.iter().map(|e| e.message.as_str()).collect()
    }

    #[test]
    fn wraps_around_keeping_the_newest_in_order() {
        let ring = Arc::new(LogRing::new(4));
        dispatcher::with_default(&dispatch(&ring), || {
            for i in 0..10 {
                tracing::info!("event {i}");
            }
        });
        let all = ring.snapshot(&LogFilter::default());
        assert_eq!(messages(&all), ["event 6", "event 7", "event 8", "event 9"]);
        assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), [6, 7, 8, 9]);
        assert_eq!(ring.total(), 10);
        assert_eq!(ring.dropped(), 6);
        assert_eq!(
            messages(&ring.since(8, &LogFilter::default())),
            ["event 8", "event 9"]
        );
        assert!(ring.since(10, &LogFilter::default()).is_empty());
    }

    #[test]
    fn records_level_target_thread_and_fields() {
        let ring = Arc::new(LogRing::new(8));
        std::thread::Builder::new()
            .name("glass-test".into())
            .spawn({
                let ring = ring.clone();
                move || {
                    dispatcher::with_default(&dispatch(&ring), || {
                        tracing::warn!(target: "engine::sched", frames = 3, name = "clip", "late");
                    })
                }
            })
            .and_then(|handle| handle.join().map_err(|_| std::io::Error::other("panicked")))
            .expect("logging thread");
        let event = &ring.snapshot(&LogFilter::default())[0];
        assert_eq!(event.level, Level::WARN);
        assert_eq!(event.target, "engine::sched");
        assert_eq!(event.thread.as_deref(), Some("glass-test"));
        assert_eq!(event.message, "late");
        assert_eq!(event.fields, "frames=3 name=clip");
    }

    #[test]
    fn filters_by_level_and_target() {
        let ring = Arc::new(LogRing::new(16));
        dispatcher::with_default(&dispatch(&ring), || {
            tracing::error!(target: "engine::sched", "e");
            tracing::warn!(target: "engine", "w");
            tracing::info!(target: "engine_extra", "i");
            tracing::debug!(target: "ui::timeline", "d");
            tracing::trace!(target: "engine::sched", "t");
        });
        let warn_up = LogFilter {
            min_level: Some(Level::WARN),
            target: None,
        };
        assert_eq!(messages(&ring.snapshot(&warn_up)), ["e", "w"]);
        let engine = LogFilter {
            min_level: None,
            target: Some("engine".into()),
        };
        assert_eq!(messages(&ring.snapshot(&engine)), ["e", "w", "t"]);
        let both = LogFilter {
            min_level: Some(Level::DEBUG),
            target: Some("engine::sched".into()),
        };
        assert_eq!(messages(&ring.snapshot(&both)), ["e"]);
    }

    #[test]
    fn concurrent_writers_lose_nothing_but_the_oldest() {
        const THREADS: u64 = 8;
        const PER_THREAD: u64 = 5_000;
        const CAPACITY: usize = 1_024;
        let ring = Arc::new(LogRing::new(CAPACITY));
        let dispatch = dispatch(&ring);
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let dispatch = dispatch.clone();
                scope.spawn(move || {
                    dispatcher::with_default(&dispatch, || {
                        for i in 0..PER_THREAD {
                            tracing::info!(thread = t, i, "x");
                        }
                    })
                });
            }
            // A reader running alongside the writers only ever sees ordered, distinct
            // sequence numbers.
            scope.spawn(|| {
                for _ in 0..200 {
                    let seqs: Vec<u64> = ring
                        .snapshot(&LogFilter::default())
                        .iter()
                        .map(|e| e.seq)
                        .collect();
                    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
                    assert!(seqs.len() <= CAPACITY);
                }
            });
        });
        let total = THREADS * PER_THREAD;
        assert_eq!(ring.total(), total);
        assert_eq!(ring.dropped(), total - CAPACITY as u64);
        let held = ring.snapshot(&LogFilter::default());
        let expected: Vec<u64> = (total - CAPACITY as u64..total).collect();
        assert_eq!(held.iter().map(|e| e.seq).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn filter_falls_back_to_the_default() {
        assert!(filter_from(None).1.is_none());
        assert!(filter_from(Some("  ")).1.is_none());
        assert!(filter_from(Some("engine=debug,info")).1.is_none());
        let (filter, error) = filter_from(Some("engine=notalevel"));
        assert!(error.is_some());
        assert_eq!(
            filter.to_string(),
            EnvFilter::new(DEFAULT_DIRECTIVES).to_string()
        );
    }
}
