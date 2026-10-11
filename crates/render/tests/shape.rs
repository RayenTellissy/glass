//! Vector shapes on the GPU against the CPU reference in `render::shape::reference`:
//! coverage of each primitive (rectangle, rounded rectangle, rotated ellipse, polygon,
//! path with quadratic and cubic curves), fill rules, strokes (width, caps, joins,
//! dashes), gradients with stops, paint order across atlas fills, the mask coverage
//! node, and the GPU time of a 4K frame of 50 mixed shapes.
//!
//! References are exact pixel areas of polygons (clipped to each pixel square): straight
//! edges exactly, curves as polygons of 4096 or more points per curve. Tolerances, per
//! pixel: 2e-3 for straight edges (f16 output rounding is 4.9e-4 at 1, the rest is f32
//! geometry), 0.025 where curves are involved (the GPU's polygon is within
//! `shape::TOLERANCE` = 1/64 px of the curve, and the area between a chord and its arc
//! inside one pixel is at most that times the chord's length there, up to 1.42 px),
//! 2e-3 for gradient colours at pixel centres.

mod common;

use std::f64::consts::{PI, TAU};

use render::graph::{Graph, TexturePool};
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::shape::reference::{self, P};
use render::shape::{
    Affine, Cap, CoverageNode, Dash, FillRule, Geometry, Join, Paint, Path, Point, Rect, Shape,
    ShapeNode, Stop, Stroke,
};
use render::wgpu;

const STRAIGHT: f64 = 2e-3;
const CURVED: f64 = 0.025;
const WHITE: [f32; 4] = [1.0; 4];

/// A device with blendable `R32Float` (and timestamps when available).
fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .ok()?;
    let features =
        adapter.features() & (wgpu::Features::FLOAT32_BLENDABLE | wgpu::Features::TIMESTAMP_QUERY);
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        ..Default::default()
    }))
    .ok()
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    pool: TexturePool,
}

impl Gpu {
    fn new() -> Option<Self> {
        let Some((device, queue)) = device() else {
            eprintln!("skipped: no GPU adapter");
            return None;
        };
        let renderer = Renderer::new(&device, &queue);
        Some(Self {
            device,
            queue,
            renderer,
            pool: TexturePool::new(),
        })
    }

    fn node(&self, canvas: (u32, u32), shapes: &[Shape]) -> ShapeNode {
        ShapeNode::new(&self.renderer, canvas, shapes)
    }

    fn render(&mut self, canvas: (u32, u32), shapes: &[Shape]) -> Vec<[f32; 4]> {
        let node = self.node(canvas, shapes);
        self.render_node(canvas, node)
    }

    fn render_node(&mut self, canvas: (u32, u32), node: ShapeNode) -> Vec<[f32; 4]> {
        let out = common::blank(&self.device, wgpu::TextureFormat::Rgba16Float, canvas);
        let view = out.create_view(&Default::default());
        let mut g = Graph::new();
        let id = g.add(node, &[]);
        g.export(id.out(0), &view);
        g.compile()
            .expect("compiles")
            .execute(&self.renderer, &mut self.pool, None);
        common::read(&self.device, &self.queue, &out)
    }
}

fn pt(p: P) -> Point {
    Point::new(p[0] as f32, p[1] as f32)
}

/// Max and mean absolute difference between GPU coverage (alpha) and the reference,
/// asserted within `tol`; the GPU's premultiplied white also has red equal to alpha.
fn check_coverage(gpu: &[[f32; 4]], reference: &[f64], tol: f64, what: &str) {
    check_coverage_except(gpu, reference, 0, tol, what, &[]);
}

/// Coverage limit near points where overlap meets outside (see below).
const AT_OVERLAP: f64 = 0.5;

