//! Muxing: encoded or copied packets into a container file.
//!
//! Streams come from an [`Encoder`] ([`Muxer::add_stream`]) or are copied from an input
//! stream ([`Muxer::add_copy_stream`], stream copy for concatenation and, in Phase 8,
//! smart export). Stream metadata (timecode), display matrices and container options are
//! set before [`Muxer::write_header`]; packets are rescaled from their own time base to
//! the stream's and interleaved by FFmpeg. Text subtitle streams come from the export
//! session's subtitle encoders. Phase 8 adds more stream kinds (chapters) and muxer
//! options through the same calls.

use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;

use crate::demux::{Demuxer, Packet};
use crate::encode::Encoder;
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::subtitle::SubtitleEncoder;
use crate::time::Rational;

/// An output file being written. Dropping it without [`finish`](Self::finish) leaves an
/// incomplete file.
pub struct Muxer {
    output: ffmpeg::format::context::Output,
    path: PathBuf,
    header_written: bool,
    /// Per stream: its time base once the header is written.
    time_bases: Vec<Rational>,
    options: Vec<(String, String)>,
}

impl std::fmt::Debug for Muxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Muxer")
            .field("path", &self.path)
            .field("streams", &self.time_bases.len())
            .finish()
    }
}

impl Muxer {
    /// Creates `path`; the container comes from `format` (FFmpeg's muxer name: `mov`,
    /// `mp4`, `matroska`) or else from the extension.
    pub fn create(path: impl AsRef<Path>, format: Option<&str>) -> Result<Self> {
        crate::init();
        let path = path.as_ref().to_path_buf();
        let output = match format {
            Some(format) => ffmpeg::format::output_as(&path, format),
            None => ffmpeg::format::output(&path),
        }
        .map_err(|e| MediaError::Write {
            path: path.clone(),
            action: "creating the file",
            source: e.into(),
        })?;
        Ok(Self {
            output,
            path,
            header_written: false,
            time_bases: Vec::new(),
            options: Vec::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether encoders feeding this container must put codec headers in extradata
    /// ([`VideoEncoderSettings::global_header`](crate::VideoEncoderSettings::global_header)).
    pub fn wants_global_header(&self) -> bool {
        self.output
            .format()
            .flags()
            .contains(ffmpeg::format::Flags::GLOBAL_HEADER)
    }

    fn error(&self, action: &'static str) -> impl Fn(ffmpeg::Error) -> MediaError + '_ {
        move |e| MediaError::Write {
            path: self.path.clone(),
            action,
            source: e.into(),
        }
    }

    /// Adds a stream carrying `encoder`'s packets; returns its index. `frame_rate`
    /// (video) is stored as the stream's nominal rate, which timecode tracks need.
    pub fn add_stream(&mut self, encoder: &Encoder, frame_rate: Option<Rational>) -> Result<usize> {
        let codec = encoder.codec();
        let mut stream = self
            .output
            .add_stream(codec)
            .map_err(|e| MediaError::Write {
                path: self.path.clone(),
                action: "adding a stream",
                source: e.into(),
            })?;
        stream.set_parameters(encoder.parameters());
        stream.set_time_base(ffmpeg::Rational::from(encoder.time_base()));
        if let Some(rate) = frame_rate {
            stream.set_avg_frame_rate(ffmpeg::Rational::from(rate));
            stream.set_rate(ffmpeg::Rational::from(rate));
        }
        let index = stream.index();
        self.time_bases.push(encoder.time_base());
        Ok(index)
    }

    /// Adds a text subtitle stream carrying `encoder`'s packets; returns its index.
    pub(crate) fn add_subtitle_stream(&mut self, encoder: &SubtitleEncoder) -> Result<usize> {
        let mut stream =
            self.output
                .add_stream(encoder.codec())
                .map_err(|e| MediaError::Write {
                    path: self.path.clone(),
                    action: "adding a stream",
                    source: e.into(),
                })?;
        stream.set_parameters(encoder.parameters());
        stream.set_time_base(ffmpeg::Rational::from(encoder.time_base()));
        let index = stream.index();
        self.time_bases.push(encoder.time_base());
        Ok(index)
    }

    /// Adds a stream that copies `stream` of `input` packet for packet; returns its index.
    pub fn add_copy_stream(&mut self, input: &Demuxer, stream: usize) -> Result<usize> {
        let summary = input.stream(stream)?.clone();
        let source = input
            .input()
            .stream(stream)
            .ok_or_else(|| MediaError::NoSuchStream {
                path: input.path().to_path_buf(),
                index: stream,
            })?;
        let parameters = source.parameters();
        let avg = source.avg_frame_rate();
        let rate = source.rate();
        let mut out = self
            .output
            .add_stream(ffmpeg::encoder::find(ffmpeg::codec::Id::None))
            .map_err(|e| MediaError::Write {
                path: self.path.clone(),
                action: "adding a stream",
                source: e.into(),
            })?;
        out.set_parameters(parameters);
        out.set_time_base(ffmpeg::Rational::from(summary.time_base));
        if avg.numerator() > 0 {
            out.set_avg_frame_rate(avg);
        }
        if rate.numerator() > 0 {
            out.set_rate(rate);
        }
        let index = out.index();
        self.time_bases.push(summary.time_base);
        Ok(index)
    }

    /// Sets a metadata tag of a stream (`timecode`, `language`, `handler_name`).
    pub fn set_stream_metadata(&mut self, stream: usize, key: &str, value: &str) -> Result<()> {
        let Some(mut st) = self.output.stream_mut(stream) else {
            return Err(MediaError::NoSuchStream {
                path: self.path.clone(),
                index: stream,
            });
        };
        let mut dict = st.metadata().to_owned();
        dict.set(key, value);
        st.set_metadata(dict);
        Ok(())
    }

    /// Sets a container metadata tag.
    pub fn set_metadata(&mut self, key: &str, value: &str) {
        let mut dict = self.output.metadata().to_owned();
        dict.set(key, value);
        self.output.set_metadata(dict);
    }

    /// Stores a display matrix (rotation and mirroring, as read from a source stream) on
    /// a video stream.
    pub fn set_display_matrix(&mut self, stream: usize, matrix: &[i32; 9]) -> Result<()> {
        let result = ffi::set_display_matrix(&mut self.output, stream, matrix);
        result.map_err(self.error("setting the display matrix"))
    }

    /// Stores HDR mastering display and content light levels on a video stream, which
    /// the MOV/MP4 muxer writes as `mdcv` and `clli` boxes and Matroska as colour
    /// elements.
    pub fn set_hdr_metadata(
        &mut self,
        stream: usize,
        hdr: &crate::export::HdrMetadata,
    ) -> Result<()> {
        let result = ffi::set_hdr_metadata(&mut self.output, stream, hdr);
        result.map_err(self.error("setting the HDR metadata"))
    }

    /// Sets the codec tag of a stream (`hvc1` for HEVC that QuickTime players accept).
    pub fn set_codec_tag(&mut self, stream: usize, fourcc: [u8; 4]) -> Result<()> {
        let result = ffi::set_stream_codec_tag(&mut self.output, stream, fourcc);
        result.map_err(self.error("setting the codec tag"))
    }

    /// A muxer option applied when the header is written (`movflags`, ...).
    pub fn set_option(&mut self, key: &str, value: &str) {
        self.options.push((key.to_string(), value.to_string()));
    }

    /// Writes the container header; streams and metadata are fixed from here on.
    pub fn write_header(&mut self) -> Result<()> {
        let mut dict = ffmpeg::Dictionary::new();
        for (k, v) in &self.options {
            dict.set(k, v);
        }
        let result = self.output.write_header_with(dict).map(|_| ());
        result.map_err(self.error("writing the header"))?;
        self.time_bases = (0..self.output.nb_streams() as usize)
            .map(|i| {
                self.output
                    .stream(i)
                    .map_or(Rational::new(1, 1), |s| Rational::from(s.time_base()))
            })
            .collect();
        self.header_written = true;
        Ok(())
    }

    /// Writes a packet to `stream`, rescaling its timestamps from the packet's time base
    /// to the stream's.
    pub fn write_packet(&mut self, stream: usize, packet: Packet) -> Result<()> {
        if !self.header_written {
            self.write_header()?;
        }
        let Some(&to) = self.time_bases.get(stream) else {
            return Err(MediaError::NoSuchStream {
                path: self.path.clone(),
                index: stream,
            });
        };
        let from = packet.time_base();
        let mut raw = packet.into_raw();
        raw.set_stream(stream);
        raw.rescale_ts(ffmpeg::Rational::from(from), ffmpeg::Rational::from(to));
        raw.set_position(-1);
        let result = raw.write_interleaved(&mut self.output);
        result.map_err(self.error("writing a packet"))
    }

    /// Flushes interleaved packets and writes the trailer (the MOV index).
    pub fn finish(mut self) -> Result<()> {
        if !self.header_written {
            self.write_header()?;
        }
        let result = self.output.write_trailer();
        result.map_err(self.error("writing the trailer"))
    }
}
