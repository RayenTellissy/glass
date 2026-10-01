//! The LUT node: identity tables exact within f16 in every input space, GPU against the
//! f64 CPU reference (shaper, 3D table, gamut into Rec.709 from a BT.2020 working space,
//! intensity), intensity 0 passing pixels through untouched, a 65-point `.cube` written
//! and read back from disk, and the GPU time of a 4K pass. Every LUT is generated here.
//! Skips without a GPU.

mod common;

use std::sync::Arc;

use render::colour::WorkingSpace;
use render::graph::{Graph, TexturePool};
use render::lut::{self, Domain, Lut, Lut1d, Lut3d, LutSpace};
use render::nodes::{GpuLut, LutNode};
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::wgpu;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
}

fn gpu() -> Option<Gpu> {
    let (device, queue) = common::device()?;
    let renderer = Renderer::new(&device, &queue);
    Some(Gpu {
        device,
        queue,
        renderer,
    })
}

/// Runs a LUT node over premultiplied `pixels` (one row) and reads the result back.
fn run(
    gpu: &Gpu,
    lut: &Lut,
    working: WorkingSpace,
    space: LutSpace,
    intensity: f32,
    pixels: &[[f32; 4]],
) -> Vec<[f32; 4]> {
    let size = (pixels.len() as u32, 1);
    let input = common::rgba16f(&gpu.device, &gpu.queue, size, pixels);
    let input_view = input.create_view(&Default::default());
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, size);
    let target_view = target.create_view(&Default::default());
    let uploaded = Arc::new(GpuLut::new(&gpu.device, &gpu.queue, lut));
    let mut g = Graph::new();
    let src = g.import("linear", &input_view);
    let out = g.add(LutNode::new(uploaded, working, space, intensity), &[src]);
    g.export(out.out(0), &target_view);
    let mut pool = TexturePool::new();
    g.compile()
        .expect("compiles")
        .execute(&gpu.renderer, &mut pool, None);
    common::read(&gpu.device, &gpu.queue, &target)
}

fn f16(v: f32) -> f32 {
    common::f16_to_f32(common::f32_to_f16(v))
}

/// One f16 step at `v` (the spacing of f16 values around it).
fn f16_step(v: f32) -> f32 {
    let a = v.abs().max(2f32.powi(-14));
    2f32.powi(a.log2().floor() as i32 - 10)
}

/// A LUT's table values as the GPU stores them (f16), so the CPU reference and the GPU
/// see the same table.
fn as_f16(lut: &Lut) -> Lut {
    let round = |t: &Vec<[f32; 3]>| t.iter().map(|e| e.map(f16)).collect();
    Lut {
        title: lut.title.clone(),
        shaper: lut.shaper.as_ref().map(|s| Lut1d {
            table: round(&s.table),
            ..s.clone()
        }),
        cube: lut.cube.as_ref().map(|c| Lut3d {
            table: round(&c.table),
            ..c.clone()
        }),
    }
}

fn noise(n: usize, seed: u32, max: f32) -> Vec<[f32; 4]> {
    let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state >> 8) as f32 / (1 << 24) as f32
    };
    (0..n)
        .map(|i| {
            let a = match i % 4 {
                0 => 1.0,
                1 => 0.5,
                2 => 0.25 + 0.75 * next(),
                _ => 1.0,
            };
            // A few values below 0 and above the LUT domain, to exercise clamping.
            let mut v = || (next() * (max + 0.1) - 0.05) * a;
            [f16(v()), f16(v()), f16(v()), f16(a)]
        })
        .collect()
}

/// A film-ish look: a warm offset, an S-curve per channel and a little saturation,
/// smooth so interpolation error stays small.
fn look(rgb: [f64; 3]) -> [f64; 3] {
    let s = |v: f64| v * v * (3.0 - 2.0 * v) * 0.9 + 0.1 * v;
    let [r, g, b] = rgb.map(s);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    [
        (y + 1.2 * (r - y) + 0.03).clamp(0.0, 1.0),
        (y + 1.2 * (g - y) + 0.01).clamp(0.0, 1.0),
        (y + 1.2 * (b - y) - 0.02).clamp(0.0, 1.0),
    ]
}

fn assert_matches_cpu(
    gpu: &Gpu,
    lut: &Lut,
    working: WorkingSpace,
    space: LutSpace,
    intensity: f32,
    pixels: &[[f32; 4]],
) -> f64 {
    let got = run(gpu, lut, working, space, intensity, pixels);
    let reference = as_f16(lut);
    let mut worst = 0.0f64;
    for (px, out) in pixels.iter().zip(&got) {
        let want = lut::apply_in_working_space(
            &reference,
            working,
            space,
            f64::from(intensity),
            px.map(f64::from),
        );
        for c in 0..4 {
            let err = (f64::from(out[c]) - want[c]).abs();
            worst = worst.max(err);
            assert!(
                err <= 1.5e-3 * want[c].abs().max(1.0),
                "{space:?} {intensity}: {px:?} -> {out:?}, want {want:?}"
            );
        }
    }
    worst
}