/// The accumulation gives `min(|integral of w|, 1)` per pixel (folded for even-odd), not
/// the integral of `min(|w|, 1)`: where a region wound twice (overlapping stroke pieces,
/// a pentagram's centre) and an empty one meet inside one pixel, that pixel's coverage
/// is off by up to the smaller of the two areas. Pixels of a `width`-wide canvas within
/// 1.5 px of such `hot` points are held to [`AT_OVERLAP`] instead of `tol`.
fn check_coverage_except(
    gpu: &[[f32; 4]],
    reference: &[f64],
    width: usize,
    tol: f64,
    what: &str,
    hot: &[P],
) {
    assert_eq!(gpu.len(), reference.len());
    let near = |i: usize| {
        let (x, y) = (
            (i % width.max(1)) as f64 + 0.5,
            (i / width.max(1)) as f64 + 0.5,
        );
        hot.iter().any(|p| (p[0] - x).hypot(p[1] - y) <= 1.5)
    };
    let mut max = 0.0f64;
    let mut max_hot = 0.0f64;
    let mut sum = 0.0;
    let mut hot_pixels = 0;
    for (i, (g, r)) in gpu.iter().zip(reference).enumerate() {
        let d = (f64::from(g[3]) - r).abs();
        sum += d;
        let limit = if near(i) {
            hot_pixels += 1;
            max_hot = max_hot.max(d);
            AT_OVERLAP
        } else {
            max = max.max(d);
            tol
        };
        assert!(d <= limit, "{what}: pixel {i}: gpu {}, reference {r}", g[3]);
        assert!((g[0] - g[3]).abs() < 1e-3, "{what}: pixel {i}: {g:?}");
    }
    let area_gpu: f64 = gpu.iter().map(|g| f64::from(g[3])).sum();
    let area_ref: f64 = reference.iter().sum();
    println!(
        "{what}: max {max:.5} (at overlaps {max_hot:.5}), mean {:.6}, area gpu {area_gpu:.3} \
         reference {area_ref:.3}",
        sum / gpu.len() as f64
    );
    let slack = 1e-3 * area_ref.max(1.0) + 0.05 + AT_OVERLAP * f64::from(hot_pixels);
    assert!(
        (area_gpu - area_ref).abs() <= slack,
        "{what}: area {area_gpu} vs {area_ref}"
    );
}

fn arc_points(c: P, r: f64, a0: f64, sweep: f64, n: usize) -> Vec<P> {
    (0..=n)
        .map(|i| {
            let a = a0 + sweep * i as f64 / n as f64;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect()
}

fn circle(c: P, r: f64) -> Vec<P> {
    let mut p = arc_points(c, r, 0.0, TAU, 16384);
    p.pop();
    p
}

fn rect_poly(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P> {
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

fn solid(geometry: Geometry) -> Shape {
    Shape::new(geometry).filled(Paint::Solid(WHITE))
}

#[test]
fn rectangle_edges_are_exact() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (48, 32);
    let shape = solid(Geometry::Rect {
        rect: Rect::new(10.25, 5.5, 20.5, 12.75),
        radius: 0.0,
    });
    let out = gpu.render(canvas, &[shape]);
    let reference = reference::polygon_coverage(&[rect_poly(10.25, 5.5, 30.75, 18.25)], canvas);
    check_coverage(&out, &reference, STRAIGHT, "rectangle");
    let at = |x: usize, y: usize| out[y * 48 + x][3];
    assert!((at(10, 10) - 0.75).abs() < 1e-3, "left edge {}", at(10, 10));
    assert!((at(20, 5) - 0.5).abs() < 1e-3, "top edge {}", at(20, 5));
    assert!((at(30, 18) - 0.1875).abs() < 1e-3, "corner {}", at(30, 18));
    assert_eq!(at(9, 10), 0.0);
    assert_eq!(at(31, 10), 0.0, "nothing is left right of the shape");
    assert_eq!(at(20, 10), 1.0);
}

#[test]
fn rounded_rectangle_matches_the_reference() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 40);
    let (x0, y0, w, h, r) = (6.3, 4.7, 50.2, 30.9, 9.5);
    let shape = solid(Geometry::Rect {
        rect: Rect::new(x0 as f32, y0 as f32, w as f32, h as f32),
        radius: r as f32,
    });
    let out = gpu.render(canvas, &[shape]);
    let mut poly = Vec::new();
    for (cx, cy, a0) in [
        (x0 + w - r, y0 + r, -PI / 2.0),
        (x0 + w - r, y0 + h - r, 0.0),
        (x0 + r, y0 + h - r, PI / 2.0),
        (x0 + r, y0 + r, PI),
    ] {
        poly.extend(arc_points([cx, cy], r, a0, PI / 2.0, 4096));
    }
    let reference = reference::polygon_coverage(&[poly], canvas);
    check_coverage(&out, &reference, CURVED, "rounded rectangle");
}

#[test]
fn rotated_ellipse_matches_the_reference() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 48);
    let (angle, tx, ty) = (0.5f64, 32.3, 24.6);
    let shape = solid(Geometry::Ellipse {
        center: Point::new(0.0, 0.0),
        radii: [26.0, 11.0],
    })
    .transformed(Affine::rotate(angle as f32).then(Affine::translate(tx as f32, ty as f32)));
    let out = gpu.render(canvas, &[shape]);
    let poly: Vec<P> = (0..16384)
        .map(|i| {
            let t = TAU * f64::from(i) / 16384.0;
            let (x, y) = (26.0 * t.cos(), 11.0 * t.sin());
            [
                x * angle.cos() - y * angle.sin() + tx,
                x * angle.sin() + y * angle.cos() + ty,
            ]
        })
        .collect();
    let reference = reference::polygon_coverage(&[poly], canvas);
    check_coverage(&out, &reference, CURVED, "rotated ellipse");
    let area: f64 = out.iter().map(|p| f64::from(p[3])).sum();
    assert!(
        (area - PI * 26.0 * 11.0).abs() < 1e-3 * PI * 26.0 * 11.0,
        "area {area}"
    );
}

