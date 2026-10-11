//! The async readback ring: byte-exact round trips of every format with padded rows,
//! the packed RGBA8/RGBA16 conversions, submission order and tags, lossless back-pressure
//! under a slow consumer, drop-oldest for scopes, and that submitting never waits for the
//! GPU. `readback_4k_throughput_and_latency` prints the 4K numbers.

mod common;

use std::time::{Duration, Instant};

use render::readback::{
    Backpressure, ReadbackConfig, ReadbackError, ReadbackRing, SubmitError, f16_to_f32, to_rgba8,
    to_rgba16,
};
use render::wgpu;

const FORMATS: [wgpu::TextureFormat; 7] = [
    wgpu::TextureFormat::Rgba16Float,
    wgpu::TextureFormat::Rgba8Unorm,
    wgpu::TextureFormat::Rgba8UnormSrgb,
    wgpu::TextureFormat::Bgra8Unorm,
    wgpu::TextureFormat::Bgra8UnormSrgb,
    wgpu::TextureFormat::Rgb10a2Unorm,
    wgpu::TextureFormat::Rgba16Unorm,
];

fn supported(device: &wgpu::Device, format: wgpu::TextureFormat) -> bool {
    format != wgpu::TextureFormat::Rgba16Unorm
        || device
            .features()
            .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
}

/// A deterministic pattern of `len` bytes; for `Rgba16Float` every value is a finite
/// half so no NaN payload has to survive the copy.
fn pattern(format: wgpu::TextureFormat, len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    if format == wgpu::TextureFormat::Rgba16Float {
        (0..len / 2)
            .flat_map(|_| {
                let v = next();
                // Exponent 0..=29 keeps it finite.
                let half = ((v & 0x8000) | ((v >> 16) % 30) << 10 | (v & 0x3ff)) as u16;
                half.to_le_bytes()
            })
            .collect()
    } else {
        (0..len).map(|_| next() as u8).collect()
    }
}

fn bpp(format: wgpu::TextureFormat) -> u32 {
    render::readback::bytes_per_pixel(format).expect("supported")
}

fn filled(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    (w, h): (u32, u32),
    seed: u32,
) -> (wgpu::Texture, Vec<u8>) {
    let data = pattern(format, (w * h * bpp(format)) as usize, seed);
    // Copy usages only: not every format can be a render attachment (Rgba16Unorm).
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * bpp(format)),
            rows_per_image: Some(h),
        },
        texture.size(),
    );
    (texture, data)
}

#[test]
fn every_format_round_trips_with_padded_rows() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // 37 px rows need padding in every format, 64 px Rgba8 rows are exactly 256 bytes.
    for size in [(37, 19), (64, 4), (1, 1)] {
        for (seed, format) in FORMATS.into_iter().enumerate() {
            if !supported(&device, format) {
                eprintln!("{format:?}: skipped, no 16-bit normalised textures");
                continue;
            }
            let (texture, data) = filled(&device, &queue, format, size, seed as u32);
            let mut ring = ReadbackRing::new(ReadbackConfig::EXPORT);
            let id = ring.submit(&device, &queue, &texture, 42).expect("submit");
            let frame = ring.wait(&device).expect("mapped").expect("a frame");
            assert_eq!((frame.id, frame.timestamp), (id, 42));
            assert_eq!(
                (frame.width, frame.height, frame.format),
                (size.0, size.1, format)
            );
            assert!(frame.data == data, "{format:?} {size:?}: bytes differ");

            ring.submit(&device, &queue, &texture, 43).expect("submit");
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            let rows = ring
                .poll_with(&device, |view| {
                    assert_eq!(view.bytes_per_row % 256, 0);
                    (0..view.height).all(|y| {
                        let row = (size.0 * bpp(format)) as usize;
                        view.row(y) == &data[y as usize * row..(y as usize + 1) * row]
                    })
                })
                .expect("mapped")
                .expect("a frame");
            assert!(rows, "{format:?} {size:?}: in-place rows differ");
            assert_eq!(frame.to_rgba8().len(), (size.0 * size.1 * 4) as usize);
        }
    }
}

