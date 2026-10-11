//! The dissolve family beyond the cross dissolve (`assets/transitions/`): dip to black,
//! white and colour, film, additive and non-additive. Each is exactly A at progress 0 and
//! exactly B at 1 (linear and eased), and matches an f64 reference of its formula in
//! between; plus the GPU time of each at 4K. GPU checks skip without an adapter.

mod common;

use std::sync::Arc;

use render::graph::{Graph, TexturePool};
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::shaders::ShaderLibrary;
use render::transition::{Easing, ParamValue, Transition, TransitionNode, TransitionRegistry};
use render::wgpu;

const IDS: [&str; 6] = [
    "dip-to-black",
    "dip-to-white",
    "dip-to-colour",
    "film-dissolve",
    "additive-dissolve",
    "non-additive-dissolve",
];

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

/// One transition pass over `a` and `b` (premultiplied pixels of `size`), read back.
fn run(
    gpu: &Gpu,
    size: (u32, u32),
    a: &[[f32; 4]],
    b: &[[f32; 4]],
    node: TransitionNode,
) -> Vec<[f32; 4]> {
    let ta = common::rgba16f(&gpu.device, &gpu.queue, size, a);
    let tb = common::rgba16f(&gpu.device, &gpu.queue, size, b);
    let (va, vb) = (
        ta.create_view(&Default::default()),
        tb.create_view(&Default::default()),
    );
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, size);
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

type Px = [f64; 4];

fn px(p: [f32; 4]) -> Px {
    p.map(f64::from)
}

fn mix(x: Px, wx: f64, y: Px, wy: f64) -> Px {
    std::array::from_fn(|c| x[c] * wx + y[c] * wy)
}

/// The dip shaders' formula: A to `c` until the midpoint `m`, then `c` to B.
fn dip(a: Px, b: Px, c: Px, m: f64, t: f64) -> Px {
    if t <= m {
        let s = (t / m).clamp(0.0, 1.0);
        mix(a, 1.0 - s, c, s)
    } else {
        let s = ((t - m) / (1.0 - m)).clamp(0.0, 1.0);
        mix(c, 1.0 - s, b, s)
    }
}

/// The film dissolve's formula: straight channels through the odd 1/2.4 power, mixed
/// premultiplied, decoded with the 2.4 power.
fn film(a: Px, b: Px, t: f64) -> Px {
    let power = |x: f64, y: f64| x.signum() * x.abs().powf(y);
    let code = |p: Px, y: f64| -> Px {
        let alpha = if p[3] <= 0.0 { 1.0 } else { p[3] };
        let mut out = p;
        for c in 0..3 {
            out[c] = power(p[c] / alpha, y) * alpha;
        }
        out
    };
    code(mix(code(a, 1.0 / 2.4), 1.0 - t, code(b, 1.0 / 2.4), t), 2.4)
}

fn weights(t: f64) -> (f64, f64) {
    ((2.0 * (1.0 - t)).clamp(0.0, 1.0), (2.0 * t).clamp(0.0, 1.0))
}

/// The additive dissolve's formula: the ramped sum, capped per channel at the larger of
/// the alpha and the brighter weighted input.
fn additive(a: Px, b: Px, t: f64) -> Px {
    let (wa, wb) = weights(t);
    let alpha = (a[3] * wa + b[3] * wb).min(1.0);
    std::array::from_fn(|c| {
        if c == 3 {
            alpha
        } else {
            (a[c] * wa + b[c] * wb).min(alpha.max(a[c] * wa).max(b[c] * wb))
        }
    })
}

/// The non-additive dissolve's formula: the per-channel maximum of the ramped inputs.
fn non_additive(a: Px, b: Px, t: f64) -> Px {
    let (wa, wb) = weights(t);
    std::array::from_fn(|c| (a[c] * wa).max(b[c] * wb))
}