#[test]
fn concave_polygon_matches_the_reference() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (56, 40);
    let points: Vec<P> = vec![
        [4.3, 20.1],
        [24.7, 3.2],
        [24.2, 12.9],
        [51.6, 11.4],
        [50.1, 28.8],
        [23.9, 27.3],
        [25.4, 37.6],
    ];
    let shape = solid(Geometry::Polygon(points.iter().copied().map(pt).collect()));
    let out = gpu.render(canvas, &[shape]);
    let reference = reference::polygon_coverage(&[points], canvas);
    check_coverage(&out, &reference, STRAIGHT, "concave polygon");
}

fn quad(p0: P, c: P, p1: P, t: f64) -> P {
    let u = 1.0 - t;
    [0, 1].map(|k| u * u * p0[k] + 2.0 * u * t * c[k] + t * t * p1[k])
}

fn cubic(p0: P, c1: P, c2: P, p1: P, t: f64) -> P {
    let u = 1.0 - t;
    [0, 1].map(|k| {
        u * u * u * p0[k] + 3.0 * u * u * t * c1[k] + 3.0 * u * t * t * c2[k] + t * t * t * p1[k]
    })
}

#[test]
fn path_with_curves_matches_the_reference() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 48);
    let (a, qc, b) = ([6.0, 40.5], [20.0, 0.0], [34.2, 40.5]);
    let (c1, c2, d) = ([40.0, 10.0], [56.0, 10.0], [58.7, 40.5]);
    let path = Path::new()
        .move_to(a[0] as f32, a[1] as f32)
        .quad_to(qc[0] as f32, qc[1] as f32, b[0] as f32, b[1] as f32)
        .cubic_to(
            c1[0] as f32,
            c1[1] as f32,
            c2[0] as f32,
            c2[1] as f32,
            d[0] as f32,
            d[1] as f32,
        )
        .line_to(40.0, 45.3)
        .close();
    let out = gpu.render(canvas, &[solid(Geometry::Path(path))]);
    let n = 8192;
    let mut poly: Vec<P> = (0..n)
        .map(|i| quad(a, qc, b, f64::from(i) / f64::from(n)))
        .collect();
    poly.extend((0..=n).map(|i| cubic(b, c1, c2, d, f64::from(i) / f64::from(n))));
    poly.push([40.0, 45.3]);
    let reference = reference::polygon_coverage(&[poly], canvas);
    check_coverage(&out, &reference, CURVED, "path with curves");
}

