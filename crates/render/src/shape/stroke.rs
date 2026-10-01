//! Stroking: flattened polylines to closed polygons that all wind the same way, so the
//! non-zero rule fills their union.
//!
//! Each run of segments whose vertices are smooth (samples inside a curve, where the
//! inner offset does not fold back) becomes one strip polygon: the offset on the outer
//! side of each vertex follows a round arc, the inner side takes the inner miter point.
//! Path corners, cusps and tight bends end the strip and get a join piece (the stroke's
//! join at a corner, a round wedge elsewhere); open ends get caps. Pieces meet along
//! shared edges, which cancel exactly in the coverage accumulation, and overlap
//! elsewhere, which the non-zero rule clamps.

use super::Stroke;
use super::geometry::{Point, Polyline};

/// How two segments of a stroke meet at a path corner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Join {
    /// Extended edges meet at a point, or a bevel beyond the miter limit.
    #[default]
    Miter,
    Round,
    Bevel,
}

/// How an open subpath's ends are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cap {
    /// Flat at the end point.
    #[default]
    Butt,
    /// Flat, half the width beyond the end point.
    Square,
    Round,
}

/// Dash lengths alternating on and off, starting `offset` into the pattern (SVG
/// `stroke-dasharray` and `stroke-dashoffset`: an odd count repeats twice). Every
/// dash gets the stroke's caps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dash {
    pub array: Vec<f32>,
    pub offset: f32,
}

/// Most dashes one polyline may produce before it is drawn solid instead.
const MAX_DASHES: f32 = 100_000.0;

pub(crate) fn stroke(lines: &[Polyline], style: &Stroke, tolerance: f32) -> Vec<Vec<Point>> {
    let h = style.width / 2.0;
    if h.is_nan() || h <= 0.0 {
        return Vec::new();
    }
    let mut pieces: Vec<Vec<Vec<Point>>> = Vec::new();
    for line in lines {
        let dashed = match &style.dash {
            Some(d) => dash(line, d),
            None => vec![line.clone()],
        };
        for line in &dashed {
            stroke_polyline(line, style, h, tolerance, &mut pieces);
        }
    }
    let mut out = Vec::new();
    for mut piece in pieces {
        // Coverage accumulates positive where the shoelace area is negative (a left edge
        // going down the canvas); give every piece that orientation.
        let area: f32 = piece.iter().map(|c| signed_area(c)).sum();
        if area > 0.0 {
            for c in &mut piece {
                c.reverse();
            }
        }
        out.extend(piece.into_iter().filter(|c| c.len() >= 3));
    }
    out
}

pub(crate) fn signed_area(c: &[Point]) -> f32 {
    let n = c.len();
    (0..n).map(|i| c[i].cross(c[(i + 1) % n])).sum::<f32>() / 2.0
}

/// Points on the circle around `center` from `center + from * r`, turning by `sweep`
/// radians, both ends included.
fn arc(center: Point, r: f32, from: Point, sweep: f32, tolerance: f32) -> Vec<Point> {
    let n = if r <= tolerance {
        1
    } else {
        ((sweep.abs() / (8.0 * tolerance / r).sqrt()).ceil() as usize).clamp(1, 4096)
    };
    let a0 = from.y.atan2(from.x);
    (0..=n)
        .map(|i| {
            let (s, c) = (a0 + sweep * i as f32 / n as f32).sin_cos();
            center + Point::new(c, s) * r
        })
        .collect()
}

/// Signed angle turning unit vector `a` into `b`, the short way.
fn angle_between(a: Point, b: Point) -> f32 {
    a.cross(b).atan2(a.dot(b))
}

/// The outer arc at a vertex from `v + a h` to `v + b h` (a, b unit outer normals),
/// turning through the side `ahead` points to when the two are opposite.
fn outer_arc(v: Point, h: f32, a: Point, b: Point, ahead: Point, tolerance: f32) -> Vec<Point> {
    let mut sweep = angle_between(a, b);
    let (s, c) = (sweep / 2.0).sin_cos();
    let mid = Point::new(a.x * c - a.y * s, a.x * s + a.y * c);
    if mid.dot(ahead) < -1e-4 {
        sweep -= std::f32::consts::TAU * sweep.signum();
    }
    arc(v, h, a, sweep, tolerance)
}

struct Seg {
    a: Point,
    b: Point,
    d: Point,
    n: Point,
    len: f32,
}

