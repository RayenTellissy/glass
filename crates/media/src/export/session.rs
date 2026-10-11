//! The export session: settings checked, encoders opened, packets written to the file.

use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;

use crate::decode::VideoFrame;
use crate::demux::Packet;
use crate::encode::{
    AudioEncoderSettings, Encoder, FrameScaler, VideoEncoderSettings, encoder_available,
};
use crate::error::MediaError;
use crate::ffi;
use crate::format::{PixelFormat, SampleFormat, SampleType};
use crate::mux::Muxer;
use crate::time::Rational;

use super::gif_writer::GifWriter;
use super::picture::{Converter, Picture};
use super::settings::{
    AudioCodec, AudioSettings, Container, EncoderChoice, ExportSettings, RateControl, VideoCodec,
    VideoSettings, is_hardware_encoder, video_encoder_choices,
};
use super::{ExportError, Result};

/// Frames a hardware encoder may take without producing a packet before it counts as
/// working; until then they are kept to replay into a fallback encoder.
const TRIAL_FRAMES: usize = 8;

/// What a finished export wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSummary {
    /// Every file written: one, or one per frame for an image sequence.
    pub files: Vec<PathBuf>,
    /// Their total size in bytes.
    pub bytes: u64,
    pub frames: u64,
    /// Audio samples per channel.
    pub samples: u64,
    /// The FFmpeg encoders that wrote the streams (`libx264`, `h264_videotoolbox`,
    /// `prores_videotoolbox`; `gif` for GIF).
    pub video_encoder: Option<String>,
    pub audio_encoder: Option<String>,
    /// Video encoders that failed to open or to encode the first frames and were
    /// replaced by the next one, in the order tried.
    pub fallbacks: Vec<EncoderFallback>,
}

/// A video encoder the session gave up on in favour of the next one (a hardware encoder
/// replaced by x264 or x265).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderFallback {
    /// The FFmpeg encoder that failed (`h264_videotoolbox`).
    pub encoder: String,
    /// Why, as FFmpeg reported it.
    pub reason: String,
}

/// One export being written. See the [module docs](super).
///
/// `Send` but not `Sync`: it belongs to the export thread. Dropping it without
/// [`finish`](Self::finish) leaves an incomplete file.
pub struct ExportSession {
    path: PathBuf,
    sink: Sink,
    video: Option<VideoOut>,
    audio: Option<AudioOut>,
    fallbacks: Vec<EncoderFallback>,
}

impl std::fmt::Debug for ExportSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportSession")
            .field("path", &self.path)
            .field("video_encoder", &self.video_encoder())
            .field("audio_encoder", &self.audio_encoder())
            .field("fallbacks", &self.fallbacks)
            .finish()
    }
}

/// Where encoded packets go.
enum Sink {
    Muxer(Muxer),
    /// The muxer before its streams are added: a hardware video encoder is on trial and
    /// may still be replaced, so audio packets wait here.
    Waiting {
        muxer: Muxer,
        audio: Vec<Packet>,
    },
    Sequence(SequenceFiles),
    /// GIF: the writer is the video stream itself.
    None,
}

enum VideoKind {
    Encoder { encoder: Encoder, stream: usize },
    Gif(GifWriter),
}

struct VideoOut {
    kind: VideoKind,
    converter: Converter,
    time_base: Rational,
    next_pts: i64,
    frame_rate: Rational,
    /// HEVC in MP4 or MOV, tagged `hvc1`.
    hvc1: bool,
    /// Set while a hardware encoder has not produced a packet yet.
    trial: Option<Trial>,
}

/// What replacing a hardware encoder on trial takes.
struct Trial {
    settings: VideoSettings,
    global_header: bool,
    /// The encoders to try next, in order.
    next: Vec<EncoderChoice>,
    /// The frames sent so far, replayed into the replacement.
    sent: Vec<VideoFrame>,
}

