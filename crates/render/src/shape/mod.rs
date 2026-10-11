//! Vector shapes: rectangles with rounded corners, ellipses, polygons and paths (lines,
//! quadratic and cubic Béziers), filled with the non-zero or even-odd rule and stroked
//! (width, miter, round and bevel joins, butt, square and round caps, dashes), painted
//! with a solid colour or a linear or radial gradient.
//!
//! A [`Shape`] becomes one or two [`Outline`]s on the CPU: closed polygons in canvas
//! pixels plus a fill rule. Curves and arcs are flattened to within [`TOLERANCE`] of the
//! true curve (so coverage is within about 1.5 times that of exact), and a stroke becomes
//! pieces (one polygon per run of smooth segments, plus joins and caps) that all wind the
//! same way and are filled with the non-zero rule, which unions them.
//!
//! Outlines are rasterised on the GPU as exact box-filtered coverage by signed-area
//! accumulation: in each pixel, every edge adds its height within the pixel's row times
//! the fraction of the pixel to the right of the edge, with additive blending into an
//! `R32Float` texture. The sum in a pixel is the
//! integral of the winding number over it, from which the fill rule gives coverage:
//! `min(|a|, 1)` for non-zero, a triangle wave of period 2 for even-odd. Coverage is
//! exact for straight edges and needs no multisampling. Right of every edge crossing a
//! row the sum is constant, so the CPU bins each outline into bands of 4 rows
//! (`strips`): edges accumulate only across the columns some edge crosses in their
//! band, the CPU adds up what they carry to the right, and the paint pass reads the
//! accumulation only there and paints the gaps between with the carried sum. The work
//! per outline is about its area plus its edges' strips.
//! The one approximation: the fill rule is applied to the pixel's integral, not point
//! by point, so a pixel where a region wound twice (overlapping stroke pieces at an inner
//! join, a pentagram's centre) meets an empty one is off by up to the smaller of the two
//! areas inside it (in the tests: 0.06 at inner stroke joins, 0.19 non-zero and 0.35
//! even-odd at a pentagram's inner corners; only at such meeting points).
//!
//! [`ShapeNode`] draws any number of shapes into a transparent working-space layer
//! (composited afterwards with [`crate::nodes::CompositeNode`] and its blend modes):
//! outlines are packed into accumulation atlases (pooled outputs), then painted
//! source-over in order. [`CoverageNode`] writes one outline's coverage as a
//! single-channel canvas-sized texture, for masks. [`reference`] holds the CPU reference
//! the GPU is tested against.

mod geometry;
mod node;
pub mod reference;
mod strips;
mod stroke;

pub use geometry::{Affine, Path, Point, Verb};
pub use node::{CoverageNode, ShapeNode, accumulation_format};
pub use stroke::{Cap, Dash, Join};

use geometry::Polyline;

/// Maximum distance, in canvas pixels, between a curve or arc and the polygon that
/// replaces it.
pub const TOLERANCE: f32 = 1.0 / 64.0;

/// Linear-light colour in the working primaries, straight (not premultiplied) alpha.
pub type Color = [f32; 4];

/// Which points are inside a self-intersecting or multi-contour outline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

impl FillRule {
    pub(crate) fn code(self) -> u32 {
        match self {
            Self::NonZero => 0,
            Self::EvenOdd => 1,
        }
    }
}

/// An axis-aligned rectangle in shape-local units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// What a shape outlines, in shape-local units ([`Shape::transform`] maps them to canvas
/// pixels).
#[derive(Clone, Debug, PartialEq)]
pub enum Geometry {
    /// Corner `radius` is clamped to half the shorter side.
    Rect {
        rect: Rect,
        radius: f32,
    },
    Ellipse {
        center: Point,
        radii: [f32; 2],
    },
    /// A closed polygon through the points.
    Polygon(Vec<Point>),
    Path(Path),
}

/// A gradient colour stop; `offset` is 0..1 along the gradient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub offset: f32,
    pub color: Color,
}

impl Stop {
    pub fn new(offset: f32, color: Color) -> Self {
        Self { offset, color }
    }
}

/// How a fill or stroke is coloured. Gradient coordinates are shape-local, so they
/// follow the shape's transform (a scaled radial gradient becomes elliptical). Beyond
/// the first and last stop the end colours extend (pad). Between stops colours are
/// interpolated premultiplied, in linear light, so a stop fading to transparent does not
/// darken.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(Color),
    Linear {
        start: Point,
        end: Point,
        stops: Vec<Stop>,
    },
    Radial {
        center: Point,
        radius: f32,
        stops: Vec<Stop>,
    },
}

