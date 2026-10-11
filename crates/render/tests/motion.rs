//! The motion transitions (`assets/transitions/`): push, slide, whip pan, zoom, spin,
//! swirl and roll. Each is exactly A at progress 0 and exactly B at 1 (linear and eased,
//! whatever the direction and parameters); push and slide match f64 references of their
//! formulas mid way; the whip pan's blur keeps a flat picture's level and is symmetric
//! and energy-preserving on a line; a few placements are checked by sampling; plus the
//! GPU time of each at 4K. GPU checks skip without an adapter.

mod common;

use std::sync::Arc;

use render::graph::{Graph, TexturePool};
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::shaders::ShaderLibrary;
use render::transition::{
    Direction, Easing, ParamValue, Transition, TransitionNode, TransitionRegistry,
};
use render::wgpu;

const IDS: [&str; 7] = ["push", "slide", "whip-pan", "zoom", "spin", "swirl", "roll"];

/// The four directions the sliding transitions offer.
const SIDES: [Direction; 4] = [
    Direction::Left,
    Direction::Right,
    Direction::Up,
    Direction::Down,
];

/// The test pictures' size: small and not square.
const SIZE: (u32, u32) = (64, 36);

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
}

fn gpu() -> Option<Gpu> {
    let (device, queue) = common::device()?;
    let library = ShaderLibrary::embedded_only();
    TransitionRegistry::bundled().add_shaders(&library);
    let renderer = Renderer::with_shaders(&device, &queue, library);
    Some(Gpu {
        device,
        queue,
        renderer,
    })
}

fn transition(id: &str) -> Arc<Transition> {
    TransitionRegistry::bundled()
        .get(id)
        .unwrap_or_else(|| panic!("no bundled {id}"))
        .clone()
}

/// One transition pass over `a` and `b` (premultiplied pixels of `SIZE`), read back.
fn run(gpu: &Gpu, a: &[[f32; 4]], b: &[[f32; 4]], node: TransitionNode) -> Vec<[f32; 4]> {
    let ta = common::rgba16f(&gpu.device, &gpu.queue, SIZE, a);
    let tb = common::rgba16f(&gpu.device, &gpu.queue, SIZE, b);
    let (va, vb) = (
        ta.create_view(&Default::default()),
        tb.create_view(&Default::default()),
    );
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, SIZE);
    let target_view = target.create_view(&Default::default());
    let mut g = Graph::new();
    let (ia, ib) = (g.import("a", &va), g.import("b", &vb));
    let out = g.add(node, &[ia, ib]);
    g.export(out.out(0), &target_view);
    g.compile()
        .expect("compiles")
        .execute(&gpu.renderer, &mut TexturePool::new(), None);
    common::read(&gpu.device, &gpu.queue, &target)
}

fn f16(v: f32) -> f32 {
    common::f16_to_f32(common::f32_to_f16(v))
}

/// Premultiplied pixels with red up to twice the alpha (HDR), green a little below zero
/// at times (wide gamut) and alpha from 0.25 to 1.
fn noise(n: usize, seed: u32) -> Vec<[f32; 4]> {
    let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state >> 8) as f32 / (1 << 24) as f32
    };
    (0..n)
        .map(|_| {
            let a = 0.25 + 0.75 * next();
            [next() * 2.0 * a, (next() - 0.1) * a, next() * a, a].map(f16)
        })
        .collect()
}

/// A transition with a direction and parameters, as a function of linear progress and
/// easing.
struct Variant {
    what: String,
    node: Box<dyn Fn(f64, Easing) -> TransitionNode>,
}

fn variant(
    id: &'static str,
    what: String,
    setup: impl Fn(TransitionNode) -> TransitionNode + 'static,
) -> Variant {
    Variant {
        what: format!("{id} {what}"),
        node: Box::new(move |t, easing| setup(TransitionNode::new(transition(id), t, easing))),
    }
}

fn number(v: f64) -> ParamValue {
    ParamValue::Number(v)
}

fn choice(id: &str) -> ParamValue {
    ParamValue::Choice(id.to_owned())
}

