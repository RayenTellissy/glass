//! Points, affine transforms, paths, and flattening of every [`Geometry`] into
//! polylines within a tolerance.

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::ops::{Add, Mul, Neg, Sub};

use super::{Geometry, Rect};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y
    }

    /// z of the 3D cross product.
    pub fn cross(self, o: Self) -> f32 {
        self.x * o.y - self.y * o.x
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// Unit vector, or zero for a zero vector.
    pub fn normalized(self) -> Self {
        let l = self.length();
        if l > 0.0 { self * (1.0 / l) } else { self }
    }

    /// Rotated 90 degrees: the left normal in y-up axes, the right one on a y-down
    /// canvas.
    pub fn perp(self) -> Self {
        Self::new(-self.y, self.x)
    }

    pub fn lerp(self, o: Self, t: f32) -> Self {
        self + (o - self) * t
    }
}

impl Add for Point {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Point {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f32> for Point {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s)
    }
}

impl Neg for Point {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y)
    }
}

/// A 2D affine transform `[a, b, c, d, e, f]`: `x' = a x + c y + e`, `y' = b x + d y + f`
/// (the SVG and canvas `matrix(a, b, c, d, e, f)` order).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine(pub [f32; 6]);

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn translate(x: f32, y: f32) -> Self {
        Self([1.0, 0.0, 0.0, 1.0, x, y])
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Self([sx, 0.0, 0.0, sy, 0.0, 0.0])
    }

    /// Clockwise on a y-down canvas, in radians.
    pub fn rotate(angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Self([c, s, -s, c, 0.0, 0.0])
    }

    /// `self` applied first, then `next`.
    pub fn then(self, next: Self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let [na, nb, nc, nd, ne, nf] = next.0;
        Self([
            na * a + nc * b,
            nb * a + nd * b,
            na * c + nc * d,
            nb * c + nd * d,
            na * e + nc * f + ne,
            nb * e + nd * f + nf,
        ])
    }

    pub fn apply(&self, p: Point) -> Point {
        let [a, b, c, d, e, f] = self.0;
        Point::new(a * p.x + c * p.y + e, b * p.x + d * p.y + f)
    }

    pub fn apply_all(&self, points: &[Point]) -> Vec<Point> {
        points.iter().map(|p| self.apply(*p)).collect()
    }

    /// The inverse, or `None` when the transform collapses the plane.
    pub fn inverse(&self) -> Option<Self> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * d - b * c;
        if det.abs() < 1e-12 {
            return None;
        }
        let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
        Some(Self([
            ia,
            ib,
            ic,
            id,
            -(ia * e + ic * f),
            -(ib * e + id * f),
        ]))
    }

    /// The largest factor any length is scaled by (the larger singular value).
    pub fn max_scale(&self) -> f32 {
        let [a, b, c, d, ..] = self.0;
        let s = a * a + b * b + c * c + d * d;
        let det = a * d - b * c;
        ((s + (s * s - 4.0 * det * det).max(0.0).sqrt()) / 2.0).sqrt()
    }
}

/// One path command. Coordinates are shape-local.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verb {
    MoveTo(Point),
    LineTo(Point),
    /// Control point, end point.
    QuadTo(Point, Point),
    /// Two control points, end point.
    CubicTo(Point, Point, Point),
    Close,
}

/// A sequence of subpaths. A drawing command without a preceding move starts at the
/// last subpath's start (after a close) or at the origin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    pub verbs: Vec<Verb>,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(mut self, x: f32, y: f32) -> Self {
        self.verbs.push(Verb::MoveTo(Point::new(x, y)));
        self
    }

    pub fn line_to(mut self, x: f32, y: f32) -> Self {
        self.verbs.push(Verb::LineTo(Point::new(x, y)));
        self
    }

    pub fn quad_to(mut self, cx: f32, cy: f32, x: f32, y: f32) -> Self {
        self.verbs
            .push(Verb::QuadTo(Point::new(cx, cy), Point::new(x, y)));
        self
    }

    pub fn cubic_to(mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) -> Self {
        self.verbs.push(Verb::CubicTo(
            Point::new(c1x, c1y),
            Point::new(c2x, c2y),
            Point::new(x, y),
        ));
        self
    }

    pub fn close(mut self) -> Self {
        self.verbs.push(Verb::Close);
        self
    }
}

/// A flattened subpath. `corners[i]` marks a vertex that is a path point (where the
/// stroke's join style applies) rather than a sample inside a curve or arc.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Polyline {
    pub points: Vec<Point>,
    pub corners: Vec<bool>,
    pub closed: bool,
}

impl Polyline {
    fn push(&mut self, p: Point, corner: bool) {
        if let Some(last) = self.points.last()
            && *last == p
        {
            if corner && let Some(c) = self.corners.last_mut() {
                *c = true;
            }
            return;
        }
        self.points.push(p);
        self.corners.push(corner);
    }

