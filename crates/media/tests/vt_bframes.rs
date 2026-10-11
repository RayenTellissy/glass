//! VideoToolbox exports with B-frames: every built-in H.264 and HEVC preset (each asks
//! for 2 B-frames) encodes one second of generated pictures and a tone through
//! `media::export` with the default preference (Auto) and with Hardware, on VideoToolbox.
//! The file is probed for its codec, read packet by packet for reordering, verified
//! against its settings and preset, and decoded and compared with the source pictures.
//! The same presets on the software encoders are the control.
//!
//! Every picture is different (a bar moves a thirtieth of the width per frame), so frames
//! decoded out of order or dropped fail the comparison. macOS only.

use std::path::{Path, PathBuf};

use media::analysis::Unattended;
use media::export::presets::{BUILT_IN, Preset, TimelineFormat};
use media::export::verify::{Expected, verify};
use media::export::{EncoderPreference, ExportSession, ExportSettings, Picture, VideoCodec};
use media::{Demuxer, Frame, FrameReader, MediaKind, Rational, StreamSelect, probe};

const TIMELINE: TimelineFormat = TimelineFormat {
    width: 1920,
    height: 1080,
    frame_rate: Rational::new(30, 1),
    audio_channels: 2,
};

/// Lowest RGB PSNR any decoded frame may have against its source picture.
const MIN_PSNR: f64 = 33.0;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-vt-bframes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch folder");
    dir
}

/// Frame `n`: red across, green down, and a blue bar an eighth of the width wide that
/// moves a thirtieth of the width per frame, as packed 8-bit RGBA.
fn picture(n: u64, width: u32, height: u32) -> Vec<u8> {
    let at = (n as u32 * width / 30) % width;
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let g = (y * 255 / height) as u8;
        for x in 0..width {
            let bar = (x + width - at) % width < width / 8;
            let r = (x * 255 / width) as u8;
            out.extend_from_slice(&[r, g, if bar { 230 } else { 40 }, 255]);
        }
    }
    out
}

/// A 440 Hz sine at -12 dBFS on every channel, `len` samples from `from`.
fn tone(channels: usize, rate: u32, from: usize, len: usize) -> Vec<Vec<f32>> {
    let plane: Vec<f32> = (from..from + len)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            (0.25 * (std::f64::consts::TAU * 440.0 * t).sin()) as f32
        })
        .collect();
    vec![plane; channels]
}

/// PSNR in dB of `a` against `b` (packed RGBA, alpha ignored).
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mut sum = 0u64;
    let mut n = 0u64;
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for c in 0..3 {
            let d = u64::from(pa[c].abs_diff(pb[c]));
            sum += d * d;
            n += 1;
        }
    }
    if sum == 0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 * n as f64 / sum as f64).log10()
}

/// The built-in presets whose video is H.264 or HEVC.
fn h264_and_hevc_presets() -> Vec<&'static Preset> {
    let presets: Vec<&Preset> = BUILT_IN
        .iter()
        .filter(|p| matches!(p.video.codec, VideoCodec::H264 | VideoCodec::Hevc))
        .collect();
    assert!(
        presets.iter().any(|p| p.video.codec == VideoCodec::H264)
            && presets.iter().any(|p| p.video.codec == VideoCodec::Hevc),
        "{:?}",
        presets.iter().map(|p| p.id).collect::<Vec<_>>()
    );
    presets
}

/// The FFmpeg encoder `preference` must run for `codec`.
fn expected_encoder(codec: VideoCodec, preference: EncoderPreference) -> &'static str {
    match (codec, preference) {
        (VideoCodec::H264, EncoderPreference::Software) => "libx264",
        (VideoCodec::Hevc, EncoderPreference::Software) => "libx265",
        (VideoCodec::H264, _) => "h264_videotoolbox",
        (VideoCodec::Hevc, _) => "hevc_videotoolbox",
        (other, _) => panic!("unexpected codec {other:?}"),
    }
}

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        let held: bool = $cond;
        if !held {
            return Err(format!($($arg)+));
        }
    };
}

/// Exports one second of the preset with `preference` and checks the file; the first
/// problem found, as text.
fn check_preset(preset: &Preset, preference: EncoderPreference) -> Result<(), String> {
    let mut settings = preset
        .resolve(&TIMELINE)
        .map_err(|e| format!("resolves: {e}"))?;
    settings.video.as_mut().expect("video").encoder_preference = preference;
    let dir = scratch(&format!("{}-{preference:?}", preset.id));
    let path = dir.join(format!("out.{}", settings.container.extension()));
    let result = export_and_check(preset, &settings, preference, &path);
    let _ = std::fs::remove_dir_all(dir);
    result
}