/// Every offered direction and choice, at the defaults and at unusual parameter values.
fn variants() -> Vec<Variant> {
    let mut v = Vec::new();
    for d in SIDES {
        v.push(variant("push", d.id().to_owned(), move |n| n.direction(d)));
        for mode in ["in", "out"] {
            v.push(variant("slide", format!("{} {mode}", d.id()), move |n| {
                n.direction(d).value("mode", &choice(mode))
            }));
        }
        for (blur, samples) in [(0.25, 32.0), (0.0, 32.0), (1.0, 4.0), (0.6, 128.0)] {
            v.push(variant(
                "whip-pan",
                format!("{} blur {blur} samples {samples}", d.id()),
                move |n| {
                    n.direction(d)
                        .value("blur", &number(blur))
                        .value("samples", &number(samples))
                },
            ));
        }
        v.push(variant("roll", d.id().to_owned(), move |n| n.direction(d)));
    }
    for mode in ["in", "out"] {
        for (zoom, blur) in [(3.0, 0.5), (1.0, 0.5), (12.0, 0.0), (2.0, 1.0)] {
            v.push(variant(
                "zoom",
                format!("{mode} zoom {zoom} blur {blur}"),
                move |n| {
                    n.value("mode", &choice(mode))
                        .value("zoom", &number(zoom))
                        .value("blur", &number(blur))
                },
            ));
        }
    }
    for rotation in ["clockwise", "anticlockwise"] {
        for (angle, zoom) in [(180.0, 1.25), (0.0, 1.0), (720.0, 3.0), (45.0, 1.0)] {
            v.push(variant(
                "spin",
                format!("{rotation} {angle} zoom {zoom}"),
                move |n| {
                    n.value("rotation", &choice(rotation))
                        .value("angle", &number(angle))
                        .value("zoom", &number(zoom))
                },
            ));
        }
        for (angle, radius) in [(540.0, 1.0), (90.0, 0.2), (1800.0, 2.5)] {
            v.push(variant(
                "swirl",
                format!("{rotation} {angle} radius {radius}"),
                move |n| {
                    n.value("rotation", &choice(rotation))
                        .value("angle", &number(angle))
                        .value("radius", &number(radius))
                },
            ));
        }
    }
    v
}

#[test]
fn the_motion_transitions_are_bundled_with_names() {
    let registry = TransitionRegistry::bundled();
    assert!(registry.problems().is_empty(), "{:#?}", registry.problems());
    let english = assets::i18n::Locale::english();
    assert!(english.get("transition.category.motion").is_some());
    for id in IDS {
        let t = registry.get(id).unwrap_or_else(|| panic!("no {id}"));
        assert_eq!(t.category(), "motion");
        assert!(english.get(t.name()).is_some(), "{id}: no {}", t.name());
        // Every parameter label and choice label is translated.
        for json in t.schema_params() {
            let label = json["label"].as_str().expect("a label");
            assert!(english.get(label).is_some(), "{id}: no {label}");
            if let Some(options) = json["kind"]["choice"]["options"].as_array() {
                for option in options {
                    let label = option["label"].as_str().expect("a label");
                    assert!(english.get(label).is_some(), "{id}: no {label}");
                }
            }
        }
    }
    let directions = |id: &str| registry.get(id).expect("bundled").directions().to_vec();
    for id in ["push", "slide", "whip-pan"] {
        assert_eq!(directions(id), SIDES, "{id}");
    }
    assert_eq!(
        directions("roll"),
        [
            Direction::Right,
            Direction::Left,
            Direction::Down,
            Direction::Up
        ]
    );
    for id in ["zoom", "spin", "swirl"] {
        assert!(directions(id).is_empty(), "{id}");
    }
}

#[test]
fn every_motion_transition_is_exactly_a_then_exactly_b() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let n = (SIZE.0 * SIZE.1) as usize;
    let (a, b) = (noise(n, 1), noise(n, 2));
    for v in variants() {
        for easing in [Easing::Linear, Easing::EaseInOut] {
            let at = |t| run(&gpu, &a, &b, (v.node)(t, easing));
            assert_eq!(at(0.0), a, "{}: progress 0 is A", v.what);
            assert_eq!(at(1.0), b, "{}: progress 1 is B", v.what);
        }
        // Mid way it is neither, and every value is finite.
        let mid = run(&gpu, &a, &b, (v.node)(0.5, Easing::Linear));
        assert!(mid != a && mid != b, "{}: A or B at 0.5", v.what);
        assert!(
            mid.iter().flatten().all(|c| c.is_finite()),
            "{}: not finite at 0.5",
            v.what
        );
    }
}

