//! Audio decode for playback and export: any audio stream in, planar `f32` at the
//! project's sample rate and channel layout out, in chunks written into buffers the
//! caller owns.
//!
//! FFmpeg only decodes. Sample-format conversion (`s16`, `s32`, `flt`, `dbl`, planar or
//! interleaved, to `f32`) and the channel matrix are plain Rust here, and the sample-rate
//! conversion is `rubato`'s asynchronous sinc resampler:
//!
//! | Setting | Value | Why |
//! | --- | --- | --- |
//! | Resampler | `rubato::Async::new_sinc` | band-limited sinc interpolation; rubato rates its quality equal to its FFT resampler, and the ratio can later follow clock drift or varispeed |
//! | Filter length | 256 taps | long enough for an automatic cutoff near 0.95 of the lower Nyquist rate with a BlackmanHarris2 window |
//! | Window | BlackmanHarris2 | sidelobes (aliasing) below -150 dB |
//! | Cutoff | automatic (`f_cutoff: None`) | rubato picks the highest cutoff the window's sidelobe level allows |
//! | Oversampling | 256 | sinc table resolution between input samples |
//! | Interpolation | cubic | between table points; the highest quality mode |
//! | Latency | `output_delay()` frames, about 128 input samples (2.7 ms at 48 kHz) | trimmed: the first output sample lines up with the first input sample |
//!
//! Sample `n` of the output is at `n / rate` seconds on the file's timeline, the same
//! zero that video pts use, so audio and video line up without offsets. Decoded frames
//! are placed by their presentation time, so encoder priming (AAC's 1024 or 2112
//! samples, Opus pre-skip, MP3 gapless headers), which FFmpeg removes and accounts for in
//! the pts, never shifts audio against video; a jump in pts is filled with silence and
//! an overlap is trimmed. Samples before time zero are dropped.
//!
//! Channel rules (`ChannelLayout`), gains on FFmpeg's channel labels:
//!
//! | Source | Mono out | Stereo out | 5.1 out (FL FR FC LFE SL SR) |
//! | --- | --- | --- | --- |
//! | mono | as is | copied to L and R (unity) | to FC |
//! | stereo | (L + R) / 2 | as is | to FL and FR |
//! | 5.1, 7.1, others | the stereo downmix, then (L + R) / 2 | ITU-R BS.775: L = FL + 0.707 FC + 0.707 SL (or BL), likewise R; LFE dropped | each channel to its speaker; back and side share the surround pair (0.707 each when both exist); top and wide channels fold into the nearest speaker at 0.707 |
//!
//! Downmixes are not normalised, so loud multichannel sources can exceed 1.0; the mixer
//! works in float and limits at the output.
//!
//! Threads: an [`AudioReader`] lives on the `glass-audio-decode` thread, never on the
//! audio callback. After the first chunks it allocates nothing: every buffer is sized
//! once for the largest decoded frame and resampler chunk.

use std::path::{Path, PathBuf};

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{
    Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use crate::decode::AudioFrame;
use crate::error::{MediaError, Result};
use crate::ffi;
use crate::format::{MediaKind, SampleType};
use crate::reader::{FrameReader, StreamSelect};
use crate::time::{Rational, Timestamp};

/// The channel layout audio is delivered in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    Mono,
    #[default]
    Stereo,
    /// FL, FR, FC, LFE, SL, SR (the WAV and SMPTE order).
    Surround51,
}

impl ChannelLayout {
    pub fn channels(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Surround51 => 6,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Mono => "mono",
            Self::Stereo => "stereo",
            Self::Surround51 => "5.1",
        }
    }

    /// The layout that keeps a source's channels: mono, stereo, 5.1 for anything wider
    /// (folded by the rules above), stereo when the count is unknown (0).
    pub fn for_channels(channels: usize) -> Self {
        match channels {
            1 => Self::Mono,
            0 | 2 => Self::Stereo,
            _ => Self::Surround51,
        }
    }
}

/// What an audio stream holds, known once [`AudioReader`] has opened it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceAudio {
    /// The stream's own sample rate.
    pub sample_rate: u32,
    /// The stream's channel count (0 for a stream without frames).
    pub channels: usize,
    /// The container's duration, else the stream's.
    pub duration: Option<Timestamp>,
}

/// How to decode an audio stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioOptions {
    /// Output sample rate (the project's); 48 kHz by default.
    pub sample_rate: u32,
    pub layout: ChannelLayout,
    pub stream: StreamSelect,
    /// Input frames per resampler call: the work done per chunk, not the size callers
    /// read, which can be anything.
    pub chunk_frames: usize,
}

impl Default for AudioOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            layout: ChannelLayout::Stereo,
            stream: StreamSelect::BestAudio,
            chunk_frames: 1024,
        }
    }
}

