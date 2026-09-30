//! Layer transforms in the composite pass against the CPU reference
//! (`nodes::Layer::reference`, built on `render::transform`): transforms that map pixel
//! centres onto pixel centres (identity, whole-pixel moves, quarter turns, flips) are
//! exact; free transforms match the reference within a tolerance for each filter; the
//! per-pass resample cap; and the GPU time of five 4K layers untransformed, transformed
//! bilinear and transformed bicubic.
//!
//! Tolerance for the smooth filters: the GPU's bilinear fetches weight texels with a
//! fixed-point fraction, and the bicubic combines nine of them, so outputs differ from
//! the f64 reference by up to about 0.003 on M2 (values to 1.4); `TOLERANCE` allows 0.005
//! times max(1, |value|).

mod common;

use render::blend::BlendMode;
use render::crop::Crop;
use render::graph::{Graph, Node, TexturePool};
use render::nodes::{CompositeNode, Layer, composite_stack_capped};
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::transform::{Affine2, Resample, Transform2D};
use render::wgpu;

const CANVAS: (u32, u32) = (64, 48);
const LAYER: (u32, u32) = (40, 30);
/// Absolute error allowed per channel for bilinear and bicubic, times max(1, |value|).
const TOLERANCE: f64 = 5e-3;

/// Premultiplied texels with smooth gradients, edges and a few values above 1.
fn texels((w, h): (u32, u32)) -> Vec<[f32; 4]> {
    let mut state = 0x9e37_79b9_u32;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state >> 8) as f32 / (1 << 24) as f32
    };
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            let a = if (i % w) % 7 == 3 { 0.5 } else { 1.0 };
            let r = if x > 0.5 { 1.4 } else { x };
            [r * a, y * a, next() * a, a].map(|v| common::f16_to_f32(common::f32_to_f16(v)))
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

    /// Composites `layers` (texture pixels and settings) over nothing on `canvas`.
    fn render(
        &mut self,
        canvas: (u32, u32),
        layers: &[(&[[f32; 4]], (u32, u32), Layer)],
        max_resample: Resample,
    ) -> Vec<[f32; 4]> {
        let textures: Vec<wgpu::Texture> = layers
            .iter()
            .map(|(px, size, _)| common::rgba16f(&self.device, &self.queue, *size, px))
            .collect();
        let views: Vec<wgpu::TextureView> = textures
            .iter()
            .map(|t| t.create_view(&Default::default()))
            .collect();
        let out = common::blank(&self.device, wgpu::TextureFormat::Rgba16Float, canvas);
        let ov = out.create_view(&Default::default());
        let mut g = Graph::new();
        let ports = views
            .iter()
            .zip(layers)
            .map(|(v, (_, _, l))| (g.import("layer", v), l.clone()))
            .collect();
        let result =
            composite_stack_capped(&mut g, canvas, None, ports, max_resample).expect("layers");
        g.export(result, &ov);
        g.compile()
            .expect("compiles")
            .execute(&self.renderer, &mut self.pool, None);
        common::read(&self.device, &self.queue, &out)
    }
}

fn centre(i: usize, canvas: (u32, u32)) -> [f64; 2] {
    let w = canvas.0 as usize;
    [(i % w) as f64 + 0.5, (i / w) as f64 + 0.5]
}

