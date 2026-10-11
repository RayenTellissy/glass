//! Adaptive preview quality: under load, playback lowers the preview resolution before
//! it drops a frame, and goes back to full resolution when playback stops.
//!
//! [`QualityGovernor`] is a pure state machine over per-frame timings, so a synthetic
//! timeline of frame costs drives it exactly as playback does. For every presented frame
//! the caller reports a [`FrameTiming`]: the time the frame had before its deadline (one
//! frame interval of the clock), how long render waited for its decoded input past that
//! start (decode lateness) and how long rendering took. Their sum is the frame's cost and
//! `1 - cost / budget` its headroom; a frame whose cost exceeds its budget missed.
//!
//! - **Step down** one [`QualityLevel`] (full, half, quarter resolution) when the rolling
//!   window of the last [`GovernorConfig::window`] frames at the current level holds
//!   [`GovernorConfig::miss_limit`] misses, or, once full, its mean headroom is under
//!   [`GovernorConfig::step_down_headroom`]. The window starts empty at every change, so
//!   frames rendered at the old level never judge the new one.
//! - **Step up** one level only after the window has shown no miss and a mean headroom of
//!   at least [`GovernorConfig::step_up_headroom`] for [`GovernorConfig::step_up_after`]
//!   of frame time in a row. Between the two thresholds nothing changes. A step up that
//!   steps back down within [`GovernorConfig::probe_window`] doubles the wait before the
//!   next try at that level (up to [`GovernorConfig::max_step_up_after`]), so a load the
//!   higher level cannot carry costs fewer and fewer probes instead of a steady flicker.
//! - **Restore** to full at once when playback stops ([`QualityGovernor::idle`]), which
//!   also forgets the timing history and the back-off; the caller re-renders the frame on
//!   screen at full resolution.
//! - **Drop** a frame ([`QualityGovernor::admit`]) only at the lowest level, when the last
//!   frame at that level still missed and the frames so far have fallen at least one
//!   frame interval behind the clock. Above the lowest level a late frame is shown late
//!   and the level steps down instead.
//!
//! Decode lateness counts in the cost although a smaller preview does not decode faster:
//! a decoder that cannot keep up walks the level down to the lowest, where frames are
//! dropped, which is what a decode-bound stream needs.

use std::collections::VecDeque;
use std::time::Duration;

use render::transform::Resample;

/// Preview resolution steps, from full to a quarter of the canvas on each axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QualityLevel {
    #[default]
    Full,
    Half,
    Quarter,
}

impl QualityLevel {
    /// The lowest level: the only one that drops frames.
    pub const LOWEST: Self = Self::Quarter;