#[test]
fn fill_rules_on_a_pentagram_and_holes() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 64);
    let (c, r) = ([32.3, 32.6], 27.0);
    let at = |radius: f64, deg: f64| {
        let a = deg.to_radians();
        [c[0] + radius * a.cos(), c[1] + radius * a.sin()]
    };
    let outer: Vec<P> = (0..5).map(|k| at(r, -90.0 + 72.0 * f64::from(k))).collect();
    let ri = r * 72f64.to_radians().cos() / 36f64.to_radians().cos();
    let inner: Vec<P> = (0..5)
        .map(|k| at(ri, -54.0 + 72.0 * f64::from(k)))
        .collect();
    let star: Vec<P> = (0..5).flat_map(|k| [outer[k], inner[k]]).collect();
    let mut path = Path::new().move_to(outer[0][0] as f32, outer[0][1] as f32);
    for k in [2, 4, 1, 3] {
        path = path.line_to(outer[k][0] as f32, outer[k][1] as f32);
    }
    let path = path.close();
    for (rule, polys) in [
        (FillRule::NonZero, vec![star.clone()]),
        (
            FillRule::EvenOdd,
            vec![star.clone(), inner.iter().rev().copied().collect()],
        ),
    ] {
        let shape = Shape::new(Geometry::Path(path.clone())).filled_with(Paint::Solid(WHITE), rule);
        let out = gpu.render(canvas, &[shape]);
        let reference = reference::polygon_coverage(&polys, canvas);
        check_coverage_except(
            &out,
            &reference,
            64,
            STRAIGHT,
            &format!("pentagram {rule:?}"),
            &inner,
        );
        let centre = out[32 * 64 + 32][3];
        let expected = if rule == FillRule::NonZero { 1.0 } else { 0.0 };
        assert!((centre - expected).abs() < 1e-3, "{rule:?} centre {centre}");
    }

    // A square with a square hole: the hole is empty for even-odd either way, and for
    // non-zero only when it winds the other way.
    let square = |x0: f32, y0: f32, x1: f32, y1: f32, reverse: bool| {
        let mut pts = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
        if reverse {
            pts.reverse();
        }
        let mut p = Path::new().move_to(pts[0].0, pts[0].1);
        for q in &pts[1..] {
            p = p.line_to(q.0, q.1);
        }
        p.close()
    };
    for reverse in [false, true] {
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            let mut path = square(8.5, 8.5, 55.5, 55.5, false);
            path.verbs
                .extend(square(20.25, 20.25, 43.75, 43.75, reverse).verbs);
            let shape = Shape::new(Geometry::Path(path)).filled_with(Paint::Solid(WHITE), rule);
            let out = gpu.render(canvas, &[shape]);
            let hole = rule == FillRule::EvenOdd || reverse;
            let mut polys = vec![rect_poly(8.5, 8.5, 55.5, 55.5)];
            if hole {
                polys.push(
                    rect_poly(20.25, 20.25, 43.75, 43.75)
                        .into_iter()
                        .rev()
                        .collect(),
                );
            }
            let reference = reference::polygon_coverage(&polys, canvas);
            check_coverage(
                &out,
                &reference,
                STRAIGHT,
                &format!("square hole {rule:?} reversed {reverse}"),
            );
        }
    }
}

fn stroke(width: f32, join: Join, cap: Cap) -> Stroke {
    Stroke {
        join,
        cap,
        miter_limit: 10.0,
        ..Stroke::new(Paint::Solid(WHITE), width)
    }
}

fn stroked(geometry: Geometry, s: Stroke) -> Shape {
    Shape::new(geometry).stroked(s)
}

fn line(a: P, b: P) -> Geometry {
    Geometry::Path(
        Path::new()
            .move_to(a[0] as f32, a[1] as f32)
            .line_to(b[0] as f32, b[1] as f32),
    )
}

