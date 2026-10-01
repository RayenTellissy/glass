//! Look-up tables: the `.cube` format (Adobe Cube LUT Specification 1.0 and the
//! DaVinci Resolve variant with a 1D shaper ahead of the 3D table), the CPU reference of
//! applying one, and the colour space a LUT expects its input in.
//!
//! A file holds a 1D table (`LUT_1D_SIZE`, 2 to 65,536 entries), a 3D table
//! (`LUT_3D_SIZE`, 2 to 256 per side; 17, 33 and 65 are usual) or both, in which case the
//! 1D table comes first and shapes the input of the 3D one. Data lines are three numbers,
//! red changing fastest. `DOMAIN_MIN`/`DOMAIN_MAX` give the input range per channel
//! (default 0 to 1; with both tables they apply to the 1D one), `LUT_1D_INPUT_RANGE` and
//! `LUT_3D_INPUT_RANGE` give one range for all channels of each table. `#` starts a
//! comment. Inputs outside the domain are clamped to it.
//!
//! 3D tables interpolate tetrahedrally: four entries per lookup, exact for affine colour
//! transforms and free of the grey-axis hue shifts of trilinear interpolation.
//! [`crate::nodes::LutNode`] runs the same arithmetic on the GPU.

use std::fmt::Write as _;
use std::path::Path;

use media::Primaries;

use crate::colour::{self, WorkingSpace};

pub const MAX_1D_SIZE: usize = 65_536;
pub const MAX_3D_SIZE: usize = 256;

/// The input range of a table, per channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Domain {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Domain {
    pub const UNIT: Self = Self {
        min: [0.0; 3],
        max: [1.0; 3],
    };

    /// `v` as 0..1 across the domain, clamped.
    pub fn normalise(&self, v: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|c| {
            ((v[c] - self.min[c]) / (self.max[c] - self.min[c])).clamp(0.0, 1.0)
        })
    }
}

impl Default for Domain {
    fn default() -> Self {
        Self::UNIT
    }
}

/// A 1D table: one curve per channel, `size` entries each.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1d {
    pub size: usize,
    pub domain: Domain,
    pub table: Vec<[f32; 3]>,
}

impl Lut1d {
    /// Linear interpolation between the entries either side, per channel.
    pub fn apply(&self, rgb: [f64; 3]) -> [f64; 3] {
        let t = self.domain.normalise(rgb);
        let last = (self.size - 1) as f64;
        std::array::from_fn(|c| {
            let p = t[c] * last;
            let i = (p.floor() as usize).min(self.size - 2);
            let f = p - i as f64;
            let a = f64::from(self.table[i][c]);
            let b = f64::from(self.table[i + 1][c]);
            a + (b - a) * f
        })
    }
}

/// A 3D table of `size`³ entries, red changing fastest, then green, then blue.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3d {
    pub size: usize,
    pub domain: Domain,
    pub table: Vec<[f32; 3]>,
}

