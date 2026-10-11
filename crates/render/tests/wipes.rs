//! The wipes (`assets/transitions/`): linear in 8 directions, radial, clock, iris shapes,
//! barn door, venetian blinds and checker. Each is exactly A at progress 0 and exactly B
//! at 1 (linear and eased, whatever the direction and parameters); B's coverage and the
//! border's outer edge never shrink as progress grows; the linear wipe and the checker
//! match f64 references of their formulas mid way; a few placements are checked by
//! sampling; plus the GPU time of each at 4K. GPU checks skip without an adapter.

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

const IDS: [&str; 7] = [
    "linear-wipe",
    "radial-wipe",
    "clock-wipe",
    "iris",
    "barn-door",
    "venetian-blinds",
    "checker-wipe",
];

/// The test pictures' size: small, not square, with a checker cell under a pixel's
/// multiple.
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

/// A wipe with a direction and parameters, as a function of linear progress and easing.
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

/// A green border, so a black-to-white wipe shows B's coverage in red and the border's
/// outer edge in green.
const GREEN: ParamValue = ParamValue::Colour([0.0, 1.0, 0.0, 1.0]);

/// Every offered direction and choice, with hard and soft edges (the soft ones with a
/// border), at the default and at unusual parameter values.
fn variants() -> Vec<Variant> {
    let mut v = Vec::new();
    // (softness, border)
    let edges = [(0.0, 0.0), (6.0, 2.0)];
    for (soft, border) in edges {
        let styled = move |node: TransitionNode| {
            node.value("softness", &number(soft))
                .value("border_width", &number(border))
                .value("border_colour", &GREEN)
        };
        let style = format!("softness {soft} border {border}");
        for d in Direction::ALL {
            v.push(variant(
                "linear-wipe",
                format!("{} {style}", d.id()),
                move |n| styled(n.direction(d)),
            ));
        }
        for centre in [[0.5, 0.5], [0.1, 0.8], [1.3, -0.2]] {
            v.push(variant(
                "radial-wipe",
                format!("centre {centre:?} {style}"),
                move |n| styled(n.value("centre", &ParamValue::Point(centre))),
            ));
        }
        for start in [0.0, 90.0, -30.0] {
            for rotation in ["clockwise", "anticlockwise"] {
                for centre in [[0.5, 0.5], [0.3, 0.6]] {
                    v.push(variant(
                        "clock-wipe",
                        format!("{start} {rotation} {centre:?} {style}"),
                        move |n| {
                            styled(
                                n.value("start_angle", &number(start))
                                    .value("rotation", &choice(rotation))
                                    .value("centre", &ParamValue::Point(centre)),
                            )
                        },
                    ));
                }
            }
        }
        for shape in ["circle", "diamond", "rectangle", "star"] {
            for mode in ["open", "close"] {
                v.push(variant(
                    "iris",
                    format!("{shape} {mode} {style}"),
                    move |n| {
                        styled(
                            n.value("shape", &choice(shape))
                                .value("mode", &choice(mode)),
                        )
                    },
                ));
            }
        }
        for split in ["vertical", "horizontal"] {
            for mode in ["open", "close"] {
                v.push(variant(
                    "barn-door",
                    format!("{split} {mode} {style}"),
                    move |n| {
                        styled(
                            n.value("split", &choice(split))
                                .value("mode", &choice(mode)),
                        )
                    },
                ));
            }
        }
        for d in [
            Direction::Down,
            Direction::Up,
            Direction::Right,
            Direction::Left,
        ] {
            for count in [1.0, 5.0] {
                v.push(variant(
                    "venetian-blinds",
                    format!("{} count {count} {style}", d.id()),
                    move |n| styled(n.direction(d).value("count", &number(count))),
                ));
            }
            for (columns, rows) in [(8.0, 5.0), (3.0, 2.0)] {
                v.push(variant(
                    "checker-wipe",
                    format!("{} {columns} x {rows} {style}", d.id()),
                    move |n| {
                        styled(
                            n.direction(d)
                                .value("columns", &number(columns))
                                .value("rows", &number(rows)),
                        )
                    },
                ));
            }
        }
    }
    v
}