#[test]
fn stroke_width_and_caps() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 40);
    let (a, b) = ([8.0, 20.5], [56.0, 20.5]);
    for width in [3.0, 2.5, 0.5] {
        let out = gpu.render(
            canvas,
            &[stroked(line(a, b), stroke(width, Join::Miter, Cap::Butt))],
        );
        let h = f64::from(width) / 2.0;
        let reference =
            reference::polygon_coverage(&[rect_poly(8.0, 20.5 - h, 56.0, 20.5 + h)], canvas);
        check_coverage(
            &out,
            &reference,
            STRAIGHT,
            &format!("butt line width {width}"),
        );
    }
    let out = gpu.render(
        canvas,
        &[stroked(line(a, b), stroke(3.0, Join::Miter, Cap::Square))],
    );
    let reference = reference::polygon_coverage(&[rect_poly(6.5, 19.0, 57.5, 22.0)], canvas);
    check_coverage(&out, &reference, STRAIGHT, "square caps");

    let out = gpu.render(
        canvas,
        &[stroked(line(a, b), stroke(9.0, Join::Miter, Cap::Round))],
    );
    let mut stadium = arc_points(b, 4.5, -PI / 2.0, PI, 4096);
    stadium.extend(arc_points(a, 4.5, PI / 2.0, PI, 4096));
    let reference = reference::polygon_coverage(&[stadium], canvas);
    check_coverage(&out, &reference, CURVED, "round caps");

    let dashed = Stroke {
        dash: Some(Dash {
            array: vec![10.0, 5.0],
            offset: 0.0,
        }),
        ..stroke(3.0, Join::Miter, Cap::Butt)
    };
    let out = gpu.render(canvas, &[stroked(line(a, b), dashed)]);
    let dashes: Vec<Vec<P>> = [(8.0, 18.0), (23.0, 33.0), (38.0, 48.0), (53.0, 56.0)]
        .iter()
        .map(|&(x0, x1)| rect_poly(x0, 19.0, x1, 22.0))
        .collect();
    let reference = reference::polygon_coverage(&dashes, canvas);
    check_coverage(&out, &reference, STRAIGHT, "dashes");
}

#[test]
fn circle_stroke_is_a_ring() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 64);
    let c = [31.7, 32.2];
    let shape = stroked(
        Geometry::Ellipse {
            center: pt(c),
            radii: [20.0, 20.0],
        },
        stroke(4.0, Join::Miter, Cap::Butt),
    );
    let out = gpu.render(canvas, &[shape]);
    let inner: Vec<P> = circle(c, 18.0).into_iter().rev().collect();
    let reference = reference::polygon_coverage(&[circle(c, 22.0), inner], canvas);
    check_coverage(&out, &reference, CURVED, "circle stroke");
}

fn intersect(p: P, d: P, q: P, e: P) -> P {
    let t = ((q[0] - p[0]) * e[1] - (q[1] - p[1]) * e[0]) / (d[0] * e[1] - d[1] * e[0]);
    [p[0] + d[0] * t, p[1] + d[1] * t]
}

#[test]
fn joins_match_the_reference() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 48);
    let (p0, v, p2) = ([10.2, 40.3], [30.6, 8.1], [51.4, 40.7]);
    let h = 3.0;
    let unit = |a: P, b: P| {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let l = (dx * dx + dy * dy).sqrt();
        [dx / l, dy / l]
    };
    let (d0, d1) = (unit(p0, v), unit(v, p2));
    // The turn is clockwise on the canvas (towards d.perp()), so the outer side is
    // (dy, -dx).
    assert!(d0[0] * d1[1] - d0[1] * d1[0] > 0.0);
    let (n0, n1) = ([d0[1], -d0[0]], [d1[1], -d1[0]]);
    let off = |p: P, n: P, s: f64| [p[0] + n[0] * h * s, p[1] + n[1] * h * s];
    let inner = intersect(off(p0, n0, -1.0), d0, off(p2, n1, -1.0), d1);
    let outer_miter = intersect(off(p0, n0, 1.0), d0, off(p2, n1, 1.0), d1);
    let a0 = n0[1].atan2(n0[0]);
    let mut sweep = n1[1].atan2(n1[0]) - a0;
    if sweep > PI {
        sweep -= TAU;
    }
    if sweep < -PI {
        sweep += TAU;
    }
    let cases = [
        (Join::Miter, vec![outer_miter]),
        (Join::Bevel, vec![off(v, n0, 1.0), off(v, n1, 1.0)]),
        (Join::Round, arc_points(v, h, a0, sweep, 4096)),
    ];
    let path = Geometry::Path(
        Path::new()
            .move_to(p0[0] as f32, p0[1] as f32)
            .line_to(v[0] as f32, v[1] as f32)
            .line_to(p2[0] as f32, p2[1] as f32),
    );
    for (join, corner) in cases {
        let shape = stroked(path.clone(), stroke(6.0, join, Cap::Butt));
        let out = gpu.render(canvas, &[shape]);
        let mut poly = vec![off(p0, n0, 1.0)];
        poly.extend(corner);
        poly.extend([
            off(p2, n1, 1.0),
            off(p2, n1, -1.0),
            inner,
            off(p0, n0, -1.0),
        ]);
        let reference = reference::polygon_coverage(&[poly], canvas);
        let tol = if join == Join::Round {
            CURVED
        } else {
            STRAIGHT
        };
        check_coverage_except(
            &out,
            &reference,
            64,
            tol,
            &format!("{join:?} join"),
            &[inner],
        );
    }
}

