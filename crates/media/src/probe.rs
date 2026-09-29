//! Probe: what a file contains, read from the container and codec headers.
//!
//! `probe` opens the file with FFmpeg's `avformat_find_stream_info`, which decodes the
//! first few frames only for codecs whose container leaves parameters out (H.264 in MP4
//! for its reorder delay and pixel format, PNG and JPEG for the pixel format); that pass
//! is most of the cost for those files (see the `probe` benchmark). Everything else comes
//! from headers and side data. The one extra read is the variable-frame-rate check, which
//! looks at frame timestamps only and never decodes (see [`VfrScan`]).

use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;

use crate::colour::{ColourSpace, LayoutHint};
use crate::demux::{Demuxer, StreamSummary};
use crate::error::Result;
use crate::ffi;
use crate::format::{ColourInfo, Hdr, MediaKind, PixelFormat, SampleFormat};
use crate::time::{Rational, Timestamp};

/// Frame timestamps looked at by the VFR check when the container index lists them.
const VFR_INDEX_FRAMES: usize = 2000;
/// Video packets read by the VFR check when the index does not list every frame.
const VFR_SCAN_PACKETS: usize = 300;
/// Packets of any stream read by the VFR check before it gives up on finding video.
const VFR_SCAN_MAX_PACKETS: usize = 1500;
/// A stream is VFR when its longest frame interval exceeds the shortest by this factor.
const VFR_RATIO: f64 = 1.5;
/// FFmpeg's "unknown" for profile and level.
const UNKNOWN: i32 = -99;

/// Everything `probe` found in a file.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub size_bytes: Option<u64>,
    /// FFmpeg's container name (`mov,mp4,m4a,3gp,3g2,mj2`, `matroska,webm`, `wav`).
    pub container: String,
    pub container_long_name: String,
    /// Container duration (microsecond base).
    pub duration: Option<Timestamp>,
    pub start_time: Option<Timestamp>,
    pub bit_rate: Option<u64>,
    /// `creation_time` from the container, or from the first stream that has one.
    pub creation_time: Option<String>,
    /// SMPTE timecode of the first frame (`01:02:03:04`, `;` before frames when drop-frame):
    /// the video stream's tag, else a `tmcd` track's, else the container's.
    pub timecode: Option<String>,
    /// Container tags as stored (`encoder`, `com.apple.quicktime.make`, `location`, ...).
    pub tags: Vec<(String, String)>,
    pub streams: Vec<StreamInfo>,
}

impl MediaInfo {
    /// The main video stream: the first one that is not a cover image.
    pub fn video(&self) -> Option<(&StreamInfo, &VideoInfo)> {
        self.streams.iter().find_map(|s| match &s.detail {
            StreamDetail::Video(v) if !s.attached_picture => Some((s, v)),
            _ => None,
        })
    }

    /// The first audio stream.
    pub fn audio(&self) -> Option<(&StreamInfo, &AudioInfo)> {
        self.streams.iter().find_map(|s| match &s.detail {
            StreamDetail::Audio(a) => Some((s, a)),
            _ => None,
        })
    }

    /// A container tag by name, ignoring ASCII case.
    pub fn tag(&self, key: &str) -> Option<&str> {
        find_tag(&self.tags, key)
    }
}

fn find_tag<'a>(tags: &'a [(String, String)], key: &str) -> Option<&'a str> {
    tags.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
}

#[derive(Clone, Debug, PartialEq)]
pub struct StreamInfo {
    pub index: usize,
    pub kind: MediaKind,
    pub codec: CodecInfo,
    pub time_base: Rational,
    pub start: Option<Timestamp>,
    pub duration: Option<Timestamp>,
    /// Frame count from the container header.
    pub frames: Option<u64>,
    pub bit_rate: Option<u64>,
    pub language: Option<String>,
    pub attached_picture: bool,
    pub tags: Vec<(String, String)>,
    pub detail: StreamDetail,
}

