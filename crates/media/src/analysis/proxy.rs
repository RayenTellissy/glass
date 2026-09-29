//! Proxy media: a smaller, edit-friendly copy of a video file that playback and scrubbing
//! use in place of the source (Phase 3 switches to it; export always reads the source).
//!
//! **Formats** ([`ProxyFormat`]): ProRes 422 Proxy (default on macOS) or H.264 (default
//! elsewhere), both in a QuickTime `.mov` with the source's audio as PCM. ProRes uses
//! `prores_videotoolbox` (Apple's encoder, hardware on chips with a ProRes engine) when it
//! opens, else `prores_ks` profile 0 (`vendor apl0`), 10-bit 4:2:2. H.264 uses
//! `h264_videotoolbox` on macOS, else `libx264` `veryfast` CRF 23, 8-bit 4:2:0, a
//! keyframe every second and no B-frames, so every frame decodes after at most one
//! second of frames and never needs reordering (cheap seeks, and segments concatenate by
//! stream copy). [`ProxyEncoder`] forces software or hardware.
//!
//! **Size** ([`ProxySize::Auto`]): ProRes: sources up to 720 lines keep their size, larger
//! ones are halved and then fitted to 1280 on the long side (1080p → 960x540, UHD →
//! 1280x720); H.264: fitted into 1280x720 (1080p and UHD → 1280x720), smaller sources keep
//! their size. Both dimensions stay even and scale by the same factor, so the sample
//! aspect ratio carries over.
//!
//! **Kept**: every source frame with its presentation time (frame count, rate and VFR
//! timing), the display matrix (rotation, mirroring), the timecode (as a `tmcd` track),
//! the colour tags (primaries, transfer, matrix, range, chroma siting; pixels are scaled,
//! never colour converted) and the audio (best audio stream at its own rate, mono, stereo
//! or 5.1 as `AudioReader` maps it, 16-bit PCM, 24-bit for deeper sources).
//!
//! **Resumable**: the source is cut into segments of about [`ProxySettings::segment_ms`]
//! (10 s) that start on source keyframes (the first keyframe at least that far on; a
//! frame boundary when no keyframe comes within twice that). Each segment is encoded
//! into its own file under the proxy's cache entry and recorded in a manifest; a
//! cancelled or crashed job keeps finished segments and resumes at the next. When all
//! are done they are concatenated by stream copy into one `.mov` (no re-encode), the
//! manifest marks it complete and the segment files are deleted. One file, not a segment
//! playlist: the reader, the probe and any other tool open it like any movie, and the
//! copy costs one sequential read and write of the proxy (under 2% of the encode time).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::analysis::{AnalysisError, Control};
use crate::audio::{AudioOptions, AudioReader, ChannelLayout};
use crate::cache::{CacheDir, CacheEntry};
use crate::decode::{DecoderOptions, VideoFrame};
use crate::demux::Demuxer;
use crate::encode::{
    AudioEncoderSettings, Encoder, FrameScaler, PcmFormat, VideoEncoderSettings, encoder_available,
};
use crate::error::MediaError;
use crate::format::{MediaKind, PixelFormat};
use crate::gop::GopIndex;
use crate::hw::HwPreference;
use crate::mux::Muxer;
use crate::probe::{StreamDetail, probe};
use crate::reader::{FrameReader, StreamSelect};
use crate::time::{Rational, Timestamp};
use crate::timemap::SourceTimeline;

/// Bumped when the proxy format or how it is made changes.
pub const PROXY_VERSION: u32 = 1;

/// Cache kind of proxies (`<cache>/proxies/<hash>/proxy-<params>-v<version>.*`).
pub const PROXY_KIND: &str = "proxy";

/// Codec of a proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxyFormat {
    /// ProRes 422 Proxy, 10-bit 4:2:2.
    ProResProxy,
    /// H.264 High, 8-bit 4:2:0, 1 s GOP, no B-frames.
    H264,
}

impl ProxyFormat {
    /// ProRes Proxy on macOS, H.264 elsewhere.
    pub fn platform_default() -> Self {
        if cfg!(target_os = "macos") {
            Self::ProResProxy
        } else {
            Self::H264
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::ProResProxy => "prores_proxy",
            Self::H264 => "h264",
        }
    }
}

/// Which encoder makes the proxy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxyEncoder {
    /// The platform encoder (VideoToolbox) when it opens, else FFmpeg's software one.
    #[default]
    Auto,
    Hardware,
    Software,
}

