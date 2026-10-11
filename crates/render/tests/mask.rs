//! Layer masks on the GPU against the CPU reference in `render::mask`: hard and
//! feathered shape masks, inversion, union and intersection in list order, several shapes
//! in one mask, masks without shapes, and the multiply into the layer's premultiplied
//! colour; plus the GPU time of a feathered three-shape mask at 1080p and 4K.
//!
//! Shape coverage in the reference is the exact pixel area of each shape's polygon
//! (`shape::reference`). Tolerances, per channel: 2e-3 for hard masks (the coverage's
//! own straight-edge error plus f16 rounding), 4e-3 once feathered (the blur pairs taps
//! in one bilinear fetch, whose hardware weights are fixed point).

mod common;

use render::graph::{Graph, Port, TexturePool};
use render::mask::{self, Mask, MaskMode};
use render::nodes::MaskNode;
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::shape::reference::{self, P};
use render::shape::{Affine, Geometry, Point, Rect, Shape};
use render::wgpu;

const HARD: f64 = 2e-3;
const FEATHERED: f64 = 4e-3;
const SIZE: (u32, u32) = (96, 64);
/// The layer's premultiplied colour (half transparent), above 1 in red.
const COLOUR: [f32; 4] = [1.5, 0.25, 0.1, 0.5];

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

    /// A layer of [`COLOUR`] masked by `masks`, read back.
    fn render(&mut self, masks: &[Mask]) -> Vec<[f32; 4]> {
        self.render_with(|g, renderer, layer| mask::add_to_graph(g, renderer, layer, SIZE, masks))
    }

    /// A layer of [`COLOUR`] through the nodes `build` adds, read back.
    fn render_with(
        &mut self,
        build: impl FnOnce(&mut Graph<'_>, &Renderer, Port) -> Port,
    ) -> Vec<[f32; 4]> {
        let pixels = vec![COLOUR; (SIZE.0 * SIZE.1) as usize];
        let layer = common::rgba16f(&self.device, &self.queue, SIZE, &pixels)
            .create_view(&Default::default());
        let out = common::blank(&self.device, wgpu::TextureFormat::Rgba16Float, SIZE);
        let view = out.create_view(&Default::default());
        let mut g = Graph::new();
        let input = g.import("layer", &layer);
        let masked = build(&mut g, &self.renderer, input);
        g.export(masked, &view);
        g.compile()
            .expect("compiles")
            .execute(&self.renderer, &mut self.pool, None);
        common::read(&self.device, &self.queue, &out)
    }
}

/// Exact coverage of a shape's interior, from its polygon.
fn coverage(shape: &Shape) -> Vec<f64> {
    let outline = shape.outline_of_fill(mask::fill_rule(shape));
    let polys: Vec<Vec<P>> = outline
        .contours
        .iter()
        .map(|c| c.iter().map(|p| [f64::from(p.x), f64::from(p.y)]).collect())
        .collect();
    reference::polygon_coverage(&polys, SIZE)
        .into_iter()
        .map(|c| c.abs().min(1.0))
        .collect()
}

