//! Export encode speed: `media::export::ExportSession` per codec and encoder, fed packed
//! 8-bit RGBA pictures (as the render readback ring delivers them) and converted to the
//! encoder's format inside the session, at 1080p for every codec and at 4K for H.264
//! and HEVC (VideoToolbox against x264 and x265). Each case prints the median of 3 runs
//! as frames per second, including opening the encoder and draining it at the end (the
//! clips are short, so x264's and x265's look-ahead flush weighs in a little). Audio
//! cases encode 10 s of 48 kHz stereo and print times realtime. Codecs and encoders this
//! FFmpeg build or platform lacks are skipped. Run alone with
//! `cargo bench -p media --bench export`.

use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use media::Rational;
use media::export::{
    AudioCodec, AudioSettings, Container, ExportSession, ExportSettings, Picture, ProResProfile,
    VideoCodec, VideoSettings,
};

/// Distinct pictures per size, cycled through.
fn distinct(width: u32) -> usize {
    if width > 1920 { 8 } else { 30 }
}

fn pictures(w: u32, h: u32) -> Vec<Vec<u8>> {
    (0..distinct(w))
        .map(|n| {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            let t = n as f32 / 30.0;
            for y in 0..h {
                for x in 0..w {
                    let u = x as f32 / w as f32;
                    let v = y as f32 / h as f32;
                    let d = ((u - 0.3 - 0.4 * t).powi(2) + (v - 0.5).powi(2)).sqrt();
                    let disc = ((0.15 - d) * 60.0).clamp(0.0, 1.0);
                    let r = (0.15 + 0.7 * u) * (1.0 - disc) + disc;
                    let g = (0.2 + 0.6 * v) * (1.0 - disc) + 0.8 * disc;
                    let b = 0.5 + 0.2 * (u * 9.0 + t * 3.0).sin();
                    out.extend_from_slice(&[
                        (r * 255.0) as u8,
                        (g * 255.0) as u8,
                        (b * 255.0) as u8,
                        255,
                    ]);
                }
            }
            out
        })
        .collect()
}

fn encode_video(dir: &Path, case: &Case, pictures: &[Vec<u8>]) -> Option<(Duration, String)> {
    let (w, h) = case.size;
    let mut video = VideoSettings::new(case.codec, w, h, Rational::new(30, 1));
    video.encoder = case.encoder.map(str::to_string);
    let settings = ExportSettings::new(case.container).with_video(video);
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).ok()?;
    let path = match case.container {
        Container::ImageSequence => dir.join(format!("f_%05d.{}", case.extension)),
        _ => dir.join(format!("out.{}", case.extension)),
    };
    let start = Instant::now();
    let mut session = match ExportSession::create(&path, &settings) {
        Ok(s) => s,
        Err(error) => {
            eprintln!("{}: skipped ({error})", case.name);
            return None;
        }
    };
    for n in 0..case.frames {
        let data = &pictures[n % pictures.len()];
        let picture = Picture::rgba8(w, h, data, w as usize * 4);
        session.push_picture(&picture).expect("picture");
    }
    let encoder = session.video_encoder().unwrap_or("?").to_string();
    session.finish().expect("finish");
    Some((start.elapsed(), encoder))
}

struct Case {
    name: &'static str,
    size: (u32, u32),
    codec: VideoCodec,
    container: Container,
    extension: &'static str,
    encoder: Option<&'static str>,
    frames: usize,
}

const HD: (u32, u32) = (1920, 1080);
const UHD: (u32, u32) = (3840, 2160);

