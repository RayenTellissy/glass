//! Tempo and beat detection for snapping edits to music.
//!
//! **Onset strength.** The best audio stream is read as mono at its own sample rate (no
//! resampling), from one open of the file. A short-time Fourier transform (Hann window
//! of about 21 ms, 1024 samples at 44.1 or 48 kHz, hop a quarter of that: 5.3 ms at
//! 48 kHz) gives log-compressed magnitudes `ln(1 + 100 |X|)` up to 11 kHz. In each of
//! three bands (below 200 Hz, 200 Hz to 2 kHz, 2 to 11 kHz) the flux of a frame is the
//! sum of the magnitudes' increases over the previous frame (half-wave rectified
//! spectral flux), minus a 0.5 s moving average, clipped at zero and scaled to unit
//! standard deviation; the onset strength is the sum of the bands, scaled to unit
//! standard deviation. Normalising per band keeps broadband noise (hats, snares), which
//! rises in hundreds of bins at once, from drowning a kick, which rises in a few: a kick
//! and a snare then weigh about the same, and a kick (low and mid bands) outweighs an
//! off-beat hat (high band), so beats land on the kicks.
//!
//! **Tempo.** The autocorrelation of the onset strength (normalised per lag), over lags
//! between 60 and 200 BPM, weighted by a broad log-normal prior centred on 120 BPM (one
//! octave standard deviation) that only breaks near-ties, picks a period `P` (refined
//! between lags by a parabola). Octave errors: a steady pulse also correlates at `2 P`,
//! so while the half period is still in range and its autocorrelation is at least 0.8
//! of `P`'s (every beat is split by an equally strong pulse), the half period wins. A
//! kick and snare pattern at 174 BPM is therefore not read as 87 (0.87 of `P`), while a
//! groove at 90 BPM with softer eighth-note hats stays at 90 (0.47). Some music is
//! genuinely ambiguous between the two. **Confidence** is the onset strength's
//! autocovariance at `P` over its variance: near 1 for a steady pulse, near 0 without
//! one (white noise: under 0.2); a drifting tempo spreads the correlation over
//! neighbouring lags and lowers it (0.3 on the 116 to 124 BPM drift fixture).
//!
//! **Beats.** Dynamic programming after Ellis (2007): each frame's score is its
//! smoothed onset strength plus the best earlier score between `P / 2` and `2 P` back,
//! penalised by `100 (ln(interval / P))²`, so beats follow onsets and tempo drift while
//! preferring the period. Back-tracking from the last strong score gives the beats; weak
//! leading and trailing beats (below half the RMS onset strength at beats) are dropped.
//! Each beat is moved to the strongest onset within 2 frames and interpolated to a
//! fraction of a frame, and the analysis latency is subtracted: the flux of an onset
//! peaks in the frame centred 0.140 of a window before it (3.0 ms at 48 kHz, measured
//! on the click fixtures, whose onsets are known; see [`LATENCY_FRACTION`]).
//!
//! The reported tempo is the slope of a least-squares line through the beats (the mean
//! tempo when it drifts); `tempo_curve` holds the local tempo of each inter-beat
//! interval. Beat times are seconds on the file's timeline. Downbeats (bar starts) are
//! not estimated.
//!
//! **Memory**: three band fluxes per frame until the end of the stream, about 8 MB per
//! hour at 48 kHz.
//!
//! **Storage**: `beats-<p>-v1.json`, [`Beats`].

use std::path::Path;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

use crate::analysis::{AnalysisError, Control};
use crate::audio::{AudioOptions, AudioReader, ChannelLayout};

/// Bumped when the output or the algorithm changes.
pub const BEATS_VERSION: u32 = 1;
const READ_FRAMES: usize = 8192;
/// Upper edges of the onset bands, Hz.
const BAND_EDGES: [f64; 3] = [200.0, 2_000.0, 11_000.0];
const BANDS: usize = BAND_EDGES.len();
const TIGHTNESS: f32 = 100.0;
/// A half period at least this strong (relative autocorrelation) takes over.
const OCTAVE_UP: f32 = 0.8;
/// Where, as a fraction of the window length before the frame centre, a frame's flux
/// peaks for an onset. Measured on `beats_120_clicks` and `beats_drift_116_124_clicks`
/// (76 clicks at known times): the refined onset peak sat 2.99 ms (standard deviation
/// 0.46 ms) before the click at 48 kHz, 0.140 of the 1024-sample window.
pub const LATENCY_FRACTION: f64 = 0.140;

