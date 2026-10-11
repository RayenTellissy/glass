//! Waveform peaks for the timeline: per channel, the minimum, maximum and RMS of every
//! block of samples, at five zoom levels.
//!
//! | Level | Samples per peak | Peaks per second at 48 kHz | Timeline use |
//! | --- | --- | --- | --- |
//! | 0 | 256 | 187.5 | zoomed in to a few frames per point |
//! | 1 | 1024 | 46.9 | |
//! | 2 | 4096 | 11.7 | the default zoom |
//! | 3 | 16384 | 2.9 | |
//! | 4 | 65536 | 0.73 | a whole hour on screen |
//!
//! A mipmap chain with a factor of 4: each level is built from the one below (minimum of
//! the minima, maximum of the maxima, RMS from the summed squares), so coarser levels are
//! exactly consistent with finer ones and the samples are read once. The renderer picks
//! the coarsest level with at most as many samples per peak as it has samples per pixel
//! ([`Peaks::level_for`]), so it never draws more than four peaks per pixel. Below 256
//! samples per pixel (very deep zoom) it reads samples directly.
//!
//! Samples come from [`AudioReader`] at the project rate (48 kHz by default), in the
//! source's own layout where [`ChannelLayout`] has it (mono, stereo, 5.1; wider layouts
//! are folded to 5.1 by its rules).
//!
//! **Storage**: one little-endian file, `<stem>.bin`: a 32-byte header (`GLPK`, format
//! version, sample rate, channels, level count, total samples), a table of
//! (samples per peak u32, peak count u64) per level, then each level's peaks, channels
//! interleaved per peak, as three `i16` each: min and max quantised as
//! `round(clamp(x, -1, 1) * 32767)` (resolution -90 dBFS, so quiet passages still draw)
//! and RMS on the same scale. 6 bytes per peak per channel; an hour of stereo at 48 kHz
//! is about 10.8 MB, all levels (level 0 is 3/4 of it), against 1.4 GB of f32 samples.
//! i8 would halve that but loses everything below -42 dBFS.

use std::ops::Range;
use std::path::Path;

use crate::analysis::{AnalysisError, Control};
use crate::audio::{AudioOptions, AudioReader, ChannelLayout};
use crate::probe::probe;

/// Samples per peak at each level, finest first.
pub const LEVELS: [u32; 5] = [256, 1024, 4096, 16384, 65536];
/// Bumped when the output or the algorithm changes.
pub const PEAKS_VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"GLPK";
const HEADER: usize = 32;
const TABLE_ENTRY: usize = 12;
const READ_FRAMES: usize = 8192;

/// How peaks are made; part of the cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeakSettings {
    /// The project's sample rate.
    pub sample_rate: u32,
    /// `None`: the source's layout (mono, stereo or 5.1).
    pub layout: Option<ChannelLayout>,
}

impl Default for PeakSettings {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            layout: None,
        }
    }
}

impl PeakSettings {
    /// The canonical parameter string for the cache key.
    pub fn params(&self) -> String {
        format!(
            "rate={};layout={};spp=256x4",
            self.sample_rate,
            self.layout.map_or("source", ChannelLayout::name)
        )
    }
}

/// One channel's block summary, on the i16 scale (32767 = full scale).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Peak {
    pub min: i16,
    pub max: i16,
    pub rms: i16,
}

impl Peak {
    pub fn min_f32(self) -> f32 {
        f32::from(self.min) / 32767.0
    }

    pub fn max_f32(self) -> f32 {
        f32::from(self.max) / 32767.0
    }

    pub fn rms_f32(self) -> f32 {
        f32::from(self.rms) / 32767.0
    }
}

/// One zoom level: peaks with channels interleaved (`index * channels + channel`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeakLevel {
    pub samples_per_peak: u32,
    pub peaks: Vec<Peak>,
}

/// Peaks of a range: `data[(i - first) * channels + channel]` is peak `i`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeakSlice<'a> {
    /// Index of the first peak in the level.
    pub first: u64,
    pub samples_per_peak: u32,
    pub channels: usize,
    pub data: &'a [Peak],
}

