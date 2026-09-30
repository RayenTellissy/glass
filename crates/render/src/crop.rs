//! Layer crop with a soft edge, and its CPU reference.
//!
//! A [`Crop`] trims each edge of a layer by some layer pixels (before the transform, so
//! the cut turns and scales with the layer) and softens the resulting rectangle's edges
//! over `feather` layer pixels inward. The uncropped rectangle is the layer itself, so a
//! feather with no crop softens the layer's own edges.
//!
//! Coverage of one edge for a pixel whose centre lies `d` layer pixels inside it, where a
//! canvas pixel spans `footprint` layer pixels across it:
//!
//! ```text
//! t = clamp((d + footprint / 2) / (feather + footprint), 0, 1)
//! coverage = t                  (feather 0: the pixel's box filtered over the edge)
//!          = t^2 (3 - 2 t)      (feather > 0: a smooth ramp)
//! ```
//!
//! and a pixel's coverage is the product over the four edges. The ramp is 0 half a pixel
//! outside the edge, 0.5 at `feather / 2` inside it and 1 half a pixel past `feather`.

/// Per-edge crop in layer pixels, and the soft edge width. All zero is no crop.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Crop {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    /// Width of the soft edge inside the crop rectangle, in layer pixels.
    pub feather: f64,
}

impl Crop {
    /// The visible rectangle of a layer of `size` pixels: left, top, right and bottom
    /// edges in layer pixels. Negative amounts count as 0; an empty rectangle has right
    /// at or before left (or bottom at or before top).
    pub fn rect(&self, size: (u32, u32)) -> [f64; 4] {
        let (w, h) = (f64::from(size.0), f64::from(size.1));
        [
            self.left.max(0.0),
            self.top.max(0.0),
            w - self.right.max(0.0),
            h - self.bottom.max(0.0),
        ]
    }

    pub fn feather(&self) -> f64 {
        self.feather.max(0.0)
    }

    /// Coverage (0..1) at the layer point `p` for a canvas pixel spanning `footprint`
    /// layer pixels along x and y.
    pub fn coverage(&self, size: (u32, u32), p: [f64; 2], footprint: [f64; 2]) -> f64 {
        let [x0, y0, x1, y1] = self.rect(size);
        if x1 <= x0 || y1 <= y0 {
            return 0.0;
        }
        let f = self.feather();
        edge(p[0] - x0, f, footprint[0])
            * edge(x1 - p[0], f, footprint[0])
            * edge(p[1] - y0, f, footprint[1])
            * edge(y1 - p[1], f, footprint[1])
    }
}

/// Coverage of one edge for a pixel centre `d` layer pixels inside it (negative outside),
/// with the soft edge `feather` wide and the pixel `footprint` layer pixels across.
pub fn edge(d: f64, feather: f64, footprint: f64) -> f64 {
    let t = ((d + 0.5 * footprint) / (feather + footprint).max(1e-9)).clamp(0.0, 1.0);
    if feather > 0.0 {
        t * t * (3.0 - 2.0 * t)
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_edges_box_filter_the_pixel() {
        assert_eq!(edge(0.5, 0.0, 1.0), 1.0);
        assert_eq!(edge(-0.5, 0.0, 1.0), 0.0);
        assert_eq!(edge(0.0, 0.0, 1.0), 0.5);
        assert_eq!(edge(0.25, 0.0, 2.0), 0.625);
    }

    #[test]
    fn feather_ramps_smoothly_inward() {
        let f = 8.0;
        assert_eq!(edge(-0.5, f, 1.0), 0.0);
        assert!((edge(f / 2.0, f, 1.0) - 0.5).abs() < 1e-12);
        assert_eq!(edge(f + 0.5, f, 1.0), 1.0);
        let ramp: Vec<f64> = (0..=20)
            .map(|i| edge(f64::from(i) * 0.5 - 0.5, f, 1.0))
            .collect();
        assert!(ramp.windows(2).all(|w| w[1] >= w[0]), "{ramp:?}");
        // Symmetric about the middle.
        for d in [0.0, 1.0, 2.5] {
            let a = edge(f / 2.0 - d, f, 1.0);
            let b = edge(f / 2.0 + d, f, 1.0);
            assert!((a + b - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn crop_rect_and_coverage() {
        let crop = Crop {
            left: 3.0,
            top: 1.0,
            right: 2.5,
            bottom: 0.0,
            feather: 0.0,
        };
        assert_eq!(crop.rect((10, 8)), [3.0, 1.0, 7.5, 8.0]);
        let at = |x: f64, y: f64| crop.coverage((10, 8), [x, y], [1.0, 1.0]);
        assert_eq!(at(2.5, 4.5), 0.0);
        assert_eq!(at(3.5, 4.5), 1.0);
        assert_eq!(at(7.5, 4.5), 0.5);
        assert_eq!(at(3.5, 0.5), 0.0);
        assert_eq!(at(3.5, 7.5), 1.0);
        let gone = Crop {
            left: 6.0,
            right: 4.0,
            ..Default::default()
        };
        assert_eq!(gone.coverage((10, 8), [5.0, 4.0], [1.0, 1.0]), 0.0);
        assert_eq!(Crop::default().rect((10, 8)), [0.0, 0.0, 10.0, 8.0]);
    }
}