struct AudioOut {
    encoder: Encoder,
    stream: usize,
    channels: usize,
    frame_size: Option<usize>,
    /// Samples waiting for a whole encoder frame, per channel.
    pending: Vec<Vec<f32>>,
    samples: u64,
}

/// The video encoders to try, or the GIF writer.
enum VideoPlan {
    Encoders(Vec<EncoderChoice>),
    Gif(PixelFormat),
}

impl ExportSession {
    /// Checks the settings, opens the encoders and creates the file (for an image
    /// sequence, `path` is a printf pattern such as `shot_%05d.png`, or a plain name that
    /// gets `_%05d` before its extension).
    ///
    /// A hardware video encoder that fails to open is replaced here by the next encoder
    /// the [`EncoderPreference`](super::EncoderPreference) allows; one that fails on the
    /// first frames is replaced while they are pushed, before anything of it reaches the
    /// file.
    pub fn create(path: impl AsRef<Path>, settings: &ExportSettings) -> Result<Self> {
        settings.validate()?;
        crate::init();
        let path = path.as_ref().to_path_buf();
        let container = settings.container;

        // Everything that can fail without touching the disk comes first.
        let video_plan = match &settings.video {
            Some(v) if v.codec == VideoCodec::Gif => {
                let rgba = PixelFormat::from_name("rgba").expect("FFmpeg knows rgba");
                Some(VideoPlan::Gif(rgba))
            }
            Some(v) => {
                let choices = video_encoder_choices(v)?;
                if let Some(c) = choices
                    .iter()
                    .find(|c| v.colour.is_hdr() && c.format.bit_depth() < 10)
                {
                    return Err(ExportError::Settings(format!(
                        "HDR needs 10 bits or more, {} has {}",
                        c.format,
                        c.format.bit_depth()
                    )));
                }
                Some(VideoPlan::Encoders(choices))
            }
            None => None,
        };
        if let Some(a) = &settings.audio {
            let name = a.codec.encoder();
            if !encoder_available(name) {
                return Err(ExportError::Unavailable {
                    codec: a.codec.name(),
                    tried: vec![name],
                });
            }
        }

        let sink = match container {
            Container::ImageSequence => {
                let codec = settings.video.as_ref().map(|v| v.codec);
                let extension = if codec == Some(VideoCodec::Tiff) {
                    "tif"
                } else {
                    "png"
                };
                Sink::Sequence(SequenceFiles::new(&path, extension, settings.first_number)?)
            }
            Container::Gif => Sink::None,
            _ => {
                let mut muxer = Muxer::create(&path, container.muxer())?;
                if settings.fast_start
                    && matches!(container, Container::Mp4 | Container::Mov | Container::M4a)
                {
                    muxer.set_option("movflags", "+faststart");
                }
                Sink::Waiting {
                    muxer,
                    audio: Vec::new(),
                }
            }
        };
        let global_header =
            matches!(&sink, Sink::Waiting { muxer, .. } if muxer.wants_global_header());
        let mut session = Self {
            path,
            sink,
            video: None,
            audio: None,
            fallbacks: Vec::new(),
        };
        match session.open_streams(settings, video_plan, global_header) {
            Ok(()) => Ok(session),
            Err(error) => {
                // An encoder that would not open leaves no empty file behind.
                let file = !matches!(session.sink, Sink::Sequence(_));
                let path = std::mem::take(&mut session.path);
                drop(session);
                if file {
                    let _ = std::fs::remove_file(&path);
                }
                Err(error)
            }
        }
    }

    /// The file (or image sequence pattern) being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The video encoder in use (`gif` for GIF). A hardware encoder may still be replaced
    /// while the first frames are pushed; [`ExportSummary::video_encoder`] names the one
    /// that wrote the file.
    pub fn video_encoder(&self) -> Option<&str> {
        self.video.as_ref().map(|v| match &v.kind {
            VideoKind::Encoder { encoder, .. } => encoder.name(),
            VideoKind::Gif(_) => "gif",
        })
    }

