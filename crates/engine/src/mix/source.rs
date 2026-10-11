//! The clip side of the mix: the decoded, resampled, gain-scaled and faded audio of every
//! audio track's clips, summed per track into feed frames for the mixer graph. Runs on the
//! `glass-audio-decode` thread for playback and on the caller's thread offline; never in
//! the audio callback.
//!
//! A clip plays over its own mix frames stretched over its transitions' spans
//! ([`super::edges`]): during a crossfade the outgoing and incoming clips both play, each
//! with its own reader and fade, and their sum is the track's feed. A clip of a nested
//! sequence (Decision 69) plays that sequence's own mix as its source: the nested
//! sequence's clips are rendered here by a nested [`SequenceSource`] at the clip's reader
//! rate and summed over its tracks (every nested track at its defaults: unity fader,
//! centred, which passes each feed unchanged, Decision 73), with the parent clip's source
//! time as the nested timeline's time; nested sequences nest further the same way.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use audio::mixer::{CONTROL_FRAMES, db_to_gain};
use media::{AudioOptions, AudioReader};
use project::{ClipParam, ClipSource as Source, Item, ItemId, Project, SequenceId, Time};

use super::edges::{Edges, track_edges};
use super::{MixError, MixFormat, audio_tracks, sample_of};

/// Frames rendered per step into the planar scratch buffer.
const SCRATCH_FRAMES: usize = 4096;

/// The slowest and fastest reader rate a clip's speed may ask for.
const READER_RATES: std::ops::RangeInclusive<u32> = 1_000..=1_536_000;

/// The deepest chain of nested sequences mixed; clips deeper down are silent. Edits refuse
/// nesting cycles, so this only guards a damaged tree.
const MAX_NESTING: usize = 16;

/// What the clip side ran into. Counts include the clips inside nested sequences (once
/// per clip nesting them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceStats {
    /// Enabled clips on audio tracks.
    pub clips: usize,
    /// Clips whose media the project does not list (silent).
    pub missing_media: usize,
    /// Clips of nested sequences (Decision 69), mixed from the nested sequence's clips.
    pub nested_clips: usize,
    /// Nested clips left silent: their sequence is already being mixed above them (a
    /// cycle, which edits refuse), the project does not have it, or it nests deeper than
    /// 16 levels.
    pub silent_nested: usize,
    /// Clips with a speed ramp or an out-of-range speed (silent until varispeed exists).
    pub unsupported_speed: usize,
    /// Clips whose media could not be opened as audio (silent).
    pub failed_opens: usize,
    /// Reads that failed after opening (the clip goes silent).
    pub decode_errors: usize,
}

impl SourceStats {
    fn add(&mut self, other: SourceStats) {
        self.clips += other.clips;
        self.missing_media += other.missing_media;
        self.nested_clips += other.nested_clips;
        self.silent_nested += other.silent_nested;
        self.unsupported_speed += other.unsupported_speed;
        self.failed_opens += other.failed_opens;
        self.decode_errors += other.decode_errors;
    }

    /// Clips that play silence for any reason.
    pub fn silent(&self) -> usize {
        self.missing_media
            + self.silent_nested
            + self.unsupported_speed
            + self.failed_opens
            + self.decode_errors
    }
}

/// Where a clip's audio comes from.
enum Input {
    Media {
        path: PathBuf,
        /// Boxed: a reader is far larger than the nested variant.
        reader: Option<Box<AudioReader>>,
        failed: bool,
    },
    Nested {
        sequence: SequenceId,
        source: Box<SequenceSource>,
        /// The nested source's feed frames for one step.
        buffer: Vec<f32>,
    },
}

/// One clip on its way to a track's feed.
struct ClipSource {
    item: Arc<Item>,
    input: Input,
    /// Mix frames the clip covers on the timeline.
    start: i64,
    end: i64,
    /// Mix frames it plays over: `start..end` stretched over its transitions.
    play_start: i64,
    play_end: i64,
    edges: Edges,
    /// The reader's rate: the mix rate divided by the clip's speed.
    reader_rate: u32,
    /// The reader sample at the clip's first frame (`start`).
    first_sample: i64,
    /// The clip's gain is a curve (else the constant applies throughout).
    animated: bool,
    constant_gain: f32,
}

