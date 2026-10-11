//! Look-ahead across two segments of one open-GOP file where the second starts earlier
//! in a GOP than the first ended: the worker that decoded the first segment keeps the
//! file open and seeks its decoder backwards inside the open GOP, and every delivered
//! frame must match a linear decode exactly, with hardware decode (seeking in hardware,
//! and with the software pre-roll handing over to hardware at the keyframe) and in
//! software. Where the first segment ends is swept across the GOP (every third frame),
//! because which decoder states expose stale reference pictures depends on the bitstream.
//! Skips without the fixture.

mod common;

use std::hash::{DefaultHasher, Hasher};
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use engine::lookahead::{
    FramesError, GopIndexes, Layer, Lookahead, LookaheadConfig, MediaDecoder, MediaDecoderOptions,
    PlanSegment, PreRoll, SegmentDecoder, SegmentList, SourceId, SourceRef,
};
use engine::threads::ThreadRegistry;
use media::{
    DecoderOptions, FrameReader, HwPreference, MediaError, Rational, StreamSelect, Timestamp,
    VideoFrame,
};
use render::frame_cache::SegmentHash;

/// 60 fps, GOP 120 with an open GOP at frame 120, B-frames; 180 frames.
const FIXTURE: &str = "h264_bframes_gop120_720p60.mp4";
const WAIT: Duration = Duration::from_secs(20);

/// The plan's segments: source, first source frame, timeline frames. The timeline runs at
/// the fixture's 60 fps, so timeline frame `t` of a segment shows source frame
/// `first + t - start`. The middle segment is another source (same file, another id) for
/// the second worker, so the third goes to the worker that decoded the first.
type Spec = [(u64, usize, Range<i64>); 3];

/// Frames of the filler segment: more than the lane depth, so the first segment's worker
/// is idle before the filler's.
const FILLER: i64 = 20;
const DEPTH: usize = 4;

/// Where the first segment ends (exclusive source frame): every third frame of the open
/// GOP 120..180.
fn first_segment_ends() -> impl Iterator<Item = usize> {
    (121..180).step_by(3)
}

fn spec(first: (usize, usize), third: usize) -> Spec {
    let (start, end) = first;
    let a = (end - start) as i64;
    let c = (180 - third) as i64;
    [
        (1, start, 0..a),
        (2, 0, a..a + FILLER),
        (1, third, a + FILLER..a + FILLER + c),
    ]
}

/// Seeking in the playback decoder: the first segment plays 120..`end`, the third seeks
/// back to the keyframe at 120 and plays to the end of the file.
fn seek_back_in_gop(end: usize) -> Spec {
    spec((120, end), 120)
}

/// Software pre-roll: the first segment starts at 110, so software decodes to the
/// keyframe at 120 and hardware decodes 120..`end`; the third starts at 105, so the
/// hardware decoder is sent back to the keyframe at 120 and plays on from there to the
/// end of the file.
fn hand_off_back_to_keyframe(end: usize) -> Spec {
    spec((110, end), 105)
}

/// A picture's checksum: the surface's for hardware frames, a hash of the visible
/// samples of every plane for CPU frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sum {
    Surface([u8; 32]),
    Planes(u64),
}

fn checksum(frame: &VideoFrame) -> Sum {
    if let Some(hw) = frame.hardware() {
        return Sum::Surface(hw.content_checksum().expect("surface readable"));
    }
    let format = frame.format();
    let bytes = if format.bit_depth() > 8 { 2 } else { 1 };
    let mut hasher = DefaultHasher::new();
    for (i, plane) in frame.planes().iter().enumerate() {
        let interleaved = if format.plane_count() == 2 && i == 1 {
            2
        } else {
            1
        };
        let row = plane.width as usize * bytes * interleaved;
        for line in plane.data.chunks(plane.stride).take(plane.height as usize) {
            hasher.write(&line[..row]);
        }
    }
    Sum::Planes(hasher.finish())
}

/// Every frame of a linear decode: presentation time and checksum.
fn linear(path: &Path, hw: HwPreference) -> Vec<(Timestamp, Sum)> {
    FrameReader::open_with(path, StreamSelect::BestVideo, DecoderOptions::with_hw(hw))
        .expect("open")
        .map(|f| {
            let frame = f.expect("decode").into_video().expect("video");
            (frame.pts().expect("pts"), checksum(&frame))
        })
        .collect()
}