    fn finish(mut self, closed: bool, out: &mut Vec<Polyline>) {
        if closed && self.points.len() > 1 && self.points.first() == self.points.last() {
            self.points.pop();
            let c = self.corners.pop().unwrap_or(false);
            self.corners[0] |= c;
        }
        self.closed = closed;
        if !self.points.is_empty() {
            out.push(self);
        }
    }
}

/// Uniform steps in the parameter for a curve whose second derivative is at most
/// `max_dd`: the chord error of a step `h` is at most `max_dd * h^2 / 8`.
fn curve_steps(max_dd: f32, tolerance: f32) -> usize {
    ((max_dd / (8.0 * tolerance)).sqrt().ceil() as usize).clamp(1, 1 << 16)
}

/// Steps for an arc of `radius` spanning `angle` (a circle's chord error is
/// `r (1 - cos(h / 2)) <= r h^2 / 8`; an ellipse's, with its larger radius, too).
fn arc_steps(radius: f32, angle: f32, tolerance: f32) -> usize {
    if radius <= tolerance {
        return 1;
    }
    let step = (8.0 * tolerance / radius).sqrt();
    ((angle.abs() / step).ceil() as usize).clamp(1, 1 << 16)
}

/// Flattens `geometry` into polylines within `tolerance` of the true outline.
pub(crate) fn flatten(geometry: &Geometry, tolerance: f32) -> Vec<Polyline> {
    let tolerance = tolerance.max(1e-6);
    match geometry {
        Geometry::Rect { rect, radius } => vec![rounded_rect(*rect, *radius, tolerance)],
        Geometry::Ellipse { center, radii } => {
            let (rx, ry) = (radii[0].abs(), radii[1].abs());
            let n = arc_steps(rx.max(ry), TAU, tolerance).max(8);
            let mut line = Polyline::default();
            for i in 0..n {
                let (s, c) = (TAU * i as f32 / n as f32).sin_cos();
                line.push(Point::new(center.x + rx * c, center.y + ry * s), false);
            }
            let mut out = Vec::new();
            line.finish(true, &mut out);
            out
        }
        Geometry::Polygon(points) => {
            let mut line = Polyline::default();
            for p in points {
                line.push(*p, true);
            }
            let mut out = Vec::new();
            line.finish(true, &mut out);
            out
        }
        Geometry::Path(path) => flatten_path(path, tolerance),
    }
}

fn rounded_rect(rect: Rect, radius: f32, tolerance: f32) -> Polyline {
    let (x0, y0) = (
        rect.x.min(rect.x + rect.width),
        rect.y.min(rect.y + rect.height),
    );
    let (w, h) = (rect.width.abs(), rect.height.abs());
    let r = radius.clamp(0.0, w.min(h) / 2.0);
    let mut line = Polyline::default();
    if r <= tolerance {
        for p in [(x0, y0), (x0 + w, y0), (x0 + w, y0 + h), (x0, y0 + h)] {
            line.push(Point::new(p.0, p.1), true);
        }
    } else {
        let n = arc_steps(r, FRAC_PI_2, tolerance);
        // Corner centres clockwise on the canvas from the top right, each arc starting
        // at the angle where the previous side ends.
        let corners = [
            (x0 + w - r, y0 + r, -FRAC_PI_2),
            (x0 + w - r, y0 + h - r, 0.0),
            (x0 + r, y0 + h - r, FRAC_PI_2),
            (x0 + r, y0 + r, PI),
        ];
        for (cx, cy, start) in corners {
            for i in 0..=n {
                let (s, c) = (start + FRAC_PI_2 * i as f32 / n as f32).sin_cos();
                line.push(Point::new(cx + r * c, cy + r * s), false);
            }
        }
    }
    let mut out = Vec::new();
    line.finish(true, &mut out);
    out.pop().unwrap_or_default()
}

