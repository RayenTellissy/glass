//! Encoding: CPU frames or planar f32 audio in, compressed [`Packet`]s out.
//!
//! This is the minimal layer proxies need, shaped so the Phase 8 exporter extends it
//! rather than replaces it: settings name the FFmpeg encoder and carry every value an
//! export preset sets (size, pixel format, time base, frame rate, colour tags, GOP,
//! B-frames, rate control, private options), and `send_*`/`receive_packet`/`send_eof`
//! follow FFmpeg's send/receive model, so hardware frame input, two-pass rate control
//! and HDR side data can be added as further settings and `send_*` variants.

use ffmpeg_next as ffmpeg;

use crate::decode::VideoFrame;
use crate::demux::Packet;
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::{ColourInfo, ColourRange, MediaKind, PixelFormat, SampleFormat};
use crate::time::Rational;

/// Settings of a video encoder.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoEncoderSettings {
    /// FFmpeg encoder name (`prores_ks`, `prores_videotoolbox`, `libx264`,
    /// `h264_videotoolbox`).
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    /// Pixel format of the frames sent in (one the encoder accepts).
    pub pixel_format: PixelFormat,
    /// Time base of frame and packet timestamps.
    pub time_base: Rational,
    /// Nominal frame rate, when constant.
    pub frame_rate: Option<Rational>,
    pub sample_aspect_ratio: Rational,
    /// Colour tags written to the stream; they do not convert pixels.
    pub colour: ColourInfo,
    /// Keyframe interval in frames.
    pub gop: Option<u32>,
    pub max_b_frames: Option<u32>,
    /// Target bit rate, bits per second.
    pub bit_rate: Option<u64>,
    /// Encoder threads; zero lets FFmpeg choose.
    pub threads: usize,
    /// Codec headers go in the stream's extradata instead of every keyframe (MOV and MP4
    /// need it: [`Muxer::wants_global_header`](crate::Muxer::wants_global_header)).
    pub global_header: bool,
    /// Encoder private options (`profile`, `preset`, `crf`, `vendor`), as FFmpeg's
    /// command line would pass them.
    pub options: Vec<(String, String)>,
}

impl VideoEncoderSettings {
    /// Settings for `encoder` at a size, format and time base, everything else default.
    pub fn new(
        encoder: impl Into<String>,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        time_base: Rational,
    ) -> Self {
        Self {
            encoder: encoder.into(),
            width,
            height,
            pixel_format,
            time_base,
            frame_rate: None,
            sample_aspect_ratio: Rational::new(1, 1),
            colour: ColourInfo::default(),
            gop: None,
            max_b_frames: None,
            bit_rate: None,
            threads: 0,
            global_header: false,
            options: Vec::new(),
        }
    }

    pub fn option(mut self, key: &str, value: impl ToString) -> Self {
        self.options.push((key.to_string(), value.to_string()));
        self
    }
}

/// PCM sample formats an audio encoder takes from [`Encoder::send_audio`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PcmFormat {
    S16,
    S24,
    F32,
}

/// Settings of an audio encoder. Samples go in as planar f32 and are converted to what
/// the encoder takes.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioEncoderSettings {
    /// FFmpeg encoder name (`pcm_s16le`, `pcm_s24le`, `aac`, `flac`).
    pub encoder: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub bit_rate: Option<u64>,
    /// The sample format to feed the encoder when it takes it (`s32` makes FLAC 24-bit);
    /// otherwise the encoder's first.
    pub sample_format: Option<SampleFormat>,
    pub global_header: bool,
    pub options: Vec<(String, String)>,
}

impl AudioEncoderSettings {
    /// Little-endian PCM at a rate and channel count.
    pub fn pcm(format: PcmFormat, sample_rate: u32, channels: usize) -> Self {
        let encoder = match format {
            PcmFormat::S16 => "pcm_s16le",
            PcmFormat::S24 => "pcm_s24le",
            PcmFormat::F32 => "pcm_f32le",
        };
        Self {
            encoder: encoder.to_string(),
            sample_rate,
            channels,
            bit_rate: None,
            sample_format: None,
            global_header: false,
            options: Vec::new(),
        }
    }

    /// Settings for any audio encoder at a rate and channel count, everything else
    /// default.
    pub fn new(encoder: impl Into<String>, sample_rate: u32, channels: usize) -> Self {
        Self {
            encoder: encoder.into(),
            ..Self::pcm(PcmFormat::S16, sample_rate, channels)
        }
    }
}