/// Transforms that land every canvas pixel centre on a layer pixel centre give exactly
/// the layer's texels, whatever filter is asked for.
#[test]
fn pixel_aligned_transforms_are_exact() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let src = texels(LAYER);
    let transforms = [
        ("identity", Transform2D::default()),
        (
            "whole-pixel move",
            Transform2D {
                position: [13.0, -4.0],
                ..Default::default()
            },
        ),
        (
            "90 degrees about the centre",
            Transform2D {
                rotation: 90.0,
                ..Transform2D::centred(LAYER, CANVAS)
            },
        ),
        (
            "-90 degrees about a corner",
            Transform2D {
                position: [5.0, 45.0],
                rotation: -90.0,
                ..Default::default()
            },
        ),
        (
            "180 degrees",
            Transform2D {
                rotation: 180.0,
                ..Transform2D::centred(LAYER, CANVAS)
            },
        ),
        (
            "horizontal flip",
            Transform2D {
                scale: [-1.0, 1.0],
                ..Transform2D::centred(LAYER, CANVAS)
            },
        ),
    ];
    for (what, t) in transforms {
        for filter in [Resample::Bicubic, Resample::Bilinear, Resample::Nearest] {
            let layer = Layer::blended(BlendMode::Normal)
                .with_transform(t)
                .with_resample(filter);
            assert_eq!(layer.effective_resample(), Resample::Nearest, "{what}");
            let got = rig.render(CANVAS, &[(&src, LAYER, layer.clone())], Resample::Bicubic);
            let inv = t.matrix().inverse().expect("invertible");
            for (i, px) in got.iter().enumerate() {
                let p = inv.apply(centre(i, CANVAS));
                let inside = p[0] > 0.0 && p[1] > 0.0 && p[0] < 40.0 && p[1] < 30.0;
                let want = if inside {
                    src[p[1].floor() as usize * LAYER.0 as usize + p[0].floor() as usize]
                } else {
                    [0.0; 4]
                };
                assert_eq!(*px, want, "{what} ({filter:?}): pixel {i}");
                let cpu = layer.reference(&src, LAYER, centre(i, CANVAS));
                assert_eq!(cpu, want.map(f64::from), "{what}: CPU reference, pixel {i}");
            }
        }
    }
}

/// Free transforms (rotation, non-uniform scale up and down, subpixel position, off-centre
/// anchor) match the CPU reference with each filter, edges included.
#[test]
fn free_transforms_match_the_cpu_reference() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let src = texels(LAYER);
    let transforms = [
        Transform2D {
            position: [31.3, 22.7],
            scale: [1.3, 0.8],
            rotation: 17.0,
            anchor: [20.0, 15.0],
        },
        Transform2D {
            position: [10.25, 40.5],
            scale: [0.6, 1.1],
            rotation: -48.5,
            anchor: [3.0, 27.0],
        },
        Transform2D {
            position: [32.1, 24.2],
            scale: [2.5, 2.5],
            rotation: 0.0,
            anchor: [20.0, 15.0],
        },
        Transform2D {
            position: [0.4, 0.3],
            ..Default::default()
        },
    ];
    for filter in [Resample::Bicubic, Resample::Bilinear] {
        let mut worst = 0f64;
        for (k, t) in transforms.iter().enumerate() {
            let layer = Layer::blended(BlendMode::Normal)
                .with_transform(*t)
                .with_resample(filter);
            assert_eq!(layer.effective_resample(), filter);
            let got = rig.render(CANVAS, &[(&src, LAYER, layer.clone())], Resample::Bicubic);
            for (i, px) in got.iter().enumerate() {
                let want = layer.reference(&src, LAYER, centre(i, CANVAS));
                for c in 0..4 {
                    let err = (f64::from(px[c]) - want[c]).abs();
                    worst = worst.max(err / want[c].abs().max(1.0));
                    assert!(
                        err <= TOLERANCE * want[c].abs().max(1.0),
                        "{filter:?} transform {k} pixel {i} channel {c}: gpu {px:?}, cpu {want:?}"
                    );
                }
            }
        }
        println!("{}: max |gpu - cpu| {worst:.5}", filter.id());
    }
}

/// Nearest for pixel art: a 3x upscale lands every canvas pixel centre at a third of a
/// texel from any texel edge, so the GPU picks the same texel as the reference. Only the
/// edge coverage differs, by f32 rounding (under one f16 step).
#[test]
fn nearest_upscale_matches_the_cpu_reference() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let size = (16, 12);
    let src = texels(size);
    let layer = Layer::blended(BlendMode::Normal)
        .with_transform(Affine2::scale(3.0, 3.0))
        .with_resample(Resample::Nearest);
    let got = rig.render(CANVAS, &[(&src, size, layer.clone())], Resample::Bicubic);
    for (i, px) in got.iter().enumerate() {
        let want = layer.reference(&src, size, centre(i, CANVAS));
        for c in 0..4 {
            assert!(
                (f64::from(px[c]) - want[c]).abs() <= 1e-3 * want[c].abs().max(1.0),
                "pixel {i}: gpu {px:?}, cpu {want:?}"
            );
        }
    }
}

