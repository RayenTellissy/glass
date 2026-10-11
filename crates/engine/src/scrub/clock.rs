//! The scrubber's time source: [`MonotonicClock`] in the app, [`ManualClock`] in tests,
//! where time moves only when the test says so.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crossbeam_utils::sync::{Parker, Unparker};

/// Time since an arbitrary start, and a way for the scrub worker to sleep until a time
/// on this clock.
pub trait ScrubClock: Send + Sync + 'static {
    fn now(&self) -> Duration;

    /// Called once by the worker before it first parks, so a clock that moves on its own
    /// schedule can wake it.
    fn attach(&self, _unparker: &Unparker) {}

    /// Parks the calling thread until `deadline` on this clock (forever for `None`) or
    /// until it is unparked. May return early; callers check again.
    fn park_until(&self, parker: &Parker, deadline: Option<Duration>);
}

/// Real time from a monotonic [`Instant`].
#[derive(Clone, Copy, Debug)]
pub struct MonotonicClock {
    epoch: Instant,
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrubClock for MonotonicClock {
    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }

    fn park_until(&self, parker: &Parker, deadline: Option<Duration>) {
        match deadline {
            Some(deadline) => {
                if let Some(wait) = deadline.checked_sub(self.now()) {
                    parker.park_timeout(wait);
                }
            }
            None => parker.park(),
        }
    }
}

/// A clock that stands still until [`ManualClock::advance`], which also wakes the
/// attached worker, so tests decide exactly when a pause has lasted long enough.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: Mutex<Duration>,
    attached: Mutex<Vec<Unparker>>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&self, by: Duration) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) += by;
        for unparker in self
            .attached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            unparker.unpark();
        }
    }
}

impl ScrubClock for ManualClock {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn attach(&self, unparker: &Unparker) {
        self.attached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(unparker.clone());
    }

    fn park_until(&self, parker: &Parker, deadline: Option<Duration>) {
        if deadline.is_some_and(|deadline| deadline <= self.now()) {
            return;
        }
        // Only `advance` or the scrubber moves things on; the timeout is a safety net.
        parker.park_timeout(Duration::from_millis(100));
    }
}
