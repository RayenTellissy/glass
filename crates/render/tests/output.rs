//! The output node: every transfer function against its CPU reference and back through
//! `media`'s decoding functions (round trip), PQ and HLG spot values, 8 and 10-bit
//! quantisation within half a code value, triangular dither against banding on slow 8
//! and 10-bit gradients, and the GPU time of a 4K output pass. Skips without a GPU.

mod common;

use media::{Primaries, TransferFn};
use render::colour::WorkingSpace;
use render::graph::{Graph, TexturePool};
use render::nodes::{Dither, OutputNode, OutputTransfer};
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

/// Runs `node` over linear `pixels` and reads the target back.
fn encode(
    gpu: &Gpu,
    size: (u32, u32),
    pixels: &[[f32; 4]],
    node: OutputNode,
    format: wgpu::TextureFormat,
) -> Vec<[f32; 4]> {
    let input = common::rgba16f(&gpu.device, &gpu.queue, size, pixels);
    let input_view = input.create_view(&Default::default());
    let target = common::blank(&gpu.device, format, size);
    let target_view = target.create_view(&Default::default());
    let mut g = Graph::new();
    let src = g.import("linear", &input_view);
    let out = g.add(node, &[src]);
    g.export(out.out(0), &target_view);
    let mut pool = TexturePool::new();
    g.compile()
        .expect("compiles")
        .execute(&gpu.renderer, &mut pool, None);
    common::read(&gpu.device, &gpu.queue, &target)
}

fn node(transfer: OutputTransfer, format: wgpu::TextureFormat, dither: Dither) -> OutputNode {
    OutputNode::new(WorkingSpace::BT709, Primaries::Bt709, transfer, format).with_dither(dither)
}

/// Grey ramp of `n` values from 0 to `max`, as f16 stores them.
fn ramp(n: u32, max: f32) -> Vec<[f32; 4]> {
    (0..n)
        .map(|i| {
            let v = common::f16_to_f32(common::f32_to_f16(max * i as f32 / (n - 1) as f32));
            [v, v, v, 1.0]
        })
        .collect()
}

#[test]
fn transfers_match_the_cpu_and_round_trip() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let cases = [
        (OutputTransfer::Srgb, 1.0, Some(TransferFn::Srgb)),
        (OutputTransfer::Bt1886, 1.0, Some(TransferFn::Bt1886)),
        (OutputTransfer::Bt709, 1.0, None),
        // 10,000 nits and 1000 nits, relative to 203.
        (OutputTransfer::Pq, 49.26, Some(TransferFn::Pq)),
        (OutputTransfer::Hlg, 4.926, Some(TransferFn::Hlg)),
    ];
    for (transfer, max, decode) in cases {
        let input = ramp(256, max);
        let out = encode(
            &gpu,
            (256, 1),
            &input,
            node(transfer, wgpu::TextureFormat::Rgba16Float, Dither::None),
            wgpu::TextureFormat::Rgba16Float,
        );
        let mut worst_encode = 0f64;
        let mut worst_trip = 0f64;
        for (px, inp) in out.iter().zip(&input) {
            let linear = f64::from(inp[0]);
            let want = transfer.encode_rgb([linear; 3])[0];
            worst_encode = worst_encode.max((f64::from(px[0]) - want).abs());
            if let Some(decode) = decode {
                let back = decode.to_linear_rgb([f64::from(px[0]); 3])[0];
                // Relative to the value (PQ and HLG span four decades), floored at 1e-3.
                let err = (back - linear).abs() / linear.max(1.0);
                worst_trip = worst_trip.max(err);
            }
        }
        println!(
            "{}: max encode error {worst_encode:.5}, max round-trip error {worst_trip:.5}",
            transfer.name()
        );
        // f16 holds encoded values near 1 to about 5e-4.
        assert!(worst_encode < 1.5e-3, "{transfer:?}: {worst_encode}");
        assert!(worst_trip < 5e-3, "{transfer:?}: {worst_trip}");
    }
}

