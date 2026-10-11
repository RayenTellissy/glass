//! Loudness after ITU-R BS.1770-4 and EBU R128: integrated loudness, loudness range,
//! true peak, and short-term and momentary loudness curves for the UI.
//!
//! The measurement is the `ebur128` crate (a Rust port of libebur128, MIT), which
//! implements BS.1770-4 as EBU R128 uses it: K-weighting (the high-shelf and high-pass
//! pre-filters, recomputed for the sample rate), mean square per channel over 400 ms
//! momentary blocks overlapping by 75 %, channel weights (1.0 for left, right and
//! centre, 1.41 for the surrounds, LFE excluded), the absolute gate at -70 LUFS and the
//! relative gate 10 LU below for integrated loudness; loudness range (EBU Tech 3342) from
//! 3 s short-term blocks every 100 ms gated at -70 LUFS and 20 LU below, as the spread
//! between their 10th and 95th percentiles; true peak from 4x oversampling (2x from
//! 96 kHz) with a 49-tap interpolation filter per phase.
//!
//! Glass reads the best audio stream through [`AudioReader`] at the stream's own rate
//! (no resampling before the meter) in its own layout, mono, stereo or 5.1 (wider
//! layouts fold to 5.1 by `AudioReader`'s rules), opening the file once. Mono is
//! measured as one channel of weight 1, as FFmpeg's `ebur128` filter does; stereo as
//! left and right; 5.1 as FL, FR, FC, LFE (unused), SL, SR.
//!
//! The curves have one value every 100 ms from the stream's first sample: value `i` is
//! the window (3 s short-term, 400 ms momentary) ending at `(i + 1) * 100 ms`; `None`
//! (JSON `null`) before the first full window and while the window is below -70 LUFS.
//!
//! **Storage**: `loudness-<p>-v1.json`, [`Loudness`] (about 500 KB per hour of curves).

use std::path::Path;

use ebur128::{Channel, EbuR128, Mode};
use serde::{Deserialize, Serialize};

use crate::analysis::{AnalysisError, Control};
use crate::audio::{AudioOptions, AudioReader, ChannelLayout};

/// Bumped when the output or the algorithm changes.
pub const LOUDNESS_VERSION: u32 = 1;
/// Curve step.
pub const CURVE_STEP_MS: u32 = 100;
const READ_FRAMES: usize = 8192;
/// Below this a curve point is `None`: the absolute gate.
const GATE_LUFS: f64 = -70.0;

/// Loudness has no settings yet beyond the algorithm; the key names the standard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoudnessSettings;

impl LoudnessSettings {
    /// The canonical parameter string for the cache key.
    pub fn params(&self) -> String {
        format!("bs1770-4;step={CURVE_STEP_MS}")
    }
}

/// The loudness of a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loudness {
    pub version: u32,
    /// Gated integrated loudness, LUFS; `None` when everything is below the -70 LUFS gate.
    pub integrated_lufs: Option<f64>,
    /// Loudness range, LU.
    pub range_lu: f64,
    /// Highest true peak over channels, dBTP; `None` for digital silence.
    pub true_peak_dbtp: Option<f64>,
    /// Highest sample peak over channels, dBFS.
    pub sample_peak_dbfs: Option<f64>,
    /// Highest short-term and momentary loudness, LUFS.
    pub max_short_term_lufs: Option<f64>,
    pub max_momentary_lufs: Option<f64>,
    /// Short-term (3 s) loudness every [`CURVE_STEP_MS`] ms; value `i` is the window
    /// ending at `(i + 1) * step` after the first sample.
    pub short_term: Vec<Option<f32>>,
    /// Momentary (400 ms) loudness on the same steps.
    pub momentary: Vec<Option<f32>>,
    /// The measured rate (the stream's own) and channel count.
    pub sample_rate: u32,
    pub channels: usize,
    /// The stream's first sample on the file's timeline, at `sample_rate`: where the
    /// curves start.
    pub first_sample: u64,
}

fn db(x: f64) -> Option<f64> {
    (x > 0.0).then(|| 20.0 * x.log10())
}

/// A level worth reporting: finite and not digital silence.
fn finite(x: f64) -> Option<f64> {
    (x.is_finite() && x > -120.0).then_some(x)
}

fn gated(x: f64) -> Option<f32> {
    finite(x).filter(|&l| l >= GATE_LUFS).map(|l| l as f32)
}

fn meter_error(e: ebur128::Error) -> AnalysisError {
    AnalysisError::Encode(format!("loudness meter: {e:?}"))
}

/// Feeds planar samples to a meter and records the curves.
pub struct Meter {
    meter: EbuR128,
    step: usize,
    /// Frames fed since the last curve point.
    since: usize,
    short_term: Vec<Option<f32>>,
    momentary: Vec<Option<f32>>,
    max_short: f64,
    max_momentary: f64,
    rate: u32,
    channels: usize,
    first_sample: u64,
}