/// Tempo range; part of the cache key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatSettings {
    pub min_bpm: f32,
    pub max_bpm: f32,
}

impl Default for BeatSettings {
    fn default() -> Self {
        Self {
            min_bpm: 60.0,
            max_bpm: 200.0,
        }
    }
}

impl BeatSettings {
    /// The canonical parameter string for the cache key.
    pub fn params(&self) -> String {
        format!("bpm={:.1}-{:.1}", self.min_bpm, self.max_bpm)
    }
}

/// Tempo and beats of a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Beats {
    pub version: u32,
    /// Beats per minute; 0 when no beat was found.
    pub bpm: f64,
    /// How periodic the onsets are at that tempo, 0 to 1 (see the module docs).
    pub confidence: f32,
    /// Beat times in seconds on the file's timeline.
    pub beats: Vec<f64>,
    /// Local tempo (BPM) of each interval between consecutive beats.
    pub tempo_curve: Vec<f32>,
}

/// The onset strength envelope of mono samples.
#[derive(Clone, Debug)]
pub struct Onsets {
    /// Frames per second.
    pub fps: f64,
    /// Frame `m` is centred at `m / fps` seconds.
    pub strength: Vec<f32>,
}

/// Window length for a sample rate: about 21 ms, a power of two.
fn window_for(rate: u32) -> usize {
    ((f64::from(rate) * 0.021) as usize)
        .next_power_of_two()
        .max(256)
}

/// Streaming STFT band flux.
struct OnsetBuilder {
    n: usize,
    hop: usize,
    /// One past the last bin of each band.
    band_end: [usize; BANDS],
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    previous: Vec<f32>,
    /// Samples not yet consumed, starting with `n / 2` zeros so frames are centred.
    pending: Vec<f32>,
    flux: Vec<[f32; BANDS]>,
}

impl OnsetBuilder {
    fn new(rate: u32) -> Self {
        let n = window_for(rate);
        let hop = n / 4;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        let band_end =
            BAND_EDGES.map(|hz| ((hz / f64::from(rate) * n as f64) as usize).clamp(1, n / 2 + 1));
        let window = (0..n)
            .map(|i| {
                let x = std::f32::consts::PI * i as f32 / n as f32;
                x.sin() * x.sin()
            })
            .collect();
        Self {
            n,
            hop,
            band_end,
            window,
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            previous: vec![0.0; band_end[BANDS - 1]],
            pending: vec![0.0; n / 2],
            flux: Vec::new(),
        }
    }

    fn push(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
        let mut start = 0;
        while start + self.n <= self.pending.len() {
            self.frame(start);
            start += self.hop;
        }
        self.pending.drain(..start);
    }

    fn frame(&mut self, start: usize) {
        for ((x, &s), &w) in self
            .input
            .iter_mut()
            .zip(&self.pending[start..start + self.n])
            .zip(&self.window)
        {
            *x = s * w;
        }
        // Lengths match the plan, so this cannot fail.
        let _ =
            self.fft
                .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch);
        let scale = 2.0 / self.n as f32;
        let mut flux = [0f32; BANDS];
        let mut k = 0;
        for (band, &end) in self.band_end.iter().enumerate() {
            while k < end {
                let m = (1.0 + 100.0 * self.spectrum[k].norm() * scale).ln();
                flux[band] += (m - self.previous[k]).max(0.0);
                self.previous[k] = m;
                k += 1;
            }
        }
        // The first frame compares with silence.
        self.flux.push(if self.flux.is_empty() {
            [0.0; BANDS]
        } else {
            flux
        });
    }

    fn finish(mut self, rate: u32) -> Onsets {
        let tail = vec![0.0; self.n / 2];
        self.push(&tail);
        let fps = f64::from(rate) / self.hop as f64;
        let width = (0.5 * fps) as usize;
        let mut strength = vec![0f32; self.flux.len()];
        let mut band = Vec::with_capacity(self.flux.len());
        for b in 0..BANDS {
            band.clear();
            band.extend(self.flux.iter().map(|f| f[b]));
            for (s, v) in strength.iter_mut().zip(normalise(&band, width)) {
                *s += v;
            }
        }
        scale_to_unit_deviation(&mut strength);
        Onsets { fps, strength }
    }
}