#[test]
fn integer_outputs_round_to_the_nearest_code() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    for (format, levels) in [
        (wgpu::TextureFormat::Rgba8Unorm, 255.0),
        (wgpu::TextureFormat::Rgb10a2Unorm, 1023.0),
    ] {
        for transfer in [OutputTransfer::Srgb, OutputTransfer::Pq] {
            let max = if transfer == OutputTransfer::Pq {
                49.26
            } else {
                1.0
            };
            let input = ramp(512, max);
            let out = encode(
                &gpu,
                (512, 1),
                &input,
                node(transfer, format, Dither::None),
                format,
            );
            let worst = out
                .iter()
                .zip(&input)
                .map(|(px, inp)| {
                    let want = transfer.encode(f64::from(inp[0])) * levels;
                    (f64::from(px[0]) * levels - want).abs()
                })
                .fold(0f64, f64::max);
            println!("{format:?} {}: max {worst:.3} codes", transfer.name());
            assert!(worst <= 0.51, "{format:?} {transfer:?}: {worst} codes");
        }
    }
}

/// Longest run of identical codes along any row of `codes` (`w` wide).
fn longest_run(codes: &[u32], w: usize) -> usize {
    codes
        .chunks(w)
        .map(|row| {
            let (mut best, mut run) = (1, 1);
            for pair in row.windows(2) {
                run = if pair[0] == pair[1] { run + 1 } else { 1 };
                best = best.max(run);
            }
            best
        })
        .max()
        .unwrap_or(0)
}

/// A slow sRGB gradient spanning 3.2 code values of the target over 1024 pixels:
/// undithered, it shows as bands about 320 pixels wide. With dither, no run of identical
/// codes along a row may come near that step width, the dither must add no bias (mean
/// signed error), and the output blurred over 16 x 16 pixels (roughly what the eye
/// integrates at a normal viewing distance) must track the ideal gradient blurred the
/// same way far better than the undithered output does (RMS error in codes).
#[test]
fn dither_removes_banding_without_bias() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (w, h) = (1024u32, 64u32);
    for (format, levels) in [
        (wgpu::TextureFormat::Rgba8Unorm, 255.0),
        (wgpu::TextureFormat::Rgb10a2Unorm, 1023.0),
    ] {
        let start = 0.47;
        let span = 3.2 / levels;
        let linear =
            |x: u32| TransferFn::Srgb.to_linear(start + span * f64::from(x) / f64::from(w - 1));
        let mut pixels = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = common::f16_to_f32(common::f32_to_f16(linear(x) as f32));
                pixels.push([v, v, v, 1.0]);
            }
        }
        let ideal: Vec<f64> = pixels
            .iter()
            .map(|p| OutputTransfer::Srgb.encode(f64::from(p[0])) * levels)
            .collect();
        let codes = |out: &[[f32; 4]]| -> Vec<u32> {
            out.iter()
                .map(|p| (f64::from(p[0]) * levels).round() as u32)
                .collect()
        };
        let banding = |codes: &[u32]| {
            let block = 16usize;
            let mut sum = 0.0;
            let mut n = 0.0;
            for by in 0..(h as usize / block) {
                for bx in 0..(w as usize - block) {
                    let (mut a, mut b) = (0.0, 0.0);
                    for y in by * block..(by + 1) * block {
                        for x in bx..bx + block {
                            a += f64::from(codes[y * w as usize + x]);
                            b += ideal[y * w as usize + x];
                        }
                    }
                    let e = (a - b) / (block * block) as f64;
                    sum += e * e;
                    n += 1.0;
                }
            }
            (sum / n).sqrt()
        };
        let bias = |codes: &[u32]| {
            codes
                .iter()
                .zip(&ideal)
                .map(|(c, i)| f64::from(*c) - i)
                .sum::<f64>()
                / ideal.len() as f64
        };
        let plain = codes(&encode(
            &gpu,
            (w, h),
            &pixels,
            node(OutputTransfer::Srgb, format, Dither::None),
            format,
        ));
        let dithered = codes(&encode(
            &gpu,
            (w, h),
            &pixels,
            node(OutputTransfer::Srgb, format, Dither::Triangular).with_seed(7),
            format,
        ));
        let (step, run) = (
            longest_run(&plain, w as usize),
            longest_run(&dithered, w as usize),
        );
        let (b_plain, b_dither) = (banding(&plain), banding(&dithered));
        let (bias_plain, bias_dither) = (bias(&plain), bias(&dithered));
        println!(
            "{format:?}: longest run of one code undithered {step} px, dithered {run} px; \
             banding (RMS of 16x16 means) undithered {b_plain:.3}, dithered {b_dither:.3} codes; \
             mean error undithered {bias_plain:.4}, dithered {bias_dither:.4} codes"
        );
        assert!(
            step >= 250,
            "the test gradient should band: {step} px steps"
        );
        assert!(
            run * 4 < step,
            "dithered run of {run} px vs {step} px steps"
        );
        assert!(b_plain > 0.2, "the test gradient should band: {b_plain}");
        assert!(b_dither < 0.3 * b_plain, "{b_dither} vs {b_plain}");
        assert!(
            bias_dither.abs() < 0.02,
            "dither is biased by {bias_dither} codes"
        );

        // Exact black and white stay exact under dither.
        let flat: Vec<[f32; 4]> = (0..256)
            .map(|i| {
                if i < 128 {
                    [0.0, 0.0, 0.0, 1.0]
                } else {
                    [1.0; 4]
                }
            })
            .collect();
        let out = encode(
            &gpu,
            (256, 1),
            &flat,
            node(OutputTransfer::Srgb, format, Dither::Triangular),
            format,
        );
        for (i, px) in out.iter().enumerate() {
            let want = if i < 128 { 0.0 } else { 1.0 };
            assert_eq!(px[..3], [want; 3], "{format:?} pixel {i}");
        }
    }
}

