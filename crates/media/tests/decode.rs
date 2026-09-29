//! The software decode path against every fixture in the manifest: frame counts, first
//! timestamps, sizes and formats, audio sample totals, seek to start, and one decoded
//! frame compared with the FFmpeg command-line tool's conversion of the same frame.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use media::{Frame, FrameReader, MediaKind, SampleType, StreamSelect, probe};

struct VideoStats {
    frames: u64,
    first_pts: f64,
    last_pts: f64,
    width: u32,
    height: u32,
    format: &'static str,
}

/// Decodes the whole video stream. Every fixture codec has a software decoder in the
/// pinned FFmpeg build (AV1 through libdav1d).
fn decode_video(path: &Path) -> (VideoStats, FrameReader) {
    let mut reader =
        FrameReader::open(path, StreamSelect::BestVideo).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(reader.kind(), MediaKind::Video);
    assert!(!reader.is_hardware());
    let mut stats: Option<VideoStats> = None;
    let mut previous = f64::NEG_INFINITY;
    while let Some(frame) = reader.next_frame().expect("decode") {
        let Frame::Video(f) = frame else {
            panic!("audio frame from the video stream");
        };
        let pts = f.pts().expect("pts").seconds();
        assert!(
            pts > previous,
            "{}: pts {pts} after {previous}",
            path.display()
        );
        previous = pts;
        let s = stats.get_or_insert_with(|| {
            // Native planes, not RGB: every plane is present and sized.
            let planes = f.planes();
            assert_eq!(planes.len(), f.format().plane_count());
            for p in &planes {
                assert_eq!(p.data.len(), p.stride * p.height as usize);
            }
            let rgba = f.to_rgba8().expect("rgba");
            assert_eq!(rgba.data.len(), (f.width() * f.height() * 4) as usize);
            assert!(rgba.data.chunks(4).all(|px| px[3] == 255), "opaque");
            VideoStats {
                frames: 0,
                first_pts: pts,
                last_pts: pts,
                width: f.width(),
                height: f.height(),
                format: f.format().name(),
            }
        });
        s.frames += 1;
        s.last_pts = pts;
    }
    (stats.expect("at least one frame"), reader)
}

#[test]
fn every_fixture_decodes() {
    let Some(files) = common::manifest() else {
        return;
    };
    for entry in &files {
        let file = entry["file"].as_str().expect("file");
        let kind = entry["kind"].as_str().expect("kind");
        let path = common::generated_dir().join(file);
        let info = probe(&path).unwrap_or_else(|e| panic!("{file}: {e}"));

        if kind != "audio" {
            let (stats, mut reader) = decode_video(&path);
            assert_eq!(
                (u64::from(stats.width), u64::from(stats.height)),
                (
                    entry["width"].as_u64().unwrap_or(0),
                    entry["height"].as_u64().unwrap_or(0)
                ),
                "{file}: size"
            );
            assert_eq!(
                Some(stats.format),
                entry["pix_fmt"].as_str(),
                "{file}: pixel format"
            );
            assert!(
                stats.first_pts.abs() < 1e-3,
                "{file}: first pts {}",
                stats.first_pts
            );

            let (stream, video) = info.video().expect("video");
            let expected = if let Some(frames) = entry["frames"].as_u64() {
                frames
            } else if let Some(frames) = stream.frames {
                frames
            } else {
                let rate = video.frame_rate.and_then(|r| r.to_f64()).expect("rate");
                let duration = info.duration.expect("duration").seconds();
                (rate * duration).round() as u64
            };
            if video.vfr.is_vfr {
                // Plausible: the average rate over the duration, within 10%.
                let avg = video
                    .avg_frame_rate
                    .and_then(|r| r.to_f64())
                    .expect("avg rate");
                let from_avg = avg * info.duration.expect("duration").seconds();
                let ratio = stats.frames as f64 / from_avg;
                assert!(
                    (0.9..=1.1).contains(&ratio),
                    "{file}: {} frames, {from_avg:.1} expected",
                    stats.frames
                );
                let nominal = entry["frame_rate"].as_str().map_or(0.0, |r| {
                    let (n, d) = r.split_once('/').expect("n/d");
                    n.parse::<f64>().expect("n") / d.parse::<f64>().expect("d")
                });
                assert!(
                    (stats.frames as f64) < nominal * info.duration.expect("duration").seconds(),
                    "{file}: a VFR clip has fewer frames than its nominal rate implies"
                );
            } else {
                assert_eq!(stats.frames, expected, "{file}: frame count");
            }

            // Seek to start: the first frame again.
            reader.seek_to_start().expect("seek to start");
            let again = reader
                .next_frame()
                .expect("decode")
                .unwrap_or_else(|| panic!("{file}: no frame after seek to start"));
            let pts = again.pts().expect("pts").seconds();
            assert!(
                (pts - stats.first_pts).abs() < 1e-6,
                "{file}: after seek {pts}"
            );
        }

        if let Some(a) = entry["audio"].as_object() {
            let mut reader = FrameReader::open(&path, StreamSelect::BestAudio).expect("open audio");
            let mut samples = 0u64;
            let mut formats = Vec::new();
            while let Some(frame) = reader.next_frame().expect("decode audio") {
                let f = frame.into_audio().expect("audio frame");
                assert_eq!(
                    Some(u64::from(f.sample_rate())),
                    a["sample_rate"].as_u64(),
                    "{file}"
                );
                assert_eq!(
                    Some(u64::from(f.channels())),
                    a["channels"].as_u64(),
                    "{file}"
                );
                let format = f.format().expect("format");
                if !formats.contains(&format) {
                    formats.push(format);
                }
                for p in 0..f.plane_count() {
                    assert!(
                        f.plane(p)
                            .is_some_and(|d| !d.is_empty() || f.samples() == 0)
                    );
                }
                samples += f.samples() as u64;
            }
            assert_eq!(formats.len(), 1, "{file}: one sample format");
            let native = formats[0];
            let codec = a["codec"].as_str().expect("codec");
            let expected_type = match codec {
                "pcm_s16le" => SampleType::I16,
                "pcm_s24le" => SampleType::I32,
                _ => SampleType::F32,
            };
            assert_eq!(
                native.sample, expected_type,
                "{file}: {codec} native format {native}"
            );
            let seconds = samples as f64 / a["sample_rate"].as_f64().expect("rate");
            let duration = entry["duration"].as_f64().expect("duration");
            if codec.starts_with("pcm") {
                assert_eq!(
                    samples as f64,
                    duration * 48000.0,
                    "{file}: PCM sample count"
                );
            } else {
                assert!(
                    (seconds - duration).abs() < 0.05,
                    "{file}: {seconds} s of audio"
                );
            }
        }
    }
}