impl PeakSlice<'_> {
    pub fn len(&self) -> usize {
        self.data.len() / self.channels.max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Peak `i` of the slice (0-based) for `channel`.
    pub fn get(&self, i: usize, channel: usize) -> Option<Peak> {
        if channel >= self.channels {
            return None;
        }
        self.data.get(i * self.channels + channel).copied()
    }
}

/// Waveform peaks of an audio stream at every level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peaks {
    pub sample_rate: u32,
    pub channels: usize,
    /// Samples per channel in the stream (at `sample_rate`).
    pub total_samples: u64,
    /// Finest first ([`LEVELS`]).
    pub levels: Vec<PeakLevel>,
}

impl Peaks {
    /// The coarsest level whose peaks cover at most `samples_per_pixel` samples (level 0
    /// when even that is coarser than asked).
    pub fn level_for(&self, samples_per_pixel: f64) -> usize {
        self.levels
            .iter()
            .rposition(|l| f64::from(l.samples_per_peak) <= samples_per_pixel)
            .unwrap_or(0)
    }

    /// The peaks of `level` covering the sample range `range` (at `sample_rate`), clamped
    /// to the stream: from the peak holding `range.start` to the one holding
    /// `range.end - 1`. Empty for an unknown level or an empty range.
    pub fn peaks(&self, level: usize, range: Range<u64>) -> PeakSlice<'_> {
        let channels = self.channels;
        let Some(l) = self.levels.get(level) else {
            return PeakSlice {
                first: 0,
                samples_per_peak: 0,
                channels,
                data: &[],
            };
        };
        let spp = u64::from(l.samples_per_peak);
        let count = (l.peaks.len() / channels.max(1)) as u64;
        let first = (range.start / spp).min(count);
        let end = if range.end > range.start {
            range.end.div_ceil(spp).min(count)
        } else {
            first
        };
        PeakSlice {
            first,
            samples_per_peak: l.samples_per_peak,
            channels,
            data: &l.peaks[(first as usize * channels)..(end as usize * channels)],
        }
    }

    /// The file format described in the module docs.
    pub fn to_bytes(&self) -> Vec<u8> {
        let peaks: usize = self.levels.iter().map(|l| l.peaks.len()).sum();
        let mut out = Vec::with_capacity(HEADER + self.levels.len() * TABLE_ENTRY + peaks * 6);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&PEAKS_VERSION.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&(self.channels as u16).to_le_bytes());
        out.extend_from_slice(&(self.levels.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.total_samples.to_le_bytes());
        out.resize(HEADER, 0);
        for level in &self.levels {
            out.extend_from_slice(&level.samples_per_peak.to_le_bytes());
            let count = (level.peaks.len() / self.channels.max(1)) as u64;
            out.extend_from_slice(&count.to_le_bytes());
        }
        for level in &self.levels {
            for peak in &level.peaks {
                out.extend_from_slice(&peak.min.to_le_bytes());
                out.extend_from_slice(&peak.max.to_le_bytes());
                out.extend_from_slice(&peak.rms.to_le_bytes());
            }
        }
        out
    }

    /// Parses [`Self::to_bytes`]; `None` for another version or a damaged file.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let u16_at = |at: usize| -> Option<u16> {
            Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
        };
        let u32_at = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        let u64_at = |at: usize| -> Option<u64> {
            Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
        };
        if bytes.get(..4)? != MAGIC || u32_at(4)? != PEAKS_VERSION {
            return None;
        }
        let sample_rate = u32_at(8)?;
        let channels = usize::from(u16_at(12)?);
        let level_count = usize::from(u16_at(14)?);
        let total_samples = u64_at(16)?;
        if channels == 0 {
            return None;
        }
        let mut at = HEADER + level_count * TABLE_ENTRY;
        let mut levels = Vec::with_capacity(level_count);
        for l in 0..level_count {
            let entry = HEADER + l * TABLE_ENTRY;
            let samples_per_peak = u32_at(entry)?;
            let count = usize::try_from(u64_at(entry + 4)?).ok()?;
            let values = count.checked_mul(channels)?;
            let len = values.checked_mul(6)?;
            let data = bytes.get(at..at.checked_add(len)?)?;
            let peaks = data
                .as_chunks::<6>()
                .0
                .iter()
                .map(|p| Peak {
                    min: i16::from_le_bytes([p[0], p[1]]),
                    max: i16::from_le_bytes([p[2], p[3]]),
                    rms: i16::from_le_bytes([p[4], p[5]]),
                })
                .collect();
            levels.push(PeakLevel {
                samples_per_peak,
                peaks,
            });
            at += len;
        }
        (at == bytes.len()).then_some(Self {
            sample_rate,
            channels,
            total_samples,
            levels,
        })
    }
}

