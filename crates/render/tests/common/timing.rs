//! Budget checks for timing tests (Decision 64), shared by path with the `ui` and
//! `engine` tests.
//!
//! Tests in one binary run on parallel threads and other processes share the CPU and
//! GPU, so wall-clock and GPU times grow with load that has nothing to do with the code
//! under test; an idle GPU also clocks down between timed passes. By default the checks
//! hold figures that load barely moves, so a busy machine passes while a real
//! regression still fails:
//!
//! - [`serial`] runs the timed sections of one test binary one at a time.
//! - A CPU section is timed on two clocks ([`Stopwatch`]). The medians of the measuring
//!   thread's CPU time (which does not grow while the thread waits for a core) and of
//!   wall-clock time (which a few preempted samples do not move, but a section that
//!   sleeps or blocks does) are held to the budget; the test's own statistic (p99, max)
//!   of CPU time to [`LOOSE_CPU`] times it.
//! - Wall-clock figures with no CPU clock behind them are held to the budget at the
//!   median.
//! - GPU pass times (timestamp queries) are held to [`LOOSE_GPU_FASTEST`] times the
//!   budget at the fastest tenth and to [`LOOSE_GPU_MEDIAN`] times it at the median.
//!
//! Under `GLASS_STRICT_TIMING=1`, for a quiet machine (CI's strict timing step, or a run
//! alone), each test's own statistic is held to the budget itself: the p99 or maximum on
//! both clocks, the GPU median, the mean decode time.

#![allow(dead_code)]

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Set to `1` to hold wall-clock and GPU times to their budgets at the tests' own
/// statistics, on a machine with nothing else running.
pub const STRICT_ENV: &str = "GLASS_STRICT_TIMING";

/// Outside strict runs, limits as multiples of the budget: a CPU section's own statistic
/// (p99, max) of thread CPU time,
pub const LOOSE_CPU: f64 = 2.0;
/// the fastest tenth of GPU pass times,
pub const LOOSE_GPU_FASTEST: f64 = 1.5;
/// and their median.
pub const LOOSE_GPU_MEDIAN: f64 = 3.0;

pub fn strict() -> bool {
    std::env::var(STRICT_ENV).as_deref() == Ok("1")
}

/// Runs the timed sections of this test binary one at a time: hold the guard for the
/// whole measurement. A test that panicked while holding it does not poison the rest.
pub fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The calling thread's CPU time (user and system), or `None` where there is no
/// per-thread clock fine enough to time a frame. Windows has none: `GetThreadTimes`
/// advances by the scheduler tick (15.6 ms).
pub fn thread_cpu_time() -> Option<Duration> {
    sys::thread_cpu_time()
}

#[allow(unsafe_code)]
mod sys {
    use std::time::Duration;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub fn thread_cpu_time() -> Option<Duration> {
        #[repr(C)]
        struct Timespec {
            tv_sec: i64,
            tv_nsec: i64,
        }
        #[cfg(target_os = "macos")]
        const CLOCK_THREAD_CPUTIME_ID: i32 = 16;
        #[cfg(target_os = "linux")]
        const CLOCK_THREAD_CPUTIME_ID: i32 = 3;
        unsafe extern "C" {
            fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
        }
        let mut time = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `time` is a valid, writable `timespec` (two 64-bit fields on the 64-bit
        // targets Glass builds for) and the clock id is the platform's thread CPU clock.
        let status = unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut time) };
        (status == 0).then(|| Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn thread_cpu_time() -> Option<Duration> {
        None
    }
}

/// Wall-clock and thread CPU time of one section on the calling thread.
#[derive(Clone, Copy, Debug)]
pub struct Lap {
    pub wall: Duration,
    /// The wall time where the platform has no thread CPU clock.
    pub cpu: Duration,
}

pub struct Stopwatch {
    wall: Instant,
    cpu: Option<Duration>,
}

impl Stopwatch {
    pub fn start() -> Self {
        Self {
            cpu: thread_cpu_time(),
            wall: Instant::now(),
        }
    }

