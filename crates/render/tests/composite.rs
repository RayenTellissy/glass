//! Layer compositing on the GPU against the CPU reference in `render::blend`: every
//! blend mode over a grid of premultiplied backdrop and layer values (alpha 0, partial
//! and 1, HDR values above 1 included), alpha edge cases (transparent layers, no
//! background, layers smaller than the canvas), more layers than one pass holds, and the
//! GPU time of five 4K layers.
//!
//! Tolerance: the CPU result (f64) is rounded to f16 like the GPU's output, then the two
//! may differ by 2e-3 times max(1, |value|), about two f16 steps at 1.0; the f32 maths on
//! the GPU and f16 rounding at a boundary account for up to one.

mod common;

use render::blend::{BlendMode, composite};
use render::graph::{Graph, TexturePool};
use render::nodes::{CompositeNode, Layer, composite_stack};
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::wgpu;

const LEVELS: [f32; 8] = [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0, 1.5];
const ALPHAS: [f32; 4] = [1.0, 0.5, 0.25, 0.0];
/// `LEVELS.len()^2 * ALPHAS.len()^2` pixels, one grid combination each.
const GRID: (u32, u32) = (32, 32);

fn q(v: f32) -> f32 {
    common::f16_to_f32(common::f32_to_f16(v))
}

/// Every combination of backdrop and layer level and alpha, one per pixel.
fn grid() -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
    let mut dst = Vec::new();
    let mut src = Vec::new();
    for &cb in &LEVELS {
        for &cs in &LEVELS {
            for &ab in &ALPHAS {
                for &a_s in &ALPHAS {
                    // Green and blue take other levels so channels are independent.
                    let g = LEVELS[(LEVELS.iter().position(|&v| v == cb).unwrap_or(0) + 3) % 8];
                    dst.push([cb * ab, g * ab, 0.3 * ab, ab].map(q));
                    src.push([cs * a_s, (1.0 - cs.min(1.0)) * a_s, 0.6 * a_s, a_s].map(q));
                }
            }
        }
    }
    (dst, src)
}

fn close(gpu: [f32; 4], cpu: [f64; 4], what: &str) {
    for c in 0..4 {
        let tol = 2e-3 * cpu[c].abs().max(1.0);
        assert!(
            (f64::from(gpu[c]) - cpu[c]).abs() <= tol,
            "{what}: channel {c}: gpu {gpu:?}, cpu {cpu:?}"
        );
    }
}

fn widen(p: [f32; 4]) -> [f64; 4] {
    p.map(f64::from)
}

#[test]
fn every_blend_mode_matches_the_cpu_reference() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let renderer = Renderer::new(&device, &queue);
    let mut pool = TexturePool::new();
    let size = GRID;
    let (dst, src) = grid();
    let backdrop = common::rgba16f(&device, &queue, size, &dst);
    let layer = common::rgba16f(&device, &queue, size, &src);
    let (bv, lv) = (
        backdrop.create_view(&Default::default()),
        layer.create_view(&Default::default()),
    );
    let out = common::blank(&device, wgpu::TextureFormat::Rgba16Float, size);
    let ov = out.create_view(&Default::default());
    for mode in BlendMode::ALL {
        let mut g = Graph::new();
        let b = g.import("backdrop", &bv);
        let l = g.import("layer", &lv);
        let node = g.add(
            CompositeNode::new(size, true, vec![Layer::blended(mode)]),
            &[b, l],
        );
        g.export(node.out(0), &ov);
        g.compile()
            .expect("compiles")
            .execute(&renderer, &mut pool, None);
        let pixels = common::read(&device, &queue, &out);
        let mut worst = 0f64;
        for (i, px) in pixels.iter().enumerate() {
            let want = composite(mode, widen(src[i]), widen(dst[i]));
            let want = want.map(|v| f64::from(q(v as f32)));
            close(*px, want, &format!("{mode:?} pixel {i}"));
            for c in 0..4 {
                worst = worst.max((f64::from(px[c]) - want[c]).abs());
            }
        }
        println!("{}: max |gpu - cpu| {worst:.5}", mode.id());
    }
}

/// Twelve layers with different modes over a background: two passes (eight, then
/// four) give the same result as folding the CPU reference over all of them.
#[test]
fn stacks_longer_than_one_pass_match_the_cpu() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let renderer = Renderer::new(&device, &queue);
    let mut pool = TexturePool::new();
    let size = GRID;
    let (dst, src) = grid();
    let backdrop = common::rgba16f(&device, &queue, size, &dst);
    let bv = backdrop.create_view(&Default::default());
    let layers: Vec<(wgpu::Texture, BlendMode)> = (0..12)
        .map(|k| {
            let shifted: Vec<[f32; 4]> = (0..src.len())
                .map(|i| src[(i * 7 + k * 13) % src.len()])
                .collect();
            let mode = BlendMode::ALL[k % BlendMode::ALL.len()];
            (common::rgba16f(&device, &queue, size, &shifted), mode)
        })
        .collect();
    let views: Vec<wgpu::TextureView> = layers
        .iter()
        .map(|(t, _)| t.create_view(&Default::default()))
        .collect();
    let out = common::blank(&device, wgpu::TextureFormat::Rgba16Float, size);
    let ov = out.create_view(&Default::default());
    let mut g = Graph::new();
    let b = g.import("backdrop", &bv);
    let ports: Vec<_> = views
        .iter()
        .zip(&layers)
        .map(|(v, (_, mode))| (g.import("layer", v), Layer::blended(*mode)))
        .collect();
    let result = composite_stack(&mut g, size, Some(b), ports).expect("layers");
    g.export(result, &ov);
    let compiled = g.compile().expect("compiles");
    assert_eq!(compiled.stats().passes, 2);
    compiled.execute(&renderer, &mut pool, None);
    let pixels = common::read(&device, &queue, &out);
    let mut worst = 0f64;
    for (i, px) in pixels.iter().enumerate() {
        let mut acc = widen(dst[i]);
        for (k, (_, mode)) in layers.iter().enumerate() {
            let s = src[(i * 7 + k * 13) % src.len()];
            acc = composite(*mode, widen(s), acc);
            if k == 7 {
                // The first pass stores its result in f16.
                acc = acc.map(|v| f64::from(q(v as f32)));
            }
        }
        for c in 0..4 {
            let err = (f64::from(px[c]) - acc[c]).abs() / acc[c].abs().max(1.0);
            worst = worst.max(err);
        }
    }
    println!("12 layers in 2 passes: max relative error {worst:.5}");
    assert!(worst < 5e-3, "{worst}");
}