/// A layer scaled to nothing draws nothing; a stack of transformed layers blends each
/// through its own transform.
#[test]
fn zero_scale_and_stacks() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let src = texels(LAYER);
    let gone = Layer::default().with_transform(Affine2::scale(0.0, 1.0));
    let got = rig.render(CANVAS, &[(&src, LAYER, gone)], Resample::Bicubic);
    assert!(got.iter().all(|px| *px == [0.0; 4]));
    let layers: Vec<Layer> = [
        (BlendMode::Normal, 0.0),
        (BlendMode::Multiply, 30.0),
        (BlendMode::Screen, -60.0),
    ]
    .into_iter()
    .map(|(mode, rotation)| {
        Layer::blended(mode).with_transform(Transform2D {
            rotation,
            scale: [1.2, 1.2],
            ..Transform2D::centred(LAYER, CANVAS)
        })
    })
    .collect();
    let inputs: Vec<_> = layers
        .iter()
        .map(|l| (&src[..], LAYER, l.clone()))
        .collect();
    let got = rig.render(CANVAS, &inputs, Resample::Bicubic);
    for (i, px) in got.iter().enumerate() {
        let mut acc = [0.0; 4];
        for l in &layers {
            acc =
                render::blend::composite(l.blend, l.reference(&src, LAYER, centre(i, CANVAS)), acc);
        }
        for c in 0..4 {
            assert!(
                (f64::from(px[c]) - acc[c]).abs() <= TOLERANCE * acc[c].abs().max(1.0),
                "pixel {i}: gpu {px:?}, cpu {acc:?}"
            );
        }
    }
}

/// A resample cap turns bicubic layers into bilinear ones, and each pass reports the
/// filters it samples with (the shader entry point follows the most expensive one).
#[test]
fn resample_cap() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let src = texels(LAYER);
    let moved = Transform2D {
        position: [3.3, 2.6],
        rotation: 5.0,
        ..Default::default()
    };
    let bicubic = Layer::default().with_transform(moved);
    let bilinear = bicubic.clone().with_resample(Resample::Bilinear);
    let aligned = Layer::default();
    let node = CompositeNode::new(CANVAS, false, vec![aligned.clone(), bicubic.clone()]);
    assert!(
        node.params()
            .contains(&("resample", "nearest,bicubic".to_owned()))
    );
    let node = node.with_max_resample(Resample::Bilinear);
    assert!(
        node.params()
            .contains(&("resample", "nearest,bilinear".to_owned()))
    );
    let want = rig.render(CANVAS, &[(&src, LAYER, bilinear)], Resample::Bicubic);
    let got = rig.render(
        CANVAS,
        &[(&src, LAYER, bicubic.clone())],
        Resample::Bilinear,
    );
    assert_eq!(got, want);
    let sharp = rig.render(CANVAS, &[(&src, LAYER, bicubic)], Resample::Bicubic);
    assert_ne!(sharp, want);
}

