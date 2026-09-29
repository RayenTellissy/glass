//! Accurate seeking: on every video fixture, 50 random `seek_exact` calls return exactly
//! the frame a linear decode produces at that position (checksum of the planes), in
//! software and, where the machine has a hardware decoder, in hardware; plus seeks after
//! the end of the stream, backwards, between frames and before the start, seeks through a
//! timestamp map, and the GOP index round trip through the cache folder. Skips without
//! fixtures.

mod common;

use std::sync::Arc;

use media::{
    CacheDir, DecoderOptions, FrameReader, GopIndex, GopSource, HwPreference, Rational,
    SourceTimeline, StreamSelect, Timestamp, TimestampMap, VideoFrame,
};

/// A fixed pseudo-random sequence, so failures reproduce.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % below as u64) as usize
    }
}

/// BLAKE3 of the visible samples of every plane (CPU) or of the surface (hardware).
fn checksum(frame: &VideoFrame) -> [u8; 32] {
    if let Some(hw) = frame.hardware() {
        return hw.content_checksum().expect("surface readable");
    }
    let format = frame.format();
    let bytes = if format.bit_depth() > 8 { 2 } else { 1 };
    let mut hasher = blake3::Hasher::new();
    for (i, plane) in frame.planes().iter().enumerate() {
        let interleaved = if format.plane_count() == 2 && i == 1 {
            2
        } else {
            1
        };
        let row = plane.width as usize * bytes * interleaved;
        for line in plane.data.chunks(plane.stride).take(plane.height as usize) {
            hasher.update(&line[..row]);
        }
    }
    *hasher.finalize().as_bytes()
}

fn open(path: &std::path::Path, hw: HwPreference) -> Option<FrameReader> {
    FrameReader::open_with(path, StreamSelect::BestVideo, DecoderOptions::with_hw(hw)).ok()
}

/// Every frame of a linear decode: presentation time and checksum.
fn linear(path: &std::path::Path, hw: HwPreference) -> Vec<(Timestamp, [u8; 32])> {
    let reader = open(path, hw).expect("open");
    reader
        .map(|f| {
            let frame = f.expect("decode").into_video().expect("video");
            (frame.pts().expect("pts"), checksum(&frame))
        })
        .collect()
}

fn seek_frame(reader: &mut FrameReader, target: Timestamp) -> VideoFrame {
    reader
        .seek_exact(target)
        .expect("seek")
        .expect("a frame")
        .into_video()
        .expect("video")
}

fn check_file(file: &str, hw: HwPreference) {
    let Some(path) = common::fixture(file) else {
        return;
    };
    let Some(mut reader) = open(&path, hw) else {
        eprintln!("{file}: no hardware decoder, skipped");
        return;
    };
    if hw == HwPreference::Require && !reader.is_hardware() {
        eprintln!("{file}: decodes in software on this machine, skipped");
        return;
    }
    let frames = linear(&path, hw);
    assert!(!frames.is_empty(), "{file}");
    let label = format!("{file} ({hw:?})");
    let mut rng = Lcg(0x5eed ^ frames.len() as u64);
    for i in 0..50 {
        let k = rng.next(frames.len());
        let (pts, sum) = frames[k];
        let got = seek_frame(&mut reader, pts);
        assert_eq!(got.pts(), Some(pts), "{label}: seek {i} to frame {k}");
        assert_eq!(checksum(&got), sum, "{label}: seek {i} to frame {k}");
        // Decoding continues after the returned frame.
        if i % 10 == 0 && k + 1 < frames.len() {
            let next = reader.next_frame().expect("decode").expect("next frame");
            assert_eq!(
                next.pts(),
                Some(frames[k + 1].0),
                "{label}: after frame {k}"
            );
        }
    }

    // After the end of the stream.
    while reader.next_frame().expect("decode").is_some() {}
    let k = frames.len() / 2;
    let got = seek_frame(&mut reader, frames[k].0);
    assert_eq!(checksum(&got), frames[k].1, "{label}: seek after EOF");

    // Backwards: last frame, then the first, then the middle.
    for k in [frames.len() - 1, 0, frames.len() / 3] {
        let got = seek_frame(&mut reader, frames[k].0);
        assert_eq!(got.pts(), Some(frames[k].0), "{label}: backwards to {k}");
        assert_eq!(checksum(&got), frames[k].1, "{label}: backwards to {k}");
    }

    // Between two frames: the earlier one is on screen.
    if frames.len() > 2 {
        let (a, b) = (frames[1].0, frames[2].0);
        if b.value - a.value > 1 {
            let between = Timestamp::new(a.value + 1, a.time_base);
            let got = seek_frame(&mut reader, between);
            assert_eq!(got.pts(), Some(a), "{label}: between frames 1 and 2");
        }
    }

    // Before the first frame: the first frame.
    let first = frames[0].0;
    let got = seek_frame(
        &mut reader,
        Timestamp::new(first.value - 1000, first.time_base),
    );
    assert_eq!(got.pts(), Some(first), "{label}: before the start");
    eprintln!("{label}: {} frames, all seeks exact", frames.len());
}