/// The sinc resampler parameters (see the module docs for the reasoning).
pub fn resampler_parameters() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: None,
        oversampling_factor: 256,
        interpolation: SincInterpolationType::Cubic,
        window: WindowFunction::BlackmanHarris2,
    }
}

/// Input frames decoded before the first output sample of a seek, so the resampler's
/// filter history is real audio rather than silence: twice the filter length.
const SEEK_WARM_UP: i64 = 512;

/// How far before a seek target decoding starts, for the decoder's own pre-roll (AAC's
/// overlapped transform needs one frame, Opus asks for 80 ms).
const SEEK_PRE_ROLL_SECONDS: f64 = 0.2;

/// A frame whose start is within this many seconds of the end of the previous one
/// continues it exactly; further away, its pts wins (a gap is filled with silence, an
/// overlap trimmed). Containers round pts (WebM to milliseconds), so exact contiguity is
/// the rule and pts only resynchronise.
const CONTIGUITY_SECONDS: f64 = 0.002;

/// Decodes one audio stream to planar `f32` at a chosen rate and layout (see the module
/// docs). `Send`, not `Sync`.
pub struct AudioReader {
    path: PathBuf,
    reader: FrameReader,
    source: SourceAudio,
    in_rate: u32,
    out_rate: u32,
    layout: ChannelLayout,
    /// Input and output samples per grid step: resampler runs start on a multiple of
    /// `step_in`, whose output index `step_out` times as far is exact.
    step_in: i64,
    step_out: i64,
    resampler: Option<Async<f32>>,

    /// Channel ids of the current source layout and the matrix built for them.
    source_ids: Vec<i32>,
    ids_scratch: Vec<i32>,
    matrix: Vec<f32>,
    /// The current decoded frame, converted and mixed to the output channels at the
    /// input rate, and how much of it has been fed.
    frame: Vec<Vec<f32>>,
    frame_len: usize,
    frame_pos: usize,
    /// A decoded frame waiting to be placed (the one read at open, or after a seek).
    pending: Option<AudioFrame>,
    pending_next: Option<AudioFrame>,
    /// How far frames' pts sit from their true start, in input samples: containers that
    /// store coarse timestamps (WebM, in milliseconds) round every frame the same way when
    /// frame lengths are whole ticks (Opus), which a pts-placed frame after a seek must
    /// undo to land on the same samples as a read from the start.
    pts_bias: i64,
    /// The next frame placed by its pts is corrected by `pts_bias` (after a seek; not at
    /// the start of the file, whose first frame is the reference).
    unbias: bool,
    /// Silence still to feed before the current frame (a gap in the pts).
    silence: i64,
    /// The next frame placed takes its position from its pts (the first after a start
    /// or seek); later ones continue the previous frame unless their pts disagree.
    resync: bool,
    /// Input-rate index of the next sample fed to the resampler.
    next_in: i64,
    /// One past the last input sample, once the stream has ended.
    in_end: Option<i64>,

    /// Resampler input and output buffers.
    chunk: Vec<Vec<f32>>,
    out: Vec<Vec<f32>>,
    out_pos: usize,
    out_len: usize,
    /// Output index of the next sample the resampler produces.
    out_index: i64,
    /// Resampler delay frames still to drop after a reset.
    delay_left: usize,
    /// Output samples before this index are dropped (the target of a seek).
    discard_before: i64,
    /// Output index of the first sample of the stream.
    first_out: i64,
    /// Output-rate index of the next sample `read` returns.
    position: i64,
    finished: bool,
}