#[test]
fn conversions_to_packed_rgba() {
    use wgpu::TextureFormat as F;
    let mut out = Vec::new();
    to_rgba8(F::Bgra8Unorm, &[1, 2, 3, 4], &mut out);
    assert_eq!(out, [3, 2, 1, 4]);

    let ten = |r: u32, g: u32, b: u32, a: u32| (r | g << 10 | b << 20 | a << 30).to_le_bytes();
    out.clear();
    to_rgba8(F::Rgb10a2Unorm, &ten(1023, 0, 512, 3), &mut out);
    to_rgba8(F::Rgb10a2Unorm, &ten(2, 1021, 4, 1), &mut out);
    assert_eq!(out, [255, 0, 128, 255, 0, 255, 1, 85]);
    let mut wide = Vec::new();
    to_rgba16(F::Rgb10a2Unorm, &ten(1023, 0, 512, 2), &mut wide);
    assert_eq!(wide, [65535, 0, 32800, 43690]);

    wide.clear();
    to_rgba16(F::Rgba8Unorm, &[0, 1, 128, 255], &mut wide);
    assert_eq!(wide, [0, 257, 32896, 65535]);

    let halves: Vec<u8> = [
        0x3c00u16, 0x3800, 0xc000, 0x7e00, 0x4000, 0x0000, 0x2e66, 0x7c00,
    ]
    .iter()
    .flat_map(|h| h.to_le_bytes())
    .collect();
    out.clear();
    to_rgba8(F::Rgba16Float, &halves, &mut out);
    // 1, 0.5, -2, NaN, 2, 0, 0.1 (0.0999756), +inf
    assert_eq!(out, [255, 128, 0, 0, 255, 0, 25, 255]);
    wide.clear();
    to_rgba16(F::Rgba16Float, &halves, &mut wide);
    assert_eq!(wide, [65535, 32768, 0, 0, 65535, 0, 6552, 65535]);

    let sixteen: Vec<u8> = [0u16, 128, 32896, 65535]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    out.clear();
    to_rgba8(F::Rgba16Unorm, &sixteen, &mut out);
    assert_eq!(out, [0, 0, 128, 255]);

    assert_eq!(f16_to_f32(0x3c00), 1.0);
    assert_eq!(f16_to_f32(0xc000), -2.0);
    assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
    assert_eq!(f16_to_f32(0x8001), -(2f32.powi(-24)));
    assert_eq!(f16_to_f32(0x7bff), 65504.0);
    assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
    assert!(f16_to_f32(0x7e00).is_nan());
    for h in (0..=u16::MAX).filter(|h| h & 0x7c00 != 0x7c00) {
        assert_eq!(f16_to_f32(h), common::f16_to_f32(h), "{h:#06x}");
    }
}

#[test]
fn frames_arrive_in_submission_order_with_their_tags() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let sources: Vec<_> = (0..12)
        .map(|i| filled(&device, &queue, format, (70, 9), i))
        .collect();
    let mut ring = ReadbackRing::new(ReadbackConfig {
        slots: 3,
        policy: Backpressure::Lossless,
    });
    let mut received = Vec::new();
    for (i, (texture, _)) in sources.iter().enumerate() {
        let timestamp = 1000 + i as u64 * 40;
        loop {
            match ring.submit(&device, &queue, texture, timestamp) {
                Ok(id) => {
                    assert_eq!(id, i as u64);
                    break;
                }
                Err(SubmitError::Full) => {
                    received.push(ring.wait(&device).expect("mapped").expect("a frame"))
                }
                Err(e) => panic!("{e}"),
            }
        }
        while let Some(frame) = ring.poll(&device).expect("mapped") {
            received.push(frame);
        }
    }
    while let Some(frame) = ring.wait(&device).expect("mapped") {
        received.push(frame);
    }
    assert_eq!(received.len(), sources.len());
    for (i, frame) in received.iter().enumerate() {
        assert_eq!(frame.id, i as u64);
        assert_eq!(frame.timestamp, 1000 + i as u64 * 40);
        assert!(
            frame.data == sources[i].1,
            "frame {i} has another frame's pixels"
        );
    }
    assert!(
        received
            .windows(2)
            .all(|w| w[0].submitted <= w[1].submitted)
    );
}

#[test]
fn lossless_never_drops_under_a_slow_consumer() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba16Float;
    let sources: Vec<_> = (0..6)
        .map(|i| filled(&device, &queue, format, (256, 64), i))
        .collect();
    let mut ring = ReadbackRing::new(ReadbackConfig::EXPORT);
    let total = 60u64;
    let mut next = 0u64;
    let mut received = Vec::new();
    let mut tick = 0;
    while received.len() < total as usize {
        tick += 1;
        if next < total {
            let texture = &sources[(next % 6) as usize].0;
            match ring.submit(&device, &queue, texture, next) {
                Ok(_) => next += 1,
                Err(SubmitError::Full) => {}
                Err(e) => panic!("{e}"),
            }
        }
        // The consumer takes one frame every third tick, slowly.
        if tick % 3 == 0 || next == total {
            std::thread::sleep(Duration::from_millis(2));
            if let Some(frame) = ring.poll(&device).expect("mapped") {
                received.push(frame);
            }
        }
    }
    let stats = ring.stats();
    assert_eq!(stats.dropped, 0);
    assert_eq!(stats.delivered, total);
    assert!(
        stats.refused > 0,
        "the consumer was never behind: {stats:?}"
    );
    for (i, frame) in received.iter().enumerate() {
        assert_eq!((frame.id, frame.timestamp), (i as u64, i as u64));
        assert!(frame.data == sources[i % 6].1, "frame {i}");
    }
    println!(
        "lossless, slow consumer: {total} delivered, {} submits refused, 0 dropped",
        stats.refused
    );
}

