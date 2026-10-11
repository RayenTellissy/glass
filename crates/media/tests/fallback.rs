//! Automatic fallback to software: streams the hardware cannot decode fall back with the
//! right reason, a hardware failure mid-stream switches to software without losing or
//! repeating a frame, and `HwPreference::Require` errors instead of falling back.

mod common;

use media::{
    DecodePath, DecoderOptions, FallbackReason, FrameReader, HwPreference, MediaError,
    StreamSelect, Timestamp, hardware_decoder_available,
};

fn auto() -> DecoderOptions {
    DecoderOptions::with_hw(HwPreference::Auto)
}

/// Presentation times (seconds) of every video frame, and how many were hardware frames.
fn decode_all(reader: &mut FrameReader) -> (Vec<Timestamp>, usize) {
    let mut times = Vec::new();
    let mut hardware = 0;
    while let Some(frame) = reader.next_frame().expect("decode") {
        let frame = frame.into_video().expect("video");
        hardware += usize::from(frame.is_hardware());
        times.push(frame.pts().expect("pts"));
    }
    (times, hardware)
}

fn software_times(file: &str) -> Vec<Timestamp> {
    let path = common::generated_dir().join(file);
    let mut reader = FrameReader::open(&path, StreamSelect::BestVideo).expect("open");
    decode_all(&mut reader).0
}

fn expected_unsupported(codec: &str) -> FallbackReason {
    if cfg!(target_os = "macos") {
        FallbackReason::CodecUnsupported {
            codec: codec.to_string(),
        }
    } else {
        FallbackReason::NoHardwareApi
    }
}

#[test]
fn vp9_and_av1_fall_back_with_the_codec_reason() {
    for (file, codec, name) in [
        ("vp9_720p30.webm", "vp9", "VP9"),
        ("av1_1080p30.mp4", "av1", "AV1"),
    ] {
        let Some(path) = common::fixture(file) else {
            continue;
        };
        let mut reader = FrameReader::open_with(&path, StreamSelect::BestVideo, auto())
            .unwrap_or_else(|e| panic!("{file}: {e}"));
        if hardware_decoder_available(codec) {
            eprintln!("{file}: this machine decodes {codec} in hardware; no fallback to check");
            assert!(reader.is_hardware());
            continue;
        }
        let reason = expected_unsupported(codec);
        assert_eq!(
            reader.decode_path(),
            &DecodePath::Software {
                reason: reason.clone()
            },
            "{file}"
        );
        let message = reason.message();
        eprintln!("{file}: {message}");
        if cfg!(target_os = "macos") {
            assert!(message.contains(name), "{message}");
        }
        let (times, hardware) = decode_all(&mut reader);
        assert_eq!(hardware, 0);
        assert_eq!(times, software_times(file), "{file}: every frame, in order");
    }
}

#[test]
fn require_errors_instead_of_falling_back() {
    let Some(path) = common::fixture("vp9_720p30.webm") else {
        return;
    };
    if hardware_decoder_available("vp9") {
        eprintln!("skipped: this machine decodes VP9 in hardware");
        return;
    }
    let options = DecoderOptions::with_hw(HwPreference::Require);
    match FrameReader::open_with(&path, StreamSelect::BestVideo, options) {
        Err(MediaError::Hardware { reason, .. }) => {
            assert_eq!(reason, expected_unsupported("vp9"));
        }
        other => panic!("expected a hardware error, got {other:?}"),
    }
}