impl std::fmt::Debug for AudioReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioReader")
            .field("path", &self.path)
            .field("in_rate", &self.in_rate)
            .field("out_rate", &self.out_rate)
            .field("layout", &self.layout)
            .field("position", &self.position)
            .finish()
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl AudioReader {
    pub fn open(path: impl AsRef<Path>, options: AudioOptions) -> Result<Self> {
        Self::open_for_source(path, options, |_, _| {})
    }

    /// Opens like [`open`](Self::open), letting `adjust` change the options once the
    /// stream's rate, channel count and duration are known (measure at the source rate,
    /// keep its layout), so a caller needs no separate [`probe`](crate::probe::probe) and
    /// the file is opened once.
    pub fn open_for_source(
        path: impl AsRef<Path>,
        mut options: AudioOptions,
        adjust: impl FnOnce(&SourceAudio, &mut AudioOptions),
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut reader = FrameReader::open(&path, options.stream)?;
        if reader.kind() != MediaKind::Audio {
            return Err(MediaError::NotDecodable {
                path,
                index: reader.stream().index,
                kind: reader.kind(),
            });
        }
        let first = loop {
            match reader.next_frame()? {
                Some(frame) => {
                    if let Some(audio) = frame.into_audio()
                        && audio.samples() > 0
                    {
                        break Some(audio);
                    }
                }
                None => break None,
            }
        };
        let source = SourceAudio {
            sample_rate: first
                .as_ref()
                .map_or(options.sample_rate, |f| f.sample_rate()),
            channels: first.as_ref().map_or(0, |f| usize::from(f.channels())),
            duration: reader.duration(),
        };
        adjust(&source, &mut options);
        let in_rate = source.sample_rate;
        let out_rate = options.sample_rate.max(1);
        let g = gcd(i64::from(in_rate), i64::from(out_rate));
        let channels = options.layout.channels();
        let chunk_frames = options.chunk_frames.max(64);
        let resampler = if in_rate == out_rate {
            None
        } else {
            let ratio = f64::from(out_rate) / f64::from(in_rate);
            Some(
                Async::<f32>::new_sinc(
                    ratio,
                    1.0,
                    &resampler_parameters(),
                    chunk_frames,
                    channels,
                    FixedAsync::Input,
                )
                .map_err(|e| resample_error(&path, e))?,
            )
        };
        let (in_max, out_max) = match &resampler {
            Some(r) => (r.input_frames_max(), r.output_frames_max()),
            None => (chunk_frames, chunk_frames),
        };
        let mut this = Self {
            path,
            reader,
            source,
            in_rate,
            out_rate,
            layout: options.layout,
            step_in: i64::from(in_rate) / g,
            step_out: i64::from(out_rate) / g,
            resampler,
            source_ids: Vec::new(),
            ids_scratch: Vec::with_capacity(16),
            matrix: Vec::new(),
            frame: vec![Vec::new(); channels],
            frame_len: 0,
            frame_pos: 0,
            pending: None,
            pending_next: None,
            pts_bias: 0,
            unbias: false,
            silence: 0,
            resync: true,
            next_in: 0,
            in_end: None,
            chunk: vec![vec![0.0; in_max]; channels],
            out: vec![vec![0.0; out_max]; channels],
            out_pos: 0,
            out_len: 0,
            out_index: 0,
            delay_left: 0,
            discard_before: 0,
            first_out: 0,
            position: 0,
            finished: false,
        };
        match first {
            Some(frame) => {
                let raw_start = this.frame_start(&frame).unwrap_or(0);
                let start = raw_start.max(0);
                this.first_out = this.out_ceil(start);
                // The second frame continues the first exactly; its pts shows the bias.
                this.pending_next = this.next_audio_frame()?;
                if let Some(next) = &this.pending_next
                    && let Some(next_start) = this.frame_start(next)
                {
                    let bias = next_start - (raw_start + frame.samples() as i64);
                    if bias.abs() <= this.tolerance() {
                        this.pts_bias = bias;
                    }
                }
                this.pending = Some(frame);
                this.start_run(this.anchor_at_or_before(start), this.first_out);
            }
            None => this.finished = true,
        }
        Ok(this)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.out_rate
    }

    /// The stream's own sample rate.
    pub fn source_rate(&self) -> u32 {
        self.in_rate
    }

    /// The stream's rate, channel count and duration.
    pub fn source(&self) -> &SourceAudio {
        &self.source
    }

    pub fn layout(&self) -> ChannelLayout {
        self.layout
    }

    pub fn channels(&self) -> usize {
        self.layout.channels()
    }

    /// Output frames of resampler latency that are trimmed (0 without resampling).
    pub fn resampler_delay(&self) -> usize {
        self.resampler.as_ref().map_or(0, |r| r.output_delay())
    }

    /// Output-rate index of the next sample [`read`](Self::read) returns: sample `n` is at
    /// `n / sample_rate` seconds on the file's timeline.
    pub fn position(&self) -> u64 {
        self.position.max(0) as u64
    }

    /// Index of the stream's first sample (not 0 when audio starts after video).
    pub fn first_sample(&self) -> u64 {
        self.first_out.max(0) as u64
    }

    /// The time of output sample `n`.
    pub fn time_of(&self, n: u64) -> Timestamp {
        Timestamp::new(n as i64, Rational::new(1, self.out_rate as i32))
    }

    /// Fills `out` (one slice per output channel, all the same length) from the current
    /// position; returns the frames written, fewer than asked only at the end of the
    /// stream (0 after it).
    pub fn read(&mut self, out: &mut [&mut [f32]]) -> Result<usize> {
        let channels = self.channels();
        if out.len() != channels {
            return Err(MediaError::Audio {
                path: self.path.clone(),
                message: format!("read needs {channels} channel buffers, got {}", out.len()),
            });
        }
        let frames = out.iter().map(|c| c.len()).min().unwrap_or(0);
        let mut written = 0;
        while written < frames {
            if self.out_len > 0 {
                let n = self.out_len.min(frames - written);
                for (dest, src) in out.iter_mut().zip(&self.out) {
                    dest[written..written + n]
                        .copy_from_slice(&src[self.out_pos..self.out_pos + n]);
                }
                self.out_pos += n;
                self.out_len -= n;
                self.position += n as i64;
                written += n;
                continue;
            }
            if !self.produce()? {
                break;
            }
        }
        Ok(written)
    }

    /// Moves to output sample `n` (at the output rate, on the file's timeline): the next
    /// [`read`](Self::read) starts exactly there, with the same samples a read from the
    /// start would give at that index. Before the stream's first sample it moves there.
    pub fn seek_to_sample(&mut self, n: u64) -> Result<()> {
        let target = (n as i64).max(self.first_out);
        let target_in =
            (i128::from(target) * i128::from(self.in_rate) / i128::from(self.out_rate)) as i64;
        let first_in = self.in_index_of_out(self.first_out);
        let anchor = self.anchor_at_or_before((target_in - SEEK_WARM_UP).max(first_in));
        let seconds = anchor as f64 / f64::from(self.in_rate) - SEEK_PRE_ROLL_SECONDS;
        let time_base = self.reader.stream().time_base;
        if seconds <= 0.0 {
            if self.reader.stream().codec.starts_with("pcm") {
                self.reader.seek_to_start()?;
            } else {
                // A fresh decoder, as at open: seeking to the first packet after a flush
                // loses the encoder priming that the first real samples overlap with (AAC
                // in MP4 lands on the packet at pts 0, not the priming packet before it).
                let stream = self.reader.stream().index;
                self.reader = FrameReader::open(&self.path, StreamSelect::Index(stream))?;
            }
            self.unbias = false;
        } else {
            self.reader
                .seek_keyframe(Timestamp::from_seconds(seconds, time_base))?;
            self.unbias = true;
        }
        self.pending = None;
        self.pending_next = None;
        self.frame_len = 0;
        self.frame_pos = 0;
        self.silence = 0;
        self.in_end = None;
        self.finished = false;
        self.start_run(anchor, target);
        Ok(())
    }

    /// Back to the stream's first sample.
    pub fn seek_to_start(&mut self) -> Result<()> {
        self.seek_to_sample(self.first_sample())
    }

    /// Restarts the resampler at input index `anchor` (a grid multiple), dropping output
    /// before `discard_before`.
    fn start_run(&mut self, anchor: i64, discard_before: i64) {
        self.next_in = anchor;
        self.resync = true;
        self.out_index = anchor / self.step_in * self.step_out;
        if let Some(r) = &mut self.resampler {
            r.reset();
            self.delay_left = r.output_delay();
        }
        self.out_pos = 0;
        self.out_len = 0;
        self.discard_before = discard_before;
        self.position = discard_before;
    }

    fn anchor_at_or_before(&self, input_index: i64) -> i64 {
        input_index.div_euclid(self.step_in) * self.step_in
    }

    /// The first output index at or after input index `i`.
    fn out_ceil(&self, i: i64) -> i64 {
        let num = i128::from(i) * i128::from(self.out_rate);
        let den = i128::from(self.in_rate);
        (num + den - 1).div_euclid(den) as i64
    }

    fn in_index_of_out(&self, o: i64) -> i64 {
        (i128::from(o) * i128::from(self.in_rate)).div_euclid(i128::from(self.out_rate)) as i64
    }

    /// Input-rate index of a frame's first sample, from its pts.
    fn frame_start(&self, frame: &AudioFrame) -> Option<i64> {
        let pts = frame.pts()?;
        let tb = pts.time_base;
        // pts * tb * rate, rounded to the nearest sample.
        let num = i128::from(pts.value) * i128::from(tb.num) * i128::from(self.in_rate);
        let den = i128::from(tb.den);
        if den <= 0 {
            return None;
        }
        Some(((2 * num + den).div_euclid(2 * den)) as i64)
    }

    /// Runs the resampler once. Returns false when the stream is exhausted.
    fn produce(&mut self) -> Result<bool> {
        if self.finished {
            return Ok(false);
        }
        let need = match &self.resampler {
            Some(r) => r.input_frames_next(),
            None => self.chunk[0].len(),
        };
        let filled = self.fill(need)?;
        let end_out = self.in_end.map(|end| {
            let num = i128::from(end) * i128::from(self.out_rate);
            let den = i128::from(self.in_rate);
            ((2 * num + den).div_euclid(2 * den)) as i64
        });

        let written = match &mut self.resampler {
            Some(r) => {
                let channels = self.chunk.len();
                let input = SequentialSliceOfVecs::new(&self.chunk, channels, need)
                    .map_err(|e| resample_error(&self.path, e))?;
                let capacity = self.out[0].len();
                let mut output = SequentialSliceOfVecs::new_mut(&mut self.out, channels, capacity)
                    .map_err(|e| resample_error(&self.path, e))?;
                let indexing = Indexing {
                    input_offset: 0,
                    output_offset: 0,
                    partial_len: (filled < need).then_some(filled),
                    active_channels_mask: None,
                };
                let (_, written) = r
                    .process_into_buffer(&input, &mut output, Some(&indexing))
                    .map_err(|e| resample_error(&self.path, e))?;
                written
            }
            None => {
                for (out, chunk) in self.out.iter_mut().zip(&self.chunk) {
                    out[..filled].copy_from_slice(&chunk[..filled]);
                }
                filled
            }
        };

        let delay = self.delay_left.min(written);
        self.delay_left -= delay;
        let mut start = delay;
        let mut len = written - delay;
        // Drop output before the target of a seek (or the stream's first sample).
        if self.out_index < self.discard_before {
            let skip = ((self.discard_before - self.out_index) as usize).min(len);
            start += skip;
            len -= skip;
            self.out_index += skip as i64;
        }
        if let Some(end) = end_out {
            let room = (end - self.out_index).max(0) as usize;
            if len >= room {
                len = room;
                self.finished = true;
            }
        } else if filled == 0 && self.resampler.is_none() {
            self.finished = true;
        }
        self.out_pos = start;
        self.out_len = len;
        self.out_index += len as i64;
        Ok(len > 0 || !self.finished)
    }

    /// Fills the resampler input chunk with up to `need` frames: decoded audio, silence
    /// for gaps. Returns the frames filled (fewer only at the end of the stream).
    fn fill(&mut self, need: usize) -> Result<usize> {
        let mut filled = 0;
        while filled < need {
            if self.silence > 0 {
                let n = (self.silence as usize).min(need - filled);
                for chunk in &mut self.chunk {
                    chunk[filled..filled + n].fill(0.0);
                }
                self.silence -= n as i64;
                self.next_in += n as i64;
                filled += n;
                continue;
            }
            if self.frame_pos < self.frame_len {
                let n = (self.frame_len - self.frame_pos).min(need - filled);
                for (chunk, frame) in self.chunk.iter_mut().zip(&self.frame) {
                    chunk[filled..filled + n]
                        .copy_from_slice(&frame[self.frame_pos..self.frame_pos + n]);
                }
                self.frame_pos += n;
                self.next_in += n as i64;
                filled += n;
                continue;
            }
            if self.in_end.is_some() {
                break;
            }
            let frame = match self.pending.take().or_else(|| self.pending_next.take()) {
                Some(frame) => Some(frame),
                None => self.next_audio_frame()?,
            };
            let Some(frame) = frame else {
                self.in_end = Some(self.next_in);
                break;
            };
            self.place(&frame)?;
        }
        Ok(filled)
    }

    fn tolerance(&self) -> i64 {
        (CONTIGUITY_SECONDS * f64::from(self.in_rate)) as i64
    }

    fn next_audio_frame(&mut self) -> Result<Option<AudioFrame>> {
        loop {
            match self.reader.next_frame()? {
                Some(frame) => {
                    if let Some(audio) = frame.into_audio()
                        && audio.samples() > 0
                    {
                        return Ok(Some(audio));
                    }
                }
                None => return Ok(None),
            }
        }
    }

    /// Converts and mixes a decoded frame, and lines it up with the input position: a
    /// frame that starts later leaves a gap of silence, one that starts earlier loses
    /// its overlapping start.
    fn place(&mut self, frame: &AudioFrame) -> Result<()> {
        if frame.sample_rate() != self.in_rate {
            return Err(MediaError::Audio {
                path: self.path.clone(),
                message: format!(
                    "the sample rate changed mid-stream from {} to {} Hz",
                    self.in_rate,
                    frame.sample_rate()
                ),
            });
        }
        self.convert(frame);
        let tolerance = self.tolerance();
        let raw = self.frame_start(frame);
        let start = match raw {
            Some(s) if self.resync && self.unbias => s - self.pts_bias,
            Some(s) if self.resync || (s - self.next_in).abs() > tolerance => s,
            _ => self.next_in,
        };
        if !self.resync
            && let Some(s) = raw
            && (s - self.next_in).abs() <= tolerance
        {
            self.pts_bias = s - self.next_in;
        }
        // A frame wholly before the position (pre-roll after a seek) is dropped, and the
        // next one is placed by its pts again.
        self.resync = start + self.frame_len as i64 <= self.next_in;
        if start > self.next_in {
            self.silence = start - self.next_in;
            self.frame_pos = 0;
        } else {
            self.frame_pos = ((self.next_in - start) as usize).min(self.frame_len);
        }
        Ok(())
    }

    /// Converts a frame to `f32` and applies the channel matrix into `self.frame`.
    fn convert(&mut self, frame: &AudioFrame) {
        ffi::frame_channel_ids(frame.raw(), &mut self.ids_scratch);
        if self.ids_scratch != self.source_ids {
            std::mem::swap(&mut self.source_ids, &mut self.ids_scratch);
            self.matrix = channel_matrix(&self.source_ids, self.layout);
        }
        let samples = frame.samples();
        let in_channels = self.source_ids.len();
        for out in &mut self.frame {
            if out.len() < samples {
                out.resize(samples, 0.0);
            }
            out[..samples].fill(0.0);
        }
        self.frame_len = samples;
        let Some(format) = frame.format() else {
            return;
        };
        let out_channels = self.frame.len();
        for c in 0..in_channels {
            let (plane, stride, offset) = if format.planar {
                (frame.plane(c), 1, 0)
            } else {
                (frame.plane(0), in_channels, c)
            };
            let Some(bytes) = plane else { continue };
            for o in 0..out_channels {
                let gain = self.matrix[o * in_channels + c];
                if gain == 0.0 {
                    continue;
                }
                let dest = &mut self.frame[o][..samples];
                accumulate(dest, bytes, format.sample, stride, offset, gain);
            }
        }
    }
}

