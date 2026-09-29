//! One API for every picture source: video files, still images, animations and numbered
//! image sequences all open as a [`VideoSource`], which hands out [`VideoFrame`]s with a
//! resolved [`ColourSpace`](crate::ColourSpace) and a [`SourceTimeline`] that
//! [`TimestampMap`] puts on the sequence rate.
//!
//! | Source | Decoder | Timeline |
//! | --- | --- | --- |
//! | Video | FFmpeg ([`FrameReader`]), hardware or software | the stream's ([`SourceTimeline::scan`]) |
//! | Still (PNG, JPEG, TIFF, BMP, static WebP and GIF) | `image` | [`FrameTimes::Still`](crate::timemap::FrameTimes::Still): one frame, length set by the project |
//! | Still (HEIC, HEIF, AVIF) | ImageIO on macOS; an [`Unsupported`] error elsewhere | as above |
//! | Animation (GIF, animated WebP) | `gif`, `image-webp` | variable rate from the frame delays, in milliseconds |
//! | Sequence (`name_0001.png`, ...) | as for stills, per file | `n / rate` at the caller's rate (the project's by default); gaps hold the previous frame |
//!
//! Pictures come out as planar RGB (`gbrp`, `gbrap`, 16-bit for deeper files), full
//! range, identity matrix, sRGB transfer, with the primaries their `cICP` chunk or ICC
//! profile declares (Display P3 and BT.2020 are recognised; see [`icc`]). EXIF
//! orientation is applied to the pixels, so frames are upright; the stored value stays
//! in [`SourceInfo::orientation`].

pub mod icc;

mod animation;
mod sequence;
mod still;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use icc::{EmbeddedColour, IccProfile};
pub use sequence::ImageSequence;
pub use still::ImageKind;

use crate::colour::{ColourSpace, LayoutHint};
use crate::decode::{DecoderOptions, VideoFrame};
use crate::error::{MediaError, Result};
use crate::format::{ColourInfo, ColourRange, MediaKind};
use crate::hw::DecodePath;
use crate::probe::{StreamDetail, probe};
use crate::reader::{FrameReader, StreamSelect};
use crate::time::{Rational, Timestamp};
use crate::timemap::{SourceTimeline, TimestampMap};
use animation::AnimationReader;

/// Why a picture cannot be decoded here, with a message for the UI (`assets/i18n`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unsupported {
    /// HEIC or HEIF on a platform without a system HEIF decoder (Windows and Linux for
    /// now; Windows could use WIC with the HEIF extension).
    Heif,
    /// AVIF stills, likewise.
    Avif,
}

impl Unsupported {
    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::Heif => "media.unsupported.heif",
            Self::Avif => "media.unsupported.avif",
        }
    }

    pub fn message(self) -> String {
        assets::i18n::text(self.i18n_key())
    }
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// What kind of picture source a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Video,
    Still,
    Animation,
    Sequence,
}

/// How to open a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceOptions {
    /// For video files.
    pub decoder: DecoderOptions,
    /// The project's frame rate: an image sequence's rate unless `sequence_rate` says
    /// otherwise.
    pub project_rate: Rational,
    /// The rate the user chose for an image sequence.
    pub sequence_rate: Option<Rational>,
}

impl SourceOptions {
    pub fn new(project_rate: Rational) -> Self {
        Self {
            decoder: DecoderOptions::default(),
            project_rate,
            sequence_rate: None,
        }
    }

    /// The rate an image sequence plays at.
    pub fn sequence_rate(&self) -> Rational {
        self.sequence_rate
            .filter(|r| r.is_valid() && r.num > 0)
            .unwrap_or(self.project_rate)
    }
}

/// What a source is, before any frame is decoded.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub kind: SourceKind,
    /// The picture format (`png`, `jpeg`, `gif`, `webp`, `heif`, ...) or, for video, the
    /// codec as FFmpeg names it.
    pub format: &'static str,
    /// Size of the decoded frames: upright for pictures, stored for video (whose display
    /// rotation stays metadata, [`crate::VideoInfo::rotation`]).
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub bit_depth: u8,
    /// The EXIF orientation the picture was stored with (1 to 8, 1 is upright); already
    /// applied to the frames. 1 for video.
    pub orientation: u8,
    /// The colour description frames carry.
    pub colour_space: ColourSpace,
    /// The embedded colour description of a picture (`cICP`, ICC), if any.
    pub embedded_colour: Option<EmbeddedColour>,
    /// Frames in the source (1 for a still; files present for a sequence).
    pub frame_count: u64,
    /// The numbered files of an image sequence, with [`ImageSequence::gaps`].
    pub sequence: Option<ImageSequence>,
}