impl ClipSource {
    /// What makes a previous clip's open input reusable.
    fn key(&self) -> (ItemId, Option<&Path>, Option<SequenceId>, u32) {
        match &self.input {
            Input::Media { path, .. } => (self.item.id, Some(path), None, self.reader_rate),
            Input::Nested { sequence, .. } => {
                (self.item.id, None, Some(*sequence), self.reader_rate)
            }
        }
    }

    fn is_open(&self) -> bool {
        match &self.input {
            Input::Media { reader, .. } => reader.is_some(),
            Input::Nested { .. } => true,
        }
    }

    /// The reader (or nested mix) sample at mix frame `frame`.
    fn source_sample(&self, frame: i64) -> i64 {
        self.first_sample + (frame - self.start)
    }

    /// The linear clip gain at mix frame `frame`.
    fn gain_at(&self, frame: i64, rate: u32) -> f32 {
        let Some(clip) = self.item.clip() else {
            return self.constant_gain;
        };
        let source = Time::from_samples(frame, rate)
            .ok()
            .and_then(|t| clip.source_time_at(self.item.range.start, t).ok());
        match source {
            Some(t) => db_to_gain(clip.gain_db_at(t)),
            None => self.constant_gain,
        }
    }
}

/// Every audio track's clips, rendered into feed frames. See the module docs.
pub(crate) struct SequenceSource {
    format: MixFormat,
    media_layout: media::ChannelLayout,
    tracks: Vec<Vec<ClipSource>>,
    scratch: Vec<f32>,
    stats: SourceStats,
}

impl SequenceSource {
    /// The clips of `sequence`'s audio tracks at `format`, with declick ramps at hard cuts
    /// when `declick` (see [`super::edges`]). Readers of `previous` whose clip, media and
    /// rate are unchanged are kept (an edit does not reopen every file).
    pub(crate) fn build(
        project: &Project,
        sequence: SequenceId,
        format: MixFormat,
        declick: bool,
        previous: Option<SequenceSource>,
    ) -> Result<Self, MixError> {
        Self::build_in(
            project,
            sequence,
            format,
            declick,
            previous,
            &mut Vec::new(),
        )
    }