#[test]
fn the_wipes_are_bundled_with_names() {
    let registry = TransitionRegistry::bundled();
    assert!(registry.problems().is_empty(), "{:#?}", registry.problems());
    let english = assets::i18n::Locale::english();
    assert!(english.get("transition.category.wipe").is_some());
    for id in IDS {
        let t = registry.get(id).unwrap_or_else(|| panic!("no {id}"));
        assert_eq!(t.category(), "wipe");
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
        let ids: Vec<&str> = t.params().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids[ids.len() - 3..],
            ["softness", "border_width", "border_colour"],
            "{id}: the common parameters come last"
        );
    }
    let directions = |id: &str| registry.get(id).expect("bundled").directions().to_vec();
    assert_eq!(directions("linear-wipe").len(), 8);
    assert_eq!(directions("linear-wipe")[0], Direction::Right);
    assert_eq!(
        directions("venetian-blinds"),
        [
            Direction::Down,
            Direction::Up,
            Direction::Right,
            Direction::Left
        ]
    );
    assert_eq!(directions("checker-wipe").len(), 4);
    for id in ["radial-wipe", "clock-wipe", "iris", "barn-door"] {
        assert!(directions(id).is_empty(), "{id}");
    }
}

#[test]
fn every_wipe_is_exactly_a_then_exactly_b() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let n = (SIZE.0 * SIZE.1) as usize;
    let (a, b) = (noise(n, 1), noise(n, 2));
    for v in variants() {
        for easing in [Easing::Linear, Easing::EaseInOut] {
            let at = |t| run(&gpu, SIZE, &a, &b, (v.node)(t, easing));
            assert_eq!(at(0.0), a, "{}: progress 0 is A", v.what);
            assert_eq!(at(1.0), b, "{}: progress 1 is B", v.what);
        }
    }
}

/// B's coverage (red) and the border's outer edge (green) of a black-to-white wipe at
/// `t`.
fn coverage(gpu: &Gpu, node: TransitionNode) -> Vec<[f32; 2]> {
    let n = (SIZE.0 * SIZE.1) as usize;
    let a = vec![[0.0, 0.0, 0.0, 1.0]; n];
    let b = vec![[1.0; 4]; n];
    run(gpu, SIZE, &a, &b, node)
        .into_iter()
        .map(|p| [p[0], p[1]])
        .collect()
}

#[test]
fn a_revealed_pixel_stays_revealed() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // One f16 step at 1, for sums of edges that round differently from step to step.
    let slack = 1e-3;
    let steps = 32;
    for v in variants() {
        let mut previous = coverage(&gpu, (v.node)(0.0, Easing::Linear));
        assert!(previous.iter().all(|c| *c == [0.0, 0.0]), "{}", v.what);
        let mut partial_mid_way = false;
        for step in 1..=steps {
            let t = f64::from(step) / f64::from(steps);
            let now = coverage(&gpu, (v.node)(t, Easing::Linear));
            for (i, (was, is)) in previous.iter().zip(&now).enumerate() {
                let (x, y) = (i as u32 % SIZE.0, i as u32 / SIZE.0);
                assert!(
                    is[0] >= was[0] - slack && is[1] >= was[1] - slack,
                    "{}: pixel ({x}, {y}) shrank from {was:?} to {is:?} at {t}",
                    v.what
                );
                assert!(
                    (0.0..=1.0).contains(&is[0]) && is[1] >= is[0] - slack && is[1] <= 1.0,
                    "{}: pixel ({x}, {y}) is {is:?} at {t}",
                    v.what
                );
            }
            if step == steps / 2 {
                let revealed = now.iter().filter(|c| c[0] >= 0.5).count();
                partial_mid_way = revealed > 0 && revealed < now.len();
            }
            previous = now;
        }
        assert!(previous.iter().all(|c| *c == [1.0, 1.0]), "{}", v.what);
        assert!(partial_mid_way, "{}: nothing or everything at 0.5", v.what);
    }
}

