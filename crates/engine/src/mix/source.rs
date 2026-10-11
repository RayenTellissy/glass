//! The clip side of the mix: the decoded, resampled, gain-scaled audio of every audio
//! track's clips, summed per track into feed frames for the mixer graph. Runs on the
//! `glass-audio-decode` thread for playback and on the caller's thread offline; never in
//! the audio callback.

use std::path::PathBuf;
use std::sync::Arc;

use audio::mixer::{CONTROL_FRAMES, db_to_gain};
use media::{AudioOptions, AudioReader};
use project::{ClipParam, Item, ItemId, Project, SequenceId, Time};

use super::{MixError, MixFormat, audio_tracks, sample_of};

/// Frames rendered per step into the planar scratch buffer.
const SCRATCH_FRAMES: usize = 4096;

/// The slowest and fastest reader rate a clip's speed may ask for.
const READER_RATES: std::ops::RangeInclusive<u32> = 1_000..=1_536_000;

/// What the clip side ran into.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceStats {
    /// Enabled clips on audio tracks.
    pub clips: usize,
    /// Clips whose media the project does not list (silent).
    pub missing_media: usize,
    /// Clips of nested sequences (Decision 69): silent until the mix renders a nested
    /// sequence's audio.
    pub nested_clips: usize,
    /// Clips with a speed ramp or an out-of-range speed (silent until varispeed exists).
    pub unsupported_speed: usize,
    /// Clips whose media could not be opened as audio (silent).
    pub failed_opens: usize,
    /// Reads that failed after opening (the clip goes silent).
    pub decode_errors: usize,
}

/// One clip on its way to a track's feed.
struct ClipSource {
    item: Arc<Item>,
    path: PathBuf,
    /// Mix frames the clip covers on the timeline.
    start: i64,
    end: i64,
    /// The reader's rate: the mix rate divided by the clip's speed.
    reader_rate: u32,
    /// The reader sample at the clip's first frame.
    first_sample: i64,
    /// The clip's gain is a curve (else the constant applies throughout).
    animated: bool,
    constant_gain: f32,
    reader: Option<AudioReader>,
    failed: bool,
}

impl ClipSource {
    fn key(&self) -> (ItemId, &PathBuf, u32) {
        (self.item.id, &self.path, self.reader_rate)
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
    /// The clips of `sequence`'s audio tracks at `format`. Readers of `previous` whose
    /// clip, media and rate are unchanged are kept (an edit does not reopen every file).
    pub(crate) fn build(
        project: &Project,
        sequence: SequenceId,
        format: MixFormat,
        previous: Option<SequenceSource>,
    ) -> Result<Self, MixError> {
        let seq = project
            .sequence(sequence)
            .ok_or(MixError::UnknownSequence(sequence))?;
        let rate = format.sample_rate;
        if rate == 0 {
            return Err(MixError::ZeroRate);
        }
        let mut kept: Vec<ClipSource> = previous
            .map(|p| {
                p.tracks
                    .into_iter()
                    .flatten()
                    .filter(|c| c.reader.is_some())
                    .collect()
            })
            .unwrap_or_default();
        let mut stats = SourceStats::default();
        let mut tracks = Vec::new();
        for track in audio_tracks(seq) {
            let mut clips = Vec::new();
            for item in &track.items {
                let Some(clip) = item.clip() else { continue };
                if !clip.enabled {
                    continue;
                }
                stats.clips += 1;
                let Some(media_id) = clip.source.media() else {
                    stats.nested_clips += 1;
                    continue;
                };
                let Some(media) = project.media().get(media_id) else {
                    stats.missing_media += 1;
                    continue;
                };
                let reader_rate = clip.speed.constant_speed().and_then(|speed| {
                    let rate = (f64::from(rate) * f64::from(speed.den) / f64::from(speed.num))
                        .round() as u32;
                    READER_RATES.contains(&rate).then_some(rate)
                });
                let Some(reader_rate) = reader_rate else {
                    stats.unsupported_speed += 1;
                    continue;
                };
                let start = sample_of(item.range.start, rate)?;
                let end = sample_of(item.range.end()?, rate)?;
                let mut source = ClipSource {
                    item: Arc::clone(item),
                    path: PathBuf::from(&media.path),
                    start,
                    end,
                    reader_rate,
                    first_sample: sample_of(clip.source_start, reader_rate)?,
                    animated: clip.curve(ClipParam::GainDb).is_some(),
                    constant_gain: db_to_gain(clip.gain_db),
                    reader: None,
                    failed: false,
                };
                if let Some(i) = kept.iter().position(|k| k.key() == source.key()) {
                    source.reader = kept.swap_remove(i).reader;
                }
                clips.push(source);
            }
            tracks.push(clips);
        }
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

    pub(crate) fn stats(&self) -> SourceStats {
        self.stats
    }

    /// Opens the readers of clips that play within `from..to` and closes those of clips
    /// that ended before `from`, so a file is open only around the playhead.
    pub(crate) fn prepare(&mut self, from: i64, to: i64) {
        let layout = self.media_layout;
        for clip in self.tracks.iter_mut().flatten() {
            if clip.end <= from {
                clip.reader = None;
            } else if clip.start < to && clip.reader.is_none() && !clip.failed {
                let options = AudioOptions {
                    sample_rate: clip.reader_rate,
                    layout,
                    ..AudioOptions::default()
                };
                match AudioReader::open(&clip.path, options) {
                    Ok(reader) => clip.reader = Some(reader),
                    Err(error) => {
                        tracing::warn!(path = %clip.path.display(), %error, "audio clip did not open");
                        clip.failed = true;
                        self.stats.failed_opens += 1;
                    }
                }
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
                if clip.end <= position || clip.start >= end {
                    continue;
                }
                let from = clip.start.max(position);
                let to = clip.end.min(end);
                let n = (to - from) as usize;
                if !read_clip(clip, from, n, channels, &mut self.scratch, &mut self.stats) {
                    continue;
                }
                apply_gain(clip, from, n, channels, rate, &mut self.scratch);
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
/// the media has none). Returns false when the clip has no reader (silent).
fn read_clip(
    clip: &mut ClipSource,
    from: i64,
    n: usize,
    channels: usize,
    scratch: &mut [f32],
    stats: &mut SourceStats,
) -> bool {
    let Some(reader) = clip.reader.as_mut() else {
        return false;
    };
    for c in 0..channels {
        scratch[c * SCRATCH_FRAMES..c * SCRATCH_FRAMES + n].fill(0.0);
    }
    let want = clip.first_sample + (from - clip.start);
    // Audio that starts after the clip's first sample (a file whose audio begins late):
    // silence until it does.
    let first = reader.first_sample() as i64;
    let lead = (first - want).clamp(0, n as i64) as usize;
    let at = (want + lead as i64).max(0) as u64;
    if lead < n {
        let result = (|| {
            if reader.position() != at {
                reader.seek_to_sample(at)?;
            }
            let mut planes = scratch.chunks_mut(SCRATCH_FRAMES);
            let mut slices: [&mut [f32]; audio::mixer::MAX_CHANNELS] =
                std::array::from_fn(|_| planes.next().map_or(&mut [][..], |p| &mut p[lead..n]));
            reader.read(&mut slices[..channels])
        })();
        if let Err(error) = result {
            tracing::warn!(path = %clip.path.display(), %error, "audio clip read failed");
            stats.decode_errors += 1;
            clip.reader = None;
            clip.failed = true;
            return false;
        }
    }
    true
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