    pub fn audio_encoder(&self) -> Option<&str> {
        self.audio.as_ref().map(|a| a.encoder.name())
    }

    /// Video encoders given up on so far, in the order tried.
    pub fn fallbacks(&self) -> &[EncoderFallback] {
        &self.fallbacks
    }

    /// The pixel format pictures are converted to for the encoder.
    pub fn pixel_format(&self) -> Option<PixelFormat> {
        self.video.as_ref().map(|v| v.converter.format())
    }

    /// Frames sent so far.
    pub fn frames(&self) -> u64 {
        self.video.as_ref().map_or(0, |v| v.next_pts as u64)
    }

    /// Encodes the next frame from a picture in caller memory.
    pub fn push_picture(&mut self, picture: &Picture<'_>) -> Result<()> {
        let Some(video) = self.video.as_mut() else {
            return Err(ExportError::Input("this export has no video stream".into()));
        };
        let mut frame = video.converter.convert(picture)?;
        frame.set_pts(Some(video.next_pts));
        ffi::set_frame_duration(&mut frame, 1);
        let packets = match &mut video.kind {
            VideoKind::Gif(gif) => {
                gif.write(&frame)?;
                video.next_pts += 1;
                return Ok(());
            }
            VideoKind::Encoder { .. } => video.send(frame, &mut self.fallbacks)?,
        };
        video.next_pts += 1;
        self.write_video(packets)
    }

    /// Encodes the next frame from a decoded CPU frame (its own timing is ignored).
    pub fn push_frame(&mut self, frame: &VideoFrame) -> Result<()> {
        let Some(picture) = Picture::from_frame(frame) else {
            return Err(ExportError::Input(
                "hardware frames must be downloaded before export".into(),
            ));
        };
        self.push_picture(&picture)
    }

    /// Encodes planar f32 audio, one slice per channel, all the same length, following
    /// the previous call.
    pub fn push_audio(&mut self, planes: &[&[f32]]) -> Result<()> {
        let Some(audio) = self.audio.as_mut() else {
            return Err(ExportError::Input("this export has no audio stream".into()));
        };
        audio.push(planes, &mut self.sink)
    }

    /// Drains the encoders and closes the file.
    pub fn finish(mut self) -> Result<ExportSummary> {
        let mut samples = 0;
        if let Some(audio) = self.audio.as_mut() {
            audio.flush(&mut self.sink)?;
            samples = audio.samples;
        }
        let mut frames = 0;
        if let Some(video) = self.video.as_mut() {
            frames = video.next_pts as u64;
            let packets = match &mut video.kind {
                VideoKind::Encoder { .. } => video.drain_at_end(&mut self.fallbacks)?,
                VideoKind::Gif(_) => Vec::new(),
            };
            self.write_video(packets)?;
        }
        self.commit()?;
        let video_encoder = self.video_encoder().map(str::to_string);
        let audio_encoder = self.audio_encoder().map(str::to_string);
        if let Some(VideoOut {
            kind: VideoKind::Gif(gif),
            ..
        }) = self.video.take()
        {
            gif.finish()?;
        }
        let files = match self.sink {
            Sink::Muxer(muxer) => {
                muxer.finish()?;
                vec![self.path]
            }
            Sink::Sequence(sequence) => sequence.written,
            Sink::Waiting { .. } | Sink::None => vec![self.path],
        };
        let bytes = files
            .iter()
            .map(|f| std::fs::metadata(f).map_or(0, |m| m.len()))
            .sum();
        Ok(ExportSummary {
            files,
            bytes,
            frames,
            samples,
            video_encoder,
            audio_encoder,
            fallbacks: self.fallbacks,
        })
    }

