//! Export encoding: one second of generated pictures and a tone through every codec this
//! FFmpeg build has, read back with `probe` and the decoders (codec, size, frame count,
//! duration, colour tags, audio rate and channels), plus H.264 quality against the
//! source and the settings and input checks.

use std::path::{Path, PathBuf};

use media::export::{
    AudioCodec, AudioSettings, ColourTags, Container, ExportError, ExportSession, ExportSettings,
    ExportSummary, Picture, PicturePlane, ProResProfile, RateControl, VideoCodec, VideoSettings,
};
use media::{
    ColourRange, Frame, FrameReader, ImageSequence, MediaInfo, PcmFormat, PixelFormat, Rational,
    SampleType, SourceOptions, StreamSelect, VideoSource, encoder_available, probe,
};

const W: u32 = 640;
const H: u32 = 360;
const FPS: Rational = Rational::new(30, 1);
const FRAMES: u64 = 30;
const RATE: u32 = 48_000;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-export-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch folder");
    dir
}

/// Frame `n` of a moving test picture: smooth gradients, a soft disc that travels and a
/// few colour bars, as packed 8-bit RGBA with straight alpha 255.
fn picture_rgba8(n: u64, width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    let t = n as f32 / 30.0;
    let (cx, cy) = (
        width as f32 * (0.25 + 0.5 * t.fract()),
        height as f32 * (0.5 + 0.2 * (t * 6.0).sin()),
    );
    let radius = height as f32 * 0.18;
    for y in 0..height {
        for x in 0..width {
            let u = x as f32 / width as f32;
            let v = y as f32 / height as f32;
            let mut r = 0.15 + 0.7 * u;
            let mut g = 0.2 + 0.6 * v;
            let mut b = 0.5 + 0.4 * (u * 3.0 + t).sin() * 0.5;
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let disc = ((radius - d) / 8.0).clamp(0.0, 1.0);
            r = r * (1.0 - disc) + 0.95 * disc;
            g = g * (1.0 - disc) + 0.85 * disc;
            b = b * (1.0 - disc) + 0.2 * disc;
            if v > 0.85 {
                let bar = (u * 6.0) as u32;
                r = if bar & 1 == 0 { 0.75 } else { 0.1 };
                g = if bar & 2 == 0 { 0.75 } else { 0.1 };
                b = if bar & 4 == 0 { 0.75 } else { 0.1 };
            }
            for c in [r, g, b] {
                out.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            out.push(255);
        }
    }
    out
}

/// The same picture as 16-bit little-endian RGBA.
fn picture_rgba16(n: u64, width: u32, height: u32) -> Vec<u8> {
    picture_rgba8(n, width, height)
        .iter()
        .flat_map(|&v| (u16::from(v) * 257).to_le_bytes())
        .collect()
}

/// A 440 Hz sine at -12 dBFS (660 Hz on the second channel), samples `from..from + len`.
fn tone(channels: usize, from: u64, len: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|c| {
            let f = 440.0 * (1.0 + c as f64 * 0.5);
            (0..len)
                .map(|i| {
                    let t = (from + i as u64) as f64 / f64::from(RATE);
                    (0.25 * (std::f64::consts::TAU * f * t).sin()) as f32
                })
                .collect()
        })
        .collect()
}

/// Writes `frames` pictures and one second of tone (in 1000-sample blocks, so encoders
/// with fixed frame sizes have to regroup them).
fn encode(path: &Path, settings: &ExportSettings, frames: u64, sixteen_bit: bool) -> ExportSummary {
    let mut session = ExportSession::create(path, settings).expect("export opens");
    if let Some(v) = &settings.video {
        for n in 0..frames {
            if sixteen_bit {
                let data = picture_rgba16(n, v.width, v.height);
                let picture = Picture::rgba16(v.width, v.height, &data, v.width as usize * 8);
                session.push_picture(&picture).expect("picture");
            } else {
                let data = picture_rgba8(n, v.width, v.height);
                let picture = Picture::rgba8(v.width, v.height, &data, v.width as usize * 4);
                session.push_picture(&picture).expect("picture");
            }
        }
    }
    if let Some(a) = &settings.audio {
        let mut at = 0u64;
        while at < u64::from(RATE) {
            let len = 1000.min(RATE as usize - at as usize);
            let block = tone(a.channels, at, len);
            let planes: Vec<&[f32]> = block.iter().map(Vec::as_slice).collect();
            session.push_audio(&planes).expect("audio");
            at += len as u64;
        }
    }
    let summary = session.finish().expect("export finishes");
    assert_eq!(
        summary.frames,
        if settings.video.is_some() { frames } else { 0 }
    );
    assert!(summary.bytes > 0, "{summary:?}");
    summary
}

