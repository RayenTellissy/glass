//! Decoding: a [`Decoder`] per stream turns [`Packet`]s into [`Frame`]s with FFmpeg's
//! send/receive model.
//!
//! ```text
//! for each packet of the stream:  send_packet, then receive_frame until NeedsInput
//! at the end of the file:         send_eof, then receive_frame until Finished
//! after Demuxer::seek:            flush, then continue as above
//! ```

use std::path::PathBuf;

use ffmpeg_next as ffmpeg;

use crate::demux::{Demuxer, Packet};
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::{ColourInfo, MediaKind, PixelFormat, SampleFormat};
use crate::time::{Rational, Timestamp};

/// Decoder settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecoderOptions {
    /// Decoder worker threads (frame and slice threading). Zero lets FFmpeg pick one per
    /// core, which suits a single stream; the decode pool lowers it when many streams
    /// decode at once.
    pub threads: usize,
}

enum Inner {
    Video(ffmpeg::decoder::Video),
    Audio(ffmpeg::decoder::Audio),
}

/// A software decoder for one video or audio stream.
///
/// `Send` but not `Sync`: it is created and driven on one decode-pool thread
/// (`glass-decode-<n>` or `glass-audio-decode`) and may move between threads between calls.
pub struct Decoder {
    inner: Inner,
    index: usize,
    time_base: Rational,
    codec: &'static str,
    path: PathBuf,
    eof_sent: bool,
}

/// What [`Decoder::receive_frame`] produced.
#[derive(Debug)]
pub enum Received {
    Frame(Frame),
    /// The decoder needs another packet (or `send_eof`) before it can output more.
    NeedsInput,
    /// Every frame has been returned after `send_eof`. `flush` makes it usable again.
    Finished,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("path", &self.path)
            .field("stream", &self.index)
            .field("codec", &self.codec)
            .field("kind", &self.kind())
            .finish()
    }
}

impl Decoder {
    /// Opens a decoder for stream `index` of `demuxer` with default options.
    pub fn open(demuxer: &Demuxer, index: usize) -> Result<Self> {
        Self::open_with(demuxer, index, DecoderOptions::default())
    }

    pub fn open_with(demuxer: &Demuxer, index: usize, options: DecoderOptions) -> Result<Self> {
        let path = demuxer.path().to_path_buf();
        let summary = demuxer.stream(index)?.clone();
        let stream = demuxer
            .input()
            .stream(index)
            .ok_or_else(|| MediaError::NoSuchStream {
                path: path.clone(),
                index,
            })?;
        let codec_error = |action: &'static str, e: ffmpeg::Error| MediaError::Codec {
            path: path.clone(),
            index,
            action,
            source: e.into(),
        };
        if !matches!(summary.kind, MediaKind::Video | MediaKind::Audio) {
            return Err(MediaError::NotDecodable {
                path: path.clone(),
                index,
                kind: summary.kind,
            });
        }
        let codec =
            software_decoder(stream.parameters().id()).map_err(|reason| MediaError::NoDecoder {
                path: path.clone(),
                index,
                codec: summary.codec.to_string(),
                reason,
            })?;
        let context = ffmpeg::codec::Context::from_parameters(stream.parameters())
            .map_err(|e| codec_error("reading codec parameters", e))?;
        let mut decoder = context.decoder();
        decoder.set_packet_time_base(stream.time_base());
        // libavcodec defaults to one thread; "auto" keeps its frame and slice threading.
        let mut dict = ffmpeg::Dictionary::new();
        let threads = options.threads.to_string();
        dict.set(
            "threads",
            if options.threads == 0 {
                "auto"
            } else {
                &threads
            },
        );
        let opened = decoder
            .open_as_with(codec, dict)
            .map_err(|e| codec_error("opening decoder", e))?;
        let inner = match summary.kind {
            MediaKind::Video => Inner::Video(
                opened
                    .video()
                    .map_err(|e| codec_error("opening decoder", e))?,
            ),
            _ => Inner::Audio(
                opened
                    .audio()
                    .map_err(|e| codec_error("opening decoder", e))?,
            ),
        };
        Ok(Self {
            inner,
            index,
            time_base: summary.time_base,
            codec: summary.codec,
            path,
            eof_sent: false,
        })
    }

    pub fn stream_index(&self) -> usize {
        self.index
    }

    pub fn kind(&self) -> MediaKind {
        match self.inner {
            Inner::Video(_) => MediaKind::Video,
            Inner::Audio(_) => MediaKind::Audio,
        }
    }

    pub fn codec(&self) -> &'static str {
        self.codec
    }

    /// The stream time base, which frame timestamps use.
    pub fn time_base(&self) -> Rational {
        self.time_base
    }

    fn opened(&mut self) -> &mut ffmpeg::decoder::Opened {
        match &mut self.inner {
            Inner::Video(d) => d,
            Inner::Audio(d) => d,
        }
    }

    fn error(&self, action: &'static str, e: ffmpeg::Error) -> MediaError {
        MediaError::Codec {
            path: self.path.clone(),
            index: self.index,
            action,
            source: e.into(),
        }
    }

    /// Feeds one packet. Packets of other streams are ignored, so a caller may pass every
    /// packet the demuxer reads. Drain frames with `receive_frame` before sending the next
    /// packet.
    pub fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if packet.stream_index() != self.index {
            return Ok(());
        }
        match self.opened().send_packet(packet.raw()) {
            Ok(()) => Ok(()),
            // Invalid data in one packet is recoverable: FFmpeg logs it and the next
            // keyframe resynchronises the decoder.
            Err(ffmpeg::Error::InvalidData) => {
                tracing::warn!(
                    path = %self.path.display(),
                    stream = self.index,
                    "skipped an undecodable packet"
                );
                Ok(())
            }
            Err(e) => Err(self.error("sending a packet", e)),
        }
    }

    /// Signals the end of the stream so the decoder outputs the frames it still holds.
    /// Calling it again before `flush` does nothing.
    pub fn send_eof(&mut self) -> Result<()> {
        if self.eof_sent {
            return Ok(());
        }
        self.eof_sent = true;
        match self.opened().send_eof() {
            Ok(()) | Err(ffmpeg::Error::Eof) => Ok(()),
            Err(e) => Err(self.error("sending end of stream", e)),
        }
    }

    /// The next decoded frame, if the decoder has one ready.
    pub fn receive_frame(&mut self) -> Result<Received> {
        let time_base = self.time_base;
        let result = match &mut self.inner {
            Inner::Video(d) => {
                let mut frame = ffmpeg::frame::Video::empty();
                d.receive_frame(&mut frame).map(|()| {
                    Frame::Video(VideoFrame {
                        inner: frame,
                        time_base,
                    })
                })
            }
            Inner::Audio(d) => {
                let mut frame = ffmpeg::frame::Audio::empty();
                d.receive_frame(&mut frame).map(|()| {
                    Frame::Audio(AudioFrame {
                        inner: frame,
                        time_base,
                    })
                })
            }
        };
        match result {
            Ok(frame) => Ok(Received::Frame(frame)),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                Ok(Received::NeedsInput)
            }
            Err(ffmpeg::Error::Eof) => Ok(Received::Finished),
            Err(e) => Err(self.error("receiving a frame", e)),
        }
    }

    /// Drops buffered packets and frames and clears the end-of-stream state. Call after
    /// seeking the demuxer.
    pub fn flush(&mut self) {
        self.opened().flush();
        self.eof_sent = false;
    }
}

