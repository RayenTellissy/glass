//! The stylised transitions (`assets/transitions/`): glitch, RGB split, pixelate, VHS,
//! light leak, film burn, ink bleed, paper tear and liquid, all procedural (noise worked
//! out in the shader from the seed). Each is exactly A at progress 0 and exactly B at 1
//! (linear and eased, whatever the direction and parameters); the same seed gives the
//! same pixels and another seed others; the mask-based ones (film burn, ink bleed, paper
//! tear) never take back what B has covered; the light leak stays finite and within its
//! cap; pixelate's blocks are flat; plus the GPU time of each at 4K, held to the timing
//! rules (`common::timing`). GPU checks skip without an adapter. Every GPU test holds
//! the timing lock: each makes its own device, and another device's work can run between
//! the passes of a multi-pass transition, inside its timed span.

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

const IDS: [&str; 9] = [
    "glitch",
    "rgb-split",
    "pixelate",
    "vhs",
    "light-leak",
    "film-burn",
    "ink-bleed",
    "paper-tear",
    "liquid",
];

/// The transitions with a seed.
const SEEDED: [&str; 7] = [
    "glitch",
    "vhs",
    "light-leak",
    "film-burn",
    "ink-bleed",
    "paper-tear",
    "liquid",
];

/// The four sides, in the order the light leak offers them.
const SIDES: [Direction; 4] = [
    Direction::Right,
    Direction::Left,
    Direction::Down,
    Direction::Up,
];

