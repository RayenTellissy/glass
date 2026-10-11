//! Output verification: presets encode half a second through `media::export` and the
//! files verify clean against their settings and preset; damaged or wrong files (cut
//! short, a frame count that disagrees, broadcast audio far too loud, a file over a
//! size limit, not a media file at all) give exactly the expected findings.
//!
//! H.264 runs on x264 so every machine writes the same bitstream; the broadcast preset's
//! ProRes uses VideoToolbox on macOS and `prores_ks` with `codec-prores-sw`, and is
//! skipped on builds with neither.

use std::path::{Path, PathBuf};

use media::Rational;
use media::analysis::Unattended;
use media::export::presets::{Limit, Preset, TimelineFormat};
use media::export::verify::{Check, Expected, Finding, Severity, VerifyReport, verify};
use media::export::{
    Container, EncoderPreference, ExportSession, ExportSettings, Picture, VideoCodec,
};

const TIMELINE: TimelineFormat = TimelineFormat {
    width: 1920,
    height: 1080,
    frame_rate: Rational::new(30, 1),
    audio_channels: 2,
};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-verify-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch folder");
    dir
}

/// The preset's settings on the deterministic encoders, or `None` when this build has
/// no ProRes encoder.
fn settings_for(preset: &Preset) -> Option<ExportSettings> {
    let mut settings = preset.resolve(&TIMELINE).expect("preset resolves");
    let video = settings.video.as_mut().expect("video");
    if matches!(video.codec, VideoCodec::ProRes(_)) {
        if !cfg!(feature = "codec-prores-sw") && !cfg!(target_os = "macos") {
            eprintln!("skipping {}: no ProRes encoder in this build", preset.id);
            return None;
        }
    } else {
        video.encoder_preference = EncoderPreference::Software;
    }
    Some(settings)
}

/// An export of half a second: the file, its settings and the frames written.
struct Output {
    dir: PathBuf,
    path: PathBuf,
    settings: ExportSettings,
    frames: u64,
}

impl Output {
    fn expected<'a>(&'a self, preset: &Preset) -> Expected<'a> {
        Expected::new(&self.settings, self.frames, 0.5).with_preset(preset)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Encodes half a second of moving gradient and a 1 kHz sine of peak `amplitude` on
/// every channel (a stereo 1 kHz sine at -23 dBFS peak measures -23 LUFS).
fn encode(name: &str, preset: &Preset, amplitude: f64) -> Option<Output> {
    let settings = settings_for(preset)?;
    let v = settings.video.as_ref().expect("video");
    let a = settings.audio.as_ref().expect("audio");
    let dir = scratch(name);
    let path = dir.join(format!("out.{}", settings.container.extension()));
    let frames = (v.frame_rate.to_f64().expect("rate") * 0.5).round() as u64;
    let mut session = ExportSession::create(&path, &settings).expect("export opens");
    let (w, h) = (v.width, v.height);
    for n in 0..frames as u32 {
        let mut data = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h {
            for x in 0..w {
                let r = ((x + n * 16) * 255 / w) as u8;
                data.extend_from_slice(&[r, (y * 255 / h) as u8, 90, 255]);
            }
        }
        let picture = Picture::rgba8(w, h, &data, w as usize * 4);
        session.push_picture(&picture).expect("picture");
    }
    let rate = a.sample_rate as usize;
    let total = rate / 2;
    let mut at = 0;
    while at < total {
        let len = 1000.min(total - at);
        let plane: Vec<f32> = (at..at + len)
            .map(|i| {
                let t = i as f64 / rate as f64;
                (amplitude * (std::f64::consts::TAU * 1000.0 * t).sin()) as f32
            })
            .collect();
        let planes = vec![plane.as_slice(); a.channels];
        session.push_audio(&planes).expect("audio");
        at += len;
    }
    let summary = session.finish().expect("export finishes");
    assert_eq!(summary.frames, frames);
    Some(Output {
        dir,
        path,
        settings,
        frames,
    })
}

fn run(path: &Path, expected: &Expected<'_>) -> VerifyReport {
    verify(path, expected, &Unattended).expect("verifies")
}

fn keys(report: &VerifyReport) -> Vec<(&'static str, Severity)> {
    report
        .findings
        .iter()
        .map(|f| (f.key, f.severity))
        .collect()
}

fn preset(id: &str) -> &'static Preset {
    Preset::built_in(id).expect("built-in preset")
}

/// -14 dBFS peak: the streaming presets' -14 LUFS.
const STREAMING: f64 = 0.2;
/// -23 dBFS peak: EBU R128's -23 LUFS.
const EBU: f64 = 0.070_8;

#[test]
fn youtube_export_verifies_clean() {
    let yt = preset("youtube_1080p");
    let out = encode("youtube", yt, STREAMING).expect("H.264 is always available");
    let report = run(&out.path, &out.expected(yt));
    assert_eq!(report.findings, [], "{:?}", report.measured);
    assert_eq!(report.measured.frames, Some(15));
    let lufs = report.measured.integrated_lufs.expect("loudness");
    assert!((lufs + 14.0).abs() < 1.0, "{lufs} LUFS");
    for check in [
        Check::Readable,
        Check::Container,
        Check::VideoCodec,
        Check::Resolution,
        Check::FrameRate,
        Check::FrameCount,
        Check::VideoDuration,
        Check::AudioCodec,
        Check::SampleRate,
        Check::Channels,
        Check::AudioDuration,
        Check::Loudness,
        Check::TruePeak,
        Check::FileSize,
        Check::MaxDuration,
        Check::Aspect,
        Check::Colour,
    ] {
        assert!(report.checked.contains(&check), "{check:?} did not run");
    }
}