impl Lut3d {
    /// A table sampling `f` on the grid (0 to 1 in each channel).
    pub fn from_fn(size: usize, f: impl Fn([f64; 3]) -> [f64; 3]) -> Self {
        let step = 1.0 / (size - 1) as f64;
        let mut table = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let v = f([r as f64 * step, g as f64 * step, b as f64 * step]);
                    table.push(v.map(|c| c as f32));
                }
            }
        }
        Self {
            size,
            domain: Domain::UNIT,
            table,
        }
    }

    pub fn identity(size: usize) -> Self {
        Self::from_fn(size, |rgb| rgb)
    }

    pub fn at(&self, r: usize, g: usize, b: usize) -> [f64; 3] {
        self.table[r + self.size * (g + self.size * b)].map(f64::from)
    }

    /// Tetrahedral interpolation: the grid cube around the input is split into six
    /// tetrahedra along its black-white diagonal, and the input is a weighted sum of the
    /// four corners of the one it falls in (ordered by which channel's fraction is
    /// largest).
    pub fn apply(&self, rgb: [f64; 3]) -> [f64; 3] {
        let t = self.domain.normalise(rgb);
        let last = (self.size - 1) as f64;
        let mut base = [0usize; 3];
        let mut f = [0.0f64; 3];
        for c in 0..3 {
            let p = t[c] * last;
            base[c] = (p.floor() as usize).min(self.size - 2);
            f[c] = p - base[c] as f64;
        }
        let [r, g, b] = base;
        let at = |dr: usize, dg: usize, db: usize| self.at(r + dr, g + dg, b + db);
        let c000 = at(0, 0, 0);
        let c111 = at(1, 1, 1);
        // (first corner, second corner, weights in the order they apply).
        let (c1, c2, w) = if f[0] >= f[1] {
            if f[1] >= f[2] {
                (at(1, 0, 0), at(1, 1, 0), [f[0], f[1], f[2]])
            } else if f[0] >= f[2] {
                (at(1, 0, 0), at(1, 0, 1), [f[0], f[2], f[1]])
            } else {
                (at(0, 0, 1), at(1, 0, 1), [f[2], f[0], f[1]])
            }
        } else if f[2] >= f[1] {
            (at(0, 0, 1), at(0, 1, 1), [f[2], f[1], f[0]])
        } else if f[2] >= f[0] {
            (at(0, 1, 0), at(0, 1, 1), [f[1], f[2], f[0]])
        } else {
            (at(0, 1, 0), at(1, 1, 0), [f[1], f[0], f[2]])
        };
        std::array::from_fn(|c| {
            c000[c] + w[0] * (c1[c] - c000[c]) + w[1] * (c2[c] - c1[c]) + w[2] * (c111[c] - c2[c])
        })
    }
}

/// A parsed `.cube` file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lut {
    pub title: Option<String>,
    /// Applied first when present.
    pub shaper: Option<Lut1d>,
    pub cube: Option<Lut3d>,
}

/// What is wrong with a `.cube` file, with the 1-based line it was found on.
#[derive(Debug, thiserror::Error)]
pub enum CubeError {
    #[error("the LUT file could not be read: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: {problem}")]
    Syntax { line: usize, problem: CubeProblem },
}

impl CubeError {
    pub fn line(&self) -> Option<usize> {
        match self {
            Self::Io(_) => None,
            Self::Syntax { line, .. } => Some(*line),
        }
    }

