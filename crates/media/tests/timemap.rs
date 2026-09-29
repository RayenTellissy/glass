//! Timestamp maps on the fixtures: exact sequence-to-source indices for the VFR clips (from
//! a packet scan and from the MP4 index) and for constant-rate clips on other rates
//! (23.976 in 25, 30 in 24, 60 in 30), cross-checked against the decoded frames; plus a
//! property test that every map is total over the clip and monotonic. Skips without
//! fixtures.

mod common;

use std::sync::Arc;

use media::timemap::{FrameTimes, TimelineSource};
use media::{
    FrameReader, Rational, SourceTimeline, StreamSelect, TimelineCache, Timestamp, TimestampMap,
};
use proptest::prelude::*;

fn scan(file: &str) -> Option<(SourceTimeline, TimelineSource)> {
    let path = common::fixture(file)?;
    Some(SourceTimeline::scan(&path, StreamSelect::BestVideo).expect("scan"))
}

/// Presentation times of every decoded frame, in order.
fn decoded_pts(file: &str) -> Vec<Timestamp> {
    let path = common::fixture(file).expect("fixture");
    FrameReader::open(&path, StreamSelect::BestVideo)
        .expect("open")
        .map(|f| f.expect("decode").pts().expect("pts"))
        .collect()
}

fn indices(map: &TimestampMap) -> Vec<u64> {
    (0..map.len())
        .map(|n| map.source_frame(n).expect("inside the clip"))
        .collect()
}

/// The kept frames of the VFR fixtures: frames 3 to 5 of every 10 are dropped.
fn kept(total: u64) -> Vec<u64> {
    (0..total)
        .filter(|i| !(3..=5).contains(&(i % 10)))
        .collect()
}

/// For a sequence rate of `num/den`, the expected source index of each sequence frame of
/// a VFR fixture whose frame `j` sits at original 30 fps slot `kept[j]`.
fn expected_vfr(kept: &[u64], slots: u64, num: u64, den: u64) -> Vec<u64> {
    // The clip lasts `slots` thirtieths of a second; sequence frame n is at n*den/num s,
    // i.e. at 30 fps slot floor(n * den * 30 / num).
    let len = (slots * num).div_ceil(30 * den);
    (0..len)
        .map(|n| {
            let slot = n * den * 30 / num;
            kept.iter().filter(|k| **k <= slot).count() as u64 - 1
        })
        .collect()
}

fn check_vfr(file: &str, slots: u64, source: TimelineSource) {
    let Some((timeline, how)) = scan(file) else {
        return;
    };
    assert_eq!(how, source, "{file}");
    let kept = kept(slots);
    assert_eq!(timeline.frame_count(), kept.len() as u64, "{file}");
    // The scanned times are the decoder's.
    let decoded = decoded_pts(file);
    let scanned: Vec<Timestamp> = (0..timeline.frame_count())
        .map(|k| timeline.frame_pts(k).expect("frame"))
        .collect();
    assert_eq!(scanned, decoded, "{file}");
    let timeline = Arc::new(timeline);
    for (num, den) in [(30, 1), (24, 1), (25, 1), (60, 1), (24000, 1001)] {
        let map = TimestampMap::new(timeline.clone(), Rational::new(num as i32, den as i32));
        assert_eq!(
            indices(&map),
            expected_vfr(&kept, slots, num, den),
            "{file} in {num}/{den}"
        );
    }
}

#[test]
fn vfr_with_b_frames_from_a_packet_scan() {
    check_vfr("vfr_h264_720p.mp4", 90, TimelineSource::PacketScan);
}

#[test]
fn vfr_without_reordering_from_the_mp4_index() {
    check_vfr(
        "vfr_h264_nob_640x360.mp4",
        60,
        TimelineSource::ContainerIndex,
    );
}

/// A constant-rate fixture: closed form, the expected indices, and agreement with the
/// decoded frame times.
fn check_cfr(file: &str, frames: u64, sequence: Rational, expected: &[u64]) {
    let Some((timeline, how)) = scan(file) else {
        return;
    };
    assert_eq!(how, TimelineSource::ClosedForm, "{file}");
    assert!(matches!(timeline.frames, FrameTimes::Constant { .. }));
    assert_eq!(timeline.frame_count(), frames, "{file}");
    let decoded = decoded_pts(file);
    assert_eq!(decoded.len() as u64, frames);
    for (k, pts) in decoded.iter().enumerate() {
        assert_eq!(timeline.frame_pts(k as u64), Some(*pts), "{file} frame {k}");
        assert_eq!(timeline.frame_at(*pts), Some(k as u64), "{file} frame {k}");
    }
    let map = TimestampMap::new(Arc::new(timeline), sequence);
    assert_eq!(indices(&map), expected, "{file} in {sequence}");
}

#[test]
fn ntsc_film_in_25() {
    // 48 frames at 24000/1001 in a 25 fps sequence: 51 frames, frame k at
    // floor(n * 24000 / 25025).
    let expected: Vec<u64> = (0..51).map(|n| n * 24000 / 25025).collect();
    assert_eq!(&expected[..4], &[0, 0, 1, 2]);
    check_cfr(
        "prores422_1080p23976.mov",
        48,
        Rational::new(25, 1),
        &expected,
    );
}

