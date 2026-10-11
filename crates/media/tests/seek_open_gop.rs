//! Seeks inside an open GOP: in a GOP that starts at a non-IDR keyframe (some pictures
//! after it in decode order are presented before it), a reader that seeks to a frame,
//! then back to an earlier frame of the same GOP, returns exactly the frames a linear
//! decode produces from there on, in software and in hardware. The decoder has to drop
//! the reference pictures it held before the second seek; VideoToolbox keeps them through
//! a flush. Every start frame of each open GOP is covered (every third frame), not one
//! seek history, because which seeks expose stale references depends on the bitstream.
//! Skips without fixtures.

mod common;

use std::ops::Range;
use std::path::Path;

use media::{
    DecoderOptions, Demuxer, FrameReader, HwPreference, MediaKind, StreamSelect, Timestamp,
    VideoFrame,
};

/// Frames checked after each target, besides the target itself.
const FOLLOWING: usize = 8;

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

fn open(path: &Path, hw: HwPreference) -> Option<FrameReader> {
    FrameReader::open_with(path, StreamSelect::BestVideo, DecoderOptions::with_hw(hw)).ok()
}

/// Every frame of a linear decode: presentation time and checksum.
fn linear(path: &Path, hw: HwPreference) -> Vec<(Timestamp, [u8; 32])> {
    open(path, hw)
        .expect("open")
        .map(|f| {
            let frame = f.expect("decode").into_video().expect("video");
            (frame.pts().expect("pts"), checksum(&frame))
        })
        .collect()
}

/// An open GOP: its keyframe and the next keyframe, if any (presentation times).
type OpenGop = (Timestamp, Option<Timestamp>);

/// The GOPs after the first that are open: a packet that follows the GOP's keyframe in
/// decode order is presented before it.
fn open_gops(path: &Path) -> Vec<OpenGop> {
    let mut demuxer = Demuxer::open(path).expect("open");
    let stream = demuxer.best_stream(MediaKind::Video).expect("video stream");
    let mut keyframes: Vec<(Timestamp, bool)> = Vec::new();
    while let Some(packet) = demuxer.read_packet().expect("read") {
        if packet.stream_index() != stream {
            continue;
        }
        let Some(pts) = packet.pts() else {
            continue;
        };
        if packet.is_key() {
            keyframes.push((pts, false));
        } else if let [_, .., (keyframe, open)] = keyframes.as_mut_slice()
            && pts.cmp_time(*keyframe).is_lt()
        {
            *open = true;
        }
    }
    keyframes
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, (_, open))| *open)
        .map(|(i, (pts, _))| (*pts, keyframes.get(i + 1).map(|(next, _)| *next)))
        .collect()
}

/// Video fixtures with at least one open GOP after the first keyframe.
fn open_gop_fixtures() -> Vec<(String, Vec<OpenGop>)> {
    let Some(files) = common::manifest() else {
        return Vec::new();
    };
    let found: Vec<(String, Vec<OpenGop>)> = files
        .iter()
        .filter(|e| e["kind"] == "video")
        .filter_map(|e| e["file"].as_str())
        .filter_map(|file| {
            let gops = open_gops(&common::generated_dir().join(file));
            (!gops.is_empty()).then(|| (file.to_string(), gops))
        })
        .collect();
    // The fixture made for this (GOP 120, open-gop=1) must be found, or the detection
    // above is broken and every check below would pass without running.
    let made_for_this = "h264_bframes_gop120_720p60.mp4";
    if common::generated_dir().join(made_for_this).exists() {
        assert!(
            found.iter().any(|(f, _)| f == made_for_this),
            "{made_for_this} has an open GOP at frame 120; found {found:?}"
        );
    }
    found
}

