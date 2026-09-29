//! The decode path: one stream of one file in, frames out in presentation order, with
//! seek to start. Hardware and software decoders sit behind the same API.

use std::cmp::Ordering;
use std::path::Path;
use std::sync::Arc;

use crate::decode::{Decoder, DecoderOptions, Frame, Received};
use crate::demux::{Demuxer, StreamSummary};
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::MediaKind;
use crate::gop::GopIndex;
use crate::hw::{DecodePath, FallbackReason, HwPreference};
use crate::time::Timestamp;
use crate::timemap::TimestampMap;

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
/// `yuv422p10le`, `rgb24`, ...), or as hardware surfaces when the options ask for
/// hardware decode ([`DecoderOptions::hw`]), and audio frames in the decoder's native sample format
/// (`fltp` for AAC and Opus, `s16`/`s32` for PCM); conversion to RGB happens on the GPU
/// and conversion to planar f32 at the project rate in the audio decode stage.
///
/// With [`HwPreference::Auto`] the reader falls back to software for the rest of the
/// file when the hardware decoder cannot take the stream (at open, or when its first
/// frame comes back in software) or fails mid-stream. A mid-stream switch reopens a
/// software decoder, seeks to the last frame handed out and skips up to it, so the
/// caller sees every frame once and in order. [`decode_path`](Self::decode_path) keeps
/// the reason, and the switch is logged.
///
/// `Send` but not `Sync`: owned by one decode-pool thread at a time.
#[derive(Debug)]
pub struct FrameReader {
    demuxer: Demuxer,
    decoder: Decoder,
    stream: StreamSummary,
    options: DecoderOptions,
    draining: bool,
    finished: bool,
    /// Presentation time of the last frame handed out.
    last_pts: Option<Timestamp>,
    /// After a switch to software: frames up to this time were already handed out.
    skip_through: Option<Timestamp>,
    /// A frame decoded past a seek target, returned by the next `next_frame`.
    pending: Option<Frame>,
    /// Keyframes of the stream, for accurate seeks; built on the first seek unless given.
    gop: Option<Arc<GopIndex>>,
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
            options,
            draining: false,
            finished: false,
            last_pts: None,
            skip_through: None,
            pending: None,
            gop: None,
        })
    }

    /// Uses `gop` (for example loaded from the project cache folder with
    /// [`GopIndex::load_or_build`]) for accurate seeks instead of scanning the file on
    /// the first one.
    pub fn with_gop_index(mut self, gop: Arc<GopIndex>) -> Self {
        self.gop = Some(gop);
        self
    }

    /// The GOP index, scanning the file for it on first use.
    pub fn gop_index(&mut self) -> Result<Arc<GopIndex>> {
        if let Some(gop) = &self.gop {
            return Ok(gop.clone());
        }
        let (gop, _) =
            GopIndex::build(self.demuxer.path(), StreamSelect::Index(self.stream.index))?;
        let gop = Arc::new(gop);
        self.gop = Some(gop.clone());
        Ok(gop)
    }

    /// The stream being decoded.
    pub fn stream(&self) -> &StreamSummary {
        &self.stream
    }

    pub fn kind(&self) -> MediaKind {
        self.stream.kind
    }

    /// True while video frames come out as hardware surfaces.
    pub fn is_hardware(&self) -> bool {
        self.decoder.is_hardware()
    }

    /// How the stream is being decoded: hardware, or software and why.
    pub fn decode_path(&self) -> &DecodePath {
        self.decoder.decode_path()
    }

    /// Test hook: the hardware decoder fails (as a decoder error would) once it has
    /// produced `frames` frames, so the switch to software can be exercised.
    #[doc(hidden)]
    pub fn fail_hardware_after(&mut self, frames: u64) {
        self.decoder.fail_hardware_after(frames);
    }

    /// The next frame in presentation order, or `None` once the stream is exhausted.
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        if let Some(frame) = self.pending.take() {
            return Ok(Some(frame));
        }
        self.read_frame()
    }

    /// Returns exactly the frame on screen at `target`: the last frame presented at or
    /// before it (the frame whose presentation time equals `target` when there is one),
    /// or the first frame when `target` is before it; `None` only for a stream without
    /// frames. Later `next_frame` calls continue after the returned frame.
    ///
    /// It starts decoding at the last keyframe presented at or before `target` (from the
    /// GOP index) and discards frames until it reaches the target, so open GOPs and
    /// reordered (B-frame) streams land on the right frame; when the target is ahead in
    /// the current GOP it decodes forward without seeking. Works on hardware and software
    /// decoders, after the end of the stream and backwards.
    pub fn seek_exact(&mut self, target: Timestamp) -> Result<Option<Frame>> {
        let gop = self.gop_index()?;
        let target = target.rescale_floor(self.stream.time_base);
        let mut keyframe = gop.keyframe_at_or_before(target).unwrap_or(0);
        let continue_forward = !self.finished
            && self.last_pts.is_some_and(|last| {
                last.cmp_time(target) == Ordering::Less
                    && gop.keyframe_at_or_before(last) == Some(keyframe)
            })
            && self.pending.as_ref().is_none_or(|f| {
                f.pts()
                    .is_some_and(|p| p.cmp_time(target) != Ordering::Greater)
            });
        if !continue_forward {
            self.seek_to_keyframe(&gop, keyframe)?;
        }
        loop {
            let mut candidate: Option<Frame> = None;
            loop {
                let Some(frame) = self.next_frame()? else {
                    return Ok(candidate);
                };
                let Some(pts) = frame.pts() else {
                    continue;
                };
                match pts.cmp_time(target) {
                    Ordering::Less => candidate = Some(frame),
                    Ordering::Equal => return Ok(Some(frame)),
                    Ordering::Greater => {
                        if candidate.is_some() {
                            self.pending = Some(frame);
                            return Ok(candidate);
                        }
                        // The first frame is already past the target: either the target is
                        // before the first frame, or decoding started too late.
                        if keyframe == 0 {
                            return Ok(Some(frame));
                        }
                        break;
                    }
                }
            }
            keyframe -= 1;
            self.seek_to_keyframe(&gop, keyframe)?;
        }
    }

    /// [`Self::seek_exact`] to the source frame sequence frame `n` of `map` shows; `None`
    /// past the end of the clip.
    pub fn seek_to_sequence_frame(&mut self, map: &TimestampMap, n: u64) -> Result<Option<Frame>> {
        match map.source_pts(n) {
            Some(pts) => self.seek_exact(pts),
            None => Ok(None),
        }
    }

    /// Positions the demuxer at a keyframe and resets the decoder.
    fn seek_to_keyframe(&mut self, gop: &GopIndex, keyframe: usize) -> Result<()> {
        match gop.keyframe(keyframe) {
            Some(k) => {
                let at = Timestamp::new(k.dts.unwrap_or(k.pts).min(k.pts), gop.time_base);
                self.demuxer.seek_stream(self.stream.index, at)?;
            }
            None => self.demuxer.rewind()?,
        }
        self.reset_decoder()?;
        self.draining = false;
        self.finished = false;
        self.pending = None;
        self.skip_through = None;
        self.last_pts = None;
        Ok(())
    }

    /// Drops the decoder's buffered frames after a seek. A hardware decoder that was
    /// drained (end of stream sent) is reopened instead: FFmpeg's VideoToolbox HEVC path
    /// returns the last drained frame once more after a flush.
    fn reset_decoder(&mut self) -> Result<()> {
        if self.draining && self.decoder.is_hardware() {
            self.decoder = Decoder::open_with(&self.demuxer, self.stream.index, self.options)?;
        } else {
            self.decoder.flush();
        }
        Ok(())
    }

    fn read_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            let hardware = self.decoder.is_hardware();
            let reason = match self.decode_next() {
                Ok(Some(frame)) => {
                    if hardware && !self.decoder.is_hardware() {
                        // FFmpeg dropped to software on its own, on one thread; start
                        // over with a proper software decoder.
                        self.decoder
                            .decode_path()
                            .fallback_reason()
                            .cloned()
                            .unwrap_or(FallbackReason::NotRequested)
                    } else {
                        if let (Some(skip), Some(pts)) = (self.skip_through, frame.pts()) {
                            if pts.cmp_time(skip) != Ordering::Greater {
                                continue;
                            }
                            self.skip_through = None;
                        }
                        self.last_pts = frame.pts().or(self.last_pts);
                        return Ok(Some(frame));
                    }
                }
                Err(error) if hardware && self.options.hw == HwPreference::Auto => {
                    match self.decoder.decode_path().fallback_reason() {
                        Some(reason) => reason.clone(),
                        None => FallbackReason::DecodeFailed {
                            codec: self.stream.codec.to_string(),
                            detail: error.to_string(),
                            at: self.last_pts,
                        },
                    }
                }
                other => return other,
            };
            self.fall_back(reason)?;
        }
    }

    /// Replaces the hardware decoder with a software one and resumes after the last frame
    /// handed out (or from the start when there was none).
    fn fall_back(&mut self, reason: FallbackReason) -> Result<()> {
        tracing::warn!(
            path = %self.demuxer.path().display(),
            stream = self.stream.index,
            resume_after = ?self.last_pts.map(|t| t.seconds()),
            %reason,
            "switching to software decoding: {}",
            reason.message()
        );
        let options = DecoderOptions {
            hw: HwPreference::Off,
            ..self.options
        };
        let mut decoder = Decoder::open_with(&self.demuxer, self.stream.index, options)?;
        decoder.set_fallback(reason);
        self.decoder = decoder;
        match self.last_pts {
            Some(at) => {
                self.demuxer.seek(at)?;
                self.skip_through = Some(at);
            }
            None => {
                self.demuxer.rewind()?;
                self.skip_through = None;
            }
        }
        self.draining = false;
        self.finished = false;
        Ok(())
    }

    fn decode_next(&mut self) -> Result<Option<Frame>> {
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
        self.reset_decoder()?;
        self.draining = false;
        self.finished = false;
        self.last_pts = None;
        self.skip_through = None;
        self.pending = None;
        Ok(())
    }
}

impl Iterator for FrameReader {
    type Item = Result<Frame>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_frame().transpose()
    }
}