/// PQ and HLG code values for known luminances, on the GPU, into a float target (so the
/// only error is the shader's f32 maths and the f16 store).
#[test]
fn hdr_spot_values() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let cases: [(OutputTransfer, f32, f64); 8] = [
        // ST 2084: 100 nits, 203 nits (BT.2408's 58%), 1000 and 10,000 nits.
        (OutputTransfer::Pq, 100.0 / 203.0, 0.508_08),
        (OutputTransfer::Pq, 1.0, 0.580_69),
        (OutputTransfer::Pq, 1000.0 / 203.0, 0.751_83),
        (OutputTransfer::Pq, 10_000.0 / 203.0, 1.0),
        // HLG with the 1000-nit OOTF: reference white at 75% (BT.2408), peak at 100%.
        (OutputTransfer::Hlg, 1.0, 0.75),
        (OutputTransfer::Hlg, 1000.0 / 203.0, 1.0),
        (OutputTransfer::Hlg, 0.0, 0.0),
        // Above the 1000-nit peak HLG clips.
        (OutputTransfer::Hlg, 10.0, 1.0),
    ];
    for (transfer, linear, want) in cases {
        let out = encode(
            &gpu,
            (4, 1),
            &[[linear, linear, linear, 1.0]; 4],
            OutputNode::new(
                WorkingSpace::BT2020,
                Primaries::Bt2020,
                transfer,
                wgpu::TextureFormat::Rgba16Float,
            ),
            wgpu::TextureFormat::Rgba16Float,
        );
        let got = f64::from(out[0][0]);
        println!(
            "{}: {linear:.4} -> {got:.5} (want {want:.5})",
            transfer.name()
        );
        // f16 steps are 4.9e-4 just below 1.
        assert!(
            (got - want).abs() < 1.5e-3,
            "{transfer:?} {linear}: {got} vs {want}"
        );
    }
}