/// Subtracts a centred moving average, clips at zero, scales to unit deviation.
fn normalise(flux: &[f32], width: usize) -> Vec<f32> {
    let n = flux.len();
    if n == 0 {
        return Vec::new();
    }
    let half = width / 2;
    let mut prefix = vec![0f64; n + 1];
    for (i, &v) in flux.iter().enumerate() {
        prefix[i + 1] = prefix[i] + f64::from(v);
    }
    let mut out: Vec<f32> = (0..n)
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(n);
            let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
            (f64::from(flux[i]) - mean).max(0.0) as f32
        })
        .collect();
    scale_to_unit_deviation(&mut out);
    out
}

fn scale_to_unit_deviation(x: &mut [f32]) {
    let n = x.len().max(1) as f64;
    let mean = x.iter().map(|&v| f64::from(v)).sum::<f64>() / n;
    let var = x
        .iter()
        .map(|&v| (f64::from(v) - mean).powi(2))
        .sum::<f64>()
        / n;
    let std = var.sqrt() as f32;
    if std > 1e-9 {
        x.iter_mut().for_each(|v| *v /= std);
    }
}

/// The onset strength of mono samples at `rate`.
pub fn onsets(samples: &[f32], rate: u32) -> Onsets {
    let mut builder = OnsetBuilder::new(rate);
    builder.push(samples);
    builder.finish(rate)
}

/// Normalised autocorrelation of `x` at integer lags `0..=max_lag`.
fn autocorrelation(x: &[f32], max_lag: usize) -> Vec<f32> {
    let n = x.len();
    (0..=max_lag.min(n.saturating_sub(1)))
        .map(|lag| {
            let sum: f64 = x[..n - lag]
                .iter()
                .zip(&x[lag..])
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum();
            (sum / (n - lag) as f64) as f32
        })
        .collect()
}

/// Linear interpolation of `acf` at a fractional lag.
fn acf_at(acf: &[f32], lag: f64) -> f32 {
    let i = lag.floor() as usize;
    if i + 1 >= acf.len() {
        return acf.last().copied().unwrap_or(0.0);
    }
    let f = (lag - i as f64) as f32;
    acf[i] * (1.0 - f) + acf[i + 1] * f
}

