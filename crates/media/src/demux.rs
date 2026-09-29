//! Demuxing: open a file, list its streams, read packets, seek.

use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;

use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::MediaKind;
use crate::time::{Rational, Timestamp};

const NO_PTS: i64 = i64::MIN;

/// What the container says about one stream, available without decoding.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamSummary {
    pub index: usize,
    pub kind: MediaKind,
    /// FFmpeg's codec name (`h264`, `hevc`, `aac`, `prores`); `none` for data streams.
    pub codec: &'static str,
    pub time_base: Rational,
    /// Presentation time of the first frame, when the container records it.
    pub start: Option<Timestamp>,
    pub duration: Option<Timestamp>,
    /// Frame count from the container header, when it has one.
    pub frames: Option<u64>,
    /// A cover image stored as a one-packet video stream.
    pub attached_picture: bool,
}

/// An open media file.
///
/// `Send` but not `Sync`: a demuxer lives on one decode-pool thread at a time and can be
/// moved between threads. Reading and seeking need `&mut self`.
pub struct Demuxer {
    input: ffmpeg::format::context::Input,
    path: PathBuf,
    container: String,
    container_long_name: String,
    streams: Vec<StreamSummary>,
}

impl std::fmt::Debug for Demuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Demuxer")
            .field("path", &self.path)
            .field("container", &self.container())
            .field("streams", &self.streams)
            .finish()
    }
}

impl Demuxer {
    /// Opens a file and reads enough of it to know its streams (`avformat_find_stream_info`).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        crate::init();
        let path = path.as_ref().to_path_buf();
        let input = ffmpeg::format::input(&path).map_err(|e| MediaError::Open {
            path: path.clone(),
            source: e.into(),
        })?;
        let streams = input.streams().map(|s| summarize(&s)).collect();
        let format = input.format();
        Ok(Self {
            container: format.name().to_string(),
            container_long_name: format.description().to_string(),
            input,
            path,
            streams,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// FFmpeg's short name for the container format (`mov,mp4,m4a,3gp,3g2,mj2`, `matroska,webm`).
    pub fn container(&self) -> &str {
        &self.container
    }

    /// FFmpeg's descriptive name for the container format.
    pub fn container_long_name(&self) -> &str {
        &self.container_long_name
    }

    pub fn streams(&self) -> &[StreamSummary] {
        &self.streams
    }

    pub fn stream(&self, index: usize) -> Result<&StreamSummary> {
        self.streams
            .get(index)
            .ok_or_else(|| MediaError::NoSuchStream {
                path: self.path.clone(),
                index,
            })
    }

    /// The stream FFmpeg considers the main one of this kind (`av_find_best_stream`).
    pub fn best_stream(&self, kind: MediaKind) -> Option<usize> {
        let t = match kind {
            MediaKind::Video => ffmpeg::media::Type::Video,
            MediaKind::Audio => ffmpeg::media::Type::Audio,
            MediaKind::Subtitle => ffmpeg::media::Type::Subtitle,
            MediaKind::Data => ffmpeg::media::Type::Data,
            MediaKind::Attachment => ffmpeg::media::Type::Attachment,
            MediaKind::Unknown => return None,
        };
        self.input.streams().best(t).map(|s| s.index())
    }

    /// The container's duration, in microseconds.
    pub fn duration(&self) -> Option<Timestamp> {
        let d = self.input.duration();
        (d != NO_PTS && d > 0).then(|| Timestamp::new(d, Rational::MICROS))
    }

    /// The container's start time, in microseconds.
    pub fn start_time(&self) -> Option<Timestamp> {
        ffi::format_start_time(&self.input).map(|t| Timestamp::new(t, Rational::MICROS))
    }

    /// Container-level bit rate in bits per second, when known.
    pub fn bit_rate(&self) -> Option<u64> {
        u64::try_from(self.input.bit_rate()).ok().filter(|b| *b > 0)
    }

    /// The next packet of any stream, in file order, or `None` at the end of the file.
    pub fn read_packet(&mut self) -> Result<Option<Packet>> {
        let mut packet = ffmpeg::Packet::empty();
        loop {
            match packet.read(&mut self.input) {
                Ok(()) => break,
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                    continue;
                }
                Err(e) => {
                    return Err(MediaError::Read {
                        path: self.path.clone(),
                        source: e.into(),
                    });
                }
            }
        }
        let time_base = self
            .streams
            .get(packet.stream())
            .map_or(Rational::new(0, 1), |s| s.time_base);
        Ok(Some(Packet {
            inner: packet,
            time_base,
        }))
    }

    /// Moves every stream to the last keyframe at or before `target`. Decoders fed from
    /// this demuxer must be flushed afterwards; the frames they produce next may start
    /// before `target`, and callers drop them until they reach it.
    pub fn seek(&mut self, target: Timestamp) -> Result<()> {
        let ts = target.rescale(Rational::MICROS).value;
        self.input
            // ffmpeg-next passes the range end through as FFmpeg's inclusive max_ts.
            .seek(ts, ..ts)
            .map_err(|e| MediaError::Seek {
                path: self.path.clone(),
                target,
                source: e.into(),
            })
    }

    /// Moves to the last keyframe of `stream` whose index timestamp is at or before
    /// `target` (rescaled to the stream's time base, rounded down). Containers index by
    /// decode time (MOV, MP4) or presentation time (Matroska), so callers pass a
    /// keyframe's decode time when they know it. Flush decoders afterwards.
    pub fn seek_stream(&mut self, stream: usize, target: Timestamp) -> Result<()> {
        let time_base = self.stream(stream)?.time_base;
        let ts = target.rescale_floor(time_base).value;
        ffi::seek_stream(&mut self.input, stream, ts).map_err(|e| MediaError::Seek {
            path: self.path.clone(),
            target,
            source: e.into(),
        })
    }

    /// The display matrix (rotation and mirroring) of a stream, as stored.
    pub fn display_matrix(&self, stream: usize) -> Option<[i32; 9]> {
        self.input
            .stream(stream)
            .and_then(|s| ffi::display_matrix(&s))
    }

    /// Seeks back to the first packet of the file.
    pub fn rewind(&mut self) -> Result<()> {
        let start = self
            .start_time()
            .unwrap_or(Timestamp::new(0, Rational::MICROS));
        // Some files have no keyframe at or before their start time (edit lists that
        // start mid-GOP); fall back to the nearest keyframe in either direction.
        if self.seek(start).is_ok() {
            return Ok(());
        }
        self.input
            .seek(start.value, ..)
            .map_err(|e| MediaError::Seek {
                path: self.path.clone(),
                target: start,
                source: e.into(),
            })
    }

    pub(crate) fn input(&self) -> &ffmpeg::format::context::Input {
        &self.input
    }

    pub(crate) fn input_mut(&mut self) -> &mut ffmpeg::format::context::Input {
        &mut self.input
    }
}

fn summarize(stream: &ffmpeg::format::stream::Stream) -> StreamSummary {
    let params = stream.parameters();
    let time_base = Rational::from(stream.time_base());
    let ts = |v: i64| (v != NO_PTS).then(|| Timestamp::new(v, time_base));
    StreamSummary {
        index: stream.index(),
        kind: params.medium().into(),
        codec: params.id().name(),
        time_base,
        start: ts(stream.start_time()),
        duration: ts(stream.duration()).filter(|d| d.value > 0),
        frames: u64::try_from(stream.frames()).ok().filter(|n| *n > 0),
        attached_picture: stream
            .disposition()
            .contains(ffmpeg::format::stream::Disposition::ATTACHED_PIC),
    }
}

/// One compressed packet of one stream, with its timestamps in the stream's time base.
///
/// `Send` and `Sync`; the payload is reference counted by FFmpeg.
pub struct Packet {
    inner: ffmpeg::Packet,
    time_base: Rational,
}

impl std::fmt::Debug for Packet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Packet")
            .field("stream", &self.stream_index())
            .field("pts", &self.inner.pts())
            .field("dts", &self.inner.dts())
            .field("duration", &self.inner.duration())
            .field("key", &self.is_key())
            .field("size", &self.size())
            .finish()
    }
}