fn quantise(x: f64) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// A block being summarised, for one channel.
#[derive(Clone, Copy, Debug)]
struct Acc {
    min: f32,
    max: f32,
    sum_squares: f64,
    samples: u64,
}

impl Default for Acc {
    fn default() -> Self {
        Self {
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            sum_squares: 0.0,
            samples: 0,
        }
    }
}

impl Acc {
    fn peak(&self) -> Peak {
        if self.samples == 0 {
            return Peak::default();
        }
        Peak {
            min: quantise(f64::from(self.min)),
            max: quantise(f64::from(self.max)),
            rms: quantise((self.sum_squares / self.samples as f64).sqrt()),
        }
    }

    fn merge(&mut self, other: &Acc) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.sum_squares += other.sum_squares;
        self.samples += other.samples;
    }
}

/// Builds all levels in one pass: level 0 from samples, each level above from four
/// finished peaks of the one below.
struct Builder {
    channels: usize,
    /// Per level, per channel.
    acc: Vec<Vec<Acc>>,
    /// Blocks absorbed by each level's current peak (samples for level 0).
    filled: Vec<u64>,
    out: Vec<Vec<Peak>>,
}

impl Builder {
    fn new(channels: usize) -> Self {
        Self {
            channels,
            acc: vec![vec![Acc::default(); channels]; LEVELS.len()],
            filled: vec![0; LEVELS.len()],
            out: vec![Vec::new(); LEVELS.len()],
        }
    }

    fn push(&mut self, planes: &[Vec<f32>], frames: usize) {
        let spp = u64::from(LEVELS[0]);
        let mut i = 0;
        while i < frames {
            let take = ((spp - self.filled[0]) as usize).min(frames - i);
            for (c, plane) in planes.iter().enumerate().take(self.channels) {
                let acc = &mut self.acc[0][c];
                for &x in &plane[i..i + take] {
                    acc.min = acc.min.min(x);
                    acc.max = acc.max.max(x);
                    acc.sum_squares += f64::from(x) * f64::from(x);
                }
                acc.samples += take as u64;
            }
            self.filled[0] += take as u64;
            i += take;
            if self.filled[0] == spp {
                self.complete(0);
            }
        }
    }

    /// Emits level `l`'s current peak and feeds it to the level above.
    fn complete(&mut self, l: usize) {
        let finished = std::mem::replace(&mut self.acc[l], vec![Acc::default(); self.channels]);
        self.filled[l] = 0;
        self.out[l].extend(finished.iter().map(Acc::peak));
        if l + 1 < LEVELS.len() {
            for (up, acc) in self.acc[l + 1].iter_mut().zip(&finished) {
                up.merge(acc);
            }
            self.filled[l + 1] += 1;
            if self.filled[l + 1] == u64::from(LEVELS[l + 1] / LEVELS[l]) {
                self.complete(l + 1);
            }
        }
    }

    /// Emits the partial peaks at the end of the stream, finest first.
    fn finish(mut self) -> Vec<Vec<Peak>> {
        for l in 0..LEVELS.len() {
            if self.filled[l] > 0 {
                self.complete(l);
            }
        }
        self.out
    }
}