/// The beat period in frames and the confidence (see the module docs).
fn tempo_period(onsets: &Onsets, settings: &BeatSettings) -> Option<(f64, f32)> {
    let fps = onsets.fps;
    let lag_of = |bpm: f64| 60.0 * fps / bpm;
    let min_lag = lag_of(f64::from(settings.max_bpm)).floor().max(1.0) as usize;
    let max_lag = lag_of(f64::from(settings.min_bpm)).ceil() as usize;
    let acf = autocorrelation(&onsets.strength, 2 * max_lag + 2);
    if acf.len() <= max_lag + 1 || acf[0] <= 0.0 {
        return None;
    }
    let prior = |lag: f64| {
        let octaves = (lag_of(120.0) / lag).log2();
        (-0.5 * octaves * octaves).exp() as f32
    };
    let mut best: Option<(usize, f32)> = None;
    for lag in min_lag.max(1)..=max_lag.min(acf.len() - 2) {
        let v = acf[lag];
        if v > acf[lag - 1] && v >= acf[lag + 1] {
            let weighted = v * prior(lag as f64);
            if best.is_none_or(|(_, b)| weighted > b) {
                best = Some((lag, weighted));
            }
        }
    }
    let (lag, _) = best?;
    let refine = |lag: usize| -> f64 {
        let (a, b, c) = (acf[lag - 1], acf[lag], acf[lag + 1]);
        let denom = a - 2.0 * b + c;
        if denom.abs() < 1e-12 {
            lag as f64
        } else {
            lag as f64 + f64::from((0.5 * (a - c) / denom).clamp(-0.5, 0.5))
        }
    };
    let mut period = refine(lag);
    loop {
        let half = period / 2.0;
        if half < lag_of(f64::from(settings.max_bpm)) {
            break;
        }
        // The local maximum near the half period.
        let near = (half.round() as usize).clamp(2, acf.len() - 2);
        let peak = (near - 1..=near + 1)
            .max_by(|&a, &b| acf[a].total_cmp(&acf[b]))
            .unwrap_or(near);
        if acf[peak] >= OCTAVE_UP * acf_at(&acf, period) {
            period = refine(peak);
        } else {
            break;
        }
    }
    // Autocovariance over variance: the envelope is never negative, so its mean alone
    // would correlate at every lag.
    let n = onsets.strength.len() as f64;
    let mean = (onsets.strength.iter().map(|&v| f64::from(v)).sum::<f64>() / n) as f32;
    let variance = acf[0] - mean * mean;
    let confidence = if variance > 0.0 {
        ((acf_at(&acf, period) - mean * mean) / variance).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((period, confidence))
}

/// Ellis's dynamic programming beat tracker; beat frames, ascending.
fn track(strength: &[f32], period: f64) -> Vec<usize> {
    let n = strength.len();
    if n == 0 || period < 1.0 {
        return Vec::new();
    }
    // Smooth with a Gaussian of about a 32nd of the period.
    let sigma = (period / 32.0).max(0.5);
    let radius = (3.0 * sigma).ceil() as isize;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|i| (-0.5 * (i as f64 / sigma).powi(2)).exp() as f32)
        .collect();
    let local: Vec<f32> = (0..n as isize)
        .map(|i| {
            kernel
                .iter()
                .enumerate()
                .filter_map(|(k, w)| {
                    let j = i + k as isize - radius;
                    (0..n as isize)
                        .contains(&j)
                        .then(|| w * strength[j as usize])
                })
                .sum()
        })
        .collect();

    let mut score = vec![0f32; n];
    let mut back = vec![usize::MAX; n];
    let lo = (period / 2.0).round().max(1.0) as usize;
    let hi = (2.0 * period).round() as usize;
    let peak = local.iter().fold(0.0, |a: f32, &b| a.max(b));
    let first_strong = local.iter().position(|&v| v > 0.01 * peak);
    for i in 0..n {
        let mut best = f32::NEG_INFINITY;
        let mut arg = usize::MAX;
        if i >= lo {
            let earliest = i.saturating_sub(hi);
            for (t, &prev) in score[..=i - lo].iter().enumerate().skip(earliest) {
                let r = ((i - t) as f64 / period).ln() as f32;
                let v = prev - TIGHTNESS * r * r;
                if v > best {
                    best = v;
                    arg = t;
                }
            }
        }
        if arg == usize::MAX || first_strong.is_none_or(|f| i < f) {
            score[i] = local[i];
        } else {
            score[i] = local[i] + best;
            back[i] = arg;
        }
    }

    // The last beat: the last local maximum of the score above half the median of them.
    let maxima: Vec<usize> = (1..n.saturating_sub(1))
        .filter(|&i| score[i] > score[i - 1] && score[i] >= score[i + 1])
        .collect();
    if maxima.is_empty() {
        return Vec::new();
    }
    let mut values: Vec<f32> = maxima.iter().map(|&i| score[i]).collect();
    let mid = values.len() / 2;
    let median = *values.select_nth_unstable_by(mid, f32::total_cmp).1;
    let Some(&last) = maxima.iter().rev().find(|&&i| score[i] >= 0.5 * median) else {
        return Vec::new();
    };
    let mut beats = vec![last];
    let mut i = last;
    while back[i] != usize::MAX {
        i = back[i];
        beats.push(i);
    }
    beats.reverse();

    // Drop weak beats at either end.
    let rms = (beats.iter().map(|&b| local[b] * local[b]).sum::<f32>() / beats.len() as f32).sqrt();
    let strong = |b: &usize| local[*b] >= 0.5 * rms;
    let start = beats.iter().position(strong).unwrap_or(beats.len());
    let end = beats.iter().rposition(strong).map_or(start, |e| e + 1);
    beats[start..end.max(start)].to_vec()
}

