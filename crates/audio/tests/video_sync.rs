//! Video slaved to the clock: cadence at common frame and refresh rates, one repeated or
//! skipped frame per frame of drift, and no flicker between two frames from jitter.

use audio::video_sync::{HYSTERESIS, VideoSync};

/// A small deterministic generator for presentation-time jitter.
struct Lcg(u64);

impl Lcg {
    /// Uniform in -1..1.
    fn signed(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        2.0 * ((self.0 >> 11) as f64 / (1u64 << 53) as f64) - 1.0
    }
}

/// The frames picked for `refreshes` refreshes at `hz`, the clock running at `ratio` of
/// real time from `start` seconds, each reading off by up to `jitter` seconds.
fn pick(
    sync: &mut VideoSync,
    hz: f64,
    refreshes: usize,
    ratio: f64,
    start: f64,
    jitter: f64,
) -> Vec<i64> {
    let mut rng = Lcg(5);
    (0..refreshes)
        .map(|k| {
            let secs = start + k as f64 / hz * ratio + rng.signed() * jitter;
            sync.frame_at(secs, 1.0 / hz)
        })
        .collect()
}

/// How many refreshes each frame stays on screen, in order.
fn holds(frames: &[i64]) -> Vec<usize> {
    let mut holds = Vec::new();
    let mut run = 1;
    for pair in frames.windows(2) {
        if pair[1] == pair[0] {
            run += 1;
        } else {
            holds.push(run);
            run = 1;
        }
    }
    holds
}

#[test]
fn thirty_at_sixty_keeps_an_even_cadence_through_jitter() {
    // The worst phase: the middle of every other refresh lands on a frame boundary, with
    // 1 ms of jitter either way.
    let mut sync = VideoSync::new(30, 1);
    let frames = pick(&mut sync, 60.0, 600, 1.0, -1.0 / 120.0, 0.001);
    let holds = holds(&frames);
    assert!(holds[1..].iter().all(|&h| h == 2), "holds {holds:?}");
    assert!(frames.windows(2).all(|p| p[1] - p[0] <= 1));
    assert_eq!((sync.repeats(), sync.skips()), (0, 0));
}

#[test]
fn twenty_four_at_sixty_is_three_two_pulldown() {
    let mut sync = VideoSync::new(24, 1);
    let frames = pick(&mut sync, 60.0, 600, 1.0, 0.0, 0.0005);
    let holds = holds(&frames);
    assert!(
        holds[1..]
            .windows(2)
            .all(|p| (p[0], p[1]) == (2, 3) || (p[0], p[1]) == (3, 2)),
        "holds {holds:?}"
    );
    assert_eq!((sync.repeats(), sync.skips()), (0, 0));
}

#[test]
fn drift_is_corrected_one_frame_at_a_time() {
    // 60 fps on a 60 Hz display, the audio clock 0.1% fast or slow: over 60 s the video
    // falls 3.6 frames out of step, starting on a boundary, with 1 ms of jitter.
    for (ratio, fast) in [(1.001, true), (0.999, false)] {
        let mut sync = VideoSync::new(60, 1);
        let frames = pick(&mut sync, 60.0, 3600, ratio, -1.0 / 120.0, 0.001);
        let steps: Vec<i64> = frames.windows(2).map(|p| p[1] - p[0]).collect();
        let (corrections, wrong) = if fast {
            (sync.skips(), sync.repeats())
        } else {
            (sync.repeats(), sync.skips())
        };
        println!("ratio {ratio}: {corrections} corrections");
        assert!(
            (3..=4).contains(&corrections),
            "ratio {ratio}: {corrections}"
        );
        assert_eq!(wrong, 0, "ratio {ratio}: corrections the wrong way");
        assert!(steps.iter().all(|&s| (0..=2).contains(&s)));
        // Each correction is isolated: no repeat right after a skip or the other way round.
        for pair in steps.windows(2) {
            assert!(
                !(pair[0] != 1 && pair[1] != 1),
                "ratio {ratio}: flicker {pair:?}"
            );
        }
        // The shown frame covers the clock's position, give or take the hysteresis band
        // and the jitter (0.06 frame).
        for (k, &frame) in frames.iter().enumerate() {
            let exact = k as f64 * ratio;
            let off = (frame as f64 + 0.5 - exact).abs();
            assert!(
                off <= 0.5 + HYSTERESIS + 0.06,
                "refresh {k}: {frame} for {exact}"
            );
        }
    }
}

#[test]
fn reset_follows_a_seek() {
    let mut sync = VideoSync::new(25, 1);
    assert_eq!(sync.frame_at(0.0, 1.0 / 50.0), 0);
    sync.reset();
    assert_eq!(sync.frame_at(10.0, 1.0 / 50.0), 250);
    assert_eq!((sync.repeats(), sync.skips()), (0, 0));
}
