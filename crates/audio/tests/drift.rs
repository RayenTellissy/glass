//! Drift correction against a simulated device whose crystal runs 0.1% fast or slow,
//! whose callbacks wake up late by up to 1 ms, and whose buffer size varies.

use std::time::{Duration, Instant};

use audio::drift::{DriftEstimator, MAX_SLEW};
use audio::{OfflineOutput, OutputConfig};

const RATE: u32 = 48_000;
/// Reported output latency of the simulated device, in seconds.
const LATENCY: f64 = 0.012;

/// A small deterministic generator for callback jitter (no dev-dependency for this).
struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One simulated callback: when it ran (as the host saw it) and the first frame it wrote.
struct Callback {
    now: f64,
    frames: u64,
}

/// Callbacks of a device playing `seconds` at `RATE * ratio` (true frames per second of
/// monotonic time), buffers of 256 to 1024 frames, each woken 0 to 1 ms late.
fn simulate(ratio: f64, seconds: f64, seed: u64) -> Vec<Callback> {
    let true_rate = f64::from(RATE) * ratio;
    let mut rng = Lcg(seed);
    let mut callbacks = Vec::new();
    let mut frames = 0u64;
    while (frames as f64) / true_rate < seconds {
        // The frame becomes audible at `frames / true_rate + LATENCY`; the host calls back
        // `LATENCY` before that, plus wake-up jitter.
        let ideal = frames as f64 / true_rate;
        callbacks.push(Callback {
            now: ideal + rng.unit() * 0.001,
            frames,
        });
        frames += [256, 512, 512, 1024][(rng.unit() * 4.0) as usize % 4];
    }
    callbacks
}

/// True frames audible at monotonic time `t`.
fn truth(t: f64, ratio: f64) -> f64 {
    (t - LATENCY) * f64::from(RATE) * ratio
}

struct Report {
    /// Largest position error after `settle`, in milliseconds.
    max_error_ms: f64,
    /// Rate error at the end, in ppm.
    rate_error_ppm: f64,
    /// Smallest and largest slope over any 1 ms step, relative to the true rate.
    slope_range: (f64, f64),
}

fn run(ratio: f64, seconds: f64, settle: f64, seed: u64) -> Report {
    let callbacks = simulate(ratio, seconds, seed);
    let mut estimator = DriftEstimator::new(RATE);
    let true_rate = f64::from(RATE) * ratio;
    let mut max_error = 0.0f64;
    let mut slope_range = (f64::MAX, f64::MIN);
    let mut previous: Option<(f64, f64)> = None;
    for (i, cb) in callbacks.iter().enumerate() {
        // The host reports the latency; the callback's own wake-up delay is invisible.
        estimator.observe(cb.now + LATENCY, cb.frames, cb.now);
        let line = estimator.line();
        let end = callbacks.get(i + 1).map_or(cb.now + 0.01, |next| next.now);
        // Sample the published line every 1 ms (plus the update instant itself) until the
        // next callback replaces it, as readers would.
        let mut t = cb.now;
        while t < end {
            let frames = line.frames_at(t);
            if let Some((pt, pf)) = previous
                && t - pt > 1e-6
            {
                let slope = (frames - pf) / (t - pt) / true_rate;
                slope_range = (slope_range.0.min(slope), slope_range.1.max(slope));
            }
            previous = Some((t, frames));
            if t > settle {
                let error = (frames - truth(t, ratio)) / true_rate;
                max_error = max_error.max(error.abs());
            }
            t += 0.001;
        }
    }
    Report {
        max_error_ms: max_error * 1000.0,
        rate_error_ppm: (estimator.rate_ratio() / ratio - 1.0) * 1e6,
        slope_range,
    }
}

