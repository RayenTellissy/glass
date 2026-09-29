//! Scene cut detection: the frames where a shot starts.
//!
//! **Signal.** Every frame is decoded (a cut can fall on any frame, and inter frames
//! need their references) but only the picture stream, in software with two threads,
//! and only a [`SceneSettings::width`]-pixel-wide copy (96 by default) is converted
//! ([`VideoFrame::to_rgba8_scaled`]). Each small frame gives a luma histogram (32 bins),
//! a hue histogram (16 bins, each pixel weighted by its chroma, so grey pictures carry
//! almost no hue mass) and the mean luma of an 8 x 8 grid of blocks. The difference
//! between two frames is
//!
//! `d = 0.35 * luma_hist + 0.25 * hue_hist + 0.4 * min(1, 2 * blocks)`
//!
//! with each histogram term the total variation distance (half the L1 distance, 0 to 1)
//! and `blocks` the mean absolute difference of the block means (0 to 1). Histograms
//! ignore where things are, so motion barely moves them; the block term sees a cut
//! between shots with similar colours, and block means (rather than pixels) keep it
//! tolerant of small motion. The score of frame `i` is `d(i - 1, i)`.
//!
//! **Threshold.** Frame `i` is a candidate when its score exceeds
//! `max(floor, median + k * 1.4826 * MAD)` over the scores within one second on either
//! side (at least 10 frames; `i` itself excluded), a robust local threshold that rises in
//! busy footage and falls in static footage, and when it is at least `3 - s` times the
//! mean score of the two frames on either side: a cut is a one-frame spike, while a fade,
//! a whip pan or a zoom changes every frame about as much (without this, the brightening
//! frames of a fade from black cross the floor). The sensitivity `s` (0 to 1, default
//! 0.5) also sets `k = 3 + 12 * (1 - s)` and `floor = 0.30 - 0.20 * s`: 9, 0.20 and a
//! ratio of 2.5 by default.
//!
//! **Flashes.** A candidate at `i` is dropped when some frame at most
//! `flash = clamp(round(0.15 s * fps), 2, 8)` frames before `i` and some frame at most
//! that far after it, spanning at most `flash + 1` frames, differ by less than the
//! threshold at `i`: the picture came back, so the change was a flash (camera flash,
//! strobe, explosion, a few white frames), not a new shot. This removes both the jump
//! into and the jump out of the flash.
//!
//! **Fades.** Gradual transitions change each frame a little, so they never cross the
//! threshold and are not hard cuts. A fade through black is reported as one cut of kind
//! [`CutKind::Fade`] in the middle of the black run: a run of black frames (every block
//! mean below 0.06) with picture on both sides and no hard cut at either edge. A fade
//! out at the end or in at the start of a clip reports nothing, and so do dissolves and
//! fades that never reach black.
//!
//! **Minimum scene length.** Candidates are accepted strongest first (score over its
//! threshold, fades last); one closer than [`SceneSettings::min_scene_ms`] (500 ms by
//! default) to an accepted cut is dropped. Frame 0 is never a cut.
//!
//! **Storage**: `scenes-<p>-v1.json`, [`SceneCuts`] (a few bytes per cut).

use std::collections::VecDeque;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::analysis::{AnalysisError, Control};
use crate::decode::{DecoderOptions, VideoFrame};
use crate::source::{SourceOptions, VideoSource};
use crate::time::Rational;

/// Bumped when the output or the algorithm changes.
pub const SCENES_VERSION: u32 = 1;

const LUMA_BINS: usize = 32;
const HUE_BINS: usize = 16;
const GRID: usize = 8;
const BLOCKS: usize = GRID * GRID;
/// Distances kept to earlier frames for flash suppression: `d(i - k, i)` for `k` below
/// this (up to the longest flash, 8 frames, plus one).
const BACK: usize = 10;
const BLACK_BLOCK: f32 = 0.06;
const FLASH_SECONDS: f64 = 0.15;