/// The software decoder for a codec, or why there is none.
fn software_decoder(id: ffmpeg::codec::Id) -> std::result::Result<ffmpeg::Codec, &'static str> {
    if id == ffmpeg::codec::Id::AV1 {
        // FFmpeg's own "av1" decoder only drives hardware decoders; software AV1 comes
        // from libdav1d (or libaom) when FFmpeg is built with it.
        return ["libdav1d", "libaom-av1"]
            .into_iter()
            .find_map(ffmpeg::decoder::find_by_name)
            .ok_or("FFmpeg was built without libdav1d or libaom");
    }
    ffmpeg::decoder::find(id).ok_or("FFmpeg was built without a decoder for it")
}

/// A decoded frame of either kind.
#[derive(Debug)]
pub enum Frame {
    Video(VideoFrame),
    Audio(AudioFrame),
}

impl Frame {
    pub fn pts(&self) -> Option<Timestamp> {
        match self {
            Self::Video(f) => f.pts(),
            Self::Audio(f) => f.pts(),
        }
    }

    pub fn into_video(self) -> Option<VideoFrame> {
        match self {
            Self::Video(f) => Some(f),
            Self::Audio(_) => None,
        }
    }

    pub fn into_audio(self) -> Option<AudioFrame> {
        match self {
            Self::Audio(f) => Some(f),
            Self::Video(_) => None,
        }
    }
}

fn best_effort_pts(frame: &ffmpeg::Frame, time_base: Rational) -> Option<Timestamp> {
    frame
        .timestamp()
        .or_else(|| frame.pts())
        .map(|v| Timestamp::new(v, time_base))
}

fn frame_duration(frame: &ffmpeg::Frame, time_base: Rational) -> Option<Timestamp> {
    let d = ffi::frame_duration(frame);
    (d > 0).then(|| Timestamp::new(d, time_base))
}

/// One plane of a video frame in CPU memory.
#[derive(Clone, Copy, Debug)]
pub struct Plane<'a> {
    /// `stride * height` bytes; each row starts `stride` bytes after the previous one and
    /// may be followed by padding.
    pub data: &'a [u8],
    /// Bytes from the start of one row to the next.
    pub stride: usize,
    /// Width in samples of this plane: the luma width for plane 0, the subsampled chroma
    /// width for chroma planes (for `nv12`, the number of interleaved U/V pairs).
    pub width: u32,
    pub height: u32,
}