impl Paint {
    /// The stops sorted by offset (stable, so equal offsets make a hard edge in the
    /// order given), offsets clamped to 0..1; empty for a solid paint.
    pub fn sorted_stops(&self) -> Vec<Stop> {
        let stops = match self {
            Self::Solid(_) => return Vec::new(),
            Self::Linear { stops, .. } | Self::Radial { stops, .. } => stops,
        };
        let mut sorted: Vec<Stop> = stops
            .iter()
            .map(|s| Stop::new(s.offset.clamp(0.0, 1.0), s.color))
            .collect();
        sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
        sorted
    }
}

/// A fill: paint and fill rule.
#[derive(Clone, Debug, PartialEq)]
pub struct Fill {
    pub paint: Paint,
    pub rule: FillRule,
}

/// A stroke centred on the outline; `width` is in shape-local units.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub paint: Paint,
    pub width: f32,
    pub join: Join,
    pub cap: Cap,
    /// Longest miter as a multiple of the width before a miter join becomes a bevel (SVG's
    /// `stroke-miterlimit`, default 4).
    pub miter_limit: f32,
    pub dash: Option<Dash>,
}

impl Stroke {
    /// Miter joins, butt caps, no dashes.
    pub fn new(paint: Paint, width: f32) -> Self {
        Self {
            paint,
            width,
            join: Join::Miter,
            cap: Cap::Butt,
            miter_limit: 4.0,
            dash: None,
        }
    }
}

/// A shape: geometry, its placement on the canvas, and an optional fill and stroke (the
/// stroke is drawn over the fill).
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub geometry: Geometry,
    pub transform: Affine,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

impl Shape {
    pub fn new(geometry: Geometry) -> Self {
        Self {
            geometry,
            transform: Affine::IDENTITY,
            fill: None,
            stroke: None,
        }
    }

    pub fn filled(mut self, paint: Paint) -> Self {
        self.fill = Some(Fill {
            paint,
            rule: FillRule::NonZero,
        });
        self
    }

    pub fn filled_with(mut self, paint: Paint, rule: FillRule) -> Self {
        self.fill = Some(Fill { paint, rule });
        self
    }

    pub fn stroked(mut self, stroke: Stroke) -> Self {
        self.stroke = Some(stroke);
        self
    }

    pub fn transformed(mut self, transform: Affine) -> Self {
        self.transform = transform;
        self
    }

    /// Flattening tolerance in shape-local units, so the canvas error stays within
    /// [`TOLERANCE`] under the transform's largest scale.
    fn local_tolerance(&self) -> f32 {
        TOLERANCE / self.transform.max_scale().max(1e-6)
    }

    fn polylines(&self) -> Vec<Polyline> {
        geometry::flatten(&self.geometry, self.local_tolerance())
    }

    /// The filled region in canvas pixels, if the shape has a fill.
    pub fn fill_outline(&self) -> Option<Outline> {
        let fill = self.fill.as_ref()?;
        Some(self.outline_of_fill(fill.rule))
    }

    /// The geometry's interior with `rule`, in canvas pixels, whether or not the shape
    /// is filled (a mask uses this).
    pub fn outline_of_fill(&self, rule: FillRule) -> Outline {
        let contours = self
            .polylines()
            .into_iter()
            .map(|p| p.points)
            .filter(|p| p.len() >= 3)
            .map(|p| self.transform.apply_all(&p))
            .collect();
        Outline { contours, rule }
    }

    /// The stroked region in canvas pixels, if the shape has a stroke.
    pub fn stroke_outline(&self) -> Option<Outline> {
        let stroke = self.stroke.as_ref()?;
        let contours = stroke::stroke(&self.polylines(), stroke, self.local_tolerance())
            .into_iter()
            .map(|p| self.transform.apply_all(&p))
            .collect();
        Some(Outline {
            contours,
            rule: FillRule::NonZero,
        })
    }
}

/// Closed polygons in canvas pixels and the rule that decides their inside: what the
/// GPU rasterises into coverage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub contours: Vec<Vec<Point>>,
    pub rule: FillRule,
}

impl Outline {
    /// Min and max corner of every point, or `None` when there are none.
    pub fn bounds(&self) -> Option<(Point, Point)> {
        let mut points = self.contours.iter().flatten();
        let first = *points.next()?;
        Some(points.fold((first, first), |(lo, hi), p| {
            (
                Point::new(lo.x.min(p.x), lo.y.min(p.y)),
                Point::new(hi.x.max(p.x), hi.y.max(p.y)),
            )
        }))
    }

    /// Number of edges (each contour is closed).
    pub fn edge_count(&self) -> usize {
        self.contours.iter().map(Vec::len).sum()
    }

    /// Every edge, the closing ones included, skipping horizontal edges (they add
    /// nothing to the accumulation).
    pub fn edges(&self) -> impl Iterator<Item = (Point, Point)> + '_ {
        self.contours.iter().flat_map(|c| {
            (0..c.len())
                .map(move |i| (c[i], c[(i + 1) % c.len()]))
                .filter(|(a, b)| a.y != b.y)
        })
    }
}