    /// The canvas is divided by this on each axis.
    pub fn divisor(self) -> u32 {
        match self {
            Self::Full => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }

    /// The preview size for a `canvas`: each side divided by [`Self::divisor`], rounded
    /// up, at least 1.
    pub fn preview_size(self, canvas: (u32, u32)) -> (u32, u32) {
        let d = self.divisor();
        (canvas.0.div_ceil(d).max(1), canvas.1.div_ceil(d).max(1))
    }

    /// The composite's resampling cap at this level (Decision 23): reduced levels sample
    /// transformed layers bilinear, full resolution keeps the default bicubic.
    pub fn max_resample(self) -> Resample {
        match self {
            Self::Full => Resample::Bicubic,
            Self::Half | Self::Quarter => Resample::Bilinear,
        }
    }

    /// The frame cache's `RenderVariant::quality` step for frames rendered at this level.
    pub fn cache_step(self) -> u8 {
        self as u8
    }

    fn lower(self) -> Option<Self> {
        match self {
            Self::Full => Some(Self::Half),
            Self::Half => Some(Self::Quarter),
            Self::Quarter => None,
        }
    }

    fn higher(self) -> Option<Self> {
        match self {
            Self::Full => None,
            Self::Half => Some(Self::Full),
            Self::Quarter => Some(Self::Half),
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn from_index(index: u8) -> Self {
        match index {
            0 => Self::Full,
            1 => Self::Half,
            _ => Self::Quarter,
        }
    }
}

/// One presented frame's timing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameTiming {
    /// The time the frame had: from when render could start it to its deadline on the
    /// clock (one frame interval in steady playback).
    pub budget: Duration,
    /// How long render waited for the frame's decoded input past that start.
    pub decode_late: Duration,
    /// How long rendering took.
    pub render: Duration,
}

impl FrameTiming {
    pub fn cost(&self) -> Duration {
        self.decode_late + self.render
    }

    /// `1 - cost / budget`: 1 for a free frame, 0 for one that used all its time,
    /// negative for a miss.
    pub fn headroom(&self) -> f64 {
        let budget = self.budget.as_secs_f64();
        if budget <= 0.0 {
            return 0.0;
        }
        1.0 - self.cost().as_secs_f64() / budget
    }

    pub fn missed(&self) -> bool {
        self.cost() > self.budget
    }
}

/// Thresholds of a [`QualityGovernor`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GovernorConfig {
    /// Frames in the rolling window.
    pub window: usize,
    /// Misses in the window that step down at once.
    pub miss_limit: usize,
    /// A full window whose mean headroom is under this steps down.
    pub step_down_headroom: f64,
    /// Mean headroom the window must hold, with no miss, before stepping up.
    pub step_up_headroom: f64,
    /// Frame time the step-up condition must hold in a row.
    pub step_up_after: Duration,
    /// A step down within this much frame time of stepping up doubles the wait before
    /// the next step up to that level.
    pub probe_window: Duration,
    /// The longest wait before a step up.
    pub max_step_up_after: Duration,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            window: 12,
            miss_limit: 2,
            step_down_headroom: 0.10,
            step_up_headroom: 0.35,
            step_up_after: Duration::from_secs(1),
            probe_window: Duration::from_secs(1),
            max_step_up_after: Duration::from_secs(16),
        }
    }
}

/// What to do with the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Render it at this level.
    Render(QualityLevel),
    /// Skip it: the lowest level is still late.
    Drop,
}

/// A level change the governor made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelChange {
    pub from: QualityLevel,
    pub to: QualityLevel,
}

/// Counters for tests, benchmarks and the debug panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QualityStats {
    pub level: QualityLevel,
    /// Frames reported through [`QualityGovernor::record`].
    pub frames: u64,
    /// Reported frames that missed their deadline.
    pub misses: u64,
    pub steps_down: u64,
    pub steps_up: u64,
    /// Returns to full resolution on idle (only those that changed the level).
    pub restores: u64,
    pub drops: u64,
}

impl QualityStats {
    /// Every level change: steps down and up, and restores.
    pub fn level_changes(&self) -> u64 {
        self.steps_down + self.steps_up + self.restores
    }
}

/// The adaptive quality state machine. See the [module](self) docs.
#[derive(Clone, Debug)]
pub struct QualityGovernor {
    config: GovernorConfig,
    level: QualityLevel,
    /// (headroom, missed) of the latest frames at this level, oldest first.
    window: VecDeque<(f64, bool)>,
    /// Frame time the step-up condition has held.
    calm: Duration,
    /// The wait before stepping up into each level, by index.
    step_up_after: [Duration; 3],
    /// After a step up: frame time spent at the new level, while under `probe_window`.
    probing: Option<Duration>,
    /// How far the frames are behind the clock.
    behind: Duration,
    /// The latest frame's budget, the interval a drop catches up.
    last_budget: Duration,
    last_missed: bool,
    stats: QualityStats,
}

impl Default for QualityGovernor {
    fn default() -> Self {
        Self::new(GovernorConfig::default())
    }
}

impl QualityGovernor {
    pub fn new(config: GovernorConfig) -> Self {
        Self {
            config,
            level: QualityLevel::Full,
            window: VecDeque::with_capacity(config.window.max(1)),
            calm: Duration::ZERO,
            step_up_after: [config.step_up_after; 3],
            probing: None,
            behind: Duration::ZERO,
            last_budget: Duration::ZERO,
            last_missed: false,
            stats: QualityStats::default(),
        }
    }