    /// [`build`](Self::build) inside the sequences on `stack` (outermost first).
    fn build_in(
        project: &Project,
        sequence: SequenceId,
        format: MixFormat,
        declick: bool,
        previous: Option<SequenceSource>,
        stack: &mut Vec<SequenceId>,
    ) -> Result<Self, MixError> {
        let seq = project
            .sequence(sequence)
            .ok_or(MixError::UnknownSequence(sequence))?;
        let rate = format.sample_rate;
        if rate == 0 {
            return Err(MixError::ZeroRate);
        }
        let grid = seq.grid(project::TrackKind::Audio)?;
        let mut kept: Vec<ClipSource> = previous
            .map(|p| {
                p.tracks
                    .into_iter()
                    .flatten()
                    .filter(ClipSource::is_open)
                    .collect()
            })
            .unwrap_or_default();
        stack.push(sequence);
        let mut stats = SourceStats::default();
        let mut tracks = Vec::new();
        for track in audio_tracks(seq) {
            let edges = track_edges(track, grid, rate, declick);
            let mut clips = Vec::new();
            for item in &track.items {
                let Some(clip) = item.clip() else { continue };
                if !clip.enabled {
                    continue;
                }
                stats.clips += 1;
                let reader_rate = clip.speed.constant_speed().and_then(|speed| {
                    let rate = (f64::from(rate) * f64::from(speed.den) / f64::from(speed.num))
                        .round() as u32;
                    READER_RATES.contains(&rate).then_some(rate)
                });
                let input = match clip.source {
                    Source::Media(id) => {
                        let Some(media) = project.media().get(id) else {
                            stats.missing_media += 1;
                            continue;
                        };
                        Input::Media {
                            path: PathBuf::from(&media.path),
                            reader: None,
                            failed: false,
                        }
                    }
                    Source::Sequence(nested) => {
                        stats.nested_clips += 1;
                        if stack.contains(&nested)
                            || stack.len() >= MAX_NESTING
                            || project.sequence(nested).is_none()
                        {
                            stats.silent_nested += 1;
                            continue;
                        }
                        // Built below, once the reader rate is known.
                        Input::Nested {
                            sequence: nested,
                            source: Box::new(Self::empty(format)),
                            buffer: Vec::new(),
                        }
                    }
                };
                let Some(reader_rate) = reader_rate else {
                    stats.unsupported_speed += 1;
                    continue;
                };
                let start = sample_of(item.range.start, rate)?;
                let end = sample_of(item.range.end()?, rate)?;
                let edges = edges.get(&item.id).copied().unwrap_or_default();
                let (play_start, play_end) = edges.play_range(start, end);
                let mut source = ClipSource {
                    item: Arc::clone(item),
                    input,
                    start,
                    end,
                    play_start,
                    play_end,
                    edges,
                    reader_rate,
                    first_sample: sample_of(clip.source_start, reader_rate)?,
                    animated: clip.curve(ClipParam::GainDb).is_some(),
                    constant_gain: db_to_gain(clip.gain_db),
                };
                let reuse = kept
                    .iter()
                    .position(|k| k.key() == source.key())
                    .map(|i| kept.swap_remove(i).input);
                match &mut source.input {
                    Input::Media { reader, .. } => {
                        if let Some(Input::Media { reader: old, .. }) = reuse {
                            *reader = old;
                        }
                    }
                    Input::Nested {
                        sequence: nested,
                        source: inner,
                        buffer,
                    } => {
                        let previous = match reuse {
                            Some(Input::Nested { source, .. }) => Some(*source),
                            _ => None,
                        };
                        let nested_format = MixFormat {
                            sample_rate: reader_rate,
                            layout: format.layout,
                        };
                        let built = Self::build_in(
                            project,
                            *nested,
                            nested_format,
                            declick,
                            previous,
                            stack,
                        )?;
                        *buffer = vec![0.0; SCRATCH_FRAMES * built.width()];
                        **inner = built;
                    }
                }
                clips.push(source);
            }
            tracks.push(clips);
        }
        stack.pop();
        Ok(Self {
            format,
            media_layout: match format.layout {
                audio::mixer::Layout::Mono => media::ChannelLayout::Mono,
                audio::mixer::Layout::Stereo => media::ChannelLayout::Stereo,
                audio::mixer::Layout::Surround51 => media::ChannelLayout::Surround51,
            },
            tracks,
            scratch: vec![0.0; SCRATCH_FRAMES * format.channels()],
            stats,
        })
    }

    /// A source without tracks.
    pub(crate) fn empty(format: MixFormat) -> Self {
        Self {
            format,
            media_layout: media::ChannelLayout::Stereo,
            tracks: Vec::new(),
            scratch: Vec::new(),
            stats: SourceStats::default(),
        }
    }

    /// Samples per feed frame: every track's channels.
    pub(crate) fn width(&self) -> usize {
        self.tracks.len() * self.format.channels()
    }

    /// The counters of this source and the nested sources under it.
    pub(crate) fn stats(&self) -> SourceStats {
        let mut stats = self.stats;
        for clip in self.tracks.iter().flatten() {
            if let Input::Nested { source, .. } = &clip.input {
                stats.add(source.stats());
            }
        }
        stats
    }

    /// Whether any clip sounds within part of mix frames `from..to` (a nested clip when a
    /// clip of its sequence does).
    pub(crate) fn plays_between(&self, from: i64, to: i64) -> bool {
        self.tracks.iter().flatten().any(|c| {
            c.play_start < to
                && c.play_end > from
                && match &c.input {
                    Input::Media { .. } => true,
                    Input::Nested { source, .. } => source.plays_between(
                        c.source_sample(from.max(c.play_start)),
                        c.source_sample(to.min(c.play_end)),
                    ),
                }
        })
    }

