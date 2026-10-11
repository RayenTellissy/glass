//! Frame-budget runs for the UI tests: each headless frame's wall-clock and UI-thread CPU
//! time, held to the frame budget by the shared timing rules (`timing`, Decision 64): the
//! medians on both clocks and the CPU p99 at twice the budget by default, the p99 on both
//! clocks under `GLASS_STRICT_TIMING=1`.

#![allow(dead_code)]

#[path = "../../../render/tests/common/timing.rs"]
pub mod timing;

use std::fmt;
use std::sync::MutexGuard;
use std::time::Duration;

use ui::frame_time::{FrameStats, FrameTimeMonitor};
use ui::headless::{BudgetCheck, HeadlessError, HeadlessUi};

/// Frames timed on the wall clock and the UI thread's CPU clock. Holds
/// [`timing::serial`] while it lives, so no other timed section in the test binary runs
/// alongside.
pub struct FrameBudget {
    wall: FrameTimeMonitor,
    cpu: FrameTimeMonitor,
    _serial: MutexGuard<'static, ()>,
}

impl Default for FrameBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameBudget {
    pub fn new() -> Self {
        Self {
            _serial: timing::serial(),
            wall: FrameTimeMonitor::default(),
            cpu: FrameTimeMonitor::default(),
        }
    }

    /// Runs one frame and records it; returns its wall-clock time.
    pub fn frame(
        &mut self,
        headless: &mut HeadlessUi,
        draw: impl FnMut(&mut egui::Ui),
    ) -> Result<Duration, HeadlessError> {
        let (wall, lap) = timing::time(|| headless.frame(draw));
        let wall = wall?;
        self.wall.record(wall);
        self.cpu.record(lap.cpu);
        Ok(wall)
    }

    /// Runs `warmup` untimed frames, then `frames` recorded ones; the frame index is
    /// passed to `draw`.
    pub fn run(
        headless: &mut HeadlessUi,
        warmup: usize,
        frames: usize,
        mut draw: impl FnMut(&mut egui::Ui, usize),
    ) -> Result<FrameReport, HeadlessError> {
        let mut budget = Self::new();
        for i in 0..warmup + frames {
            if i < warmup {
                headless.frame(|ui| draw(ui, i))?;
            } else {
                budget.frame(headless, |ui| draw(ui, i))?;
            }
        }
        Ok(budget.report())
    }

    pub fn report(&mut self) -> FrameReport {
        FrameReport {
            wall: self.wall.stats(),
            cpu: self.cpu.stats(),
        }
    }
}

/// The recorded frames' statistics.
#[derive(Clone, Copy, Debug)]
pub struct FrameReport {
    pub wall: FrameStats,
    /// UI-thread CPU time (the wall-clock time where the platform has no thread clock).
    pub cpu: FrameStats,
}

impl FrameReport {
    /// Holds the frames to the budget on a hardware adapter; on a software one prints
    /// why the budget is not judged and returns.
    #[track_caller]
    pub fn assert_within_budget(&self, headless: &HeadlessUi, what: &str) {
        if let BudgetCheck::Skip(reason) = headless.budget_check() {
            eprintln!(
                "BUDGET NOT CHECKED {what}: {reason} (wall p99 {:?})",
                self.wall.p99
            );
            return;
        }
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        if self.wall.frames_in_window == 0 {
            panic!("{what}: no frames recorded, so the frame budget cannot be checked");
        }
        let figures = timing::Figures {
            cpu_median: ms(self.cpu.p50),
            cpu: ms(self.cpu.p99),
            wall_median: ms(self.wall.p50),
            wall: ms(self.wall.p99),
        };
        if let Err(e) = timing::check_cpu_section(timing::Stat::P99, figures, ms(self.wall.budget))
        {
            panic!("{what}: UI frame {e}\n  {self}");
        }
    }
}

impl fmt::Display for FrameReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wall: {}; UI-thread CPU: {}", self.wall, self.cpu)
    }
}