fn stroke_polyline(
    line: &Polyline,
    style: &Stroke,
    h: f32,
    tolerance: f32,
    pieces: &mut Vec<Vec<Vec<Point>>>,
) {
    let pts = &line.points;
    let count = pts.len();
    if count == 1 || (line.closed && count == 2 && pts[0] == pts[1]) {
        let p = pts[0];
        match style.cap {
            Cap::Butt => {}
            Cap::Square => pieces.push(vec![vec![
                p + Point::new(-h, -h),
                p + Point::new(h, -h),
                p + Point::new(h, h),
                p + Point::new(-h, h),
            ]]),
            Cap::Round => {
                let mut c = arc(p, h, Point::new(1.0, 0.0), std::f32::consts::TAU, tolerance);
                c.pop();
                pieces.push(vec![c]);
            }
        }
        return;
    }
    if count == 0 {
        return;
    }
    let seg_count = if line.closed { count } else { count - 1 };
    let segs: Vec<Seg> = (0..seg_count)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % count]);
            let d = (b - a).normalized();
            Seg {
                a,
                b,
                d,
                n: d.perp(),
                len: (b - a).length(),
            }
        })
        .collect();
    // Vertex k joins segment k - 1 (mod) to segment k.
    let smooth = |k: usize| -> bool {
        let (s0, s1) = (&segs[(k + seg_count - 1) % seg_count], &segs[k]);
        let cos = s0.d.dot(s1.d).clamp(-1.0, 1.0);
        let turn = s0.d.cross(s1.d).atan2(cos).abs();
        if turn < 1e-4 {
            return true;
        }
        if line.corners[k] || cos <= -0.999 {
            return false;
        }
        let tan_half = ((1.0 - cos) / (1.0 + cos)).sqrt();
        h * tan_half <= 0.5 * s0.len.min(s1.len)
    };

    // Runs of segments between breaks, as (first segment, segment count).
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut breaks: Vec<usize> = Vec::new();
    if line.closed {
        breaks = (0..count).filter(|&k| !smooth(k)).collect();
        if breaks.is_empty() {
            ring(&segs, h, tolerance, pieces);
            return;
        }
        for (i, &k) in breaks.iter().enumerate() {
            let next = breaks[(i + 1) % breaks.len()];
            let len = (next + seg_count - k - 1) % seg_count + 1;
            runs.push((k, len));
        }
    } else {
        let mut start = 0;
        for k in 1..count - 1 {
            if !smooth(k) {
                runs.push((start, k - start));
                breaks.push(k);
                start = k;
            }
        }
        runs.push((start, seg_count - start));
    }

    for &(first, len) in &runs {
        pieces.push(vec![strip(&segs, first, len, h, tolerance)]);
    }
    for &k in &breaks {
        let s0 = &segs[(k + seg_count - 1) % seg_count];
        let s1 = &segs[k];
        let join = if line.corners[k] {
            style.join
        } else {
            Join::Round
        };
        if let Some(p) = join_piece(s0, s1, h, join, style.miter_limit, tolerance) {
            pieces.push(vec![p]);
        }
    }
    if !line.closed {
        let (first, last) = (&segs[0], &segs[seg_count - 1]);
        for (p, out, n) in [(first.a, -first.d, first.n), (last.b, last.d, last.n)] {
            match style.cap {
                Cap::Butt => {}
                Cap::Square => pieces.push(vec![vec![
                    p + n * h,
                    p + n * h + out * h,
                    p - n * h + out * h,
                    p - n * h,
                ]]),
                Cap::Round => pieces.push(vec![outer_arc(p, h, n, -n, out, tolerance)]),
            }
        }
    }
}

/// The offsets at smooth vertex `v` between segments `s0` and `s1`: points on the +n
/// side and on the -n side.
fn vertex_offsets(s0: &Seg, s1: &Seg, h: f32, tolerance: f32) -> (Vec<Point>, Vec<Point>) {
    let v = s1.a;
    let cos = s0.d.dot(s1.d);
    let cross = s0.d.cross(s1.d);
    if cross.abs() < 1e-6 && cos > 0.0 {
        return (vec![v + s1.n * h], vec![v - s1.n * h]);
    }
    let inner = |s: f32| v + (s0.n + s1.n) * (s * h / (1.0 + cos));
    // Turning towards +n (clockwise on the canvas) puts +n on the inside.
    if cross > 0.0 {
        let outer = outer_arc(v, h, -s0.n, -s1.n, s0.d, tolerance);
        (vec![inner(1.0)], outer)
    } else {
        let outer = outer_arc(v, h, s0.n, s1.n, s0.d, tolerance);
        (outer, vec![inner(-1.0)])
    }
}