type Px = [f64; 4];

fn px(p: [f32; 4]) -> Px {
    p.map(f64::from)
}

/// The shaders' edge kernel: a 1 px box, widened to the softness and eased towards a
/// smoothstep as it passes 1 px.
fn edge(d: f64, softness: f64) -> f64 {
    let s = softness.max(0.0);
    let x = (d / s.max(1.0) + 0.5).clamp(0.0, 1.0);
    let smooth = x * x * (3.0 - 2.0 * x);
    x + (smooth - x) * (s - 1.0).clamp(0.0, 1.0)
}

/// The shaders' composite: A outside `outer`, the premultiplied border colour between,
/// B inside `inner`.
fn wipe(a: Px, b: Px, inner: f64, outer: f64, border: [f32; 4]) -> Px {
    let alpha = f64::from(border[3]);
    std::array::from_fn(|c| {
        let colour = if c == 3 {
            alpha
        } else {
            f64::from(border[c]) * alpha
        };
        a[c] * (1.0 - outer) + colour * (outer - inner) + b[c] * inner
    })
}

/// Compares the GPU output with `reference` (from the pixel's centre and its A and B)
/// per pixel; returns the largest error.
fn check(
    what: &str,
    out: &[[f32; 4]],
    a: &[[f32; 4]],
    b: &[[f32; 4]],
    reference: impl Fn([f64; 2], Px, Px) -> Px,
) -> f64 {
    let mut worst = 0.0f64;
    for (i, got) in out.iter().enumerate() {
        let centre = [
            f64::from(i as u32 % SIZE.0) + 0.5,
            f64::from(i as u32 / SIZE.0) + 0.5,
        ];
        let want = reference(centre, px(a[i]), px(b[i]));
        for c in 0..4 {
            let error = (f64::from(got[c]) - want[c]).abs();
            worst = worst.max(error);
            // An f16 step, and a little for f32 positions (1e-5 of a pixel moves an
            // anti-aliased edge's coverage by as much).
            let tol = want[c].abs() * 2f64.powi(-10) + 2e-4;
            assert!(
                error <= tol,
                "{what}: pixel {centre:?} channel {c}: {} vs {}",
                got[c],
                want[c]
            );
        }
    }
    worst
}

/// A wipe's softness and border width in pixels, and border colour.
#[derive(Clone, Copy, Debug)]
struct Style {
    softness: f64,
    border: f64,
    colour: [f32; 4],
}

impl Style {
    fn on(self, node: TransitionNode) -> TransitionNode {
        node.value("softness", &number(self.softness))
            .value("border_width", &number(self.border))
            .value("border_colour", &ParamValue::Colour(self.colour))
    }
}

/// The linear wipe's formula: the edge from a margin before the first corner along the
/// direction to a margin past the last.
fn linear(p: [f64; 2], a: Px, b: Px, d: Direction, t: f64, style: Style) -> Px {
    let Style {
        softness,
        border,
        colour,
    } = style;
    let dir = d.vector().map(f64::from);
    let size = [f64::from(SIZE.0), f64::from(SIZE.1)];
    let span = [dir[0] * size[0], dir[1] * size[1]];
    let margin = 0.5 * softness.max(1.0) + border;
    let first = span[0].min(0.0) + span[1].min(0.0) - margin;
    let last = span[0].max(0.0) + span[1].max(0.0) + margin;
    let d = first * (1.0 - t) + last * t - (p[0] * dir[0] + p[1] * dir[1]);
    wipe(a, b, edge(d, softness), edge(d + border, softness), colour)
}