#[test]
fn identity_luts_are_exact_within_f16() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // Every f16 grey level step of 1/256 from 0 to 1, plus colours, opaque and at half
    // alpha.
    let mut pixels: Vec<[f32; 4]> = (0..=256)
        .map(|i| {
            let v = f16(i as f32 / 256.0);
            [v, f16(v * 0.5), f16(1.0 - v), 1.0]
        })
        .collect();
    pixels.extend(
        pixels
            .clone()
            .iter()
            .map(|p| [f16(p[0] * 0.5), f16(p[1] * 0.5), f16(p[2] * 0.5), 0.5]),
    );
    for size in [2, 17, 33, 65] {
        let identity = Lut {
            cube: Some(Lut3d::identity(size)),
            ..Default::default()
        };
        for space in LutSpace::ALL {
            let got = run(&gpu, &identity, WorkingSpace::BT709, space, 1.0, &pixels);
            for (px, out) in pixels.iter().zip(&got) {
                for c in 0..4 {
                    assert!(
                        (out[c] - px[c]).abs() <= f16_step(px[c]),
                        "{size} {space:?}: {px:?} -> {out:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn gpu_matches_the_cpu_reference() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let pixels = noise(4096, 7, 1.0);
    let cube = Lut {
        title: Some("look".into()),
        shaper: None,
        cube: Some(Lut3d::from_fn(33, look)),
    };
    let mut worst = 0.0f64;
    for space in LutSpace::ALL {
        for intensity in [1.0, 0.7] {
            worst = worst.max(assert_matches_cpu(
                &gpu,
                &cube,
                WorkingSpace::BT709,
                space,
                intensity,
                &pixels,
            ));
        }
    }
    // A Rec.709 LUT in an HDR project: the BT.2020 working space is converted into
    // BT.709 and back around the table, and values above reference white clamp.
    let hdr = noise(4096, 11, 3.0);
    worst = worst.max(assert_matches_cpu(
        &gpu,
        &cube,
        WorkingSpace::BT2020,
        LutSpace::Rec709,
        1.0,
        &hdr,
    ));

    // A shaper long enough to wrap onto a second texture row, with a log-like curve
    // over an extended input range, feeding a 17-point cube with its own range.
    let shaper_size = 5000;
    let shaper = Lut1d {
        size: shaper_size,
        domain: Domain {
            min: [-0.1; 3],
            max: [1.5; 3],
        },
        table: (0..shaper_size)
            .map(|i| {
                let x = -0.1 + 1.6 * i as f64 / (shaper_size - 1) as f64;
                let v = ((x.max(0.0) * 40.0 + 1.0).ln() / 65f64.ln()) as f32;
                [v, v * 0.98, v]
            })
            .collect(),
    };
    let mut small = Lut3d::from_fn(17, look);
    small.domain = Domain {
        min: [0.0; 3],
        max: [1.05; 3],
    };
    let shaped = Lut {
        title: None,
        shaper: Some(shaper.clone()),
        cube: Some(small),
    };
    worst = worst.max(assert_matches_cpu(
        &gpu,
        &shaped,
        WorkingSpace::BT709,
        LutSpace::Linear,
        0.85,
        &noise(4096, 13, 1.4),
    ));
    let only_1d = Lut {
        title: None,
        shaper: Some(shaper),
        cube: None,
    };
    worst = worst.max(assert_matches_cpu(
        &gpu,
        &only_1d,
        WorkingSpace::BT709,
        LutSpace::Srgb,
        1.0,
        &pixels,
    ));
    println!("LUT node GPU vs f64 CPU: max error {worst:.6}");
}

#[test]
fn zero_intensity_passes_pixels_through() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let invert = Lut {
        cube: Some(Lut3d::from_fn(9, |rgb| rgb.map(|v| 1.0 - v))),
        ..Default::default()
    };
    let mut pixels = noise(1024, 3, 2.0);
    pixels.push([0.0; 4]);
    pixels.push([0.25, 0.125, 0.5, 0.0]);
    let got = run(
        &gpu,
        &invert,
        WorkingSpace::BT709,
        LutSpace::Rec709,
        0.0,
        &pixels,
    );
    assert_eq!(got, pixels);
    // Full strength does change them.
    let got = run(
        &gpu,
        &invert,
        WorkingSpace::BT709,
        LutSpace::Rec709,
        1.0,
        &pixels[..4],
    );
    assert_ne!(got, pixels[..4]);
}

#[test]
fn a_65_point_cube_file_round_trips_from_disk() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let lut = Lut {
        title: Some("Generated look 65".into()),
        shaper: None,
        cube: Some(Lut3d::from_fn(65, look)),
    };
    let dir = std::env::temp_dir().join(format!("glass-lut-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("look-65.cube");
    std::fs::write(&path, lut.to_cube()).expect("write");
    let read = Lut::read(&path).expect("parses");
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(read, lut);
    assert_eq!(read.cube.as_ref().map(|c| c.size), Some(65));
    let worst = assert_matches_cpu(
        &gpu,
        &read,
        WorkingSpace::BT709,
        LutSpace::Rec709,
        1.0,
        &noise(4096, 5, 1.0),
    );
    println!("65-point LUT from disk, GPU vs CPU: max error {worst:.6}");
}

/// A smooth 4K picture (gradients and slow waves over the whole colour cube), so
/// neighbouring pixels look up neighbouring table entries as in real footage.
fn picture_4k(gpu: &Gpu) -> wgpu::Texture {
    let (w, h) = (3840usize, 2160usize);
    let mut data = Vec::with_capacity(w * h * 8);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let b = 0.5 + 0.45 * (u * 9.0 + v * 5.0).sin();
            for c in [u, v, b, 1.0] {
                data.extend_from_slice(&common::f32_to_f16(c).to_le_bytes());
            }
        }
    }
    common::texture(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba16Float,
        (w as u32, h as u32),
        &data,
    )
}

/// GPU time of one 4K LUT pass, median of 30 frames from the pass timestamps, for 33
/// and 65-point tables (Rec.709 input, so with both transfers), a 1D-only table and the
/// pass at intensity 0 (a plain copy, for comparison). On a smooth picture, and on
/// per-pixel noise, the worst case: every pixel looks up a different part of the table,
/// so the table no longer stays in the texture cache.
#[test]
fn lut_pass_at_4k_gpu_time() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut profiler = GpuProfiler::new(&gpu.device, &gpu.queue, 4);
    if !profiler.enabled() {
        eprintln!("skipped: no timestamp queries");
        return;
    }
    let inputs = [
        ("picture", picture_4k(&gpu)),
        ("noise", common::noise_4k(&gpu.device, &gpu.queue, 1)),
    ];
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, (3840, 2160));
    let target_view = target.create_view(&Default::default());
    let mut pool = TexturePool::new();
    let curve = Lut1d {
        size: 4096,
        domain: Domain::UNIT,
        table: (0..4096)
            .map(|i| [(i as f32 / 4095.0).powf(0.8); 3])
            .collect(),
    };
    let cases = [
        (
            "33-point",
            Lut {
                cube: Some(Lut3d::from_fn(33, look)),
                ..Default::default()
            },
        ),
        (
            "65-point",
            Lut {
                cube: Some(Lut3d::from_fn(65, look)),
                ..Default::default()
            },
        ),
        (
            "1D 4096",
            Lut {
                shaper: Some(curve.clone()),
                ..Default::default()
            },
        ),
    ];
    for ((input_name, input), (name, lut)) in inputs
        .iter()
        .flat_map(|i| cases.iter().map(move |c| (i, c)))
    {
        let input_view = input.create_view(&Default::default());
        let uploaded = Arc::new(GpuLut::new(&gpu.device, &gpu.queue, lut));
        let intensities: &[f32] = if *name == "33-point" {
            &[0.8, 0.0]
        } else {
            &[0.8]
        };
        for &intensity in intensities {
            let mut times = Vec::new();
            for frame in 0..32u32 {
                let mut g = Graph::new();
                let src = g.import("linear", &input_view);
                let out = g.add(
                    LutNode::new(
                        uploaded.clone(),
                        WorkingSpace::BT709,
                        LutSpace::Rec709,
                        intensity,
                    ),
                    &[src],
                );
                g.export(out.out(0), &target_view);
                g.compile().expect("compiles").execute(
                    &gpu.renderer,
                    &mut pool,
                    Some(&mut profiler),
                );
                let timings = profiler.wait(&gpu.device).expect("timed");
                // The first two frames build the pipeline and warm the caches.
                if frame >= 2 {
                    times.extend(timings.passes[0].gpu_ms);
                }
            }
            times.sort_by(f64::total_cmp);
            let median = times[times.len() / 2];
            let max = times[times.len() - 1];
            let what = if intensity > 0.0 {
                *name
            } else {
                "copy (intensity 0)"
            };
            println!(
                "4K LUT pass, {input_name}, {what}: median {median:.3} ms, max {max:.3} ms GPU ({} frames)",
                times.len()
            );
            // Half of the 8 ms compositor budget on pictures, loose for a shared GPU; noise
            // is reported, and only has to stay under a frame at 60 fps.
            let limit = if *input_name == "picture" { 4.0 } else { 16.0 };
            assert!(median < limit, "{input_name}, {what}: {median} ms");
        }
    }
}