/// Alpha edge cases, exactly: a fully transparent layer leaves the backdrop bit for bit,
/// every mode over nothing (no background input) gives the layer itself, and a layer
/// smaller than the canvas covers only its own top-left rectangle.
#[test]
fn alpha_edge_cases() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let renderer = Renderer::new(&device, &queue);
    let mut pool = TexturePool::new();
    let size = GRID;
    let (dst, src) = grid();
    let backdrop = common::rgba16f(&device, &queue, size, &dst);
    let layer = common::rgba16f(&device, &queue, size, &src);
    let clear = common::rgba16f(&device, &queue, size, &vec![[0.0; 4]; src.len()]);
    let small_px: Vec<[f32; 4]> = (0..8 * 4).map(|i| src[i * 5]).collect();
    let small = common::rgba16f(&device, &queue, (8, 4), &small_px);
    let (bv, lv, cv, sv) = (
        backdrop.create_view(&Default::default()),
        layer.create_view(&Default::default()),
        clear.create_view(&Default::default()),
        small.create_view(&Default::default()),
    );
    let out = common::blank(&device, wgpu::TextureFormat::Rgba16Float, size);
    let ov = out.create_view(&Default::default());
    let mut run =
        |background: Option<&wgpu::TextureView>, layers: &[&wgpu::TextureView], mode: BlendMode| {
            let mut g = Graph::new();
            let mut inputs: Vec<_> = background
                .map(|v| g.import("backdrop", v))
                .into_iter()
                .collect();
            inputs.extend(layers.iter().map(|v| g.import("layer", v)));
            let node = CompositeNode::new(
                size,
                background.is_some(),
                vec![Layer::blended(mode); layers.len()],
            );
            let node = g.add(node, &inputs);
            g.export(node.out(0), &ov);
            g.compile()
                .expect("compiles")
                .execute(&renderer, &mut pool, None);
            common::read(&device, &queue, &out)
        };
    for mode in BlendMode::ALL {
        let pixels = run(Some(&bv), &[&cv, &cv], mode);
        assert_eq!(
            pixels, dst,
            "{mode:?}: transparent layers change the backdrop"
        );
        let pixels = run(None, &[&lv], mode);
        assert_eq!(
            pixels, src,
            "{mode:?}: a layer over nothing is not the layer"
        );
        let pixels = run(Some(&cv), &[&lv], mode);
        assert_eq!(
            pixels, src,
            "{mode:?}: a layer over transparency is not the layer"
        );
        let pixels = run(Some(&bv), &[&sv], mode);
        for (i, px) in pixels.iter().enumerate() {
            let (x, y) = (i % size.0 as usize, i / size.0 as usize);
            if x < 8 && y < 4 {
                let want = composite(mode, widen(small_px[y * 8 + x]), widen(dst[i]));
                close(
                    *px,
                    want.map(|v| f64::from(q(v as f32))),
                    &format!("{mode:?} small {i}"),
                );
            } else {
                assert_eq!(*px, dst[i], "{mode:?}: pixel {i} outside the small layer");
            }
        }
    }
}

/// Five 4K layers (normal at the bottom, then multiply, screen, overlay and soft light)
/// in one composite pass, timed with GPU timestamps over 60 frames after a warm-up.
/// Prints the median and p99 and holds the median under the whole Phase 4 budget (8 ms
/// for layers, transforms, text, shapes and masks together).
#[test]
fn five_4k_layers_gpu_time() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut profiler = GpuProfiler::new(&device, &queue, 4);
    if !profiler.enabled() {
        eprintln!("skipped: no timestamp queries");
        return;
    }
    let renderer = Renderer::new(&device, &queue);
    let mut pool = TexturePool::new();
    let size = (3840, 2160);
    let modes = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::SoftLight,
    ];
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
        let layers = views
            .iter()
            .zip(modes)
            .map(|(v, mode)| (g.import("layer", v), Layer::blended(mode)))
            .collect();
        let result = composite_stack(&mut g, size, None, layers).expect("layers");
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
    println!("5 layers at 4K, one pass: GPU median {median:.3} ms, p99 {p99:.3} ms");
    assert!(median < 8.0, "median {median} ms");
}
