//! Decoding: a [`Decoder`] per stream turns [`Packet`]s into [`Frame`]s with FFmpeg's
//! send/receive model.
//!
//! ```text
//! for each packet of the stream:  send_packet, then receive_frame until NeedsInput
//! at the end of the file:         send_eof, then receive_frame until Finished
//! after Demuxer::seek:            flush, then continue as above
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use ffmpeg_next as ffmpeg;

use crate::demux::{Demuxer, Packet};
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::{ColourInfo, MediaKind, PixelFormat, SampleFormat};
use crate::hw::{self, DecodePath, FallbackReason, HwApi, HwFrame, HwPreference};
use crate::time::{Rational, Timestamp};

/// Decoder settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecoderOptions {
    /// Decoder worker threads (frame and slice threading). Zero lets FFmpeg pick one per
    /// core, which suits a single stream; the decode pool lowers it when many streams
    /// decode at once. A hardware decoder always uses one.
    pub threads: usize,
    /// Hardware decode for video streams; audio always decodes in software. Software
    /// ([`HwPreference::Off`]) unless asked, so CPU consumers (thumbnails, analysis) get
    /// planes; playback asks for [`HwPreference::Auto`].
    pub hw: HwPreference,
}

impl DecoderOptions {
    /// Default threading with the given hardware preference.
    pub fn with_hw(hw: HwPreference) -> Self {
        Self { threads: 0, hw }
    }
}

