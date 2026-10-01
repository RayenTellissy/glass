//! Silence detection: the stretches of a clip quieter than a threshold for at least a
//! minimum length.
//!
//! **Level.** RMS over a sliding window ([`SilenceSettings::window_ms`], 10 ms by
//! default) that advances 1 ms at a time, in dBFS as `20 log10(rms)` (a full-scale sine
//! is -3.01 dBFS on this scale), per channel. A window is quiet only when **every**
//! channel is below the threshold (the loudest channel decides), so a mono microphone
//! on one side of a stereo file is never taken for silence, and out-of-phase channels
//! cannot cancel as they would in a sum.
//!
//! **Decision.** Silence starts at the first quiet window and lasts while windows stay
//! at or below `threshold + hysteresis` (3 dB by default), so a floor hovering at the
//! threshold does not chatter. A silence covers its windows exactly: from the start of
//! its first quiet window to the end of its last one, so a sharp edge lands within the
//! 1 ms hop of where the sound starts or stops. Silences shorter than
//! [`SilenceSettings::min_ms`] (500 ms by default) are dropped, then each is shrunk by
//! [`SilenceSettings::padding_ms`] at either end (0 by default; the start and end of the
//! stream are not padded) so cutting silence can leave breathing room.
//!
//! **Timeline.** Samples come from [`AudioReader`] at [`SilenceSettings::sample_rate`]:
//! sample `n` is at `n / rate` seconds on the file's timeline, the zero video pts use,
//! with encoder priming already removed. Ranges are in those samples, so a stream that
//! starts after the video starts its first range at its own first sample, not at 0.
//!
//! **Why samples, not the cached waveform peaks.** Peaks hold RMS per 256 samples
//! (5.3 ms at 48 kHz), which would do for the level, but block-aligned 5.3 ms windows
//! cannot slide by 1 ms or honour another window length, and they are quantised to
//! i16. A decode pass costs little (hundreds of times realtime) and gives exact edges at
//! any settings.
//!
//! **Storage**: `silence-<p>-v1.json`, [`Silences`] (a few bytes per range).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::analysis::{AnalysisError, Control};
use crate::audio::{AudioOptions, AudioReader, ChannelLayout};

/// Bumped when the output or the algorithm changes.
pub const SILENCE_VERSION: u32 = 1;
const READ_FRAMES: usize = 8192;

/// How silence is found; part of the cache key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SilenceSettings {
    /// The project's sample rate; ranges are in samples at this rate.
    pub sample_rate: u32,
    /// dBFS RMS; windows below this are quiet.
    pub threshold_db: f32,
    /// Shorter silences are not reported.
    pub min_ms: u32,
    /// RMS window length.
    pub window_ms: u32,
    /// A silence ends only when a window rises more than this above the threshold.
    pub hysteresis_db: f32,
    /// Removed from both ends of each silence.
    pub padding_ms: u32,
}

impl Default for SilenceSettings {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            threshold_db: -40.0,
            min_ms: 500,
            window_ms: 10,
            hysteresis_db: 3.0,
            padding_ms: 0,
        }
    }
}

impl SilenceSettings {
    /// The canonical parameter string for the cache key.
    pub fn params(&self) -> String {
        format!(
            "rate={};t={:.2};min={};win={};hyst={:.2};pad={}",
            self.sample_rate,
            self.threshold_db,
            self.min_ms,
            self.window_ms.max(1),
            self.hysteresis_db.max(0.0),
            self.padding_ms
        )
    }
}

/// A silent stretch, in samples at [`Silences::sample_rate`] on the file's timeline:
/// `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Silence {
    pub start: u64,
    pub end: u64,
}

/// The silences of a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Silences {
    pub version: u32,
    pub sample_rate: u32,
    /// The audio stream's first and end sample on the file's timeline.
    pub first_sample: u64,
    pub end_sample: u64,
    pub ranges: Vec<Silence>,
}

impl Silences {
    /// The ranges in seconds on the file's timeline.
    pub fn seconds(&self) -> Vec<(f64, f64)> {
        let rate = f64::from(self.sample_rate.max(1));
        self.ranges
            .iter()
            .map(|s| (s.start as f64 / rate, s.end as f64 / rate))
            .collect()
    }
}