enum Inner {
    Video(ffmpeg::encoder::video::Encoder),
    Audio(ffmpeg::encoder::audio::Encoder),
}

/// An open encoder.
///
/// Video: [`send_video`](Self::send_video) takes CPU frames already at the encoder's
/// size and pixel format (use [`FrameScaler`] to get there), with timestamps in the
/// encoder's time base. Audio: [`send_audio`](Self::send_audio) takes planar f32 and
/// numbers samples itself (time base `1/sample_rate`). Packets come out of
/// [`receive_packet`](Self::receive_packet) in decode order with timestamps in
/// [`time_base`](Self::time_base); [`send_eof`](Self::send_eof) then draining flushes.
///
/// `Send` but not `Sync`.
pub struct Encoder {
    inner: Inner,
    name: String,
    time_base: Rational,
    /// Audio: next sample index.
    next_sample: i64,
    audio_format: ffmpeg::format::Sample,
    audio_layout: Option<ffmpeg::ChannelLayout>,
    sample_rate: u32,
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("name", &self.name)
            .field("kind", &self.kind())
            .field("time_base", &self.time_base)
            .finish()
    }
}

/// Whether this FFmpeg build has an encoder of that name.
pub fn encoder_available(name: &str) -> bool {
    crate::init();
    ffmpeg::encoder::find_by_name(name).is_some()
}

/// The pixel formats a video encoder of that name takes, empty when the build has no
/// such encoder or it does not list them.
pub(crate) fn encoder_pixel_formats(name: &str) -> Vec<PixelFormat> {
    crate::init();
    ffmpeg::encoder::find_by_name(name)
        .and_then(|c| c.video().ok())
        .and_then(|v| v.formats())
        .map(|f| f.map(PixelFormat::from_ffmpeg).collect())
        .unwrap_or_default()
}

fn dictionary(options: &[(String, String)]) -> ffmpeg::Dictionary<'static> {
    let mut dict = ffmpeg::Dictionary::new();
    for (k, v) in options {
        dict.set(k, v);
    }
    dict
}