/// Frames of a video file, still picture, animation or image sequence (see the module
/// docs). `Send` but not `Sync`, like [`FrameReader`]: owned by one decode-pool thread
/// at a time.
#[derive(Debug)]
pub struct VideoSource {
    path: PathBuf,
    info: SourceInfo,
    timeline: Option<Arc<SourceTimeline>>,
    inner: Inner,
}

#[derive(Debug)]
enum Inner {
    Video(Box<FrameReader>),
    Still(StillReader),
    Animation(Box<AnimationReader>),
    Sequence(SequenceReader),
}

impl VideoSource {
    /// Opens `path`: a picture (by its content, not its extension), a `%0Nd` sequence
    /// pattern, a folder holding a numbered sequence, or anything FFmpeg reads as video.
    pub fn open(path: impl AsRef<Path>, options: SourceOptions) -> Result<Self> {
        let path = path.as_ref();
        if path.is_dir() {
            let sequence = ImageSequence::in_folder(path)?.ok_or_else(|| MediaError::Image {
                path: path.to_path_buf(),
                message: "the folder has no numbered pictures".to_string(),
            })?;
            return Self::open_sequence(sequence, options);
        }
        if !path.exists()
            && path.to_string_lossy().contains('%')
            && let Some(sequence) = ImageSequence::from_pattern(path)?
        {
            return Self::open_sequence(sequence, options);
        }
        let header = still::read_header(path)?;
        match ImageKind::sniff(&header) {
            Some(kind) => Self::open_picture(path, kind),
            None => Self::open_video(path, options),
        }
    }

    /// Opens a numbered sequence, found with [`ImageSequence::detect`],
    /// [`ImageSequence::from_pattern`] or [`ImageSequence::in_folder`].
    pub fn open_sequence(sequence: ImageSequence, options: SourceOptions) -> Result<Self> {
        let first = sequence.path_of(sequence.first());
        let kind =
            ImageKind::sniff(&still::read_header(&first)?).ok_or_else(|| MediaError::Image {
                path: first.clone(),
                message: "not a picture format Glass reads".to_string(),
            })?;
        let header = still::read_still_header(&first, kind)?;
        let (width, height) = header.size();
        let rate = options.sequence_rate();
        let timeline = Arc::new(sequence.timeline(rate));
        let info = SourceInfo {
            kind: SourceKind::Sequence,
            format: kind.name(),
            width,
            height,
            alpha: header.alpha,
            bit_depth: header.bit_depth,
            orientation: header.orientation,
            colour_space: picture_colour_space(&header.colour, header.bit_depth, height),
            embedded_colour: embedded(&header.colour),
            frame_count: sequence.len() as u64,
            sequence: Some(sequence.clone()),
        };
        Ok(Self {
            path: sequence.pattern(),
            info,
            timeline: Some(timeline.clone()),
            inner: Inner::Sequence(SequenceReader {
                sequence,
                timeline,
                next: 0,
                last: None,
            }),
        })
    }

    fn open_picture(path: &Path, kind: ImageKind) -> Result<Self> {
        if matches!(kind, ImageKind::Gif | ImageKind::WebP)
            && let Ok(header) = animation::read_header(path, kind)
            && header.delays_ms.len() > 1
        {
            let info = SourceInfo {
                kind: SourceKind::Animation,
                format: kind.name(),
                width: header.width,
                height: header.height,
                alpha: header.alpha,
                bit_depth: 8,
                orientation: 1,
                colour_space: picture_colour_space(&header.colour, 8, header.height),
                embedded_colour: embedded(&header.colour),
                frame_count: header.delays_ms.len() as u64,
                sequence: None,
            };
            let reader = AnimationReader::new(path, header);
            return Ok(Self {
                path: path.to_path_buf(),
                info,
                timeline: Some(Arc::new(reader.timeline().clone())),
                inner: Inner::Animation(Box::new(reader)),
            });
        }
        let header = still::read_still_header(path, kind)?;
        let (width, height) = header.size();
        let info = SourceInfo {
            kind: SourceKind::Still,
            format: kind.name(),
            width,
            height,
            alpha: header.alpha,
            bit_depth: header.bit_depth,
            orientation: header.orientation,
            colour_space: picture_colour_space(&header.colour, header.bit_depth, height),
            embedded_colour: embedded(&header.colour),
            frame_count: 1,
            sequence: None,
        };
        Ok(Self {
            path: path.to_path_buf(),
            info,
            timeline: Some(Arc::new(SourceTimeline::still(STILL_TIME_BASE))),
            inner: Inner::Still(StillReader {
                path: path.to_path_buf(),
                kind,
                frame: None,
                delivered: false,
            }),
        })
    }