impl Meter {
    pub fn new(channels: usize, rate: u32) -> Result<Self, AnalysisError> {
        let modes = Mode::I | Mode::LRA | Mode::S | Mode::M | Mode::TRUE_PEAK | Mode::SAMPLE_PEAK;
        let mut meter = EbuR128::new(channels as u32, rate, modes).map_err(meter_error)?;
        let map: &[Channel] = match channels {
            1 => &[Channel::Center],
            2 => &[Channel::Left, Channel::Right],
            _ => &[
                Channel::Left,
                Channel::Right,
                Channel::Center,
                Channel::Unused,
                Channel::LeftSurround,
                Channel::RightSurround,
            ],
        };
        for (c, &channel) in map.iter().enumerate().take(channels) {
            meter.set_channel(c as u32, channel).map_err(meter_error)?;
        }
        Ok(Self {
            meter,
            step: (rate as usize * CURVE_STEP_MS as usize / 1000).max(1),
            since: 0,
            short_term: Vec::new(),
            momentary: Vec::new(),
            max_short: f64::NEG_INFINITY,
            max_momentary: f64::NEG_INFINITY,
            rate,
            channels,
            first_sample: 0,
        })
    }

    /// Adds `frames` samples of each plane.
    pub fn push(&mut self, planes: &[&[f32]], frames: usize) -> Result<(), AnalysisError> {
        let mut at = 0;
        let mut chunk: Vec<&[f32]> = Vec::with_capacity(planes.len());
        while at < frames {
            let take = (self.step - self.since).min(frames - at);
            chunk.clear();
            chunk.extend(planes.iter().map(|p| &p[at..at + take]));
            self.meter
                .add_frames_planar_f32(&chunk)
                .map_err(meter_error)?;
            self.since += take;
            at += take;
            if self.since == self.step {
                self.since = 0;
                self.point()?;
            }
        }
        Ok(())
    }

    /// Records the curve values of the windows ending now.
    fn point(&mut self) -> Result<(), AnalysisError> {
        let elapsed_ms = (self.short_term.len() + 1) * CURVE_STEP_MS as usize;
        let momentary = if elapsed_ms >= 400 {
            self.meter.loudness_momentary().map_err(meter_error)?
        } else {
            f64::NEG_INFINITY
        };
        let short = if elapsed_ms >= 3000 {
            self.meter.loudness_shortterm().map_err(meter_error)?
        } else {
            f64::NEG_INFINITY
        };
        self.max_momentary = self.max_momentary.max(momentary);
        self.max_short = self.max_short.max(short);
        self.momentary.push(gated(momentary));
        self.short_term.push(gated(short));
        Ok(())
    }

    pub fn finish(self) -> Result<Loudness, AnalysisError> {
        let integrated = self.meter.loudness_global().map_err(meter_error)?;
        let range = self.meter.loudness_range().map_err(meter_error)?;
        let mut true_peak = 0f64;
        let mut sample_peak = 0f64;
        for c in 0..self.channels as u32 {
            true_peak = true_peak.max(self.meter.true_peak(c).map_err(meter_error)?);
            sample_peak = sample_peak.max(self.meter.sample_peak(c).map_err(meter_error)?);
        }
        Ok(Loudness {
            version: LOUDNESS_VERSION,
            integrated_lufs: finite(integrated).filter(|&l| l >= GATE_LUFS),
            range_lu: if range.is_finite() { range } else { 0.0 },
            true_peak_dbtp: db(true_peak),
            sample_peak_dbfs: db(sample_peak),
            max_short_term_lufs: finite(self.max_short),
            max_momentary_lufs: finite(self.max_momentary),
            short_term: self.short_term,
            momentary: self.momentary,
            sample_rate: self.rate,
            channels: self.channels,
            first_sample: self.first_sample,
        })
    }
}

/// Measures planar samples already in memory (tests, tools).
pub fn measure_samples(planes: &[&[f32]], rate: u32) -> Result<Loudness, AnalysisError> {
    let mut meter = Meter::new(planes.len(), rate)?;
    meter.push(planes, planes.first().map_or(0, |p| p.len()))?;
    meter.finish()
}

