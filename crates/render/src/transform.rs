//! 2D layer transforms and the CPU reference for resampling a layer through one.
//!
//! Coordinates are pixels with y down; a pixel's centre is at `i + 0.5`. A layer pixel
//! `p` lands on the canvas at `M p`, where
//!
//! ```text
//! M = translate(position) * rotate(rotation) * scale(scale) * translate(-anchor)
//! ```
//!
//! so the anchor point (in layer pixels) sits at `position` (in canvas pixels), scale and
//! rotation happen about it, and positive rotation turns clockwise on screen. The
//! compositor inverts `M` and samples the layer at `M^-1 q` for every canvas pixel centre
//! `q`, inside the composite pass (no intermediate texture per layer); positions are never
//! rounded, so subpixel moves resample. The identity puts the layer at the canvas's top
//! left, unscaled.
//!
//! Resampling ([`Resample`]) is nearest, bilinear or bicubic (Catmull-Rom, B = 0,
//! C = 0.5) on premultiplied texels with the edge texels extended; the layer's own
//! rectangle then limits it, antialiased over the canvas pixel's footprint. A transform
//! that maps every canvas pixel centre onto a layer pixel centre (whole-pixel moves,
//! quarter turns, flips) is sampled with nearest whatever the setting, which is exact
//! and the cheapest.
//!
//! Motion blur (Phase 13) needs a layer's transform at several times across the shutter
//! interval; [`ShutterTransforms`] holds them. The compositor renders the sample at the
//! middle of the interval until then.

/// `p -> (a x + c y + tx, b x + d y + ty)`, CSS `matrix(a, b, c, d, tx, ty)` order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Default for Affine2 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine2 {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub fn translate(x: f64, y: f64) -> Self {
        Self {
            tx: x,
            ty: y,
            ..Self::IDENTITY
        }
    }

    pub fn scale(x: f64, y: f64) -> Self {
        Self {
            a: x,
            d: y,
            ..Self::IDENTITY
        }
    }

    /// Clockwise on screen (y down) by `degrees`. Whole quarter turns are exact.
    pub fn rotate(degrees: f64) -> Self {
        let turns = degrees / 90.0;
        let (s, c) = if turns == turns.round() && turns.abs() < 1e15 {
            match (turns as i64).rem_euclid(4) {
                0 => (0.0, 1.0),
                1 => (1.0, 0.0),
                2 => (0.0, -1.0),
                _ => (-1.0, 0.0),
            }
        } else {
            degrees.to_radians().sin_cos()
        };
        Self {
            a: c,
            b: s,
            c: -s,
            d: c,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// `self * other`: applies `other` first, then `self`.
    pub fn then_apply(&self, other: &Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            tx: self.a * other.tx + self.c * other.ty + self.tx,
            ty: self.b * other.tx + self.d * other.ty + self.ty,
        }
    }

    pub fn apply(&self, [x, y]: [f64; 2]) -> [f64; 2] {
        [
            self.a * x + self.c * y + self.tx,
            self.b * x + self.d * y + self.ty,
        ]
    }

    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// `None` when the matrix collapses the layer (zero scale).
    pub fn inverse(&self) -> Option<Self> {
        let det = self.determinant();
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Self {
            a,
            b,
            c,
            d,
            tx: -(a * self.tx + c * self.ty),
            ty: -(b * self.tx + d * self.ty),
        })
    }

    /// Whether every pixel centre maps onto a pixel centre: the linear part only swaps
    /// and flips axes, and the pixel at the origin lands on a pixel centre.
    pub fn maps_pixel_centres(&self) -> bool {
        let unit = |v: f64| v == 0.0 || v.abs() == 1.0;
        if ![self.a, self.b, self.c, self.d].into_iter().all(unit)
            || self.determinant().abs() != 1.0
        {
            return false;
        }
        let [x, y] = self.apply([0.5, 0.5]);
        let whole = |v: f64| (v - 0.5 - (v - 0.5).round()).abs() < 1e-9;
        whole(x) && whole(y)
    }

    /// How far one canvas pixel reaches in the other space along x and y, for a matrix
    /// from canvas to layer pixels: the extents of the pixel's image.
    pub fn footprint(&self) -> [f64; 2] {
        [self.a.abs() + self.c.abs(), self.b.abs() + self.d.abs()]
    }
}

/// A layer's transform as the inspector shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform2D {
    /// Where the anchor lands, in canvas pixels.
    pub position: [f64; 2],
    /// 1.0 is 100%, per axis; negative flips.
    pub scale: [f64; 2],
    /// Degrees, clockwise.
    pub rotation: f64,
    /// The point scale and rotation happen about, in layer pixels.
    pub anchor: [f64; 2],
}