#[test]
fn broadcast_export_verifies_clean() {
    let ebu = preset("broadcast_ebu_1080p25");
    let Some(out) = encode("broadcast", ebu, EBU) else {
        return;
    };
    let report = run(&out.path, &out.expected(ebu));
    assert_eq!(report.findings, [], "{:?}", report.measured);
    assert_eq!(report.measured.frames, Some(13));
    assert_eq!(report.measured.samples, Some(24_000));
}

#[test]
fn wrong_frame_count_and_container_are_reported() {
    let yt = preset("youtube_1080p");
    let out = encode("frames", yt, STREAMING).expect("H.264 is always available");
    let mut expected = out.expected(yt);
    expected.frames = 17;
    let report = run(&out.path, &expected);
    assert_eq!(
        report.findings,
        [Finding {
            check: Check::FrameCount,
            severity: Severity::Failure,
            key: "export.verify.frame_count",
            args: vec![("expected", "17".into()), ("found", "15".into())],
        }]
    );
    assert!(!report.passed());

    // The same file checked as QuickTime: MP4 and MOV share FFmpeg's demuxer and differ
    // by brand.
    let mut as_mov = out.settings.clone();
    as_mov.container = Container::Mov;
    let report = run(&out.path, &Expected::new(&as_mov, out.frames, 0.5));
    assert_eq!(
        keys(&report),
        [("export.verify.container", Severity::Failure)]
    );
    assert_eq!(report.findings[0].arg("expected"), Some("mov"));
}

#[test]
fn truncated_file_is_reported() {
    let x = preset("x_1080p");
    let out = encode("truncated", x, STREAMING).expect("H.264 is always available");
    let full = std::fs::read(&out.path).expect("read");
    let cut = out.dir.join("cut.mp4");
    std::fs::write(&cut, &full[..full.len() * 3 / 5]).expect("write");
    let report = run(&cut, &out.expected(x));
    assert_eq!(
        keys(&report),
        [
            ("export.verify.frame_count", Severity::Failure),
            ("export.verify.video.duration", Severity::Failure),
            ("export.verify.audio.duration", Severity::Failure),
        ],
        "{:?}",
        report.findings
    );
    let found: u64 = report.findings[0]
        .arg("found")
        .and_then(|n| n.parse().ok())
        .expect("count");
    assert!(found < 15, "{found} frames");
    assert_eq!(report.findings[0].arg("expected"), Some("15"));
}

#[test]
fn loud_audio_fails_the_broadcast_target() {
    let ebu = preset("broadcast_ebu_1080p25");
    let Some(out) = encode("loud", ebu, 0.99) else {
        return;
    };
    let report = run(&out.path, &out.expected(ebu));
    assert_eq!(
        keys(&report),
        [
            ("export.verify.loudness.integrated", Severity::Failure),
            ("export.verify.loudness.true_peak", Severity::Failure),
        ]
    );
    let integrated = &report.findings[0];
    assert_eq!(integrated.arg("target"), Some("-23"));
    assert_eq!(integrated.arg("tolerance"), Some("0.5"));
    let found: f64 = integrated
        .arg("found")
        .and_then(|n| n.parse().ok())
        .expect("reading");
    assert!(found > -1.0, "{found} LUFS");
    assert_eq!(report.findings[1].arg("max"), Some("-1"));

    // The same file against a streaming preset's soft target only warns.
    let mut soft = out.expected(ebu);
    soft.loudness = preset("youtube_1080p").loudness;
    let report = run(&out.path, &soft);
    assert_eq!(
        keys(&report),
        [
            ("export.verify.loudness.integrated", Severity::Warning),
            ("export.verify.loudness.true_peak", Severity::Warning),
        ]
    );
    assert!(report.passed());
}

#[test]
fn limits_fail_when_hard_and_warn_when_soft() {
    let yt = preset("youtube_1080p");
    let out = encode("limits", yt, STREAMING).expect("H.264 is always available");
    let mut expected = out.expected(yt);
    expected.limits.max_file_size = Some(Limit::hard(100 << 10));
    expected.limits.max_duration = Some(Limit::soft(0));
    expected.limits.aspect = Some(Limit::soft((9, 16)));
    let report = run(&out.path, &expected);
    assert_eq!(
        report.findings[0],
        Finding {
            check: Check::FileSize,
            severity: Severity::Failure,
            key: "export.verify.limit.file_size",
            args: vec![
                (
                    "found",
                    report.findings[0].arg("found").unwrap_or("").into()
                ),
                ("limit", "0.1".into()),
            ],
        }
    );
    assert_eq!(
        keys(&report),
        [
            ("export.verify.limit.file_size", Severity::Failure),
            ("export.verify.limit.duration", Severity::Warning),
            ("export.verify.limit.aspect", Severity::Warning),
        ]
    );
    assert_eq!(report.findings[1].arg("found"), Some("0.5"));
    assert_eq!(report.findings[2].arg("found"), Some("1920x1080"));
    assert_eq!(report.findings[2].arg("expected"), Some("9:16"));
}

#[test]
fn a_file_that_is_not_media_is_unreadable() {
    let yt = preset("youtube_1080p");
    let settings = settings_for(yt).expect("settings");
    let dir = scratch("garbage");
    let path = dir.join("out.mp4");
    std::fs::write(&path, b"not a video at all").expect("write");
    let report = run(&path, &Expected::new(&settings, 15, 0.5).with_preset(yt));
    assert_eq!(
        keys(&report),
        [("export.verify.unreadable", Severity::Failure)]
    );
    let _ = std::fs::remove_dir_all(dir);
}