    /// Opens the readers of clips that play within `from..to` and closes those of clips
    /// that ended before `from`, so a file is open only around the playhead.
    pub(crate) fn prepare(&mut self, from: i64, to: i64) {
        let layout = self.media_layout;
        for clip in self.tracks.iter_mut().flatten() {
            let (play_start, play_end) = (clip.play_start, clip.play_end);
            let span = (
                clip.source_sample(from.max(play_start)),
                clip.source_sample(to.min(play_end)),
            );
            match &mut clip.input {
                Input::Media {
                    path,
                    reader,
                    failed,
                } => {
                    if play_end <= from {
                        *reader = None;
                    } else if play_start < to && reader.is_none() && !*failed {
                        let options = AudioOptions {
                            sample_rate: clip.reader_rate,
                            layout,
                            ..AudioOptions::default()
                        };
                        match AudioReader::open(&*path, options) {
                            Ok(opened) => *reader = Some(Box::new(opened)),
                            Err(error) => {
                                tracing::warn!(path = %path.display(), %error, "audio clip did not open");
                                *failed = true;
                                self.stats.failed_opens += 1;
                            }
                        }
                    }
                }
                Input::Nested { source, .. } => {
                    if play_end <= from {
                        source.close();
                    } else if play_start < to {
                        source.prepare(span.0, span.1);
                    }
                }
            }
        }
    }

    /// Closes every reader, nested ones included.
    fn close(&mut self) {
        for clip in self.tracks.iter_mut().flatten() {
            match &mut clip.input {
                Input::Media { reader, .. } => *reader = None,
                Input::Nested { source, .. } => source.close(),
            }
        }
    }

    /// Writes `frames` feed frames from mix frame `position` into `out` (interleaved,
    /// [`width`](Self::width) samples per frame).
    pub(crate) fn render(&mut self, position: i64, frames: usize, out: &mut [f32]) {
        let width = self.width();
        out[..frames * width].fill(0.0);
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(SCRATCH_FRAMES);
            let at = position + done as i64;
            self.prepare(at, at + n as i64);
            self.render_step(at, n, &mut out[done * width..(done + n) * width]);
            done += n;
        }
    }

    fn render_step(&mut self, position: i64, frames: usize, out: &mut [f32]) {
        let channels = self.format.channels();
        let width = self.width();
        let rate = self.format.sample_rate;
        let end = position + frames as i64;
        for (t, clips) in self.tracks.iter_mut().enumerate() {
            for clip in clips.iter_mut() {
                if clip.play_end <= position || clip.play_start >= end {
                    continue;
                }
                let from = clip.play_start.max(position);
                let to = clip.play_end.min(end);
                let n = (to - from) as usize;
                if !read_clip(clip, from, n, channels, &mut self.scratch, &mut self.stats) {
                    continue;
                }
                apply_gain(clip, from, n, channels, rate, &mut self.scratch);
                apply_edges(clip, from, n, channels, &mut self.scratch);
                let offset = (from - position) as usize;
                for c in 0..channels {
                    let plane = &self.scratch[c * SCRATCH_FRAMES..c * SCRATCH_FRAMES + n];
                    for (k, &s) in plane.iter().enumerate() {
                        out[(offset + k) * width + t * channels + c] += s;
                    }
                }
            }
        }
    }
}