fn video_settings(codec: VideoCodec) -> VideoSettings {
    VideoSettings::new(codec, W, H, FPS)
}

/// Decodes every video frame: count, size and the frames' resolved colour.
fn decode_video(path: &Path) -> Vec<media::VideoFrame> {
    let mut reader = FrameReader::open(path, StreamSelect::BestVideo).expect("reader");
    let mut frames = Vec::new();
    while let Some(frame) = reader.next_frame().expect("decode") {
        if let Frame::Video(v) = frame {
            frames.push(v);
        }
    }
    frames
}

/// Decodes the audio: samples per channel and channel 0 as f32.
fn decode_audio(path: &Path) -> (usize, Vec<f32>) {
    let mut reader = FrameReader::open(path, StreamSelect::BestAudio).expect("reader");
    let mut samples = 0;
    let mut first = Vec::new();
    while let Some(frame) = reader.next_frame().expect("decode") {
        let Frame::Audio(a) = frame else { continue };
        samples += a.samples();
        let format = a.format().expect("sample format");
        let channels = usize::from(a.channels());
        let plane = a.plane(0).expect("plane 0");
        let step = if format.planar { 1 } else { channels };
        let size = format.sample.bytes();
        for i in 0..a.samples() {
            let at = i * step * size;
            let b = &plane[at..at + size];
            first.push(match format.sample {
                SampleType::I16 => f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0,
                SampleType::I32 => {
                    i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0
                }
                SampleType::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                other => panic!("unexpected sample type {other:?}"),
            });
        }
    }
    (samples, first)
}

fn duration_seconds(info: &MediaInfo) -> f64 {
    info.duration.expect("duration").seconds()
}

/// Checks the video stream of `path` against the settings.
fn check_video(path: &Path, codec: &str, settings: &VideoSettings, frames: u64) -> MediaInfo {
    let info = probe(path).expect("probe");
    let (stream, video) = info.video().expect("video stream");
    assert_eq!(stream.codec.name, codec, "{}", path.display());
    assert_eq!(
        (video.width, video.height),
        (settings.width, settings.height)
    );
    let rate = video.frame_rate.expect("frame rate");
    assert_eq!(
        rate.num * settings.frame_rate.den,
        settings.frame_rate.num * rate.den
    );
    let decoded = decode_video(path);
    assert_eq!(
        decoded.len() as u64,
        frames,
        "decoded frames of {}",
        path.display()
    );
    let expected = settings.colour.info(false);
    if !settings.codec.is_rgb() {
        assert_eq!(video.colour.primaries, expected.primaries, "primaries");
        assert_eq!(video.colour.transfer, expected.transfer, "transfer");
        assert_eq!(video.colour.matrix, expected.matrix, "matrix");
        assert_eq!(video.colour.range, ColourRange::Limited, "range");
        let first = decoded.first().expect("a frame").colour_space();
        assert!(
            !first.defaulted.primaries,
            "decoded primaries come from the tags"
        );
    }
    info
}

fn check_audio(path: &Path, codec: &str, channels: usize) -> Vec<f32> {
    let info = probe(path).expect("probe");
    let (stream, audio) = info.audio().expect("audio stream");
    assert_eq!(stream.codec.name, codec);
    assert_eq!(audio.sample_rate, RATE);
    assert_eq!(usize::from(audio.channels), channels);
    let (samples, first) = decode_audio(path);
    // Encoder priming is trimmed by the decoder where the container records it; allow one
    // codec frame of padding at the end otherwise.
    assert!(
        (RATE as usize..=RATE as usize + 2048).contains(&samples),
        "{} decoded samples in {}",
        samples,
        path.display()
    );
    first
}