type Px = [f64; 4];

/// `pixels` (of `SIZE`) filtered bilinearly at `uv` with the edge clamped, as the
/// transitions' sampler does.
fn bilinear(pixels: &[[f32; 4]], uv: [f64; 2]) -> Px {
    let (w, h) = (i64::from(SIZE.0), i64::from(SIZE.1));
    let x = uv[0] * w as f64 - 0.5;
    let y = uv[1] * h as f64 - 0.5;
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let texel = |i: i64, j: i64| {
        let p = pixels[(j.clamp(0, h - 1) * w + i.clamp(0, w - 1)) as usize];
        p.map(f64::from)
    };
    let (i, j) = (x0 as i64, y0 as i64);
    let (p00, p10, p01, p11) = (
        texel(i, j),
        texel(i + 1, j),
        texel(i, j + 1),
        texel(i + 1, j + 1),
    );
    std::array::from_fn(|c| {
        let top = p00[c] + (p10[c] - p00[c]) * fx;
        let bottom = p01[c] + (p11[c] - p01[c]) * fx;
        top + (bottom - top) * fy
    })
}

/// The shaders' moved frame: `pixels` with their point `q` (in pixels) under the output
/// pixel, weighted by the frame's coverage (1 px box at its edges, 0 outside).
fn frame(pixels: &[[f32; 4]], q: [f64; 2]) -> Px {
    let size = [f64::from(SIZE.0), f64::from(SIZE.1)];
    let cover = |k: usize| (q[k].min(size[k] - q[k]) + 0.5).clamp(0.0, 1.0);
    let c = cover(0) * cover(1);
    if c <= 0.0 {
        return [0.0; 4];
    }
    bilinear(pixels, [q[0] / size[0], q[1] / size[1]]).map(|v| v * c)
}

fn over(top: Px, bottom: Px) -> Px {
    std::array::from_fn(|c| top[c] + bottom[c] * (1.0 - top[3]))
}

/// Compares the GPU output with `reference` (from the pixel's centre) per pixel within
/// `tol` of each value's size; returns the largest error.
fn check(what: &str, out: &[[f32; 4]], tol: f64, reference: impl Fn([f64; 2]) -> Px) -> f64 {
    let mut worst = 0.0f64;
    for (i, got) in out.iter().enumerate() {
        let centre = [
            f64::from(i as u32 % SIZE.0) + 0.5,
            f64::from(i as u32 / SIZE.0) + 0.5,
        ];
        let want = reference(centre);
        for c in 0..4 {
            let error = (f64::from(got[c]) - want[c]).abs();
            worst = worst.max(error);
            // An f16 step, plus the filter's own precision.
            let limit = want[c].abs() * 2f64.powi(-10) + tol;
            assert!(
                error <= limit,
                "{what}: pixel {centre:?} channel {c}: {} vs {}",
                got[c],
                want[c]
            );
        }
    }
    worst
}

#[test]
fn push_and_slide_match_their_references_mid_way() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let n = (SIZE.0 * SIZE.1) as usize;
    let (a, b) = (noise(n, 3), noise(n, 4));
    let size = [f64::from(SIZE.0), f64::from(SIZE.1)];
    let mut report = Vec::new();
    // Half way the frames move by whole pixels; at 0.37 they fall between texels, and
    // the GPU's filter weights are only so precise.
    for (t, tol) in [(0.5f32, 2e-4), (0.37, 4e-3)] {
        let t = f64::from(t);
        for d in SIDES {
            let dir = d.vector().map(f64::from);
            let span = [dir[0] * size[0], dir[1] * size[1]];
            let node = TransitionNode::new(transition("push"), t, Easing::Linear).direction(d);
            let out = run(&gpu, &a, &b, node);
            let what = format!("push {} at {t}", d.id());
            let worst = check(&what, &out, tol, |p| {
                let q = [p[0] - t * span[0], p[1] - t * span[1]];
                let fa = frame(&a, q);
                let fb = frame(&b, [q[0] + span[0], q[1] + span[1]]);
                std::array::from_fn(|c| fa[c] + fb[c])
            });
            report.push((what, worst));
            for mode in ["in", "out"] {
                let node = TransitionNode::new(transition("slide"), t, Easing::Linear)
                    .direction(d)
                    .value("mode", &choice(mode));
                let out = run(&gpu, &a, &b, node);
                let what = format!("slide {} {mode} at {t}", d.id());
                let worst = check(&what, &out, tol, |p| {
                    let i = ((p[1] - 0.5) * size[0] + p[0] - 0.5) as usize;
                    let still = |pixels: &[[f32; 4]]| pixels[i].map(f64::from);
                    if mode == "out" {
                        let q = [p[0] - t * span[0], p[1] - t * span[1]];
                        over(frame(&a, q), still(&b))
                    } else {
                        let q = [p[0] + (1.0 - t) * span[0], p[1] + (1.0 - t) * span[1]];
                        over(frame(&b, q), still(&a))
                    }
                });
                report.push((what, worst));
            }
        }
    }
    for (what, worst) in report {
        println!("{what}: max error {worst:.6}");
    }
}