    fn open_video(path: &Path, options: SourceOptions) -> Result<Self> {
        let reader = FrameReader::open_with(path, StreamSelect::BestVideo, options.decoder)?;
        let info = probe(path)?;
        let stream = reader.stream().index;
        let video = info
            .streams
            .iter()
            .find(|s| s.index == stream)
            .and_then(|s| match &s.detail {
                StreamDetail::Video(v) => Some((s, v)),
                _ => None,
            })
            .ok_or_else(|| MediaError::NoStream {
                path: path.to_path_buf(),
                kind: MediaKind::Video,
            })?;
        let (stream_info, v) = video;
        let info = SourceInfo {
            kind: SourceKind::Video,
            format: stream_info.codec.name,
            width: v.width,
            height: v.height,
            alpha: v.pixel_format.is_some_and(|p| p.has_alpha()),
            bit_depth: v.bit_depth,
            orientation: 1,
            colour_space: v.colour_space,
            embedded_colour: None,
            frame_count: stream_info.frames.unwrap_or(0),
            sequence: None,
        };
        Ok(Self {
            path: path.to_path_buf(),
            info,
            timeline: None,
            inner: Inner::Video(Box::new(reader)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn info(&self) -> &SourceInfo {
        &self.info
    }

    pub fn kind(&self) -> SourceKind {
        self.info.kind
    }

    /// The source's frame times; for video, built on first use (see
    /// [`SourceTimeline::scan`]).
    pub fn timeline(&mut self) -> Result<Arc<SourceTimeline>> {
        if let Some(timeline) = &self.timeline {
            return Ok(timeline.clone());
        }
        let stream = match &self.inner {
            Inner::Video(reader) => reader.stream().index,
            _ => 0,
        };
        let (timeline, _) = SourceTimeline::scan(&self.path, StreamSelect::Index(stream))?;
        let timeline = Arc::new(timeline);
        self.timeline = Some(timeline.clone());
        Ok(timeline)
    }

    /// How a video file decodes (hardware or software and why); `None` for pictures,
    /// which always decode on the CPU.
    pub fn decode_path(&self) -> Option<&DecodePath> {
        match &self.inner {
            Inner::Video(reader) => Some(reader.decode_path()),
            _ => None,
        }
    }

    /// The next frame in presentation order, or `None` after the last one (a still has
    /// one frame).
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
        match &mut self.inner {
            Inner::Video(reader) => loop {
                match reader.next_frame()? {
                    Some(frame) => {
                        if let Some(video) = frame.into_video() {
                            return Ok(Some(video));
                        }
                    }
                    None => return Ok(None),
                }
            },
            Inner::Still(still) => {
                if still.delivered {
                    return Ok(None);
                }
                let frame = still.frame()?;
                still.delivered = true;
                Ok(Some(frame))
            }
            Inner::Animation(reader) => reader.next_frame(),
            Inner::Sequence(sequence) => {
                let index = sequence.next;
                if index >= sequence.sequence.len() {
                    return Ok(None);
                }
                sequence.frame(index).map(Some)
            }
        }
    }

    /// The frame on screen at `target` (the last one presented at or before it; the
    /// first frame for an earlier target, the last for a later one). For a still, the
    /// picture at any time. Later `next_frame` calls continue after it.
    pub fn seek_exact(&mut self, target: Timestamp) -> Result<Option<VideoFrame>> {
        match &mut self.inner {
            Inner::Video(reader) => Ok(reader.seek_exact(target)?.and_then(|f| f.into_video())),
            Inner::Still(still) => {
                let frame = still.frame()?;
                still.delivered = true;
                Ok(Some(frame))
            }
            Inner::Animation(reader) => {
                let k = frame_index_at(reader.timeline(), target);
                reader.frame(k)
            }
            Inner::Sequence(sequence) => {
                let k = frame_index_at(&sequence.timeline.clone(), target);
                sequence.frame(k).map(Some)
            }
        }
    }

    /// [`Self::seek_exact`] to the source frame that sequence frame `n` of `map` shows;
    /// `None` past the end of the clip (never for a still).
    pub fn seek_to_sequence_frame(
        &mut self,
        map: &TimestampMap,
        n: u64,
    ) -> Result<Option<VideoFrame>> {
        match map.source_pts(n) {
            Some(pts) => self.seek_exact(pts),
            None => Ok(None),
        }
    }

    /// Back to the first frame.
    pub fn seek_to_start(&mut self) -> Result<()> {
        match &mut self.inner {
            Inner::Video(reader) => reader.seek_to_start(),
            Inner::Still(still) => {
                still.delivered = false;
                Ok(())
            }
            Inner::Animation(reader) => {
                reader.rewind();
                Ok(())
            }
            Inner::Sequence(sequence) => {
                sequence.next = 0;
                Ok(())
            }
        }
    }
}

/// Stills are timed in microseconds (their one frame is at 0).
const STILL_TIME_BASE: Rational = Rational::MICROS;

/// The frame index on screen at `target`, clamped to the first and last frames.
fn frame_index_at(timeline: &SourceTimeline, target: Timestamp) -> usize {
    let count = timeline.frame_count().max(1);
    match timeline.frame_at(target) {
        Some(k) => k as usize,
        None if target.rescale_floor(timeline.time_base).value < timeline.start() => 0,
        None => (count - 1) as usize,
    }
}

fn embedded(colour: &EmbeddedColour) -> Option<EmbeddedColour> {
    colour.profile.is_some().then(|| colour.clone())
}

/// The colour a picture's frames carry: full-range RGB, sRGB transfer and BT.709
/// primaries unless the picture declares others.
fn picture_colour_space(colour: &EmbeddedColour, bit_depth: u8, height: u32) -> ColourSpace {
    let tags = ColourInfo {
        primaries: Some(
            colour
                .primaries
                .unwrap_or(crate::colour::Primaries::Bt709)
                .name(),
        ),
        transfer: Some(
            colour
                .transfer
                .unwrap_or(crate::colour::TransferFn::Srgb)
                .name(),
        ),
        matrix: Some("gbr"),
        range: ColourRange::Full,
        chroma_location: None,
    };
    ColourSpace::resolve(
        &tags,
        LayoutHint {
            bit_depth,
            rgb: true,
            jpeg: false,
            surface_full_range: None,
            height,
        },
    )
}

#[derive(Debug)]
struct StillReader {
    path: PathBuf,
    kind: ImageKind,
    /// Decoded on first use and kept: every request shares its buffers.
    frame: Option<VideoFrame>,
    delivered: bool,
}

impl StillReader {
    fn frame(&mut self) -> Result<VideoFrame> {
        if self.frame.is_none() {
            let decoded = still::decode_still(&self.path, self.kind)?;
            self.frame = Some(still::frame_from_image(
                &decoded.image,
                &decoded.header.colour,
                Timestamp::new(0, STILL_TIME_BASE),
                None,
            ));
        }
        let frame = self.frame.as_ref().ok_or_else(|| MediaError::Image {
            path: self.path.clone(),
            message: "no picture".to_string(),
        })?;
        frame.share_at(0, None).ok_or_else(|| MediaError::Image {
            path: self.path.clone(),
            message: "out of memory sharing the decoded picture".to_string(),
        })
    }
}

#[derive(Debug)]
struct SequenceReader {
    sequence: ImageSequence,
    timeline: Arc<SourceTimeline>,
    /// Index (into the present files) of the next frame `next_frame` returns.
    next: usize,
    /// The last decoded frame and its index, reused when asked again (held frames).
    last: Option<(usize, VideoFrame)>,
}

impl SequenceReader {
    /// Frame `index` of the present files, timed at its number.
    fn frame(&mut self, index: usize) -> Result<VideoFrame> {
        let pts = self.timeline.frame_pts(index as u64).map_or(0, |t| t.value);
        let end = self
            .timeline
            .frame_pts(index as u64 + 1)
            .map_or(self.timeline.end(), |t| t.value);
        self.next = index + 1;
        if let Some((k, frame)) = &self.last
            && *k == index
            && let Some(shared) = frame.share_at(pts, Some(end - pts))
        {
            return Ok(shared);
        }
        let number = self.sequence.numbers[index];
        let path = self.sequence.path_of(number);
        let kind =
            ImageKind::sniff(&still::read_header(&path)?).ok_or_else(|| MediaError::Image {
                path: path.clone(),
                message: "not a picture format Glass reads".to_string(),
            })?;
        let decoded = still::decode_still(&path, kind)?;
        let frame = still::frame_from_image(
            &decoded.image,
            &decoded.header.colour,
            Timestamp::new(pts, self.timeline.time_base),
            Some(end - pts),
        );
        if let Some(shared) = frame.share_at(pts, Some(end - pts)) {
            self.last = Some((index, shared));
        }
        Ok(frame)
    }
}
