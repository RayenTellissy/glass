//! Scrub mode on generated media, decoding in software: the time from the end of the
//! 40 ms pause to the exact frame at 1080p and 4K, and how near the immediate answer is
//! during a fast synthetic scrub. Timings are printed (`-- --nocapture`); the assertions
//! only check that every exact frame is the right one.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::lookahead::{GopIndexes, Layer, PlanSegment, SegmentList, SourceId, SourceRef};
use engine::scrub::{
    CachedFrames, MonotonicClock, ScrubConfig, ScrubRender, ScrubStats, Scrubber,
    SoftwareDecoderOptions, SoftwareScrubDecoder,
};
use engine::threads::ThreadRegistry;
use media::{Rational, Timestamp, VideoFrame};
use render::frame_cache::SegmentHash;

const WAIT: Duration = Duration::from_secs(10);
const PAUSE: Duration = Duration::from_millis(40);
/// An immediate answer within half a second (at 30 fps) of the pointer counts as a hit.
const HIT_RADIUS: i64 = 15;

struct Clip {
    name: &'static str,
    path: PathBuf,
    /// Source frames (30 fps, 512 ticks of 1/15360 each).
    frames: i64,
}

fn clips() -> Vec<Clip> {
    [
        ("1080p", "h264_1080p30.mp4", 90),
        ("4K", "h264_4k30.mp4", 60),
    ]
    .into_iter()
    .filter_map(|(name, file, frames)| {
        Some(Clip {
            name,
            path: common::fixture(file)?,
            frames,
        })
    })
    .collect()
}

fn time_base() -> Rational {
    Rational::new(1, 15360)
}

/// The clip `loops` times in a row on a 30 fps timeline.
fn looped(clip: &Clip, loops: i64) -> SegmentList {
    SegmentList::new((0..loops).map(|i| PlanSegment {
        source: SourceRef::new(SourceId(1), &clip.path),
        timeline: i * clip.frames..(i + 1) * clip.frames,
        source_start: Timestamp::new(0, time_base()),
        speed: Rational::new(1, 1),
        layer: 0,
        hash: SegmentHash::from(i as u64),
    }))
}

/// Records the source time of each rendered frame.
#[derive(Clone, Default)]
struct Record {
    pts: Arc<Mutex<HashMap<i64, Option<Timestamp>>>>,
}

impl ScrubRender<VideoFrame> for Record {
    fn render(&mut self, frame: i64, layers: &[Layer<VideoFrame>]) {
        let pts = layers.first().and_then(|l| l.source_pts);
        self.pts.lock().unwrap().insert(frame, pts);
    }
}

fn start(clip: &Clip, loops: i64, prefetch_per_move: usize, record: Record) -> Scrubber {
    let gops = GopIndexes::new();
    Scrubber::start(
        looped(clip, loops),
        ScrubConfig {
            pause: PAUSE,
            frame_duration: Rational::new(1, 30),
            prefetch_per_move,
            registry: ThreadRegistry::new(),
            ..ScrubConfig::default()
        },
        CachedFrames::new(),
        Arc::new(MonotonicClock::new()),
        move |source: &SourceRef| {
            SoftwareScrubDecoder::open(source, &gops, SoftwareDecoderOptions::default())
        },
        record,
        None,
    )
    .expect("scrub thread starts")
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Requests `frame`, then waits for its exact frame; the time from the request.
fn exact_after_request(scrubber: &mut Scrubber, frame: i64) -> Duration {
    let started = Instant::now();
    scrubber.request(frame);
    loop {
        if let Some(ready) = scrubber.poll_ready() {
            assert_eq!(ready.frame, frame);
            return started.elapsed();
        }
        assert!(started.elapsed() < WAIT, "frame {frame} never arrived");
        std::thread::sleep(Duration::from_micros(250));
    }
}

#[test]
fn exact_frames_arrive_soon_after_the_pause() {
    for clip in clips() {
        let loops = 4;
        let record = Record::default();
        let mut scrubber = start(&clip, loops, 0, record.clone());
        let len = clip.frames * loops;
        // The first request opens the decoder and builds the GOP index.
        let first = exact_after_request(&mut scrubber, len / 2);
        // Distinct frames in a scrambled order, so most land mid-GOP and none is cached.
        let mut targets: Vec<i64> = (0..len).filter(|f| *f != len / 2).collect();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for i in (1..targets.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            targets.swap(i, (seed % (i as u64 + 1)) as usize);
        }
        targets.truncate(60);
        let mut after_pause: Vec<f64> = targets
            .iter()
            .map(|&frame| ms(exact_after_request(&mut scrubber, frame).saturating_sub(PAUSE)))
            .collect();
        after_pause.sort_by(f64::total_cmp);

        let pts = record.pts.lock().unwrap();
        for &frame in targets.iter().chain([len / 2].iter()) {
            let expected = Timestamp::new((frame % clip.frames) * 512, time_base());
            assert_eq!(
                pts.get(&frame),
                Some(&Some(expected)),
                "{} frame {frame}",
                clip.name
            );
        }
        let stats = scrubber.finish().stats;
        assert_eq!(stats.decode_errors, 0);
        assert_eq!(stats.exact_decodes, targets.len() as u64 + 1);
        eprintln!(
            "{}: time to exact frame after the {} ms pause over {} random frames: median {:.1} ms, \
             p95 {:.1} ms, max {:.1} ms; first request (open + GOP index) {:.1} ms after the pause",
            clip.name,
            PAUSE.as_millis(),
            targets.len(),
            percentile(&after_pause, 0.5),
            percentile(&after_pause, 0.95),
            after_pause.last().copied().unwrap_or(0.0),
            ms(first.saturating_sub(PAUSE)),
        );
    }
}

#[test]
fn a_fast_scrub_finds_near_cached_frames() {
    for clip in clips() {
        let loops = 12;
        let len = clip.frames * loops;
        for prefetch in [0, 2] {
            let mut scrubber = start(&clip, loops, prefetch, Record::default());
            // 3 frames per 8 ms pointer event: 12.5 times real time, never pausing.
            let mut distances = Vec::new();
            let mut answered = 0;
            let mut frame = 0;
            while frame < len {
                if let Some(shown) = scrubber.request(frame) {
                    answered += 1;
                    distances.push((shown.frame - frame).abs());
                }
                std::thread::sleep(Duration::from_millis(8));
                frame += 3;
            }
            let requests = (len + 2) / 3;
            let hits = distances.iter().filter(|d| **d <= HIT_RADIUS).count();
            let hit_rate = hits as f64 / requests as f64;
            distances.sort_unstable();
            let median = distances.get(distances.len() / 2).copied().unwrap_or(-1);
            let stats: ScrubStats = scrubber.finish().stats;
            eprintln!(
                "{} fast scrub, prefetch {prefetch} per move: {requests} requests, hit rate {:.0}% \
                 (within {HIT_RADIUS} frames), answered {answered}, median distance {median} \
                 frames, {} cheap frames prefetched, {} cancelled, {} exact decodes (pointer \
                 events late by over the pause)",
                clip.name,
                hit_rate * 100.0,
                stats.prefetched,
                stats.prefetch_cancelled,
                stats.exact_decodes,
            );
            assert_eq!(stats.decode_errors, 0);
            if prefetch == 0 {
                assert_eq!(answered, 0, "nothing cached without prefetching");
            } else {
                assert!(stats.prefetched > 0);
            }
        }
    }
}