/// Compares the GPU output with `reference` per pixel; returns the largest error.
fn check(
    what: &str,
    out: &[[f32; 4]],
    a: &[[f32; 4]],
    b: &[[f32; 4]],
    relative: f64,
    reference: impl Fn(Px, Px) -> Px,
) -> f64 {
    let mut worst = 0.0f64;
    for (i, got) in out.iter().enumerate() {
        let want = reference(px(a[i]), px(b[i]));
        for c in 0..4 {
            let error = (f64::from(got[c]) - want[c]).abs();
            worst = worst.max(error);
            let tol = want[c].abs() * relative + 1e-4;
            assert!(
                error <= tol,
                "{what}: pixel {i} channel {c}: {} vs {}",
                got[c],
                want[c]
            );
        }
    }
    worst
}

#[test]
fn the_dissolves_are_bundled_with_names() {
    let registry = TransitionRegistry::bundled();
    assert!(registry.problems().is_empty(), "{:#?}", registry.problems());
    let english = assets::i18n::Locale::english();
    for id in IDS {
        let t = registry.get(id).unwrap_or_else(|| panic!("no {id}"));
        assert_eq!(t.category(), "dissolve");
        assert!(t.directions().is_empty());
        assert!(english.get(t.name()).is_some(), "{id}: no {}", t.name());
    }
    let ids = |id: &str| -> Vec<String> {
        registry
            .get(id)
            .expect("bundled")
            .params()
            .iter()
            .map(|p| p.id.clone())
            .collect()
    };
    assert_eq!(ids("dip-to-black"), ["midpoint"]);
    assert_eq!(ids("dip-to-white"), ["midpoint"]);
    assert_eq!(ids("dip-to-colour"), ["colour", "midpoint"]);
    assert!(ids("film-dissolve").is_empty());
}

#[test]
fn every_dissolve_is_exactly_a_then_exactly_b() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let size = (64, 32);
    let (a, b) = (noise(64 * 32, 1), noise(64 * 32, 2));
    for id in IDS {
        for easing in [Easing::Linear, Easing::EaseInOut] {
            let at = |t| {
                run(
                    &gpu,
                    size,
                    &a,
                    &b,
                    TransitionNode::new(transition(id), t, easing),
                )
            };
            assert_eq!(at(0.0), a, "{id}: progress 0 is A");
            assert_eq!(at(1.0), b, "{id}: progress 1 is B");
        }
    }
    // A set colour and midpoint keep the endpoints too.
    let node = |t| {
        TransitionNode::new(transition("dip-to-colour"), t, Easing::Linear)
            .value("colour", &ParamValue::Colour([0.2, 3.0, -0.1, 0.5]))
            .value("midpoint", &ParamValue::Number(0.8))
    };
    assert_eq!(run(&gpu, size, &a, &b, node(0.0)), a);
    assert_eq!(run(&gpu, size, &a, &b, node(1.0)), b);
}

