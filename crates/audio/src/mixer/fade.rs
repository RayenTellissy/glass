//! Fade and crossfade curves: the gains of the clip leaving and the clip arriving over a
//! transition on an audio track, and of a clip fading in from or out to nothing.
//!
//! A fade covers whole samples `from..to` of the timeline. Each sample's gain is the curve
//! at the sample's centre, linear progress `p = (k − from + ½) / (to − from)` (see
//! [`progress`]), so a fade in and the fade out over the same samples are exact mirror
//! images and no sample of a fade sits at exactly 0 or 1; the samples before a fade in
//! are silent and those after it at full gain. The caller may bend `p` with an easing
//! curve first: every curve below keeps its sum rule for any `p`.

use std::f64::consts::FRAC_PI_2;

/// The shape of a crossfade: how the outgoing gain falls and the incoming gain rises with
/// (eased) progress `q`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FadeCurve {
    /// Constant power: out `cos(πq/2)`, in `sin(πq/2)`, so `out² + in² = 1`. Two
    /// unrelated recordings (uncorrelated) keep their loudness through the fade; one
    /// signal faded against itself rises 3 dB mid-way. The default.
    #[default]
    EqualPower,
    /// Constant gain: out `1 − q`, in `q`, so `out + in = 1`. The same or phase-aligned
    /// material keeps its level; uncorrelated material dips 3 dB mid-way.
    Linear,
    /// Constant gain with no slope at either end: in `sin²(πq/2) = (1 − cos πq) / 2`, out
    /// `cos²(πq/2)`, so `out + in = 1`; gentler than linear at the edges of the fade.
    SCurve,
}

impl FadeCurve {
    /// `(outgoing, incoming)` gains at progress `q` (clamped to 0..1; NaN reads as 0).
    pub fn gains(self, q: f64) -> (f64, f64) {
        let q = if q.is_nan() { 0.0 } else { q.clamp(0.0, 1.0) };
        match self {
            Self::EqualPower => {
                let (s, c) = (q * FRAC_PI_2).sin_cos();
                (c, s)
            }
            Self::Linear => (1.0 - q, q),
            Self::SCurve => {
                let (s, c) = (q * FRAC_PI_2).sin_cos();
                (c * c, s * s)
            }
        }
    }

    /// The gain of a clip fading in at progress `q`.
    pub fn fade_in(self, q: f64) -> f64 {
        self.gains(q).1
    }

    /// The gain of a clip fading out at progress `q`.
    pub fn fade_out(self, q: f64) -> f64 {
        self.gains(q).0
    }
}

/// Linear progress of timeline sample `k` through a fade over samples `from..to`: its
/// centre, `(k − from + ½) / (to − from)`, clamped to 0..1 (0 before the fade, 1 after
/// it; 1 for an empty fade).
pub fn progress(k: i64, from: i64, to: i64) -> f64 {
    if to <= from {
        return 1.0;
    }
    ((k - from) as f64 + 0.5) / (to - from) as f64
}

/// The gain at sample `k` of a fade in over `from..to` with `curve` (0 before it, 1 after
/// it), progress bent by `ease` first.
pub fn fade_in_at(curve: FadeCurve, from: i64, to: i64, k: i64, ease: impl Fn(f64) -> f64) -> f64 {
    if k < from {
        0.0
    } else if k >= to {
        1.0
    } else {
        curve.fade_in(ease(progress(k, from, to)))
    }
}

/// The gain at sample `k` of a fade out over `from..to` with `curve` (1 before it, 0
/// after it), progress bent by `ease` first.
pub fn fade_out_at(curve: FadeCurve, from: i64, to: i64, k: i64, ease: impl Fn(f64) -> f64) -> f64 {
    if k < from {
        1.0
    } else if k >= to {
        0.0
    } else {
        curve.fade_out(ease(progress(k, from, to)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [FadeCurve; 3] = [FadeCurve::EqualPower, FadeCurve::Linear, FadeCurve::SCurve];

    #[test]
    fn curves_keep_their_sum_rules_and_end_points() {
        for i in 0..=1000 {
            let q = f64::from(i) / 1000.0;
            let (o, n) = FadeCurve::EqualPower.gains(q);
            assert!((o * o + n * n - 1.0).abs() < 1e-12, "power at {q}");
            for curve in [FadeCurve::Linear, FadeCurve::SCurve] {
                let (o, n) = curve.gains(q);
                assert!((o + n - 1.0).abs() < 1e-12, "{curve:?} gain at {q}");
            }
        }
        for curve in ALL {
            assert_eq!(curve.gains(0.0).1, 0.0, "{curve:?}");
            assert!((curve.gains(0.0).0 - 1.0).abs() < 1e-15, "{curve:?}");
            assert!((curve.gains(1.0).1 - 1.0).abs() < 1e-15, "{curve:?}");
            assert!(curve.gains(1.0).0.abs() < 1e-15, "{curve:?}");
            assert_eq!(curve.gains(f64::NAN), curve.gains(0.0));
            assert_eq!(curve.gains(2.0), curve.gains(1.0));
        }
    }

    #[test]
    fn fades_in_and_out_mirror_each_other_and_rise_monotonically() {
        for curve in ALL {
            let mut last = 0.0;
            for k in 0..100 {
                let rise = fade_in_at(curve, 0, 100, k, |p| p);
                let fall = fade_out_at(curve, 0, 100, 99 - k, |p| p);
                assert!((rise - fall).abs() < 1e-12, "{curve:?} at {k}");
                assert!(rise > last && rise < 1.0, "{curve:?} at {k}");
                last = rise;
            }
            assert_eq!(fade_in_at(curve, 0, 100, -1, |p| p), 0.0);
            assert_eq!(fade_in_at(curve, 0, 100, 100, |p| p), 1.0);
            assert_eq!(fade_out_at(curve, 0, 100, -1, |p| p), 1.0);
            assert_eq!(fade_out_at(curve, 0, 100, 100, |p| p), 0.0);
        }
        // The middle of an even fade: the two centre samples straddle progress one half.
        assert_eq!(progress(49, 0, 100), 0.495);
        assert_eq!(progress(50, 0, 100), 0.505);
        assert_eq!(progress(3, 3, 3), 1.0);
    }
}