fn flatten_path(path: &Path, tolerance: f32) -> Vec<Polyline> {
    let mut out = Vec::new();
    let mut line = Polyline::default();
    let mut start = Point::default();
    let mut current = Point::default();
    let mut open = false;
    let begin = |line: &mut Polyline, open: &mut bool, current: Point| {
        if !*open {
            line.push(current, true);
            *open = true;
        }
    };
    for verb in &path.verbs {
        match *verb {
            Verb::MoveTo(p) => {
                if open {
                    std::mem::take(&mut line).finish(false, &mut out);
                    open = false;
                }
                start = p;
                current = p;
            }
            Verb::LineTo(p) => {
                begin(&mut line, &mut open, current);
                line.push(p, true);
                current = p;
            }
            Verb::QuadTo(c, p) => {
                begin(&mut line, &mut open, current);
                let p0 = current;
                let dd = (p0 - c * 2.0 + p).length() * 2.0;
                let n = curve_steps(dd, tolerance);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let a = p0.lerp(c, t);
                    let b = c.lerp(p, t);
                    line.push(if i == n { p } else { a.lerp(b, t) }, i == n);
                }
                current = p;
            }
            Verb::CubicTo(c1, c2, p) => {
                begin(&mut line, &mut open, current);
                let p0 = current;
                let d0 = (p0 - c1 * 2.0 + c2).length();
                let d1 = (c1 - c2 * 2.0 + p).length();
                let n = curve_steps(6.0 * d0.max(d1), tolerance);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    let q = p0 * (u * u * u)
                        + c1 * (3.0 * u * u * t)
                        + c2 * (3.0 * u * t * t)
                        + p * (t * t * t);
                    line.push(if i == n { p } else { q }, i == n);
                }
                current = p;
            }
            Verb::Close => {
                if open {
                    std::mem::take(&mut line).finish(true, &mut out);
                    open = false;
                }
                current = start;
            }
        }
    }
    if open {
        line.finish(false, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distance_to_polygon(p: Point, poly: &[Point]) -> f32 {
        (0..poly.len())
            .map(|i| {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                let ab = b - a;
                let t = ((p - a).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0);
                (p - a.lerp(b, t)).length()
            })
            .fold(f32::INFINITY, f32::min)
    }

    #[test]
    fn ellipse_stays_within_tolerance() {
        let tol = 1.0 / 64.0;
        for radii in [[300.0, 300.0], [900.0, 20.0], [3.0, 40.0]] {
            let g = Geometry::Ellipse {
                center: Point::new(0.0, 0.0),
                radii,
            };
            let poly = &flatten(&g, tol)[0].points;
            for i in 0..4000 {
                let (s, c) = (TAU * i as f32 / 4000.0).sin_cos();
                let p = Point::new(radii[0] * c, radii[1] * s);
                let d = distance_to_polygon(p, poly);
                assert!(d <= tol * 1.05, "{radii:?}: {d}");
            }
        }
    }

    #[test]
    fn cubic_stays_within_tolerance() {
        let tol = 1.0 / 64.0;
        let path = Path::new()
            .move_to(0.0, 0.0)
            .cubic_to(400.0, -300.0, -100.0, 500.0, 300.0, 200.0);
        let lines = flatten(&Geometry::Path(path), tol);
        let poly = &lines[0].points;
        assert!(!lines[0].closed);
        assert_eq!(lines[0].corners.iter().filter(|c| **c).count(), 2);
        let open: Vec<Point> = poly.clone();
        for i in 0..=4000 {
            let t = i as f32 / 4000.0;
            let u = 1.0 - t;
            let p = Point::new(0.0, 0.0) * (u * u * u)
                + Point::new(400.0, -300.0) * (3.0 * u * u * t)
                + Point::new(-100.0, 500.0) * (3.0 * u * t * t)
                + Point::new(300.0, 200.0) * (t * t * t);
            // Distance to the open polyline (skip the closing edge).
            let d = (0..open.len() - 1)
                .map(|k| {
                    let (a, b) = (open[k], open[k + 1]);
                    let ab = b - a;
                    let s = ((p - a).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0);
                    (p - a.lerp(b, s)).length()
                })
                .fold(f32::INFINITY, f32::min);
            assert!(d <= tol * 1.05, "t {t}: {d}");
        }
    }

    #[test]
    fn subpaths_close_and_restart() {
        let path = Path::new()
            .move_to(0.0, 0.0)
            .line_to(10.0, 0.0)
            .line_to(10.0, 10.0)
            .line_to(0.0, 0.0)
            .close()
            .line_to(-5.0, 5.0)
            .move_to(20.0, 20.0);
        let lines = flatten(&Geometry::Path(path), 0.1);
        assert_eq!(lines.len(), 2, "a lone move draws nothing");
        assert!(lines[0].closed);
        assert_eq!(lines[0].points.len(), 3, "closing duplicate dropped");
        assert_eq!(
            lines[1].points,
            vec![Point::new(0.0, 0.0), Point::new(-5.0, 5.0)]
        );
    }

    #[test]
    fn rounded_rect_radius_is_clamped() {
        let g = Geometry::Rect {
            rect: Rect::new(0.0, 0.0, 10.0, 4.0),
            radius: 100.0,
        };
        let poly = &flatten(&g, 0.01)[0].points;
        for p in poly {
            assert!(p.x >= -1e-4 && p.x <= 10.0001 && p.y >= -1e-4 && p.y <= 4.0001);
        }
        assert!(
            poly.iter()
                .any(|p| (p.y - 2.0).abs() < 1e-3 && p.x.abs() < 1e-3)
        );
    }

    #[test]
    fn affine_inverse_and_scale() {
        let m = Affine::scale(2.0, 0.5)
            .then(Affine::rotate(0.7))
            .then(Affine::translate(3.0, -4.0));
        let p = Point::new(1.5, -2.0);
        let q = m.inverse().expect("invertible").apply(m.apply(p));
        assert!((q - p).length() < 1e-5);
        assert!((m.max_scale() - 2.0).abs() < 1e-5);
    }
}