/// How cuts are found; part of the cache key except the decode threads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneSettings {
    /// 0 (only the most obvious cuts) to 1 (every plausible one).
    pub sensitivity: f32,
    /// Shots shorter than this are merged into their neighbour.
    pub min_scene_ms: u32,
    /// Width of the analysed copy of each frame, 64 to 128 pixels.
    pub width: u32,
    /// Decoder threads (not part of the key: it does not change the result).
    pub decode_threads: usize,
}

impl Default for SceneSettings {
    fn default() -> Self {
        Self {
            sensitivity: 0.5,
            min_scene_ms: 500,
            width: 96,
            decode_threads: 2,
        }
    }
}

impl SceneSettings {
    /// The canonical parameter string for the cache key.
    pub fn params(&self) -> String {
        format!(
            "s={:.3};min={};w={}",
            self.sensitivity.clamp(0.0, 1.0),
            self.min_scene_ms,
            self.analysis_width()
        )
    }

    fn analysis_width(&self) -> u32 {
        self.width.clamp(64, 128)
    }

    fn k(&self) -> f32 {
        3.0 + 12.0 * (1.0 - self.sensitivity.clamp(0.0, 1.0))
    }

    fn local_ratio(&self) -> f32 {
        3.0 - self.sensitivity.clamp(0.0, 1.0)
    }

    fn floor(&self) -> f32 {
        0.30 - 0.20 * self.sensitivity.clamp(0.0, 1.0)
    }
}

/// How a shot begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CutKind {
    /// From one frame to the next.
    Hard,
    /// Through black (the cut is the middle of the black run).
    Fade,
}

/// The first frame of a new shot.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneCut {
    /// Index of the frame in presentation order (0 is the first decoded frame).
    pub frame: u64,
    /// Its presentation time after the first frame, in microseconds.
    pub time_us: i64,
    pub kind: CutKind,
    /// Score over threshold (above 1; higher is surer). 1 for fades.
    pub strength: f32,
}

/// The cuts of a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneCuts {
    pub version: u32,
    /// Frames analysed.
    pub frames: u64,
    /// Frames per second measured from the decoded timestamps.
    pub fps: f64,
    pub cuts: Vec<SceneCut>,
}

/// One small frame's summary.
#[derive(Clone, Debug)]
struct Features {
    luma: [f32; LUMA_BINS],
    hue: [f32; HUE_BINS],
    blocks: [f32; BLOCKS],
}

impl Features {
    fn of(rgba: &[u8], width: usize, height: usize) -> Self {
        let mut luma = [0f32; LUMA_BINS];
        let mut hue = [0f32; HUE_BINS];
        let mut block_sum = [0f32; BLOCKS];
        let mut block_count = [0u32; BLOCKS];
        for y in 0..height {
            let by = y * GRID / height.max(1);
            for x in 0..width {
                let at = (y * width + x) * 4;
                let (r, g, b) = (
                    i32::from(rgba[at]),
                    i32::from(rgba[at + 1]),
                    i32::from(rgba[at + 2]),
                );
                let y8 = ((54 * r + 183 * g + 19 * b) >> 8).clamp(0, 255);
                luma[(y8 as usize * LUMA_BINS) >> 8] += 1.0;
                let block = by * GRID + x * GRID / width.max(1);
                block_sum[block] += y8 as f32;
                block_count[block] += 1;
                let max = r.max(g).max(b);
                let min = r.min(g).min(b);
                let chroma = max - min;
                if chroma > 12 {
                    let c = chroma as f32;
                    let h = if max == r {
                        ((g - b) as f32 / c).rem_euclid(6.0)
                    } else if max == g {
                        (b - r) as f32 / c + 2.0
                    } else {
                        (r - g) as f32 / c + 4.0
                    };
                    let bin = ((h / 6.0 * HUE_BINS as f32) as usize).min(HUE_BINS - 1);
                    hue[bin] += c / 255.0;
                }
            }
        }
        let pixels = (width * height).max(1) as f32;
        luma.iter_mut().for_each(|v| *v /= pixels);
        hue.iter_mut().for_each(|v| *v /= pixels);
        let mut blocks = [0f32; BLOCKS];
        for (i, block) in blocks.iter_mut().enumerate() {
            *block = block_sum[i] / (block_count[i].max(1) as f32 * 255.0);
        }
        Self { luma, hue, blocks }
    }

