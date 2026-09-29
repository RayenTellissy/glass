//! The software decode path: one stream of one file in, frames out in decode order of
//! presentation, with seek to start. Hardware decoders will sit behind the same API.

use std::path::Path;

use crate::decode::{Decoder, DecoderOptions, Frame, Received};
use crate::demux::{Demuxer, StreamSummary};
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::MediaKind;

/// Which stream of a file to decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamSelect {
    /// FFmpeg's main video stream.
    BestVideo,
    /// FFmpeg's main audio stream.
    BestAudio,
    /// A stream by its container index.
    Index(usize),
}

/// Decodes one video or audio stream of a file, frame by frame.
///
/// Video frames come out in their native CPU pixel format (`yuv420p`, `yuv420p10le`,
/// `yuv422p10le`, `rgb24`, ...) and audio frames in the decoder's native sample format
/// (`fltp` for AAC and Opus, `s16`/`s32` for PCM); conversion to RGB happens on the GPU
/// and conversion to planar f32 at the project rate in the audio decode stage.
///
/// `Send` but not `Sync`: owned by one decode-pool thread at a time.
#[derive(Debug)]
pub struct FrameReader {
    demuxer: Demuxer,
    decoder: Decoder,
    stream: StreamSummary,
    draining: bool,
    finished: bool,
}

impl FrameReader {
    pub fn open(path: impl AsRef<Path>, select: StreamSelect) -> Result<Self> {
        Self::open_with(path, select, DecoderOptions::default())
    }

    pub fn open_with(
        path: impl AsRef<Path>,
        select: StreamSelect,
        options: DecoderOptions,
    ) -> Result<Self> {
        let mut demuxer = Demuxer::open(path)?;
        let index = match select {
            StreamSelect::Index(i) => i,
            StreamSelect::BestVideo | StreamSelect::BestAudio => {
                let kind = if select == StreamSelect::BestVideo {
                    MediaKind::Video
                } else {
                    MediaKind::Audio
                };
                demuxer
                    .best_stream(kind)
                    .ok_or_else(|| MediaError::NoStream {
                        path: demuxer.path().to_path_buf(),
                        kind,
                    })?
            }
        };
        let stream = demuxer.stream(index)?.clone();
        let decoder = Decoder::open_with(&demuxer, index, options)?;
        ffi::keep_only_streams(demuxer.input_mut(), &[index]);
        Ok(Self {
            demuxer,
            decoder,
            stream,
            draining: false,
            finished: false,
        })
    }

    /// The stream being decoded.
    pub fn stream(&self) -> &StreamSummary {
        &self.stream
    }

    pub fn kind(&self) -> MediaKind {
        self.stream.kind
    }

    /// Always false for now; hardware decode arrives behind this same reader.
    pub fn is_hardware(&self) -> bool {
        false
    }

    /// The next frame in presentation order, or `None` once the stream is exhausted.
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            if self.finished {
                return Ok(None);
            }
            match self.decoder.receive_frame()? {
                Received::Frame(frame) => return Ok(Some(frame)),
                Received::Finished => {
                    self.finished = true;
                    return Ok(None);
                }
                Received::NeedsInput if self.draining => {
                    // A decoder that asks for input after end of stream has nothing left.
                    self.finished = true;
                    return Ok(None);
                }
                Received::NeedsInput => match self.demuxer.read_packet()? {
                    Some(packet) => self.decoder.send_packet(&packet)?,
                    None => {
                        self.decoder.send_eof()?;
                        self.draining = true;
                    }
                },
            }
        }
    }

    /// Goes back to the first frame: seeks the demuxer to the start and flushes the
    /// decoder, so the next `next_frame` returns the first frame again.
    pub fn seek_to_start(&mut self) -> Result<()> {
        let container = self.demuxer.container();
        if container == "image2" || container.ends_with("_pipe") {
            // FFmpeg's image demuxers do not reliably return to their first image after
            // reaching the end; reopening a still or a sequence costs next to nothing.
            let mut demuxer = Demuxer::open(self.demuxer.path())?;
            ffi::keep_only_streams(demuxer.input_mut(), &[self.stream.index]);
            self.demuxer = demuxer;
        } else {
            self.demuxer.rewind()?;
        }
        self.decoder.flush();
        self.draining = false;
        self.finished = false;
        Ok(())
    }
}

impl Iterator for FrameReader {
    type Item = Result<Frame>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_frame().transpose()
    }
}