/// A decoded video frame in its native pixel format, in CPU memory.
///
/// `Send` and `Sync`; the pixel buffers are reference counted by FFmpeg, so a frame can
/// be handed from a decode thread to the render thread without a copy.
pub struct VideoFrame {
    inner: ffmpeg::frame::Video,
    time_base: Rational,
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("format", &self.format())
            .field("pts", &self.pts())
            .finish()
    }
}

impl VideoFrame {
    pub fn width(&self) -> u32 {
        self.inner.width()
    }

    pub fn height(&self) -> u32 {
        self.inner.height()
    }

    pub fn format(&self) -> PixelFormat {
        PixelFormat::from_ffmpeg(self.inner.format())
    }

    /// Presentation time in the stream time base (FFmpeg's best-effort timestamp, which
    /// fills in missing or broken pts).
    pub fn pts(&self) -> Option<Timestamp> {
        best_effort_pts(&self.inner, self.time_base)
    }

    pub fn duration(&self) -> Option<Timestamp> {
        frame_duration(&self.inner, self.time_base)
    }

    pub fn time_base(&self) -> Rational {
        self.time_base
    }

    pub fn is_key(&self) -> bool {
        self.inner.is_key()
    }

    pub fn is_interlaced(&self) -> bool {
        self.inner.is_interlaced()
    }

    pub fn sample_aspect_ratio(&self) -> Rational {
        self.inner.aspect_ratio().into()
    }

    pub fn colour(&self) -> ColourInfo {
        ColourInfo::from_ffmpeg(
            self.inner.color_primaries(),
            self.inner.color_transfer_characteristic(),
            self.inner.color_space(),
            self.inner.color_range(),
            self.inner.chroma_location(),
        )
    }

    /// Number of CPU planes (0 for a hardware surface).
    pub fn plane_count(&self) -> usize {
        if self.format().is_hardware() {
            0
        } else {
            self.inner.planes()
        }
    }

    /// Plane `index` in CPU memory, or `None` past the last plane.
    pub fn plane(&self, index: usize) -> Option<Plane<'_>> {
        (index < self.plane_count()).then(|| Plane {
            data: self.inner.data(index),
            stride: self.inner.stride(index),
            width: self.inner.plane_width(index),
            height: self.inner.plane_height(index),
        })
    }

    pub fn planes(&self) -> Vec<Plane<'_>> {
        (0..self.plane_count())
            .filter_map(|i| self.plane(i))
            .collect()
    }

    pub(crate) fn raw(&self) -> &ffmpeg::frame::Video {
        &self.inner
    }
}

/// A decoded audio frame in the decoder's native sample format.
///
/// `Send` and `Sync`, like [`VideoFrame`].
pub struct AudioFrame {
    inner: ffmpeg::frame::Audio,
    time_base: Rational,
}

impl std::fmt::Debug for AudioFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioFrame")
            .field("format", &self.format())
            .field("channels", &self.channels())
            .field("rate", &self.sample_rate())
            .field("samples", &self.samples())
            .field("pts", &self.pts())
            .finish()
    }
}

impl AudioFrame {
    /// `None` only for a frame FFmpeg left without a format, which decoders do not emit.
    pub fn format(&self) -> Option<SampleFormat> {
        SampleFormat::from_ffmpeg(self.inner.format())
    }

    pub fn channels(&self) -> u16 {
        self.inner.channels()
    }

    /// FFmpeg's description of the channel layout (`stereo`, `5.1(side)`, `2 channels`).
    pub fn channel_layout(&self) -> Option<String> {
        ffi::frame_channel_layout(&self.inner)
    }

    pub fn sample_rate(&self) -> u32 {
        self.inner.rate()
    }

    /// Samples per channel.
    pub fn samples(&self) -> usize {
        self.inner.samples()
    }

    pub fn pts(&self) -> Option<Timestamp> {
        best_effort_pts(&self.inner, self.time_base)
    }

    pub fn duration(&self) -> Option<Timestamp> {
        frame_duration(&self.inner, self.time_base)
    }

    pub fn time_base(&self) -> Rational {
        self.time_base
    }

    /// One plane per channel for planar formats, a single interleaved plane otherwise.
    pub fn plane_count(&self) -> usize {
        match self.format() {
            Some(f) if f.planar => usize::from(self.channels()),
            Some(_) => 1,
            None => 0,
        }
    }

    /// The samples of plane `index` as bytes, exactly `samples x bytes per sample`
    /// (times the channel count for an interleaved plane), without padding.
    pub fn plane(&self, index: usize) -> Option<&[u8]> {
        let format = self.format()?;
        if index >= self.plane_count() {
            return None;
        }
        let per_plane_channels = if format.planar {
            1
        } else {
            usize::from(self.channels())
        };
        let len = self.samples() * format.sample.bytes() * per_plane_channels;
        self.inner.data(index).get(..len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn thread_safety() {
        assert_send::<Demuxer>();
        assert_send::<Decoder>();
        assert_send_sync::<Packet>();
        assert_send_sync::<Frame>();
        assert_send_sync::<VideoFrame>();
        assert_send_sync::<AudioFrame>();
    }
}