#[test]
fn locks_onto_a_device_running_fast_or_slow() {
    let cases = [(1.001, 1), (0.999, 2), (1.0, 3), (1.0003, 4)];
    let reports: Vec<_> = cases
        .iter()
        .map(|&(ratio, seed)| (ratio, run(ratio, 120.0, 10.0, seed)))
        .collect();
    for (ratio, report) in &reports {
        println!(
            "ratio {ratio}: max error after 10 s {:.3} ms, rate error {:.2} ppm, slope {:.5}..{:.5}",
            report.max_error_ms, report.rate_error_ppm, report.slope_range.0, report.slope_range.1
        );
    }
    for (ratio, report) in reports {
        // Wake-up jitter is 0 to 1 ms, so a lag of its mean (0.5 ms) is expected; the rest
        // must be filtered out.
        assert!(
            report.max_error_ms < 1.0,
            "ratio {ratio}: position error {:.3} ms",
            report.max_error_ms
        );
        assert!(
            report.rate_error_ppm.abs() < 30.0,
            "ratio {ratio}: rate error {:.2} ppm",
            report.rate_error_ppm
        );
        // No jumps: over the whole run, including lock-in, the reported position moves at
        // the device rate within the slew limit (plus the loop's rate error while locking).
        let limit = MAX_SLEW + 0.0015;
        assert!(
            report.slope_range.0 > 1.0 - limit && report.slope_range.1 < 1.0 + limit,
            "ratio {ratio}: slope range {:?}",
            report.slope_range
        );
    }
}

#[test]
fn converges_within_ten_seconds() {
    // After 10 s the error is already within 1 ms; after 3 s within 3 ms.
    let early = run(1.001, 20.0, 3.0, 7);
    println!("0.1% fast, after 3 s: {:.3} ms", early.max_error_ms);
    assert!(early.max_error_ms < 3.0, "{:.3} ms", early.max_error_ms);
    let settled = run(1.001, 20.0, 10.0, 7);
    println!("0.1% fast, after 10 s: {:.3} ms", settled.max_error_ms);
    assert!(settled.max_error_ms < 1.0, "{:.3} ms", settled.max_error_ms);
}

#[test]
fn a_stall_resyncs_once() {
    let mut estimator = DriftEstimator::new(RATE);
    let mut now = 0.0;
    let mut frames = 0u64;
    for _ in 0..1000 {
        estimator.observe(now + LATENCY, frames, now);
        now += 512.0 / f64::from(RATE);
        frames += 512;
    }
    // The device stops for half a second without its frame counter moving.
    now += 0.5;
    for _ in 0..100 {
        estimator.observe(now + LATENCY, frames, now);
        now += 512.0 / f64::from(RATE);
        frames += 512;
    }
    assert_eq!(estimator.resyncs(), 1);
    // Frame `frames` becomes audible `LATENCY` after `now`.
    let expected = frames as f64 - LATENCY * f64::from(RATE);
    let error = (estimator.line().frames_at(now) - expected) / f64::from(RATE);
    assert!(error.abs() < 0.001, "error {error} s");
}

/// The same drift correction end to end through the offline backend's device timing: the
/// callback path the `cpal` stream uses, read back through `Clock::at`.
#[test]
fn clock_follows_simulated_device_timing() {
    let config = OutputConfig {
        sample_rate: Some(RATE),
        ..OutputConfig::default()
    };
    let (mut output, _feeder, clock) = OfflineOutput::with_device_timing(&config);
    let base = Instant::now();
    let ratio = 1.001;
    let mut buffer = vec![0.0f32; 512 * 2];
    let callbacks = simulate(ratio, 30.0, 11);
    let mut max_error = 0.0f64;
    let mut last = 0.0;
    for (i, cb) in callbacks.iter().enumerate() {
        let frames = callbacks
            .get(i + 1)
            .map_or(512, |next| (next.frames - cb.frames) as usize);
        buffer.resize(frames * 2, 0.0);
        output.render_at(
            &mut buffer,
            base + Duration::from_secs_f64(cb.now),
            Duration::from_secs_f64(LATENCY),
        );
        let t = cb.now + 0.004;
        let secs = clock.at(base + Duration::from_secs_f64(t));
        assert!(secs >= last, "clock went back at {t}");
        last = secs;
        if t > 10.0 {
            let expected = truth(t, ratio) / f64::from(RATE);
            max_error = max_error.max((secs - expected).abs());
        }
    }
    println!(
        "offline device timing: max error {:.3} ms",
        max_error * 1000.0
    );
    assert!(max_error < 0.001, "{max_error}");
    assert!((clock.rate_ratio() / ratio - 1.0).abs() < 30e-6);
    assert!((clock.latency().as_secs_f64() - LATENCY).abs() < 1e-6);
}
