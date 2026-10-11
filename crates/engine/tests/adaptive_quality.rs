//! Adaptive quality in the playback pipeline: with a render stage too slow for the frame
//! rate, the preview resolution drops before any frame does, and pausing renders the
//! frame on screen again at full resolution.

use std::time::{Duration, Instant};

use engine::frame_queue::FrameStamp;
use engine::playback::{PassThrough, Playback, PlaybackConfig, Presented, Render};
use engine::quality::QualityLevel;
use engine::threads::ThreadRegistry;
use media::Rational;

/// 100 fps: a 10 ms budget per frame.
const FPS: i32 = 100;
const WAIT: Duration = Duration::from_secs(10);

/// Sleeps `fixed + area / divisor²` per frame: a render stage whose cost follows the
/// preview's pixel count on top of a fixed part.
struct SlowRender {
    fixed: Duration,
    area: Duration,
}

impl SlowRender {
    fn cost(&self, quality: QualityLevel) -> Duration {
        let d = quality.divisor();
        self.fixed + self.area / (d * d)
    }
}

impl Render for SlowRender {
    type Input = FrameStamp;
    type Output = QualityLevel;

    fn render(&mut self, _: &FrameStamp, _: FrameStamp, quality: QualityLevel) -> QualityLevel {
        std::thread::sleep(self.cost(quality));
        quality
    }

    fn rerender(&mut self, _: &FrameStamp, quality: QualityLevel) -> Option<QualityLevel> {
        std::thread::sleep(self.cost(quality));
        Some(quality)
    }
}

fn start(render: SlowRender) -> (Playback, Presented<QualityLevel>) {
    Playback::start(
        PlaybackConfig {
            decode_workers: 2,
            frame_duration: Rational::new(1, FPS),
            registry: ThreadRegistry::new(),
            ..PlaybackConfig::default()
        },
        |_| PassThrough,
        render,
    )
    .expect("playback")
}

/// Presented frames as (index, level), popped as soon as they are ready.
fn collect(presented: &mut Presented<QualityLevel>, until: i64) -> Vec<(i64, QualityLevel)> {
    let mut frames = Vec::new();
    let deadline = Instant::now() + WAIT;
    while frames.last().is_none_or(|(i, _)| *i < until) {
        assert!(Instant::now() < deadline, "timed out after {frames:?}");
        if let Ok(frame) = presented.pop_timeout(Duration::from_millis(50)) {
            frames.push((frame.stamp.index, frame.frame));
        }
    }
    frames
}

/// Indices missing between the first and the last presented frame.
fn gaps(frames: &[(i64, QualityLevel)]) -> Vec<i64> {
    frames
        .windows(2)
        .flat_map(|w| (w[0].0 + 1)..w[1].0)
        .collect()
}

fn count(frames: &[(i64, QualityLevel)], level: QualityLevel) -> usize {
    frames.iter().filter(|(_, l)| *l == level).count()
}

fn first_at(frames: &[(i64, QualityLevel)], level: QualityLevel) -> Option<i64> {
    frames.iter().find(|(_, l)| *l == level).map(|(i, _)| *i)
}

#[test]
fn a_slow_render_lowers_the_resolution_before_dropping_frames() {
    // Full resolution takes 24.5 ms of a 10 ms frame, half 6.5 ms, a quarter 2 ms:
    // stepping down absorbs the load, no frame drops.
    let (playback, mut presented) = start(SlowRender {
        fixed: Duration::from_micros(500),
        area: Duration::from_millis(24),
    });
    playback.play();
    let frames = collect(&mut presented, 150);
    playback.pause();
    let stats = playback.quality();
    eprintln!(
        "render 24.5 / 6.5 / 2 ms per frame at 100 fps: {} presented (full {}, half {}, quarter {}), first half frame {:?}, {} misses, {} steps down, {} up, {} dropped",
        frames.len(),
        count(&frames, QualityLevel::Full),
        count(&frames, QualityLevel::Half),
        count(&frames, QualityLevel::Quarter),
        first_at(&frames, QualityLevel::Half),
        stats.misses,
        stats.steps_down,
        stats.steps_up,
        stats.drops,
    );
    assert!(
        first_at(&frames, QualityLevel::Half).is_some_and(|i| i <= 3),
        "full resolution kept: {frames:?}"
    );
    assert!(stats.steps_down >= 1);
    assert!(gaps(&frames).is_empty(), "dropped {:?}", gaps(&frames));
    assert_eq!(stats.drops, 0);
    playback.shutdown();
}

#[test]
fn frames_drop_only_once_the_lowest_resolution_is_still_late() {
    // 30 / 18 / 15 ms per frame against 10 ms: every level misses, so the preview walks
    // down to a quarter and only then drops frames; pausing restores full resolution.
    let (playback, mut presented) = start(SlowRender {
        fixed: Duration::from_millis(14),
        area: Duration::from_millis(16),
    });
    playback.play();
    let frames = collect(&mut presented, 120);
    playback.pause();
    let stats = playback.quality();
    let gaps = gaps(&frames);
    let quarter = first_at(&frames, QualityLevel::Quarter).expect("reached a quarter");
    eprintln!(
        "render 30 / 18 / 15 ms per frame at 100 fps: {} presented (full {}, half {}, quarter {}), first half frame {:?}, first quarter frame {quarter}, first dropped frame {:?}, {} of the first {} dropped ({} in all), {} misses",
        frames.len(),
        count(&frames, QualityLevel::Full),
        count(&frames, QualityLevel::Half),
        count(&frames, QualityLevel::Quarter),
        first_at(&frames, QualityLevel::Half),
        gaps.first(),
        gaps.len(),
        frames.last().map_or(0, |(i, _)| i + 1),
        stats.drops,
        stats.misses,
    );
    assert!(!gaps.is_empty(), "nothing dropped: {frames:?}");
    assert!(
        gaps[0] > quarter,
        "dropped frame {} before the first quarter frame {quarter}",
        gaps[0]
    );
    let levels: Vec<QualityLevel> = frames.iter().map(|(_, l)| *l).collect();
    assert!(levels.is_sorted(), "the level only went down: {frames:?}");
    assert!(stats.drops >= gaps.len() as u64);
    assert_eq!(stats.steps_down, 2);

    // Paused: once the frames in flight are through, the last one comes again at full.
    let last = frames.last().map(|(i, _)| *i).expect("frames");
    let deadline = Instant::now() + WAIT;
    let mut tail = Vec::new();
    let restored = loop {
        assert!(Instant::now() < deadline, "no full frame after {tail:?}");
        if let Ok(frame) = presented.pop_timeout(Duration::from_millis(50)) {
            tail.push((frame.stamp.index, frame.frame));
            if frame.frame == QualityLevel::Full {
                break frame.stamp.index;
            }
        }
    };
    let shown = tail.iter().rev().nth(1).map_or(last, |(i, _)| *i);
    assert_eq!(
        restored, shown,
        "the frame on screen renders again: {tail:?}"
    );
    let stats = playback.quality();
    assert_eq!(stats.level, QualityLevel::Full);
    assert_eq!(stats.restores, 1);
    eprintln!(
        "paused: {} frames in flight, then frame {restored} again at full; {} level changes, {} dropped",
        tail.len() - 1,
        stats.level_changes(),
        stats.drops
    );
    playback.shutdown();
}