/// Rec. 709 luminance of a premultiplied pixel.
fn luminance(p: [f32; 4]) -> f64 {
    0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
}

#[test]
fn the_whip_pan_blur_keeps_levels_and_is_symmetric() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let n = (SIZE.0 * SIZE.1) as usize;
    let whip = |t: f64, d: Direction, blur: f64| {
        TransitionNode::new(transition("whip-pan"), t, Easing::Linear)
            .direction(d)
            .value("blur", &number(blur))
    };
    // Enough taps for one a pixel: none staggered.
    let dense = |t: f64, d: Direction, blur: f64| whip(t, d, blur).value("samples", &number(128.0));

    // A flat picture (the same on both sides) keeps its level at every pixel, so its
    // mean luminance is unchanged: the blur never averages in anything transparent,
    // with taps staggered or not.
    let flat = vec![[0.6f32, 0.3, 0.15, 1.0].map(f16); n];
    let level = luminance(flat[0]);
    for d in SIDES {
        for (blur, samples) in [(0.25, 8.0), (1.0, 3.0), (0.25, 128.0)] {
            for step in 1..16 {
                let t = f64::from(step) / 16.0;
                let node = whip(t, d, blur).value("samples", &number(samples));
                let out = run(&gpu, &flat, &flat, node);
                let mean = out.iter().map(|p| luminance(*p)).sum::<f64>() / n as f64;
                assert!(
                    (mean - level).abs() < 1e-3,
                    "{} blur {blur} x {samples} at {t}: mean {mean} vs {level}",
                    d.id()
                );
                for p in &out {
                    for c in 0..4 {
                        assert!(
                            (p[c] - flat[0][c]).abs() <= 1e-3,
                            "{} blur {blur} x {samples} at {t}: {p:?}",
                            d.id()
                        );
                    }
                }
            }
        }
    }

    // A one-pixel white column on black (the same on both sides, so the strip repeats
    // every frame width) moves half a frame by the middle and spreads into a box
    // symmetric about its new place, with its energy kept.
    let mut line = vec![[0.0, 0.0, 0.0, 1.0]; n];
    for y in 0..SIZE.1 {
        line[(y * SIZE.0 + 16) as usize] = [1.0; 4];
    }
    for blur in [0.25, 0.3] {
        let out = run(&gpu, &line, &line, dense(0.5, Direction::Right, blur));
        let row: Vec<f64> = (0..SIZE.0)
            .map(|x| f64::from(out[(18 * SIZE.0 + x) as usize][0]))
            .collect();
        let energy: f64 = row.iter().sum();
        assert!((energy - 1.0).abs() < 0.01, "blur {blur}: energy {energy}");
        let spread = row.iter().filter(|v| **v > 1e-3).count();
        let expected = blur * f64::from(SIZE.0);
        assert!(
            (spread as f64 - expected).abs() <= 2.0,
            "blur {blur}: {spread} columns lit, about {expected} expected: {row:?}"
        );
        for k in 1..16 {
            let (left, right) = (row[48 - k], row[48 + k]);
            assert!(
                (left - right).abs() < 2e-3,
                "blur {blur}: not symmetric at {k}: {left} vs {right}"
            );
        }
        println!("blur {blur}: {spread} columns, energy {energy:.5}");
    }
}