#[test]
fn drop_oldest_keeps_the_newest_frames() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let sources: Vec<_> = (0..10)
        .map(|i| filled(&device, &queue, format, (40, 8), i))
        .collect();
    let mut ring = ReadbackRing::new(ReadbackConfig::SCOPES);
    assert_eq!(ring.policy(), Backpressure::DropOldest);
    for (i, (texture, _)) in sources.iter().enumerate() {
        ring.submit(&device, &queue, texture, i as u64)
            .expect("an evictable slot");
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    }
    let mut kept = Vec::new();
    while let Some(frame) = ring.poll(&device).expect("mapped") {
        kept.push(frame);
    }
    let timestamps: Vec<_> = kept.iter().map(|f| f.timestamp).collect();
    assert_eq!(timestamps, [7, 8, 9]);
    for frame in &kept {
        assert!(frame.data == sources[frame.timestamp as usize].1);
    }
    assert_eq!(ring.stats().dropped, 7);
    assert_eq!(ring.stats().refused, 0);
}

#[test]
fn drop_oldest_drops_the_new_frame_when_nothing_has_finished() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (texture, _) = filled(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        (16, 16),
        1,
    );
    let mut ring = ReadbackRing::new(ReadbackConfig {
        slots: 2,
        policy: Backpressure::DropOldest,
    });
    // Recorded but not submitted yet, so neither can have finished.
    let mut encoder = device.create_command_encoder(&Default::default());
    ring.record(&device, &mut encoder, &texture, 0)
        .expect("slot 0");
    ring.record(&device, &mut encoder, &texture, 1)
        .expect("slot 1");
    assert_eq!(
        ring.record(&device, &mut encoder, &texture, 2),
        Err(SubmitError::Full)
    );
    assert_eq!(ring.stats().dropped, 1);
    queue.submit([encoder.finish()]);
    let first = ring.wait(&device).expect("mapped").expect("frame 0");
    let second = ring.wait(&device).expect("mapped").expect("frame 1");
    assert_eq!((first.timestamp, second.timestamp), (0, 1));
}

#[test]
fn rejects_what_it_cannot_read() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut ring = ReadbackRing::new(ReadbackConfig::EXPORT);
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    assert_eq!(
        ring.submit(&device, &queue, &depth, 0),
        Err(SubmitError::UnsupportedFormat(
            wgpu::TextureFormat::R32Float
        ))
    );
    let sampled = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: depth.size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    assert_eq!(
        ring.submit(&device, &queue, &sampled, 0),
        Err(SubmitError::NotCopySource)
    );

    let (texture, _) = filled(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, (8, 8), 3);
    let mut encoder = device.create_command_encoder(&Default::default());
    let id = ring
        .record(&device, &mut encoder, &texture, 0)
        .expect("slot");
    drop(encoder);
    assert_eq!(
        ring.wait(&device).err(),
        Some(ReadbackError::NotSubmitted { id })
    );
    assert_eq!(ring.in_flight(), 0);
    ring.submit(&device, &queue, &texture, 1)
        .expect("the slot is free again");
    assert!(ring.wait(&device).expect("mapped").is_some());
}

const UHD: (u32, u32) = (3840, 2160);

/// Clears `texture` to a colour that changes per frame, standing in for a rendered frame.
fn render_into(encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture, frame: u64) {
    let view = texture.create_view(&Default::default());
    let v = (frame % 7) as f64 / 7.0;
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("stand-in frame"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: v,
                    g: 0.5,
                    b: 1.0 - v,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).round() as usize]
}