/// The checker wipe's formula travelling right: cells of the first parity reach `2t` of
/// their width, the others `2t - 1`, filtered along and across by the edge kernel over
/// the cell and its neighbours.
fn checker(p: [f64; 2], a: Px, b: Px, grid: [f64; 2], t: f64, style: Style) -> Px {
    let Style {
        softness,
        border,
        colour,
    } = style;
    let (ca, cb) = (f64::from(SIZE.0) / grid[0], f64::from(SIZE.1) / grid[1]);
    let softness = softness.min(ca.min(cb));
    let (ka, kb) = ((p[0] / ca).floor(), (p[1] / cb).floor());
    let (x, y) = (p[0] - ka * ca, p[1] - kb * cb);
    let reach = [
        ca * (2.0 * t).clamp(0.0, 1.0),
        ca * (2.0 * t - 1.0).clamp(0.0, 1.0),
    ];
    let (mut inner, mut outer) = (0.0, 0.0);
    for j in -1..=1 {
        let across = y - f64::from(j) * cb;
        let w = edge(across, softness) - edge(across - cb, softness);
        for i in -1..=1 {
            let parity = (ka as i64 + i64::from(i) + kb as i64 + i64::from(j)).rem_euclid(2);
            let e = reach[parity as usize];
            let o = e + border.min(e).min(ca - e);
            let start = x - f64::from(i) * ca;
            inner += w * (edge(start, softness) - edge(start - e, softness));
            outer += w * (edge(start, softness) - edge(start - o, softness));
        }
    }
    wipe(a, b, inner.clamp(0.0, 1.0), outer.clamp(0.0, 1.0), colour)
}

#[test]
fn the_linear_and_checker_wipes_match_their_references_mid_way() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let n = (SIZE.0 * SIZE.1) as usize;
    let (a, b) = (noise(n, 3), noise(n, 4));
    let colour = [0.9f32, 0.2, 2.0, 0.75];
    let style = |softness, border| Style {
        softness,
        border,
        colour,
    };
    let mut report = Vec::new();
    for (d, style) in [
        (Direction::Right, style(0.0, 0.0)),
        (Direction::DownLeft, style(0.0, 0.0)),
        (Direction::Up, style(5.0, 2.5)),
        (Direction::UpRight, style(9.0, 3.0)),
    ] {
        let t = f64::from(0.37f32);
        let node = TransitionNode::new(transition("linear-wipe"), t, Easing::Linear).direction(d);
        let out = run(&gpu, SIZE, &a, &b, style.on(node));
        let what = format!("linear {} {style:?}", d.id());
        let worst = check(&what, &out, &a, &b, |p, a, b| linear(p, a, b, d, t, style));
        report.push((what, worst));
    }
    for (grid, t, style) in [
        ([8.0, 5.0], 0.3f32, style(0.0, 0.0)),
        ([3.0, 2.0], 0.7, style(4.0, 1.5)),
    ] {
        let t = f64::from(t);
        let node = TransitionNode::new(transition("checker-wipe"), t, Easing::Linear)
            .value("columns", &number(grid[0]))
            .value("rows", &number(grid[1]));
        let out = run(&gpu, SIZE, &a, &b, style.on(node));
        let what = format!("checker {grid:?} at {t} {style:?}");
        let worst = check(&what, &out, &a, &b, |p, a, b| {
            checker(p, a, b, grid, t, style)
        });
        report.push((what, worst));
    }
    for (what, worst) in report {
        println!("{what}: max error {worst:.6}");
    }
}