    fn distance(&self, other: &Self) -> f32 {
        let tv = |a: &[f32], b: &[f32]| -> f32 {
            0.5 * a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>()
        };
        let blocks = self
            .blocks
            .iter()
            .zip(&other.blocks)
            .map(|(x, y)| (x - y).abs())
            .sum::<f32>()
            / BLOCKS as f32;
        0.35 * tv(&self.luma, &other.luma)
            + 0.25 * tv(&self.hue, &other.hue).min(1.0)
            + 0.4 * (2.0 * blocks).min(1.0)
    }

    fn is_black(&self) -> bool {
        self.blocks.iter().all(|&b| b < BLACK_BLOCK)
    }
}

/// Per-frame measurements, gathered in one pass and decided on afterwards.
#[derive(Clone, Debug, Default)]
pub struct SceneSignal {
    /// `d(i - 1, i)`; 0 for frame 0.
    pub scores: Vec<f32>,
    /// `back[i][k] = d(i - k, i)` for `2 <= k < BACK` (infinite where there is no frame).
    back: Vec<[f32; BACK]>,
    black: Vec<bool>,
    /// Presentation times after the first frame, microseconds.
    times_us: Vec<i64>,
    recent: VecDeque<Features>,
}

impl SceneSignal {
    fn push(&mut self, features: Features, time_us: i64) {
        let mut back = [f32::INFINITY; BACK];
        for (k, earlier) in self.recent.iter().rev().enumerate() {
            back[k + 1] = earlier.distance(&features);
        }
        self.scores
            .push(if self.recent.is_empty() { 0.0 } else { back[1] });
        self.back.push(back);
        self.black.push(features.is_black());
        self.times_us.push(time_us);
        self.recent.push_back(features);
        if self.recent.len() >= BACK {
            self.recent.pop_front();
        }
    }

    fn len(&self) -> usize {
        self.scores.len()
    }

    /// Frames per second from the timestamps (30 when they say nothing).
    fn fps(&self) -> f64 {
        let n = self.times_us.len();
        if n < 2 {
            return 30.0;
        }
        let span = (self.times_us[n - 1] - self.times_us[0]) as f64 / 1e6;
        if span > 0.0 {
            (n - 1) as f64 / span
        } else {
            30.0
        }
    }