impl Packet {
    pub(crate) fn from_encoder(inner: ffmpeg::Packet, time_base: Rational) -> Self {
        Self { inner, time_base }
    }

    /// A reference to the same payload with its timestamps moved by `offset` (in the
    /// packet's time base) and its stream index set to `stream`: for writing packets of
    /// several files into one (stream copy, concatenation).
    pub fn retimed(&self, stream: usize, offset: i64) -> Self {
        let mut inner = self.inner.clone();
        inner.set_stream(stream);
        inner.set_pts(self.inner.pts().map(|t| t + offset));
        inner.set_dts(self.inner.dts().map(|t| t + offset));
        inner.set_position(-1);
        Self {
            inner,
            time_base: self.time_base,
        }
    }

    pub fn stream_index(&self) -> usize {
        self.inner.stream()
    }

    pub fn time_base(&self) -> Rational {
        self.time_base
    }

    /// Presentation time, when the container stores one.
    pub fn pts(&self) -> Option<Timestamp> {
        self.inner.pts().map(|v| Timestamp::new(v, self.time_base))
    }

    /// Decode time, when the container stores one.
    pub fn dts(&self) -> Option<Timestamp> {
        self.inner.dts().map(|v| Timestamp::new(v, self.time_base))
    }

    /// How long this packet is presented, when known (zero means unknown).
    pub fn duration(&self) -> Option<Timestamp> {
        let d = self.inner.duration();
        (d > 0).then(|| Timestamp::new(d, self.time_base))
    }

    pub fn is_key(&self) -> bool {
        self.inner.is_key()
    }

    pub fn size(&self) -> usize {
        self.inner.size()
    }

    pub fn data(&self) -> &[u8] {
        self.inner.data().unwrap_or(&[])
    }

    /// Byte offset in the file, when known.
    pub fn position(&self) -> Option<u64> {
        u64::try_from(self.inner.position()).ok()
    }

    pub(crate) fn raw(&self) -> &ffmpeg::Packet {
        &self.inner
    }

    pub(crate) fn into_raw(self) -> ffmpeg::Packet {
        self.inner
    }
}