/// Reads the best audio stream of `path` and measures its loudness.
pub fn measure(
    path: &Path,
    _settings: &LoudnessSettings,
    control: &dyn Control,
) -> Result<Loudness, AnalysisError> {
    let mut reader = AudioReader::open_for_source(path, AudioOptions::default(), |source, o| {
        o.sample_rate = source.sample_rate.max(8_000);
        o.layout = ChannelLayout::for_channels(source.channels);
    })?;
    let rate = reader.sample_rate();
    let channels = reader.channels();
    let expected = reader
        .source()
        .duration
        .map_or(0.0, |d| d.seconds() * f64::from(rate))
        .max(1.0);
    let mut planes = vec![vec![0f32; READ_FRAMES]; channels];
    let mut meter = Meter::new(channels, rate)?;
    meter.first_sample = reader.position();
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
        let views: Vec<&[f32]> = planes.iter().map(|p| &p[..frames]).collect();
        meter.push(&views, frames)?;
        total += frames as u64;
        control.progress((total as f64 / expected).min(0.99) as f32);
    }
    meter.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// 1 kHz sine segments `(dBFS peak, seconds)`, the same in every channel.
    fn sine(segments: &[(f64, f64)], channels: usize) -> Vec<Vec<f32>> {
        let mut x = Vec::new();
        let mut n = 0usize;
        for &(level, seconds) in segments {
            let amp = 10f64.powf(level / 20.0);
            for _ in 0..(seconds * f64::from(RATE)).round() as usize {
                let t = n as f64 / f64::from(RATE);
                x.push((amp * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()) as f32);
                n += 1;
            }
        }
        vec![x; channels]
    }

    fn run(planes: &[Vec<f32>]) -> Loudness {
        let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        measure_samples(&views, RATE).expect("meter")
    }

    fn integrated(segments: &[(f64, f64)]) -> f64 {
        run(&sine(segments, 2)).integrated_lufs.expect("level")
    }

    /// EBU Tech 3341 cases 1 to 5 (stereo 1 kHz sines; expected -23 or -33 ±0.1 LUFS).
    #[test]
    fn tech_3341_integrated_and_gating() {
        let cases: [(&[(f64, f64)], f64); 5] = [
            (&[(-23.0, 20.0)], -23.0),
            (&[(-33.0, 20.0)], -33.0),
            (&[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)], -23.0),
            (
                &[
                    (-72.0, 10.0),
                    (-36.0, 10.0),
                    (-23.0, 60.0),
                    (-36.0, 10.0),
                    (-72.0, 10.0),
                ],
                -23.0,
            ),
            (&[(-26.0, 20.0), (-20.0, 20.1), (-26.0, 20.0)], -23.0),
        ];
        for (i, (segments, expected)) in cases.iter().enumerate() {
            let got = integrated(segments);
            assert!((got - expected).abs() <= 0.1, "case {}: {got} LUFS", i + 1);
        }
    }

    /// EBU Tech 3342 cases 1 to 4 (loudness range ±1 LU).
    #[test]
    fn tech_3342_loudness_range() {
        let cases: [(&[(f64, f64)], f64); 4] = [
            (&[(-20.0, 20.0), (-30.0, 20.0)], 10.0),
            (&[(-20.0, 20.0), (-15.0, 20.0)], 5.0),
            (&[(-40.0, 20.0), (-20.0, 20.0)], 20.0),
            (
                &[
                    (-50.0, 20.0),
                    (-35.0, 20.0),
                    (-20.0, 20.0),
                    (-35.0, 20.0),
                    (-50.0, 20.0),
                ],
                15.0,
            ),
        ];
        for (i, (segments, expected)) in cases.iter().enumerate() {
            let got = run(&sine(segments, 2)).range_lu;
            assert!((got - expected).abs() <= 1.0, "case {}: {got} LU", i + 1);
        }
    }

    /// A 12 kHz sine at 45 degrees: samples reach only 0.707 of the peak, the true peak
    /// is the amplitude (-6.02 dBTP within +0.2 / -0.4 dB, Tech 3341's tolerance).
    #[test]
    fn true_peak_sees_between_samples() {
        let amp = 0.5f64;
        let x: Vec<f32> = (0..RATE as usize * 2)
            .map(|n| {
                let phase = std::f64::consts::FRAC_PI_4 + std::f64::consts::FRAC_PI_2 * n as f64;
                (amp * phase.sin()) as f32
            })
            .collect();
        let l = run(&[x.clone(), x]);
        let tp = l.true_peak_dbtp.expect("peak");
        let sp = l.sample_peak_dbfs.expect("peak");
        assert!((-6.42..=-5.82).contains(&tp), "true peak {tp}");
        assert!((sp + 9.03).abs() < 0.05, "sample peak {sp}");
    }

    #[test]
    fn curves_have_a_point_every_100_ms() {
        let l = run(&sine(&[(-23.0, 10.0)], 1));
        assert_eq!(l.short_term.len(), 100);
        assert_eq!(l.momentary.len(), 100);
        assert!(l.short_term[..29].iter().all(Option::is_none));
        assert!(l.momentary[..3].iter().all(Option::is_none));
        // Mono at -23 dBFS peak is 3 dB below the same sine in two channels.
        let short = l.short_term[99].expect("level");
        let momentary = l.momentary[3].expect("level");
        assert!((f64::from(short) + 26.0).abs() < 0.1, "{short}");
        assert!((f64::from(momentary) + 26.0).abs() < 0.1, "{momentary}");
        let silent = run(&[vec![0.0; RATE as usize * 4]]);
        assert!(silent.integrated_lufs.is_none());
        assert!(
            silent
                .short_term
                .iter()
                .chain(&silent.momentary)
                .all(Option::is_none)
        );
    }
}