    pub fn problem(&self) -> Option<&CubeProblem> {
        match self {
            Self::Io(_) => None,
            Self::Syntax { problem, .. } => Some(problem),
        }
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CubeProblem {
    #[error("unknown keyword {0:?}")]
    UnknownKeyword(String),
    #[error("{keyword} expects {expected} value(s), found {found}")]
    WrongCount {
        keyword: String,
        expected: usize,
        found: usize,
    },
    #[error("{0:?} is not a finite number")]
    NotANumber(String),
    #[error("{keyword} {size} is outside {min} to {max}")]
    SizeOutOfRange {
        keyword: &'static str,
        size: String,
        min: usize,
        max: usize,
    },
    #[error("{0} appears twice")]
    Duplicate(String),
    #[error("{0} comes after the table data")]
    KeywordAfterData(String),
    #[error("the domain minimum is not below the maximum")]
    EmptyDomain,
    #[error("table data before LUT_1D_SIZE or LUT_3D_SIZE")]
    DataBeforeSize,
    #[error("no LUT_1D_SIZE or LUT_3D_SIZE")]
    NoSize,
    #[error("more table entries than the sizes declare ({expected})")]
    TooManyEntries { expected: usize },
    #[error("{found} table entries, the sizes declare {expected}")]
    TooFewEntries { expected: usize, found: usize },
}

fn syntax(line: usize, problem: CubeProblem) -> CubeError {
    CubeError::Syntax { line, problem }
}

fn number(token: &str) -> Result<f64, CubeProblem> {
    token
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| CubeProblem::NotANumber(token.to_owned()))
}

impl Lut {
    pub fn read(path: &Path) -> Result<Self, CubeError> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    pub fn parse(text: &str) -> Result<Self, CubeError> {
        let mut title = None;
        let mut size_1d: Option<usize> = None;
        let mut size_3d: Option<usize> = None;
        let mut domain: Option<Domain> = None;
        let mut range_1d: Option<[f64; 2]> = None;
        let mut range_3d: Option<[f64; 2]> = None;
        let mut data: Vec<[f32; 3]> = Vec::new();
        let mut last_line = 0;
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            last_line = line;
            let content = raw.split('#').next().unwrap_or_default().trim();
            if content.is_empty() {
                continue;
            }
            let mut tokens = content.split_whitespace();
            let first = tokens.next().unwrap_or_default();
            let starts_number = first
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || "+-.".contains(c));
            if starts_number {
                let values: Vec<&str> = content.split_whitespace().collect();
                if values.len() != 3 {
                    return Err(syntax(
                        line,
                        CubeProblem::WrongCount {
                            keyword: "a table entry".to_owned(),
                            expected: 3,
                            found: values.len(),
                        },
                    ));
                }
                let expected = match (size_1d, size_3d) {
                    (None, None) => return Err(syntax(line, CubeProblem::DataBeforeSize)),
                    (a, b) => a.unwrap_or(0) + b.map_or(0, |n| n * n * n),
                };
                if data.len() == expected {
                    return Err(syntax(line, CubeProblem::TooManyEntries { expected }));
                }
                let mut rgb = [0.0f32; 3];
                for (c, token) in values.iter().enumerate() {
                    rgb[c] = number(token).map_err(|p| syntax(line, p))? as f32;
                }
                data.push(rgb);
                continue;
            }
            let keyword = first.to_ascii_uppercase();
            if !data.is_empty() {
                return Err(syntax(line, CubeProblem::KeywordAfterData(keyword)));
            }
            let rest: Vec<&str> = tokens.collect();
            let numbers = |expected: usize| -> Result<Vec<f64>, CubeError> {
                if rest.len() != expected {
                    return Err(syntax(
                        line,
                        CubeProblem::WrongCount {
                            keyword: keyword.clone(),
                            expected,
                            found: rest.len(),
                        },
                    ));
                }
                rest.iter()
                    .map(|t| number(t).map_err(|p| syntax(line, p)))
                    .collect()
            };
            let duplicate = || syntax(line, CubeProblem::Duplicate(keyword.clone()));
            match keyword.as_str() {
                "TITLE" => {
                    if title.is_some() {
                        return Err(duplicate());
                    }
                    let text = content[first.len()..].trim();
                    let text = text
                        .strip_prefix('"')
                        .map(|t| t.strip_suffix('"').unwrap_or(t))
                        .unwrap_or(text);
                    title = Some(text.to_owned());
                }
                "LUT_1D_SIZE" | "LUT_3D_SIZE" => {
                    let is_3d = keyword == "LUT_3D_SIZE";
                    let (min, max, name) = if is_3d {
                        (2, MAX_3D_SIZE, "LUT_3D_SIZE")
                    } else {
                        (2, MAX_1D_SIZE, "LUT_1D_SIZE")
                    };
                    if rest.len() != 1 {
                        return Err(syntax(
                            line,
                            CubeProblem::WrongCount {
                                keyword: keyword.clone(),
                                expected: 1,
                                found: rest.len(),
                            },
                        ));
                    }
                    let size = rest[0]
                        .parse::<usize>()
                        .ok()
                        .filter(|n| (min..=max).contains(n))
                        .ok_or_else(|| {
                            syntax(
                                line,
                                CubeProblem::SizeOutOfRange {
                                    keyword: name,
                                    size: rest[0].to_owned(),
                                    min,
                                    max,
                                },
                            )
                        })?;
                    let slot = if is_3d { &mut size_3d } else { &mut size_1d };
                    if slot.is_some() {
                        return Err(duplicate());
                    }
                    *slot = Some(size);
                }
                "DOMAIN_MIN" | "DOMAIN_MAX" => {
                    let v = numbers(3)?;
                    let d = domain.get_or_insert(Domain {
                        min: [f64::NAN; 3],
                        max: [f64::NAN; 3],
                    });
                    let slot = if keyword == "DOMAIN_MIN" {
                        &mut d.min
                    } else {
                        &mut d.max
                    };
                    if !slot[0].is_nan() {
                        return Err(duplicate());
                    }
                    *slot = [v[0], v[1], v[2]];
                }
                "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE" => {
                    let v = numbers(2)?;
                    let slot = if keyword == "LUT_1D_INPUT_RANGE" {
                        &mut range_1d
                    } else {
                        &mut range_3d
                    };
                    if slot.is_some() {
                        return Err(duplicate());
                    }
                    if v[0] >= v[1] {
                        return Err(syntax(line, CubeProblem::EmptyDomain));
                    }
                    *slot = Some([v[0], v[1]]);
                }
                _ => return Err(syntax(line, CubeProblem::UnknownKeyword(keyword.clone()))),
            }
        }
        if size_1d.is_none() && size_3d.is_none() {
            return Err(syntax(last_line.max(1), CubeProblem::NoSize));
        }
        let domain = match domain {
            None => None,
            Some(d) => {
                // A missing DOMAIN_MIN or DOMAIN_MAX keeps the default for that end.
                let fill =
                    |v: [f64; 3], default: f64| v.map(|c| if c.is_nan() { default } else { c });
                let d = Domain {
                    min: fill(d.min, 0.0),
                    max: fill(d.max, 1.0),
                };
                if (0..3).any(|c| d.min[c] >= d.max[c]) {
                    return Err(syntax(last_line, CubeProblem::EmptyDomain));
                }
                Some(d)
            }
        };
        let expected = size_1d.unwrap_or(0) + size_3d.map_or(0, |n| n * n * n);
        if data.len() != expected {
            return Err(syntax(
                last_line,
                CubeProblem::TooFewEntries {
                    expected,
                    found: data.len(),
                },
            ));
        }
        let range = |r: [f64; 2]| Domain {
            min: [r[0]; 3],
            max: [r[1]; 3],
        };
        let mut rest = data.as_slice();
        let shaper = size_1d.map(|size| {
            let (table, tail) = rest.split_at(size);
            rest = tail;
            Lut1d {
                size,
                domain: range_1d.map(range).or(domain).unwrap_or_default(),
                table: table.to_vec(),
            }
        });
        let cube = size_3d.map(|size| Lut3d {
            size,
            domain: range_3d
                .map(range)
                .or(if shaper.is_none() { domain } else { None })
                .unwrap_or_default(),
            table: rest.to_vec(),
        });
        Ok(Self {
            title,
            shaper,
            cube,
        })
    }

    /// The file text: the title, sizes and ranges, then the 1D and 3D data. Ranges that
    /// are the same for every channel are written as `LUT_*_INPUT_RANGE`, others as
    /// `DOMAIN_MIN`/`DOMAIN_MAX` (possible only for a single table).
    pub fn to_cube(&self) -> String {
        let mut out = String::new();
        if let Some(title) = &self.title {
            let _ = writeln!(out, "TITLE \"{title}\"");
        }
        let both = self.shaper.is_some() && self.cube.is_some();
        let mut ranges = String::new();
        let mut header = |keyword: &str, size: usize, domain: &Domain| {
            let _ = writeln!(out, "LUT_{keyword}_SIZE {size}");
            if *domain == Domain::UNIT {
                return;
            }
            let uniform = domain.min.iter().all(|v| *v == domain.min[0])
                && domain.max.iter().all(|v| *v == domain.max[0]);
            if uniform || both {
                let _ = writeln!(
                    ranges,
                    "LUT_{keyword}_INPUT_RANGE {} {}",
                    domain.min[0], domain.max[0]
                );
            } else {
                let [a, b, c] = domain.min;
                let [d, e, f] = domain.max;
                let _ = writeln!(ranges, "DOMAIN_MIN {a} {b} {c}\nDOMAIN_MAX {d} {e} {f}");
            }
        };
        if let Some(s) = &self.shaper {
            header("1D", s.size, &s.domain);
        }
        if let Some(c) = &self.cube {
            header("3D", c.size, &c.domain);
        }
        out.push_str(&ranges);
        out.push('\n');
        let tables = self.shaper.iter().map(|s| &s.table);
        for table in tables.chain(self.cube.iter().map(|c| &c.table)) {
            for [r, g, b] in table {
                let _ = writeln!(out, "{r} {g} {b}");
            }
        }
        out
    }

    /// The shaper, then the cube, on values in the LUT's input space.
    pub fn apply(&self, rgb: [f64; 3]) -> [f64; 3] {
        let rgb = self.shaper.as_ref().map_or(rgb, |s| s.apply(rgb));
        self.cube.as_ref().map_or(rgb, |c| c.apply(rgb))
    }
}