/// Proxy frame size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxySize {
    /// By format, see the module documentation.
    #[default]
    Auto,
    /// Fitted into this box (never enlarged).
    Fit { width: u32, height: u32 },
}

/// How to make a proxy; the settings UI edits one of these (later phase).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProxySettings {
    pub format: ProxyFormat,
    pub encoder: ProxyEncoder,
    pub size: ProxySize,
    /// Target segment length in milliseconds (the unit of resumption).
    pub segment_ms: u32,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            format: ProxyFormat::platform_default(),
            encoder: ProxyEncoder::Auto,
            size: ProxySize::Auto,
            segment_ms: 10_000,
        }
    }
}

impl ProxySettings {
    /// Canonical parameter string (hashed into the cache name).
    pub fn params(&self) -> String {
        let size = match self.size {
            ProxySize::Auto => "auto".to_string(),
            ProxySize::Fit { width, height } => format!("{width}x{height}"),
        };
        let encoder = match self.encoder {
            ProxyEncoder::Auto => "auto",
            ProxyEncoder::Hardware => "hw",
            ProxyEncoder::Software => "sw",
        };
        format!(
            "format={};encoder={encoder};size={size};seg={}",
            self.format.tag(),
            self.segment_ms
        )
    }

    /// Proxy size for a stored (unrotated) source size.
    pub fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        let (long, short) = (width.max(height), width.min(height));
        let scale = match (self.size, self.format) {
            (
                ProxySize::Fit {
                    width: bw,
                    height: bh,
                },
                _,
            ) => (f64::from(bw) / f64::from(width))
                .min(f64::from(bh) / f64::from(height))
                .min(1.0),
            (ProxySize::Auto, ProxyFormat::ProResProxy) => {
                if short <= 720 {
                    1.0
                } else {
                    (0.5f64).min(1280.0 / f64::from(long))
                }
            }
            (ProxySize::Auto, ProxyFormat::H264) => (1280.0 / f64::from(long))
                .min(720.0 / f64::from(short))
                .min(1.0),
        };
        let even = |v: u32| {
            let scaled = (f64::from(v) * scale / 2.0).round() as u32 * 2;
            scaled.max(2)
        };
        if scale >= 1.0 {
            (width + width % 2, height + height % 2)
        } else {
            (even(width), even(height))
        }
    }
}

/// One segment of the plan: source frames `[first_frame, first_frame + frames)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentPlan {
    pub first_frame: u64,
    pub frames: u64,
    /// Presentation time of the first frame (source video time base).
    pub start_pts: i64,
    /// Presentation time of the next segment's first frame; `None` for the last.
    pub end_pts: Option<i64>,
    /// Audio samples `[audio_start, audio_end)` at the audio rate; to the end for the last.
    pub audio_start: u64,
    pub audio_end: Option<u64>,
}

/// What the proxy will be; a resumed job continues only when it matches exactly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyPlan {
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub time_base: Rational,
    pub audio: Option<AudioPlan>,
    pub segments: Vec<SegmentPlan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioPlan {
    pub sample_rate: u32,
    pub channels: usize,
    pub bits: u8,
}

/// The resume state, `<stem>.manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyManifest {
    pub version: u32,
    pub plan: ProxyPlan,
    /// Per segment: file size once finished.
    pub done: Vec<Option<u64>>,
    /// The concatenated file is in place.
    pub complete: bool,
}

/// A finished proxy: the job's result (`<stem>.json`), valid while the file exists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Proxy {
    pub version: u32,
    /// File name of the proxy in the cache entry's folder (`<stem>.mov`).
    pub file: String,
    pub format: ProxyFormat,
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    /// Source duration in microseconds.
    pub duration_us: i64,
    pub bytes: u64,
    pub segments: usize,
    /// Segments encoded by this run; the rest were finished by an earlier one.
    pub segments_encoded: usize,
    /// Wall time of this run (encoding and concatenation), seconds.
    pub seconds: f64,
}

impl Proxy {
    pub fn path(&self, entry: &CacheEntry) -> PathBuf {
        entry.dir().join(&self.file)
    }
}