#[test]
fn thirty_in_24() {
    let expected: Vec<u64> = (0..72).map(|n| n * 30 / 24).collect();
    assert_eq!(&expected[..6], &[0, 1, 2, 3, 5, 6]);
    check_cfr("h264_1080p30.mp4", 90, Rational::new(24, 1), &expected);
}

#[test]
fn sixty_in_30() {
    let expected: Vec<u64> = (0..90).map(|n| n * 2).collect();
    check_cfr(
        "h264_bframes_gop120_720p60.mp4",
        180,
        Rational::new(30, 1),
        &expected,
    );
}

#[test]
fn same_rate_is_the_identity() {
    let expected: Vec<u64> = (0..50).collect();
    check_cfr(
        "hevc_10bit_1080p25.mp4",
        50,
        Rational::new(25, 1),
        &expected,
    );
}

#[test]
fn cache_scans_each_file_once() {
    let Some(path) = common::fixture("vfr_h264_720p.mp4") else {
        return;
    };
    let cache = TimelineCache::new();
    let a = cache
        .get_or_scan(&path, StreamSelect::BestVideo)
        .expect("scan");
    let b = cache
        .get_or_scan(&path, StreamSelect::BestVideo)
        .expect("cached");
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(cache.len(), 1);
}

fn arb_timeline() -> impl Strategy<Value = SourceTimeline> {
    let constant = (
        prop_oneof![
            Just(Rational::new(24000, 1001)),
            Just(Rational::new(24, 1)),
            Just(Rational::new(25, 1)),
            Just(Rational::new(30000, 1001)),
            Just(Rational::new(30, 1)),
            Just(Rational::new(50, 1)),
            Just(Rational::new(60000, 1001)),
            Just(Rational::new(60, 1)),
            Just(Rational::new(120, 1)),
        ],
        1u64..400,
        -5000i64..5000,
        prop_oneof![
            Just(Rational::new(1, 90_000)),
            Just(Rational::new(1, 30_000)),
            Just(Rational::new(1, 1000)),
            Just(Rational::new(1, 24000)),
        ],
    )
        .prop_map(|(rate, count, start, time_base)| SourceTimeline {
            stream_index: 0,
            time_base,
            frames: FrameTimes::Constant { start, rate, count },
        });
    let variable = (
        prop::collection::vec(1i64..5000, 1..300),
        -5000i64..5000,
        1i64..5000,
    )
        .prop_map(|(gaps, start, last)| {
            let mut t = start;
            let pts: Vec<i64> = gaps
                .iter()
                .map(|g| {
                    let at = t;
                    t += g;
                    at
                })
                .collect();
            let end = pts.last().copied().unwrap_or(start) + last;
            SourceTimeline {
                stream_index: 0,
                time_base: Rational::new(1, 90_000),
                frames: FrameTimes::Variable { pts, end },
            }
        });
    prop_oneof![constant, variable]
}

fn arb_rate() -> impl Strategy<Value = Rational> {
    prop_oneof![
        Just(Rational::new(24000, 1001)),
        Just(Rational::new(24, 1)),
        Just(Rational::new(25, 1)),
        Just(Rational::new(30000, 1001)),
        Just(Rational::new(30, 1)),
        Just(Rational::new(50, 1)),
        Just(Rational::new(60, 1)),
        (1i32..240, 1i32..1002).prop_map(|(n, d)| Rational::new(n, d)),
    ]
}

proptest! {
    #[test]
    fn maps_are_total_and_monotonic(timeline in arb_timeline(), rate in arb_rate()) {
        let timeline = Arc::new(timeline);
        let map = TimestampMap::new(timeline.clone(), rate);
        let frames = timeline.frame_count();
        prop_assert!(!map.is_empty());
        // Total: every sequence frame inside the clip maps, the next one does not, and the
        // last one's instant is still inside the clip.
        prop_assert_eq!(map.source_frame(map.len()), None);
        prop_assert!(map.instant(map.len() - 1).value < timeline.end());
        prop_assert!(map.instant(map.len()).value >= timeline.end() - 1);
        prop_assert_eq!(map.source_frame(0), Some(0));
        let mut previous = 0;
        for n in 0..map.len() {
            let k = map.source_frame(n);
            prop_assert!(k.is_some(), "frame {} of {} unmapped", n, map.len());
            let k = k.unwrap_or_default();
            prop_assert!(k < frames);
            // Monotonic.
            prop_assert!(k >= previous, "frame {} goes back from {} to {}", n, previous, k);
            previous = k;
            // Nearest earlier: the frame starts at or before the instant and the next
            // frame starts after it.
            let instant = map.instant(n);
            if let FrameTimes::Variable { .. } = timeline.frames {
                let pts = timeline.frame_pts(k).map(|t| t.value);
                prop_assert!(pts.is_some_and(|p| p <= instant.value));
                if let Some(next) = timeline.frame_pts(k + 1) {
                    prop_assert!(next.value > instant.value);
                }
            }
            // The inverse lookup lands on the same sequence frame, or the one before when
            // the instant falls between ticks and was rounded down.
            let back = map.sequence_frame(instant);
            prop_assert!(back.is_some_and(|m| m == n || m + 1 == n), "{:?} for {}", back, n);
        }
    }
}