impl Default for Transform2D {
    fn default() -> Self {
        Self {
            position: [0.0; 2],
            scale: [1.0; 2],
            rotation: 0.0,
            anchor: [0.0; 2],
        }
    }
}

impl Transform2D {
    /// A layer centred on the canvas at 100%, anchored at its centre: the default for a
    /// new clip.
    pub fn centred(layer: (u32, u32), canvas: (u32, u32)) -> Self {
        Self {
            position: [f64::from(canvas.0) / 2.0, f64::from(canvas.1) / 2.0],
            anchor: [f64::from(layer.0) / 2.0, f64::from(layer.1) / 2.0],
            ..Self::default()
        }
    }

    /// Layer pixels to canvas pixels.
    pub fn matrix(&self) -> Affine2 {
        Affine2::translate(self.position[0], self.position[1])
            .then_apply(&Affine2::rotate(self.rotation))
            .then_apply(&Affine2::scale(self.scale[0], self.scale[1]))
            .then_apply(&Affine2::translate(-self.anchor[0], -self.anchor[1]))
    }
}

impl From<Transform2D> for Affine2 {
    fn from(t: Transform2D) -> Self {
        t.matrix()
    }
}

/// The most shutter samples one layer holds.
pub const MAX_SHUTTER_SAMPLES: usize = 16;

/// A layer's transform (layer to canvas pixels) across the shutter interval: samples
/// evenly spaced in time from shutter open to close. One sample is a still transform.
/// Motion blur (Phase 13) will fill several from the keyframed transform at sub-frame
/// times and average them; until then the compositor renders [`Self::centre`].
#[derive(Clone, Debug, PartialEq)]
pub struct ShutterTransforms {
    samples: Vec<Affine2>,
}

impl Default for ShutterTransforms {
    fn default() -> Self {
        Self::still(Affine2::IDENTITY)
    }
}

impl From<Affine2> for ShutterTransforms {
    fn from(m: Affine2) -> Self {
        Self::still(m)
    }
}

impl From<Transform2D> for ShutterTransforms {
    fn from(t: Transform2D) -> Self {
        Self::still(t.matrix())
    }
}

impl ShutterTransforms {
    pub fn still(m: Affine2) -> Self {
        Self { samples: vec![m] }
    }

    /// Samples across the shutter interval, in time order; more than
    /// [`MAX_SHUTTER_SAMPLES`] are thinned evenly (first and last kept). An empty list is
    /// a still identity.
    pub fn new(samples: Vec<Affine2>) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        if samples.len() <= MAX_SHUTTER_SAMPLES {
            return Self { samples };
        }
        let n = samples.len();
        let picked = (0..MAX_SHUTTER_SAMPLES)
            .map(|i| samples[i * (n - 1) / (MAX_SHUTTER_SAMPLES - 1)])
            .collect();
        Self { samples: picked }
    }

    /// `count` samples of `at(t)` for `t` evenly across `0..=1` (shutter open to close);
    /// one sample is taken at 0.5.
    pub fn sample(count: usize, at: impl Fn(f64) -> Affine2) -> Self {
        let count = count.clamp(1, MAX_SHUTTER_SAMPLES);
        if count == 1 {
            return Self::still(at(0.5));
        }
        Self::new(
            (0..count)
                .map(|i| at(i as f64 / (count - 1) as f64))
                .collect(),
        )
    }

    pub fn samples(&self) -> &[Affine2] {
        &self.samples
    }

    pub fn is_still(&self) -> bool {
        self.samples.len() == 1
    }

    /// The transform at the middle of the shutter interval (the middle sample, or the
    /// average of the two middle ones).
    pub fn centre(&self) -> Affine2 {
        let n = self.samples.len();
        let (lo, hi) = (self.samples[(n - 1) / 2], self.samples[n / 2]);
        let mid = |x: f64, y: f64| 0.5 * (x + y);
        Affine2 {
            a: mid(lo.a, hi.a),
            b: mid(lo.b, hi.b),
            c: mid(lo.c, hi.c),
            d: mid(lo.d, hi.d),
            tx: mid(lo.tx, hi.tx),
            ty: mid(lo.ty, hi.ty),
        }
    }
}

/// How a transformed layer is resampled, cheapest first. The codes are what the composite
/// shader switches on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Resample {
    /// The texel under the point: pixel art, and the fastest.
    Nearest,
    /// Hardware bilinear: softer, as cheap as nearest (the pass is bound by memory
    /// bandwidth).
    Bilinear,
    /// Catmull-Rom from 4 x 4 texels (nine bilinear fetches on the GPU): sharp and
    /// smooth, the default; about twice the GPU time of bilinear for a full-frame layer.
    #[default]
    Bicubic,
}