/// Reads the best audio stream of `path` and computes its peaks at every level.
pub fn generate(
    path: &Path,
    settings: &PeakSettings,
    control: &dyn Control,
) -> Result<Peaks, AnalysisError> {
    let info = probe(path)?;
    let layout =
        settings
            .layout
            .unwrap_or_else(|| match info.audio().map_or(2, |(_, a)| a.channels) {
                1 => ChannelLayout::Mono,
                2 => ChannelLayout::Stereo,
                _ => ChannelLayout::Surround51,
            });
    let mut reader = AudioReader::open(
        path,
        AudioOptions {
            sample_rate: settings.sample_rate,
            layout,
            ..AudioOptions::default()
        },
    )?;
    let channels = reader.channels();
    let expected = info
        .duration
        .map_or(0.0, |d| d.seconds() * f64::from(settings.sample_rate))
        .max(1.0);
    let mut planes = vec![vec![0f32; READ_FRAMES]; channels];
    let mut builder = Builder::new(channels);
    let mut total: u64 = 0;
    loop {
        control.checkpoint()?;
        let frames = {
            let mut out: Vec<&mut [f32]> = planes.iter_mut().map(Vec::as_mut_slice).collect();
            reader.read(&mut out)?
        };
        if frames == 0 {
            break;
        }
        builder.push(&planes, frames);
        total += frames as u64;
        control.progress((total as f64 / expected).min(0.99) as f32);
    }
    let levels = builder
        .finish()
        .into_iter()
        .zip(LEVELS)
        .map(|(peaks, samples_per_peak)| PeakLevel {
            samples_per_peak,
            peaks,
        })
        .collect();
    Ok(Peaks {
        sample_rate: settings.sample_rate,
        channels,
        total_samples: total,
        levels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(samples: &[Vec<f32>]) -> Peaks {
        let channels = samples.len();
        let mut builder = Builder::new(channels);
        let len = samples[0].len();
        for start in (0..len).step_by(1000) {
            let end = (start + 1000).min(len);
            let planes: Vec<Vec<f32>> = samples.iter().map(|c| c[start..end].to_vec()).collect();
            builder.push(&planes, end - start);
        }
        Peaks {
            sample_rate: 48_000,
            channels,
            total_samples: len as u64,
            levels: builder
                .finish()
                .into_iter()
                .zip(LEVELS)
                .map(|(peaks, samples_per_peak)| PeakLevel {
                    samples_per_peak,
                    peaks,
                })
                .collect(),
        }
    }

    #[test]
    fn counts_include_the_partial_last_peak() {
        let len = 70_000;
        let peaks = build(&[vec![0.25; len], vec![-0.5; len]]);
        for level in &peaks.levels {
            let spp = level.samples_per_peak as usize;
            assert_eq!(level.peaks.len(), len.div_ceil(spp) * 2);
            assert_eq!(level.peaks[0].max, quantise(0.25));
            assert_eq!(level.peaks[1].min, quantise(-0.5));
            assert_eq!(level.peaks[1].rms, quantise(0.5));
        }
    }

    #[test]
    fn bytes_round_trip_and_reject_damage() {
        let samples: Vec<f32> = (0..100_000).map(|i| ((i as f32) * 0.01).sin()).collect();
        let peaks = build(&[samples]);
        let bytes = peaks.to_bytes();
        assert_eq!(Peaks::from_bytes(&bytes), Some(peaks));
        assert_eq!(Peaks::from_bytes(&bytes[..bytes.len() - 1]), None);
        let mut other = bytes.clone();
        other[4] = 99;
        assert_eq!(Peaks::from_bytes(&other), None);
    }

    #[test]
    fn lookup_clamps_and_picks_levels() {
        let peaks = build(&[vec![0.1; 10_000]]);
        assert_eq!(peaks.level_for(100.0), 0);
        assert_eq!(peaks.level_for(1024.0), 1);
        assert_eq!(peaks.level_for(5000.0), 2);
        assert_eq!(peaks.level_for(1e9), 4);
        let slice = peaks.peaks(0, 300..1000);
        assert_eq!((slice.first, slice.len()), (1, 3));
        let slice = peaks.peaks(0, 9_000..1_000_000);
        assert_eq!((slice.first, slice.len()), (35, 5));
        assert!(peaks.peaks(0, 5..5).is_empty());
        assert!(peaks.peaks(9, 0..100).is_empty());
    }
}
