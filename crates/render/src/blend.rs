//! Blend modes and the CPU reference for layer compositing.
//!
//! Compositing follows W3C Compositing and Blending Level 1, section 5.9 ("source-over"
//! with a separable blend function B) on premultiplied colour:
//!
//! ```text
//! co = cs (1 - ab) + cb (1 - as) + as ab B(Cb, Cs)
//! ao = as + ab (1 - as)
//! ```
//!
//! where `cs`/`cb` are the premultiplied source (layer) and backdrop colours, `Cs`/`Cb`
//! the same unpremultiplied (0 where alpha is 0), and `as`/`ab` their alphas. The blend
//! functions (W3C section 9.1, plus Add and Subtract, which the spec does not define):
//!
//! | Mode | B(Cb, Cs) |
//! | --- | --- |
//! | Normal | Cs |
//! | Multiply | Cb Cs |
//! | Screen | Cb + Cs - Cb Cs (both at or below 1; above that max(Cb, Cs), see below) |
//! | Overlay | HardLight(Cs, Cb), i.e. hard light with the arguments swapped |
//! | Add | Cb + Cs |
//! | Subtract | max(Cb - Cs, 0) |
//! | Difference | abs(Cb - Cs) |
//! | Soft light | Cs <= 0.5: Cb - (1 - 2 Cs) Cb (1 - Cb); else Cb + (2 Cs - 1)(D(Cb) - Cb), D(Cb) = ((16 Cb - 12) Cb + 4) Cb when Cb <= 0.25, else sqrt(Cb) |
//! | Hard light | Cs <= 0.5: Multiply(Cb, 2 Cs); else Screen(Cb, 2 Cs - 1) |
//! | Colour dodge | Cb <= 0: 0; Cs >= 1: max(1, Cb); else min(max(1, Cb), Cb / (1 - Cs)) |
//! | Colour burn | Cb >= 1: Cb; Cs <= 0: 0; else 1 - min(1, (1 - Cb) / Cs) |
//!
//! Glass evaluates them on linear light in the working space, not on encoded values, so
//! Multiply, Add and Normal are physically meaningful and Overlay or Soft light look
//! somewhat different from an sRGB-space editor's. The formulas assume 0..1; HDR values
//! above 1 extrapolate them, except Screen, which would darken a highlight (Cb + Cs -
//! Cb Cs < Cb once Cb > 1) and takes the larger value instead, as film compositors do,
//! and Colour dodge and burn, whose cap at 1 applies only to backdrops at or below 1 so
//! a black dodge layer or a white burn layer leaves HDR highlights as they are (for
//! 0..1 values both are exactly the W3C functions).
//! Negative components (out-of-gamut after a gamut conversion) pass through; Soft light's
//! square root sees 0 for them.

/// A layer's blend mode. The ids are stable (projects and keyframes store them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Add,
    Subtract,
    Difference,
    SoftLight,
    HardLight,
    ColourDodge,
    ColourBurn,
}

impl BlendMode {
    pub const ALL: [BlendMode; 11] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::Add,
        Self::Subtract,
        Self::Difference,
        Self::SoftLight,
        Self::HardLight,
        Self::ColourDodge,
        Self::ColourBurn,
    ];

    /// The stable id stored in projects.
    pub fn id(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Multiply => "multiply",
            Self::Screen => "screen",
            Self::Overlay => "overlay",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Difference => "difference",
            Self::SoftLight => "soft-light",
            Self::HardLight => "hard-light",
            Self::ColourDodge => "colour-dodge",
            Self::ColourBurn => "colour-burn",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }

    /// The code the composite shader switches on.
    pub fn code(self) -> u32 {
        self as u32
    }

    /// B(Cb, Cs) for one unpremultiplied channel.
    pub fn blend(self, cb: f64, cs: f64) -> f64 {
        match self {
            Self::Normal => cs,
            Self::Multiply => cb * cs,
            Self::Screen => screen(cb, cs),
            Self::Overlay => hard_light(cs, cb),
            Self::Add => cb + cs,
            Self::Subtract => (cb - cs).max(0.0),
            Self::Difference => (cb - cs).abs(),
            Self::SoftLight => {
                if cs <= 0.5 {
                    cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
                } else {
                    let d = if cb <= 0.25 {
                        ((16.0 * cb - 12.0) * cb + 4.0) * cb
                    } else {
                        cb.max(0.0).sqrt()
                    };
                    cb + (2.0 * cs - 1.0) * (d - cb)
                }
            }
            Self::HardLight => hard_light(cb, cs),
            Self::ColourDodge => {
                if cb <= 0.0 {
                    0.0
                } else if cs >= 1.0 {
                    cb.max(1.0)
                } else {
                    (cb / (1.0 - cs)).min(cb.max(1.0))
                }
            }
            Self::ColourBurn => {
                if cb >= 1.0 {
                    cb
                } else if cs <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - cb) / cs).min(1.0)
                }
            }
        }
    }
}