/// The frames (indexes into a linear decode) of an open GOP.
fn gop_range(frames: &[(Timestamp, [u8; 32])], (keyframe, next): OpenGop) -> Range<usize> {
    let index = |pts: Timestamp| {
        frames
            .iter()
            .position(|(p, _)| p.cmp_time(pts).is_eq())
            .expect("keyframe in the linear decode")
    };
    index(keyframe)..next.map_or(frames.len(), index)
}

/// Opens a fresh reader, seeks to `start`, decodes the next frame, seeks back to
/// `target`, and compares `target` and the frames after it with the linear decode.
/// Returns a description of each mismatch.
fn seek_back(
    path: &Path,
    hw: HwPreference,
    frames: &[(Timestamp, [u8; 32])],
    start: usize,
    target: usize,
) -> Vec<String> {
    let mut reader = open(path, hw).expect("open");
    let mut errors = Vec::new();
    let mut compare = |what: &str, k: usize, frame: Option<VideoFrame>| match frame {
        Some(frame) if frame.pts() != Some(frames[k].0) => errors.push(format!(
            "{what} {k}: pts {:?}, expected {:?}",
            frame.pts(),
            frames[k].0
        )),
        Some(frame) if checksum(&frame) != frames[k].1 => {
            errors.push(format!("{what} {k}: wrong pixels"));
        }
        Some(_) => {}
        None => errors.push(format!("{what} {k}: missing")),
    };
    let seek = |reader: &mut FrameReader, k: usize| {
        reader
            .seek_exact(frames[k].0)
            .expect("seek")
            .and_then(|f| f.into_video())
    };
    let next = |reader: &mut FrameReader| {
        reader
            .next_frame()
            .expect("decode")
            .and_then(|f| f.into_video())
    };

    compare("first seek, frame", start, seek(&mut reader, start));
    if start + 1 < frames.len() {
        compare("next, frame", start + 1, next(&mut reader));
    }
    compare("seek back, frame", target, seek(&mut reader, target));
    for k in target + 1..frames.len().min(target + 1 + FOLLOWING) {
        compare("then frame", k, next(&mut reader));
    }
    errors
}

/// For every third start frame of every open GOP (and its last frame), seeks there and
/// then back to the GOP's keyframe, its middle frame and the frame before the start.
fn check_open_gops(hw: HwPreference) {
    let mut cases = 0;
    let mut failures = Vec::new();
    for (file, gops) in open_gop_fixtures() {
        let path = common::generated_dir().join(&file);
        let Some(reader) = open(&path, hw) else {
            eprintln!("{file}: no hardware decoder, skipped");
            continue;
        };
        if hw == HwPreference::Require && !reader.is_hardware() {
            eprintln!("{file}: decodes in software on this machine, skipped");
            continue;
        }
        drop(reader);
        let frames = linear(&path, hw);
        for gop in gops.iter().map(|g| gop_range(&frames, *g)) {
            let middle = gop.start + gop.len() / 2;
            let mut starts: Vec<usize> = gop.clone().step_by(3).collect();
            starts.push(gop.end - 1);
            starts.dedup();
            for start in starts {
                let mut targets = vec![gop.start, middle, start.saturating_sub(1)];
                targets.retain(|t| gop.contains(t) && *t < start);
                targets.sort_unstable();
                targets.dedup();
                for target in targets {
                    cases += 1;
                    let errors = seek_back(&path, hw, &frames, start, target);
                    if !errors.is_empty() {
                        failures.push(format!(
                            "{file} ({hw:?}): seek to frame {start} then {target}: {}",
                            errors.join("; ")
                        ));
                    }
                }
            }
        }
        eprintln!("{file} ({hw:?}): {cases} seek pairs so far");
    }
    assert!(
        failures.is_empty(),
        "{} of {cases} seek pairs returned wrong frames:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn software_seeks_back_inside_an_open_gop_match_linear_decode() {
    check_open_gops(HwPreference::Off);
}

#[cfg(target_os = "macos")]
#[test]
fn hardware_seeks_back_inside_an_open_gop_match_linear_decode() {
    check_open_gops(HwPreference::Require);
}