fn strip(segs: &[Seg], first: usize, len: usize, h: f32, tolerance: f32) -> Vec<Point> {
    let n = segs.len();
    let start = &segs[first];
    let end = &segs[(first + len - 1) % n];
    let mut plus = vec![start.a + start.n * h];
    let mut minus = vec![start.a - start.n * h];
    for i in 1..len {
        let (p, m) = vertex_offsets(
            &segs[(first + i - 1) % n],
            &segs[(first + i) % n],
            h,
            tolerance,
        );
        plus.extend(p);
        minus.extend(m);
    }
    plus.push(end.b + end.n * h);
    minus.push(end.b - end.n * h);
    plus.extend(minus.into_iter().rev());
    plus
}

/// A closed polyline with only smooth vertices: the outer and inner offset loops.
fn ring(segs: &[Seg], h: f32, tolerance: f32, pieces: &mut Vec<Vec<Vec<Point>>>) {
    let n = segs.len();
    let mut plus = Vec::new();
    let mut minus = Vec::new();
    for k in 0..n {
        let (p, m) = vertex_offsets(&segs[(k + n - 1) % n], &segs[k], h, tolerance);
        plus.extend(p);
        minus.extend(m);
    }
    minus.reverse();
    pieces.push(vec![plus, minus]);
}

fn join_piece(
    s0: &Seg,
    s1: &Seg,
    h: f32,
    join: Join,
    miter_limit: f32,
    tolerance: f32,
) -> Option<Vec<Point>> {
    let v = s1.a;
    let cos = s0.d.dot(s1.d).clamp(-1.0, 1.0);
    let cross = s0.d.cross(s1.d);
    // The outer side: -n when turning towards +n.
    let s = if cross > 0.0 { -1.0 } else { 1.0 };
    let (a, b) = (s0.n * s, s1.n * s);
    let bevel = || vec![v, v + a * h, v + b * h];
    match join {
        Join::Bevel => Some(bevel()),
        Join::Round => {
            let mut p = vec![v];
            p.extend(outer_arc(v, h, a, b, s0.d, tolerance));
            Some(p)
        }
        Join::Miter => {
            if cos > -0.9999 && 1.0 / ((1.0 + cos) / 2.0).sqrt() <= miter_limit {
                let m = v + (a + b) * (h / (1.0 + cos));
                Some(vec![v, v + a * h, m, v + b * h])
            } else {
                Some(bevel())
            }
        }
    }
}

/// Splits a polyline into its dashes (open polylines); a closed polyline whose pattern
/// is on across its start joins its last dash to its first.
fn dash(line: &Polyline, dash: &Dash) -> Vec<Polyline> {
    let mut pattern: Vec<f32> = dash.array.clone();
    if pattern.len() % 2 == 1 {
        pattern.extend_from_within(..);
    }
    let period: f32 = pattern.iter().sum();
    let mut pts = line.points.clone();
    let mut corners = line.corners.clone();
    if line.closed && !pts.is_empty() {
        pts.push(pts[0]);
        corners.push(corners[0]);
    }
    let length: f32 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    if pattern.is_empty()
        || pattern.iter().any(|v| !v.is_finite() || *v < 0.0)
        || period <= 0.0
        || length / period > MAX_DASHES
    {
        return vec![line.clone()];
    }
    let mut index = 0;
    let mut remaining = pattern[0];
    let mut phase = dash.offset.rem_euclid(period);
    while phase > 0.0 {
        if phase >= remaining {
            phase -= remaining;
            index = (index + 1) % pattern.len();
            remaining = pattern[index];
        } else {
            remaining -= phase;
            phase = 0.0;
        }
    }
    let starts_on = index % 2 == 0;
    let mut on = starts_on;
    let mut out: Vec<Polyline> = Vec::new();
    let mut current = Polyline::default();
    if on {
        current.points.push(pts[0]);
        current.corners.push(false);
    }
    for (w, &corner) in pts.windows(2).zip(&corners[1..]) {
        let (a, b) = (w[0], w[1]);
        let seg = (b - a).length();
        let mut pos = 0.0;
        while seg - pos > remaining {
            pos += remaining;
            let q = Point::new(a.x + (b.x - a.x) * pos / seg, a.y + (b.y - a.y) * pos / seg);
            if on {
                push_point(&mut current, q, false);
                out.push(std::mem::take(&mut current));
            } else {
                current.points.push(q);
                current.corners.push(false);
            }
            on = !on;
            index = (index + 1) % pattern.len();
            remaining = pattern[index];
        }
        remaining -= seg - pos;
        if on {
            push_point(&mut current, b, corner);
        }
    }
    if on && !current.points.is_empty() {
        if line.closed && starts_on && !out.is_empty() {
            let first = out.remove(0);
            current.corners.pop();
            current.points.pop();
            let seam = current.points.len();
            current.points.extend(first.points);
            current.corners.extend(first.corners);
            if let Some(c) = current.corners.get_mut(seam) {
                *c = line.corners[0];
            }
        }
        out.push(current);
    }
    out
}