fn export_and_check(
    preset: &Preset,
    settings: &ExportSettings,
    preference: EncoderPreference,
    path: &Path,
) -> Result<(), String> {
    let v = settings.video.as_ref().expect("video");
    let a = settings.audio.as_ref().expect("audio");
    let (w, h) = (v.width, v.height);
    let frames = v.frame_rate.to_f64().expect("rate").round() as u64;

    let mut session = ExportSession::create(path, settings).map_err(|e| format!("opens: {e}"))?;
    for n in 0..frames {
        let data = picture(n, w, h);
        session
            .push_picture(&Picture::rgba8(w, h, &data, w as usize * 4))
            .map_err(|e| format!("picture {n}: {e}"))?;
    }
    let samples = a.sample_rate as usize;
    let mut at = 0;
    while at < samples {
        let len = 1000.min(samples - at);
        let block = tone(a.channels, a.sample_rate, at, len);
        let planes: Vec<&[f32]> = block.iter().map(Vec::as_slice).collect();
        session
            .push_audio(&planes)
            .map_err(|e| format!("audio at {at}: {e}"))?;
        at += len;
    }
    let summary = session.finish().map_err(|e| format!("finishes: {e}"))?;

    let encoder = expected_encoder(v.codec, preference);
    ensure!(
        summary.video_encoder.as_deref() == Some(encoder) && summary.fallbacks.is_empty(),
        "ran {:?} with fallbacks {:?}, expected {encoder}",
        summary.video_encoder,
        summary.fallbacks
    );
    ensure!(
        summary.frames == frames,
        "summary has {} frames, expected {frames}",
        summary.frames
    );

    let info = probe(path).map_err(|e| format!("probe: {e}"))?;
    let (stream, _) = info.video().ok_or("no video stream")?;
    let codec = if v.codec == VideoCodec::H264 {
        "h264"
    } else {
        "hevc"
    };
    ensure!(
        stream.codec.name == codec,
        "probes as {}, expected {codec}",
        stream.codec.name
    );

    if preset.video.max_b_frames.is_some_and(|b| b > 0) {
        // B-frames are sent after the frames they refer to: presentation times in
        // packet order go back at least once.
        let mut demuxer = Demuxer::open(path).map_err(|e| format!("demux: {e}"))?;
        let index = demuxer.best_stream(MediaKind::Video).ok_or("no video")?;
        let mut pts = Vec::new();
        while let Some(packet) = demuxer.read_packet().map_err(|e| format!("read: {e}"))? {
            if packet.stream_index() == index {
                pts.push(packet.pts().ok_or("packet without pts")?.value);
            }
        }
        ensure!(
            pts.windows(2).any(|p| p[1] < p[0]),
            "no B-frames: presentation order equals decode order ({} packets)",
            pts.len()
        );
    }

    let expected = Expected::new(settings, frames, 1.0).with_preset(preset);
    let report = verify(path, &expected, &Unattended).map_err(|e| format!("verify: {e}"))?;
    let failures: Vec<_> = report.failures().collect();
    ensure!(failures.is_empty(), "verify failures: {failures:?}");
    ensure!(
        report.measured.frames == Some(frames),
        "verify read {:?} frames, expected {frames}",
        report.measured.frames
    );
    let duration = report.measured.video_duration.ok_or("no video duration")?;
    ensure!(
        (duration - 1.0).abs() < 1e-3,
        "video lasts {duration} s, expected 1 s"
    );

    let mut reader =
        FrameReader::open(path, StreamSelect::BestVideo).map_err(|e| format!("reader: {e}"))?;
    let mut n = 0u64;
    let mut worst = f64::INFINITY;
    while let Some(frame) = reader.next_frame().map_err(|e| format!("decode: {e}"))? {
        let Frame::Video(frame) = frame else { continue };
        let rgba = frame.to_rgba8().map_err(|e| format!("rgba: {e}"))?;
        let p = psnr(&rgba.data, &picture(n, w, h));
        ensure!(p > MIN_PSNR, "frame {n}: RGB PSNR {p:.2} dB");
        worst = worst.min(p);
        n += 1;
    }
    ensure!(n == frames, "decoded {n} frames, expected {frames}");
    eprintln!(
        "{} {preference:?} ({encoder}): {} bytes, worst frame {worst:.2} dB",
        preset.id, summary.bytes
    );
    Ok(())
}

/// Runs every H.264 and HEVC preset with `preference` and fails with every preset's
/// problem at once.
fn check_all(preference: EncoderPreference) {
    if !cfg!(target_os = "macos") {
        eprintln!("SKIPPED VideoToolbox B-frame exports: VideoToolbox is macOS only");
        return;
    }
    let failures: Vec<String> = h264_and_hevc_presets()
        .into_iter()
        .filter_map(|p| {
            check_preset(p, preference)
                .err()
                .map(|e| format!("{}: {e}", p.id))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{preference:?}:\n{}",
        failures.join("\n")
    );
}

#[test]
fn presets_with_b_frames_export_on_videotoolbox_by_default() {
    check_all(EncoderPreference::Auto);
}

#[test]
fn presets_with_b_frames_export_on_videotoolbox_when_hardware_is_required() {
    check_all(EncoderPreference::Hardware);
}

#[test]
fn presets_with_b_frames_export_on_the_software_encoders() {
    check_all(EncoderPreference::Software);
}