/// The eight directions, in the order RGB split and paper tear offer them.
const EIGHT: [Direction; 8] = [
    Direction::Right,
    Direction::Left,
    Direction::Up,
    Direction::Down,
    Direction::UpLeft,
    Direction::UpRight,
    Direction::DownLeft,
    Direction::DownRight,
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

/// One transition over `a` and `b` (premultiplied pixels of `SIZE`), read back.
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

/// Every offered direction, at the defaults and at unusual parameter values and seeds.
fn variants() -> Vec<Variant> {
    let mut v = Vec::new();
    for (seed, amount) in [(1.0, 0.6), (0.0, 1.0), (9999.0, 0.2)] {
        let what = format!("seed {seed} amount {amount}");
        v.push(variant("glitch", what.clone(), move |n| {
            n.value("seed", &number(seed))
                .value("amount", &number(amount))
        }));
        v.push(variant("vhs", what.clone(), move |n| {
            n.value("seed", &number(seed))
                .value("amount", &number(amount))
        }));
        // Liquid's amount is a fraction of the width.
        let push = amount * 0.5;
        v.push(variant(
            "liquid",
            format!("seed {seed} amount {push}"),
            move |n| {
                n.value("seed", &number(seed))
                    .value("amount", &number(push))
            },
        ));
    }
    v.push(variant("liquid", "no push".into(), |n| {
        n.value("amount", &number(0.0))
    }));
    for d in EIGHT {
        for amount in [0.06, 0.5, 0.0] {
            v.push(variant(
                "rgb-split",
                format!("{} amount {amount}", d.id()),
                move |n| n.direction(d).value("amount", &number(amount)),
            ));
        }
        for (seed, roughness) in [(1.0, 0.04), (42.0, 0.2), (3.0, 0.0)] {
            v.push(variant(
                "paper-tear",
                format!("{} seed {seed} roughness {roughness}", d.id()),
                move |n| {
                    n.direction(d)
                        .value("seed", &number(seed))
                        .value("roughness", &number(roughness))
                },
            ));
        }
    }
    for size in [0.05, 0.25, 0.0, 0.01] {
        v.push(variant("pixelate", format!("block {size}"), move |n| {
            n.value("block_size", &number(size))
        }));
    }
    for d in SIDES {
        for (seed, intensity) in [(1.0, 2.0), (5.0, 8.0)] {
            v.push(variant(
                "light-leak",
                format!("{} seed {seed} intensity {intensity}", d.id()),
                move |n| {
                    n.direction(d)
                        .value("seed", &number(seed))
                        .value("intensity", &number(intensity))
                },
            ));
        }
    }
    for (seed, glow) in [(1.0, 2.0), (12.0, 0.0), (300.0, 8.0)] {
        v.push(variant(
            "film-burn",
            format!("seed {seed} glow {glow}"),
            move |n| n.value("seed", &number(seed)).value("glow", &number(glow)),
        ));
    }
    for (seed, softness) in [(1.0, 0.06), (12.0, 0.0), (300.0, 0.5)] {
        v.push(variant(
            "ink-bleed",
            format!("seed {seed} softness {softness}"),
            move |n| {
                n.value("seed", &number(seed))
                    .value("softness", &number(softness))
            },
        ));
    }
    v
}

#[test]
fn the_stylised_transitions_are_bundled_with_names() {
    let registry = TransitionRegistry::bundled();
    assert!(registry.problems().is_empty(), "{:#?}", registry.problems());
    let english = assets::i18n::Locale::english();
    assert!(english.get("transition.category.stylised").is_some());
    for id in IDS {
        let t = registry.get(id).unwrap_or_else(|| panic!("no {id}"));
        assert_eq!(t.category(), "stylised");
        assert!(english.get(t.name()).is_some(), "{id}: no {}", t.name());
        for json in t.schema_params() {
            let label = json["label"].as_str().expect("a label");
            assert!(english.get(label).is_some(), "{id}: no {label}");
        }
        let seeded = t.schema_params().iter().any(|p| p["id"] == "seed");
        assert_eq!(seeded, SEEDED.contains(&id), "{id}: seed");
    }
    let directions = |id: &str| registry.get(id).expect("bundled").directions().to_vec();
    for id in ["rgb-split", "paper-tear"] {
        assert_eq!(directions(id), EIGHT, "{id}");
    }
    assert_eq!(directions("light-leak"), SIDES);
    for id in [
        "glitch",
        "pixelate",
        "vhs",
        "film-burn",
        "ink-bleed",
        "liquid",
    ] {
        assert!(directions(id).is_empty(), "{id}");
    }
}

#[test]
fn every_stylised_transition_is_exactly_a_then_exactly_b() {
    let _serial = common::timing::serial();
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

#[test]
fn the_same_seed_gives_the_same_pixels_and_another_seed_others() {
    let _serial = common::timing::serial();
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let n = (SIZE.0 * SIZE.1) as usize;
    let (a, b) = (noise(n, 3), noise(n, 4));
    // A second device: nothing carries over between runs.
    let other = self::gpu().expect("a second device on the same adapter");
    for id in SEEDED {
        for t in [0.3, 0.5, 0.7] {
            let at = |seed: f64| {
                let node = TransitionNode::new(transition(id), t, Easing::Linear)
                    .value("seed", &number(seed));
                run(&gpu, &a, &b, node)
            };
            let first = at(17.0);
            assert_eq!(first, at(17.0), "{id} at {t}: seed 17 twice");
            let again = run(
                &other,
                &a,
                &b,
                TransitionNode::new(transition(id), t, Easing::Linear).value("seed", &number(17.0)),
            );
            assert_eq!(first, again, "{id} at {t}: seed 17 on another device");
            for seed in [18.0, 0.0, 9999.0] {
                let different = at(seed);
                let changed = first.iter().zip(&different).filter(|(x, y)| x != y).count();
                assert!(
                    changed > n / 20,
                    "{id} at {t}: seed {seed} changes only {changed} of {n} pixels"
                );
            }
        }
    }
}

/// The alpha of a transition from opaque black (A) to transparent (B): A's remaining
/// coverage, since none of the mask-based transitions adds coverage of its own.
fn remaining(gpu: &Gpu, node: TransitionNode) -> Vec<f32> {
    let n = (SIZE.0 * SIZE.1) as usize;
    let a = vec![[0.0, 0.0, 0.0, 1.0]; n];
    let b = vec![[0.0; 4]; n];
    run(gpu, &a, &b, node).iter().map(|p| p[3]).collect()
}

/// A transition as a function of linear progress.
type NodeAt = Box<dyn Fn(f64) -> TransitionNode>;

#[test]
fn the_mask_transitions_never_take_back_what_b_covered() {
    let _serial = common::timing::serial();
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut cases: Vec<(String, NodeAt)> = Vec::new();
    for seed in [1.0, 77.0, 4096.0] {
        cases.push((
            format!("film-burn seed {seed}"),
            Box::new(move |t| {
                TransitionNode::new(transition("film-burn"), t, Easing::Linear)
                    .value("seed", &number(seed))
            }),
        ));
        for softness in [0.06, 0.0, 0.5] {
            cases.push((
                format!("ink-bleed seed {seed} softness {softness}"),
                Box::new(move |t| {
                    TransitionNode::new(transition("ink-bleed"), t, Easing::Linear)
                        .value("seed", &number(seed))
                        .value("softness", &number(softness))
                }),
            ));
        }
    }
    for d in EIGHT {
        for roughness in [0.04, 0.2] {
            cases.push((
                format!("paper-tear {} roughness {roughness}", d.id()),
                Box::new(move |t| {
                    TransitionNode::new(transition("paper-tear"), t, Easing::Linear)
                        .direction(d)
                        .value("roughness", &number(roughness))
                }),
            ));
        }
    }
    let n = (SIZE.0 * SIZE.1) as usize;
    for (what, node) in &cases {
        let mut before = vec![1.0f32; n];
        let mut partly = 0;
        for step in 0..=32 {
            let t = f64::from(step) / 32.0;
            let now = remaining(&gpu, node(t));
            for (i, (x, y)) in before.iter().zip(&now).enumerate() {
                // An f16 step of slack for the rounding of sums that cancel.
                assert!(
                    *y <= x + 1e-3,
                    "{what}: pixel {i} uncovered again at {t}: {x} -> {y}"
                );
                assert!((0.0..=1.0).contains(y), "{what}: alpha {y} at {t}");
            }
            if now.iter().any(|v| *v > 0.0 && *v < 1.0) {
                partly += 1;
            }
            if step % 8 == 0 && step > 0 && step < 32 {
                let left = now.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
                println!("{what} at {t}: {left:.3} of A left");
            }
            before = now;
        }
        assert!(before.iter().all(|v| *v == 0.0), "{what}: not all B at 1");
        // B spreads over many steps, not in one jump.
        assert!(
            partly >= 16,
            "{what}: only {partly} of 33 steps part covered"
        );
    }
}

#[test]
fn the_light_leak_stays_finite_and_within_its_cap() {
    let _serial = common::timing::serial();
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let n = (SIZE.0 * SIZE.1) as usize;
    // Noise with HDR red and negative green, a white field and a black field with one
    // HDR highlight.
    let white = vec![[1.0f32; 4]; n];
    let mut highlight = vec![[0.0, 0.0, 0.0, 1.0]; n];
    highlight[18 * SIZE.0 as usize + 32] = [4.0, 4.0, 4.0, 1.0];
    let pairs = [
        ("noise", noise(n, 5), noise(n, 6)),
        ("white", white.clone(), white.clone()),
        ("highlight", highlight.clone(), highlight.clone()),
    ];
    for (what, a, b) in &pairs {
        for d in SIDES {
            for intensity in [2.0, 8.0] {
                for step in 1..16 {
                    let t = f64::from(step) / 16.0;
                    let node = TransitionNode::new(transition("light-leak"), t, Easing::Linear)
                        .direction(d)
                        .value("intensity", &number(intensity));
                    let out = run(&gpu, a, b, node);
                    for (i, p) in out.iter().enumerate() {
                        let (pa, pb) = (a[i], b[i]);
                        assert!(p.iter().all(|c| c.is_finite()), "{what}: {p:?}");
                        for c in 0..3 {
                            // The cap: the alpha, or the brighter of the two inputs.
                            let cap = p[3].max(pa[c]).max(pb[c]);
                            let floor = pa[c].min(pb[c]);
                            let slack = 2e-3 * cap.abs().max(1.0);
                            assert!(
                                p[c] <= cap + slack && p[c] >= floor - slack,
                                "{what} {} intensity {intensity} at {t}: pixel {i} \
                                 channel {c} {} outside {floor}..{cap}",
                                d.id(),
                                p[c]
                            );
                        }
                        let alpha = [pa[3], pb[3]];
                        assert!(p[3] >= alpha[0].min(alpha[1]) - 1e-3);
                        assert!(p[3] <= alpha[0].max(alpha[1]) + 1e-3);
                    }
                    if *what == "highlight" {
                        let h = out[18 * SIZE.0 as usize + 32];
                        assert!(h[0] <= 4.0 + 1e-2, "highlight gained: {h:?}");
                    }
                }
            }
        }
    }
    // The leak does light the picture.
    let mid = run(
        &gpu,
        &pairs[0].1,
        &pairs[0].2,
        TransitionNode::new(transition("light-leak"), 0.5, Easing::Linear),
    );
    let lift: f64 = mid
        .iter()
        .zip(pairs[0].1.iter().zip(&pairs[0].2))
        .map(|(p, (x, y))| f64::from(p[0]) - 0.5 * f64::from(x[0] + y[0]))
        .sum::<f64>()
        / n as f64;
    println!("light leak half way: mean red lift {lift:.4}");
    assert!(lift > 0.02, "mean red lift {lift}");
}

#[test]
fn pixelate_blocks_are_flat_and_grow_to_the_middle() {
    let _serial = common::timing::serial();
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let n = (SIZE.0 * SIZE.1) as usize;
    let (a, b) = (noise(n, 7), noise(n, 8));
    // A block size of an eighth of 64 px: 8 px half way, on a grid from (32, 18).
    let at = |t: f64| {
        let node = TransitionNode::new(transition("pixelate"), t, Easing::Linear)
            .value("block_size", &number(0.125));
        run(&gpu, &a, &b, node)
    };
    let distinct = |out: &[[f32; 4]]| {
        let mut values: Vec<[u32; 4]> = out.iter().map(|p| p.map(f32::to_bits)).collect();
        values.sort_unstable();
        values.dedup();
        values.len()
    };
    let mid = at(0.5);
    for y in 0..SIZE.1 {
        for x in 0..SIZE.0 {
            // The block's top left corner, cut at the frame's top.
            let corner = (
                32 + (i64::from(x) - 32).div_euclid(8) * 8,
                (18 + (i64::from(y) - 18).div_euclid(8) * 8).max(0),
            );
            let first = mid[(corner.1 * i64::from(SIZE.0) + corner.0) as usize];
            assert_eq!(
                mid[(y * SIZE.0 + x) as usize],
                first,
                "({x}, {y}) differs from its block's corner {corner:?}"
            );
        }
    }
    // 8 columns of blocks by 6 rows (the top and bottom ones cut).
    assert!(distinct(&mid) <= 48, "{} values half way", distinct(&mid));
    let counts: Vec<usize> = [0.1, 0.25, 0.5, 0.75, 0.9]
        .iter()
        .map(|t| distinct(&at(*t)))
        .collect();
    assert!(
        counts[0] > counts[1] && counts[1] > counts[2] && counts[2] < counts[3],
        "values per frame {counts:?}"
    );
    assert!(counts[3] < counts[4], "values per frame {counts:?}");
}

/// GPU time of each stylised transition at 4K over noise with its defaults, from the
/// node's timestamps (spanning all its passes) over 30 frames across the transition,
/// held to 3 ms by the shared timing rules (`common::timing`).
#[test]
fn stylised_transitions_at_4k_gpu_time() {
    let _serial = common::timing::serial();
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
    let mut measured = Vec::new();
    // The cross dissolve first, for reference: the cost of reading both frames.
    for id in std::iter::once("cross-dissolve").chain(IDS) {
        let t = transition(id);
        let mut times = Vec::new();
        for frame in 0..32u32 {
            let mut g = Graph::new();
            let (ia, ib) = (g.import("a", &va), g.import("b", &vb));
            let node = TransitionNode::new(t.clone(), f64::from(frame) / 31.0, Easing::EaseInOut);
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
        let times = common::timing::Samples::new(times);
        println!("4K {id} GPU: {times}");
        measured.push((id, times));
    }
    for (id, times) in measured.iter().skip(1) {
        // Under half the 8 ms compositor budget, as the motion transitions.
        common::timing::assert_gpu_within(&format!("4K {id}"), times, 3.0);
    }
}