/// B's share (red) of a black-to-white transition at `t`, both opaque.
fn coverage(gpu: &Gpu, node: TransitionNode) -> impl Fn(u32, u32) -> f32 {
    let n = (SIZE.0 * SIZE.1) as usize;
    let a = vec![[0.0, 0.0, 0.0, 1.0]; n];
    let b = vec![[1.0; 4]; n];
    let out = run(gpu, &a, &b, node);
    move |x: u32, y: u32| out[(y * SIZE.0 + x) as usize][0]
}

/// Alpha of a black-to-white transition at `t`, both opaque.
fn alpha(gpu: &Gpu, node: TransitionNode) -> impl Fn(u32, u32) -> f32 {
    let n = (SIZE.0 * SIZE.1) as usize;
    let a = vec![[0.0, 0.0, 0.0, 1.0]; n];
    let b = vec![[1.0; 4]; n];
    let out = run(gpu, &a, &b, node);
    move |x: u32, y: u32| out[(y * SIZE.0 + x) as usize][3]
}

#[test]
fn the_motion_transitions_move_where_intended() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let new = |id: &str, t: f64| TransitionNode::new(transition(id), t, Easing::Linear);

    // Push left a quarter of the way: B in the right quarter, everything opaque.
    let c = coverage(&gpu, new("push", 0.25));
    assert_eq!((c(50, 18), c(46, 18), c(10, 18)), (1.0, 0.0, 0.0));
    let o = alpha(&gpu, new("push", 0.25));
    assert!((0..SIZE.0).all(|x| o(x, 0) == 1.0 && o(x, 35) == 1.0));
    // Push down: B from the top.
    let c = coverage(&gpu, new("push", 0.25).direction(Direction::Down));
    assert_eq!((c(32, 4), c(32, 12)), (1.0, 0.0));

    // Slide in from the right; slide out to the left uncovers B on the right.
    let c = coverage(&gpu, new("slide", 0.25));
    assert_eq!((c(50, 18), c(40, 18)), (1.0, 0.0));
    let c = coverage(&gpu, new("slide", 0.25).value("mode", &choice("out")));
    assert_eq!((c(50, 18), c(40, 18)), (1.0, 0.0));

    // Whip pan right: moved under 4 px at a fifth, half way the seam in the middle.
    let c = coverage(&gpu, new("whip-pan", 0.2).direction(Direction::Right));
    assert_eq!((c(0, 18), c(12, 18), c(60, 18)), (1.0, 0.0, 0.0));
    let c = coverage(
        &gpu,
        new("whip-pan", 0.5)
            .direction(Direction::Right)
            .value("samples", &number(128.0)),
    );
    assert!(c(5, 18) > 0.9 && c(58, 18) < 0.1);
    assert!((c(31, 18) - 0.5).abs() < 0.1 && (c(32, 18) - 0.5).abs() < 0.1);

    // Zoom in: B small in the middle, A around it; nothing transparent.
    let c = coverage(&gpu, new("zoom", 0.5).value("blur", &number(0.0)));
    assert!(c(32, 18) > 0.4 && c(2, 2) == 0.0);
    let o = alpha(&gpu, new("zoom", 0.5));
    assert!((0..SIZE.0).all(|x| (0..SIZE.1).all(|y| (o(x, y) - 1.0).abs() < 1e-3)));
    // Zoom out: A small in the middle fading, B around it.
    let c = coverage(&gpu, new("zoom", 0.5).value("mode", &choice("out")));
    assert!(c(32, 18) < 0.6 && c(2, 2) == 1.0);

    // Spin and swirl cover the frame all the way.
    for id in ["spin", "swirl"] {
        for step in 1..8 {
            let o = alpha(&gpu, new(id, f64::from(step) / 8.0));
            assert!(
                (0..SIZE.0).all(|x| (0..SIZE.1).all(|y| (o(x, y) - 1.0).abs() < 1e-3)),
                "{id} at {step} / 8"
            );
        }
    }

    // Roll right half way: A turned a quarter clockwise about the bottom centre lies to
    // its right, B to its left, above the bottom edge.
    let c = coverage(&gpu, new("roll", 0.5));
    let o = alpha(&gpu, new("roll", 0.5));
    assert_eq!((c(36, 30), c(27, 30)), (0.0, 1.0));
    assert_eq!((o(36, 30), o(27, 30)), (1.0, 1.0));
    // A frame 64 wide reaches 32 above the bottom when turned: the top rows are empty.
    assert_eq!((o(32, 1), o(10, 1)), (0.0, 0.0));
}