#[test]
fn the_wipes_reveal_b_where_intended() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // B's coverage at pixel (x, y) of the 64 x 36 frame.
    let at = |node: TransitionNode| {
        let c = coverage(&gpu, node);
        move |x: u32, y: u32| c[(y * SIZE.0 + x) as usize][0]
    };
    let new = |id: &str, t: f64| TransitionNode::new(transition(id), t, Easing::Linear);

    // Linear, travelling right a quarter of the way: the left columns only.
    let c = at(new("linear-wipe", 0.25));
    assert_eq!((c(2, 18), c(40, 18)), (1.0, 0.0));
    // Travelling up: the bottom rows first.
    let c = at(new("linear-wipe", 0.25).direction(Direction::Up));
    assert_eq!((c(32, 34), c(32, 2)), (1.0, 0.0));

    // Clock, clockwise from twelve, a quarter turn: the top right quadrant.
    let c = at(new("clock-wipe", 0.25));
    assert_eq!(
        [c(48, 9), c(16, 9), c(16, 27), c(48, 27)],
        [1.0, 0.0, 0.0, 0.0]
    );
    // Anticlockwise from three o'clock, a quarter turn: the top right quadrant again.
    let c = at(new("clock-wipe", 0.25)
        .value("rotation", &choice("anticlockwise"))
        .value("start_angle", &number(90.0)));
    assert_eq!(
        [c(48, 9), c(16, 9), c(16, 27), c(48, 27)],
        [1.0, 0.0, 0.0, 0.0]
    );

    // Radial from the top left corner: that corner first.
    let c = at(new("radial-wipe", 0.3).value("centre", &ParamValue::Point([0.0, 0.0])));
    assert_eq!((c(1, 1), c(62, 34)), (1.0, 0.0));

    // Iris open: the centre first; close: the corners first.
    for shape in ["circle", "diamond", "rectangle", "star"] {
        let c = at(new("iris", 0.3).value("shape", &choice(shape)));
        assert_eq!((c(32, 18), c(0, 0)), (1.0, 0.0), "{shape} open");
        let c = at(new("iris", 0.6)
            .value("shape", &choice(shape))
            .value("mode", &choice("close")));
        assert_eq!((c(32, 18), c(0, 0)), (0.0, 1.0), "{shape} close");
    }
    // The star points up: above the centre it reaches farther than to the side.
    let c = at(new("iris", 0.25).value("shape", &choice("star")));
    assert_eq!((c(32, 4), c(18, 18)), (1.0, 0.0));

    // Barn door, vertical split: the middle columns open first; horizontal: the middle
    // rows; closing: the edges.
    let c = at(new("barn-door", 0.3));
    assert_eq!((c(32, 2), c(2, 18), c(62, 18)), (1.0, 0.0, 0.0));
    let c = at(new("barn-door", 0.3).value("split", &choice("horizontal")));
    assert_eq!((c(2, 18), c(32, 1), c(32, 34)), (1.0, 0.0, 0.0));
    let c = at(new("barn-door", 0.3).value("mode", &choice("close")));
    assert_eq!((c(32, 2), c(1, 18), c(62, 18)), (0.0, 1.0, 1.0));

    // Venetian blinds, 4 slats of 9 rows travelling down, a third of the way: the top 3
    // rows of each slat.
    let c = at(new("venetian-blinds", 1.0 / 3.0).value("count", &number(4.0)));
    for slat in 0..4 {
        let top = slat * 9;
        assert_eq!((c(10, top + 1), c(10, top + 6)), (1.0, 0.0), "slat {slat}");
    }

    // Checker, 8 x 5 cells of 8 x 7.2 px travelling right, a quarter of the way: the
    // first half of the even cells, nothing of the odd ones.
    let c = at(new("checker-wipe", 0.25));
    assert_eq!((c(1, 3), c(6, 3), c(9, 3)), (1.0, 0.0, 0.0));
    assert_eq!((c(9, 10), c(14, 10), c(1, 10)), (1.0, 0.0, 0.0));
    // Three quarters: the even cells full, the odd ones half way.
    let c = at(new("checker-wipe", 0.75));
    assert_eq!((c(6, 3), c(9, 3), c(14, 3)), (1.0, 1.0, 0.0));
}

/// GPU time of one 4K pass of each wipe over noise with a soft edge and a border, median
/// of 30 frames from the pass timestamps; each is held to the budget by the shared timing
/// rules (`common::timing`).
#[test]
fn wipes_at_4k_gpu_time() {
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
    for id in IDS {
        let mut times = Vec::new();
        for frame in 0..32u32 {
            let mut g = Graph::new();
            let (ia, ib) = (g.import("a", &va), g.import("b", &vb));
            let node =
                TransitionNode::new(transition(id), f64::from(frame) / 31.0, Easing::EaseInOut)
                    .value("softness", &number(20.0))
                    .value("border_width", &number(6.0));
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
    for (id, times) in &measured {
        // A quarter of the 8 ms compositor budget.
        common::timing::assert_gpu_within(&format!("4K {id}"), times, 2.0);
    }
}