    /// Opens the encoders; the streams are added and the header written now, or with the
    /// first packet of a hardware video encoder on trial.
    fn open_streams(
        &mut self,
        settings: &ExportSettings,
        video_plan: Option<VideoPlan>,
        global_header: bool,
    ) -> Result<()> {
        if let (Some(v), Some(plan)) = (&settings.video, video_plan) {
            let video = open_video(
                v,
                plan,
                settings.container,
                global_header,
                &self.path,
                &mut self.fallbacks,
            )?;
            self.video = Some(video);
        }
        if let Some(a) = &settings.audio {
            if matches!(self.sink, Sink::Sequence(_) | Sink::None) {
                return Err(ExportError::Settings("this container has no audio".into()));
            }
            self.audio = Some(open_audio(a, global_header)?);
        }
        if self.video.as_ref().is_none_or(|v| v.trial.is_none()) {
            self.commit()?;
        }
        Ok(())
    }

    /// Adds the streams, writes the header and the audio packets that waited for it.
    fn commit(&mut self) -> Result<()> {
        if !matches!(self.sink, Sink::Waiting { .. }) {
            return Ok(());
        }
        let Sink::Waiting { mut muxer, audio } = std::mem::replace(&mut self.sink, Sink::None)
        else {
            unreachable!("checked above");
        };
        if let Some(video) = self.video.as_mut()
            && let VideoKind::Encoder { encoder, stream } = &mut video.kind
        {
            *stream = muxer.add_stream(encoder, Some(video.frame_rate))?;
            if video.hvc1 {
                muxer.set_codec_tag(*stream, *b"hvc1")?;
            }
        }
        if let Some(out) = self.audio.as_mut() {
            out.stream = muxer.add_stream(&out.encoder, None)?;
        }
        muxer.write_header()?;
        if let Some(out) = &self.audio {
            for packet in audio {
                muxer.write_packet(out.stream, packet)?;
            }
        }
        self.sink = Sink::Muxer(muxer);
        Ok(())
    }

    /// Writes video packets, adding the streams first once no encoder is on trial.
    fn write_video(&mut self, packets: Vec<Packet>) -> Result<()> {
        let Some(video) = &self.video else {
            return Ok(());
        };
        if video.trial.is_some() {
            debug_assert!(packets.is_empty(), "a trial ends with its first packet");
            return Ok(());
        }
        let VideoKind::Encoder { .. } = video.kind else {
            return Ok(());
        };
        self.commit()?;
        let Some(VideoOut {
            kind: VideoKind::Encoder { stream, .. },
            ..
        }) = &self.video
        else {
            return Ok(());
        };
        for packet in packets {
            write_packet(&mut self.sink, *stream, packet)?;
        }
        Ok(())
    }
}