/// Seeks a [`MediaDecoder`] was given: (worker, source, target).
type SeekLog = Arc<Mutex<Vec<(usize, SourceId, Timestamp)>>>;

/// The real decoder, with its seeks logged so the test can check which worker took which
/// segment.
struct Logged {
    worker: usize,
    inner: MediaDecoder,
    log: SeekLog,
}

impl SegmentDecoder for Logged {
    type Frame = VideoFrame;

    fn seek(&mut self, source: &SourceRef, at: Timestamp) -> Result<(), MediaError> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((self.worker, source.id, at));
        self.inner.seek(source, at)
    }

    fn next_frame(&mut self) -> Result<Option<(Timestamp, VideoFrame)>, MediaError> {
        self.inner.next_frame()
    }
}

struct Expected {
    software: Vec<(Timestamp, Sum)>,
    hardware: Option<Vec<(Timestamp, Sum)>>,
}

/// Plays `spec` through the look-ahead and compares every delivered frame with the
/// linear decode (hardware frames with the hardware decode, software frames with the
/// software one), and checks the third segment reused the first one's decoder. Returns
/// the mismatches and how many frames of the third segment came from hardware.
fn play(
    path: &Path,
    spec: &Spec,
    options: MediaDecoderOptions,
    expected: &Expected,
) -> (Vec<String>, usize) {
    let segments = spec.iter().map(|(source, first, timeline)| PlanSegment {
        source: SourceRef::new(SourceId(*source), path),
        timeline: timeline.clone(),
        source_start: expected.software[*first].0,
        speed: Rational::new(1, 1),
        layer: 0,
        hash: SegmentHash::from(timeline.start as u64),
    });
    let plan = SegmentList::new(segments);
    let config = LookaheadConfig {
        decode_workers: 2,
        depth: DEPTH,
        frame_duration: Rational::new(1, 60),
        registry: ThreadRegistry::new(),
    };
    let log = SeekLog::default();
    let gops = GopIndexes::new();
    let (lookahead, mut frames) = Lookahead::start(plan, config, |worker| Logged {
        worker,
        inner: MediaDecoder::new(options, gops.clone()),
        log: log.clone(),
    })
    .expect("start");

    let end = spec[2].2.end;
    let mut layers: Vec<Layer<VideoFrame>> = Vec::new();
    let mut errors = Vec::new();
    let mut hardware_in_third = 0;
    for t in 0..end {
        let stamp = frames
            .next_into_timeout(&mut layers, WAIT)
            .expect("next frame");
        assert_eq!(stamp.index, t);
        let (source, first, timeline) = spec
            .iter()
            .find(|(_, _, timeline)| timeline.contains(&t))
            .expect("in the plan");
        let k = first + (t - timeline.start) as usize;
        let [layer] = &layers[..] else {
            panic!("frame {t}: {} layers, expected 1", layers.len());
        };
        assert_eq!(layer.source, SourceId(*source), "frame {t}");
        let Some(frame) = &layer.frame else {
            errors.push(format!("source frame {k}: no picture"));
            continue;
        };
        let reference = match (frame.hardware(), &expected.hardware) {
            (Some(_), Some(hardware)) => hardware,
            (Some(_), None) => panic!("frame {t}: hardware frame in a software run"),
            (None, _) => &expected.software,
        };
        if layer.source_pts != Some(reference[k].0) {
            errors.push(format!(
                "source frame {k}: pts {:?}, expected {:?}",
                layer.source_pts, reference[k].0
            ));
        } else if checksum(frame) != reference[k].1 {
            let from = if frame.hardware().is_some() {
                "hardware"
            } else {
                "software"
            };
            errors.push(format!("source frame {k} ({from}): wrong pixels"));
        }
        if timeline.start == spec[2].2.start && frame.hardware().is_some() {
            hardware_in_third += 1;
        }
    }
    assert_eq!(
        frames.next_into_timeout(&mut layers, WAIT).unwrap_err(),
        FramesError::Finished
    );
    assert_eq!(lookahead.stats().decode_errors, 0);
    drop(frames);
    lookahead.shutdown();

    // The third segment went to the decoder that played the first one, so its seek was a
    // backward seek on a decoder already inside the GOP.
    let log = log.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let worker_of = |source: u64, first: usize| {
        log.iter()
            .find(|(_, s, at)| {
                *s == SourceId(source) && at.cmp_time(expected.software[first].0).is_eq()
            })
            .map(|(worker, _, _)| *worker)
    };
    let (a, c) = (&spec[0], &spec[2]);
    assert_eq!(
        worker_of(a.0, a.1),
        worker_of(c.0, c.1),
        "the third segment reuses the first one's decoder: {log:?}"
    );
    (errors, hardware_in_third)
}

