//! The working space and gamut conversion.
//!
//! The render graph works in linear light relative to SDR reference white (1.0 = 203
//! nits, as [`crate::yuv`] writes it), premultiplied alpha, in the project's primaries:
//! BT.709 by default and BT.2020 for HDR projects (Decision Log). Sources in other
//! primaries go through a 3x3 matrix, [`gamut_matrix`], derived from the CIE 1931
//! chromaticities of both sets of primaries (SMPTE RP 177), with a Bradford chromatic
//! adaptation when the white points differ (DCI-P3's DCI white to D65).

use media::Primaries;

/// A row-major 3x3 matrix applied to column vectors of linear RGB.
pub type Mat3 = [[f64; 3]; 3];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The primaries and white point the graph works in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkingSpace {
    pub primaries: Primaries,
}

impl WorkingSpace {
    pub const BT709: Self = Self {
        primaries: Primaries::Bt709,
    };
    pub const BT2020: Self = Self {
        primaries: Primaries::Bt2020,
    };

    /// BT.2020 for an HDR (PQ or HLG) project, BT.709 otherwise.
    pub fn for_project(hdr: bool) -> Self {
        if hdr { Self::BT2020 } else { Self::BT709 }
    }

    /// The matrix taking linear RGB in `source` primaries into this space.
    pub fn from(&self, source: Primaries) -> Mat3 {
        gamut_matrix(source, self.primaries)
    }
}

impl Default for WorkingSpace {
    fn default() -> Self {
        Self::BT709
    }
}

/// CIE 1931 xy of red, green, blue and white.
pub fn chromaticities(primaries: Primaries) -> [[f64; 2]; 4] {
    const D65: [f64; 2] = [0.3127, 0.3290];
    match primaries {
        Primaries::Bt709 => [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060], D65],
        Primaries::Bt601Pal => [[0.640, 0.330], [0.290, 0.600], [0.150, 0.060], D65],
        Primaries::Bt601Ntsc => [[0.630, 0.340], [0.310, 0.595], [0.155, 0.070], D65],
        Primaries::Bt2020 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046], D65],
        Primaries::DciP3 => [
            [0.680, 0.320],
            [0.265, 0.690],
            [0.150, 0.060],
            [0.314, 0.351],
        ],
        Primaries::DisplayP3 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060], D65],
    }
}

fn xyz(xy: [f64; 2]) -> [f64; 3] {
    [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
}

/// Linear RGB to CIE XYZ (Y = 1 for white).
pub fn rgb_to_xyz(primaries: Primaries) -> Mat3 {
    let [r, g, b, w] = chromaticities(primaries);
    let (r, g, b) = (xyz(r), xyz(g), xyz(b));
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = apply(&invert(&m), xyz(w));
    std::array::from_fn(|i| std::array::from_fn(|j| m[i][j] * s[j]))
}

const BRADFORD: Mat3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// Bradford chromatic adaptation between two white points (XYZ to XYZ).
pub fn bradford(from: [f64; 2], to: [f64; 2]) -> Mat3 {
    let s = apply(&BRADFORD, xyz(from));
    let d = apply(&BRADFORD, xyz(to));
    let scale = [
        [d[0] / s[0], 0.0, 0.0],
        [0.0, d[1] / s[1], 0.0],
        [0.0, 0.0, d[2] / s[2]],
    ];
    mul(&invert(&BRADFORD), &mul(&scale, &BRADFORD))
}

/// The matrix taking linear RGB in `from` primaries to linear RGB in `to` primaries.
pub fn gamut_matrix(from: Primaries, to: Primaries) -> Mat3 {
    if from == to {
        return IDENTITY;
    }
    let white_from = chromaticities(from)[3];
    let white_to = chromaticities(to)[3];
    let mut m = rgb_to_xyz(from);
    if white_from != white_to {
        m = mul(&bradford(white_from, white_to), &m);
    }
    mul(&invert(&rgb_to_xyz(to)), &m)
}

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

pub fn apply(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

pub fn invert(m: &Mat3) -> Mat3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let co = [
        [e * i - f * h, c * h - b * i, b * f - c * e],
        [f * g - d * i, a * i - c * g, c * d - a * f],
        [d * h - e * g, b * g - a * h, a * e - b * d],
    ];
    let det = a * co[0][0] + b * co[1][0] + c * co[2][0];
    co.map(|row| row.map(|v| v / det))
}

/// The matrix as three `vec4` rows for a WGSL uniform (w unused).
pub fn uniform_rows(m: &Mat3) -> [[f32; 4]; 3] {
    m.map(|r| [r[0] as f32, r[1] as f32, r[2] as f32, 0.0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &Mat3, b: &Mat3, tol: f64) {
        for i in 0..3 {
            for j in 0..3 {
                assert!((a[i][j] - b[i][j]).abs() < tol, "{a:?} vs {b:?} at {i},{j}");
            }
        }
    }

    #[test]
    fn bt709_to_bt2020_matches_bt2087() {
        // ITU-R BT.2087-0, equation (4), to four decimals.
        let expected = [
            [0.6274, 0.3293, 0.0433],
            [0.0691, 0.9195, 0.0114],
            [0.0164, 0.0880, 0.8956],
        ];
        close(
            &gamut_matrix(Primaries::Bt709, Primaries::Bt2020),
            &expected,
            6e-4,
        );
    }

    #[test]
    fn bt709_luma_row_is_the_bt709_weights() {
        let m = rgb_to_xyz(Primaries::Bt709);
        close(&[m[1], m[1], m[1]], &[[0.2126, 0.7152, 0.0722]; 3], 1e-4);
    }

    #[test]
    fn display_p3_to_bt709() {
        // The widely published Display P3 to sRGB matrix (D65 both sides).
        let expected = [
            [1.2249, -0.2247, 0.0],
            [-0.0420, 1.0419, 0.0],
            [-0.0197, -0.0786, 1.0979],
        ];
        close(
            &gamut_matrix(Primaries::DisplayP3, Primaries::Bt709),
            &expected,
            6e-4,
        );
    }

    #[test]
    fn round_trips_and_white_is_preserved() {
        let all = [
            Primaries::Bt709,
            Primaries::Bt601Pal,
            Primaries::Bt601Ntsc,
            Primaries::Bt2020,
            Primaries::DciP3,
            Primaries::DisplayP3,
        ];
        for a in all {
            for b in all {
                let there = gamut_matrix(a, b);
                let back = gamut_matrix(b, a);
                close(&mul(&back, &there), &IDENTITY, 1e-9);
                let white = apply(&there, [1.0, 1.0, 1.0]);
                if chromaticities(a)[3] == chromaticities(b)[3] {
                    for c in white {
                        assert!((c - 1.0).abs() < 1e-9, "{a:?} -> {b:?}: {white:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn dci_white_adapts_to_d65() {
        // DCI white in DCI-P3 lands on D65 white in Display P3 up to the adaptation's
        // luminance, which Bradford keeps at Y = 1.
        let m = gamut_matrix(Primaries::DciP3, Primaries::DisplayP3);
        let white = apply(&m, [1.0, 1.0, 1.0]);
        let xyz = apply(&rgb_to_xyz(Primaries::DisplayP3), white);
        let sum: f64 = xyz.iter().sum();
        assert!((xyz[0] / sum - 0.3127).abs() < 1e-4, "{xyz:?}");
        assert!((xyz[1] / sum - 0.3290).abs() < 1e-4, "{xyz:?}");
        assert!((xyz[1] - 1.0).abs() < 1e-9);
    }
}