/// The reference of `masks` over the layer, checked against the GPU within `tol`.
fn check(gpu: &mut Gpu, masks: &[Mask], tol: f64, what: &str) -> Vec<f64> {
    let unions: Vec<Vec<f64>> = masks
        .iter()
        .map(|m| {
            m.shapes
                .iter()
                .map(coverage)
                .reduce(|a, b| a.iter().zip(&b).map(|(x, y)| x.max(*y)).collect())
                .unwrap_or_else(|| vec![0.0; (SIZE.0 * SIZE.1) as usize])
        })
        .collect();
    let expected = mask::reference(masks, &unions, SIZE);
    let actual = gpu.render(masks);
    let mut worst = (0.0f64, 0);
    for (i, (px, m)) in actual.iter().zip(&expected).enumerate() {
        for (c, want) in px.iter().zip(COLOUR) {
            // f16 output: relative rounding on top of the absolute tolerance.
            let want = f64::from(want) * m;
            let d = (f64::from(*c) - want).abs() - want.abs() * 2f64.powi(-11);
            if d > worst.0 {
                worst = (d, i);
            }
        }
    }
    let (x, y) = (worst.1 % SIZE.0 as usize, worst.1 / SIZE.0 as usize);
    eprintln!("{what}: max error {:.5} at ({x}, {y})", worst.0);
    assert!(
        worst.0 <= tol,
        "{what}: off by {:.5} at ({x}, {y}): GPU {:?}, mask {}",
        worst.0,
        actual[worst.1],
        expected[worst.1]
    );
    expected
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Shape {
    Shape::new(Geometry::Rect {
        rect: Rect::new(x, y, w, h),
        radius: 0.0,
    })
}

fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Shape {
    Shape::new(Geometry::Ellipse {
        center: Point::new(0.0, 0.0),
        radii: [rx, ry],
    })
    .transformed(Affine::rotate(0.3).then(Affine::translate(cx, cy)))
}

#[test]
fn a_hard_mask_is_the_shapes_coverage() {
    let Some(mut gpu) = Gpu::new() else { return };
    let m = check(
        &mut gpu,
        &[Mask::new(vec![rect(10.25, 8.5, 40.5, 30.25)])],
        HARD,
        "hard rectangle",
    );
    assert!(m.contains(&1.0) && m.contains(&0.0) && m.iter().any(|v| *v > 0.2 && *v < 0.8));
    check(
        &mut gpu,
        &[Mask::new(vec![ellipse(48.0, 30.0, 30.0, 18.0)])],
        HARD,
        "hard ellipse",
    );
}

#[test]
fn a_feathered_mask_matches_the_reference() {
    let Some(mut gpu) = Gpu::new() else { return };
    for feather in [0.5, 1.0, 3.0, 7.5, 20.0] {
        check(
            &mut gpu,
            &[Mask::new(vec![ellipse(40.0, 32.0, 24.0, 14.0)]).feathered(feather)],
            FEATHERED,
            &format!("ellipse feathered {feather}"),
        );
    }
    // Feathered against the layer's border: coverage beyond it repeats the edge.
    check(
        &mut gpu,
        &[Mask::new(vec![rect(-10.0, -10.0, 50.0, 40.0)]).feathered(6.0)],
        FEATHERED,
        "rectangle over the corner, feathered",
    );
}

#[test]
fn inverted_masks_keep_the_outside() {
    let Some(mut gpu) = Gpu::new() else { return };
    check(
        &mut gpu,
        &[Mask::new(vec![rect(20.5, 10.0, 30.0, 30.0)]).inverted()],
        HARD,
        "inverted",
    );
    check(
        &mut gpu,
        &[Mask::new(vec![ellipse(48.0, 30.0, 20.0, 12.0)])
            .feathered(5.0)
            .inverted()],
        FEATHERED,
        "inverted and feathered",
    );
}

#[test]
fn masks_combine_by_union_and_intersection_in_order() {
    let Some(mut gpu) = Gpu::new() else { return };
    let a = || Mask::new(vec![rect(8.0, 8.0, 40.0, 40.0)]);
    let b = || Mask::new(vec![ellipse(50.0, 32.0, 26.0, 16.0)]);
    let c = || Mask::new(vec![rect(30.0, 0.0, 12.0, 64.0)]);
    check(&mut gpu, &[a(), b()], HARD, "union");
    check(
        &mut gpu,
        &[a(), b().with_mode(MaskMode::Intersection)],
        HARD,
        "intersection",
    );
    // Order matters: (a U b) I c differs from (a I c) U b.
    let one = check(
        &mut gpu,
        &[a(), b(), c().with_mode(MaskMode::Intersection)],
        HARD,
        "(a U b) I c",
    );
    let two = check(
        &mut gpu,
        &[a(), c().with_mode(MaskMode::Intersection), b()],
        HARD,
        "(a I c) U b",
    );
    assert_ne!(one, two);
    // Feathered, inverted and intersected: a soft hole cut into a soft rectangle, then a
    // band added back.
    check(
        &mut gpu,
        &[
            a().feathered(4.0),
            b().feathered(3.0)
                .inverted()
                .with_mode(MaskMode::Intersection),
            c().feathered(2.0),
        ],
        FEATHERED,
        "feathered, inverted, mixed",
    );
}

#[test]
fn shapes_of_one_mask_are_unioned() {
    let Some(mut gpu) = Gpu::new() else { return };
    let shapes = vec![
        rect(4.0, 4.0, 30.0, 20.0),
        ellipse(30.0, 30.0, 18.0, 10.0),
        Shape::new(Geometry::Polygon(vec![
            Point::new(60.0, 6.0),
            Point::new(90.0, 40.0),
            Point::new(55.0, 58.0),
        ])),
        rect(70.5, 44.5, 20.0, 15.0),
    ];
    check(&mut gpu, &[Mask::new(shapes.clone())], HARD, "four shapes");
    check(
        &mut gpu,
        &[Mask::new(shapes[..3].to_vec()).feathered(4.0)],
        FEATHERED,
        "three shapes feathered",
    );
    check(
        &mut gpu,
        &[Mask::new(shapes[..2].to_vec()).feathered(2.0).inverted()],
        FEATHERED,
        "two shapes feathered and inverted",
    );
}

#[test]
fn masks_without_shapes_hide_or_keep_the_layer() {
    let Some(mut gpu) = Gpu::new() else { return };
    let hidden = gpu.render(&[Mask::new(Vec::new())]);
    assert!(hidden.iter().all(|p| *p == [0.0; 4]));
    let kept = gpu.render(&[Mask::new(Vec::new()).inverted()]);
    // A mask node with no masks copies the layer.
    let colour = gpu.render_with(|g, _, layer| g.add(MaskNode::new(SIZE, &[]), &[layer]).out(0));
    assert_eq!(kept, colour);
    assert!(
        colour
            .iter()
            .all(|p| (p[0] - COLOUR[0]).abs() < 1e-3 && p[3] == COLOUR[3])
    );
}

/// GPU time of one mask of three shapes (a rectangle, a rotated ellipse and a polygon)
/// feathered by 20 px over a layer at 1080p and 4K: the coverage nodes and the mask node.
#[test]
#[ignore = "timing: run alone with --ignored --nocapture"]
fn feathered_three_shape_mask_gpu_time() {
    let Some(mut gpu) = Gpu::new() else { return };
    let mut profiler = GpuProfiler::new(&gpu.device, &gpu.queue, 4);
    if !profiler.enabled() {
        eprintln!("skipped: no timestamp queries");
        return;
    }
    for (name, size) in [("1080p", (1920, 1080)), ("4K", (3840, 2160))] {
        let (w, h) = (size.0 as f32, size.1 as f32);
        let shapes = vec![
            rect(w * 0.1, h * 0.1, w * 0.35, h * 0.4),
            Shape::new(Geometry::Ellipse {
                center: Point::new(0.0, 0.0),
                radii: [w * 0.2, h * 0.25],
            })
            .transformed(Affine::rotate(0.4).then(Affine::translate(w * 0.6, h * 0.55))),
            Shape::new(Geometry::Polygon(vec![
                Point::new(w * 0.7, h * 0.05),
                Point::new(w * 0.95, h * 0.45),
                Point::new(w * 0.55, h * 0.9),
                Point::new(w * 0.45, h * 0.3),
            ])),
        ];
        let masks = [Mask::new(shapes).feathered(20.0)];
        let layer = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, size)
            .create_view(&Default::default());
        let out = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, size);
        let view = out.create_view(&Default::default());
        let mut times = Vec::new();
        for frame in 0..60 {
            let mut g = Graph::new();
            let input = g.import("layer", &layer);
            let masked = mask::add_to_graph(&mut g, &gpu.renderer, input, size, &masks);
            g.export(masked, &view);
            g.compile().expect("compiles").execute(
                &gpu.renderer,
                &mut gpu.pool,
                Some(&mut profiler),
            );
            let timings = profiler.wait(&gpu.device).expect("timed");
            if frame >= 10 {
                times.push(timings.total_ms.expect("total"));
            }
        }
        times.sort_by(f64::total_cmp);
        let median = times[times.len() / 2];
        let p99 = times[(times.len() * 99).div_ceil(100) - 1];
        println!(
            "three-shape mask feathered 20 px at {name}: GPU median {median:.3} ms, p99 \
             {p99:.3} ms"
        );
    }
}
