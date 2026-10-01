//! CPU reference for shape rasterisation and paint, in f64, independent of the GPU's
//! accumulation: exact pixel coverage of polygons by clipping each one to the pixel
//! square (Sutherland-Hodgman), winding numbers for supersampled coverage of
//! self-intersecting outlines, and gradient evaluation.

use super::{FillRule, Paint};

pub type P = [f64; 2];

/// Signed area (shoelace) of a closed polygon; positive when it turns clockwise on a
/// y-down canvas.
pub fn signed_area(poly: &[P]) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        / 2.0
}

fn clip(poly: &[P], inside: impl Fn(P) -> bool, cross: impl Fn(P, P) -> P) -> Vec<P> {
    let mut out = Vec::with_capacity(poly.len() + 4);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        match (inside(a), inside(b)) {
            (true, true) => out.push(b),
            (true, false) => out.push(cross(a, b)),
            (false, true) => {
                out.push(cross(a, b));
                out.push(b);
            }
            (false, false) => {}
        }
    }
    out
}

/// Signed area of `poly` inside the square `[x, x + 1] x [y, y + 1]`.
pub fn pixel_area(poly: &[P], x: f64, y: f64) -> f64 {
    let at_x = |a: P, b: P, v: f64| [v, a[1] + (b[1] - a[1]) * (v - a[0]) / (b[0] - a[0])];
    let at_y = |a: P, b: P, v: f64| [a[0] + (b[0] - a[0]) * (v - a[1]) / (b[1] - a[1]), v];
    let mut p = clip(poly, |q| q[0] >= x, |a, b| at_x(a, b, x));
    p = clip(&p, |q| q[0] <= x + 1.0, |a, b| at_x(a, b, x + 1.0));
    p = clip(&p, |q| q[1] >= y, |a, b| at_y(a, b, y));
    p = clip(&p, |q| q[1] <= y + 1.0, |a, b| at_y(a, b, y + 1.0));
    if p.len() < 3 { 0.0 } else { signed_area(&p) }
}

/// Coverage of every pixel of a `w` x `h` canvas, row-major, by polygons combined by
/// signed area: disjoint polygons turning the same way add, a hole turning the other way
/// subtracts. Exact for outlines where that is the fill (simple polygons, rings).
pub fn polygon_coverage(polys: &[Vec<P>], (w, h): (u32, u32)) -> Vec<f64> {
    let mut out = vec![0.0; (w * h) as usize];
    for poly in polys {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in poly {
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
        let xs = (x0.floor().max(0.0) as u32)..(x1.ceil().min(f64::from(w)).max(0.0) as u32);
        let ys = (y0.floor().max(0.0) as u32)..(y1.ceil().min(f64::from(h)).max(0.0) as u32);
        for y in ys {
            for x in xs.clone() {
                out[(y * w + x) as usize] += pixel_area(poly, f64::from(x), f64::from(y));
            }
        }
    }
    out.iter_mut().for_each(|v| *v = v.abs().min(1.0));
    out
}

/// Winding number of the closed contours around `p`, counting a contour that turns
/// clockwise on a y-down canvas as positive.
pub fn winding(contours: &[Vec<P>], p: P) -> i32 {
    let mut w = 0;
    for c in contours {
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            let side = (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]);
            if a[1] <= p[1] && b[1] > p[1] && side > 0.0 {
                w += 1;
            } else if a[1] > p[1] && b[1] <= p[1] && side < 0.0 {
                w -= 1;
            }
        }
    }
    w
}

pub fn inside(w: i32, rule: FillRule) -> bool {
    match rule {
        FillRule::NonZero => w != 0,
        FillRule::EvenOdd => w % 2 != 0,
    }
}

/// Coverage of pixel `(x, y)` from `n` x `n` samples at cell centres. Error up to about
/// `k / n` for a pixel `k` edges cross.
pub fn supersampled(contours: &[Vec<P>], rule: FillRule, x: u32, y: u32, n: u32) -> f64 {
    let mut hits = 0;
    for j in 0..n {
        for i in 0..n {
            let p = [
                f64::from(x) + (f64::from(i) + 0.5) / f64::from(n),
                f64::from(y) + (f64::from(j) + 0.5) / f64::from(n),
            ];
            if inside(winding(contours, p), rule) {
                hits += 1;
            }
        }
    }
    f64::from(hits) / f64::from(n * n)
}

/// `paint` at a point in paint (shape-local) space, premultiplied.
pub fn paint(paint: &Paint, p: P) -> [f64; 4] {
    let pre = |c: [f32; 4]| {
        let a = f64::from(c[3]);
        [
            f64::from(c[0]) * a,
            f64::from(c[1]) * a,
            f64::from(c[2]) * a,
            a,
        ]
    };
    let t = match paint {
        Paint::Solid(c) => return pre(*c),
        Paint::Linear { start, end, .. } => {
            let d = [f64::from(end.x - start.x), f64::from(end.y - start.y)];
            let q = [p[0] - f64::from(start.x), p[1] - f64::from(start.y)];
            (q[0] * d[0] + q[1] * d[1]) / (d[0] * d[0] + d[1] * d[1]).max(1e-20)
        }
        Paint::Radial { center, radius, .. } => {
            let q = [p[0] - f64::from(center.x), p[1] - f64::from(center.y)];
            (q[0] * q[0] + q[1] * q[1]).sqrt() / f64::from(*radius).max(1e-20)
        }
    };
    let t = t.clamp(0.0, 1.0);
    let stops = paint.sorted_stops();
    let Some(first) = stops.first() else {
        return [0.0; 4];
    };
    match stops.iter().position(|s| t < f64::from(s.offset)) {
        Some(0) => pre(first.color),
        None => pre(stops[stops.len() - 1].color),
        Some(i) => {
            let (s0, s1) = (stops[i - 1], stops[i]);
            let f = (t - f64::from(s0.offset)) / f64::from(s1.offset - s0.offset);
            let (c0, c1) = (pre(s0.color), pre(s1.color));
            [0, 1, 2, 3].map(|k| c0[k] + (c1[k] - c0[k]) * f)
        }
    }
}

/// Premultiplied source-over.
pub fn over(dst: [f64; 4], src: [f64; 4]) -> [f64; 4] {
    [0, 1, 2, 3].map(|k| src[k] + dst[k] * (1.0 - src[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_area_of_a_half_plane_edge() {
        let square = vec![[0.25, -5.0], [10.0, -5.0], [10.0, 5.0], [0.25, 5.0]];
        assert!((pixel_area(&square, 0.0, 0.0).abs() - 0.75).abs() < 1e-12);
        let tri = vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        assert!((pixel_area(&tri, 0.0, 0.0).abs() - 0.5).abs() < 1e-12);
    }

    #[test]
    fn winding_counts_direction() {
        let cw = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let ccw: Vec<P> = cw.iter().rev().copied().collect();
        assert_eq!(winding(&[cw.clone()], [5.0, 5.0]), 1);
        assert_eq!(winding(&[ccw.clone()], [5.0, 5.0]), -1);
        assert_eq!(winding(&[cw.clone(), cw], [5.0, 5.0]), 2);
        assert_eq!(winding(&[ccw], [15.0, 5.0]), 0);
    }
}
