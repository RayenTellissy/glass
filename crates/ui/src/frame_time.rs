//! Frame-time monitor: UI-thread frame durations in a fixed-size ring, with percentiles
//! for the debug panel and a budget check that tests use to fail slow frames.
//!
//! Recording is a store into a preallocated ring plus two counter updates, so it can run
//! every frame without allocating. Percentiles sort a preallocated scratch copy of the
//! window when asked.

use std::fmt;
use std::time::{Duration, Instant};

/// The UI frame budget: under 8 ms, so a frame fits a 120 Hz refresh (8.33 ms).
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(8);

/// Frames kept for percentiles: about 8.5 s at 120 Hz.
pub const DEFAULT_WINDOW: usize = 1024;

/// Records frame durations and reports percentiles over the most recent window.
#[derive(Clone, Debug)]
pub struct FrameTimeMonitor {
    samples_ns: Box<[u64]>,
    scratch_ns: Box<[u64]>,
    next: usize,
    filled: usize,
    budget: Duration,
    total_frames: u64,
    over_budget: u64,
}

impl Default for FrameTimeMonitor {
    fn default() -> Self {
        Self::new(DEFAULT_WINDOW, DEFAULT_BUDGET)
    }
}

impl FrameTimeMonitor {
    /// # Panics
    /// If `window` is 0.
    pub fn new(window: usize, budget: Duration) -> Self {
        assert!(window > 0, "frame-time window must hold at least one frame");
        Self {
            samples_ns: vec![0; window].into_boxed_slice(),
            scratch_ns: vec![0; window].into_boxed_slice(),
            next: 0,
            filled: 0,
            budget,
            total_frames: 0,
            over_budget: 0,
        }
    }

    pub fn budget(&self) -> Duration {
        self.budget
    }

    pub fn set_budget(&mut self, budget: Duration) {
        self.budget = budget;
    }

    /// Records one frame's duration. Frames longer than the budget are counted.
    pub fn record(&mut self, frame: Duration) {
        let ns = u64::try_from(frame.as_nanos()).unwrap_or(u64::MAX);
        self.samples_ns[self.next] = ns;
        self.next = (self.next + 1) % self.samples_ns.len();
        self.filled = (self.filled + 1).min(self.samples_ns.len());
        self.total_frames += 1;
        if frame > self.budget {
            self.over_budget += 1;
        }
    }

    /// Runs `frame`, records how long it took, and returns its result.
    pub fn time<R>(&mut self, frame: impl FnOnce() -> R) -> R {
        let start = Instant::now();
        let result = frame();
        self.record(start.elapsed());
        result
    }

    /// Forgets every recorded frame and the counters; the budget is kept.
    pub fn reset(&mut self) {
        self.next = 0;
        self.filled = 0;
        self.total_frames = 0;
        self.over_budget = 0;
    }

    /// Percentiles over the frames in the window (nearest rank), plus the counters.
    pub fn stats(&mut self) -> FrameStats {
        let window = &mut self.scratch_ns[..self.filled];
        window.copy_from_slice(&self.samples_ns[..self.filled]);
        window.sort_unstable();
        let at = |p: f64| -> Duration {
            if window.is_empty() {
                return Duration::ZERO;
            }
            let rank = ((p / 100.0) * window.len() as f64).ceil() as usize;
            Duration::from_nanos(window[rank.clamp(1, window.len()) - 1])
        };
        FrameStats {
            frames_in_window: window.len(),
            total_frames: self.total_frames,
            over_budget: self.over_budget,
            budget: self.budget,
            p50: at(50.0),
            p95: at(95.0),
            p99: at(99.0),
            max: window
                .last()
                .map_or(Duration::ZERO, |&ns| Duration::from_nanos(ns)),
        }
    }
}

/// A summary of the frames in a [`FrameTimeMonitor`] window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameStats {
    pub frames_in_window: usize,
    /// Frames recorded since creation or the last reset.
    pub total_frames: u64,
    /// Frames over the budget since creation or the last reset.
    pub over_budget: u64,
    pub budget: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    /// The slowest frame in the window.
    pub max: Duration,
}