fn resample_error(path: &Path, e: impl std::fmt::Display) -> MediaError {
    MediaError::Audio {
        path: path.to_path_buf(),
        message: format!("resampler: {e}"),
    }
}

/// Adds `gain` times channel samples (every `stride`-th sample from `offset`) to `dest`.
fn accumulate(
    dest: &mut [f32],
    bytes: &[u8],
    sample: SampleType,
    stride: usize,
    offset: usize,
    gain: f32,
) {
    let size = sample.bytes();
    for (i, d) in dest.iter_mut().enumerate() {
        let at = (i * stride + offset) * size;
        let Some(s) = bytes.get(at..at + size) else {
            return;
        };
        let v = match sample {
            SampleType::U8 => (f32::from(s[0]) - 128.0) / 128.0,
            SampleType::I16 => f32::from(i16::from_ne_bytes([s[0], s[1]])) / 32768.0,
            SampleType::I32 => {
                i32::from_ne_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0
            }
            SampleType::I64 => {
                let v = i64::from_ne_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
                (v as f64 / 9_223_372_036_854_775_808.0) as f32
            }
            SampleType::F32 => f32::from_ne_bytes([s[0], s[1], s[2], s[3]]),
            SampleType::F64 => {
                f64::from_ne_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32
            }
        };
        *d += gain * v;
    }
}