/// A BT.2020 HDR working space delivered in BT.709 primaries: the gamut matrix runs
/// before the transfer, as the CPU reference does.
#[test]
fn output_primaries_are_converted_before_the_transfer() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let colour = [0.4f32, 0.2, 0.1, 1.0];
    let out = encode(
        &gpu,
        (4, 1),
        &[colour; 4],
        OutputNode::new(
            WorkingSpace::BT2020,
            Primaries::Bt709,
            OutputTransfer::Srgb,
            wgpu::TextureFormat::Rgba16Float,
        ),
        wgpu::TextureFormat::Rgba16Float,
    );
    let stored = colour.map(|v| f64::from(common::f16_to_f32(common::f32_to_f16(v))));
    let bt709 = render::colour::apply(
        &render::colour::gamut_matrix(Primaries::Bt2020, Primaries::Bt709),
        [stored[0], stored[1], stored[2]],
    );
    let want = OutputTransfer::Srgb.encode_rgb(bt709);
    for c in 0..3 {
        let got = f64::from(out[0][c]);
        assert!(
            (got - want[c]).abs() < 1.5e-3,
            "channel {c}: {got} vs {}",
            want[c]
        );
    }
}

/// GPU time of one 4K output pass (Rgba16Float working space to dithered 8-bit sRGB and
/// to 10-bit PQ, and a plain float copy for comparison), median of 30 frames, from the
/// pass timestamps.
#[test]
fn output_pass_at_4k_gpu_time() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut profiler = GpuProfiler::new(&gpu.device, &gpu.queue, 4);
    if !profiler.enabled() {
        eprintln!("skipped: no timestamp queries");
        return;
    }
    let (w, h) = (3840u32, 2160u32);
    let row: Vec<u8> = (0..w)
        .flat_map(|x| {
            let v = common::f32_to_f16(0.5 * x as f32 / w as f32).to_le_bytes();
            [v, v, v, common::f32_to_f16(1.0).to_le_bytes()]
        })
        .flatten()
        .collect();
    let data = row.repeat(h as usize);
    let input = common::texture(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba16Float,
        (w, h),
        &data,
    );
    let input_view = input.create_view(&Default::default());
    let mut pool = TexturePool::new();
    for (transfer, format) in [
        (OutputTransfer::Srgb, wgpu::TextureFormat::Rgba8Unorm),
        (OutputTransfer::Pq, wgpu::TextureFormat::Rgb10a2Unorm),
        // Baseline: no transfer, no dither, a float copy of the same size.
        (OutputTransfer::Linear, wgpu::TextureFormat::Rgba16Float),
    ] {
        let target = common::blank(&gpu.device, format, (w, h));
        let target_view = target.create_view(&Default::default());
        let mut times = Vec::new();
        for frame in 0..32u32 {
            let mut g = Graph::new();
            let src = g.import("linear", &input_view);
            let out = g.add(
                OutputNode::new(WorkingSpace::BT709, Primaries::Bt709, transfer, format)
                    .with_seed(frame),
                &[src],
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
        println!(
            "4K output pass, {} to {format:?}: median {median:.3} ms, max {max:.3} ms GPU ({} frames)",
            transfer.name(),
            times.len()
        );
        // Half of the 8 ms compositor budget, loose for a shared GPU.
        assert!(median < 4.0, "{median} ms");
    }
}

#[test]
fn straight_alpha_output_divides_out_alpha() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let px = [[0.25f32, 0.1, 0.0, 0.5]; 4];
    let out = encode(
        &gpu,
        (4, 1),
        &px,
        node(
            OutputTransfer::Linear,
            wgpu::TextureFormat::Rgba16Float,
            Dither::None,
        ),
        wgpu::TextureFormat::Rgba16Float,
    );
    assert_eq!(
        out[0],
        [0.5, 0.2, 0.0, 0.5].map(|v| common::f16_to_f32(common::f32_to_f16(v)))
    );
}
