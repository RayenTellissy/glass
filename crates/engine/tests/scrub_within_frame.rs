//! Scrub pauses between two source frames: the exact frame published is the source frame
//! on screen at that instant (pts <= t < next pts), not the one after it. A 25 fps clip
//! on a 50 fps timeline puts every odd timeline frame halfway through a source frame, and
//! the GOP walk's last step lands exactly on the frame before the target. Skips without
//! fixtures.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::lookahead::{GopIndexes, Layer, PlanSegment, SegmentList, SourceId, SourceRef};
use engine::scrub::{
    CachedFrames, MonotonicClock, ScrubConfig, ScrubRender, Scrubber, SoftwareDecoderOptions,
    SoftwareScrubDecoder,
};
use engine::threads::ThreadRegistry;
use media::{Rational, Timestamp, VideoFrame};
use render::frame_cache::SegmentHash;

const WAIT: Duration = Duration::from_secs(10);

/// The fixture's stream time base: a 25 fps frame is 512 ticks.
fn time_base() -> Rational {
    Rational::new(1, 12800)
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

/// The whole 1 s clip on a 50 fps timeline: timeline frame `f` shows source time `f / 50`.
fn start(path: &Path, record: Record) -> Scrubber {
    let gops = GopIndexes::new();
    Scrubber::start(
        SegmentList::new([PlanSegment {
            source: SourceRef::new(SourceId(1), path),
            timeline: 0..50,
            source_start: Timestamp::new(0, time_base()),
            speed: Rational::new(1, 1),
            layer: 0,
            hash: SegmentHash::from(1u64),
        }]),
        ScrubConfig {
            frame_duration: Rational::new(1, 50),
            prefetch_per_move: 0,
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

/// Requests timeline frame `frame` and waits for its exact frame to be published.
fn pause_at(scrubber: &mut Scrubber, frame: i64) {
    let started = Instant::now();
    scrubber.request(frame);
    loop {
        if let Some(ready) = scrubber.poll_ready() {
            assert_eq!(ready.frame, frame);
            return;
        }
        assert!(started.elapsed() < WAIT, "frame {frame} never arrived");
        std::thread::sleep(Duration::from_micros(250));
    }
}

#[test]
fn a_pause_between_source_frames_publishes_the_frame_on_screen() {
    let Some(path) = common::fixture("h264_fullrange_640x360.mp4") else {
        return;
    };
    let record = Record::default();
    let mut scrubber = start(&path, record.clone());
    // 0.22 s (frames at 0.20 and 0.24 s, the reported case), then mid-frame pauses after
    // a forward move inside the GOP, a backward one, and one onto the very first frame.
    let pauses = [11, 31, 17, 1];
    for frame in pauses {
        pause_at(&mut scrubber, frame);
    }
    let pts = record.pts.lock().unwrap();
    for frame in pauses {
        // Timeline frame f is source time f / 50 s; the source frame on screen is f / 2.
        let expected = Timestamp::new(frame / 2 * 512, time_base());
        assert_eq!(
            pts.get(&frame),
            Some(&Some(expected)),
            "timeline frame {frame} ({:.2} s) shows the source frame at {:.2} s",
            frame as f64 / 50.0,
            expected.seconds()
        );
    }
    drop(pts);
    assert_eq!(scrubber.finish().stats.decode_errors, 0);
}
