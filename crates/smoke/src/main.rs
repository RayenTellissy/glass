//! Headless check binary: opens the reference project, plays a short range and exports
//! a short range without a window, then exits 0. It starts as a stub and grows with
//! each phase.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use engine::logging::{self, LogFilter};

const STEPS: &[&str] = &[
    "open tests/fixtures/reference.proj",
    "play 2 s headless",
    "export 1 s",
];

fn main() -> ExitCode {
    let ring = match logging::init() {
        Ok(ring) => ring,
        Err(error) => {
            eprintln!("smoke: logging: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!("smoke started");
    // GLASS_LOG may silence info; the ring is only checked when the event passed the filter.
    if tracing::enabled!(tracing::Level::INFO) && ring.snapshot(&LogFilter::default()).is_empty() {
        eprintln!("smoke: the log ring did not record the start event");
        return ExitCode::FAILURE;
    }

    if let Err(message) = decode_check() {
        eprintln!("smoke: {message}");
        return ExitCode::FAILURE;
    }

    println!("smoke: still to come:");
    for step in STEPS {
        println!("  - {step}");
    }
    println!("smoke: ok");
    ExitCode::SUCCESS
}

/// The H.264 fixture from `tests/fixtures/generate.sh`; `GLASS_FIXTURES` points elsewhere.
fn fixture_path() -> PathBuf {
    std::env::var_os("GLASS_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated")
        })
        .join("h264_1080p30.mp4")
}

/// Probes the H.264 fixture and decodes its first 2 s of video and audio.
fn decode_check() -> Result<(), String> {
    const SECONDS: f64 = 2.0;
    let path = fixture_path();
    if !path.is_file() {
        println!(
            "smoke: decode check skipped, {} missing (run tests/fixtures/generate.sh)",
            path.display()
        );
        return Ok(());
    }
    let check = engine::media_check::probe_and_decode(&path, SECONDS).map_err(|e| e.to_string())?;
    let Some((stream, video)) = check.info.video() else {
        return Err(format!("{}: probe found no video", path.display()));
    };
    let rate = video.frame_rate.and_then(|r| r.to_f64()).unwrap_or(0.0);
    let expected = (rate * SECONDS).round() as u64;
    if check.video_frames != expected {
        return Err(format!(
            "decoded {} video frames in {SECONDS} s, expected {expected}",
            check.video_frames
        ));
    }
    if check.first_video_pts.map(|t| t.seconds()) != Some(0.0) {
        return Err(format!("first video frame at {:?}", check.first_video_pts));
    }
    let audio_seconds = check
        .info
        .audio()
        .map(|(_, a)| check.audio_samples as f64 / f64::from(a.sample_rate.max(1)))
        .unwrap_or(0.0);
    println!(
        "smoke: decoded {} {}x{} {} frames and {:.2} s of audio from {} in {} ms",
        check.video_frames,
        video.width,
        video.height,
        stream.codec.name,
        audio_seconds,
        path.file_name()
            .map_or_else(Default::default, |n| n.to_string_lossy()),
        check.elapsed.as_millis()
    );
    Ok(())
}