    /// Applies the rules in the module docs.
    pub fn cuts(&self, settings: &SceneSettings) -> Vec<SceneCut> {
        let n = self.len();
        let fps = self.fps();
        let half_window = (fps.round() as usize).max(10);
        let flash = ((FLASH_SECONDS * fps).round() as usize).clamp(2, BACK - 2);
        let (k, floor) = (settings.k(), settings.floor());

        let mut candidates: Vec<(usize, f32, CutKind)> = Vec::new();
        let mut window = Vec::with_capacity(2 * half_window);
        let mut thresholds = vec![f32::INFINITY; n];
        for i in 1..n {
            window.clear();
            let lo = i.saturating_sub(half_window).max(1);
            let hi = (i + half_window).min(n - 1);
            window.extend((lo..=hi).filter(|&j| j != i).map(|j| self.scores[j]));
            let threshold = if window.is_empty() {
                floor
            } else {
                let median = median_of(&mut window);
                window.iter_mut().for_each(|v| *v = (*v - median).abs());
                let mad = median_of(&mut window);
                (median + k * 1.4826 * mad).max(floor)
            };
            thresholds[i] = threshold;
            if self.scores[i] > threshold
                && self.stands_out(i, settings.local_ratio())
                && !self.is_flash(i, flash, threshold)
            {
                candidates.push((i, self.scores[i] / threshold, CutKind::Hard));
            }
        }

        let hard: Vec<usize> = candidates.iter().map(|c| c.0).collect();
        let near_hard = |f: usize| hard.iter().any(|&h| h.abs_diff(f) <= 1);
        let mut i = 0;
        while i < n {
            if !self.black[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < n && self.black[i] {
                i += 1;
            }
            let end = i; // first frame after the run
            if start > 0 && end < n && !near_hard(start) && !near_hard(end) {
                candidates.push(((start + end) / 2, 1.0, CutKind::Fade));
            }
        }

        candidates.sort_by(|a, b| {
            let fade_last = (a.2 == CutKind::Fade).cmp(&(b.2 == CutKind::Fade));
            fade_last.then(b.1.total_cmp(&a.1))
        });
        let min_frames = (f64::from(settings.min_scene_ms) / 1000.0 * fps).round() as usize;
        let mut accepted: Vec<(usize, f32, CutKind)> = Vec::new();
        for c in candidates {
            if c.0 == 0 || accepted.iter().any(|a| a.0.abs_diff(c.0) < min_frames) {
                continue;
            }
            accepted.push(c);
        }
        accepted.sort_by_key(|a| a.0);
        accepted
            .into_iter()
            .map(|(frame, strength, kind)| SceneCut {
                frame: frame as u64,
                time_us: self.times_us[frame],
                kind,
                strength,
            })
            .collect()
    }

    /// Whether score `i` is at least `ratio` times the mean of the two scores on either
    /// side: a cut is a spike, a fade or a fast pan is a plateau.
    fn stands_out(&self, i: usize, ratio: f32) -> bool {
        let around: Vec<f32> = (i.saturating_sub(2).max(1)..=(i + 2).min(self.len() - 1))
            .filter(|&j| j != i)
            .map(|j| self.scores[j])
            .collect();
        if around.is_empty() {
            return true;
        }
        let mean = around.iter().sum::<f32>() / around.len() as f32;
        self.scores[i] >= ratio * mean
    }

    /// Whether a frame before `i` and one after it, at most `flash + 1` apart, match.
    fn is_flash(&self, i: usize, flash: usize, threshold: f32) -> bool {
        (i..=(i + flash).min(self.len() - 1)).any(|q| {
            let nearest = q - i + 1;
            (nearest.max(2)..=flash + 1).any(|k| k <= q && self.back[q][k] < threshold)
        })
    }
}

fn median_of(values: &mut [f32]) -> f32 {
    let mid = values.len() / 2;
    let (_, m, _) = values.select_nth_unstable_by(mid, f32::total_cmp);
    *m
}

/// Decodes the pictures of `path` and returns its scene cuts.
pub fn detect(
    path: &Path,
    settings: &SceneSettings,
    control: &dyn Control,
) -> Result<SceneCuts, AnalysisError> {
    let mut options = SourceOptions::new(Rational::new(30, 1));
    options.decoder = DecoderOptions {
        threads: settings.decode_threads,
        ..DecoderOptions::default()
    };
    let mut source = VideoSource::open(path, options)?;
    let info = source.info().clone();
    let width = settings.analysis_width();
    let height = ((u64::from(width) * u64::from(info.height.max(1))) / u64::from(info.width.max(1)))
        .clamp(16, u64::from(width) * 4) as u32;
    let expected = info.frame_count.max(1) as f32;

    let mut signal = SceneSignal::default();
    let mut first_us = None;
    while let Some(frame) = source.next_frame()? {
        control.checkpoint()?;
        let time_us = frame_time_us(&frame, &mut first_us);
        let small = frame.to_rgba8_scaled(width, height)?;
        signal.push(
            Features::of(&small.data, small.width as usize, small.height as usize),
            time_us,
        );
        if signal.len() % 16 == 0 {
            control.progress((signal.len() as f32 / expected).min(0.99));
        }
    }
    Ok(SceneCuts {
        version: SCENES_VERSION,
        frames: signal.len() as u64,
        fps: signal.fps(),
        cuts: signal.cuts(settings),
    })
}

fn frame_time_us(frame: &VideoFrame, first: &mut Option<i64>) -> i64 {
    let us = frame
        .pts()
        .map_or(0, |pts| pts.rescale(Rational::MICROS).value);
    us - *first.get_or_insert(us)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A horizontal ramp in one hue at `level` (0 to 1), shifted by `offset` pixels.
    fn ramp(hue: [f32; 3], level: f32, offset: usize) -> Features {
        let (w, h) = (64, 36);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for _ in 0..h {
            for x in 0..w {
                let v = ((x + offset) % w) as f32 / w as f32 * 0.8 + 0.2;
                for c in hue {
                    rgba.push((c * v * level * 255.0) as u8);
                }
                rgba.push(255);
            }
        }
        Features::of(&rgba, w, h)
    }

    const WARM: [f32; 3] = [1.0, 0.6, 0.2];
    const COOL: [f32; 3] = [0.2, 0.5, 1.0];
    const GREEN: [f32; 3] = [0.3, 1.0, 0.3];

    fn shot(hue: [f32; 3], frames: std::ops::Range<usize>) -> impl Iterator<Item = Features> {
        frames.map(move |i| ramp(hue, 1.0, i))
    }

    fn signal(frames: &[Features]) -> SceneSignal {
        let mut s = SceneSignal::default();
        for (i, f) in frames.iter().enumerate() {
            s.push(f.clone(), i as i64 * 1_000_000 / 30);
        }
        s
    }

    #[test]
    fn distances_are_zero_for_equal_frames_and_large_for_different_ones() {
        let a = ramp(WARM, 1.0, 0);
        assert_eq!(a.distance(&a), 0.0);
        assert!(a.distance(&ramp(WARM, 1.0, 1)) < 0.05);
        assert!(a.distance(&ramp(COOL, 1.0, 0)) > 0.25);
        assert!(ramp(WARM, 0.02, 0).is_black());
        assert!(!a.is_black());
    }

    #[test]
    fn a_hard_cut_is_found_at_its_first_frame_and_a_flash_is_not() {
        let mut frames: Vec<Features> = shot(WARM, 0..40).collect();
        frames.extend(shot(COOL, 40..90));
        frames[60] = ramp([1.0, 1.0, 1.0], 1.0, 0);
        frames[61] = ramp([1.0, 1.0, 1.0], 0.95, 0);
        let cuts = signal(&frames).cuts(&SceneSettings::default());
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        assert_eq!((cuts[0].frame, cuts[0].kind), (40, CutKind::Hard));
    }

    #[test]
    fn short_scenes_merge() {
        let mut frames: Vec<Features> = shot(WARM, 0..30).collect();
        frames.extend(shot(COOL, 30..35));
        frames.extend(shot(GREEN, 35..70));
        let cuts = signal(&frames).cuts(&SceneSettings::default());
        assert_eq!(cuts.len(), 1, "{cuts:?}");
    }

    #[test]
    fn a_fade_through_black_is_one_fade_cut() {
        let mut frames: Vec<Features> = shot(WARM, 0..30).collect();
        frames.extend((0..10).map(|s| ramp(WARM, 1.0 - s as f32 / 10.0, 0)));
        frames.extend((0..3).map(|_| ramp(WARM, 0.0, 0)));
        frames.extend((0..10).map(|s| ramp(COOL, s as f32 / 10.0, 0)));
        frames.extend(shot(COOL, 0..30));
        let cuts = signal(&frames).cuts(&SceneSettings::default());
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        assert_eq!(cuts[0].kind, CutKind::Fade);
        assert!(cuts[0].frame.abs_diff(41) <= 2, "{cuts:?}");
    }
}