#[test]
fn gradient_stops_match_the_reference() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 24);
    let stops = vec![
        Stop::new(0.75, [0.0, 0.0, 1.0, 1.0]),
        Stop::new(0.0, [1.0, 0.0, 0.0, 1.0]),
        Stop::new(0.25, [0.0, 1.0, 0.0, 0.5]),
        Stop::new(0.6, [1.0, 1.0, 0.0, 1.0]),
        Stop::new(0.6, [0.0, 1.0, 1.0, 1.0]),
        Stop::new(1.0, [1.5, 1.5, 1.5, 1.0]),
    ];
    let transform = Affine::scale(2.0, 1.0).then(Affine::translate(1.0, 0.0));
    let paints = [
        Paint::Linear {
            start: Point::new(4.0, 0.0),
            end: Point::new(28.0, 6.0),
            stops: stops.clone(),
        },
        Paint::Radial {
            center: Point::new(15.5, 12.0),
            radius: 10.0,
            stops: stops.clone(),
        },
    ];
    let inverse = transform.inverse().expect("invertible");
    for paint in paints {
        let shape = Shape::new(Geometry::Rect {
            rect: Rect::new(-0.5, 0.0, 32.0, 24.0),
            radius: 0.0,
        })
        .filled(paint.clone())
        .transformed(transform);
        let out = gpu.render(canvas, &[shape]);
        let mut max = 0.0f64;
        for y in 0..24 {
            for x in 0..64 {
                let local = inverse.apply(Point::new(x as f32 + 0.5, y as f32 + 0.5));
                let expected = reference::paint(&paint, [f64::from(local.x), f64::from(local.y)]);
                let got = out[y * 64 + x];
                for k in 0..4 {
                    let d = (f64::from(got[k]) - expected[k]).abs();
                    max = max.max(d);
                    assert!(
                        d <= 2e-3 * expected[k].abs().max(1.0),
                        "{paint:?} at ({x}, {y}): gpu {got:?}, reference {expected:?}"
                    );
                }
            }
        }
        println!("gradient: max colour difference {max:.5}");
    }
}

#[test]
fn atlas_fills_keep_paint_order() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 40);
    let colours = [
        [1.0, 0.0, 0.0, 0.6],
        [0.0, 1.0, 0.0, 0.5],
        [0.0, 0.0, 1.0, 0.7],
        [1.0, 1.0, 1.0, 0.4],
    ];
    let rects: Vec<(f64, f64)> = vec![(0.5, 0.25), (6.25, 4.5), (11.75, 8.0), (3.5, 9.75)];
    let shapes: Vec<Shape> = rects
        .iter()
        .zip(colours)
        .map(|(&(x, y), c)| {
            Shape::new(Geometry::Rect {
                rect: Rect::new(x as f32, y as f32, 50.0, 29.0),
                radius: 0.0,
            })
            .filled(Paint::Solid(c))
        })
        .collect();
    let node = gpu.node(canvas, &shapes);
    assert_eq!(node.batch_count(), 4, "each 51x30 box fills a 64x40 atlas");
    let out = gpu.render_node(canvas, node);
    let coverages: Vec<Vec<f64>> = rects
        .iter()
        .map(|&(x, y)| reference::polygon_coverage(&[rect_poly(x, y, x + 50.0, y + 29.0)], canvas))
        .collect();
    for i in 0..(64 * 40) {
        let mut expected = [0.0; 4];
        for (cov, c) in coverages.iter().zip(colours) {
            let src = reference::paint(&Paint::Solid(c), [0.0, 0.0]).map(|v| v * cov[i]);
            expected = reference::over(expected, src);
        }
        for k in 0..4 {
            let d = (f64::from(out[i][k]) - expected[k]).abs();
            assert!(
                d <= STRAIGHT,
                "pixel {i}: gpu {:?}, reference {expected:?}",
                out[i]
            );
        }
    }
}

