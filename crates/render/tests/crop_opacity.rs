//! Layer crop, feather and opacity in the composite pass against the CPU reference
//! (`nodes::Layer::reference`, `render::crop`, `render::blend::composite`): whole-pixel
//! crops are exact, the feather ramp follows `crop::edge`, cropped transformed layers
//! match the reference, and opacity scales the layer before every blend mode (0 leaves
//! the backdrop, 1 changes nothing).

mod common;

use render::blend::{BlendMode, composite};
use render::crop::{self, Crop};
use render::graph::{Graph, TexturePool};
use render::nodes::{CompositeNode, Layer};
use render::renderer::Renderer;
use render::transform::Transform2D;
use render::wgpu;

const SIZE: (u32, u32) = (48, 32);
/// Absolute error allowed per channel, times max(1, |value|): as in `transform.rs`.
const TOLERANCE: f64 = 5e-3;

fn q(v: f32) -> f32 {
    common::f16_to_f32(common::f32_to_f16(v))
}

/// Premultiplied texels with gradients, partial alpha and values above 1.
fn texels((w, h): (u32, u32), seed: u32) -> Vec<[f32; 4]> {
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let a = [1.0, 0.75, 0.5, 0.25][((x + y + seed) % 4) as usize];
            let r = (x as f32 / w as f32) * 1.5;
            let g = (y as f32 / h as f32 + seed as f32 * 0.3) % 1.0;
            let b = ((x * 7 + y * 3 + seed) % 11) as f32 / 10.0;
            [r * a, g * a, b * a, a].map(q)
        })
        .collect()
}

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    pool: TexturePool,
}

impl Rig {
    fn new() -> Option<Self> {
        let (device, queue) = common::device()?;
        let renderer = Renderer::new(&device, &queue);
        Some(Self {
            device,
            queue,
            renderer,
            pool: TexturePool::new(),
        })
    }

    /// One layer of `SIZE` over an optional backdrop of `SIZE`, on a `SIZE` canvas.
    fn render(
        &mut self,
        backdrop: Option<&[[f32; 4]]>,
        src: &[[f32; 4]],
        layer: Layer,
    ) -> Vec<[f32; 4]> {
        let b = backdrop.map(|px| common::rgba16f(&self.device, &self.queue, SIZE, px));
        let l = common::rgba16f(&self.device, &self.queue, SIZE, src);
        let bv = b.as_ref().map(|t| t.create_view(&Default::default()));
        let lv = l.create_view(&Default::default());
        let out = common::blank(&self.device, wgpu::TextureFormat::Rgba16Float, SIZE);
        let ov = out.create_view(&Default::default());
        let mut g = Graph::new();
        let mut inputs: Vec<_> = bv.iter().map(|v| g.import("backdrop", v)).collect();
        inputs.push(g.import("layer", &lv));
        let node = g.add(
            CompositeNode::new(SIZE, backdrop.is_some(), vec![layer]),
            &inputs,
        );
        g.export(node.out(0), &ov);
        g.compile()
            .expect("compiles")
            .execute(&self.renderer, &mut self.pool, None);
        common::read(&self.device, &self.queue, &out)
    }
}

fn centre(i: usize) -> [f64; 2] {
    let w = SIZE.0 as usize;
    [(i % w) as f64 + 0.5, (i / w) as f64 + 0.5]
}

fn close(gpu: [f32; 4], cpu: [f64; 4], what: &str) {
    for c in 0..4 {
        assert!(
            (f64::from(gpu[c]) - cpu[c]).abs() <= TOLERANCE * cpu[c].abs().max(1.0),
            "{what}: channel {c}: gpu {gpu:?}, cpu {cpu:?}"
        );
    }
}

/// Whole-pixel crops on an untransformed layer keep the pixels inside bit for bit and
/// clear the rest; a crop that meets itself leaves nothing.
#[test]
fn whole_pixel_crops_are_exact() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let src = texels(SIZE, 0);
    let crop = Crop {
        left: 5.0,
        top: 3.0,
        right: 11.0,
        bottom: 7.0,
        feather: 0.0,
    };
    let got = rig.render(None, &src, Layer::default().with_crop(crop));
    for (i, px) in got.iter().enumerate() {
        let (x, y) = (i % SIZE.0 as usize, i / SIZE.0 as usize);
        let inside = (5..48 - 11).contains(&x) && (3..32 - 7).contains(&y);
        let want = if inside { src[i] } else { [0.0; 4] };
        assert_eq!(*px, want, "pixel {i}");
    }
    let gone = Crop {
        left: 30.0,
        right: 18.0,
        ..Default::default()
    };
    let got = rig.render(None, &src, Layer::default().with_crop(gone));
    assert!(got.iter().all(|px| *px == [0.0; 4]));
}

