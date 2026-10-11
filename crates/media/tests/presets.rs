//! Built-in export presets: each resolves against a 1080p30 stereo timeline and encodes
//! half a second of generated pictures and a tone through `media::export`, and the file
//! is probed and decoded for codec, size, frame rate, frame count, colour tags, pixel
//! format and audio parameters; the HDR preset also for its mastering display and
//! content light side data.
//!
//! H.264 and HEVC run on the software encoders so every machine checks the same
//! bitstream; ProRes uses VideoToolbox on macOS (its only encoder without the
//! `codec-prores-sw` feature) and `prores_ks` with it.

use std::path::{Path, PathBuf};

use media::export::presets::{BUILT_IN, Preset, PresetCategory, TimelineFormat};
use media::export::{
    AudioCodec, EncoderPreference, ExportSession, ExportSettings, Picture, VideoCodec,
};
use media::{ColourRange, Frame, FrameReader, Rational, StreamSelect, probe};

const TIMELINE: TimelineFormat = TimelineFormat {
    width: 1920,
    height: 1080,
    frame_rate: Rational::new(30, 1),
    audio_channels: 2,
};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-preset-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch folder");
    dir
}

/// Two RGBA8 pictures of gradients and a bar in different places, cheap to build at 4K.
fn pictures(width: u32, height: u32) -> [Vec<u8>; 2] {
    let make = |shift: u32| {
        let mut out = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let bar = (x + shift) % width < width / 8;
                let r = (x * 255 / width) as u8;
                let g = (y * 255 / height) as u8;
                let b = if bar { 230 } else { 60 };
                out.extend_from_slice(&[r, g, b, 255]);
            }
        }
        out
    };
    [make(0), make(width / 3)]
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

/// The preset's settings on the deterministic encoders, or `None` when this build has
/// no ProRes encoder.
fn settings_for(preset: &Preset) -> Option<ExportSettings> {
    let mut settings = preset
        .resolve(&TIMELINE)
        .unwrap_or_else(|e| panic!("{} resolves: {e}", preset.id));
    let video = settings.video.as_mut().expect("video");
    let prores = matches!(video.codec, VideoCodec::ProRes(_));
    if prores && !cfg!(feature = "codec-prores-sw") {
        if !cfg!(target_os = "macos") {
            eprintln!("skipping {}: no ProRes encoder in this build", preset.id);
            return None;
        }
    } else {
        video.encoder_preference = EncoderPreference::Software;
    }
    Some(settings)
}

/// Encodes half a second; returns the frames written.
fn encode(path: &Path, settings: &ExportSettings) -> u64 {
    let v = settings.video.as_ref().expect("video");
    let a = settings.audio.as_ref().expect("audio");
    let frames = (v.frame_rate.to_f64().expect("rate") * 0.5).round() as u64;
    let mut session = ExportSession::create(path, settings).expect("export opens");
    let [first, second] = pictures(v.width, v.height);
    for n in 0..frames {
        let data = if n % 2 == 0 { &first } else { &second };
        let picture = Picture::rgba8(v.width, v.height, data, v.width as usize * 4);
        session.push_picture(&picture).expect("picture");
    }
    let samples = a.sample_rate as usize / 2;
    let mut at = 0;
    while at < samples {
        let len = 1000.min(samples - at);
        let block = tone(a.channels, a.sample_rate, at, len);
        let planes: Vec<&[f32]> = block.iter().map(Vec::as_slice).collect();
        session.push_audio(&planes).expect("audio");
        at += len;
    }
    let summary = session.finish().expect("export finishes");
    assert_eq!(summary.frames, frames);
    frames
}