/// Reads `n` frames of `clip` from mix frame `from` into the scratch planes (silence where
/// the source has none). Returns false when the clip has no reader (silent).
fn read_clip(
    clip: &mut ClipSource,
    from: i64,
    n: usize,
    channels: usize,
    scratch: &mut [f32],
    stats: &mut SourceStats,
) -> bool {
    let want = clip.source_sample(from);
    match &mut clip.input {
        Input::Media {
            path,
            reader,
            failed,
        } => {
            let Some(r) = reader.as_mut() else {
                return false;
            };
            for c in 0..channels {
                scratch[c * SCRATCH_FRAMES..c * SCRATCH_FRAMES + n].fill(0.0);
            }
            // Audio that starts after the clip's first sample (a file whose audio begins
            // late), or a head handle reaching before the file: silence until it does.
            let first = r.first_sample() as i64;
            let lead = (first - want).clamp(0, n as i64) as usize;
            let at = (want + lead as i64).max(0) as u64;
            if lead < n {
                let result = (|| {
                    if r.position() != at {
                        r.seek_to_sample(at)?;
                    }
                    let mut planes = scratch.chunks_mut(SCRATCH_FRAMES);
                    let mut slices: [&mut [f32]; audio::mixer::MAX_CHANNELS] =
                        std::array::from_fn(|_| {
                            planes.next().map_or(&mut [][..], |p| &mut p[lead..n])
                        });
                    r.read(&mut slices[..channels])
                })();
                if let Err(error) = result {
                    tracing::warn!(path = %path.display(), %error, "audio clip read failed");
                    stats.decode_errors += 1;
                    *reader = None;
                    *failed = true;
                    return false;
                }
            }
            true
        }
        Input::Nested { source, buffer, .. } => {
            let width = source.width();
            for c in 0..channels {
                scratch[c * SCRATCH_FRAMES..c * SCRATCH_FRAMES + n].fill(0.0);
            }
            if width == 0 {
                return true;
            }
            source.render(want, n, &mut buffer[..n * width]);
            // Every nested track at its defaults: the mix is the sum of their feeds.
            for (k, frame) in buffer[..n * width].chunks_exact(width).enumerate() {
                for track in frame.chunks_exact(channels) {
                    for (c, &s) in track.iter().enumerate() {
                        scratch[c * SCRATCH_FRAMES + k] += s;
                    }
                }
            }
            true
        }
    }
}

/// Scales the scratch planes of `clip` (frames `from..from + n`) by its gain.
fn apply_gain(
    clip: &ClipSource,
    from: i64,
    n: usize,
    channels: usize,
    rate: u32,
    scratch: &mut [f32],
) {
    if !clip.animated {
        if clip.constant_gain != 1.0 {
            for c in 0..channels {
                for s in &mut scratch[c * SCRATCH_FRAMES..c * SCRATCH_FRAMES + n] {
                    *s *= clip.constant_gain;
                }
            }
        }
        return;
    }
    // The gain at each control point, ramped linearly in between: a function of the
    // timeline frame alone, so any block size gives the same samples.
    let control = CONTROL_FRAMES as i64;
    let mut k = 0usize;
    while k < n {
        let f = from + k as i64;
        let before = f.div_euclid(control) * control;
        let len = ((before + control - f) as usize).min(n - k);
        let (g0, g1) = (
            clip.gain_at(before, rate),
            clip.gain_at(before + control, rate),
        );
        for c in 0..channels {
            let plane = &mut scratch[c * SCRATCH_FRAMES + k..c * SCRATCH_FRAMES + k + len];
            for (j, s) in plane.iter_mut().enumerate() {
                let x = (f - before + j as i64) as f32 / CONTROL_FRAMES as f32;
                *s *= g0 + (g1 - g0) * x;
            }
        }
        k += len;
    }
}

/// Scales the scratch planes of `clip` (frames `from..from + n`) by its fades and declick
/// ramps, sample by sample.
fn apply_edges(clip: &ClipSource, from: i64, n: usize, channels: usize, scratch: &mut [f32]) {
    let to = from + n as i64;
    if !clip.edges.shapes(clip.start, clip.end, from, to) {
        return;
    }
    for k in 0..n {
        let gain = clip.edges.gain(clip.start, clip.end, from + k as i64) as f32;
        if gain != 1.0 {
            for c in 0..channels {
                scratch[c * SCRATCH_FRAMES + k] *= gain;
            }
        }
    }
}