#[test]
fn every_dissolve_matches_its_reference_mid_way() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let size = (64, 32);
    let (a, b) = (noise(64 * 32, 3), noise(64 * 32, 4));
    let at = |id: &str, t: f64| {
        run(
            &gpu,
            size,
            &a,
            &b,
            TransitionNode::new(transition(id), t, Easing::Linear),
        )
    };
    let tight = 2f64.powi(-10);
    let black = [0.0, 0.0, 0.0, 1.0];
    let white = [1.0; 4];
    let mut report = Vec::new();

    let t = 0.25;
    let w = check(
        "dip to black",
        &at("dip-to-black", t),
        &a,
        &b,
        tight,
        |a, b| dip(a, b, black, 0.5, t),
    );
    report.push(("dip to black 0.25", w));
    let t = 0.75;
    let w = check(
        "dip to white",
        &at("dip-to-white", t),
        &a,
        &b,
        tight,
        |a, b| dip(a, b, white, 0.5, t),
    );
    report.push(("dip to white 0.75", w));

    // The default colour at half way is the colour alone.
    let red = [0.8f32, 0.05, 0.03, 1.0].map(f64::from);
    let w = check(
        "dip to colour",
        &at("dip-to-colour", 0.5),
        &a,
        &b,
        tight,
        |_, _| red,
    );
    report.push(("dip to colour 0.5", w));
    // A set colour (premultiplied by the shader) and an early midpoint, past it.
    let (colour, m, t) = ([0.2f32, 3.0, -0.1, 0.5], 0.3f32, 0.6);
    let premultiplied = [
        colour[0] * colour[3],
        colour[1] * colour[3],
        colour[2] * colour[3],
        colour[3],
    ]
    .map(f64::from);
    let set = run(
        &gpu,
        size,
        &a,
        &b,
        TransitionNode::new(transition("dip-to-colour"), t, Easing::Linear)
            .value("colour", &ParamValue::Colour(colour))
            .value("midpoint", &ParamValue::Number(f64::from(m))),
    );
    let w = check("dip to colour, set", &set, &a, &b, tight, |a, b| {
        dip(a, b, premultiplied, f64::from(m), t)
    });
    report.push(("dip to colour 0.6, midpoint 0.3", w));

    let t = 0.4;
    let w = check("film", &at("film-dissolve", t), &a, &b, 2e-3, |a, b| {
        film(a, b, t)
    });
    report.push(("film 0.4", w));
    for t in [0.25, 0.5] {
        let w = check(
            "additive",
            &at("additive-dissolve", t),
            &a,
            &b,
            tight,
            |a, b| additive(a, b, t),
        );
        report.push(("additive", w));
    }
    let t = 0.3;
    let w = check(
        "non-additive",
        &at("non-additive-dissolve", t),
        &a,
        &b,
        tight,
        |a, b| non_additive(a, b, t),
    );
    report.push(("non-additive 0.3", w));
    for (what, worst) in report {
        println!("{what}: max error {worst:.6}");
    }
}

#[test]
fn the_film_and_additive_dissolves_differ_from_the_cross_dissolve_as_intended() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // Black to white, opaque.
    let (a, b) = ([[0.0, 0.0, 0.0, 1.0]], [[1.0, 1.0, 1.0, 1.0]]);
    let mid = |id: &str| {
        run(
            &gpu,
            (1, 1),
            &a,
            &b,
            TransitionNode::new(transition(id), 0.5, Easing::Linear),
        )[0]
    };
    assert_eq!(mid("cross-dissolve")[0], 0.5);
    let film = mid("film-dissolve")[0];
    assert!((film - 0.5f32.powf(2.4)).abs() < 1e-3, "film: {film}");
    // Mid grey to mid grey: the additive dissolve doubles it half way, capped at white.
    let grey = [[0.4, 0.4, 0.4, 1.0]];
    let additive = run(
        &gpu,
        (1, 1),
        &grey,
        &grey,
        TransitionNode::new(transition("additive-dissolve"), 0.5, Easing::Linear),
    )[0];
    assert_eq!(additive, [f16(0.8), f16(0.8), f16(0.8), 1.0]);
    let hot = [[0.9, 3.0, 0.1, 1.0]];
    let additive = run(
        &gpu,
        (1, 1),
        &hot,
        &grey,
        TransitionNode::new(transition("additive-dissolve"), 0.5, Easing::Linear),
    )[0];
    assert_eq!(
        additive,
        [1.0, 3.0, f16(f16(0.1) + 0.4), 1.0],
        "HDR kept, not pushed"
    );
}

/// GPU time of one 4K pass of each dissolve over noise, median of 30 frames from the pass
/// timestamps; the slowest is checked against the budget.
#[test]
fn dissolves_at_4k_gpu_time() {
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
    for id in IDS {
        let mut times = Vec::new();
        for frame in 0..32u32 {
            let mut g = Graph::new();
            let (ia, ib) = (g.import("a", &va), g.import("b", &vb));
            let out = g.add(
                TransitionNode::new(transition(id), f64::from(frame) / 31.0, Easing::EaseInOut),
                &[ia, ib],
            );
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
        if median > slowest.0 {
            slowest = (median, id);
        }
    }
    println!("slowest: {} at {:.3} ms", slowest.1, slowest.0);
    // A quarter of the 8 ms compositor budget, loose for a shared GPU.
    assert!(slowest.0 < 2.0, "{} {} ms", slowest.1, slowest.0);
}