/// A hardware decoder's device and `get_format` state. The codec context points at
/// `negotiation`, so a `Decoder` declares this after `inner` and drops it last.
struct HwState {
    negotiation: Arc<ffi::HwNegotiation>,
    _device: ffi::HwDevice,
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
    hw: Option<HwState>,
    decode_path: DecodePath,
    preference: HwPreference,
    index: usize,
    time_base: Rational,
    codec: &'static str,
    path: PathBuf,
    eof_sent: bool,
    frames_out: u64,
    /// Test hook: fail like a broken hardware decoder after this many frames.
    fail_after: Option<u64>,
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
            .field("decode_path", &self.decode_path)
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
        let mut fallback = FallbackReason::NotRequested;
        if summary.kind == MediaKind::Video && options.hw != HwPreference::Off {
            let codec_tag = ffi::stream_params(&stream).codec_tag;
            match open_hardware(&stream, summary.codec, codec_tag) {
                Ok((decoder, state, api)) => {
                    tracing::debug!(
                        path = %path.display(),
                        stream = index,
                        codec = summary.codec,
                        api = api.name(),
                        "decoding in hardware"
                    );
                    return Ok(Self {
                        inner: Inner::Video(decoder),
                        hw: Some(state),
                        decode_path: DecodePath::Hardware { api },
                        preference: options.hw,
                        index,
                        time_base: summary.time_base,
                        codec: summary.codec,
                        path,
                        eof_sent: false,
                        frames_out: 0,
                        fail_after: None,
                    });
                }
                Err(reason) if options.hw == HwPreference::Require => {
                    return Err(MediaError::Hardware {
                        path: path.clone(),
                        index,
                        reason,
                    });
                }
                Err(reason) => {
                    tracing::info!(
                        path = %path.display(),
                        stream = index,
                        %reason,
                        "decoding in software"
                    );
                    fallback = reason;
                }
            }
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
            hw: None,
            decode_path: DecodePath::Software { reason: fallback },
            preference: options.hw,
            index,
            time_base: summary.time_base,
            codec: summary.codec,
            path,
            eof_sent: false,
            frames_out: 0,
            fail_after: None,
        })
    }

    /// How this decoder decodes: hardware, or software and why.
    pub fn decode_path(&self) -> &DecodePath {
        &self.decode_path
    }

    /// True while frames come out as hardware surfaces ([`VideoFrame::hardware`]).
    pub fn is_hardware(&self) -> bool {
        self.decode_path.is_hardware()
    }

    /// The preference the decoder was opened with.
    pub fn hw_preference(&self) -> HwPreference {
        self.preference
    }

    pub(crate) fn set_fallback(&mut self, reason: FallbackReason) {
        self.decode_path = DecodePath::Software { reason };
    }

    pub(crate) fn fail_hardware_after(&mut self, frames: u64) {
        self.fail_after = Some(frames);
    }

    /// The injected failure of [`Self::fail_hardware_after`], once it is due.
    fn injected_failure(&self) -> Result<()> {
        if self.decode_path.is_hardware() && self.fail_after.is_some_and(|n| self.frames_out >= n) {
            return Err(MediaError::Codec {
                path: self.path.clone(),
                index: self.index,
                action: "decoding in hardware",
                source: crate::error::FfmpegError {
                    code: ffmpeg::ffi::AVERROR_EXTERNAL,
                    message: "injected hardware decoder failure".to_string(),
                },
            });
        }
        Ok(())
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
        self.injected_failure()?;
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
    ///
    /// A hardware decoder returns frames whose [`VideoFrame::hardware`] surface holds the
    /// picture. If FFmpeg drops to software on its own (a profile the hardware does not
    /// take), the frame is a CPU frame and [`decode_path`](Self::decode_path) says why;
    /// with [`HwPreference::Require`] that is a [`MediaError::Hardware`] instead.
    pub fn receive_frame(&mut self) -> Result<Received> {
        self.injected_failure()?;
        let time_base = self.time_base;
        let result = match &mut self.inner {
            Inner::Video(d) => {
                let mut frame = ffmpeg::frame::Video::empty();
                d.receive_frame(&mut frame).map(|()| {
                    Frame::Video(VideoFrame {
                        inner: frame,
                        time_base,
                        hw: None,
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
        if result.is_ok() {
            self.frames_out += 1;
        }
        match result {
            Ok(Frame::Video(mut frame)) if self.hw.is_some() && self.decode_path.is_hardware() => {
                frame.hw = self.hardware_frame(&frame.inner)?;
                Ok(Received::Frame(Frame::Video(frame)))
            }
            Ok(frame) => Ok(Received::Frame(frame)),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                Ok(Received::NeedsInput)
            }
            Err(ffmpeg::Error::Eof) => Ok(Received::Finished),
            Err(e) => Err(self.error("receiving a frame", e)),
        }
    }

    /// The hardware surface of a frame from a hardware decoder, or `None` when FFmpeg fell
    /// back to software on its own (the frame is then a CPU frame).
    fn hardware_frame(&mut self, frame: &ffmpeg::frame::Video) -> Result<Option<HwFrame>> {
        #[cfg(target_os = "macos")]
        if let Some(buffer) = ffi::PixelBuffer::from_frame(frame) {
            return match HwFrame::from_videotoolbox(buffer, colour_of(frame)) {
                Ok(hw) => Ok(Some(hw)),
                Err(fourcc) => {
                    let reason = FallbackReason::FormatUnsupported {
                        codec: self.codec.to_string(),
                        format: String::from_utf8_lossy(&fourcc).into_owned(),
                    };
                    self.set_fallback(reason.clone());
                    Err(MediaError::Hardware {
                        path: self.path.clone(),
                        index: self.index,
                        reason,
                    })
                }
            };
        }
        let offered = self.hw.as_ref().is_some_and(|h| {
            h.negotiation
                .offered
                .load(std::sync::atomic::Ordering::Relaxed)
        });
        let reason = FallbackReason::FormatUnsupported {
            codec: self.codec.to_string(),
            format: PixelFormat::from_ffmpeg(frame.format()).name().to_string(),
        };
        tracing::info!(
            path = %self.path.display(),
            stream = self.index,
            offered,
            %reason,
            "the hardware decoder returned a software frame"
        );
        self.set_fallback(reason.clone());
        if self.preference == HwPreference::Require {
            return Err(MediaError::Hardware {
                path: self.path.clone(),
                index: self.index,
                reason,
            });
        }
        Ok(None)
    }

    /// Drops buffered packets and frames and clears the end-of-stream state. Call after
    /// seeking the demuxer.
    pub fn flush(&mut self) {
        self.opened().flush();
        self.eof_sent = false;
    }
}

/// Opens a decoder for `stream` with the platform's hardware decoder attached, or says why
/// it cannot.
#[cfg(target_os = "macos")]
fn open_hardware(
    stream: &ffmpeg::format::stream::Stream,
    codec_name: &'static str,
    codec_tag: u32,
) -> std::result::Result<(ffmpeg::decoder::Video, HwState, HwApi), FallbackReason> {
    let api = hw::hardware_candidate(codec_name, codec_tag)?;
    let id = stream.parameters().id();
    let unsupported = || FallbackReason::CodecUnsupported {
        codec: codec_name.to_string(),
    };
    // FFmpeg's native AV1 decoder drives hardware; libdav1d, the default, does not.
    let codec = if id == ffmpeg::codec::Id::AV1 {
        ffmpeg::decoder::find_by_name("av1")
    } else {
        ffmpeg::decoder::find(id)
    }
    .ok_or_else(unsupported)?;
    let failed = |e: ffmpeg::Error| FallbackReason::DeviceFailed {
        detail: e.to_string(),
    };
    let device = ffi::HwDevice::videotoolbox().map_err(failed)?;
    let negotiation = Arc::new(ffi::HwNegotiation::default());
    let mut context =
        ffmpeg::codec::Context::from_parameters(stream.parameters()).map_err(failed)?;
    ffi::attach_videotoolbox(&mut context, &device, &negotiation).map_err(failed)?;
    let mut decoder = context.decoder();
    decoder.set_packet_time_base(stream.time_base());
    // The hardware does the work; frame threads would only add latency.
    let mut dict = ffmpeg::Dictionary::new();
    dict.set("threads", "1");
    let opened = decoder
        .open_as_with(codec, dict)
        .and_then(|d| d.video())
        .map_err(|e| FallbackReason::FormatUnsupported {
            codec: codec_name.to_string(),
            format: e.to_string(),
        })?;
    Ok((
        opened,
        HwState {
            negotiation,
            _device: device,
        },
        api,
    ))
}

#[cfg(not(target_os = "macos"))]
fn open_hardware(
    _stream: &ffmpeg::format::stream::Stream,
    codec_name: &'static str,
    codec_tag: u32,
) -> std::result::Result<(ffmpeg::decoder::Video, HwState, HwApi), FallbackReason> {
    hw::hardware_candidate(codec_name, codec_tag)?;
    Err(FallbackReason::NoHardwareApi)
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

/// Where a video frame's pixels are.
#[derive(Debug)]
pub enum FrameStorage<'a> {
    /// CPU planes in the frame's native pixel format.
    Cpu(Vec<Plane<'a>>),
    /// A hardware surface, imported by `render::interop` without a copy.
    Hardware(&'a HwFrame),
}

/// A decoded video frame: CPU planes in its native pixel format, or a hardware surface
/// ([`VideoFrame::hardware`]) when a hardware decoder produced it.
///
/// `Send` and `Sync`; the pixel buffers are reference counted by FFmpeg (or CoreVideo),
/// so a frame can be handed from a decode thread to the render thread without a copy.
pub struct VideoFrame {
    inner: ffmpeg::frame::Video,
    time_base: Rational,
    hw: Option<HwFrame>,
}

fn colour_of(frame: &ffmpeg::frame::Video) -> ColourInfo {
    ColourInfo::from_ffmpeg(
        frame.color_primaries(),
        frame.color_transfer_characteristic(),
        frame.color_space(),
        frame.color_range(),
        frame.chroma_location(),
    )
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("format", &self.format())
            .field("pts", &self.pts())
            .field("hardware", &self.hw)
            .finish()
    }
}

impl VideoFrame {
    /// The hardware surface holding the picture, for a frame from a hardware decoder.
    pub fn hardware(&self) -> Option<&HwFrame> {
        self.hw.as_ref()
    }

    pub fn is_hardware(&self) -> bool {
        self.hw.is_some()
    }

    pub fn storage(&self) -> FrameStorage<'_> {
        match &self.hw {
            Some(hw) => FrameStorage::Hardware(hw),
            None => FrameStorage::Cpu(self.planes()),
        }
    }

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
        colour_of(&self.inner)
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