fn push_point(line: &mut Polyline, p: Point, corner: bool) {
    if line.points.last() == Some(&p) {
        return;
    }
    line.points.push(p);
    line.corners.push(corner);
}

#[cfg(test)]
mod tests {
    use super::super::Paint;
    use super::*;

    fn polyline(points: &[(f32, f32)], closed: bool) -> Polyline {
        Polyline {
            points: points.iter().map(|p| Point::new(p.0, p.1)).collect(),
            corners: vec![true; points.len()],
            closed,
        }
    }

    fn style(width: f32, join: Join, cap: Cap) -> Stroke {
        Stroke {
            join,
            cap,
            ..Stroke::new(Paint::Solid([1.0; 4]), width)
        }
    }

    fn total_area(contours: &[Vec<Point>]) -> f32 {
        contours.iter().map(|c| signed_area(c)).sum()
    }

    #[test]
    fn every_piece_winds_the_same_way() {
        let line = polyline(
            &[(0.0, 0.0), (50.0, 0.0), (50.0, 40.0), (10.0, 60.0)],
            false,
        );
        for join in [Join::Miter, Join::Round, Join::Bevel] {
            for cap in [Cap::Butt, Cap::Square, Cap::Round] {
                let pieces = stroke(std::slice::from_ref(&line), &style(6.0, join, cap), 0.01);
                assert!(!pieces.is_empty());
                for p in &pieces {
                    assert!(
                        signed_area(p) <= 1e-3,
                        "{join:?} {cap:?}: {}",
                        signed_area(p)
                    );
                }
            }
        }
    }

    #[test]
    fn straight_butt_stroke_is_a_rectangle() {
        let line = polyline(&[(0.0, 0.0), (40.0, 0.0)], false);
        let pieces = stroke(&[line], &style(3.0, Join::Miter, Cap::Butt), 0.01);
        assert_eq!(pieces.len(), 1);
        assert!((total_area(&pieces) + 120.0).abs() < 1e-3);
    }

    #[test]
    fn dashes_split_by_length() {
        let line = polyline(&[(0.0, 0.0), (100.0, 0.0)], false);
        let d = Dash {
            array: vec![10.0, 5.0],
            offset: 0.0,
        };
        let dashes = dash(&line, &d);
        assert_eq!(dashes.len(), 7);
        assert_eq!(
            dashes[1].points,
            vec![Point::new(15.0, 0.0), Point::new(25.0, 0.0)]
        );
        assert_eq!(
            dashes[6].points,
            vec![Point::new(90.0, 0.0), Point::new(100.0, 0.0)]
        );
        let shifted = dash(
            &line,
            &Dash {
                array: vec![10.0, 5.0],
                offset: 12.0,
            },
        );
        assert_eq!(
            shifted[0].points,
            vec![Point::new(3.0, 0.0), Point::new(13.0, 0.0)]
        );
    }

    #[test]
    fn closed_dash_joins_across_the_start() {
        let square = polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], true);
        let dashes = dash(
            &square,
            &Dash {
                array: vec![6.0, 2.0],
                offset: 4.0,
            },
        );
        // 40 long: on 0-2, 4-10, 12-18, 20-26, 28-34 and 36-40, which continues into 0-2.
        assert_eq!(dashes.len(), 5);
        let joined = dashes.last().expect("dashes");
        assert_eq!(joined.points.first(), Some(&Point::new(0.0, 4.0)));
        assert_eq!(joined.points.last(), Some(&Point::new(2.0, 0.0)));
        assert!(
            joined.corners[joined
                .points
                .iter()
                .position(|p| *p == Point::new(0.0, 0.0))
                .expect("corner")]
        );
    }
}