/// Streaming detector: feed planar samples, then [`Detector::finish`].
struct Detector {
    channels: usize,
    hop: usize,
    /// Hops per window.
    hops: usize,
    /// Mean square below which a window is quiet, and above which silence ends.
    enter: f64,
    leave: f64,
    /// Sample index of the first sample pushed.
    origin: u64,
    /// Sum of squares of the current, unfinished hop, per channel.
    partial: Vec<f64>,
    filled: usize,
    /// The last `hops` finished hops' sums, per channel (ring).
    ring: Vec<Vec<f64>>,
    finished_hops: u64,
    /// Start sample of the silence in progress.
    open: Option<u64>,
    /// End of the last quiet window of the silence in progress.
    open_end: u64,
    found: Vec<Silence>,
}

impl Detector {
    fn new(channels: usize, sample_rate: u32, origin: u64, settings: &SilenceSettings) -> Self {
        let hop = (sample_rate as usize / 1000).max(1);
        let hops = settings.window_ms.max(1) as usize;
        let power = |db: f32| 10f64.powf(f64::from(db) / 10.0);
        Self {
            channels,
            hop,
            hops,
            enter: power(settings.threshold_db),
            leave: power(settings.threshold_db + settings.hysteresis_db.max(0.0)),
            origin,
            partial: vec![0.0; channels],
            filled: 0,
            ring: vec![vec![0.0; hops]; channels],
            finished_hops: 0,
            open: None,
            open_end: 0,
            found: Vec::new(),
        }
    }

    fn push(&mut self, planes: &[&[f32]], frames: usize) {
        let mut i = 0;
        while i < frames {
            let take = (self.hop - self.filled).min(frames - i);
            for (c, plane) in planes.iter().enumerate().take(self.channels) {
                self.partial[c] += plane[i..i + take]
                    .iter()
                    .map(|&x| f64::from(x) * f64::from(x))
                    .sum::<f64>();
            }
            self.filled += take;
            i += take;
            if self.filled == self.hop {
                self.finish_hop();
            }
        }
    }

    fn finish_hop(&mut self) {
        let slot = (self.finished_hops % self.hops as u64) as usize;
        for c in 0..self.channels {
            self.ring[c][slot] = std::mem::take(&mut self.partial[c]);
        }
        self.filled = 0;
        self.finished_hops += 1;
        if self.finished_hops < self.hops as u64 {
            return;
        }
        let samples = (self.hop * self.hops) as f64;
        let loudest = self
            .ring
            .iter()
            .map(|r| r.iter().sum::<f64>() / samples)
            .fold(0.0, f64::max);
        let end = self.origin + self.finished_hops * self.hop as u64;
        let start = end - (self.hop * self.hops) as u64;
        match self.open {
            None if loudest < self.enter => {
                self.open = Some(start);
                self.open_end = end;
            }
            Some(_) if loudest <= self.leave => self.open_end = end,
            Some(open) => {
                self.found.push(Silence {
                    start: open,
                    end: self.open_end,
                });
                self.open = None;
            }
            None => {}
        }
    }

    /// The silences before the length and padding rules; a silence still open runs to
    /// `end`, the stream's end sample.
    fn finish(mut self, end: u64) -> Vec<Silence> {
        if let Some(open) = self.open.take() {
            self.found.push(Silence { start: open, end });
        }
        self.found
    }
}

/// Applies the minimum length and the padding; `first..end` is the stream.
fn finalise(
    found: Vec<Silence>,
    rate: u32,
    first: u64,
    end: u64,
    settings: &SilenceSettings,
) -> Vec<Silence> {
    let samples = |ms: u32| u64::from(ms) * u64::from(rate) / 1000;
    let (min, pad) = (samples(settings.min_ms), samples(settings.padding_ms));
    found
        .into_iter()
        .filter(|s| s.end - s.start >= min.max(1))
        .filter_map(|s| {
            let start = if s.start <= first {
                first
            } else {
                s.start + pad
            };
            let stop = if s.end >= end {
                end
            } else {
                s.end.saturating_sub(pad)
            };
            (stop > start).then_some(Silence { start, end: stop })
        })
        .collect()
}

/// Reads the best audio stream of `path` and finds its silences.
pub fn detect(
    path: &Path,
    settings: &SilenceSettings,
    control: &dyn Control,
) -> Result<Silences, AnalysisError> {
    let mut reader = AudioReader::open_for_source(
        path,
        AudioOptions {
            sample_rate: settings.sample_rate,
            ..AudioOptions::default()
        },
        |source, options| options.layout = ChannelLayout::for_channels(source.channels),
    )?;
    let channels = reader.channels();
    let first = reader.position();
    let expected = reader
        .source()
        .duration
        .map_or(0.0, |d| d.seconds() * f64::from(settings.sample_rate))
        .max(1.0);
    let mut planes = vec![vec![0f32; READ_FRAMES]; channels];
    let mut detector = Detector::new(channels, settings.sample_rate, first, settings);
    let mut end = first;
    loop {
        control.checkpoint()?;
        let frames = {
            let mut out: Vec<&mut [f32]> = planes.iter_mut().map(Vec::as_mut_slice).collect();
            reader.read(&mut out)?
        };
        if frames == 0 {
            break;
        }
        let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        detector.push(&views, frames);
        end += frames as u64;
        control.progress((end as f64 / expected).min(0.99) as f32);
    }
    let found = detector.finish(end);
    Ok(Silences {
        version: SILENCE_VERSION,
        sample_rate: settings.sample_rate,
        first_sample: first,
        end_sample: end,
        ranges: finalise(found, settings.sample_rate, first, end, settings),
    })
}