/// GPU time of one 4K pass of each motion transition over noise with its defaults,
/// median of 30 frames from the pass timestamps. The slowest unblurred one is checked
/// against the 3 ms budget. The blurred ones (whip pan, zoom) read A or B once per tap,
/// and at 4K every tap of a long blur costs about a full read of the frame, so they are
/// only held to the 8 ms compositor frame budget; their cost by tap count is printed.
#[test]
fn motion_transitions_at_4k_gpu_time() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut profiler = GpuProfiler::new(&gpu.device, &gpu.queue, 4);
    if !profiler.enabled() {
        eprintln!("skipped: no timestamp queries");
        return;
    }
    let a = common::noise_4k(&gpu.device, &gpu.queue, 1);
    let b = common::noise_4k(&gpu.device, &gpu.queue, 2);
    let (va, vb) = (
        a.create_view(&Default::default()),
        b.create_view(&Default::default()),
    );
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, (3840, 2160));
    let target_view = target.create_view(&Default::default());
    let mut pool = TexturePool::new();
    let mut slowest = (0.0f64, "");
    let mut blurred = (0.0f64, "");
    for id in IDS {
        let mut times = Vec::new();
        for frame in 0..32u32 {
            let mut g = Graph::new();
            let (ia, ib) = (g.import("a", &va), g.import("b", &vb));
            let node =
                TransitionNode::new(transition(id), f64::from(frame) / 31.0, Easing::EaseInOut);
            let out = g.add(node, &[ia, ib]);
            g.export(out.out(0), &target_view);
            g.compile()
                .expect("compiles")
                .execute(&gpu.renderer, &mut pool, Some(&mut profiler));
            let timings = profiler.wait(&gpu.device).expect("timed");
            // The first two frames build the pipeline and warm the caches.
            if frame >= 2 {
                times.extend(timings.passes[0].gpu_ms);
            }
        }
        times.sort_by(f64::total_cmp);
        let median = times[times.len() / 2];
        let max = times[times.len() - 1];
        println!("4K {id}: median {median:.3} ms, max {max:.3} ms GPU");
        let worst = if ["whip-pan", "zoom"].contains(&id) {
            &mut blurred
        } else {
            &mut slowest
        };
        if median > worst.0 {
            *worst = (median, id);
        }
    }
    println!("slowest: {} at {:.3} ms", slowest.1, slowest.0);
    println!("slowest blurred: {} at {:.3} ms", blurred.1, blurred.0);
    // The blurred ones by tap count, at their fastest (half way), for the record.
    for (id, counts) in [
        ("whip-pan", [4.0, 8.0, 16.0, 32.0]),
        ("zoom", [3.0, 6.0, 12.0, 24.0]),
    ] {
        for samples in counts {
            let mut times = Vec::new();
            for frame in 0..12u32 {
                let mut g = Graph::new();
                let (ia, ib) = (g.import("a", &va), g.import("b", &vb));
                let node = TransitionNode::new(transition(id), 0.5, Easing::Linear)
                    .value("samples", &number(samples));
                let out = g.add(node, &[ia, ib]);
                g.export(out.out(0), &target_view);
                g.compile().expect("compiles").execute(
                    &gpu.renderer,
                    &mut pool,
                    Some(&mut profiler),
                );
                let timings = profiler.wait(&gpu.device).expect("timed");
                if frame >= 2 {
                    times.extend(timings.passes[0].gpu_ms);
                }
            }
            times.sort_by(f64::total_cmp);
            println!(
                "4K {id} half way, {samples} samples: median {:.3} ms",
                times[times.len() / 2]
            );
        }
    }
    // The motion budget, loose for a shared GPU.
    assert!(slowest.0 < 3.0, "{} {} ms", slowest.1, slowest.0);
    assert!(blurred.0 < 8.0, "{} {} ms", blurred.1, blurred.0);
}