fn open_video(
    v: &VideoSettings,
    plan: VideoPlan,
    container: Container,
    global_header: bool,
    path: &Path,
    fallbacks: &mut Vec<EncoderFallback>,
) -> Result<VideoOut> {
    let time_base = v.time_base();
    let hvc1 = v.codec == VideoCodec::Hevc && matches!(container, Container::Mp4 | Container::Mov);
    let mut choices = match plan {
        VideoPlan::Gif(format) => {
            let converter = Converter::new(format, v.width, v.height, v.colour.info(true));
            let gif = GifWriter::create(path, v.width, v.height, v.frame_rate)?;
            return Ok(VideoOut {
                kind: VideoKind::Gif(gif),
                converter,
                time_base,
                next_pts: 0,
                frame_rate: v.frame_rate,
                hvc1,
                trial: None,
            });
        }
        VideoPlan::Encoders(choices) => choices,
    };
    loop {
        let choice = choices.remove(0);
        match open_encoder(v, &choice, global_header) {
            Ok(encoder) => {
                let colour = v.colour.info(choice.format.is_rgb());
                let trial =
                    (is_hardware_encoder(&choice.name) && !choices.is_empty()).then(|| Trial {
                        settings: v.clone(),
                        global_header,
                        next: choices,
                        sent: Vec::new(),
                    });
                return Ok(VideoOut {
                    kind: VideoKind::Encoder { encoder, stream: 0 },
                    converter: Converter::new(choice.format, v.width, v.height, colour),
                    time_base,
                    next_pts: 0,
                    frame_rate: v.frame_rate,
                    hvc1,
                    trial,
                });
            }
            Err(error) if !choices.is_empty() && is_hardware_encoder(&choice.name) => {
                fallbacks.push(EncoderFallback {
                    encoder: choice.name,
                    reason: error.to_string(),
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Opens one video encoder of the settings.
fn open_encoder(
    v: &VideoSettings,
    choice: &EncoderChoice,
    global_header: bool,
) -> Result<Encoder, MediaError> {
    let colour = v.colour.info(choice.format.is_rgb());
    let mut s = VideoEncoderSettings::new(
        choice.name.clone(),
        v.width,
        v.height,
        choice.format,
        v.time_base(),
    );
    s.frame_rate = Some(v.frame_rate);
    s.colour = colour;
    s.gop = v.gop;
    s.max_b_frames = v.max_b_frames;
    if let RateControl::Bitrate { average, .. } = v.rate {
        s.bit_rate = Some(average);
    }
    s.threads = v.threads;
    s.global_header = global_header;
    s.options = v.encoder_options(&choice.name);
    Encoder::video(&s)
}

fn open_audio(a: &AudioSettings, global_header: bool) -> Result<AudioOut> {
    let mut s = AudioEncoderSettings::new(a.codec.encoder(), a.sample_rate, a.channels);
    s.global_header = global_header;
    match a.codec {
        AudioCodec::Aac | AudioCodec::Mp3 => {
            s.bit_rate = Some(a.bit_rate.unwrap_or(128_000 * a.channels as u64));
        }
        AudioCodec::Flac { bits } => {
            s.sample_format = Some(SampleFormat {
                sample: if bits > 16 {
                    SampleType::I32
                } else {
                    SampleType::I16
                },
                planar: false,
            });
        }
        AudioCodec::Pcm(_) => {}
    }
    let encoder = Encoder::audio(&s)?;
    let frame_size = encoder.frame_size();
    Ok(AudioOut {
        encoder,
        stream: 0,
        channels: a.channels,
        frame_size,
        pending: vec![Vec::new(); a.channels],
        samples: 0,
    })
}

impl VideoOut {
    fn encoder(&mut self) -> &mut Encoder {
        match &mut self.kind {
            VideoKind::Encoder { encoder, .. } => encoder,
            VideoKind::Gif(_) => unreachable!("GIF frames go to the writer"),
        }
    }

    /// Sends a timed frame to the encoder and returns the packets it has ready. An
    /// encoder on trial that fails is replaced, the kept frames replayed into the next.
    fn send(
        &mut self,
        frame: ffmpeg::frame::Video,
        fallbacks: &mut Vec<EncoderFallback>,
    ) -> Result<Vec<Packet>> {
        let frame = VideoFrame::from_cpu(frame, self.time_base);
        let mut packets = Vec::new();
        let encoder = self.encoder();
        let result = encoder
            .send_video(&frame)
            .and_then(|()| receive_all(encoder, &mut packets));
        match (result, self.trial.as_mut()) {
            (Ok(()), None) => {}
            (Ok(()), Some(trial)) => {
                if packets.is_empty() && trial.sent.len() < TRIAL_FRAMES {
                    trial.sent.push(frame);
                } else {
                    self.trial = None;
                }
            }
            (Err(error), Some(_)) => {
                packets.clear();
                self.fall_back(error, Some(frame), false, &mut packets, fallbacks)?;
            }
            (Err(error), None) => return Err(error.into()),
        }
        Ok(packets)
    }

    /// Drains the encoder at the end of the export.
    fn drain_at_end(&mut self, fallbacks: &mut Vec<EncoderFallback>) -> Result<Vec<Packet>> {
        let mut packets = Vec::new();
        let encoder = self.encoder();
        let result = encoder
            .send_eof()
            .and_then(|()| receive_all(encoder, &mut packets));
        match (result, self.trial.is_some()) {
            (Ok(()), _) => {}
            (Err(error), true) => {
                packets.clear();
                self.fall_back(error, None, true, &mut packets, fallbacks)?;
            }
            (Err(error), false) => return Err(error.into()),
        }
        self.trial = None;
        Ok(packets)
    }

    /// Replaces the encoder on trial with the next one that opens and takes the frames
    /// sent so far (and `current`, then the end of the stream when `eof`), collecting its
    /// packets.
    fn fall_back(
        &mut self,
        error: MediaError,
        current: Option<VideoFrame>,
        eof: bool,
        packets: &mut Vec<Packet>,
        fallbacks: &mut Vec<EncoderFallback>,
    ) -> Result<()> {
        let Some(mut trial) = self.trial.take() else {
            return Err(error.into());
        };
        let mut sent = std::mem::take(&mut trial.sent);
        sent.extend(current);
        let mut failed = self.encoder().name().to_string();
        let mut error = error;
        loop {
            fallbacks.push(EncoderFallback {
                encoder: failed,
                reason: error.to_string(),
            });
            if trial.next.is_empty() {
                return Err(error.into());
            }
            let choice = trial.next.remove(0);
            sent = convert_frames(sent, choice.format, &trial.settings)?;
            match replay(&trial, &choice, &sent, eof, packets) {
                Ok(encoder) => {
                    if choice.format != self.converter.format() {
                        let colour = trial.settings.colour.info(choice.format.is_rgb());
                        let (w, h) = (trial.settings.width, trial.settings.height);
                        self.converter = Converter::new(choice.format, w, h, colour);
                    }
                    let more = is_hardware_encoder(&choice.name) && !trial.next.is_empty();
                    self.kind = VideoKind::Encoder { encoder, stream: 0 };
                    if more && !eof && packets.is_empty() && sent.len() <= TRIAL_FRAMES {
                        trial.sent = sent;
                        self.trial = Some(trial);
                    }
                    return Ok(());
                }
                Err(e) => {
                    packets.clear();
                    failed = choice.name;
                    error = e;
                }
            }
        }
    }
}

/// The frames in `format` (moved as they are when they already are).
fn convert_frames(
    frames: Vec<VideoFrame>,
    format: PixelFormat,
    v: &VideoSettings,
) -> Result<Vec<VideoFrame>> {
    let Some(first) = frames.first() else {
        return Ok(frames);
    };
    if first.format() == format {
        return Ok(frames);
    }
    let (w, h) = (v.width, v.height);
    let mut scaler = FrameScaler::new((first.format(), w, h), (format, w, h))?;
    let converted: Result<Vec<_>, MediaError> = frames.iter().map(|f| scaler.scale(f)).collect();
    Ok(converted?)
}

/// Opens `choice` and sends it `sent`, then the end of the stream when `eof`.
fn replay(
    trial: &Trial,
    choice: &EncoderChoice,
    sent: &[VideoFrame],
    eof: bool,
    packets: &mut Vec<Packet>,
) -> Result<Encoder, MediaError> {
    let mut encoder = open_encoder(&trial.settings, choice, trial.global_header)?;
    for frame in sent {
        encoder.send_video(frame)?;
        receive_all(&mut encoder, packets)?;
    }
    if eof {
        encoder.send_eof()?;
        receive_all(&mut encoder, packets)?;
    }
    Ok(encoder)
}

impl AudioOut {
    fn push(&mut self, planes: &[&[f32]], sink: &mut Sink) -> Result<()> {
        if planes.len() != self.channels {
            return Err(ExportError::Input(format!(
                "{} audio channels for a {}-channel export",
                planes.len(),
                self.channels
            )));
        }
        let len = planes[0].len();
        if planes.iter().any(|p| p.len() != len) {
            return Err(ExportError::Input(
                "audio channels of different lengths".into(),
            ));
        }
        let Some(size) = self.frame_size else {
            self.encoder.send_audio(planes)?;
            self.samples += len as u64;
            return drain(&mut self.encoder, sink, self.stream);
        };
        for (pending, plane) in self.pending.iter_mut().zip(planes) {
            pending.extend_from_slice(plane);
        }
        let available = self.pending[0].len();
        let mut at = 0;
        while available - at >= size {
            let chunk: Vec<&[f32]> = self.pending.iter().map(|p| &p[at..at + size]).collect();
            self.encoder.send_audio(&chunk)?;
            drain(&mut self.encoder, sink, self.stream)?;
            at += size;
        }
        for pending in &mut self.pending {
            pending.drain(..at);
        }
        self.samples += len as u64;
        Ok(())
    }

    /// Sends the samples short of a whole frame, then drains the encoder.
    fn flush(&mut self, sink: &mut Sink) -> Result<()> {
        if self.pending.first().is_some_and(|p| !p.is_empty()) {
            let small_last = self.encoder.accepts_small_last_frame();
            if !small_last && let Some(size) = self.frame_size {
                for pending in &mut self.pending {
                    pending.resize(size, 0.0);
                }
            }
            let chunk: Vec<&[f32]> = self.pending.iter().map(Vec::as_slice).collect();
            self.encoder.send_audio(&chunk)?;
            for pending in &mut self.pending {
                pending.clear();
            }
        }
        self.encoder.send_eof()?;
        drain(&mut self.encoder, sink, self.stream)
    }
}

/// Moves every packet the audio encoder has ready into the sink (or the waiting list
/// before the streams are added).
fn drain(encoder: &mut Encoder, sink: &mut Sink, stream: usize) -> Result<()> {
    while let Some(packet) = encoder.receive_packet()? {
        match sink {
            Sink::Waiting { audio, .. } => audio.push(packet),
            _ => write_packet(sink, stream, packet)?,
        }
    }
    Ok(())
}

/// Collects every packet the encoder has ready.
fn receive_all(encoder: &mut Encoder, packets: &mut Vec<Packet>) -> Result<(), MediaError> {
    while let Some(packet) = encoder.receive_packet()? {
        packets.push(packet);
    }
    Ok(())
}

fn write_packet(sink: &mut Sink, stream: usize, packet: Packet) -> Result<()> {
    match sink {
        Sink::Muxer(muxer) => muxer.write_packet(stream, packet)?,
        Sink::Sequence(sequence) => sequence.write(packet.data())?,
        Sink::Waiting { .. } => {
            unreachable!("video packets are written after the streams are added")
        }
        Sink::None => {}
    }
    Ok(())
}

/// Numbered picture files from a printf pattern (`dir/shot_%05d.png`).
struct SequenceFiles {
    dir: PathBuf,
    prefix: String,
    digits: usize,
    suffix: String,
    next: u64,
    written: Vec<PathBuf>,
}

impl SequenceFiles {
    fn new(path: &Path, extension: &str, first: u64) -> Result<Self> {
        let invalid = || ExportError::Settings(format!("{} is not a file pattern", path.display()));
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(invalid)?;
        let dir = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let (prefix, digits, suffix) = match name.find('%') {
            Some(at) => {
                let (prefix, rest) = name.split_at(at);
                let rest = &rest[1..];
                let d = rest.find('d').ok_or_else(invalid)?;
                let spec = &rest[..d];
                let digits = match spec {
                    "" => 0,
                    s if s.starts_with('0') => s[1..].parse::<usize>().map_err(|_| invalid())?,
                    _ => return Err(invalid()),
                };
                (prefix.to_string(), digits, rest[d + 1..].to_string())
            }
            None => {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(invalid)?;
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or(extension);
                (format!("{stem}_"), 5, format!(".{ext}"))
            }
        };
        if !dir.as_os_str().is_empty() && !dir.is_dir() {
            return Err(ExportError::Write {
                path: dir,
                message: "the folder does not exist".into(),
            });
        }
        Ok(Self {
            dir,
            prefix,
            digits,
            suffix,
            next: first,
            written: Vec::new(),
        })
    }

    fn write(&mut self, data: &[u8]) -> Result<()> {
        let name = format!(
            "{}{:0width$}{}",
            self.prefix,
            self.next,
            self.suffix,
            width = self.digits
        );
        let path = self.dir.join(name);
        std::fs::write(&path, data).map_err(|e| ExportError::Write {
            path: path.clone(),
            message: e.to_string(),
        })?;
        self.written.push(path);
        self.next += 1;
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::export::{ColourTags, ExportSettings, VideoSettings};

    /// A VideoToolbox encoder that fails after taking frames without producing a packet
    /// is replaced by x265: the kept frames are converted from P010 and replayed, and the
    /// file holds every frame from the software encoder only.
    #[test]
    fn a_hardware_encoder_failing_on_the_first_frames_is_replaced() {
        let dir = std::env::temp_dir().join(format!("glass-export-trial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch folder");
        let path = dir.join("out.mkv");
        let (w, h) = (128u32, 72u32);
        let mut video = VideoSettings::new(VideoCodec::Hevc, w, h, Rational::new(30, 1));
        video.colour = ColourTags::Bt2020Pq;
        let settings = ExportSettings::new(Container::Matroska).with_video(video);
        let mut session = ExportSession::create(&path, &settings).expect("open");
        assert_eq!(session.video_encoder(), Some("hevc_videotoolbox"));
        assert_eq!(session.pixel_format().map(|f| f.name()), Some("p010le"));
        let data: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let picture = Picture::rgba8(w, h, &data, w as usize * 4);

        // Two frames the encoder took without a packet.
        let video = session.video.as_mut().expect("video");
        for _ in 0..2 {
            let mut frame = video.converter.convert(&picture).expect("convert");
            frame.set_pts(Some(video.next_pts));
            ffi::set_frame_duration(&mut frame, 1);
            let frame = VideoFrame::from_cpu(frame, video.time_base);
            video.trial.as_mut().expect("on trial").sent.push(frame);
            video.next_pts += 1;
        }
        let error = MediaError::Encode {
            encoder: "hevc_videotoolbox".into(),
            action: "sending a frame",
            source: ffmpeg::Error::External.into(),
        };
        let mut packets = Vec::new();
        video
            .fall_back(error, None, false, &mut packets, &mut session.fallbacks)
            .expect("fallback opens");
        session.write_video(packets).expect("write");
        assert_eq!(session.video_encoder(), Some("libx265"));
        assert_eq!(
            session.pixel_format().map(|f| f.name()),
            Some("yuv420p10le")
        );
        for _ in 0..3 {
            session.push_picture(&picture).expect("picture");
        }
        let summary = session.finish().expect("finish");
        assert_eq!(summary.frames, 5);
        assert_eq!(summary.video_encoder.as_deref(), Some("libx265"));
        assert_eq!(summary.fallbacks.len(), 1);
        assert_eq!(summary.fallbacks[0].encoder, "hevc_videotoolbox");

        let info = crate::probe(&path).expect("probe");
        let (stream, v) = info.video().expect("video stream");
        assert_eq!(stream.codec.name, "hevc");
        assert_eq!(v.bit_depth, 10);
        let mut reader =
            crate::FrameReader::open(&path, crate::StreamSelect::BestVideo).expect("reader");
        let mut decoded = 0;
        while let Some(frame) = reader.next_frame().expect("decode") {
            decoded += usize::from(matches!(frame, crate::Frame::Video(_)));
        }
        assert_eq!(decoded, 5);
        let _ = std::fs::remove_dir_all(dir);
    }
}
