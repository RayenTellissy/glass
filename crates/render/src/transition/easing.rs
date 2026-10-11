//! Easing curves: how a transition's progress moves from 0 to 1 over its duration.

/// Maps linear progress (0..1) to eased progress. Every curve starts at exactly 0 and
/// ends at exactly 1. The named eases are the CSS ones: `ease-in` is
/// `cubic-bezier(0.42, 0, 1, 1)`, `ease-out` `(0, 0, 0.58, 1)` and `ease-in-out`
/// `(0.42, 0, 0.58, 1)`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Easing {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// A CSS `cubic-bezier(x1, y1, x2, y2)` from (0, 0) to (1, 1). The x of each control
    /// point is clamped to 0..1 so the curve stays a function of time; y may leave 0..1
    /// to overshoot.
    CubicBezier {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
}

impl Easing {
    /// Eased progress at linear progress `t`, clamped to 0..1 first.
    pub fn apply(self, t: f64) -> f64 {
        if t.is_nan() || t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        let (x1, y1, x2, y2) = match self {
            Self::Linear => return t,
            Self::EaseIn => (0.42, 0.0, 1.0, 1.0),
            Self::EaseOut => (0.0, 0.0, 0.58, 1.0),
            Self::EaseInOut => (0.42, 0.0, 0.58, 1.0),
            Self::CubicBezier { x1, y1, x2, y2 } => (x1, y1, x2, y2),
        };
        let (x1, x2) = (clamp_unit(x1), clamp_unit(x2));
        bezier(y1, y2, solve_x(x1, x2, t))
    }
}