/// Moves a beat frame to the strongest onset within 2 frames, to a fraction of a frame.
fn refine_beat(strength: &[f32], frame: usize) -> f64 {
    let lo = frame.saturating_sub(2);
    let hi = (frame + 2).min(strength.len() - 1);
    let peak = (lo..=hi)
        .max_by(|&a, &b| strength[a].total_cmp(&strength[b]))
        .unwrap_or(frame);
    if peak == 0 || peak + 1 >= strength.len() {
        return peak as f64;
    }
    let (a, b, c) = (strength[peak - 1], strength[peak], strength[peak + 1]);
    let denom = a - 2.0 * b + c;
    if denom.abs() < 1e-9 {
        peak as f64
    } else {
        peak as f64 + f64::from((0.5 * (a - c) / denom).clamp(-0.5, 0.5))
    }
}

/// Seconds by which a frame's centre trails the onset whose flux peaks there (negative:
/// the centre leads, so subtracting it moves the beat later). See [`LATENCY_FRACTION`].
pub fn latency_seconds(rate: u32) -> f64 {
    -LATENCY_FRACTION * window_for(rate) as f64 / f64::from(rate)
}

/// The time of the onset nearest `frame` (within 2 frames), latency removed.
fn onset_time(onsets: &Onsets, rate: u32, frame: usize) -> f64 {
    (refine_beat(&onsets.strength, frame) / onsets.fps - latency_seconds(rate)).max(0.0)
}

/// Where the onset nearest `seconds` peaks (within 2 frames), latency removed:
/// calibration and tests against known onset times.
pub fn nearest_onset(onsets: &Onsets, rate: u32, seconds: f64) -> f64 {
    let frame = ((seconds + latency_seconds(rate)) * onsets.fps)
        .round()
        .max(0.0) as usize;
    onset_time(
        onsets,
        rate,
        frame.min(onsets.strength.len().saturating_sub(1)),
    )
}

/// Tempo and beats from an onset envelope (times from the first sample).
pub fn beats_from_onsets(onsets: &Onsets, rate: u32, settings: &BeatSettings) -> Beats {
    let empty = Beats {
        version: BEATS_VERSION,
        bpm: 0.0,
        confidence: 0.0,
        beats: Vec::new(),
        tempo_curve: Vec::new(),
    };
    let Some((period, confidence)) = tempo_period(onsets, settings) else {
        return empty;
    };
    let frames = track(&onsets.strength, period);
    if frames.len() < 2 {
        return empty;
    }
    let beats: Vec<f64> = frames
        .iter()
        .map(|&f| onset_time(onsets, rate, f))
        .collect();
    let n = beats.len() as f64;
    let mean_i = (n - 1.0) / 2.0;
    let mean_t = beats.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (i, t) in beats.iter().enumerate() {
        let di = i as f64 - mean_i;
        sxy += di * (t - mean_t);
        sxx += di * di;
    }
    let slope = sxy / sxx;
    let tempo_curve = beats
        .windows(2)
        .map(|w| (60.0 / (w[1] - w[0]).max(1e-6)) as f32)
        .collect();
    Beats {
        version: BEATS_VERSION,
        bpm: if slope > 0.0 { 60.0 / slope } else { 0.0 },
        confidence,
        beats,
        tempo_curve,
    }
}