    pub fn stop(&self) -> Lap {
        let wall = self.wall.elapsed();
        let cpu = match (self.cpu, thread_cpu_time()) {
            (Some(start), Some(end)) => end.saturating_sub(start),
            _ => wall,
        };
        Lap { wall, cpu }
    }
}

/// Times `section` on the calling thread.
pub fn time<R>(section: impl FnOnce() -> R) -> (R, Lap) {
    let watch = Stopwatch::start();
    let result = section();
    (result, watch.stop())
}

/// Samples in milliseconds, sorted, with nearest-rank percentiles (as
/// `ui::frame_time::FrameTimeMonitor` reports them).
#[derive(Clone, Debug)]
pub struct Samples(Vec<f64>);

impl Samples {
    /// # Panics
    /// If `ms` is empty.
    pub fn new(mut ms: Vec<f64>) -> Self {
        assert!(!ms.is_empty(), "no timing samples");
        ms.sort_by(f64::total_cmp);
        Self(ms)
    }

    pub fn from_durations(durations: impl IntoIterator<Item = Duration>) -> Self {
        Self::new(
            durations
                .into_iter()
                .map(|d| d.as_secs_f64() * 1e3)
                .collect(),
        )
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The nearest-rank percentile `p` (0 to 100).
    pub fn percentile(&self, p: f64) -> f64 {
        let rank = ((p / 100.0) * self.0.len() as f64).ceil() as usize;
        self.0[rank.clamp(1, self.0.len()) - 1]
    }

    pub fn min(&self) -> f64 {
        self.0[0]
    }

    pub fn fastest_tenth(&self) -> f64 {
        self.percentile(10.0)
    }

    /// The upper median: with an even count, the higher of the middle two.
    pub fn median(&self) -> f64 {
        self.0[self.0.len() / 2]
    }

    pub fn p99(&self) -> f64 {
        self.percentile(99.0)
    }

    pub fn max(&self) -> f64 {
        self.0[self.0.len() - 1]
    }

    pub fn mean(&self) -> f64 {
        self.0.iter().sum::<f64>() / self.0.len() as f64
    }

    pub fn stat(&self, stat: Stat) -> f64 {
        match stat {
            Stat::Median => self.median(),
            Stat::Mean => self.mean(),
            Stat::P99 => self.p99(),
            Stat::Max => self.max(),
        }
    }
}

impl std::fmt::Display for Samples {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} samples: min {:.3} ms, p10 {:.3} ms, median {:.3} ms, p99 {:.3} ms, max {:.3} ms",
            self.len(),
            self.min(),
            self.fastest_tenth(),
            self.median(),
            self.p99(),
            self.max()
        )
    }
}

/// The statistic a test holds to its budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stat {
    Median,
    Mean,
    P99,
    Max,
}

impl std::fmt::Display for Stat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Median => "median",
            Self::Mean => "mean",
            Self::P99 => "p99",
            Self::Max => "max",
        })
    }
}

/// Sections timed with [`Stopwatch`].
#[derive(Clone, Debug, Default)]
pub struct Laps(Vec<Lap>);

impl Laps {
    pub fn push(&mut self, lap: Lap) {
        self.0.push(lap);
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn wall(&self) -> Samples {
        Samples::from_durations(self.0.iter().map(|l| l.wall))
    }

    pub fn cpu(&self) -> Samples {
        Samples::from_durations(self.0.iter().map(|l| l.cpu))
    }

    /// Holds the sections to `budget_ms` with [`check_cpu_section`].
    #[track_caller]
    pub fn assert_within(&self, what: &str, stat: Stat, budget_ms: f64) {
        let (wall, cpu) = (self.wall(), self.cpu());
        let figures = Figures {
            cpu_median: cpu.median(),
            cpu: cpu.stat(stat),
            wall_median: wall.median(),
            wall: wall.stat(stat),
        };
        if let Err(e) = check_cpu_section(stat, figures, budget_ms) {
            panic!("{what}: {e}\n  wall: {wall}\n  CPU:  {cpu}");
        }
    }
}

/// A CPU section's medians and its own statistic, on both clocks, in milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Figures {
    pub cpu_median: f64,
    pub cpu: f64,
    pub wall_median: f64,
    pub wall: f64,
}

