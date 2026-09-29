//! Tolerant image comparison for snapshots.
//!
//! A pixel differs when any of its RGBA channels differs by more than
//! [`Tolerance::channel`] (out of 255); an image matches when its size is the same and
//! at most [`Tolerance::max_differing_ratio`] of its pixels differ. The channel
//! threshold absorbs anti-aliasing and blending rounding between GPUs; the ratio lets a
//! few edge pixels move while a changed word, colour, border or layout still fails.

/// How far a rendering may drift from its golden.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerance {
    pub channel: u8,
    pub max_differing_ratio: f64,
}

impl Tolerance {
    /// Hardware GPUs: a channel may be off by 16 levels anywhere, and by more on up to
    /// 0.002% of the pixels (3 pixels of a 480 x 320 image at 1x, 12 at 2x). egui
    /// rasterises glyphs on the CPU, so GPUs differ only in filtering and blending
    /// rounding, which stays under the channel threshold; one changed letter at 13 px
    /// ("Proxy" to "Proxv") moves 15 pixels at 1x and 56 at 2x.
    pub const GPU: Tolerance = Tolerance {
        channel: 16,
        max_differing_ratio: 0.000_02,
    };

    /// Software rasterisers (lavapipe, WARP) anti-alias differently: 48 levels, and more
    /// on up to 0.1% of the pixels. That catches a missing or moved control or a wrong
    /// theme, not a single changed letter.
    pub const SOFTWARE: Tolerance = Tolerance {
        channel: 48,
        max_differing_ratio: 0.001,
    };
}

/// RGBA8 pixels, rows top to bottom, tightly packed.
#[derive(Clone, Copy, Debug)]
pub struct Image<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

/// The outcome of a comparison.
#[derive(Clone, Debug, PartialEq)]
pub enum Comparison {
    Match {
        differing: usize,
        max_channel_delta: u8,
    },
    SizeMismatch {
        expected: (u32, u32),
        actual: (u32, u32),
    },
    Mismatch {
        differing: usize,
        total: usize,
        max_channel_delta: u8,
        /// Differing pixels in the diff colour over a faded copy of the golden, RGBA8.
        diff: Vec<u8>,
    },
}

impl Comparison {
    pub fn is_match(&self) -> bool {
        matches!(self, Comparison::Match { .. })
    }
}

impl std::fmt::Display for Comparison {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Comparison::Match {
                differing,
                max_channel_delta,
            } => write!(
                f,
                "matches ({differing} pixels over the threshold, largest channel difference {max_channel_delta})"
            ),
            Comparison::SizeMismatch { expected, actual } => write!(
                f,
                "size {}x{} but the golden is {}x{}",
                actual.0, actual.1, expected.0, expected.1
            ),
            Comparison::Mismatch {
                differing,
                total,
                max_channel_delta,
                ..
            } => write!(
                f,
                "{differing} of {total} pixels differ ({:.3}%), largest channel difference {max_channel_delta}",
                *differing as f64 * 100.0 / (*total).max(1) as f64
            ),
        }
    }
}

/// Compares `actual` with `expected` under `tolerance`. `marker` is the RGBA colour
/// differing pixels get in the diff image.
pub fn compare(
    expected: Image<'_>,
    actual: Image<'_>,
    tolerance: Tolerance,
    marker: [u8; 4],
) -> Comparison {
    if (expected.width, expected.height) != (actual.width, actual.height)
        || expected.rgba.len() != actual.rgba.len()
    {
        return Comparison::SizeMismatch {
            expected: (expected.width, expected.height),
            actual: (actual.width, actual.height),
        };
    }
    let total = expected.rgba.len() / 4;
    let mut differing = 0usize;
    let mut max_channel_delta = 0u8;
    let mut diff = Vec::with_capacity(expected.rgba.len());
    for (e, a) in expected
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(actual.rgba.as_chunks::<4>().0.iter())
    {
        let delta = e
            .iter()
            .zip(a)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        max_channel_delta = max_channel_delta.max(delta);
        if delta > tolerance.channel {
            differing += 1;
            diff.extend_from_slice(&marker);
        } else {
            // A quarter-strength greyscale of the golden, so the diff shows where the
            // changes sit.
            let luma = (u32::from(e[0]) * 3 + u32::from(e[1]) * 6 + u32::from(e[2])) / 10;
            let faded = (luma / 4 + 160) as u8;
            diff.extend_from_slice(&[faded, faded, faded, 255]);
        }
    }
    if differing as f64 <= tolerance.max_differing_ratio * total as f64 {
        Comparison::Match {
            differing,
            max_channel_delta,
        }
    } else {
        Comparison::Mismatch {
            differing,
            total,
            max_channel_delta,
            diff,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARK: [u8; 4] = [255, 0, 255, 255];

    fn image(width: u32, height: u32, rgba: &[u8]) -> Image<'_> {
        Image {
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn small_channel_drift_matches() {
        let golden = vec![100u8; 100 * 100 * 4];
        let mut actual = golden.clone();
        for px in actual.as_chunks_mut::<4>().0.iter_mut() {
            px[0] = 116;
        }
        let result = compare(
            image(100, 100, &golden),
            image(100, 100, &actual),
            Tolerance::GPU,
            MARK,
        );
        assert_eq!(
            result,
            Comparison::Match {
                differing: 0,
                max_channel_delta: 16
            }
        );
    }

    #[test]
    fn a_few_pixels_may_move_but_not_more() {
        let golden = vec![0u8; 500 * 500 * 4];
        let mut actual = golden.clone();
        // 5 of 250,000 pixels is the GPU limit (0.002%).
        for px in actual.as_chunks_mut::<4>().0.iter_mut().take(5) {
            px[1] = 255;
        }
        let ok = compare(
            image(500, 500, &golden),
            image(500, 500, &actual),
            Tolerance::GPU,
            MARK,
        );
        assert!(ok.is_match(), "{ok}");
        actual[5 * 4 + 1] = 255;
        let bad = compare(
            image(500, 500, &golden),
            image(500, 500, &actual),
            Tolerance::GPU,
            MARK,
        );
        let Comparison::Mismatch {
            differing, diff, ..
        } = &bad
        else {
            panic!("expected a mismatch, got {bad}");
        };
        assert_eq!(*differing, 6);
        assert_eq!(&diff[..4], &MARK);
        assert_ne!(&diff[6 * 4..6 * 4 + 4], &MARK);
        assert!(
            compare(
                image(500, 500, &golden),
                image(500, 500, &actual),
                Tolerance::SOFTWARE,
                MARK
            )
            .is_match()
        );
    }

    #[test]
    fn size_changes_fail() {
        let a = vec![0u8; 4 * 4 * 4];
        let b = vec![0u8; 4 * 2 * 4];
        assert!(matches!(
            compare(image(4, 4, &a), image(8, 2, &b), Tolerance::SOFTWARE, MARK),
            Comparison::SizeMismatch { .. }
        ));
    }
}