/// Reads the best audio stream of `path` and finds its tempo and beats.
pub fn detect(
    path: &Path,
    settings: &BeatSettings,
    control: &dyn Control,
) -> Result<Beats, AnalysisError> {
    let mut reader = AudioReader::open_for_source(path, AudioOptions::default(), |source, o| {
        o.sample_rate = source.sample_rate.max(8_000);
        o.layout = ChannelLayout::Mono;
    })?;
    let rate = reader.sample_rate();
    let expected = reader
        .source()
        .duration
        .map_or(0.0, |d| d.seconds() * f64::from(rate))
        .max(1.0);
    let first = reader.position();
    let mut plane = vec![0f32; READ_FRAMES];
    let mut builder = OnsetBuilder::new(rate);
    let mut total: u64 = 0;
    loop {
        control.checkpoint()?;
        let frames = reader.read(&mut [plane.as_mut_slice()])?;
        if frames == 0 {
            break;
        }
        builder.push(&plane[..frames]);
        total += frames as u64;
        control.progress((total as f64 / expected).min(0.9) as f32);
    }
    control.checkpoint()?;
    let onsets = builder.finish(rate);
    let mut beats = beats_from_onsets(&onsets, rate, settings);
    // Onset times count from the stream's first sample; beats are on the file's timeline.
    let offset = first as f64 / f64::from(rate);
    beats.beats.iter_mut().for_each(|b| *b += offset);
    Ok(beats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Decaying 1.5 kHz clicks at `times` (seconds), `seconds` long.
    fn clicks(times: &[f64], seconds: f64) -> Vec<f32> {
        let mut x = vec![0f32; (seconds * f64::from(RATE)) as usize];
        for &t in times {
            let start = (t * f64::from(RATE)).round() as usize;
            for i in 0..(RATE as usize / 50) {
                if let Some(s) = x.get_mut(start + i) {
                    let u = i as f32 / RATE as f32;
                    *s +=
                        0.5 * (2.0 * std::f32::consts::PI * 1500.0 * u).sin() * (-u * 200.0).exp();
                }
            }
        }
        x
    }

    /// Synthetic clicks at times off the frame grid land within 1.5 ms on average and
    /// 3 ms each after the latency correction.
    #[test]
    fn latency_is_calibrated() {
        let times: Vec<f64> = (0..20).map(|i| 0.5 + i as f64 * 0.5013).collect();
        let onsets = onsets(&clicks(&times, 11.0), RATE);
        let errors: Vec<f64> = times
            .iter()
            .map(|&t| nearest_onset(&onsets, RATE, t) - t)
            .collect();
        let mean = errors.iter().sum::<f64>() / errors.len() as f64;
        assert!(mean.abs() < 0.0015, "mean onset error {mean} s: {errors:?}");
        assert!(errors.iter().all(|e| e.abs() < 0.003), "{errors:?}");
    }

    #[test]
    fn steady_clicks_give_tempo_and_beats() {
        for bpm in [90.0, 120.0, 174.0] {
            let period = 60.0 / bpm;
            let times: Vec<f64> = (0..)
                .map(|i| 0.25 + i as f64 * period)
                .take_while(|&t| t < 19.5)
                .collect();
            let beats = beats_from_onsets(
                &onsets(&clicks(&times, 20.0), RATE),
                RATE,
                &BeatSettings::default(),
            );
            assert!((beats.bpm - bpm).abs() < 1.0, "{bpm}: {}", beats.bpm);
            assert!(beats.confidence > 0.7, "{bpm}: {}", beats.confidence);
            for t in &times[1..times.len() - 1] {
                let nearest = beats
                    .beats
                    .iter()
                    .map(|b| (b - t).abs())
                    .fold(f64::INFINITY, f64::min);
                assert!(nearest < 0.01, "{bpm}: beat at {t} missed by {nearest}");
            }
        }
    }

    /// White noise has no pulse: low confidence; silence gives no beats.
    #[test]
    fn noise_has_low_confidence_and_silence_no_beats() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let noise: Vec<f32> = (0..RATE as usize * 20)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
            })
            .collect();
        let found = beats_from_onsets(&onsets(&noise, RATE), RATE, &BeatSettings::default());
        assert!(found.confidence < 0.2, "{}", found.confidence);
        let silent = vec![0f32; RATE as usize * 5];
        let found = beats_from_onsets(&onsets(&silent, RATE), RATE, &BeatSettings::default());
        assert!(found.beats.is_empty() && found.bpm == 0.0, "{found:?}");
    }
}