/// The check for a section timed on the CPU. By default: the medians of thread CPU time
/// and of wall-clock time under the budget, and the CPU time's `stat` under [`LOOSE_CPU`]
/// times it (a busy machine slows a thread's cores and caches about twofold). Under
/// `GLASS_STRICT_TIMING=1`: `stat` under the budget on both clocks.
pub fn check_cpu_section(stat: Stat, f: Figures, budget_ms: f64) -> Result<(), String> {
    let mut over = Vec::new();
    let mut hold = |name: &str, value: f64, limit: f64| {
        if value >= limit {
            over.push(format!("{name} {value:.3} ms (limit {limit:.3} ms)"));
        }
    };
    hold("thread CPU time median", f.cpu_median, budget_ms);
    hold("wall-clock median", f.wall_median, budget_ms);
    if strict() {
        hold(&format!("thread CPU time {stat}"), f.cpu, budget_ms);
        hold(
            &format!("wall-clock {stat} ({STRICT_ENV}=1)"),
            f.wall,
            budget_ms,
        );
    } else {
        hold(
            &format!("thread CPU time {stat}"),
            f.cpu,
            LOOSE_CPU * budget_ms,
        );
    }
    verdict(over, budget_ms)
}

/// The check for wall-clock times with no CPU clock behind them (frame lateness, decode
/// times): the median under the budget, and `stat` too under `GLASS_STRICT_TIMING=1`.
pub fn check_wall(samples: &Samples, stat: Stat, budget_ms: f64) -> Result<(), String> {
    let mut over = Vec::new();
    if samples.median() >= budget_ms {
        over.push(format!("wall-clock median {:.3} ms", samples.median()));
    }
    if strict() && samples.stat(stat) >= budget_ms {
        over.push(format!(
            "wall-clock {stat} {:.3} ms ({STRICT_ENV}=1)",
            samples.stat(stat)
        ));
    }
    verdict(over, budget_ms)
}

/// Holds wall-clock times to `budget_ms` with [`check_wall`].
#[track_caller]
pub fn assert_wall_within(what: &str, samples: &Samples, stat: Stat, budget_ms: f64) {
    if let Err(e) = check_wall(samples, stat, budget_ms) {
        panic!("{what}: {e}\n  wall: {samples}");
    }
}

/// The check for GPU pass times. By default: the fastest tenth under [`LOOSE_GPU_FASTEST`] times
/// the budget and the median under [`LOOSE_GPU_MEDIAN`] times it, since other processes'
/// work and the GPU's clock (lower when it idles between the timed passes) stretch a
/// pass by up to about 1.8 times. Under `GLASS_STRICT_TIMING=1`: the median under the
/// budget.
pub fn check_gpu(samples: &Samples, budget_ms: f64) -> Result<(), String> {
    let mut over = Vec::new();
    let mut hold = |name: &str, value: f64, limit: f64| {
        if value >= limit {
            over.push(format!("{name} {value:.3} ms (limit {limit:.3} ms)"));
        }
    };
    if strict() {
        hold(
            &format!("median ({STRICT_ENV}=1)"),
            samples.median(),
            budget_ms,
        );
    } else {
        hold(
            "fastest tenth",
            samples.fastest_tenth(),
            LOOSE_GPU_FASTEST * budget_ms,
        );
        hold("median", samples.median(), LOOSE_GPU_MEDIAN * budget_ms);
    }
    verdict(over, budget_ms)
}

/// Holds GPU pass times to `budget_ms` with [`check_gpu`].
#[track_caller]
pub fn assert_gpu_within(what: &str, samples: &Samples, budget_ms: f64) {
    if let Err(e) = check_gpu(samples, budget_ms) {
        panic!("{what}: {e}\n  GPU: {samples}");
    }
}

fn verdict(over: Vec<String>, budget_ms: f64) -> Result<(), String> {
    if over.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "over the {budget_ms:.3} ms budget: {}",
            over.join(", ")
        ))
    }
}