/// Segment boundaries: start at frame 0, then at the first keyframe at least `segment`
/// after the current start, or at the frame `segment` after it when no keyframe comes
/// within twice that.
fn plan_segments(
    timeline: &SourceTimeline,
    gop: &GopIndex,
    segment_ms: u32,
    audio_rate: Option<u32>,
) -> Vec<SegmentPlan> {
    let tb = timeline.time_base;
    let count = timeline.frame_count();
    let pts = |k: u64| timeline.frame_pts(k).map_or(0, |t| t.value);
    let seg = Timestamp::new(i64::from(segment_ms.max(1)), Rational::new(1, 1000))
        .rescale(tb)
        .value
        .max(1);
    let mut starts = vec![0u64];
    let mut k = 0u64;
    while k < count {
        let start = pts(k);
        let want = start + seg;
        let next_key = gop.keyframes.iter().map(|kf| kf.pts).find(|&p| p >= want);
        let next = match next_key {
            Some(p) if p < start + 2 * seg => frame_at_or_after(timeline, p),
            _ => frame_at_or_after(timeline, want),
        };
        if next <= k || next >= count {
            break;
        }
        starts.push(next);
        k = next;
    }
    let audio_at = |p: i64| -> u64 {
        audio_rate.map_or(0, |rate| {
            Timestamp::new(p, tb)
                .rescale(Rational::new(1, rate as i32))
                .value
                .max(0) as u64
        })
    };
    starts
        .iter()
        .enumerate()
        .map(|(i, &first)| {
            let end = starts.get(i + 1).copied();
            let start_pts = pts(first);
            let end_pts = end.map(pts);
            SegmentPlan {
                first_frame: first,
                frames: end.unwrap_or(count) - first,
                start_pts,
                end_pts,
                audio_start: if i == 0 { 0 } else { audio_at(start_pts) },
                audio_end: end_pts.map(audio_at),
            }
        })
        .collect()
}