/// [`detect`] on samples already in memory, the first at sample 0 (tests, tools).
pub fn detect_samples(
    planes: &[&[f32]],
    sample_rate: u32,
    settings: &SilenceSettings,
) -> Vec<Silence> {
    let total = planes.first().map_or(0, |p| p.len()) as u64;
    let mut detector = Detector::new(planes.len(), sample_rate, 0, settings);
    detector.push(planes, total as usize);
    let found = detector.finish(total);
    finalise(found, sample_rate, 0, total, settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn ms(m: usize) -> usize {
        m * RATE as usize / 1000
    }

    fn tone(len: usize, level: f32) -> Vec<f32> {
        (0..len)
            .map(|i| level * (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / RATE as f32).sin())
            .collect()
    }

    fn near(found: &Silence, start: usize, end: usize) -> bool {
        found.start.abs_diff(ms(start) as u64) <= ms(1) as u64
            && found.end.abs_diff(ms(end) as u64) <= ms(1) as u64
    }

    #[test]
    fn edges_land_on_the_millisecond() {
        // 0.3 s tone, 0.8 s silence, 0.2 s tone, 0.3 s silence (too short), 0.2 s tone,
        // then 0.6 s of silence to the end.
        let mut x = tone(ms(300), 0.5);
        x.extend(vec![0.0; ms(800)]);
        x.extend(tone(ms(200), 0.5));
        x.extend(vec![0.0; ms(300)]);
        x.extend(tone(ms(200), 0.5));
        x.extend(vec![0.0; ms(600)]);
        let found = detect_samples(&[&x], RATE, &SilenceSettings::default());
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(near(&found[0], 300, 1100), "{found:?}");
        assert!(near(&found[1], 1800, 2400), "{found:?}");
    }

    #[test]
    fn one_loud_channel_is_not_silence_and_padding_shrinks() {
        let left = vec![0.0; ms(2000)];
        let right = tone(ms(2000), 0.1);
        assert!(detect_samples(&[&left, &right], RATE, &SilenceSettings::default()).is_empty());

        let mut x = tone(ms(200), 0.5);
        x.extend(vec![0.0; ms(1000)]);
        x.extend(tone(ms(200), 0.5));
        let settings = SilenceSettings {
            padding_ms: 100,
            ..SilenceSettings::default()
        };
        let found = detect_samples(&[&x], RATE, &settings);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(near(&found[0], 300, 1100), "{found:?}");
    }

    #[test]
    fn the_stream_edges_are_not_padded() {
        let mut x = vec![0.0; ms(700)];
        x.extend(tone(ms(200), 0.5));
        x.extend(vec![0.0; ms(700)]);
        let settings = SilenceSettings {
            padding_ms: 100,
            ..SilenceSettings::default()
        };
        let found = detect_samples(&[&x], RATE, &settings);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(near(&found[0], 0, 600), "{found:?}");
        assert!(near(&found[1], 1000, 1600), "{found:?}");
    }

    #[test]
    fn hysteresis_holds_silence_through_a_floor_near_the_threshold() {
        // Floor alternating between -42 and -39 dBFS RMS every 50 ms.
        let a = 10f32.powf(-42.0 / 20.0) * std::f32::consts::SQRT_2;
        let b = 10f32.powf(-39.0 / 20.0) * std::f32::consts::SQRT_2;
        let mut x = Vec::new();
        for i in 0..20 {
            x.extend(tone(ms(50), if i % 2 == 0 { a } else { b }));
        }
        let found = detect_samples(&[&x], RATE, &SilenceSettings::default());
        assert_eq!(found.len(), 1, "{found:?}");
        let none = SilenceSettings {
            hysteresis_db: 0.0,
            ..SilenceSettings::default()
        };
        assert!(detect_samples(&[&x], RATE, &none).is_empty());
    }
}