/// `encoder: None` is the codec's default (ProRes: VideoToolbox on macOS).
const CASES: &[Case] = &[
    Case {
        size: HD,
        name: "H.264 (CRF 23, medium)",
        codec: VideoCodec::H264,
        container: Container::Mp4,
        extension: "mp4",
        encoder: Some("libx264"),
        frames: 120,
    },
    Case {
        size: HD,
        name: "HEVC (CRF 28, medium)",
        codec: VideoCodec::Hevc,
        container: Container::Mp4,
        extension: "mp4",
        encoder: Some("libx265"),
        frames: 60,
    },
    Case {
        size: HD,
        name: "H.264 VideoToolbox",
        codec: VideoCodec::H264,
        container: Container::Mp4,
        extension: "mp4",
        encoder: Some("h264_videotoolbox"),
        frames: 120,
    },
    Case {
        size: HD,
        name: "HEVC VideoToolbox",
        codec: VideoCodec::Hevc,
        container: Container::Mp4,
        extension: "mp4",
        encoder: Some("hevc_videotoolbox"),
        frames: 120,
    },
    Case {
        size: UHD,
        name: "H.264 (CRF 23, medium)",
        codec: VideoCodec::H264,
        container: Container::Mp4,
        extension: "mp4",
        encoder: Some("libx264"),
        frames: 30,
    },
    Case {
        size: UHD,
        name: "HEVC (CRF 28, medium)",
        codec: VideoCodec::Hevc,
        container: Container::Mp4,
        extension: "mp4",
        encoder: Some("libx265"),
        frames: 15,
    },
    Case {
        size: UHD,
        name: "H.264 VideoToolbox",
        codec: VideoCodec::H264,
        container: Container::Mp4,
        extension: "mp4",
        encoder: Some("h264_videotoolbox"),
        frames: 60,
    },
    Case {
        size: UHD,
        name: "HEVC VideoToolbox",
        codec: VideoCodec::Hevc,
        container: Container::Mp4,
        extension: "mp4",
        encoder: Some("hevc_videotoolbox"),
        frames: 60,
    },
    Case {
        size: HD,
        name: "ProRes 422 HQ",
        codec: VideoCodec::ProRes(ProResProfile::Hq),
        container: Container::Mov,
        extension: "mov",
        encoder: None,
        frames: 120,
    },
    Case {
        size: HD,
        name: "ProRes 4444",
        codec: VideoCodec::ProRes(ProResProfile::P4444),
        container: Container::Mov,
        extension: "mov",
        encoder: None,
        frames: 120,
    },
    Case {
        size: HD,
        name: "ProRes 422 HQ (software)",
        codec: VideoCodec::ProRes(ProResProfile::Hq),
        container: Container::Mov,
        extension: "mov",
        encoder: Some("prores_ks"),
        frames: 60,
    },
    Case {
        size: HD,
        name: "PNG sequence",
        codec: VideoCodec::Png,
        container: Container::ImageSequence,
        extension: "png",
        encoder: None,
        frames: 30,
    },
    Case {
        size: HD,
        name: "TIFF sequence",
        codec: VideoCodec::Tiff,
        container: Container::ImageSequence,
        extension: "tif",
        encoder: None,
        frames: 30,
    },
    Case {
        size: HD,
        name: "GIF",
        codec: VideoCodec::Gif,
        container: Container::Gif,
        extension: "gif",
        encoder: None,
        frames: 10,
    },
];

fn encode_audio(path: &Path, codec: AudioCodec, container: Container) -> Option<Duration> {
    let settings = ExportSettings::new(container).with_audio(AudioSettings::new(codec, 48_000, 2));
    let block: Vec<Vec<f32>> = (0..2)
        .map(|c| {
            (0..1024)
                .map(|i| 0.25 * (i as f32 * 0.0577 * (c + 1) as f32).sin())
                .collect()
        })
        .collect();
    let planes: Vec<&[f32]> = block.iter().map(Vec::as_slice).collect();
    let start = Instant::now();
    let mut session = ExportSession::create(path, &settings).ok()?;
    for _ in 0..(480_000 / 1024) {
        session.push_audio(&planes).expect("audio");
    }
    session.finish().expect("finish");
    Some(start.elapsed())
}

fn median(mut times: Vec<Duration>) -> f64 {
    times.sort_unstable();
    times[times.len() / 2].as_secs_f64()
}

fn export(c: &mut Criterion) {
    let scratch = std::env::temp_dir().join(format!("glass-export-bench-{}", std::process::id()));
    let hd = pictures(HD.0, HD.1);
    let mut uhd = None;
    for case in CASES {
        let pictures = if case.size == HD {
            &hd
        } else {
            uhd.get_or_insert_with(|| pictures(UHD.0, UHD.1))
        };
        let dir = scratch.join(case.extension);
        let mut times = Vec::new();
        let mut encoder = String::new();
        for _ in 0..3 {
            match encode_video(&dir, case, pictures) {
                Some((t, e)) => {
                    times.push(t);
                    encoder = e;
                }
                None => break,
            }
        }
        if times.is_empty() {
            continue;
        }
        let seconds = median(times);
        eprintln!(
            "{} ({encoder}): {}x{}, {} frames in {seconds:.3} s median: {:.1} fps",
            case.name,
            case.size.0,
            case.size.1,
            case.frames,
            case.frames as f64 / seconds
        );
    }
    for (name, codec, container, extension) in [
        ("AAC 256 kbit/s", AudioCodec::Aac, Container::M4a, "m4a"),
        (
            "FLAC 24-bit",
            AudioCodec::Flac { bits: 24 },
            Container::Flac,
            "flac",
        ),
    ] {
        std::fs::create_dir_all(&scratch).expect("scratch");
        let path = scratch.join(format!("audio.{extension}"));
        let times: Vec<Duration> = (0..3)
            .filter_map(|_| encode_audio(&path, codec, container))
            .collect();
        if times.is_empty() {
            continue;
        }
        let seconds = median(times);
        eprintln!(
            "{name}: 10 s of 48 kHz stereo in {seconds:.3} s median: {:.0}x realtime",
            10.0 / seconds
        );
    }

    let mut group = c.benchmark_group("export");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    let h264 = &CASES[0];
    let dir = scratch.join("criterion");
    group.bench_function("h264_1080p_120_frames", |b| {
        b.iter(|| encode_video(&dir, h264, &hd).expect("encode"));
    });
    group.finish();
    let _ = std::fs::remove_dir_all(&scratch);
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(Duration::from_secs(1));
    targets = export
}
criterion_main!(benches);