impl Encoder {
    /// Opens a video encoder.
    pub fn video(settings: &VideoEncoderSettings) -> Result<Self> {
        crate::init();
        let name = settings.encoder.clone();
        let error = |action: &'static str| {
            let name = name.clone();
            move |e: ffmpeg::Error| MediaError::Encode {
                encoder: name,
                action,
                source: e.into(),
            }
        };
        let codec = ffmpeg::encoder::find_by_name(&settings.encoder)
            .ok_or(ffmpeg::Error::EncoderNotFound)
            .map_err(error("finding the encoder"))?;
        let mut context = ffmpeg::codec::Context::new_with_codec(codec);
        ffi::set_encoder_threads(&mut context, settings.threads);
        if settings.global_header {
            context.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }
        let (primaries, transfer, matrix) = ffi::colour_enums(
            settings.colour.primaries,
            settings.colour.transfer,
            settings.colour.matrix,
        );
        if let Some(location) = settings
            .colour
            .chroma_location
            .and_then(chroma_location_from_name)
        {
            ffi::set_chroma_location(&mut context, location);
        }
        let mut video = context
            .encoder()
            .video()
            .map_err(error("configuring the encoder"))?;
        video.set_width(settings.width);
        video.set_height(settings.height);
        video.set_format(settings.pixel_format.to_ffmpeg());
        video.set_time_base(ffmpeg::Rational::from(settings.time_base));
        if let Some(rate) = settings.frame_rate {
            video.set_frame_rate(Some(ffmpeg::Rational::from(rate)));
        }
        if settings.sample_aspect_ratio.is_valid() {
            video.set_aspect_ratio(ffmpeg::Rational::from(settings.sample_aspect_ratio));
        }
        video.set_color_primaries(primaries);
        video.set_color_transfer_characteristic(transfer);
        video.set_colorspace(matrix);
        video.set_color_range(match settings.colour.range {
            ColourRange::Full => ffmpeg::color::Range::JPEG,
            ColourRange::Limited => ffmpeg::color::Range::MPEG,
            ColourRange::Unspecified => ffmpeg::color::Range::Unspecified,
        });
        if let Some(gop) = settings.gop {
            video.set_gop(gop);
        }
        if let Some(b) = settings.max_b_frames {
            video.set_max_b_frames(b as usize);
        }
        if let Some(rate) = settings.bit_rate {
            video.set_bit_rate(rate as usize);
        }
        let opened = video
            .open_as_with(codec, dictionary(&settings.options))
            .map_err(error("opening the encoder"))?;
        let time_base = Rational::from(opened.time_base());
        Ok(Self {
            inner: Inner::Video(opened),
            name: settings.encoder.clone(),
            time_base: if time_base.is_valid() {
                time_base
            } else {
                settings.time_base
            },
            next_sample: 0,
            audio_format: ffmpeg::format::Sample::None,
            audio_layout: None,
            sample_rate: 0,
        })
    }

    /// Opens an audio encoder.
    pub fn audio(settings: &AudioEncoderSettings) -> Result<Self> {
        crate::init();
        let name = settings.encoder.clone();
        let error = |action: &'static str| {
            let name = name.clone();
            move |e: ffmpeg::Error| MediaError::Encode {
                encoder: name,
                action,
                source: e.into(),
            }
        };
        let codec = ffmpeg::encoder::find_by_name(&settings.encoder)
            .ok_or(ffmpeg::Error::EncoderNotFound)
            .map_err(error("finding the encoder"))?;
        let formats: Vec<ffmpeg::format::Sample> = codec
            .audio()
            .ok()
            .and_then(|a| a.formats())
            .map(|f| f.collect())
            .unwrap_or_default();
        let wanted = settings.sample_format;
        let format = formats
            .iter()
            .copied()
            .find(|f| wanted.is_some() && SampleFormat::from_ffmpeg(*f) == wanted)
            .or_else(|| formats.first().copied())
            .ok_or(ffmpeg::Error::InvalidData)
            .map_err(error("choosing a sample format"))?;
        let mut context = ffmpeg::codec::Context::new_with_codec(codec);
        if settings.global_header {
            context.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }
        let mut audio = context
            .encoder()
            .audio()
            .map_err(error("configuring the encoder"))?;
        let channels = i32::try_from(settings.channels)
            .map_err(|_| ffmpeg::Error::InvalidData)
            .map_err(error("setting the channel layout"))?;
        let layout = ffmpeg::ChannelLayout::default(channels);
        audio.set_rate(settings.sample_rate as i32);
        audio.set_channel_layout(layout);
        audio.set_format(format);
        audio.set_time_base(ffmpeg::Rational::new(1, settings.sample_rate as i32));
        if let Some(rate) = settings.bit_rate {
            audio.set_bit_rate(rate as usize);
        }
        let opened = audio
            .open_as_with(codec, dictionary(&settings.options))
            .map_err(error("opening the encoder"))?;
        Ok(Self {
            inner: Inner::Audio(opened),
            name: settings.encoder.clone(),
            time_base: Rational::new(1, settings.sample_rate as i32),
            next_sample: 0,
            audio_format: format,
            audio_layout: Some(layout),
            sample_rate: settings.sample_rate,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> MediaKind {
        match self.inner {
            Inner::Video(_) => MediaKind::Video,
            Inner::Audio(_) => MediaKind::Audio,
        }
    }

    /// Time base of the packets this encoder produces.
    pub fn time_base(&self) -> Rational {
        self.time_base
    }

    /// Audio: the samples per channel every [`send_audio`](Self::send_audio) call must
    /// carry (the last may be shorter), or `None` when any count is accepted (PCM).
    pub fn frame_size(&self) -> Option<usize> {
        let Inner::Audio(encoder) = &self.inner else {
            return None;
        };
        let variable = encoder.codec().is_some_and(|c| {
            c.capabilities()
                .contains(ffmpeg::codec::Capabilities::VARIABLE_FRAME_SIZE)
        });
        let size = encoder.frame_size() as usize;
        (!variable && size > 0).then_some(size)
    }

    /// Audio: whether the last frame may be shorter than [`frame_size`](Self::frame_size).
    pub fn accepts_small_last_frame(&self) -> bool {
        self.codec().is_some_and(|c| {
            c.capabilities()
                .contains(ffmpeg::codec::Capabilities::SMALL_LAST_FRAME)
        })
    }

    fn error(&self, action: &'static str) -> impl Fn(ffmpeg::Error) -> MediaError + '_ {
        move |e| MediaError::Encode {
            encoder: self.name.clone(),
            action,
            source: e.into(),
        }
    }

    /// Sends a CPU frame at the encoder's size and pixel format. Its pts (and duration,
    /// when set) must be in the encoder's time base.
    pub fn send_video(&mut self, frame: &VideoFrame) -> Result<()> {
        let Inner::Video(encoder) = &mut self.inner else {
            return Err(MediaError::Encode {
                encoder: self.name.clone(),
                action: "sending a video frame to an audio encoder",
                source: ffmpeg::Error::InvalidData.into(),
            });
        };
        let raw = frame.raw();
        if raw.width() != encoder.width()
            || raw.height() != encoder.height()
            || raw.format() != encoder.format()
        {
            return Err(MediaError::Encode {
                encoder: self.name.clone(),
                action: "sending a frame of another size or pixel format",
                source: ffmpeg::Error::InvalidData.into(),
            });
        }
        let result = encoder.send_frame(raw);
        result.map_err(self.error("sending a frame"))
    }

    /// Sends planar f32 samples (one slice per channel, equal lengths), numbered on from
    /// the previous call; converted to the encoder's sample format (clamped to ±1 for
    /// integer formats).
    pub fn send_audio(&mut self, planes: &[&[f32]]) -> Result<()> {
        let layout = self.audio_layout;
        let format = self.audio_format;
        let Inner::Audio(encoder) = &mut self.inner else {
            return Err(MediaError::Encode {
                encoder: self.name.clone(),
                action: "sending audio to a video encoder",
                source: ffmpeg::Error::InvalidData.into(),
            });
        };
        let (Some(layout), Some(first)) = (layout, planes.first()) else {
            return Ok(());
        };
        let samples = first.len();
        if samples == 0 {
            return Ok(());
        }
        let mut frame = ffmpeg::frame::Audio::new(format, samples, layout);
        frame.set_rate(self.sample_rate);
        frame.set_pts(Some(self.next_sample));
        write_samples(&mut frame, format, planes);
        let result = encoder.send_frame(&frame);
        self.next_sample += samples as i64;
        result.map_err(|e| MediaError::Encode {
            encoder: self.name.clone(),
            action: "sending audio",
            source: e.into(),
        })
    }

    /// No more input: the encoder drains its delayed packets.
    pub fn send_eof(&mut self) -> Result<()> {
        let result = match &mut self.inner {
            Inner::Video(e) => e.send_eof(),
            Inner::Audio(e) => e.send_eof(),
        };
        match result {
            Ok(()) | Err(ffmpeg::Error::Eof) => Ok(()),
            Err(e) => Err(self.error("draining")(e)),
        }
    }

    /// The next packet, or `None` when the encoder needs more input (or, after
    /// [`send_eof`](Self::send_eof), has nothing left).
    pub fn receive_packet(&mut self) -> Result<Option<Packet>> {
        let mut packet = ffmpeg::Packet::empty();
        let result = match &mut self.inner {
            Inner::Video(e) => e.receive_packet(&mut packet),
            Inner::Audio(e) => e.receive_packet(&mut packet),
        };
        match result {
            Ok(()) => Ok(Some(Packet::from_encoder(packet, self.time_base))),
            Err(ffmpeg::Error::Eof) => Ok(None),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => Ok(None),
            Err(e) => Err(self.error("receiving a packet")(e)),
        }
    }

    pub(crate) fn codec(&self) -> Option<ffmpeg::Codec> {
        match &self.inner {
            Inner::Video(e) => e.codec(),
            Inner::Audio(e) => e.codec(),
        }
    }

    pub(crate) fn parameters(&self) -> ffmpeg::codec::Parameters {
        match &self.inner {
            Inner::Video(e) => ffmpeg::codec::Parameters::from(e),
            Inner::Audio(e) => ffmpeg::codec::Parameters::from(e),
        }
    }
}

fn chroma_location_from_name(name: &str) -> Option<ffmpeg::chroma::Location> {
    use ffmpeg::chroma::Location;
    Some(match name {
        "left" => Location::Left,
        "center" => Location::Center,
        "topleft" => Location::TopLeft,
        "top" => Location::Top,
        "bottomleft" => Location::BottomLeft,
        "bottom" => Location::Bottom,
        _ => return None,
    })
}

fn write_samples(
    frame: &mut ffmpeg::frame::Audio,
    format: ffmpeg::format::Sample,
    planes: &[&[f32]],
) {
    use ffmpeg::format::Sample;
    use ffmpeg::format::sample::Type;
    let channels = planes.len();
    let samples = planes[0].len();
    let int = |x: f32, scale: f32| (x.clamp(-1.0, 1.0) * scale).round();
    match format {
        Sample::I16(Type::Packed) => {
            let out = frame.data_mut(0);
            for i in 0..samples {
                for (c, plane) in planes.iter().enumerate() {
                    let v = int(plane[i], 32767.0) as i16;
                    let at = (i * channels + c) * 2;
                    out[at..at + 2].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        Sample::I32(Type::Packed) => {
            // pcm_s24le takes s32 samples and keeps the top 24 bits.
            let out = frame.data_mut(0);
            for i in 0..samples {
                for (c, plane) in planes.iter().enumerate() {
                    let v = (int(plane[i], 8_388_607.0) as i32) << 8;
                    let at = (i * channels + c) * 4;
                    out[at..at + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        Sample::F32(Type::Packed) => {
            let out = frame.data_mut(0);
            for i in 0..samples {
                for (c, plane) in planes.iter().enumerate() {
                    let at = (i * channels + c) * 4;
                    out[at..at + 4].copy_from_slice(&plane[i].to_le_bytes());
                }
            }
        }
        Sample::F32(Type::Planar) => {
            for (c, plane) in planes.iter().enumerate() {
                let out = frame.data_mut(c);
                for (i, v) in plane.iter().enumerate() {
                    out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        Sample::I16(Type::Planar) => {
            for (c, plane) in planes.iter().enumerate() {
                let out = frame.data_mut(c);
                for (i, v) in plane.iter().enumerate() {
                    let v = int(*v, 32767.0) as i16;
                    out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        _ => {
            tracing::warn!(
                ?format,
                "unsupported encoder sample format; writing silence"
            );
        }
    }
}

/// Converts CPU frames to another size and pixel format with swscale (area averaging
/// when shrinking, bicubic otherwise), keeping pts, duration and colour tags. YUV to YUV
/// conversion copies sample values (no matrix or range change), so tags stay valid.
pub struct FrameScaler {
    context: ffmpeg::software::scaling::Context,
    width: u32,
    height: u32,
    format: PixelFormat,
}

impl std::fmt::Debug for FrameScaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameScaler")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format.name())
            .finish()
    }
}

impl FrameScaler {
    pub fn new(from: (PixelFormat, u32, u32), to: (PixelFormat, u32, u32)) -> Result<Self> {
        let (sf, sw, sh) = from;
        let (df, dw, dh) = to;
        let flags = if dw < sw || dh < sh {
            ffmpeg::software::scaling::Flags::AREA
        } else {
            ffmpeg::software::scaling::Flags::BICUBIC
        } | ffmpeg::software::scaling::Flags::ACCURATE_RND;
        let context = ffmpeg::software::scaling::Context::get(
            sf.to_ffmpeg(),
            sw,
            sh,
            df.to_ffmpeg(),
            dw,
            dh,
            flags,
        )
        .map_err(|e| MediaError::Convert {
            format: sf.name(),
            width: sw,
            height: sh,
            to: df.name(),
            source: e.into(),
        })?;
        Ok(Self {
            context,
            width: dw,
            height: dh,
            format: df,
        })
    }

    /// The frame at the output size and format, with the input's timing and tags.
    pub fn scale(&mut self, frame: &VideoFrame) -> Result<VideoFrame> {
        let src = frame.raw();
        let mut out = ffmpeg::frame::Video::new(self.format.to_ffmpeg(), self.width, self.height);
        self.context
            .run(src, &mut out)
            .map_err(|e| MediaError::Convert {
                format: frame.format().name(),
                width: frame.width(),
                height: frame.height(),
                to: self.format.name(),
                source: e.into(),
            })?;
        out.set_pts(src.pts());
        out.set_color_primaries(src.color_primaries());
        out.set_color_transfer_characteristic(src.color_transfer_characteristic());
        out.set_color_space(src.color_space());
        out.set_color_range(src.color_range());
        ffi::set_frame_duration(&mut out, ffi::frame_duration(src));
        Ok(VideoFrame::from_cpu(out, frame.time_base()))
    }
}