impl Resample {
    pub fn id(self) -> &'static str {
        match self {
            Self::Nearest => "nearest",
            Self::Bilinear => "bilinear",
            Self::Bicubic => "bicubic",
        }
    }

    pub fn code(self) -> u32 {
        self as u32
    }
}

/// Catmull-Rom weights for the four texels around a point `f` (0..1) past the second.
pub fn catmull_rom_weights(f: f64) -> [f64; 4] {
    [
        f * (-0.5 + f * (1.0 - 0.5 * f)),
        1.0 + f * f * (-2.5 + 1.5 * f),
        f * (0.5 + f * (2.0 - 1.5 * f)),
        f * f * (-0.5 + 0.5 * f),
    ]
}

/// The CPU reference for sampling premultiplied `texels` (row-major, `size` pixels) at
/// the layer point `p`, with the edge texels extended as the GPU sampler does. Bicubic
/// output keeps alpha in 0..1 and colour at or above 0, as the shader does.
pub fn resample(texels: &[[f32; 4]], size: (u32, u32), p: [f64; 2], filter: Resample) -> [f64; 4] {
    let (w, h) = (i64::from(size.0), i64::from(size.1));
    let texel = |x: i64, y: i64| {
        let (x, y) = (x.clamp(0, w - 1), y.clamp(0, h - 1));
        texels[(y * w + x) as usize].map(f64::from)
    };
    match filter {
        Resample::Nearest => texel(p[0].floor() as i64, p[1].floor() as i64),
        Resample::Bilinear => {
            let (x, y) = (p[0] - 0.5, p[1] - 0.5);
            let (x0, y0) = (x.floor(), y.floor());
            let (fx, fy) = (x - x0, y - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let mut out = [0.0; 4];
            for (dx, dy, wt) in [
                (0, 0, (1.0 - fx) * (1.0 - fy)),
                (1, 0, fx * (1.0 - fy)),
                (0, 1, (1.0 - fx) * fy),
                (1, 1, fx * fy),
            ] {
                let t = texel(x0 + dx, y0 + dy);
                for c in 0..4 {
                    out[c] += t[c] * wt;
                }
            }
            out
        }
        Resample::Bicubic => {
            let (x, y) = (p[0] - 0.5, p[1] - 0.5);
            let (x0, y0) = (x.floor(), y.floor());
            let (wx, wy) = (catmull_rom_weights(x - x0), catmull_rom_weights(y - y0));
            let (x0, y0) = (x0 as i64, y0 as i64);
            let mut out = [0.0; 4];
            for (j, wy) in wy.into_iter().enumerate() {
                for (i, wx) in wx.into_iter().enumerate() {
                    let t = texel(x0 - 1 + i as i64, y0 - 1 + j as i64);
                    for c in 0..4 {
                        out[c] += t[c] * wx * wy;
                    }
                }
            }
            [
                out[0].max(0.0),
                out[1].max(0.0),
                out[2].max(0.0),
                out[3].clamp(0.0, 1.0),
            ]
        }
    }
}

/// Coverage of one hard edge for a pixel whose centre is `d` layer pixels inside it
/// (negative outside), box-filtered over the pixel's `footprint` in layer pixels.
pub fn edge_coverage(d: f64, footprint: f64) -> f64 {
    ((d + 0.5 * footprint) / footprint.max(1e-9)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f64; 2]) {
        assert!(
            (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9,
            "{a:?} vs {b:?}"
        );
    }

    #[test]
    fn anchor_lands_on_position_and_rotation_is_clockwise() {
        let t = Transform2D {
            position: [100.0, 50.0],
            scale: [2.0, 3.0],
            rotation: 90.0,
            anchor: [10.0, 10.0],
        };
        let m = t.matrix();
        close(m.apply([10.0, 10.0]), [100.0, 50.0]);
        // One pixel right of the anchor goes two pixels down (clockwise, y down).
        close(m.apply([11.0, 10.0]), [100.0, 52.0]);
        // One pixel below the anchor goes three pixels left.
        close(m.apply([10.0, 11.0]), [97.0, 50.0]);
        let inv = m.inverse().expect("invertible");
        close(inv.apply(m.apply([3.0, 7.0])), [3.0, 7.0]);
    }

    #[test]
    fn anchor_is_the_fixed_point_of_scale_and_rotation() {
        for rotation in [0.0, 17.0, 90.0, -135.0] {
            for scale in [[0.5, 2.0], [-1.0, 1.0], [3.0, 3.0]] {
                let t = Transform2D {
                    position: [40.0, 30.0],
                    scale,
                    rotation,
                    anchor: [12.5, 7.25],
                };
                close(t.matrix().apply(t.anchor), t.position);
            }
        }
        let c = Transform2D::centred((1920, 1080), (3840, 2160));
        close(c.matrix().apply([0.0, 0.0]), [960.0, 540.0]);
        close(c.matrix().apply([1920.0, 1080.0]), [2880.0, 1620.0]);
    }

    #[test]
    fn quarter_turns_are_exact_and_map_pixel_centres() {
        assert_eq!(Affine2::rotate(90.0).a, 0.0);
        assert_eq!(Affine2::rotate(-270.0), Affine2::rotate(90.0));
        assert_eq!(Affine2::rotate(360.0), Affine2::IDENTITY);
        let t = Transform2D {
            position: [16.0, 16.0],
            rotation: 90.0,
            anchor: [8.0, 4.0],
            ..Default::default()
        };
        assert!(t.matrix().inverse().expect("inv").maps_pixel_centres());
        assert!(Affine2::translate(3.0, -2.0).maps_pixel_centres());
        assert!(Affine2::scale(-1.0, 1.0).maps_pixel_centres());
        assert!(!Affine2::translate(0.5, 0.0).maps_pixel_centres());
        assert!(!Affine2::rotate(1.0).maps_pixel_centres());
        assert!(!Affine2::scale(2.0, 2.0).maps_pixel_centres());
        // Anchored half a pixel off a corner, a quarter turn lands centres between pixels.
        let off = Transform2D {
            position: [10.0, 10.0],
            rotation: 90.0,
            anchor: [0.5, 0.0],
            ..Default::default()
        };
        assert!(!off.matrix().maps_pixel_centres());
    }

    #[test]
    fn zero_scale_has_no_inverse_and_footprint_follows_scale() {
        assert!(Affine2::scale(0.0, 1.0).inverse().is_none());
        let m = Affine2::scale(2.0, 0.5).inverse().expect("inv");
        assert_eq!(m.footprint(), [0.5, 2.0]);
        let r = Affine2::rotate(45.0).footprint();
        assert!((r[0] - 2f64.sqrt()).abs() < 1e-12);
    }

    #[test]
    fn shutter_samples_are_even_and_capped() {
        let s = ShutterTransforms::sample(3, |t| Affine2::translate(t * 4.0, 0.0));
        let xs: Vec<f64> = s.samples().iter().map(|m| m.tx).collect();
        assert_eq!(xs, [0.0, 2.0, 4.0]);
        assert_eq!(s.centre().tx, 2.0);
        let open_close = ShutterTransforms::new(vec![
            Affine2::translate(0.0, 0.0),
            Affine2::translate(6.0, 2.0),
        ]);
        assert_eq!(open_close.centre(), Affine2::translate(3.0, 1.0));
        assert!(!open_close.is_still());
        let many = ShutterTransforms::new(
            (0..40)
                .map(|i| Affine2::translate(f64::from(i), 0.0))
                .collect(),
        );
        assert_eq!(many.samples().len(), MAX_SHUTTER_SAMPLES);
        assert_eq!(many.samples()[0].tx, 0.0);
        assert_eq!(many.samples()[MAX_SHUTTER_SAMPLES - 1].tx, 39.0);
        assert_eq!(
            ShutterTransforms::new(Vec::new()),
            ShutterTransforms::default()
        );
    }

    #[test]
    fn filters_are_exact_on_texel_centres_and_bicubic_interpolates() {
        let texels: Vec<[f32; 4]> = (0..16).map(|i| [i as f32 / 16.0, 0.5, 0.0, 1.0]).collect();
        for filter in [Resample::Nearest, Resample::Bilinear, Resample::Bicubic] {
            for i in 0..16 {
                let p = [(i % 4) as f64 + 0.5, (i / 4) as f64 + 0.5];
                let got = resample(&texels, (4, 4), p, filter);
                assert_eq!(got, texels[i].map(f64::from), "{filter:?} {i}");
            }
        }
        // A linear ramp is reproduced exactly between centres by both smooth filters.
        for filter in [Resample::Bilinear, Resample::Bicubic] {
            let got = resample(&texels, (4, 4), [1.75, 1.5], filter);
            assert!(
                (got[0] - 1.25 / 16.0 - 4.0 / 16.0).abs() < 1e-12,
                "{filter:?}"
            );
        }
        for f in [0.0, 0.3, 0.5, 0.9] {
            let w = catmull_rom_weights(f);
            assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn edge_coverage_box_filters() {
        assert_eq!(edge_coverage(0.5, 1.0), 1.0);
        assert_eq!(edge_coverage(-0.5, 1.0), 0.0);
        assert_eq!(edge_coverage(0.0, 1.0), 0.5);
        assert_eq!(edge_coverage(0.25, 2.0), 0.625);
    }
}