/// The encoding a LUT expects its input in and returns its output in. Creative LUTs
/// are mostly built for display-referred Rec.709 video, so that is the default; the
/// node converts the working space (linear light, project primaries) into it and back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LutSpace {
    /// BT.709 primaries, encoded with the inverse BT.1886 EOTF (a 2.4 power): the code
    /// values of delivered SDR video, as [`crate::nodes::OutputTransfer::Bt1886`] writes
    /// them.
    #[default]
    Rec709,
    /// BT.709 primaries, piecewise sRGB encoding (LUTs made for stills and web images).
    Srgb,
    /// The working space itself: linear light in the project primaries.
    Linear,
}

impl LutSpace {
    pub const ALL: [Self; 3] = [Self::Rec709, Self::Srgb, Self::Linear];

    /// The stable id stored in projects.
    pub fn id(self) -> &'static str {
        match self {
            Self::Rec709 => "rec709",
            Self::Srgb => "srgb",
            Self::Linear => "linear",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    /// The shader's transfer code.
    pub(crate) fn code(self) -> u32 {
        match self {
            Self::Rec709 => 0,
            Self::Srgb => 1,
            Self::Linear => 2,
        }
    }

    /// The primaries of the space, or `None` for the working primaries.
    pub fn primaries(self) -> Option<Primaries> {
        match self {
            Self::Rec709 | Self::Srgb => Some(Primaries::Bt709),
            Self::Linear => None,
        }
    }

    /// Linear light (negative values clamped to 0) to the encoding.
    pub fn encode(self, v: f64) -> f64 {
        let v = v.max(0.0);
        match self {
            Self::Rec709 => v.powf(1.0 / 2.4),
            Self::Srgb => {
                if v <= 0.003_130_8 {
                    12.92 * v
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                }
            }
            Self::Linear => v,
        }
    }

    /// The encoding (negative values clamped to 0) back to linear light.
    pub fn decode(self, e: f64) -> f64 {
        let e = e.max(0.0);
        match self {
            Self::Rec709 => e.powf(2.4),
            Self::Srgb => {
                if e <= 0.04045 {
                    e / 12.92
                } else {
                    ((e + 0.055) / 1.055).powf(2.4)
                }
            }
            Self::Linear => e,
        }
    }
}

/// The f64 CPU reference of [`crate::nodes::LutNode`] on one premultiplied working-space
/// pixel: alpha divided out, into the LUT's space, the LUT, back to the working space,
/// mixed with the input by `intensity` (0 leaves the pixel untouched), alpha multiplied
/// back in.
pub fn apply_in_working_space(
    lut: &Lut,
    working: WorkingSpace,
    space: LutSpace,
    intensity: f64,
    rgba: [f64; 4],
) -> [f64; 4] {
    let a = rgba[3].clamp(0.0, 1.0);
    if intensity <= 0.0 || a <= 0.0 {
        return rgba;
    }
    let straight = [rgba[0] / a, rgba[1] / a, rgba[2] / a];
    let (into, back) = match space.primaries() {
        Some(p) => (
            colour::gamut_matrix(working.primaries, p),
            colour::gamut_matrix(p, working.primaries),
        ),
        None => (colour::IDENTITY, colour::IDENTITY),
    };
    let encoded = colour::apply(&into, straight).map(|v| space.encode(v));
    let graded = colour::apply(&back, lut.apply(encoded).map(|e| space.decode(e)));
    let t = intensity.min(1.0);
    let mut out = [0.0; 4];
    for c in 0..3 {
        out[c] = rgba[c] + t * (graded[c] * a - rgba[c]);
    }
    out[3] = rgba[3];
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3], tol: f64) {
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() <= tol, "{a:?} vs {b:?}");
        }
    }

    fn problem(text: &str) -> (usize, CubeProblem) {
        match Lut::parse(text) {
            Err(CubeError::Syntax { line, problem }) => (line, problem),
            other => panic!("expected a syntax error, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_3d_cube_with_title_comments_and_domain() {
        let text = "# made by a test\nTITLE \"Warm\"\n\nLUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2 # trailing\n\
            0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let lut = Lut::parse(text).expect("parses");
        assert_eq!(lut.title.as_deref(), Some("Warm"));
        assert!(lut.shaper.is_none());
        let cube = lut.cube.as_ref().expect("cube");
        assert_eq!(cube.size, 2);
        assert_eq!(cube.domain.max, [2.0; 3]);
        // Red changes fastest.
        assert_eq!(cube.at(1, 0, 0), [1.0, 0.0, 0.0]);
        assert_eq!(cube.at(0, 0, 1), [0.0, 0.0, 1.0]);
        // The domain 0..2 maps 1.0 to the middle of the table.
        close(lut.apply([1.0, 0.5, 2.0]), [0.5, 0.25, 1.0], 1e-12);
    }

    #[test]
    fn parses_a_1d_lut_and_a_resolve_shaper_with_ranges() {
        let lut = Lut::parse("LUT_1D_SIZE 3\n0 0 0\n0.25 0.5 1\n1 1 1\n").expect("parses");
        let s = lut.shaper.as_ref().expect("1D");
        assert_eq!(s.size, 3);
        close(lut.apply([0.25, 0.25, 0.75]), [0.125, 0.25, 1.0], 1e-7);

        let text = "LUT_1D_SIZE 2\nLUT_3D_SIZE 2\nLUT_1D_INPUT_RANGE -0.5 1.5\nLUT_3D_INPUT_RANGE 0 1\n\
            0 0 0\n1 1 1\n\
            0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let lut = Lut::parse(text).expect("parses");
        assert_eq!(lut.shaper.as_ref().map(|s| s.domain.min), Some([-0.5; 3]));
        assert_eq!(lut.cube.as_ref().map(|c| c.domain), Some(Domain::UNIT));
        close(lut.apply([0.5, -0.5, 1.5]), [0.5, 0.0, 1.0], 1e-7);
    }

    #[test]
    fn errors_carry_the_line() {
        let entries = "0 0 0\n".repeat(8);
        assert_eq!(
            problem("LUT_3D_SIZE 2\nBOGUS 1\n"),
            (2, CubeProblem::UnknownKeyword("BOGUS".into()))
        );
        assert_eq!(problem("0 0 0\n").0, 1);
        assert!(matches!(
            problem("LUT_3D_SIZE 1\n"),
            (1, CubeProblem::SizeOutOfRange { .. })
        ));
        assert!(matches!(
            problem("LUT_3D_SIZE 257\n"),
            (1, CubeProblem::SizeOutOfRange { .. })
        ));
        assert!(matches!(
            problem("LUT_1D_SIZE 65537\n"),
            (1, CubeProblem::SizeOutOfRange { .. })
        ));
        assert!(matches!(
            problem(&format!("LUT_3D_SIZE 2\n{}0 0\n", "0 0 0\n".repeat(3))),
            (5, CubeProblem::WrongCount { found: 2, .. })
        ));
        assert_eq!(
            problem("LUT_3D_SIZE 2\n0 zero 0\n"),
            (2, CubeProblem::NotANumber("zero".into()))
        );
        assert_eq!(
            problem("LUT_3D_SIZE 2\n0 nan 0\n").1,
            CubeProblem::NotANumber("nan".into())
        );
        assert_eq!(
            problem(&format!("LUT_3D_SIZE 2\n{entries}1 1 1\n")),
            (10, CubeProblem::TooManyEntries { expected: 8 })
        );
        assert_eq!(
            problem("LUT_3D_SIZE 2\n0 0 0\n\n"),
            (
                3,
                CubeProblem::TooFewEntries {
                    expected: 8,
                    found: 1
                }
            )
        );
        assert_eq!(
            problem("LUT_3D_SIZE 2\nLUT_3D_SIZE 2\n"),
            (2, CubeProblem::Duplicate("LUT_3D_SIZE".into()))
        );
        assert_eq!(
            problem("LUT_3D_SIZE 2\n0 0 0\nTITLE \"late\"\n"),
            (3, CubeProblem::KeywordAfterData("TITLE".into()))
        );
        assert_eq!(
            problem(&format!(
                "LUT_3D_SIZE 2\nDOMAIN_MIN 1 0 0\nDOMAIN_MAX 1 1 1\n{entries}"
            ))
            .1,
            CubeProblem::EmptyDomain
        );
        assert_eq!(
            problem("# nothing\nTITLE \"x\"\n"),
            (2, CubeProblem::NoSize)
        );
    }

    #[test]
    fn writes_what_it_reads() {
        let mut cube = Lut3d::from_fn(5, |[r, g, b]| [g, b * 0.5, r * r]);
        cube.domain = Domain {
            min: [0.0, -0.1, 0.0],
            max: [1.0, 1.0, 2.0],
        };
        let lut = Lut {
            title: Some("Swap".into()),
            shaper: None,
            cube: Some(cube),
        };
        assert_eq!(Lut::parse(&lut.to_cube()).expect("parses"), lut);
        let both = Lut {
            title: None,
            shaper: Some(Lut1d {
                size: 4,
                domain: Domain {
                    min: [-1.0; 3],
                    max: [3.0; 3],
                },
                table: vec![[0.0; 3], [0.2; 3], [0.7; 3], [1.0; 3]],
            }),
            cube: Some(Lut3d::identity(3)),
        };
        assert_eq!(Lut::parse(&both.to_cube()).expect("parses"), both);
    }

    #[test]
    fn tetrahedral_is_exact_on_affine_tables_and_the_grid() {
        let affine = |[r, g, b]: [f64; 3]| {
            [
                0.8 * r + 0.15 * g + 0.05 * b + 0.01,
                0.1 * r + 0.7 * g + 0.2 * b,
                0.3 * r - 0.1 * g + 0.75 * b + 0.02,
            ]
        };
        let cube = Lut3d::from_fn(9, affine);
        let mut state = 12345u32;
        for _ in 0..1000 {
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                f64::from(state) / f64::from(u32::MAX)
            };
            let rgb = [next(), next(), next()];
            // Table entries are f32.
            close(cube.apply(rgb), affine(rgb), 1e-6);
        }
        let curve = Lut3d::from_fn(17, |rgb| rgb.map(|v| v.powf(0.7)));
        for i in 0..17 {
            let v = i as f64 / 16.0;
            close(curve.apply([v, v, v]), [v.powf(0.7); 3], 1e-6);
        }
    }

    #[test]
    fn spaces_round_trip_and_ids_are_stable() {
        for space in LutSpace::ALL {
            assert_eq!(LutSpace::from_id(space.id()), Some(space));
            for i in 0..=100 {
                let v = f64::from(i) / 50.0;
                assert!((space.decode(space.encode(v)) - v).abs() < 1e-12);
            }
        }
        assert!((LutSpace::Rec709.encode(0.18) - 0.489_437).abs() < 1e-6);
    }

    #[test]
    fn identity_lut_and_zero_intensity_leave_pixels_alone() {
        let lut = Lut {
            cube: Some(Lut3d::identity(33)),
            ..Default::default()
        };
        let px = [0.3, 0.12, 0.05, 0.5];
        for space in LutSpace::ALL {
            let out = apply_in_working_space(&lut, WorkingSpace::BT709, space, 1.0, px);
            for c in 0..4 {
                assert!((out[c] - px[c]).abs() < 1e-6, "{space:?}: {out:?}");
            }
        }
        let invert = Lut {
            cube: Some(Lut3d::from_fn(2, |rgb| rgb.map(|v| 1.0 - v))),
            ..Default::default()
        };
        assert_eq!(
            apply_in_working_space(&invert, WorkingSpace::BT709, LutSpace::Rec709, 0.0, px),
            px
        );
        let half = apply_in_working_space(&invert, WorkingSpace::BT709, LutSpace::Linear, 0.5, px);
        // Straight 0.6 inverts to 0.4; half way is 0.5, times alpha 0.5.
        assert!((half[0] - 0.25).abs() < 1e-7, "{half:?}");
    }
}