/// Reads an `R16Float` texture's red channel.
fn read_r16f(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<f32> {
    let (w, h) = (texture.width(), texture.height());
    let row = (w * 2).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let data = buffer
        .slice(..)
        .get_mapped_range()
        .expect("mapped")
        .to_vec();
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let at = y * row as usize + x * 2;
            out.push(common::f16_to_f32(u16::from_le_bytes([
                data[at],
                data[at + 1],
            ])));
        }
    }
    out
}

#[test]
fn coverage_node_matches_the_shape_layer() {
    let Some(mut gpu) = Gpu::new() else { return };
    let canvas = (64, 48);
    let shape = Shape::new(Geometry::Ellipse {
        center: Point::new(30.4, 22.7),
        radii: [40.0, 15.0],
    })
    .filled_with(Paint::Solid(WHITE), FillRule::EvenOdd)
    .stroked(stroke(3.0, Join::Round, Cap::Round));
    let layer = gpu.render(canvas, std::slice::from_ref(&shape));
    for outline in [shape.fill_outline(), shape.stroke_outline()]
        .into_iter()
        .flatten()
    {
        let node = CoverageNode::new(&gpu.renderer, canvas, &outline);
        let out = common::blank(&gpu.device, CoverageNode::FORMAT, canvas);
        let view = out.create_view(&Default::default());
        let mut g = Graph::new();
        let id = g.add(node, &[]);
        g.export(id.out(0), &view);
        g.compile()
            .expect("compiles")
            .execute(&gpu.renderer, &mut gpu.pool, None);
        let coverage = read_r16f(&gpu.device, &gpu.queue, &out);
        // The same outline drawn alone as a layer has alpha equal to its coverage.
        let alone = gpu.render_node(canvas, {
            let mut s = shape.clone();
            if outline.rule == FillRule::EvenOdd {
                s.stroke = None;
            } else {
                s.fill = None;
            }
            gpu.node(canvas, &[s])
        });
        for (c, a) in coverage.iter().zip(&alone) {
            assert!(
                (c - a[3]).abs() < 1e-3,
                "coverage {c}, layer alpha {}",
                a[3]
            );
        }
        assert!(coverage.iter().any(|c| *c > 0.99) && coverage.contains(&0.0));
    }
    assert!(layer.iter().any(|p| p[3] > 0.99));
}

/// 50 shapes over a 4K canvas, deterministic: rounded rectangles, ellipses, stars,
/// cubic blobs and polylines, 80 to 900 px, solid, linear and radial paints, a third
/// stroked (some dashed, round or miter joins).
fn mixed_shapes() -> Vec<Shape> {
    let mut state = 0x9e37_79b9u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state >> 8) as f32 / (1 << 24) as f32
    };
    let mut shapes = Vec::new();
    for i in 0..50 {
        let (cx, cy) = (next() * 3840.0, next() * 2160.0);
        let size = 80.0 + next() * 820.0;
        let colour = [next(), next(), next(), 0.5 + next() * 0.5];
        let geometry = match i % 5 {
            0 => Geometry::Rect {
                rect: Rect::new(cx - size / 2.0, cy - size / 3.0, size, size / 1.5),
                radius: size * 0.1,
            },
            1 => Geometry::Ellipse {
                center: Point::new(cx, cy),
                radii: [size / 2.0, size / 3.0],
            },
            2 => Geometry::Polygon(
                (0..10)
                    .map(|k| {
                        let r = if k % 2 == 0 { size / 2.0 } else { size / 5.0 };
                        let a = k as f32 * std::f32::consts::PI / 5.0;
                        Point::new(cx + r * a.cos(), cy + r * a.sin())
                    })
                    .collect(),
            ),
            3 => Geometry::Path(
                Path::new()
                    .move_to(cx - size / 2.0, cy)
                    .cubic_to(
                        cx - size / 2.0,
                        cy - size / 2.0,
                        cx,
                        cy - size / 2.0,
                        cx,
                        cy,
                    )
                    .cubic_to(
                        cx,
                        cy + size / 2.0,
                        cx + size / 2.0,
                        cy + size / 2.0,
                        cx + size / 2.0,
                        cy,
                    )
                    .quad_to(cx, cy + size, cx - size / 2.0, cy)
                    .close(),
            ),
            _ => Geometry::Path(
                Path::new()
                    .move_to(cx - size / 2.0, cy + size / 3.0)
                    .line_to(cx - size / 6.0, cy - size / 3.0)
                    .line_to(cx + size / 6.0, cy + size / 4.0)
                    .quad_to(
                        cx + size / 2.0,
                        cy - size / 2.0,
                        cx + size / 2.0,
                        cy + size / 3.0,
                    ),
            ),
        };
        let paint = match i % 3 {
            0 => Paint::Solid(colour),
            1 => Paint::Linear {
                start: Point::new(cx - size / 2.0, cy),
                end: Point::new(cx + size / 2.0, cy + size / 4.0),
                stops: vec![
                    Stop::new(0.0, colour),
                    Stop::new(0.5, [1.0, 1.0, 1.0, 0.8]),
                    Stop::new(1.0, [0.0, 0.2, 0.8, 1.0]),
                ],
            },
            _ => Paint::Radial {
                center: Point::new(cx, cy),
                radius: size / 2.0,
                stops: vec![Stop::new(0.0, [1.0, 0.9, 0.5, 1.0]), Stop::new(1.0, colour)],
            },
        };
        let mut shape = Shape::new(geometry);
        if i % 5 != 4 {
            shape = shape.filled(paint);
        }
        if i % 3 == 0 || i % 5 == 4 {
            shape = shape.stroked(Stroke {
                join: if i % 2 == 0 { Join::Round } else { Join::Miter },
                cap: Cap::Round,
                dash: (i % 4 == 0).then(|| Dash {
                    array: vec![size / 8.0, size / 16.0],
                    offset: 0.0,
                }),
                ..Stroke::new(Paint::Solid([0.1, 0.1, 0.1, 1.0]), 4.0 + next() * 20.0)
            });
        }
        shapes.push(shape);
    }
    shapes
}