/// Encodes the preset and checks the file against its settings.
fn check_preset(id: &str) {
    let preset = Preset::built_in(id).unwrap_or_else(|| panic!("{id} is built in"));
    let Some(settings) = settings_for(preset) else {
        return;
    };
    let v = settings.video.as_ref().expect("video");
    let a = settings.audio.as_ref().expect("audio");
    let dir = scratch(id);
    let path = dir.join(format!("out.{}", settings.container.extension()));
    let frames = encode(&path, &settings);

    let info = probe(&path).expect("probe");
    let (stream, video) = info.video().expect("video stream");
    let (codec, pixel_format) = match v.codec {
        VideoCodec::H264 => ("h264", "yuv420p"),
        VideoCodec::Hevc => ("hevc", "yuv420p10le"),
        // FFmpeg's ProRes decoder gives 4444 as 12-bit.
        VideoCodec::ProRes(p) if p.is_444() => ("prores", "yuv444p12le"),
        VideoCodec::ProRes(_) => ("prores", "yuv422p10le"),
        other => panic!("{id}: unexpected codec {other:?}"),
    };
    assert_eq!(stream.codec.name, codec, "{id}");
    assert_eq!((video.width, video.height), (v.width, v.height), "{id}");
    assert_eq!(
        video.pixel_format.map(|p| p.name()),
        Some(pixel_format),
        "{id}"
    );
    let rate = video.frame_rate.expect("frame rate");
    assert_eq!(
        i64::from(rate.num) * i64::from(v.frame_rate.den),
        i64::from(v.frame_rate.num) * i64::from(rate.den),
        "{id}: {rate:?} against {:?}",
        v.frame_rate
    );
    let expected = v.colour.info(false);
    assert_eq!(video.colour.primaries, expected.primaries, "{id} primaries");
    assert_eq!(video.colour.transfer, expected.transfer, "{id} transfer");
    assert_eq!(video.colour.matrix, expected.matrix, "{id} matrix");
    assert_eq!(video.colour.range, ColourRange::Limited, "{id} range");
    let hdr = v.hdr_metadata.is_some();
    assert_eq!(
        video.mastering_display_metadata, hdr,
        "{id} mastering display"
    );
    assert_eq!(video.content_light_level, hdr, "{id} content light level");

    let (stream, audio) = info.audio().expect("audio stream");
    let audio_codec = match a.codec {
        AudioCodec::Aac => "aac",
        AudioCodec::Flac { .. } => "flac",
        AudioCodec::Pcm(_) => "pcm_s24le",
        other => panic!("{id}: unexpected audio codec {other:?}"),
    };
    assert_eq!(stream.codec.name, audio_codec, "{id}");
    assert_eq!(audio.sample_rate, a.sample_rate, "{id}");
    assert_eq!(usize::from(audio.channels), a.channels, "{id}");

    let mut reader = FrameReader::open(&path, StreamSelect::BestVideo).expect("reader");
    let mut decoded = 0;
    while let Some(frame) = reader.next_frame().expect("decode") {
        decoded += u64::from(matches!(frame, Frame::Video(_)));
    }
    assert_eq!(decoded, frames, "{id}: decoded frames");
    let mut reader = FrameReader::open(&path, StreamSelect::BestAudio).expect("reader");
    let mut samples = 0;
    while let Some(frame) = reader.next_frame().expect("decode") {
        if let Frame::Audio(audio) = frame {
            samples += audio.samples();
        }
    }
    let half = a.sample_rate as usize / 2;
    // Encoder priming is trimmed where the container records it; allow one codec frame
    // of padding otherwise.
    assert!(
        (half..=half + 2048).contains(&samples),
        "{id}: {samples} decoded samples"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Runs every built-in preset of a category.
fn check_category(category: PresetCategory) {
    let ids: Vec<&str> = BUILT_IN
        .iter()
        .filter(|p| p.category == category)
        .map(|p| p.id)
        .collect();
    assert!(!ids.is_empty(), "{category:?}");
    for id in ids {
        check_preset(id);
    }
}

#[test]
fn youtube_presets_encode() {
    check_category(PresetCategory::YouTube);
}

#[test]
fn tiktok_and_instagram_presets_encode() {
    check_category(PresetCategory::TikTok);
    check_category(PresetCategory::Instagram);
}

#[test]
fn x_and_vimeo_presets_encode() {
    check_category(PresetCategory::X);
    check_category(PresetCategory::Vimeo);
}

#[test]
fn broadcast_presets_encode() {
    check_category(PresetCategory::Broadcast);
}

#[test]
fn archival_presets_encode() {
    check_category(PresetCategory::Archival);
}
