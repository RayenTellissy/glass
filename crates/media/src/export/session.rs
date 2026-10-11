//! The export session: settings checked, encoders opened, packets written to the file.

use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;

use crate::decode::VideoFrame;
use crate::encode::{AudioEncoderSettings, Encoder, VideoEncoderSettings, encoder_available};
use crate::ffi;
use crate::format::{PixelFormat, SampleFormat, SampleType};
use crate::mux::Muxer;
use crate::time::Rational;

use super::gif_writer::GifWriter;
use super::picture::{Converter, Picture};
use super::settings::{
    AudioCodec, AudioSettings, Container, ExportSettings, RateControl, VideoCodec, VideoSettings,
    video_encoder_for,
};
use super::{ExportError, Result};

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
    /// The FFmpeg encoders used (`libx264`, `prores_videotoolbox`; `gif` for GIF).
    pub video_encoder: Option<String>,
    pub audio_encoder: Option<String>,
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
}

impl std::fmt::Debug for ExportSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportSession")
            .field("path", &self.path)
            .field("video_encoder", &self.video_encoder())
            .field("audio_encoder", &self.audio_encoder())
            .finish()
    }
}

/// Where encoded packets go.
enum Sink {
    Muxer(Muxer),
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

impl ExportSession {
    /// Checks the settings, opens the encoders and creates the file (for an image
    /// sequence, `path` is a printf pattern such as `shot_%05d.png`, or a plain name that
    /// gets `_%05d` before its extension).
    pub fn create(path: impl AsRef<Path>, settings: &ExportSettings) -> Result<Self> {
        settings.validate()?;
        crate::init();
        let path = path.as_ref().to_path_buf();
        let container = settings.container;

        // Everything that can fail without touching the disk comes first.
        let video_plan = match &settings.video {
            Some(v) if v.codec == VideoCodec::Gif => {
                let rgba = PixelFormat::from_name("rgba").expect("FFmpeg knows rgba");
                Some((None, rgba))
            }
            Some(v) => {
                let encoder = video_encoder_for(v)?;
                let format = v.pixel_format_for(&encoder)?;
                if v.colour.is_hdr() && format.bit_depth() < 10 {
                    return Err(ExportError::Settings(format!(
                        "HDR needs 10 bits or more, {format} has {}",
                        format.bit_depth()
                    )));
                }
                Some((Some(encoder), format))
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

        let mut sink = match container {
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
            _ => Sink::Muxer(Muxer::create(&path, container.muxer())?),
        };
        let global_header = matches!(&sink, Sink::Muxer(m) if m.wants_global_header());

        let opened = open_streams(settings, video_plan, global_header, &path, &mut sink);
        match opened {
            Ok((video, audio)) => Ok(Self {
                path,
                sink,
                video,
                audio,
            }),
            Err(error) => {
                // An encoder that would not open leaves no empty file behind.
                let file = !matches!(sink, Sink::Sequence(_));
                drop(sink);
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

    /// The video encoder in use (`gif` for GIF).
    pub fn video_encoder(&self) -> Option<&str> {
        self.video.as_ref().map(|v| match &v.kind {
            VideoKind::Encoder { encoder, .. } => encoder.name(),
            VideoKind::Gif(_) => "gif",
        })
    }

    pub fn audio_encoder(&self) -> Option<&str> {
        self.audio.as_ref().map(|a| a.encoder.name())
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
        let frame = video.converter.convert(picture)?;
        video.send(frame, &mut self.sink)
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
        let video_encoder = self.video_encoder().map(str::to_string);
        let audio_encoder = self.audio_encoder().map(str::to_string);
        let mut samples = 0;
        if let Some(mut audio) = self.audio.take() {
            audio.flush(&mut self.sink)?;
            samples = audio.samples;
        }
        let mut frames = 0;
        if let Some(video) = self.video.take() {
            frames = video.next_pts as u64;
            match video.kind {
                VideoKind::Encoder {
                    mut encoder,
                    stream,
                } => {
                    encoder.send_eof()?;
                    drain(&mut encoder, &mut self.sink, stream)?;
                }
                VideoKind::Gif(gif) => gif.finish()?,
            }
        }
        let files = match self.sink {
            Sink::Muxer(muxer) => {
                muxer.finish()?;
                vec![self.path]
            }
            Sink::Sequence(sequence) => sequence.written,
            Sink::None => vec![self.path],
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
        })
    }
}

/// Opens the encoders, adds their streams and writes the container header.
fn open_streams(
    settings: &ExportSettings,
    video_plan: Option<(Option<String>, PixelFormat)>,
    global_header: bool,
    path: &Path,
    sink: &mut Sink,
) -> Result<(Option<VideoOut>, Option<AudioOut>)> {
    let container = settings.container;
    let video = match (&settings.video, video_plan) {
        (Some(v), Some((encoder, format))) => Some(open_video(
            v,
            encoder,
            format,
            container,
            global_header,
            path,
            sink,
        )?),
        _ => None,
    };
    let audio = match &settings.audio {
        Some(a) => Some(open_audio(a, global_header, sink)?),
        None => None,
    };
    if let Sink::Muxer(muxer) = sink {
        if settings.fast_start
            && matches!(container, Container::Mp4 | Container::Mov | Container::M4a)
        {
            muxer.set_option("movflags", "+faststart");
        }
        muxer.write_header()?;
    }
    Ok((video, audio))
}

fn open_video(
    v: &VideoSettings,
    encoder: Option<String>,
    format: PixelFormat,
    container: Container,
    global_header: bool,
    path: &Path,
    sink: &mut Sink,
) -> Result<VideoOut> {
    let colour = v.colour.info(format.is_rgb());
    let converter = Converter::new(format, v.width, v.height, colour);
    let time_base = v.time_base();
    let Some(name) = encoder else {
        let gif = GifWriter::create(path, v.width, v.height, v.frame_rate)?;
        return Ok(VideoOut {
            kind: VideoKind::Gif(gif),
            converter,
            time_base,
            next_pts: 0,
        });
    };
    let mut s = VideoEncoderSettings::new(name.clone(), v.width, v.height, format, time_base);
    s.frame_rate = Some(v.frame_rate);
    s.colour = colour;
    s.gop = v.gop;
    s.max_b_frames = v.max_b_frames;
    if let RateControl::Bitrate { average, .. } = v.rate {
        s.bit_rate = Some(average);
    }
    s.threads = v.threads;
    s.global_header = global_header;
    s.options = v.encoder_options(&name);
    let encoder = Encoder::video(&s)?;
    let stream = match sink {
        Sink::Muxer(muxer) => {
            let stream = muxer.add_stream(&encoder, Some(v.frame_rate))?;
            if v.codec == VideoCodec::Hevc && matches!(container, Container::Mp4 | Container::Mov) {
                muxer.set_codec_tag(stream, *b"hvc1")?;
            }
            stream
        }
        Sink::Sequence(_) | Sink::None => 0,
    };
    Ok(VideoOut {
        kind: VideoKind::Encoder { encoder, stream },
        converter,
        time_base,
        next_pts: 0,
    })
}

fn open_audio(a: &AudioSettings, global_header: bool, sink: &mut Sink) -> Result<AudioOut> {
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
    let stream = match sink {
        Sink::Muxer(muxer) => muxer.add_stream(&encoder, None)?,
        Sink::Sequence(_) | Sink::None => {
            return Err(ExportError::Settings("this container has no audio".into()));
        }
    };
    let frame_size = encoder.frame_size();
    Ok(AudioOut {
        encoder,
        stream,
        channels: a.channels,
        frame_size,
        pending: vec![Vec::new(); a.channels],
        samples: 0,
    })
}

impl VideoOut {
    fn send(&mut self, mut frame: ffmpeg::frame::Video, sink: &mut Sink) -> Result<()> {
        frame.set_pts(Some(self.next_pts));
        ffi::set_frame_duration(&mut frame, 1);
        match &mut self.kind {
            VideoKind::Encoder { encoder, stream } => {
                encoder.send_video(&VideoFrame::from_cpu(frame, self.time_base))?;
                drain(encoder, sink, *stream)?;
            }
            VideoKind::Gif(gif) => gif.write(&frame)?,
        }
        self.next_pts += 1;
        Ok(())
    }
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

/// Moves every packet the encoder has ready into the sink.
fn drain(encoder: &mut Encoder, sink: &mut Sink, stream: usize) -> Result<()> {
    while let Some(packet) = encoder.receive_packet()? {
        match sink {
            Sink::Muxer(muxer) => muxer.write_packet(stream, packet)?,
            Sink::Sequence(sequence) => sequence.write(packet.data())?,
            Sink::None => {}
        }
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
