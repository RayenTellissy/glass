//! Proxy generation: size, codec, frame count, duration, timecode, rotation, colour tags
//! and audio of proxies made from the fixtures; frame-accurate match to the source at
//! sampled frames; cancel then resume keeps finished segments and gives the same file
//! length; the H.264 option in software and hardware. Skips without fixtures.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use media::analysis::proxy::{
    self, Proxy, ProxyEncoder, ProxyFormat, ProxyManifest, ProxySettings, cache_entry,
};
use media::analysis::{AnalysisError, Control, Stopped, Unattended};
use media::{CacheDir, FrameReader, MediaKind, SourceFile, StreamDetail, StreamSelect, VideoFrame};

fn temp_cache(name: &str) -> CacheDir {
    let dir = std::env::temp_dir().join(format!("glass-proxy-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    CacheDir::new(dir)
}

fn make(
    file: &str,
    settings: &ProxySettings,
    name: &str,
) -> Option<(Proxy, std::path::PathBuf, CacheDir)> {
    let path = common::fixture(file)?;
    let cache = temp_cache(name);
    let source = SourceFile::open(&path).expect("hash");
    let entry = cache_entry(&cache, &source, settings);
    let proxy = proxy::generate(&path, settings, &entry, Some(&cache), &Unattended).expect("proxy");
    let out = proxy.path(&entry);
    assert!(out.is_file(), "{file}: proxy written");
    Some((proxy, out, cache))
}

fn frames(path: &std::path::Path) -> Vec<VideoFrame> {
    FrameReader::open(path, StreamSelect::BestVideo)
        .expect("open")
        .filter_map(|f| f.expect("decode").into_video())
        .collect()
}

/// Mean absolute difference of two frames scaled to 64x36 RGBA.
fn difference(a: &VideoFrame, b: &VideoFrame) -> f64 {
    let a = a.to_rgba8_scaled(64, 36).expect("rgba");
    let b = b.to_rgba8_scaled(64, 36).expect("rgba");
    let sum: u64 = a
        .data
        .iter()
        .zip(&b.data)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    sum as f64 / a.data.len() as f64
}

fn video_info(path: &std::path::Path) -> (media::StreamInfo, media::VideoInfo) {
    let info = media::probe(path).expect("probe");
    info.streams
        .iter()
        .find_map(|s| match &s.detail {
            StreamDetail::Video(v) if s.kind == MediaKind::Video => Some((s.clone(), v.clone())),
            _ => None,
        })
        .expect("video stream")
}

/// Same frames at the same times, each closest to its own source frame.
fn check_frames(file: &str, source: &std::path::Path, proxy: &std::path::Path) {
    let src = frames(source);
    let out = frames(proxy);
    assert_eq!(out.len(), src.len(), "{file}: frame count");
    for (a, b) in src.iter().zip(&out) {
        assert_eq!(
            a.pts().map(|t| t.seconds()),
            b.pts().map(|t| t.seconds()),
            "{file}: pts"
        );
    }
    let n = src.len();
    for k in [0, 1, n / 3, n / 2, n - 2, n - 1] {
        let own = difference(&src[k], &out[k]);
        assert!(own < 4.0, "{file} frame {k}: mean difference {own:.2}");
        // Where neighbouring source frames differ, the proxy frame is closest to its own.
        for j in [k.wrapping_sub(1), k + 1] {
            if let Some(other) = src.get(j)
                && difference(other, &src[k]) > 2.0 * own + 0.5
            {
                let d = difference(other, &out[k]);
                assert!(own < d, "{file} frame {k}: {own:.2} vs frame {j} {d:.2}");
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn prores_proxies_of_1080p_and_4k() {
    for (file, size) in [
        ("h264_1080p30.mp4", (960, 540)),
        ("h264_4k30.mp4", (1280, 720)),
    ] {
        let settings = ProxySettings::default();
        assert_eq!(settings.format, ProxyFormat::ProResProxy);
        let Some((proxy, out, cache)) = make(file, &settings, file) else {
            continue;
        };
        let source = common::fixture(file).expect("fixture");
        assert_eq!((proxy.width, proxy.height), size, "{file}");
        let (stream, video) = video_info(&out);
        let (_, source_video) = video_info(&source);
        assert_eq!(stream.codec.name, "prores", "{file}");
        assert_eq!(stream.codec.profile, Some("Proxy"), "{file}");
        assert_eq!((video.width, video.height), size, "{file}");
        assert_eq!(video.frame_rate, source_video.frame_rate, "{file}");
        let info = media::probe(&out).expect("probe");
        let source_info = media::probe(&source).expect("probe");
        let d = (info.duration.expect("duration").seconds()
            - source_info.duration.expect("duration").seconds())
        .abs();
        assert!(d < 0.034, "{file}: duration differs by {d}");
        check_frames(file, &source, &out);
        eprintln!(
            "{file}: {} {}x{}, {} frames, {} bytes, {:.2} s",
            proxy.encoder, proxy.width, proxy.height, proxy.frames, proxy.bytes, proxy.seconds
        );
        let _ = std::fs::remove_dir_all(cache.root());
    }
}

#[test]
fn software_prores_proxy_keeps_audio_timecode_rotation_and_colour() {
    let settings = ProxySettings {
        format: ProxyFormat::ProResProxy,
        encoder: ProxyEncoder::Software,
        ..ProxySettings::default()
    };
    // PCM audio at 48 kHz, 16-bit: kept sample for sample.
    if let Some((proxy, out, cache)) = make("sync_h264_pcm.mov", &settings, "pcm") {
        assert_eq!(proxy.encoder, "prores_ks");
        let info = media::probe(&out).expect("probe");
        let audio = info
            .streams
            .iter()
            .find(|s| s.kind == MediaKind::Audio)
            .expect("audio");
        assert_eq!(audio.codec.name, "pcm_s16le");
        assert_eq!(audio.frames, Some(144_000));
        check_frames(
            "sync_h264_pcm.mov",
            &common::fixture("sync_h264_pcm.mov").expect("fixture"),
            &out,
        );
        let _ = std::fs::remove_dir_all(cache.root());
    }
    // 24-bit PCM stays 24-bit.
    if let Some((_, out, cache)) = make("prores422_1080p23976.mov", &settings, "s24") {
        let info = media::probe(&out).expect("probe");
        let audio = info
            .streams
            .iter()
            .find(|s| s.kind == MediaKind::Audio)
            .expect("audio");
        assert_eq!(audio.codec.name, "pcm_s24le");
        let (_, video) = video_info(&out);
        assert_eq!((video.width, video.height), (960, 540));
        let _ = std::fs::remove_dir_all(cache.root());
    }
    // Timecode and a 90 degree display matrix.
    if let Some((_, out, cache)) = make("rotated_timecode_h264_640x360_25.mov", &settings, "tc") {
        let info = media::probe(&out).expect("probe");
        assert_eq!(info.timecode.as_deref(), Some("01:02:03:04"));
        let (_, video) = video_info(&out);
        let source = common::fixture("rotated_timecode_h264_640x360_25.mov").expect("fixture");
        let (_, source_video) = video_info(&source);
        assert_eq!(video.rotation, source_video.rotation);
        assert_eq!(video.rotation, 90);
        assert_eq!((video.width, video.height), (640, 360));
        let _ = std::fs::remove_dir_all(cache.root());
    }
    // PQ 10-bit HEVC: colour tags carried.
    if let Some((_, out, cache)) = make("hevc_pq_10bit_640x360.mp4", &settings, "pq") {
        let (_, video) = video_info(&out);
        let source = common::fixture("hevc_pq_10bit_640x360.mp4").expect("fixture");
        let (_, source_video) = video_info(&source);
        assert_eq!(video.colour.transfer, Some("smpte2084"));
        assert_eq!(video.colour.primaries, source_video.colour.primaries);
        assert_eq!(video.colour.matrix, source_video.colour.matrix);
        assert_eq!(video.colour.range, source_video.colour.range);
        let _ = std::fs::remove_dir_all(cache.root());
    }
}

#[test]
fn h264_proxies_in_software_and_hardware() {
    let encoders: &[ProxyEncoder] = if cfg!(target_os = "macos") {
        &[ProxyEncoder::Software, ProxyEncoder::Hardware]
    } else {
        &[ProxyEncoder::Software]
    };
    for &encoder in encoders {
        let settings = ProxySettings {
            format: ProxyFormat::H264,
            encoder,
            ..ProxySettings::default()
        };
        let Some((proxy, out, cache)) = make("h264_1080p30.mp4", &settings, "h264") else {
            return;
        };
        let expected = if encoder == ProxyEncoder::Hardware {
            "h264_videotoolbox"
        } else {
            "libx264"
        };
        assert_eq!(proxy.encoder, expected);
        let (stream, video) = video_info(&out);
        assert_eq!(stream.codec.name, "h264");
        assert_eq!((video.width, video.height), (1280, 720));
        let source = common::fixture("h264_1080p30.mp4").expect("fixture");
        check_frames(expected, &source, &out);
        // No reordering and a keyframe every second.
        let mut demuxer = media::Demuxer::open(&out).expect("open");
        let mut since_key = 0;
        while let Some(p) = demuxer.read_packet().expect("read") {
            if demuxer.streams()[p.stream_index()].kind != MediaKind::Video {
                continue;
            }
            assert_eq!(p.pts(), p.dts(), "{expected}: no B-frames");
            since_key = if p.is_key() { 0 } else { since_key + 1 };
            assert!(since_key < 30, "{expected}: GOP longer than a second");
        }
        let _ = std::fs::remove_dir_all(cache.root());
    }
}

/// Stops the job once `after` segments are finished (the manifest is written after each
/// segment, so a checkpoint sees it on disk).
struct StopAfter {
    manifest: std::path::PathBuf,
    after: usize,
    stopped: AtomicBool,
    checks: AtomicU32,
}

impl Control for StopAfter {
    fn checkpoint(&self) -> Result<(), Stopped> {
        self.checks.fetch_add(1, Ordering::Relaxed);
        let done = std::fs::read(&self.manifest)
            .ok()
            .and_then(|b| serde_json::from_slice::<ProxyManifest>(&b).ok())
            .map_or(0, |m| m.done.iter().flatten().count());
        if done >= self.after {
            self.stopped.store(true, Ordering::Relaxed);
            return Err(Stopped);
        }
        Ok(())
    }

    fn progress(&self, _: f32) {}

    fn partial(&self, _: u32) {}
}

#[test]
fn cancelled_proxy_resumes_without_redoing_segments() {
    let file = "h264_1080p30.mp4";
    let Some(path) = common::fixture(file) else {
        return;
    };
    let settings = ProxySettings {
        format: ProxyFormat::ProResProxy,
        encoder: ProxyEncoder::Software,
        segment_ms: 500,
        ..ProxySettings::default()
    };
    // Uninterrupted reference.
    let Some((reference, reference_out, reference_cache)) = make(file, &settings, "ref") else {
        return;
    };
    assert!(reference.segments >= 4, "{} segments", reference.segments);
    assert_eq!(reference.segments_encoded, reference.segments);

    let cache = temp_cache("resume");
    let source = SourceFile::open(&path).expect("hash");
    let entry = cache_entry(&cache, &source, &settings);
    let stop = StopAfter {
        manifest: entry.path("manifest.json"),
        after: 2,
        stopped: AtomicBool::new(false),
        checks: AtomicU32::new(0),
    };
    let first = proxy::generate(&path, &settings, &entry, Some(&cache), &stop);
    assert!(matches!(first, Err(AnalysisError::Stopped)), "cancelled");
    assert!(stop.stopped.load(Ordering::Relaxed));
    let manifest: ProxyManifest =
        serde_json::from_slice(&std::fs::read(entry.path("manifest.json")).expect("manifest"))
            .expect("parse");
    let kept = manifest.done.iter().flatten().count();
    assert_eq!(kept, 2, "two finished segments on disk");
    assert!(!manifest.complete);

    let resumed =
        proxy::generate(&path, &settings, &entry, Some(&cache), &Unattended).expect("resume");
    assert_eq!(
        resumed.segments_encoded,
        resumed.segments - kept,
        "finished segments kept"
    );
    let out = resumed.path(&entry);
    let a = frames(&reference_out);
    let b = frames(&out);
    assert_eq!(a.len(), b.len());
    let d = (media::probe(&out)
        .expect("probe")
        .duration
        .expect("d")
        .seconds()
        - media::probe(&reference_out)
            .expect("probe")
            .duration
            .expect("d")
            .seconds())
    .abs();
    assert!(d < 1e-6, "same duration");
    check_frames(file, &path, &out);
    // Segment files are gone once the proxy is complete.
    let leftovers = std::fs::read_dir(entry.dir())
        .expect("dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".s0"))
        .count();
    assert_eq!(leftovers, 0);
    // A third run finds the finished proxy and encodes nothing.
    let again =
        proxy::generate(&path, &settings, &entry, Some(&cache), &Unattended).expect("again");
    assert_eq!(again.segments_encoded, 0);
    let _ = std::fs::remove_dir_all(cache.root());
    let _ = std::fs::remove_dir_all(reference_cache.root());
}