/// A feathered crop on an opaque white layer: each row across the left edge follows the
/// smooth ramp of `crop::edge` (0 half a pixel outside, rising monotonically, 1 past the
/// feather), and corners multiply the two edges.
#[test]
fn feather_ramp() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let white = vec![[1.0; 4]; (SIZE.0 * SIZE.1) as usize];
    let crop = Crop {
        left: 4.0,
        top: 2.0,
        right: 3.5,
        bottom: 0.0,
        feather: 10.0,
    };
    let got = rig.render(None, &white, Layer::default().with_crop(crop));
    let alpha = |x: usize, y: usize| f64::from(got[y * SIZE.0 as usize + x][3]);
    let row = 16;
    let mut last = 0.0;
    for x in 0..24 {
        let a = alpha(x, row);
        let ramp = crop::edge(x as f64 + 0.5 - 4.0, 10.0, 1.0)
            * crop::edge(16.5 - 2.0, 10.0, 1.0)
            * crop::edge(32.0 - 16.5, 10.0, 1.0);
        assert!((a - ramp).abs() < 1e-3, "x {x}: {a} vs {ramp}");
        assert!(a >= last, "ramp falls at x {x}");
        last = a;
    }
    assert_eq!(alpha(3, row), 0.0);
    assert!((alpha(8, row) - 0.5).abs() < 0.08);
    assert_eq!(alpha(20, row), 1.0);
    for (i, px) in got.iter().enumerate() {
        let want = Layer::default()
            .with_crop(crop)
            .reference(&white, SIZE, centre(i));
        close(*px, want, &format!("pixel {i}"));
    }
}

/// Crop and feather move with the layer: a rotated, scaled, cropped and feathered layer
/// with partial opacity matches the CPU reference.
#[test]
fn cropped_transformed_layers_match_the_cpu_reference() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let src = texels(SIZE, 1);
    let crops = [
        Crop {
            left: 3.25,
            top: 1.5,
            right: 7.75,
            bottom: 4.0,
            feather: 0.0,
        },
        Crop {
            left: 6.0,
            top: 2.0,
            right: 1.0,
            bottom: 5.5,
            feather: 4.5,
        },
    ];
    let mut worst = 0f64;
    for crop in crops {
        for (rotation, scale) in [(0.0, [1.0, 1.0]), (23.0, [0.9, 1.2]), (-71.0, [1.4, 0.7])] {
            let layer = Layer::blended(BlendMode::Normal)
                .with_transform(Transform2D {
                    position: [24.3, 16.6],
                    scale,
                    rotation,
                    anchor: [24.0, 16.0],
                })
                .with_crop(crop)
                .with_opacity(0.8);
            let got = rig.render(None, &src, layer.clone());
            for (i, px) in got.iter().enumerate() {
                let want = layer.reference(&src, SIZE, centre(i));
                close(*px, want, &format!("{crop:?} {rotation} pixel {i}"));
                for c in 0..4 {
                    worst = worst.max((f64::from(px[c]) - want[c]).abs());
                }
            }
        }
    }
    println!("cropped transformed layers: max |gpu - cpu| {worst:.5}");
}

/// Opacity scales the premultiplied layer before blending, for every mode: the result
/// is the CPU composite of the scaled layer; opacity 0 leaves the backdrop bit for bit
/// and opacity 1 is bit for bit the same as no opacity setting.
#[test]
fn opacity_with_every_blend_mode() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let dst = texels(SIZE, 2);
    let src = texels(SIZE, 3);
    for mode in BlendMode::ALL {
        let plain = rig.render(Some(&dst), &src, Layer::blended(mode));
        let full = rig.render(Some(&dst), &src, Layer::blended(mode).with_opacity(1.0));
        assert_eq!(full, plain, "{mode:?}: opacity 1 changed the result");
        let none = rig.render(Some(&dst), &src, Layer::blended(mode).with_opacity(0.0));
        assert_eq!(none, dst, "{mode:?}: opacity 0 changed the backdrop");
        for opacity in [0.25f32, 0.5, 0.8] {
            let got = rig.render(Some(&dst), &src, Layer::blended(mode).with_opacity(opacity));
            for (i, px) in got.iter().enumerate() {
                let o = f64::from(opacity);
                let scaled = src[i].map(|c| f64::from(c) * o);
                let want = composite(mode, scaled, dst[i].map(f64::from));
                let want = want.map(|v| f64::from(q(v as f32)));
                close(*px, want, &format!("{mode:?} opacity {opacity} pixel {i}"));
            }
        }
    }
}