fn frame_at_or_after(timeline: &SourceTimeline, pts: i64) -> u64 {
    let count = timeline.frame_count();
    let (mut lo, mut hi) = (0u64, count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if timeline.frame_pts(mid).map_or(i64::MAX, |t| t.value) < pts {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The encoder, pixel format and settings for a format and preference, or why none
/// opens.
fn video_settings(
    settings: &ProxySettings,
    hardware: bool,
    size: (u32, u32),
    source_bits: u8,
    base: &VideoEncoderSettings,
    fps: Option<f64>,
) -> Option<VideoEncoderSettings> {
    let (name, format) = match (settings.format, hardware) {
        (ProxyFormat::ProResProxy, true) => ("prores_videotoolbox", "p210le"),
        (ProxyFormat::ProResProxy, false) => ("prores_ks", "yuv422p10le"),
        (ProxyFormat::H264, true) => ("h264_videotoolbox", "nv12"),
        (ProxyFormat::H264, false) => ("libx264", "yuv420p"),
    };
    if !encoder_available(name) {
        return None;
    }
    let _ = source_bits;
    let mut s = base.clone();
    s.encoder = name.to_string();
    s.width = size.0;
    s.height = size.1;
    s.pixel_format = PixelFormat::from_name(format)?;
    let gop = fps.map_or(30, |f| f.round().max(1.0) as u32);
    match (settings.format, hardware) {
        (ProxyFormat::ProResProxy, true) => {
            s.options.push(("profile".into(), "proxy".into()));
        }
        (ProxyFormat::ProResProxy, false) => {
            s.options.push(("profile".into(), "0".into()));
            s.options.push(("vendor".into(), "apl0".into()));
        }
        (ProxyFormat::H264, true) => {
            s.gop = Some(gop);
            s.max_b_frames = Some(0);
            // About 0.12 bits per pixel: 3.3 Mbit/s at 1280x720 30 fps.
            let rate = fps.unwrap_or(30.0);
            s.bit_rate = Some((f64::from(size.0 * size.1) * rate * 0.12) as u64);
            s.options.push(("profile".into(), "high".into()));
        }
        (ProxyFormat::H264, false) => {
            s.gop = Some(gop);
            s.max_b_frames = Some(0);
            s.options.push(("preset".into(), "veryfast".into()));
            s.options.push(("crf".into(), "23".into()));
            s.options
                .push(("x264-params".into(), "scenecut=0:open-gop=0".into()));
        }
    }
    Some(s)
}

struct Source {
    path: PathBuf,
    timeline: Arc<SourceTimeline>,
    gop: Arc<GopIndex>,
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    frame_rate: Option<Rational>,
    timecode: Option<String>,
    display_matrix: Option<[i32; 9]>,
    audio: Option<AudioPlan>,
    audio_index: Option<usize>,
    audio_layout: ChannelLayout,
    base: VideoEncoderSettings,
}

fn open_source(path: &Path, cache: Option<&CacheDir>) -> Result<Source, AnalysisError> {
    let info = probe(path)?;
    let (index, video) = info
        .streams
        .iter()
        .find_map(|s| match &s.detail {
            StreamDetail::Video(v) if s.kind == MediaKind::Video => Some((s.index, v.clone())),
            _ => None,
        })
        .ok_or_else(|| MediaError::NoStream {
            path: path.to_path_buf(),
            kind: MediaKind::Video,
        })?;
    let select = StreamSelect::Index(index);
    let timeline = match cache {
        Some(cache) => {
            SourceTimeline::load_or_scan(cache, &crate::SourceFile::open(path)?, select)?.0
        }
        None => SourceTimeline::scan(path, select)?.0,
    };
    let gop = match cache {
        Some(cache) => GopIndex::load_or_build(cache, path, select)?.0,
        None => GopIndex::build(path, select)?.0,
    };
    let audio_info = info.streams.iter().find_map(|s| match &s.detail {
        StreamDetail::Audio(a) if s.kind == MediaKind::Audio => Some((s.index, a.clone())),
        _ => None,
    });
    let (audio, audio_index, audio_layout) = match audio_info {
        Some((index, a)) if a.sample_rate > 0 => {
            let layout = match a.channels {
                1 => ChannelLayout::Mono,
                2 => ChannelLayout::Stereo,
                _ => ChannelLayout::Surround51,
            };
            let bits = if a.bit_depth.unwrap_or(16) > 16 {
                24
            } else {
                16
            };
            (
                Some(AudioPlan {
                    sample_rate: a.sample_rate,
                    channels: layout.channels(),
                    bits,
                }),
                Some(index),
                layout,
            )
        }
        _ => (None, None, ChannelLayout::Stereo),
    };
    let demuxer = Demuxer::open(path)?;
    let display_matrix = demuxer.display_matrix(index);
    let pixel_format = video
        .pixel_format
        .or_else(|| PixelFormat::from_name("yuv420p"))
        .ok_or_else(|| AnalysisError::Encode("no pixel format".to_string()))?;
    let mut base = VideoEncoderSettings::new(
        "",
        video.width,
        video.height,
        pixel_format,
        timeline.time_base,
    );
    let frame_rate = video.frame_rate.filter(|r| r.is_valid() && r.num > 0);
    base.frame_rate = frame_rate.filter(|_| !video.vfr.is_vfr);
    base.sample_aspect_ratio = video.sample_aspect_ratio;
    // Untagged fields are written out as resolved: a smaller proxy would otherwise be
    // read with other defaults (SD lines select BT.601).
    base.colour = video.colour.or(video.colour_space.tags());
    base.global_header = true;
    Ok(Source {
        path: path.to_path_buf(),
        timeline: Arc::new(timeline),
        gop: Arc::new(gop),
        width: video.width,
        height: video.height,
        pixel_format,
        frame_rate,
        timecode: info.timecode.clone(),
        display_matrix,
        audio,
        audio_index,
        audio_layout,
        base,
    })
}

/// Makes (or finishes) the proxy of `path` with `settings` in `entry` (a
/// [`CacheArea::Proxies`](crate::cache::CacheArea::Proxies) entry). Finished segments of
/// an earlier, interrupted run with the same plan are kept. Checkpoints between frames.
pub fn generate(
    path: &Path,
    settings: &ProxySettings,
    entry: &CacheEntry,
    cache: Option<&CacheDir>,
    control: &dyn Control,
) -> Result<Proxy, AnalysisError> {
    let started = Instant::now();
    let source = open_source(path, cache)?;
    let size = settings.output_size(source.width, source.height);
    let fps = source.frame_rate.and_then(|r| r.to_f64());
    let bits = source.pixel_format.bit_depth();
    let candidates: &[bool] = match settings.encoder {
        ProxyEncoder::Auto if cfg!(target_os = "macos") => &[true, false],
        ProxyEncoder::Auto | ProxyEncoder::Software => &[false],
        ProxyEncoder::Hardware => &[true],
    };
    let manifest_name = "manifest.json";
    let previous: Option<ProxyManifest> = entry
        .read_json(manifest_name)
        .filter(|m: &ProxyManifest| m.version == PROXY_VERSION);

    // Choose the encoder: the one an earlier run used when it still opens (segments
    // from different encoders must not be mixed), else the first that opens.
    let mut chosen: Option<VideoEncoderSettings> = None;
    let mut preferred: Vec<bool> = candidates.to_vec();
    if let Some(m) = &previous {
        let hw = m.plan.encoder.contains("videotoolbox");
        if preferred.contains(&hw) {
            preferred.retain(|&h| h != hw);
            preferred.insert(0, hw);
        }
    }
    let mut last_error = None;
    for hardware in preferred {
        let Some(s) = video_settings(settings, hardware, size, bits, &source.base, fps) else {
            continue;
        };
        match Encoder::video(&s) {
            Ok(_) => {
                chosen = Some(s);
                break;
            }
            Err(error) => {
                tracing::debug!(encoder = %s.encoder, %error, "proxy encoder does not open");
                last_error = Some(error);
            }
        }
    }
    let Some(video) = chosen else {
        return Err(match last_error {
            Some(e) => e.into(),
            None => AnalysisError::Encode("no proxy encoder in this build".to_string()),
        });
    };

    let plan = ProxyPlan {
        encoder: video.encoder.clone(),
        width: size.0,
        height: size.1,
        pixel_format: video.pixel_format.name().to_string(),
        time_base: source.timeline.time_base,
        audio: source.audio,
        segments: plan_segments(
            &source.timeline,
            &source.gop,
            settings.segment_ms,
            source.audio.map(|a| a.sample_rate),
        ),
    };
    let output_name = entry.file_name("mov");
    let output = entry.dir().join(&output_name);
    let segment_path = |i: usize| entry.path(&format!("s{i:05}.mov"));
    let mut manifest = match previous {
        Some(m) if m.plan == plan => m,
        _ => {
            remove_segments(entry)?;
            ProxyManifest {
                version: PROXY_VERSION,
                plan: plan.clone(),
                done: vec![None; plan.segments.len()],
                complete: false,
            }
        }
    };
    // A segment counts as done only while its file is there with the recorded size.
    for (i, done) in manifest.done.iter_mut().enumerate() {
        if let Some(bytes) = *done
            && std::fs::metadata(segment_path(i)).ok().map(|m| m.len()) != Some(bytes)
        {
            *done = None;
        }
    }
    let total_frames = source.timeline.frame_count().max(1);
    let mut encoded = 0usize;

    if !(manifest.complete && output.is_file()) {
        manifest.complete = false;
        entry.write_json(manifest_name, &manifest)?;
        let mut reader = FrameReader::open_with(
            &source.path,
            StreamSelect::Index(source.timeline.stream_index),
            DecoderOptions::with_hw(HwPreference::Off),
        )?
        .with_gop_index(source.gop.clone());
        let mut audio = match (plan.audio, source.audio_index) {
            (Some(a), Some(index)) => Some(AudioReader::open(
                &source.path,
                AudioOptions {
                    sample_rate: a.sample_rate,
                    layout: source.audio_layout,
                    stream: StreamSelect::Index(index),
                    chunk_frames: 4096,
                },
            )?),
            _ => None,
        };
        let mut scaler: Option<FrameScaler> = None;
        let mut frames_done: u64 = manifest
            .done
            .iter()
            .zip(&plan.segments)
            .filter(|(d, _)| d.is_some())
            .map(|(_, s)| s.frames)
            .sum();
        for (i, segment) in plan.segments.iter().enumerate() {
            if manifest.done[i].is_some() {
                continue;
            }
            control.checkpoint()?;
            let file = segment_path(i);
            let bytes = encode_segment(
                &source,
                &plan,
                &video,
                segment,
                &file,
                &mut reader,
                audio.as_mut(),
                &mut scaler,
                &mut |n| {
                    control.progress(((frames_done + n) as f32 / total_frames as f32) * 0.98);
                    control.checkpoint()
                },
            )?;
            frames_done += segment.frames;
            encoded += 1;
            manifest.done[i] = Some(bytes);
            entry.write_json(manifest_name, &manifest)?;
        }
        control.checkpoint()?;
        concatenate(&source, &plan, &segment_path, entry, &output_name)?;
        manifest.complete = true;
        entry.write_json(manifest_name, &manifest)?;
        remove_segments(entry)?;
    }
    control.progress(1.0);
    let bytes = std::fs::metadata(&output)?.len();
    Ok(Proxy {
        version: PROXY_VERSION,
        file: output_name,
        format: settings.format,
        encoder: plan.encoder.clone(),
        width: plan.width,
        height: plan.height,
        frames: source.timeline.frame_count(),
        duration_us: source.timeline.duration().rescale(Rational::MICROS).value,
        bytes,
        segments: plan.segments.len(),
        segments_encoded: encoded,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// Deletes the segment files of an entry (finished or partial).
fn remove_segments(entry: &CacheEntry) -> std::io::Result<()> {
    let prefix = format!("{}.s", entry.stem());
    let items = match std::fs::read_dir(entry.dir()) {
        Ok(items) => items,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for item in items {
        let item = item?;
        let name = item.file_name();
        let Some(name) = name.to_str() else { continue };
        let segment = name
            .strip_prefix(&prefix)
            .is_some_and(|rest| rest.len() > 5 && rest[..5].bytes().all(|b| b.is_ascii_digit()));
        if segment {
            std::fs::remove_file(item.path())?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_segment(
    source: &Source,
    plan: &ProxyPlan,
    video: &VideoEncoderSettings,
    segment: &SegmentPlan,
    file: &Path,
    reader: &mut FrameReader,
    mut audio: Option<&mut AudioReader>,
    scaler: &mut Option<FrameScaler>,
    step: &mut dyn FnMut(u64) -> Result<(), crate::analysis::Stopped>,
) -> Result<u64, AnalysisError> {
    let partial = file.with_extension("mov.partial");
    let mut muxer = Muxer::create(&partial, Some("mov"))?;
    let mut settings = video.clone();
    settings.global_header = muxer.wants_global_header();
    let mut encoder = Encoder::video(&settings)?;
    let video_stream = muxer.add_stream(&encoder, source.frame_rate)?;
    let mut audio_out = match (plan.audio, audio.as_deref_mut()) {
        (Some(a), Some(_)) => {
            let format = if a.bits > 16 {
                PcmFormat::S24
            } else {
                PcmFormat::S16
            };
            let settings = AudioEncoderSettings::pcm(format, a.sample_rate, a.channels);
            let encoder = Encoder::audio(&settings)?;
            let stream = muxer.add_stream(&encoder, None)?;
            Some((encoder, stream, a))
        }
        _ => None,
    };
    muxer.write_header()?;
    if let (Some(reader), Some(_)) = (audio.as_deref_mut(), &audio_out)
        && reader.position() != segment.audio_start
    {
        reader.seek_to_sample(segment.audio_start)?;
    }

    let tb = plan.time_base;
    let start = Timestamp::new(segment.start_pts, tb);
    let mut next = reader.seek_exact(start)?.and_then(|f| f.into_video());
    let mut frames = 0u64;
    let mut buffers: Vec<Vec<f32>> = Vec::new();
    while let Some(frame) = next.take() {
        let Some(pts) = frame.pts() else {
            next = read_video(reader)?;
            continue;
        };
        if segment.end_pts.is_some_and(|end| pts.value >= end) {
            break;
        }
        let scaler = match scaler {
            Some(s) => s,
            None => scaler.insert(FrameScaler::new(
                (frame.format(), frame.width(), frame.height()),
                (settings.pixel_format, settings.width, settings.height),
            )?),
        };
        let mut scaled = scaler.scale(&frame)?;
        let duration = frame.duration().map_or(0, |d| d.rescale(tb).value);
        scaled.set_timing(Some(pts.rescale(tb).value - segment.start_pts), duration);
        encoder.send_video(&scaled)?;
        drain(&mut encoder, &mut muxer, video_stream)?;
        frames += 1;

        // Audio up to the end of this frame.
        if let (Some((aenc, astream, a)), Some(reader)) = (&mut audio_out, audio.as_deref_mut()) {
            let frame_end = Timestamp::new(pts.rescale(tb).value + duration.max(1), tb)
                .rescale(Rational::new(1, a.sample_rate as i32))
                .value
                .max(0) as u64;
            let limit = segment.audio_end.map_or(frame_end, |e| e.min(frame_end));
            pump_audio(reader, aenc, &mut muxer, *astream, limit, &mut buffers)?;
        }
        step(frames)?;
        next = read_video(reader)?;
    }
    if frames != segment.frames {
        tracing::warn!(
            path = %source.path.display(),
            expected = segment.frames,
            got = frames,
            "proxy segment frame count differs from the timeline"
        );
    }
    encoder.send_eof()?;
    drain(&mut encoder, &mut muxer, video_stream)?;
    if let (Some((aenc, astream, _)), Some(reader)) = (&mut audio_out, audio) {
        let limit = segment.audio_end.unwrap_or(u64::MAX);
        pump_audio(reader, aenc, &mut muxer, *astream, limit, &mut buffers)?;
        aenc.send_eof()?;
        drain(aenc, &mut muxer, *astream)?;
    }
    muxer.finish()?;
    std::fs::rename(&partial, file)?;
    Ok(std::fs::metadata(file)?.len())
}

fn read_video(reader: &mut FrameReader) -> Result<Option<VideoFrame>, AnalysisError> {
    loop {
        match reader.next_frame()? {
            Some(frame) => {
                if let Some(video) = frame.into_video() {
                    return Ok(Some(video));
                }
            }
            None => return Ok(None),
        }
    }
}

fn drain(encoder: &mut Encoder, muxer: &mut Muxer, stream: usize) -> Result<(), AnalysisError> {
    while let Some(packet) = encoder.receive_packet()? {
        muxer.write_packet(stream, packet)?;
    }
    Ok(())
}

/// Reads and encodes audio until the reader's position reaches `until` (or its end).
fn pump_audio(
    reader: &mut AudioReader,
    encoder: &mut Encoder,
    muxer: &mut Muxer,
    stream: usize,
    until: u64,
    buffers: &mut Vec<Vec<f32>>,
) -> Result<(), AnalysisError> {
    const CHUNK: usize = 4096;
    let channels = reader.channels();
    if buffers.len() != channels {
        *buffers = vec![vec![0.0; CHUNK]; channels];
    }
    while reader.position() < until {
        let want = (until - reader.position()).min(CHUNK as u64) as usize;
        let mut outs: Vec<&mut [f32]> = buffers.iter_mut().map(|b| &mut b[..want]).collect();
        let got = reader.read(&mut outs)?;
        if got == 0 {
            break;
        }
        let planes: Vec<&[f32]> = buffers.iter().map(|b| &b[..got]).collect();
        encoder.send_audio(&planes)?;
        drain(encoder, muxer, stream)?;
    }
    Ok(())
}

/// Stream-copies the segments, in order, into `<stem>.mov`, moving each segment's packets
/// back to its place on the source timeline; adds timecode and display matrix.
fn concatenate(
    source: &Source,
    plan: &ProxyPlan,
    segment_path: &dyn Fn(usize) -> PathBuf,
    entry: &CacheEntry,
    output_name: &str,
) -> Result<(), AnalysisError> {
    let output = entry.dir().join(output_name);
    let partial = entry.dir().join(format!("{output_name}.partial"));
    let mut muxer = Muxer::create(&partial, Some("mov"))?;
    let mut streams: Vec<(usize, Rational)> = Vec::new();
    for (i, segment) in plan.segments.iter().enumerate() {
        let mut demuxer = Demuxer::open(segment_path(i))?;
        if i == 0 {
            for s in demuxer.streams().to_vec() {
                let out = muxer.add_copy_stream(&demuxer, s.index)?;
                streams.push((out, s.time_base));
                if s.kind == MediaKind::Video {
                    if let Some(tc) = &source.timecode {
                        muxer.set_stream_metadata(out, "timecode", tc)?;
                    }
                    if let Some(m) = &source.display_matrix {
                        muxer.set_display_matrix(out, m)?;
                    }
                }
            }
            muxer.write_header()?;
        }
        let summaries = demuxer.streams().to_vec();
        while let Some(packet) = demuxer.read_packet()? {
            let index = packet.stream_index();
            let Some(&(out, _)) = streams.get(index) else {
                continue;
            };
            let kind = summaries.get(index).map(|s| s.kind);
            let offset = match kind {
                Some(MediaKind::Video) => {
                    Timestamp::new(segment.start_pts, plan.time_base)
                        .rescale(packet.time_base())
                        .value
                }
                Some(MediaKind::Audio) => plan.audio.map_or(0, |a| {
                    Timestamp::new(
                        segment.audio_start as i64,
                        Rational::new(1, a.sample_rate as i32),
                    )
                    .rescale(packet.time_base())
                    .value
                }),
                _ => 0,
            };
            muxer.write_packet(out, packet.retimed(out, offset))?;
        }
    }
    muxer.finish()?;
    std::fs::rename(&partial, &output)?;
    Ok(())
}

/// The cache entry of a source's proxy with these settings.
pub fn cache_entry(
    cache: &CacheDir,
    source: &crate::SourceFile,
    settings: &ProxySettings,
) -> CacheEntry {
    cache.entry_hashed_in(
        crate::cache::CacheArea::Proxies,
        &source.hash,
        PROXY_KIND,
        &crate::cache::params_hash(&settings.params()),
        PROXY_VERSION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(format: ProxyFormat) -> ProxySettings {
        ProxySettings {
            format,
            ..ProxySettings::default()
        }
    }

    #[test]
    fn proxy_sizes() {
        let prores = settings(ProxyFormat::ProResProxy);
        assert_eq!(prores.output_size(3840, 2160), (1280, 720));
        assert_eq!(prores.output_size(4096, 2160), (1280, 676));
        assert_eq!(prores.output_size(1920, 1080), (960, 540));
        assert_eq!(prores.output_size(1280, 720), (1280, 720));
        assert_eq!(prores.output_size(640, 360), (640, 360));
        assert_eq!(prores.output_size(1080, 1920), (540, 960));
        let h264 = settings(ProxyFormat::H264);
        assert_eq!(h264.output_size(3840, 2160), (1280, 720));
        assert_eq!(h264.output_size(1920, 1080), (1280, 720));
        assert_eq!(h264.output_size(1080, 1920), (720, 1280));
        assert_eq!(h264.output_size(720, 480), (720, 480));
        let fit = ProxySettings {
            size: ProxySize::Fit {
                width: 640,
                height: 640,
            },
            ..prores
        };
        assert_eq!(fit.output_size(1920, 1080), (640, 360));
    }

    #[test]
    fn params_name_every_setting() {
        let a = ProxySettings::default();
        let b = ProxySettings {
            segment_ms: 5000,
            ..a
        };
        assert_ne!(a.params(), b.params());
        assert_ne!(
            settings(ProxyFormat::H264).params(),
            settings(ProxyFormat::ProResProxy).params()
        );
    }

    fn timeline(frames: u64) -> SourceTimeline {
        SourceTimeline {
            stream_index: 0,
            time_base: Rational::new(1, 30),
            frames: crate::timemap::FrameTimes::Constant {
                start: 0,
                rate: Rational::new(30, 1),
                count: frames,
            },
        }
    }

    fn gop(keys: &[i64]) -> GopIndex {
        GopIndex {
            version: 1,
            stream_index: 0,
            time_base: Rational::new(1, 30),
            keyframes: keys
                .iter()
                .map(|&pts| crate::gop::Keyframe {
                    pts,
                    dts: Some(pts),
                    pos: None,
                })
                .collect(),
            frames: 0,
        }
    }

    #[test]
    fn segments_start_on_keyframes() {
        // 40 s at 30 fps, keyframe every 4 s: 10 s segments cut at 12, 24, 36 s.
        let keys: Vec<i64> = (0..10).map(|k| k * 120).collect();
        let plan = plan_segments(&timeline(1200), &gop(&keys), 10_000, Some(48_000));
        let starts: Vec<u64> = plan.iter().map(|s| s.first_frame).collect();
        assert_eq!(starts, [0, 360, 720, 1080]);
        assert_eq!(plan.iter().map(|s| s.frames).sum::<u64>(), 1200);
        assert_eq!(plan[1].audio_start, 12 * 48_000);
        assert_eq!(plan[0].audio_end, Some(12 * 48_000));
        assert_eq!(plan[3].end_pts, None);
        assert_eq!(plan[3].audio_end, None);
    }

    #[test]
    fn long_gops_cut_between_keyframes() {
        // One keyframe only: cut every 10 s on frame boundaries.
        let plan = plan_segments(&timeline(900), &gop(&[0]), 10_000, None);
        let starts: Vec<u64> = plan.iter().map(|s| s.first_frame).collect();
        assert_eq!(starts, [0, 300, 600]);
    }
}