#[test]
fn h264_and_aac_in_mp4() {
    let dir = scratch("h264");
    let path = dir.join("out.mp4");
    let video = video_settings(VideoCodec::H264);
    let settings = ExportSettings::new(Container::Mp4)
        .with_video(video.clone())
        .with_audio(AudioSettings::new(AudioCodec::Aac, RATE, 2));
    let summary = encode(&path, &settings, FRAMES, false);
    assert_eq!(summary.video_encoder.as_deref(), Some("libx264"));
    assert_eq!(summary.audio_encoder.as_deref(), Some("aac"));
    assert_eq!(summary.samples, u64::from(RATE));
    let info = check_video(&path, "h264", &video, FRAMES);
    assert!(
        (duration_seconds(&info) - 1.0).abs() < 0.05,
        "{:?}",
        info.duration
    );
    let (_, v) = info.video().expect("video");
    assert_eq!(v.pixel_format.map(|p| p.name()), Some("yuv420p"));
    let first = check_audio(&path, "aac", 2);
    let rms = (first.iter().map(|s| s * s).sum::<f32>() / first.len() as f32).sqrt();
    assert!(
        (rms - 0.25 / 2f32.sqrt()).abs() < 0.02,
        "AAC tone RMS {rms}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// PSNR in dB of `a` against `b` (packed RGBA, alpha ignored).
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for c in 0..3 {
            let d = f64::from(pa[c]) - f64::from(pb[c]);
            sum += d * d;
            n += 1;
        }
    }
    let mse = sum / n as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

#[test]
fn h264_at_the_default_crf_is_close_to_the_source() {
    let dir = scratch("psnr");
    let path = dir.join("out.mp4");
    let video = video_settings(VideoCodec::H264);
    let settings = ExportSettings::new(Container::Mp4).with_video(video);
    encode(&path, &settings, FRAMES, false);
    let decoded = decode_video(&path);
    assert_eq!(decoded.len() as u64, FRAMES);
    let mut worst = f64::INFINITY;
    let mut total = 0.0;
    for (n, frame) in decoded.iter().enumerate() {
        let rgba = frame.to_rgba8().expect("rgba");
        let p = psnr(&rgba.data, &picture_rgba8(n as u64, W, H));
        worst = worst.min(p);
        total += p;
    }
    let mean = total / decoded.len() as f64;
    eprintln!("H.264 CRF 23 RGB PSNR: mean {mean:.2} dB, worst frame {worst:.2} dB");
    assert!(worst > 35.0, "worst frame {worst:.2} dB");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hevc_in_mp4_is_tagged_hvc1() {
    let dir = scratch("hevc");
    let path = dir.join("out.mp4");
    let video = video_settings(VideoCodec::Hevc);
    let settings = ExportSettings::new(Container::Mp4).with_video(video.clone());
    let summary = encode(&path, &settings, FRAMES, false);
    assert_eq!(summary.video_encoder.as_deref(), Some("libx265"));
    let info = check_video(&path, "hevc", &video, FRAMES);
    let (stream, _) = info.video().expect("video");
    assert_eq!(stream.codec.fourcc.as_deref(), Some("hvc1"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hevc_hdr10_pq_from_16_bit_pictures() {
    let dir = scratch("hevc-pq");
    let path = dir.join("out.mov");
    let mut video = video_settings(VideoCodec::Hevc);
    video.colour = ColourTags::Bt2020Pq;
    video.rate = RateControl::Crf(24.0);
    let settings = ExportSettings::new(Container::Mov)
        .with_video(video.clone())
        .with_audio(AudioSettings::new(AudioCodec::Pcm(PcmFormat::S24), RATE, 2));
    encode(&path, &settings, FRAMES, true);
    let info = check_video(&path, "hevc", &video, FRAMES);
    let (_, v) = info.video().expect("video");
    assert_eq!(v.pixel_format.map(|p| p.name()), Some("yuv420p10le"));
    assert_eq!(v.bit_depth, 10);
    assert_eq!(v.hdr, media::Hdr::Pq);
    check_audio(&path, "pcm_s24le", 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn h264_hlg_with_flac_in_matroska() {
    let dir = scratch("hlg");
    let path = dir.join("out.mkv");
    let mut video = video_settings(VideoCodec::H264);
    video.colour = ColourTags::Bt2020Hlg;
    video.rate = RateControl::Bitrate {
        average: 4_000_000,
        max: Some(6_000_000),
    };
    let settings = ExportSettings::new(Container::Matroska)
        .with_video(video.clone())
        .with_audio(AudioSettings::new(AudioCodec::Flac { bits: 24 }, RATE, 2));
    encode(&path, &settings, FRAMES, true);
    let info = check_video(&path, "h264", &video, FRAMES);
    let (_, v) = info.video().expect("video");
    assert_eq!(v.pixel_format.map(|p| p.name()), Some("yuv420p10le"));
    assert_eq!(v.hdr, media::Hdr::Hlg);
    let first = check_audio(&path, "flac", 2);
    let expected = &tone(1, 0, RATE as usize)[0];
    let max_error = first
        .iter()
        .zip(expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_error < 1.0 / 8_000_000.0,
        "24-bit FLAC differs by {max_error}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(target_os = "macos")]
#[test]
fn prores_through_videotoolbox() {
    for (profile, format) in [
        (ProResProfile::Hq, "yuv422p10le"),
        // FFmpeg's ProRes decoder gives 4444 as 12-bit.
        (ProResProfile::P4444, "yuv444p12le"),
    ] {
        let dir = scratch("prores-vt");
        let path = dir.join("out.mov");
        let video = video_settings(VideoCodec::ProRes(profile));
        let settings = ExportSettings::new(Container::Mov)
            .with_video(video.clone())
            .with_audio(AudioSettings::new(AudioCodec::Pcm(PcmFormat::S16), RATE, 2));
        let summary = encode(&path, &settings, FRAMES, true);
        assert_eq!(
            summary.video_encoder.as_deref(),
            Some("prores_videotoolbox")
        );
        let info = check_video(&path, "prores", &video, FRAMES);
        let (_, v) = info.video().expect("video");
        assert_eq!(
            v.pixel_format.map(|p| p.name()),
            Some(format),
            "{profile:?}"
        );
        check_audio(&path, "pcm_s16le", 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(feature = "codec-prores-sw")]
#[test]
fn prores_in_software_behind_the_flag() {
    let dir = scratch("prores-ks");
    let path = dir.join("out.mov");
    let mut video = video_settings(VideoCodec::ProRes(ProResProfile::Hq));
    video.encoder = Some("prores_ks".to_string());
    let settings = ExportSettings::new(Container::Mov).with_video(video.clone());
    let summary = encode(&path, &settings, FRAMES, true);
    assert_eq!(summary.video_encoder.as_deref(), Some("prores_ks"));
    let info = check_video(&path, "prores", &video, FRAMES);
    let (stream, _) = info.video().expect("video");
    assert_eq!(stream.codec.profile, Some("HQ"));
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(not(feature = "codec-prores-sw"))]
#[test]
fn software_prores_needs_the_flag() {
    let mut video = video_settings(VideoCodec::ProRes(ProResProfile::Hq));
    video.encoder = Some("prores_ks".to_string());
    let settings = ExportSettings::new(Container::Mov).with_video(video);
    let dir = scratch("prores-ks-off");
    let error = ExportSession::create(dir.join("out.mov"), &settings).expect_err("refused");
    assert!(matches!(error, ExportError::Settings(_)), "{error}");
    assert!(!dir.join("out.mov").exists(), "no file is created");
    let _ = std::fs::remove_dir_all(dir);
}

fn check_sequence(pattern: &Path, summary: &ExportSummary, format: &str, width: u32, height: u32) {
    assert_eq!(summary.files.len() as u64, FRAMES);
    let sequence = ImageSequence::from_pattern(pattern)
        .expect("scan")
        .expect("sequence");
    assert_eq!(sequence.len() as u64, FRAMES);
    assert_eq!(sequence.first(), 0);
    let first = probe(&summary.files[0]).expect("probe");
    let (_, v) = first.video().expect("picture");
    assert_eq!((v.width, v.height), (width, height));
    assert_eq!(v.pixel_format.map(|p| p.name()), Some(format));
    let mut source =
        VideoSource::open_sequence(sequence, SourceOptions::new(FPS)).expect("open sequence");
    let mut frames = 0;
    while source.next_frame().expect("decode").is_some() {
        frames += 1;
    }
    assert_eq!(frames, FRAMES);
}

#[test]
fn png_sequence_keeps_alpha_and_16_bits() {
    let dir = scratch("png");
    let pattern = dir.join("shot_%04d.png");
    let mut video = video_settings(VideoCodec::Png);
    video.pixel_format = PixelFormat::from_name("rgba64be");
    let settings = ExportSettings::new(Container::ImageSequence).with_video(video);
    let summary = encode(&pattern, &settings, FRAMES, true);
    assert_eq!(summary.files[0], dir.join("shot_0000.png"));
    check_sequence(&pattern, &summary, "rgba64be", W, H);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn tiff_sequence_from_a_plain_name() {
    let dir = scratch("tiff");
    let video = video_settings(VideoCodec::Tiff);
    let settings = ExportSettings::new(Container::ImageSequence).with_video(video);
    let summary = encode(&dir.join("frame.tif"), &settings, FRAMES, false);
    assert_eq!(summary.files[0], dir.join("frame_00000.tif"));
    check_sequence(&dir.join("frame_%05d.tif"), &summary, "rgb24", W, H);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn gif_loops_at_the_frame_rate() {
    let dir = scratch("gif");
    let path = dir.join("out.gif");
    let video = VideoSettings::new(VideoCodec::Gif, 320, 180, FPS);
    let settings = ExportSettings::new(Container::Gif).with_video(video);
    let summary = encode(&path, &settings, FRAMES, false);
    assert_eq!(summary.video_encoder.as_deref(), Some("gif"));
    let info = probe(&path).expect("probe");
    let (stream, v) = info.video().expect("video");
    assert_eq!(stream.codec.name, "gif");
    assert_eq!((v.width, v.height), (320, 180));
    assert!(
        (duration_seconds(&info) - 1.0).abs() < 0.02,
        "{:?}",
        info.duration
    );
    assert_eq!(decode_video(&path).len() as u64, FRAMES);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn audio_only_files() {
    let dir = scratch("audio");
    for (container, codec, name, channels, exact) in [
        (
            Container::Wav,
            AudioCodec::Pcm(PcmFormat::S16),
            "pcm_s16le",
            2,
            Some(1.0 / 32768.0),
        ),
        (
            Container::Wav,
            AudioCodec::Pcm(PcmFormat::S24),
            "pcm_s24le",
            1,
            Some(1.0 / 8_000_000.0),
        ),
        (
            Container::Flac,
            AudioCodec::Flac { bits: 16 },
            "flac",
            2,
            Some(1.0 / 32768.0),
        ),
        (Container::M4a, AudioCodec::Aac, "aac", 2, None),
    ] {
        let path = dir.join(format!("out-{name}.{}", container.extension()));
        let settings =
            ExportSettings::new(container).with_audio(AudioSettings::new(codec, RATE, channels));
        let summary = encode(&path, &settings, 0, false);
        assert_eq!(summary.samples, u64::from(RATE));
        let first = check_audio(&path, name, channels);
        if let Some(step) = exact {
            let expected = &tone(1, 0, RATE as usize)[0];
            let max_error = first
                .iter()
                .zip(expected)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(max_error <= step, "{name}: differs by {max_error}");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn codecs_missing_from_the_build_are_reported() {
    let dir = scratch("missing");
    let cases = [
        (
            ExportSettings::new(Container::Mp4).with_video(video_settings(VideoCodec::Av1)),
            ["libsvtav1", "libaom-av1", "librav1e"].as_slice(),
        ),
        (
            ExportSettings::new(Container::WebM).with_video(video_settings(VideoCodec::Vp9)),
            ["libvpx-vp9"].as_slice(),
        ),
        (
            ExportSettings::new(Container::Mp3).with_audio(AudioSettings::new(
                AudioCodec::Mp3,
                RATE,
                2,
            )),
            ["libmp3lame"].as_slice(),
        ),
    ];
    for (i, (settings, encoders)) in cases.into_iter().enumerate() {
        let path = dir.join(format!("out-{i}.{}", settings.container.extension()));
        if encoders.iter().any(|e| encoder_available(e)) {
            encode(&path, &settings, FRAMES, false);
            continue;
        }
        match ExportSession::create(&path, &settings) {
            Err(ExportError::Unavailable { tried, .. }) => assert_eq!(tried, encoders),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert!(!path.exists());
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn settings_are_checked_before_any_file_is_made() {
    let dir = scratch("settings");
    let path = dir.join("out.mp4");
    let refuse = |settings: ExportSettings| {
        let error = ExportSession::create(&path, &settings).expect_err("refused");
        assert!(!path.exists());
        error
    };
    let prores = video_settings(VideoCodec::ProRes(ProResProfile::Hq));
    assert!(matches!(
        refuse(ExportSettings::new(Container::Mp4).with_video(prores)),
        ExportError::Incompatible { .. }
    ));
    let mut hdr8 = video_settings(VideoCodec::H264);
    hdr8.colour = ColourTags::Bt2020Pq;
    hdr8.pixel_format = PixelFormat::from_name("yuv420p");
    assert!(matches!(
        refuse(ExportSettings::new(Container::Mp4).with_video(hdr8)),
        ExportError::Settings(_)
    ));
    let odd = VideoSettings::new(VideoCodec::H264, 641, 360, FPS);
    assert!(matches!(
        refuse(ExportSettings::new(Container::Mp4).with_video(odd)),
        ExportError::Settings(_)
    ));
    let mut wrong_format = video_settings(VideoCodec::H264);
    wrong_format.pixel_format = PixelFormat::from_name("rgb24");
    assert!(matches!(
        refuse(ExportSettings::new(Container::Mp4).with_video(wrong_format)),
        ExportError::Settings(_)
    ));
    let fast_gif = VideoSettings::new(VideoCodec::Gif, 320, 180, Rational::new(60, 1));
    assert!(matches!(
        refuse(ExportSettings::new(Container::Gif).with_video(fast_gif)),
        ExportError::Settings(_)
    ));
    assert!(matches!(
        refuse(ExportSettings::new(Container::Mp4)),
        ExportError::Settings(_)
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn input_must_match_the_settings() {
    let dir = scratch("input");
    let settings = ExportSettings::new(Container::Mov)
        .with_video(video_settings(VideoCodec::H264))
        .with_audio(AudioSettings::new(AudioCodec::Pcm(PcmFormat::S16), RATE, 2));
    let mut session = ExportSession::create(dir.join("out.mov"), &settings).expect("open");
    let small = picture_rgba8(0, 320, 180);
    let error = session
        .push_picture(&Picture::rgba8(320, 180, &small, 320 * 4))
        .expect_err("wrong size");
    assert!(matches!(error, ExportError::Input(_)), "{error}");
    // A buffer a row short is refused before swscale reads it.
    let full = picture_rgba8(0, W, H);
    let short = &full[..full.len() - W as usize * 4];
    let error = session
        .push_picture(&Picture::rgba8(W, H, short, W as usize * 4))
        .expect_err("short buffer");
    assert!(matches!(error, ExportError::Input(_)), "{error}");
    let mono = tone(1, 0, 100);
    let error = session.push_audio(&[&mono[0]]).expect_err("one channel");
    assert!(matches!(error, ExportError::Input(_)), "{error}");
    // Rows padded to 256 bytes, as the readback ring maps them, and BGRA order.
    let stride = (W as usize * 4).next_multiple_of(256) + 256;
    let mut padded = vec![0u8; stride * H as usize];
    for (i, px) in full.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (i % W as usize, i / W as usize);
        let at = y * stride + x * 4;
        padded[at..at + 4].copy_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    session
        .push_picture(&Picture::bgra8(W, H, &padded, stride))
        .expect("padded BGRA rows");
    let yuv = PixelFormat::from_name("yuv420p").expect("yuv420p");
    let (y, u, v) = (
        vec![128u8; (W * H) as usize],
        vec![128u8; (W * H / 4) as usize],
        vec![128u8; (W * H / 4) as usize],
    );
    let planes = [
        PicturePlane {
            data: &y,
            stride: W as usize,
        },
        PicturePlane {
            data: &u,
            stride: W as usize / 2,
        },
        PicturePlane {
            data: &v,
            stride: W as usize / 2,
        },
    ];
    session
        .push_picture(&Picture::planar(yuv, W, H, &planes))
        .expect("YUV planes");
    let summary = session.finish().expect("finish");
    assert_eq!(summary.frames, 2);
    let _ = std::fs::remove_dir_all(dir);
}