fn screen(cb: f64, cs: f64) -> f64 {
    if cb > 1.0 || cs > 1.0 {
        cb.max(cs)
    } else {
        cb + cs - cb * cs
    }
}

fn hard_light(cb: f64, cs: f64) -> f64 {
    if cs <= 0.5 {
        cb * 2.0 * cs
    } else {
        screen(cb, 2.0 * cs - 1.0)
    }
}

/// Composites premultiplied `src` over premultiplied `dst` with `mode`.
pub fn composite(mode: BlendMode, src: [f64; 4], dst: [f64; 4]) -> [f64; 4] {
    let (a_s, a_b) = (src[3], dst[3]);
    let unpremultiply = |c: f64, a: f64| if a > 0.0 { c / a } else { 0.0 };
    let mut out = [0.0; 4];
    for i in 0..3 {
        let b = mode.blend(unpremultiply(dst[i], a_b), unpremultiply(src[i], a_s));
        out[i] = src[i] * (1.0 - a_b) + dst[i] * (1.0 - a_s) + a_s * a_b * b;
    }
    out[3] = a_s + a_b * (1.0 - a_s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_codes_are_dense() {
        for (i, mode) in BlendMode::ALL.into_iter().enumerate() {
            assert_eq!(BlendMode::from_id(mode.id()), Some(mode));
            assert_eq!(mode.code(), i as u32);
        }
    }

    #[test]
    fn normal_is_source_over() {
        let src = [0.2, 0.1, 0.0, 0.5];
        let dst = [0.4, 0.4, 0.4, 1.0];
        let out = composite(BlendMode::Normal, src, dst);
        for i in 0..3 {
            assert!((out[i] - (src[i] + dst[i] * 0.5)).abs() < 1e-12);
        }
        assert_eq!(out[3], 1.0);
    }

    #[test]
    fn w3c_reference_values() {
        // Opaque layers, so the result is B(Cb, Cs).
        let cases = [
            (BlendMode::Multiply, 0.5, 0.4, 0.2),
            (BlendMode::Screen, 0.5, 0.4, 0.7),
            (BlendMode::Overlay, 0.25, 0.6, 0.3),
            (BlendMode::Overlay, 0.75, 0.6, 0.8),
            (BlendMode::HardLight, 0.6, 0.25, 0.3),
            (BlendMode::HardLight, 0.6, 0.75, 0.8),
            (BlendMode::SoftLight, 0.5, 0.25, 0.375),
            (BlendMode::SoftLight, 0.64, 0.75, 0.72),
            (BlendMode::ColourDodge, 0.3, 0.5, 0.6),
            (BlendMode::ColourDodge, 0.6, 0.5, 1.0),
            (BlendMode::ColourBurn, 0.7, 0.5, 0.4),
            (BlendMode::ColourBurn, 0.2, 0.5, 0.0),
            (BlendMode::Difference, 0.2, 0.7, 0.5),
            (BlendMode::Add, 0.2, 0.7, 0.9),
            (BlendMode::Subtract, 0.2, 0.7, 0.0),
            (BlendMode::Subtract, 0.7, 0.2, 0.5),
        ];
        for (mode, cb, cs, want) in cases {
            let out = composite(mode, [cs, cs, cs, 1.0], [cb, cb, cb, 1.0]);
            assert!(
                (out[0] - want).abs() < 1e-12,
                "{mode:?}({cb}, {cs}) = {out:?}"
            );
        }
    }

    #[test]
    fn transparent_layer_changes_nothing_and_clear_backdrop_shows_layer() {
        let dst = [0.3, 0.2, 0.1, 0.8];
        let src = [0.4, 0.5, 0.6, 0.9];
        for mode in BlendMode::ALL {
            assert_eq!(composite(mode, [0.0; 4], dst), dst, "{mode:?}");
            let out = composite(mode, src, [0.0; 4]);
            for i in 0..4 {
                assert!((out[i] - src[i]).abs() < 1e-12, "{mode:?}");
            }
        }
    }

    #[test]
    fn screen_keeps_hdr_highlights() {
        assert_eq!(BlendMode::Screen.blend(2.0, 0.5), 2.0);
    }

    #[test]
    fn neutral_dodge_and_burn_layers_keep_hdr_highlights() {
        assert_eq!(BlendMode::ColourDodge.blend(2.0, 0.0), 2.0);
        assert_eq!(BlendMode::ColourDodge.blend(2.0, 1.0), 2.0);
        assert_eq!(BlendMode::ColourDodge.blend(0.5, 1.0), 1.0);
        assert_eq!(BlendMode::ColourBurn.blend(2.0, 1.0), 2.0);
        assert_eq!(BlendMode::ColourBurn.blend(2.0, 0.0), 2.0);
    }
}