fn ffmpeg_cli() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FFMPEG") {
        return Some(p.into());
    }
    if let Some(dir) = std::env::var_os("FFMPEG_DIR") {
        let p = Path::new(&dir).join("bin/ffmpeg");
        if p.is_file() {
            return Some(p);
        }
    }
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| PathBuf::from("ffmpeg"))
}

#[test]
fn decoded_frame_matches_ffmpeg_cli() {
    const FRAME: usize = 10;
    let Some(path) = common::fixture("h264_1080p30.mp4") else {
        return;
    };
    let Some(cli) = ffmpeg_cli() else {
        eprintln!("skipped: no ffmpeg command-line tool (set FFMPEG or FFMPEG_DIR)");
        return;
    };
    let mut reader = FrameReader::open(&path, StreamSelect::BestVideo).expect("open");
    let frame = reader
        .nth(FRAME)
        .expect("frame")
        .expect("decode")
        .into_video()
        .expect("video");
    let ours = frame.to_rgba8().expect("rgba");

    // The same conversion the helper documents for an untagged 1080p frame, with the
    // left chroma siting the H.264 decoder reports (the scale filter's "auto" siting
    // treats it as centred, so it is passed explicitly).
    assert_eq!(frame.colour_space().siting, media::ChromaSiting::Left);
    let filter = format!(
        "select=eq(n\\,{FRAME}),scale=in_color_matrix=bt709:in_range=tv:out_range=pc:in_chroma_loc=left:flags=bilinear+accurate_rnd+full_chroma_int,format=rgba"
    );
    let output = Command::new(&cli)
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-vf", &filter, "-frames:v", "1", "-f", "rawvideo", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reference = output.stdout;
    assert_eq!(reference.len(), ours.data.len());

    let mut max = 0u8;
    let mut total = 0u64;
    for (a, b) in ours.data.iter().zip(&reference) {
        let d = a.abs_diff(*b);
        max = max.max(d);
        total += u64::from(d);
    }
    let mean = total as f64 / reference.len() as f64;
    eprintln!("frame {FRAME}: mean abs difference {mean:.4}, max {max}");
    assert!(mean < 0.5, "mean abs difference {mean}");
    assert!(max <= 2, "max difference {max}");
}

#[test]
fn audio_only_file_has_no_video() {
    let Some(path) = common::fixture("audio_48k_stereo.wav") else {
        return;
    };
    let error = FrameReader::open(&path, StreamSelect::BestVideo).expect_err("no video");
    assert!(error.to_string().contains("no video stream"), "{error}");
}