/// GPU time of 50 mixed shapes into a 4K layer, timed with timestamps over 60 frames
/// after a warm-up (the node rebuilt every frame, as the engine will). Prints the median
/// and p99 GPU time, the CPU time to build the node and the fragments each kind of pass
/// shades, and holds the GPU time to the whole Phase 4 budget by the shared timing rules
/// (`common::timing`).
#[test]
fn fifty_shapes_4k_gpu_time() {
    let _serial = common::timing::serial();
    let Some(mut gpu) = Gpu::new() else { return };
    let mut profiler = GpuProfiler::new(&gpu.device, &gpu.queue, 4);
    if !profiler.enabled() {
        eprintln!("skipped: no timestamp queries");
        return;
    }
    let canvas = (3840, 2160);
    let shapes = mixed_shapes();
    let out = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, canvas);
    let view = out.create_view(&Default::default());
    let mut times = Vec::new();
    let mut build = Vec::new();
    let mut stats = (0, 0, 0);
    let mut fragments = (0, 0);
    for frame in 0..70 {
        let started = std::time::Instant::now();
        let node = gpu.node(canvas, &shapes);
        build.push(started.elapsed().as_secs_f64() * 1e3);
        stats = (node.outline_count(), node.edge_count(), node.batch_count());
        fragments = (node.accumulate_fragments(), node.paint_fragments());
        let mut g = Graph::new();
        let id = g.add(node, &[]);
        g.export(id.out(0), &view);
        g.compile()
            .expect("compiles")
            .execute(&gpu.renderer, &mut gpu.pool, Some(&mut profiler));
        let timings = profiler.wait(&gpu.device).expect("timed");
        if frame >= 10 {
            times.push(timings.total_ms.expect("total"));
        }
    }
    let times = common::timing::Samples::new(times);
    let build = common::timing::Samples::new(build);
    println!(
        "50 shapes at 4K ({} outlines, {} edges, {} atlas fills): GPU {times}; CPU build median \
         {:.3} ms",
        stats.0,
        stats.1,
        stats.2,
        build.median()
    );
    println!(
        "fragments: accumulate {:.1} M, paint {:.1} M",
        fragments.0 as f64 / 1e6,
        fragments.1 as f64 / 1e6
    );
    let pixels = common::read(&gpu.device, &gpu.queue, &out);
    assert!(pixels.iter().filter(|p| p[3] > 0.0).count() > 3840 * 2160 / 10);
    common::timing::assert_gpu_within("50 shapes at 4K", &times, 8.0);
}