/// Plays `plan(end)` for every end of the first segment; fails with every plan that
/// delivered a wrong frame. `hardware` is how many frames of the third segment must come
/// from hardware.
fn sweep(
    path: &Path,
    name: &str,
    plan: fn(usize) -> Spec,
    options: MediaDecoderOptions,
    expected: &Expected,
    hardware: usize,
) {
    let mut failures = Vec::new();
    let mut plans = 0;
    for end in first_segment_ends() {
        plans += 1;
        let (errors, from_hardware) = play(path, &plan(end), options, expected);
        assert_eq!(
            from_hardware, hardware,
            "{name}, first segment to {end}: frames of the third segment from hardware"
        );
        if !errors.is_empty() {
            failures.push(format!(
                "{name}, first segment to {end}: {} wrong frames: {}",
                errors.len(),
                errors.join("; ")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {plans} plans delivered wrong frames:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn fixture() -> Option<std::path::PathBuf> {
    common::fixture(FIXTURE)
}

/// The linear decodes, with the hardware one only when the machine decodes the fixture
/// in hardware.
fn expected(path: &Path, hardware: bool) -> Option<Expected> {
    let software = linear(path, HwPreference::Off);
    if !hardware {
        return Some(Expected {
            software,
            hardware: None,
        });
    }
    let reader = FrameReader::open_with(
        path,
        StreamSelect::BestVideo,
        DecoderOptions::with_hw(HwPreference::Require),
    );
    if !reader.is_ok_and(|r| r.is_hardware()) {
        eprintln!("{FIXTURE}: no hardware decoder on this machine, skipped");
        return None;
    }
    Some(Expected {
        software,
        hardware: Some(linear(path, HwPreference::Require)),
    })
}

fn hardware_options(pre_roll: PreRoll) -> MediaDecoderOptions {
    MediaDecoderOptions {
        decoder: DecoderOptions::with_hw(HwPreference::Require),
        project_rate: Rational::new(60, 1),
        pre_roll,
    }
}

#[cfg(target_os = "macos")]
#[test]
fn hardware_seek_back_inside_an_open_gop_delivers_exact_frames() {
    let Some(path) = fixture() else {
        return;
    };
    let Some(expected) = expected(&path, true) else {
        return;
    };
    // Every frame of the third segment (120..180) from hardware.
    sweep(
        &path,
        "hardware seek",
        seek_back_in_gop,
        hardware_options(PreRoll::Steady),
        &expected,
        60,
    );
}

#[cfg(target_os = "macos")]
#[test]
fn software_pre_roll_hand_off_back_to_an_open_gop_keyframe_delivers_exact_frames() {
    let Some(path) = fixture() else {
        return;
    };
    let Some(expected) = expected(&path, true) else {
        return;
    };
    // Software from 105 to the keyframe, hardware from 120 to the end.
    sweep(
        &path,
        "software pre-roll",
        hand_off_back_to_keyframe,
        hardware_options(PreRoll::Software),
        &expected,
        60,
    );
}

#[test]
fn software_seek_back_inside_an_open_gop_delivers_exact_frames() {
    let Some(path) = fixture() else {
        return;
    };
    let Some(expected) = expected(&path, false) else {
        return;
    };
    let options = MediaDecoderOptions {
        decoder: DecoderOptions::with_hw(HwPreference::Off),
        project_rate: Rational::new(60, 1),
        pre_roll: PreRoll::Steady,
    };
    for (name, plan) in [
        (
            "software, seek-back plan",
            seek_back_in_gop as fn(usize) -> Spec,
        ),
        ("software, hand-off plan", hand_off_back_to_keyframe),
    ] {
        sweep(&path, name, plan, options, &expected, 0);
    }
}