/// Record and submit are timed on the wall clock and the thread's CPU clock and held to
/// the frame budget by the shared timing rules (`common::timing`).
#[test]
fn submit_never_waits_for_the_gpu() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let _serial = common::timing::serial();
    let format = wgpu::TextureFormat::Rgba16Float;
    let texture = common::blank(&device, format, UHD);
    let mut ring = ReadbackRing::new(ReadbackConfig::EXPORT);
    ring.reserve(&device, UHD.0, UHD.1, format)
        .expect("reserve");

    // What a blocking readback costs: submit, then wait for the frame.
    let mut blocking = Vec::new();
    for i in 0..5 {
        let start = Instant::now();
        ring.submit(&device, &queue, &texture, i)
            .expect("free slot");
        ring.wait(&device).expect("mapped").expect("a frame");
        blocking.push(start.elapsed().as_secs_f64() * 1e3);
    }

    // Submits against a consumer that never keeps up: accepted or refused, never waiting.
    let mut submits = common::timing::Laps::default();
    let mut refused = 0;
    for i in 0..40 {
        let mut encoder = device.create_command_encoder(&Default::default());
        render_into(&mut encoder, &texture, i);
        let (result, lap) = common::timing::time(|| {
            let result = ring.record(&device, &mut encoder, &texture, i);
            queue.submit([encoder.finish()]);
            result
        });
        submits.push(lap);
        match result {
            Ok(_) => {}
            Err(SubmitError::Full) => refused += 1,
            Err(e) => panic!("{e}"),
        }
        if i % 8 == 7 {
            // A consumer that shows up now and then, taking one frame.
            let _ = ring.wait(&device);
        }
    }
    let blocking_median = percentile(&mut blocking, 0.5);
    let median = submits.wall().median();
    println!(
        "4K Rgba16Float: record + submit ({refused} of 40 refused) wall {}, CPU {}; blocking \
         readback median {blocking_median:.2} ms",
        submits.wall(),
        submits.cpu()
    );
    assert!(refused > 0);
    submits.assert_within("record + submit", common::timing::Stat::Max, 8.0);
    assert!(
        median < blocking_median / 2.0,
        "submit median {median:.2} ms vs blocking {blocking_median:.2} ms"
    );
}

#[test]
fn readback_4k_throughput_and_latency() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let _serial = common::timing::serial();
    for format in [
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureFormat::Rgba8Unorm,
    ] {
        for copy_out in [true, false] {
            let texture = common::blank(&device, format, UHD);
            let mut ring = ReadbackRing::new(ReadbackConfig::EXPORT);
            ring.reserve(&device, UHD.0, UHD.1, format)
                .expect("reserve");
            let frames = 60u64;
            let mut submitted = 0u64;
            let mut latency_frames = Vec::new();
            let mut latency_ms = Vec::new();
            let mut take = |ring: &mut ReadbackRing, submitted: u64, wait: bool| loop {
                let got = if copy_out {
                    ring.poll(&device).expect("mapped").map(|frame| {
                        let meta = (frame.id, frame.submitted);
                        ring.recycle(frame);
                        meta
                    })
                } else {
                    ring.poll_with(&device, |view| {
                        std::hint::black_box(view.data[view.data.len() - 1]);
                        (view.id, view.submitted)
                    })
                    .expect("mapped")
                };
                if let Some((id, at)) = got {
                    latency_frames.push((submitted - id - 1) as f64);
                    latency_ms.push(at.elapsed().as_secs_f64() * 1e3);
                    return true;
                }
                if !wait || ring.in_flight() == 0 {
                    return false;
                }
                let _ = device.poll(wgpu::PollType::wait_indefinitely());
            };
            let start = Instant::now();
            while submitted < frames {
                let mut encoder = device.create_command_encoder(&Default::default());
                render_into(&mut encoder, &texture, submitted);
                match ring.record(&device, &mut encoder, &texture, submitted) {
                    Ok(_) => {
                        queue.submit([encoder.finish()]);
                        submitted += 1;
                    }
                    Err(SubmitError::Full) => {
                        take(&mut ring, submitted, true);
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                }
                while take(&mut ring, submitted, false) {}
            }
            while ring.in_flight() > 0 {
                take(&mut ring, submitted, true);
            }
            let seconds = start.elapsed().as_secs_f64();
            let bytes =
                f64::from(UHD.0 * UHD.1 * render::readback::bytes_per_pixel(format).unwrap());
            let fps = frames as f64 / seconds;
            println!(
                "4K {format:?} {}: {fps:.1} fps, {:.2} GB/s, latency median {:.0} frames / \
                 {:.1} ms, p95 {:.0} frames / {:.1} ms",
                if copy_out { "copied out" } else { "in place" },
                fps * bytes / 1e9,
                percentile(&mut latency_frames.clone(), 0.5),
                percentile(&mut latency_ms.clone(), 0.5),
                percentile(&mut latency_frames, 0.95),
                percentile(&mut latency_ms, 0.95),
            );
            assert_eq!(ring.stats().delivered, frames);
        }
    }
}