    pub fn level(&self) -> QualityLevel {
        self.level
    }

    pub fn stats(&self) -> QualityStats {
        QualityStats {
            level: self.level,
            ..self.stats
        }
    }

    /// How far the frames reported so far are behind the clock.
    pub fn behind(&self) -> Duration {
        self.behind
    }

    /// Decides the next frame: render it at the current level, or drop it when the
    /// lowest level is still late and a whole frame interval behind. A drop catches up
    /// one interval.
    pub fn admit(&mut self) -> Admit {
        let late = self.last_missed && self.behind >= self.last_budget;
        if self.level == QualityLevel::LOWEST && late && !self.last_budget.is_zero() {
            self.behind -= self.last_budget;
            self.stats.drops += 1;
            return Admit::Drop;
        }
        Admit::Render(self.level)
    }

    /// Records a presented frame rendered at the current level; returns the level change
    /// it caused, if any.
    pub fn record(&mut self, timing: FrameTiming) -> Option<LevelChange> {
        let headroom = timing.headroom();
        let missed = timing.missed();
        self.stats.frames += 1;
        self.stats.misses += u64::from(missed);
        self.last_budget = timing.budget;
        self.last_missed = missed;
        self.behind = (self.behind + timing.cost()).saturating_sub(timing.budget);

        let capacity = self.config.window.max(1);
        if self.window.len() == capacity {
            self.window.pop_front();
        }
        self.window.push_back((headroom, missed));
        if let Some(spent) = &mut self.probing {
            *spent += timing.budget;
            if *spent > self.config.probe_window {
                self.probing = None;
            }
        }

        let misses = self.window.iter().filter(|(_, m)| *m).count();
        let full = self.window.len() == capacity;
        let mean = self.window.iter().map(|(h, _)| h).sum::<f64>() / self.window.len() as f64;

        if misses >= self.config.miss_limit.max(1)
            || (full && mean < self.config.step_down_headroom)
        {
            self.calm = Duration::ZERO;
            let lower = self.level.lower()?;
            if self.probing.take().is_some() {
                let wait = &mut self.step_up_after[self.level.index()];
                *wait = (*wait * 2).min(self.config.max_step_up_after);
            }
            self.stats.steps_down += 1;
            return Some(self.change(lower));
        }

        if full && misses == 0 && mean >= self.config.step_up_headroom {
            self.calm += timing.budget;
        } else {
            self.calm = Duration::ZERO;
        }
        let higher = self.level.higher()?;
        if self.calm >= self.step_up_after[higher.index()] {
            self.calm = Duration::ZERO;
            self.probing = Some(Duration::ZERO);
            self.stats.steps_up += 1;
            return Some(self.change(higher));
        }
        None
    }

    /// Playback stopped: back to full resolution, with the timing history and the
    /// back-off forgotten. Returns the change when the level was reduced, and the caller
    /// re-renders the frame on screen at full resolution.
    pub fn idle(&mut self) -> Option<LevelChange> {
        self.reset_timing();
        self.step_up_after = [self.config.step_up_after; 3];
        if self.level == QualityLevel::Full {
            return None;
        }
        self.stats.restores += 1;
        Some(self.change(QualityLevel::Full))
    }

    /// Forgets the timing history (window, calm run, how far behind) but keeps the
    /// level: for a seek or a new start of playback, whose first frames say nothing
    /// about steady load.
    pub fn reset_timing(&mut self) {
        self.window.clear();
        self.calm = Duration::ZERO;
        self.probing = None;
        self.behind = Duration::ZERO;
        self.last_missed = false;
    }

    fn change(&mut self, to: QualityLevel) -> LevelChange {
        let from = self.level;
        self.level = to;
        self.window.clear();
        self.calm = Duration::ZERO;
        // Only a miss at the new level makes it late.
        self.last_missed = false;
        LevelChange { from, to }
    }
}