fn clamp_unit(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// One coordinate of the cubic from 0 through control values `c1`, `c2` to 1 at `s`.
fn bezier(c1: f64, c2: f64, s: f64) -> f64 {
    let u = 1.0 - s;
    3.0 * u * u * s * c1 + 3.0 * u * s * s * c2 + s * s * s
}

fn bezier_slope(c1: f64, c2: f64, s: f64) -> f64 {
    let u = 1.0 - s;
    3.0 * u * u * c1 + 6.0 * u * s * (c2 - c1) + 3.0 * s * s * (1.0 - c2)
}

/// The curve parameter whose x is `x`. With both control x in 0..1, x is monotonic in
/// the parameter, so Newton's method converges from `x` in a few steps; where the slope
/// vanishes (a flat start or end), bisection finishes the job.
fn solve_x(x1: f64, x2: f64, x: f64) -> f64 {
    const EPSILON: f64 = 1e-12;
    let mut s = x;
    for _ in 0..8 {
        let error = bezier(x1, x2, s) - x;
        if error.abs() < EPSILON {
            return s;
        }
        let slope = bezier_slope(x1, x2, s);
        if slope.abs() < 1e-9 {
            break;
        }
        s = (s - error / slope).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    s = x;
    for _ in 0..64 {
        let value = bezier(x1, x2, s);
        if (value - x).abs() < EPSILON {
            break;
        }
        if value < x {
            lo = s;
        } else {
            hi = s;
        }
        s = 0.5 * (lo + hi);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Easing; 6] = [
        Easing::Linear,
        Easing::EaseIn,
        Easing::EaseOut,
        Easing::EaseInOut,
        Easing::CubicBezier {
            x1: 0.68,
            y1: -0.55,
            x2: 0.27,
            y2: 1.55,
        },
        Easing::CubicBezier {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
        },
    ];

    #[test]
    fn every_curve_starts_at_zero_and_ends_at_one_exactly() {
        for easing in ALL {
            assert_eq!(easing.apply(0.0), 0.0, "{easing:?}");
            assert_eq!(easing.apply(1.0), 1.0, "{easing:?}");
            assert_eq!(easing.apply(-0.5), 0.0, "{easing:?}");
            assert_eq!(easing.apply(1.5), 1.0, "{easing:?}");
            assert_eq!(easing.apply(f64::NAN), 0.0, "{easing:?}");
        }
    }

    #[test]
    fn linear_and_the_straight_bezier_are_the_identity() {
        for i in 0..=100 {
            let t = f64::from(i) / 100.0;
            assert_eq!(Easing::Linear.apply(t), t);
            let straight = Easing::CubicBezier {
                x1: 0.25,
                y1: 0.25,
                x2: 0.75,
                y2: 0.75,
            };
            assert!((straight.apply(t) - t).abs() < 1e-9, "{t}");
        }
    }

    /// Values of the CSS curves, from the cubic evaluated with a fine parameter sweep
    /// (independent of the solver).
    #[test]
    fn named_eases_match_the_css_curves() {
        for (easing, (x1, y1, x2, y2)) in [
            (Easing::EaseIn, (0.42, 0.0, 1.0, 1.0)),
            (Easing::EaseOut, (0.0, 0.0, 0.58, 1.0)),
            (Easing::EaseInOut, (0.42, 0.0, 0.58, 1.0)),
        ] {
            for i in 1..1000 {
                let s = f64::from(i) / 1000.0;
                let (x, y) = (bezier(x1, x2, s), bezier(y1, y2, s));
                let got = easing.apply(x);
                assert!((got - y).abs() < 1e-9, "{easing:?} at x {x}: {got} vs {y}");
            }
        }
        // Known points: ease-in-out is symmetric about the middle.
        assert!((Easing::EaseInOut.apply(0.5) - 0.5).abs() < 1e-12);
        for i in 1..50 {
            let t = f64::from(i) / 100.0;
            let a = Easing::EaseInOut.apply(t);
            let b = Easing::EaseInOut.apply(1.0 - t);
            assert!((a + b - 1.0).abs() < 1e-9, "{t}");
        }
    }

    #[test]
    fn eases_bend_the_right_way_and_stay_monotonic() {
        for i in 1..100 {
            let t = f64::from(i) / 100.0;
            assert!(Easing::EaseIn.apply(t) < t, "ease-in starts slow at {t}");
            assert!(Easing::EaseOut.apply(t) > t, "ease-out starts fast at {t}");
        }
        for easing in &ALL[..4] {
            let mut last = 0.0;
            for i in 1..=1000 {
                let v = easing.apply(f64::from(i) / 1000.0);
                assert!(v >= last, "{easing:?} at {i}");
                last = v;
            }
        }
    }

    #[test]
    fn a_bezier_may_overshoot_but_its_x_is_clamped() {
        let back = ALL[4];
        let values: Vec<f64> = (0..=200)
            .map(|i| back.apply(f64::from(i) / 200.0))
            .collect();
        assert!(values.iter().any(|&v| v < 0.0), "anticipates below 0");
        assert!(values.iter().any(|&v| v > 1.0), "overshoots above 1");
        // x outside 0..1 is clamped: the curve with x1 = -1, x2 = 2 equals x1 = 0, x2 = 1.
        let wild = Easing::CubicBezier {
            x1: -1.0,
            y1: 0.1,
            x2: 2.0,
            y2: 0.9,
        };
        let clamped = Easing::CubicBezier {
            x1: 0.0,
            y1: 0.1,
            x2: 1.0,
            y2: 0.9,
        };
        for i in 0..=20 {
            let t = f64::from(i) / 20.0;
            assert_eq!(wild.apply(t), clamped.apply(t));
        }
    }

    #[test]
    fn flat_ends_still_solve() {
        // x1 = 0 and x2 = 1 make dx/ds vanish at both ends, where Newton stalls.
        let flat = ALL[5];
        for t in [1e-9, 1e-6, 0.001, 0.999, 1.0 - 1e-6] {
            let s = solve_x(0.0, 1.0, t);
            assert!((bezier(0.0, 1.0, s) - t).abs() < 1e-9, "{t}");
            assert!((flat.apply(t) - bezier(0.0, 1.0, s)).abs() < 1e-9);
        }
    }
}