/// GPU time of one composite pass of five 4K noise layers (normal, multiply, screen,
/// overlay, soft light) with `layers`' settings, over 60 frames after a warm-up: median
/// and p99 in milliseconds, or `None` without a GPU or timestamp queries.
fn time_five_4k_layers(layers: &[Layer], max_resample: Resample) -> Option<(f64, f64)> {
    let (device, queue) = common::device()?;
    let mut profiler = GpuProfiler::new(&device, &queue, 4);
    if !profiler.enabled() {
        return None;
    }
    let renderer = Renderer::new(&device, &queue);
    let mut pool = TexturePool::new();
    let size = (3840, 2160);
    let textures: Vec<wgpu::Texture> = (0..5)
        .map(|k| common::noise_4k(&device, &queue, k + 1))
        .collect();
    let views: Vec<wgpu::TextureView> = textures
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect();
    let out = common::blank(&device, wgpu::TextureFormat::Rgba16Float, size);
    let ov = out.create_view(&Default::default());
    let mut times = Vec::new();
    for frame in 0..70 {
        let mut g = Graph::new();
        let ports = views
            .iter()
            .zip(layers)
            .map(|(v, l)| (g.import("layer", v), l.clone()))
            .collect();
        let result =
            composite_stack_capped(&mut g, size, None, ports, max_resample).expect("layers");
        g.export(result, &ov);
        let compiled = g.compile().expect("compiles");
        assert_eq!(compiled.stats().passes, 1);
        compiled.execute(&renderer, &mut pool, Some(&mut profiler));
        let timings = profiler.wait(&device).expect("timed");
        if frame >= 10 {
            times.push(timings.total_ms.expect("total"));
        }
    }
    times.sort_by(f64::total_cmp);
    let median = times[times.len() / 2];
    let p99 = times[(times.len() * 99).div_ceil(100) - 1];
    Some((median, p99))
}

const MODES: [BlendMode; 5] = [
    BlendMode::Normal,
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::SoftLight,
];

/// Each layer rotated, scaled (non-uniformly) and moved by a subpixel amount about its
/// centre, covering most of the canvas; when `cropped`, also cut by 90 to 330 pixels per
/// edge with a 40-pixel feather at 85% opacity.
fn transformed_4k_layers(cropped: bool) -> Vec<Layer> {
    MODES
        .into_iter()
        .enumerate()
        .map(|(k, mode)| {
            let k = k as f64;
            let layer = Layer::blended(mode).with_transform(Transform2D {
                position: [1920.0 + 13.3 * k, 1080.0 - 7.7 * k],
                scale: [1.05 + 0.02 * k, 1.04 - 0.01 * k],
                rotation: 2.0 * k - 3.5,
                anchor: [1920.0, 1080.0],
            });
            if !cropped {
                return layer;
            }
            layer
                .with_crop(Crop {
                    left: 90.0 + 60.0 * k,
                    top: 120.0 + 30.0 * k,
                    right: 330.0 - 50.0 * k,
                    bottom: 100.0 + 20.0 * k,
                    feather: 40.0,
                })
                .with_opacity(0.85)
        })
        .collect()
}

/// Five 4K layers untransformed, then transformed, then transformed and cropped with
/// feather and opacity, each with bilinear (the playback cap) and bicubic, timed in one
/// run so the machine's load is the same for all. The Phase 4 budget (8 ms for the whole
/// frame) is held for untransformed and bilinear; bicubic is for paused frames and export
/// and is only reported.
#[test]
fn five_transformed_4k_layers_gpu_time() {
    let plain: Vec<Layer> = MODES.into_iter().map(Layer::blended).collect();
    let Some((median, p99)) = time_five_4k_layers(&plain, Resample::Bicubic) else {
        eprintln!("skipped: no GPU adapter or timestamp queries");
        return;
    };
    println!("5 layers at 4K, one pass, GPU median / p99: untransformed {median:.3} / {p99:.3} ms");
    assert!(median < 8.0, "untransformed median {median} ms");
    for cropped in [false, true] {
        let layers = transformed_4k_layers(cropped);
        let what = if cropped {
            "transformed, cropped"
        } else {
            "transformed"
        };
        for resample in [Resample::Bilinear, Resample::Bicubic] {
            let (median, p99) = time_five_4k_layers(&layers, resample).expect("timed");
            println!(
                "5 layers at 4K, one pass, GPU median / p99: {what} {} {median:.3} / {p99:.3} ms",
                resample.id()
            );
            if resample == Resample::Bilinear {
                assert!(median < 8.0, "{what} bilinear median {median} ms");
            }
        }
    }
}