/// Where a source channel sits, from FFmpeg's `AVChannel` id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Speaker {
    Left,
    Right,
    Centre,
    Lfe,
    SurroundLeft,
    SurroundRight,
    /// Top, wide and other positions: folded in at -3 dB.
    OtherLeft,
    OtherRight,
    OtherCentre,
}

fn speaker(id: i32) -> Speaker {
    match id {
        0 | 6 | 29 => Speaker::Left,         // FL, FLC, stereo left
        1 | 7 | 30 => Speaker::Right,        // FR, FRC, stereo right
        2 => Speaker::Centre,                // FC
        3 | 35 => Speaker::Lfe,              // LFE, LFE2
        4 | 9 | 33 => Speaker::SurroundLeft, // BL, SL, surround direct left
        5 | 10 | 34 => Speaker::SurroundRight,
        12 | 15 | 31 | 36 | 39 => Speaker::OtherLeft, // TFL, TBL, wide, TSL, BFL
        14 | 17 | 32 | 37 | 40 => Speaker::OtherRight,
        _ => Speaker::OtherCentre,
    }
}

const MINUS_3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// The mixing matrix, `out * in` gains in output-major order, for source channels `ids`
/// (see the module docs for the rules).
fn channel_matrix(ids: &[i32], layout: ChannelLayout) -> Vec<f32> {
    let n = ids.len();
    let mut m = vec![0.0; layout.channels() * n];
    if n == 0 {
        return m;
    }
    // Layouts that only give a count: mono is centre, the rest take FFmpeg's default
    // order for the count (stereo, then 5.1's FL FR FC LFE BL BR, ...).
    let default_ids = [0, 1, 2, 3, 4, 5, 6, 7];
    let speakers: Vec<Speaker> = ids
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            if n == 1 {
                Speaker::Centre
            } else if id < 0 {
                speaker(default_ids.get(i).copied().unwrap_or(-1))
            } else {
                speaker(id)
            }
        })
        .collect();

    // The stereo downmix, used for stereo and mono output.
    let mut stereo = vec![[0.0f32; 2]; n];
    for (c, s) in speakers.iter().enumerate() {
        stereo[c] = match s {
            _ if n == 1 => [1.0, 1.0],
            Speaker::Left => [1.0, 0.0],
            Speaker::Right => [0.0, 1.0],
            Speaker::Centre => [MINUS_3DB, MINUS_3DB],
            Speaker::Lfe => [0.0, 0.0],
            Speaker::SurroundLeft | Speaker::OtherLeft => [MINUS_3DB, 0.0],
            Speaker::SurroundRight | Speaker::OtherRight => [0.0, MINUS_3DB],
            Speaker::OtherCentre => [0.5, 0.5],
        };
    }
    match layout {
        ChannelLayout::Stereo => {
            for c in 0..n {
                m[c] = stereo[c][0];
                m[n + c] = stereo[c][1];
            }
        }
        ChannelLayout::Mono => {
            for c in 0..n {
                m[c] = if n == 1 {
                    1.0
                } else {
                    0.5 * (stereo[c][0] + stereo[c][1])
                };
            }
        }
        ChannelLayout::Surround51 => {
            let both_surrounds = ids.iter().any(|&id| id == 4 || id == 5)
                && ids.iter().any(|&id| id == 9 || id == 10);
            let surround_gain = if both_surrounds { MINUS_3DB } else { 1.0 };
            for (c, s) in speakers.iter().enumerate() {
                let (out, gain) = match s {
                    Speaker::Left => (0, 1.0),
                    Speaker::Right => (1, 1.0),
                    Speaker::Centre => (2, 1.0),
                    Speaker::Lfe => (3, 1.0),
                    Speaker::SurroundLeft => (4, surround_gain),
                    Speaker::SurroundRight => (5, surround_gain),
                    Speaker::OtherLeft => (0, MINUS_3DB),
                    Speaker::OtherRight => (1, MINUS_3DB),
                    Speaker::OtherCentre => (2, MINUS_3DB),
                };
                m[out * n + c] = gain;
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mix(ids: &[i32], layout: ChannelLayout, input: &[f32]) -> Vec<f32> {
        let m = channel_matrix(ids, layout);
        let n = ids.len();
        (0..layout.channels())
            .map(|o| (0..n).map(|c| m[o * n + c] * input[c]).sum())
            .collect()
    }

    #[test]
    fn mono_and_stereo_rules() {
        assert_eq!(mix(&[2], ChannelLayout::Stereo, &[0.5]), vec![0.5, 0.5]);
        assert_eq!(mix(&[-1], ChannelLayout::Stereo, &[0.5]), vec![0.5, 0.5]);
        assert_eq!(mix(&[0, 1], ChannelLayout::Mono, &[0.2, 0.6]), vec![0.4]);
        assert_eq!(
            mix(&[0, 1], ChannelLayout::Stereo, &[0.2, 0.6]),
            vec![0.2, 0.6]
        );
        assert_eq!(
            mix(&[-1, -1], ChannelLayout::Surround51, &[0.2, 0.6]),
            vec![0.2, 0.6, 0.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(
            mix(&[2], ChannelLayout::Surround51, &[0.5]),
            vec![0.0, 0.0, 0.5, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn surround_downmix_follows_bs775() {
        // 5.1(side): FL FR FC LFE SL SR.
        let ids = [0, 1, 2, 3, 9, 10];
        let input = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let out = mix(&ids, ChannelLayout::Stereo, &input);
        let k = MINUS_3DB;
        assert!((out[0] - (0.1 + k * 0.3 + k * 0.5)).abs() < 1e-6);
        assert!((out[1] - (0.2 + k * 0.3 + k * 0.6)).abs() < 1e-6);
        let mono = mix(&ids, ChannelLayout::Mono, &input);
        assert!((mono[0] - (out[0] + out[1]) / 2.0).abs() < 1e-6);
        assert_eq!(mix(&ids, ChannelLayout::Surround51, &input), input.to_vec());
        // 7.1: back and side share the surround pair.
        let ids71 = [0, 1, 2, 3, 4, 5, 9, 10];
        let out = mix(
            &ids71,
            ChannelLayout::Surround51,
            &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0],
        );
        assert!((out[4] - 2.0 * k).abs() < 1e-6);
    }

    /// Resamples a 1 s sine with the production parameters and fits a sine of the same
    /// frequency to the middle of the result: returns (amplitude, SNR in dB, delay trimmed).
    fn resample_sine(from: u32, to: u32, hz: f64) -> (f64, f64, usize) {
        let input: Vec<f32> = (0..from)
            .map(|i| {
                (0.5 * (2.0 * std::f64::consts::PI * hz * f64::from(i) / f64::from(from)).sin())
                    as f32
            })
            .collect();
        let mut r = Async::<f32>::new_sinc(
            f64::from(to) / f64::from(from),
            1.0,
            &resampler_parameters(),
            1024,
            1,
            FixedAsync::Input,
        )
        .expect("resampler");
        let delay = r.output_delay();
        let mut out = vec![vec![0.0f32; r.process_all_needed_output_len(input.len())]];
        let input = vec![input];
        let len = out[0].len();
        let (_, written) = r
            .process_all_into_buffer(
                &SequentialSliceOfVecs::new(&input, 1, from as usize).expect("in"),
                &mut SequentialSliceOfVecs::new_mut(&mut out, 1, len).expect("out"),
                from as usize,
                None,
            )
            .expect("process");
        assert!((written as i64 - i64::from(to)).abs() <= 1);
        // Least squares for a sin + b cos + c over the middle half.
        let (lo, hi) = (to as usize / 4, 3 * to as usize / 4);
        let w = 2.0 * std::f64::consts::PI * hz / f64::from(to);
        let (mut ss, mut cc, mut sc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, y) in out[0][lo..hi].iter().enumerate() {
            let t = (lo + i) as f64 * w;
            let (sn, cs, y) = (t.sin(), t.cos(), f64::from(*y));
            ss += sn * sn;
            cc += cs * cs;
            sc += sn * cs;
            ys += y * sn;
            yc += y * cs;
        }
        let det = ss * cc - sc * sc;
        let a = (ys * cc - yc * sc) / det;
        let b = (yc * ss - ys * sc) / det;
        let (mut signal, mut noise) = (0.0, 0.0);
        for (i, y) in out[0][lo..hi].iter().enumerate() {
            let t = (lo + i) as f64 * w;
            let fit = a * t.sin() + b * t.cos();
            signal += fit * fit;
            noise += (f64::from(*y) - fit).powi(2);
        }
        (
            (a * a + b * b).sqrt(),
            10.0 * (signal / noise.max(1e-30)).log10(),
            delay,
        )
    }

    #[test]
    fn resampler_quality() {
        for (from, to) in [(48_000, 44_100), (44_100, 48_000), (96_000, 48_000)] {
            for hz in [997.0, 15_000.0] {
                let (amplitude, snr, delay) = resample_sine(from, to, hz);
                eprintln!(
                    "{from} -> {to} Hz, {hz} Hz sine: amplitude {amplitude:.6} ({:+.4} dB), SNR {snr:.1} dB, delay {delay} output frames",
                    20.0 * (amplitude / 0.5).log10()
                );
                // f32 processing bounds the noise floor near -140 dB.
                assert!(snr > 110.0, "{from} -> {to}: SNR {snr} dB at {hz} Hz");
                assert!(
                    (amplitude / 0.5 - 1.0).abs() < 1e-3,
                    "{from} -> {to}: {amplitude}"
                );
            }
        }
    }

    #[test]
    fn resampler_rejects_aliases() {
        // 30 kHz cannot exist at 48 kHz; what passes the filter would alias to 18 kHz.
        let from = 96_000u32;
        let input: Vec<f32> = (0..from)
            .map(|i| {
                (0.5 * (2.0 * std::f64::consts::PI * 30_000.0 * f64::from(i) / f64::from(from))
                    .sin()) as f32
            })
            .collect();
        let mut r = Async::<f32>::new_sinc(
            0.5,
            1.0,
            &resampler_parameters(),
            1024,
            1,
            FixedAsync::Input,
        )
        .expect("resampler");
        let mut out = vec![vec![0.0f32; r.process_all_needed_output_len(input.len())]];
        let len = out[0].len();
        let input = vec![input];
        r.process_all_into_buffer(
            &SequentialSliceOfVecs::new(&input, 1, from as usize).expect("in"),
            &mut SequentialSliceOfVecs::new_mut(&mut out, 1, len).expect("out"),
            from as usize,
            None,
        )
        .expect("process");
        let middle = &out[0][12_000..36_000];
        let rms = (middle.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / middle.len() as f64)
            .sqrt();
        let db = 20.0 * (rms / (0.5 / std::f64::consts::SQRT_2)).log10();
        eprintln!("96 -> 48 kHz, 30 kHz sine: alias at {db:.1} dB");
        assert!(db < -100.0, "alias at {db} dB");
    }

    #[test]
    fn sample_conversion() {
        let mut d = [0.0f32; 2];
        let bytes: Vec<u8> = [i16::MIN, 16384]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        accumulate(&mut d, &bytes, SampleType::I16, 1, 0, 1.0);
        assert_eq!(d, [-1.0, 0.5]);
        let mut d = [0.0f32; 1];
        accumulate(&mut d, &[0, 255], SampleType::U8, 2, 1, 2.0);
        assert!((d[0] - 2.0 * 127.0 / 128.0).abs() < 1e-6);
    }
}