fn video_fixtures() -> Vec<String> {
    let Some(files) = common::manifest() else {
        return Vec::new();
    };
    files
        .iter()
        .filter(|e| e["kind"] == "video")
        .filter_map(|e| e["file"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn software_seeks_match_linear_decode() {
    for file in video_fixtures() {
        check_file(&file, HwPreference::Off);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn hardware_seeks_match_linear_decode() {
    for file in video_fixtures() {
        let codec = media::probe(common::generated_dir().join(&file))
            .ok()
            .and_then(|i| {
                i.streams
                    .iter()
                    .find(|s| s.kind == media::MediaKind::Video)
                    .map(|s| s.codec.name)
            });
        if codec.is_some_and(media::hardware_decoder_available) {
            check_file(&file, HwPreference::Require);
        }
    }
}

#[test]
fn seek_through_a_timestamp_map() {
    let Some(path) = common::fixture("vfr_h264_720p.mp4") else {
        return;
    };
    let (timeline, _) = SourceTimeline::scan(&path, StreamSelect::BestVideo).expect("timeline");
    let map = TimestampMap::new(Arc::new(timeline), Rational::new(24, 1));
    let mut reader = FrameReader::open(&path, StreamSelect::BestVideo).expect("open");
    let mut rng = Lcg(24);
    for _ in 0..20 {
        let n = rng.next(map.len() as usize) as u64;
        let frame = reader
            .seek_to_sequence_frame(&map, n)
            .expect("seek")
            .expect("frame");
        assert_eq!(frame.pts(), map.source_pts(n), "sequence frame {n}");
    }
    assert!(
        reader
            .seek_to_sequence_frame(&map, map.len())
            .expect("seek")
            .is_none()
    );
}

fn temp_cache(name: &str) -> CacheDir {
    let dir = std::env::temp_dir().join(format!("glass-seek-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    CacheDir::new(dir)
}

#[test]
fn gop_index_round_trips_on_disk() {
    for (file, built_from, keyframes) in [
        (
            "h264_bframes_gop120_720p60.mp4",
            GopSource::PacketScan,
            Some(2),
        ),
        ("vfr_h264_nob_640x360.mp4", GopSource::ContainerIndex, None),
        (
            "prores422_1080p23976.mov",
            GopSource::ContainerIndex,
            Some(48),
        ),
    ] {
        let Some(path) = common::fixture(file) else {
            return;
        };
        let cache = temp_cache(file);
        let (built, source) =
            GopIndex::load_or_build(&cache, &path, StreamSelect::BestVideo).expect("build");
        assert_eq!(source, built_from, "{file}");
        if let Some(n) = keyframes {
            assert_eq!(built.keyframes.len(), n, "{file}");
        }
        assert_eq!(built.keyframes[0].pts, 0, "{file}");
        let hash = media::ContentHash::of_file(&path).expect("hash");
        assert!(
            cache.analysis_dir(&hash).join("gop-video.json").is_file(),
            "{file}: written under analysis/<hash>/"
        );
        let (loaded, source) =
            GopIndex::load_or_build(&cache, &path, StreamSelect::BestVideo).expect("load");
        assert_eq!(source, GopSource::Disk, "{file}");
        assert_eq!(loaded, built, "{file}");
        // A reader given the loaded index seeks with it.
        let mut reader = FrameReader::open(&path, StreamSelect::BestVideo)
            .expect("open")
            .with_gop_index(Arc::new(loaded));
        let last = built.frames - 1;
        let frames: Vec<Timestamp> = FrameReader::open(&path, StreamSelect::BestVideo)
            .expect("open")
            .map(|f| f.expect("decode").pts().expect("pts"))
            .collect();
        let got = seek_frame(&mut reader, frames[last as usize]);
        assert_eq!(got.pts(), Some(frames[last as usize]), "{file}");
        let _ = std::fs::remove_dir_all(cache.root());
    }
}

/// A seek starts decoding at the target's keyframe, not a GOP earlier: MP4 with
/// B-frames (index searched by presentation time), open GOPs, Matroska, MOV without
/// reordering. Seeking to a keyframe sends only the few packets the decoder's reorder
/// delay needs (one decoder thread), and a keyframe seek returns the keyframe itself.
#[test]
fn seeks_start_at_the_targets_keyframe() {
    for file in [
        "h264_1080p30.mp4",
        "h264_bframes_gop120_720p60.mp4",
        "hevc_10bit_1080p25.mp4",
        "vp9_720p30.webm",
        "prores422_1080p23976.mov",
    ] {
        let Some(path) = common::fixture(file) else {
            return;
        };
        let (gop, _) = GopIndex::build(&path, StreamSelect::BestVideo).expect("gop");
        let gop = Arc::new(gop);
        // One decoder thread: frame threading would add a packet of delay per thread.
        let options = DecoderOptions {
            threads: 1,
            hw: HwPreference::Off,
        };
        let mut reader = FrameReader::open_with(&path, StreamSelect::BestVideo, options)
            .expect("open")
            .with_gop_index(gop.clone());
        for k in (1..gop.keyframes.len()).rev() {
            let target = gop.keyframe_pts(k).expect("keyframe");
            let before = reader.packets_sent();
            let frame = seek_frame(&mut reader, target);
            assert_eq!(frame.pts(), Some(target), "{file} keyframe {k}");
            let sent = reader.packets_sent() - before;
            assert!(sent <= 8, "{file} keyframe {k}: {sent} packets decoded");

            let keyframe = reader.seek_keyframe(target).expect("seek");
            assert_eq!(keyframe, Some(target), "{file} keyframe {k}");
            let first = reader
                .next_frame()
                .expect("decode")
                .expect("frame")
                .pts()
                .expect("pts");
            assert!(first.cmp_time(target).is_le(), "{file} keyframe {k}");
        }
    }
}