impl StreamInfo {
    pub fn tag(&self, key: &str) -> Option<&str> {
        find_tag(&self.tags, key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecInfo {
    /// FFmpeg's short name (`h264`, `hevc`, `prores`, `aac`, `pcm_s24le`).
    pub name: &'static str,
    /// FFmpeg's decoder description, when a decoder is available.
    pub long_name: Option<String>,
    /// Profile name (`High`, `Main 10`, `422`).
    pub profile: Option<&'static str>,
    /// FFmpeg's level number as stored by the codec: 40 is H.264 level 4.0, 120 is HEVC
    /// level 4 (30 x level).
    pub level: Option<i32>,
    /// The container's four-character code (`avc1`, `hvc1`, `apcn`, `tmcd`).
    pub fourcc: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StreamDetail {
    Video(VideoInfo),
    Audio(AudioInfo),
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoInfo {
    /// Coded picture size, before rotation.
    pub width: u32,
    pub height: u32,
    pub pixel_format: Option<PixelFormat>,
    /// Bits per component: the codec's raw sample depth when it states one, else the
    /// pixel format's.
    pub bit_depth: u8,
    /// Sample (pixel) aspect ratio; 1:1 when the file does not say.
    pub sample_aspect_ratio: Rational,
    /// Display aspect ratio of the coded picture, before rotation.
    pub display_aspect_ratio: Rational,
    /// Average frame rate over the stream (`avg_frame_rate`).
    pub avg_frame_rate: Option<Rational>,
    /// FFmpeg's guess at the base rate every timestamp fits (`r_frame_rate`).
    pub real_frame_rate: Option<Rational>,
    /// The rate to treat the stream as: `r_frame_rate` for constant-rate streams,
    /// `avg_frame_rate` for VFR ones, whichever exists otherwise.
    pub frame_rate: Option<Rational>,
    pub vfr: VfrScan,
    /// The stream's colour tags as the file states them.
    pub colour: ColourInfo,
    /// The tags with untagged fields filled in by the rule in [`crate::colour`], as the
    /// decoded frames will carry them.
    pub colour_space: ColourSpace,
    pub hdr: Hdr,
    pub mastering_display_metadata: bool,
    pub content_light_level: bool,
    pub dolby_vision: bool,
    /// Counter-clockwise rotation to apply for display, in degrees: 0, 90, 180 or 270
    /// (other angles are rounded to the nearest degree).
    pub rotation: u16,
    /// The display matrix also mirrors the picture.
    pub mirrored: bool,
    pub interlaced: bool,
}

impl VideoInfo {
    /// Size as displayed: width and height swapped for a quarter-turn rotation.
    pub fn display_size(&self) -> (u32, u32) {
        if self.rotation % 180 == 90 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

/// How the variable-frame-rate check reached its answer.
///
/// The check collects frame timestamps (from the container's index when it lists every
/// frame, as MOV and MP4 do; otherwise by reading up to 300 video packets, no decoding),
/// sorts them and compares consecutive intervals. The stream is VFR when the longest
/// interval is more than 1.5x the shortest: rounding jitter in millisecond time bases
/// (33/34 ms at 30 fps) stays well under that, while dropped or held frames exceed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VfrScan {
    pub is_vfr: bool,
    pub source: VfrSource,
    /// Timestamps compared.
    pub frames: usize,
    pub min_interval: Option<Timestamp>,
    pub max_interval: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VfrSource {
    ContainerIndex,
    PacketScan,
    /// Too few frames to judge (a still image, or a single-frame stream).
    NotChecked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// FFmpeg's layout description (`stereo`, `5.1(side)`).
    pub channel_layout: Option<String>,
    pub sample_format: Option<SampleFormat>,
    /// Significant bits per sample, when the codec states it (24 for `pcm_s24le`).
    pub bit_depth: Option<u8>,
    /// Samples per coded frame, when fixed (1024 for AAC).
    pub frame_size: Option<u32>,
}

/// Reads a file's container and stream metadata.
pub fn probe(path: impl AsRef<Path>) -> Result<MediaInfo> {
    let path = path.as_ref();
    let mut demuxer = Demuxer::open(path)?;
    let mut streams: Vec<StreamInfo> = demuxer
        .input()
        .streams()
        .zip(demuxer.streams())
        .map(|(stream, summary)| stream_info(&stream, summary))
        .collect();
    let image_container = is_image_container(demuxer.container());

    // VFR needs timestamps: the index first, then a bounded packet scan for the rest.
    let mut needs_scan = Vec::new();
    for s in &mut streams {
        if let StreamDetail::Video(v) = &mut s.detail {
            if s.attached_picture || image_container {
                continue;
            }
            let timestamps = demuxer
                .input()
                .stream(s.index)
                .map(|st| ffi::index_timestamps(&st, VFR_INDEX_FRAMES))
                .unwrap_or_default();
            let indexed_all = s
                .frames
                .is_some_and(|n| timestamps.len() as u64 >= n.min(VFR_INDEX_FRAMES as u64));
            if indexed_all && timestamps.len() >= 3 {
                v.vfr = judge_vfr(timestamps, s.time_base, VfrSource::ContainerIndex);
            } else {
                needs_scan.push(s.index);
            }
        }
    }
    if !needs_scan.is_empty() {
        let scanned = scan_packets(&mut demuxer, &needs_scan)?;
        for (index, timestamps) in scanned {
            if let Some(s) = streams.iter_mut().find(|s| s.index == index)
                && let StreamDetail::Video(v) = &mut s.detail
            {
                v.vfr = judge_vfr(timestamps, s.time_base, VfrSource::PacketScan);
            }
        }
    }
    for s in &mut streams {
        if let StreamDetail::Video(v) = &mut s.detail {
            v.frame_rate = if v.vfr.is_vfr {
                v.avg_frame_rate.or(v.real_frame_rate)
            } else {
                v.real_frame_rate.or(v.avg_frame_rate)
            };
        }
    }

    let tags = dictionary(demuxer.input().metadata());
    let video_timecode = streams
        .iter()
        .find(|s| s.kind == MediaKind::Video && !s.attached_picture)
        .and_then(|s| s.tag("timecode"));
    let tmcd_timecode = streams
        .iter()
        .find(|s| s.codec.fourcc.as_deref() == Some("tmcd"))
        .and_then(|s| s.tag("timecode"));
    let timecode = video_timecode
        .or(tmcd_timecode)
        .or_else(|| find_tag(&tags, "timecode"))
        .map(str::to_string);
    let creation_time = find_tag(&tags, "creation_time")
        .or_else(|| streams.iter().find_map(|s| s.tag("creation_time")))
        .map(str::to_string);

    Ok(MediaInfo {
        path: path.to_path_buf(),
        size_bytes: std::fs::metadata(path).ok().map(|m| m.len()),
        container: demuxer.container().to_string(),
        container_long_name: demuxer.container_long_name().to_string(),
        duration: demuxer.duration(),
        start_time: demuxer.start_time(),
        bit_rate: demuxer.bit_rate(),
        creation_time,
        timecode,
        tags,
        streams,
    })
}

/// FFmpeg's image demuxers: `image2` for files and numbered sequences, `<codec>_pipe`
/// for single images.
fn is_image_container(container: &str) -> bool {
    container == "image2" || container.ends_with("_pipe")
}

fn dictionary(d: ffmpeg::DictionaryRef<'_>) -> Vec<(String, String)> {
    d.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn fourcc(tag: u32) -> Option<String> {
    let bytes = tag.to_le_bytes();
    (tag != 0 && bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' '))
        .then(|| String::from_utf8_lossy(&bytes).into_owned())
}

fn rate(r: ffmpeg::Rational) -> Option<Rational> {
    let r = Rational::from(r);
    r.is_valid().then(|| r.reduced())
}

fn stream_info(stream: &ffmpeg::format::stream::Stream, summary: &StreamSummary) -> StreamInfo {
    let params = ffi::stream_params(stream);
    let id = stream.parameters().id();
    let tags = dictionary(stream.metadata());
    let codec = CodecInfo {
        name: summary.codec,
        long_name: ffmpeg::decoder::find(id).map(|c| c.description().to_string()),
        profile: (params.profile != UNKNOWN)
            .then(|| ffi::profile_name(id, params.profile))
            .flatten(),
        level: (params.level != UNKNOWN && params.level > 0).then_some(params.level),
        fourcc: fourcc(params.codec_tag),
    };
    let detail = match summary.kind {
        MediaKind::Video => StreamDetail::Video(video_info(stream, &params)),
        MediaKind::Audio => StreamDetail::Audio(AudioInfo {
            sample_rate: u32::try_from(params.sample_rate).unwrap_or(0),
            channels: u16::try_from(params.channels).unwrap_or(0),
            channel_layout: params.channel_layout.clone(),
            sample_format: SampleFormat::from_ffmpeg(params.sample_format),
            bit_depth: [params.bits_per_raw_sample, params.bits_per_coded_sample]
                .into_iter()
                .find(|b| *b > 0)
                .and_then(|b| u8::try_from(b).ok()),
            frame_size: u32::try_from(params.frame_size).ok().filter(|n| *n > 0),
        }),
        _ => StreamDetail::Other,
    };
    StreamInfo {
        index: summary.index,
        kind: summary.kind,
        codec,
        time_base: summary.time_base,
        start: summary.start,
        duration: summary.duration,
        frames: summary.frames,
        bit_rate: u64::try_from(stream.parameters().bit_rate())
            .ok()
            .filter(|b| *b > 0),
        language: find_tag(&tags, "language")
            .filter(|l| *l != "und")
            .map(str::to_string),
        attached_picture: summary.attached_picture,
        tags,
        detail,
    }
}

fn video_info(stream: &ffmpeg::format::stream::Stream, params: &ffi::StreamParams) -> VideoInfo {
    let width = u32::try_from(params.width).unwrap_or(0);
    let height = u32::try_from(params.height).unwrap_or(0);
    let pixel_format = (params.pixel_format != ffmpeg::format::Pixel::None)
        .then(|| PixelFormat::from_ffmpeg(params.pixel_format));
    let bit_depth = u8::try_from(params.bits_per_raw_sample)
        .ok()
        .filter(|b| *b > 0)
        .or_else(|| pixel_format.map(|p| p.bit_depth()))
        .unwrap_or(0);
    let sar = if params.sample_aspect_ratio.is_valid() {
        params.sample_aspect_ratio.reduced()
    } else {
        Rational::new(1, 1)
    };
    let display_aspect_ratio = display_aspect(width, height, sar);
    let colour = ColourInfo::from_ffmpeg(
        params.primaries,
        params.transfer,
        params.matrix,
        params.range,
        params.chroma_location,
    );
    let colour_space = ColourSpace::resolve(
        &colour,
        LayoutHint {
            bit_depth,
            rgb: pixel_format.is_some_and(|p| p.is_rgb()),
            jpeg: pixel_format.is_some_and(|p| p.name().starts_with("yuvj")),
            surface_full_range: None,
            height,
        },
    );
    let (rotation, mirrored) = params
        .display_matrix
        .map(|m| orientation(ffi::display_rotation(&m), &m))
        .unwrap_or((0, false));
    VideoInfo {
        width,
        height,
        pixel_format,
        bit_depth,
        sample_aspect_ratio: sar,
        display_aspect_ratio,
        avg_frame_rate: rate(stream.avg_frame_rate()),
        real_frame_rate: rate(stream.rate()),
        frame_rate: None,
        vfr: VfrScan {
            is_vfr: false,
            source: VfrSource::NotChecked,
            frames: 0,
            min_interval: None,
            max_interval: None,
        },
        hdr: colour.hdr(),
        colour,
        colour_space,
        mastering_display_metadata: params.mastering_display,
        content_light_level: params.content_light_level,
        dolby_vision: params.dovi,
        rotation,
        mirrored,
        interlaced: !matches!(
            params.field_order,
            ffmpeg::FieldOrder::Progressive | ffmpeg::FieldOrder::Unknown
        ),
    }
}

fn display_aspect(width: u32, height: u32, sar: Rational) -> Rational {
    if width == 0 || height == 0 {
        return Rational::new(0, 1);
    }
    let num = i64::from(width) * i64::from(sar.num);
    let den = i64::from(height) * i64::from(sar.den);
    let g = gcd(num.unsigned_abs(), den.unsigned_abs()).max(1) as i64;
    Rational::new(
        i32::try_from(num / g).unwrap_or(0),
        i32::try_from(den / g).unwrap_or(1),
    )
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Rotation (counter-clockwise degrees in 0..360) and mirroring of a display matrix.
fn orientation(ccw_degrees: f64, matrix: &[i32; 9]) -> (u16, bool) {
    // A negative determinant of the 2x2 part means the matrix also flips.
    let det =
        i64::from(matrix[0]) * i64::from(matrix[4]) - i64::from(matrix[1]) * i64::from(matrix[3]);
    if !ccw_degrees.is_finite() {
        return (0, det < 0);
    }
    let degrees = ccw_degrees.round().rem_euclid(360.0) as u16;
    (degrees % 360, det < 0)
}

/// Reads packets from the start of the file until each listed stream has
/// `VFR_SCAN_PACKETS` timestamps or the bound is reached.
fn scan_packets(demuxer: &mut Demuxer, streams: &[usize]) -> Result<Vec<(usize, Vec<i64>)>> {
    let mut found: Vec<(usize, Vec<i64>)> = streams.iter().map(|i| (*i, Vec::new())).collect();
    let mut read = 0;
    while read < VFR_SCAN_MAX_PACKETS && found.iter().any(|(_, t)| t.len() < VFR_SCAN_PACKETS) {
        let Some(packet) = demuxer.read_packet()? else {
            break;
        };
        read += 1;
        if let Some((_, t)) = found.iter_mut().find(|(i, _)| *i == packet.stream_index())
            && t.len() < VFR_SCAN_PACKETS
            && let Some(pts) = packet.pts().or_else(|| packet.dts())
        {
            t.push(pts.value);
        }
    }
    Ok(found)
}

fn judge_vfr(mut timestamps: Vec<i64>, time_base: Rational, source: VfrSource) -> VfrScan {
    timestamps.sort_unstable();
    timestamps.dedup();
    let intervals = timestamps.windows(2).map(|w| w[1] - w[0]);
    let (min, max) = intervals.fold((i64::MAX, 0), |(lo, hi), d| (lo.min(d), hi.max(d)));
    if timestamps.len() < 3 || min <= 0 {
        return VfrScan {
            is_vfr: false,
            source: VfrSource::NotChecked,
            frames: timestamps.len(),
            min_interval: None,
            max_interval: None,
        };
    }
    VfrScan {
        is_vfr: max as f64 > VFR_RATIO * min as f64,
        source,
        frames: timestamps.len(),
        min_interval: Some(Timestamp::new(min, time_base)),
        max_interval: Some(Timestamp::new(max, time_base)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vfr_judgement() {
        let tb = Rational::new(1, 1000);
        // 30 fps in milliseconds: 33/34 ms jitter is constant rate.
        let cfr: Vec<i64> = (0..60).map(|n| n * 1000 / 30).collect();
        let scan = judge_vfr(cfr, tb, VfrSource::PacketScan);
        assert!(!scan.is_vfr);
        assert_eq!(scan.min_interval.map(|t| t.value), Some(33));
        assert_eq!(scan.max_interval.map(|t| t.value), Some(34));
        // Out of order (B-frames) with one held frame.
        let vfr = vec![0, 66, 33, 100, 233, 266];
        let scan = judge_vfr(vfr, tb, VfrSource::ContainerIndex);
        assert!(scan.is_vfr);
        assert_eq!(scan.source, VfrSource::ContainerIndex);
        assert!(!judge_vfr(vec![0, 40], tb, VfrSource::PacketScan).is_vfr);
    }

    #[test]
    fn rotation_and_mirroring() {
        let identity = [65536, 0, 0, 0, 65536, 0, 0, 0, 1 << 30];
        assert_eq!(orientation(0.0, &identity), (0, false));
        assert_eq!(orientation(90.0, &identity), (90, false));
        assert_eq!(orientation(-90.0, &identity), (270, false));
        assert_eq!(orientation(180.0, &identity), (180, false));
        let mirrored = [-65536, 0, 0, 0, 65536, 0, 0, 0, 1 << 30];
        assert!(orientation(0.0, &mirrored).1);
        assert_eq!(orientation(f64::NAN, &identity), (0, false));
    }

    #[test]
    fn aspect_ratios() {
        assert_eq!(
            display_aspect(1920, 1080, Rational::new(1, 1)),
            Rational::new(16, 9)
        );
        assert_eq!(
            display_aspect(1440, 1080, Rational::new(4, 3)),
            Rational::new(16, 9)
        );
        assert_eq!(
            display_aspect(720, 576, Rational::new(16, 15)),
            Rational::new(4, 3)
        );
    }

    #[test]
    fn fourcc_text() {
        assert_eq!(
            fourcc(u32::from_le_bytes(*b"avc1")).as_deref(),
            Some("avc1")
        );
        assert_eq!(fourcc(0), None);
        assert_eq!(fourcc(0x0000_0001), None);
    }
}