impl FrameStats {
    /// Passes when the window's p99 is under the budget.
    pub fn check_p99(&self) -> Result<(), BudgetExceeded> {
        if self.frames_in_window > 0 && self.p99 < self.budget {
            Ok(())
        } else {
            Err(BudgetExceeded(*self))
        }
    }

    /// For tests: panics with the full statistics unless the p99 is under the budget.
    #[track_caller]
    pub fn assert_p99_within_budget(&self) {
        if let Err(error) = self.check_p99() {
            panic!("{error}");
        }
    }
}

impl fmt::Display for FrameStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        write!(
            f,
            "{} frames: p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms, max {:.3} ms; budget {:.3} ms, {} of {} frames over",
            self.frames_in_window,
            ms(self.p50),
            ms(self.p95),
            ms(self.p99),
            ms(self.max),
            ms(self.budget),
            self.over_budget,
            self.total_frames,
        )
    }
}

/// The frame p99 was not under budget (or no frame was recorded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetExceeded(pub FrameStats);

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.frames_in_window == 0 {
            write!(
                f,
                "no frames recorded, so the frame budget cannot be checked"
            )
        } else {
            write!(f, "UI frame p99 is over budget: {}", self.0)
        }
    }
}

impl std::error::Error for BudgetExceeded {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let mut monitor = FrameTimeMonitor::new(100, ms(8));
        for v in (1..=100).rev() {
            monitor.record(ms(v) / 10);
        }
        let stats = monitor.stats();
        assert_eq!(stats.frames_in_window, 100);
        assert_eq!(stats.p50, ms(5));
        assert_eq!(stats.p95, Duration::from_micros(9_500));
        assert_eq!(stats.p99, Duration::from_micros(9_900));
        assert_eq!(stats.max, ms(10));
        // 8.1 ms through 10 ms are over an 8 ms budget.
        assert_eq!(stats.over_budget, 20);
    }

    #[test]
    fn window_keeps_the_newest_frames() {
        let mut monitor = FrameTimeMonitor::new(4, ms(8));
        for v in [20, 20, 1, 2, 3, 4] {
            monitor.record(ms(v));
        }
        let stats = monitor.stats();
        assert_eq!(stats.frames_in_window, 4);
        assert_eq!(stats.max, ms(4));
        assert_eq!(stats.p50, ms(2));
        assert_eq!(stats.total_frames, 6);
        assert_eq!(
            stats.over_budget, 2,
            "counted even after leaving the window"
        );
    }

    #[test]
    fn budget_check_is_strict_and_needs_frames() {
        let mut monitor = FrameTimeMonitor::new(8, ms(8));
        assert!(monitor.stats().check_p99().is_err(), "empty window fails");
        monitor.record(ms(7));
        assert!(monitor.stats().check_p99().is_ok());
        monitor.record(ms(8));
        let error = monitor
            .stats()
            .check_p99()
            .expect_err("p99 equal to the budget fails");
        assert!(error.to_string().contains("p99 8.000 ms"), "{error}");
        assert_eq!(monitor.stats().over_budget, 0, "8 ms itself is not over");
    }

    #[test]
    #[should_panic(expected = "UI frame p99 is over budget")]
    fn assertion_panics_over_budget() {
        let mut monitor = FrameTimeMonitor::default();
        monitor.record(ms(12));
        monitor.stats().assert_p99_within_budget();
    }

    #[test]
    fn reset_clears_frames_but_keeps_the_budget() {
        let mut monitor = FrameTimeMonitor::new(4, ms(3));
        monitor.time(|| ());
        monitor.record(ms(5));
        monitor.reset();
        let stats = monitor.stats();
        assert_eq!(
            (
                stats.frames_in_window,
                stats.total_frames,
                stats.over_budget
            ),
            (0, 0, 0)
        );
        assert_eq!(stats.budget, ms(3));
    }
}