#[cfg(target_os = "macos")]
#[test]
fn hardware_failure_mid_stream_continues_in_software_from_the_same_frame() {
    const FAIL_AFTER: u64 = 20;
    let file = "h264_1080p30.mp4";
    let Some(path) = common::fixture(file) else {
        return;
    };
    let mut reader = FrameReader::open_with(&path, StreamSelect::BestVideo, auto()).expect("open");
    assert!(reader.is_hardware());
    reader.fail_hardware_after(FAIL_AFTER);
    let (times, hardware) = decode_all(&mut reader);
    let reference = software_times(file);
    assert_eq!(
        times, reference,
        "every frame once and in order across the switch"
    );
    assert!(
        hardware > 0 && hardware < times.len(),
        "{hardware} of {} frames in hardware",
        times.len()
    );
    let DecodePath::Software {
        reason: FallbackReason::DecodeFailed { codec, at, detail },
    } = reader.decode_path()
    else {
        panic!("expected a decode failure, got {:?}", reader.decode_path());
    };
    assert_eq!(codec, "h264");
    assert!(detail.contains("injected"), "{detail}");
    assert_eq!(
        *at,
        Some(reference[hardware - 1]),
        "resumed after the last hardware frame"
    );
    let message = reader
        .decode_path()
        .fallback_reason()
        .expect("reason")
        .message();
    eprintln!("{file}: {hardware} hardware frames, then software: {message}");
    assert!(message.contains("H.264"), "{message}");
}

#[cfg(target_os = "macos")]
#[test]
fn require_reports_a_mid_stream_failure() {
    let Some(path) = common::fixture("h264_1080p30.mp4") else {
        return;
    };
    let options = DecoderOptions::with_hw(HwPreference::Require);
    let mut reader = FrameReader::open_with(&path, StreamSelect::BestVideo, options).expect("open");
    reader.fail_hardware_after(5);
    let mut frames = 0;
    let error = loop {
        match reader.next_frame() {
            Ok(Some(_)) => frames += 1,
            Ok(None) => panic!("no error after {frames} frames"),
            Err(error) => break error,
        }
    };
    assert_eq!(frames, 5);
    assert!(error.to_string().contains("injected"), "{error}");
    assert!(reader.is_hardware(), "Require never switches to software");
}

fn ffmpeg_cli() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("FFMPEG") {
        return Some(p.into());
    }
    let dir = std::env::var_os("FFMPEG_DIR")?;
    let p = std::path::Path::new(&dir).join("bin/ffmpeg");
    p.is_file().then_some(p)
}

#[cfg(target_os = "macos")]
#[test]
fn formats_the_hardware_rejects_fall_back_on_the_first_frame() {
    // FFmpeg never offers VideoToolbox for RGB-coded H.264 (libx264rgb), so the decoder
    // drops to software on the first frame and the reader restarts it properly.
    let Some(cli) = ffmpeg_cli() else {
        eprintln!("skipped: no ffmpeg command-line tool (set FFMPEG or FFMPEG_DIR)");
        return;
    };
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("h264_rgb_320x240.mp4");
    let status = std::process::Command::new(&cli)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=320x240:rate=30")
        .args(["-t", "1", "-c:v", "libx264rgb"])
        .arg(&path)
        .status()
        .expect("run ffmpeg");
    assert!(status.success());

    let mut reader = FrameReader::open_with(&path, StreamSelect::BestVideo, auto()).expect("open");
    let (times, hardware) = decode_all(&mut reader);
    assert_eq!(hardware, 0);
    let mut software = FrameReader::open(&path, StreamSelect::BestVideo).expect("open");
    assert_eq!(times, decode_all(&mut software).0, "every frame, in order");
    assert_eq!(times.len(), 30);
    let reason = reader
        .decode_path()
        .fallback_reason()
        .expect("software")
        .clone();
    assert_eq!(
        reason,
        FallbackReason::FormatUnsupported {
            codec: "h264".into(),
            format: "gbrp".into()
        }
    );
    eprintln!("RGB H.264: {}", reason.message());
    assert!(reason.message().contains("8-bit RGB H.264"));

    let options = DecoderOptions::with_hw(HwPreference::Require);
    let mut reader = FrameReader::open_with(&path, StreamSelect::BestVideo, options).expect("open");
    assert!(matches!(
        reader.next_frame(),
        Err(MediaError::Hardware { .. })
    ));
}
