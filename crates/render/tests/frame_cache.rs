//! The rendered-frame cache: LRU order across VRAM and RAM, byte ceilings (also when they
//! change at runtime), bit-exact spill and re-upload round trips in both spill formats,
//! invalidation by segment (spills in flight included), clear, and the stats.
//! `frame_cache_4k_costs` prints insert and lookup costs and the 4K spill and re-upload
//! times.

mod common;

use std::time::{Duration, Instant};

use render::frame_cache::{
    FrameCache, FrameCacheConfig, FrameKey, RenderVariant, SegmentHash, SpillFormat, Tier,
    srgb_encode,
};
use render::readback::{ReadbackConfig, ReadbackRing, f16_to_f32};
use render::wgpu;

const F16: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

fn key(segment: u64, frame: u64) -> FrameKey {
    FrameKey {
        segment: SegmentHash::from(segment),
        frame,
        variant: RenderVariant {
            width: 8,
            height: 8,
            quality: 0,
            settings: 0,
        },
    }
}

fn config(vram: u64, ram: u64, spill_format: SpillFormat) -> FrameCacheConfig {
    FrameCacheConfig {
        vram_ceiling: vram,
        ram_ceiling: ram,
        spill_format,
        spill_slots: 16,
    }
}

/// Random finite f16 texels (values far above 1 and negative included), row-major.
fn f16_texels(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453) | 1;
    let mut data = Vec::with_capacity(len * 8);
    for _ in 0..len * 4 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let mut bits = (state >> 7) as u16;
        if bits & 0x7c00 == 0x7c00 {
            bits &= !0x4000;
        }
        data.extend_from_slice(&bits.to_le_bytes());
    }
    data
}

fn frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    (w, h): (u32, u32),
    seed: u32,
) -> wgpu::Texture {
    common::texture(
        device,
        queue,
        F16,
        (w, h),
        &f16_texels((w * h) as usize, seed),
    )
}

/// 8 x 8 `Rgba16Float`: 512 bytes.
const SMALL: u64 = 8 * 8 * 8;

fn small(device: &wgpu::Device, queue: &wgpu::Queue, seed: u32) -> wgpu::Texture {
    frame(device, queue, (8, 8), seed)
}

/// The texture's texels, packed, exactly as stored.
fn bytes_of(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let mut ring = ReadbackRing::new(ReadbackConfig::EXPORT);
    ring.submit(device, queue, texture, 0).expect("readback");
    ring.wait(device).expect("mapped").expect("a frame").data
}

fn tier(cache: &FrameCache, segment: u64, frame: u64) -> Option<Tier> {
    cache.contains(&key(segment, frame))
}

#[test]
fn lru_order_across_both_tiers() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut cache = FrameCache::new(config(3 * SMALL, 100 * SMALL, SpillFormat::Native));
    for i in 0..3 {
        cache.insert(&device, &queue, key(1, i), small(&device, &queue, i as u32));
    }
    assert!(cache.lookup(&device, &queue, &key(1, 0)).is_some());
    cache.insert(&device, &queue, key(1, 3), small(&device, &queue, 3));
    cache.finish_spills(&device);
    // Frame 0 was used after frame 1, so frame 1 is the one that spilled.
    assert_eq!(tier(&cache, 1, 1), Some(Tier::Ram));
    for i in [0, 2, 3] {
        assert_eq!(tier(&cache, 1, i), Some(Tier::Vram), "frame {i}");
    }
    cache.insert(&device, &queue, key(1, 4), small(&device, &queue, 4));
    cache.finish_spills(&device);
    assert_eq!(tier(&cache, 1, 2), Some(Tier::Ram));

    // RAM keeps the global order: one frame of VRAM, two of RAM.
    let mut cache = FrameCache::new(config(SMALL, 2 * SMALL, SpillFormat::Native));
    for i in 0..3 {
        cache.insert(&device, &queue, key(2, i), small(&device, &queue, i as u32));
        cache.finish_spills(&device);
    }
    assert_eq!(tier(&cache, 2, 0), Some(Tier::Ram));
    assert_eq!(tier(&cache, 2, 1), Some(Tier::Ram));
    assert_eq!(tier(&cache, 2, 2), Some(Tier::Vram));
    // A RAM hit promotes a and pushes c down; RAM now holds b and c.
    let hit = cache.lookup(&device, &queue, &key(2, 0)).expect("hit");
    assert_eq!(hit.from, Tier::Ram);
    cache.finish_spills(&device);
    assert_eq!(tier(&cache, 2, 0), Some(Tier::Vram));
    assert_eq!(tier(&cache, 2, 2), Some(Tier::Ram));
    // d pushes a down; b is the oldest in RAM and is dropped.
    cache.insert(&device, &queue, key(2, 3), small(&device, &queue, 3));
    cache.finish_spills(&device);
    assert_eq!(tier(&cache, 2, 1), None);
    assert_eq!(tier(&cache, 2, 2), Some(Tier::Ram));
    assert_eq!(tier(&cache, 2, 0), Some(Tier::Ram));
    assert_eq!(tier(&cache, 2, 3), Some(Tier::Vram));
    assert_eq!(cache.stats().ram_evictions, 1);
}

#[test]
fn ceilings_hold_in_bytes() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let sizes = [(8, 8), (16, 8), (4, 4), (13, 7), (32, 16)];
    let mut cache = FrameCache::new(config(6 * SMALL, 5 * SMALL, SpillFormat::Native));
    let mut state = 12_345u32;
    let mut next = |n: u32| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state % n
    };
    for step in 0..300u32 {
        match next(10) {
            0..=4 => {
                let size = sizes[next(sizes.len() as u32) as usize];
                let format = if next(3) == 0 {
                    wgpu::TextureFormat::Rgba8Unorm
                } else {
                    F16
                };
                let texture = common::blank(&device, format, size);
                cache.insert(
                    &device,
                    &queue,
                    key(u64::from(next(3)), u64::from(next(40))),
                    texture,
                );
            }
            5..=6 => {
                let _ = cache.lookup(
                    &device,
                    &queue,
                    &key(u64::from(next(3)), u64::from(next(40))),
                );
            }
            7 => cache.maintain(&device),
            8 => {
                let bytes = u64::from(next(10)) * SMALL;
                cache.set_vram_ceiling(&device, &queue, bytes);
            }
            _ => cache.set_ram_ceiling(u64::from(next(10)) * SMALL),
        }
        if step % 7 == 0 {
            cache.finish_spills(&device);
        }
        let stats = cache.stats();
        assert!(
            stats.vram_bytes <= stats.vram_ceiling,
            "step {step}: VRAM {} > {}",
            stats.vram_bytes,
            stats.vram_ceiling
        );
        assert!(
            stats.ram_bytes <= stats.ram_ceiling,
            "step {step}: RAM {} > {}",
            stats.ram_bytes,
            stats.ram_ceiling
        );
        assert_eq!(stats.vram_frames + stats.ram_frames, cache.keys().count());
    }
    let stats = cache.stats();
    assert!(
        stats.spilled > 0 && stats.ram_evictions > 0 && stats.ram_hits > 0,
        "{stats:?}"
    );

    // A frame larger than the VRAM ceiling goes straight to RAM; larger than both, it is
    // dropped.
    let mut cache = FrameCache::new(config(SMALL, 4 * SMALL, SpillFormat::Native));
    cache.insert(
        &device,
        &queue,
        key(0, 0),
        frame(&device, &queue, (16, 8), 0),
    );
    cache.insert(
        &device,
        &queue,
        key(0, 1),
        frame(&device, &queue, (32, 16), 1),
    );
    cache.finish_spills(&device);
    assert_eq!(tier(&cache, 0, 0), Some(Tier::Ram));
    assert_eq!(tier(&cache, 0, 1), None);
    assert_eq!(cache.stats().spill_dropped, 1);
}

#[test]
fn native_spill_and_reupload_are_bit_exact() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // An odd width, so staging rows are padded.
    let size = (61, 17);
    let texels = f16_texels(61 * 17, 7);
    let texture = common::texture(&device, &queue, F16, size, &texels);
    let bytes = 61 * 17 * 8;
    let mut cache = FrameCache::new(config(bytes, 4 * bytes, SpillFormat::Native));
    cache.insert(&device, &queue, key(0, 0), texture);
    cache.insert(&device, &queue, key(0, 1), frame(&device, &queue, size, 8));
    cache.finish_spills(&device);
    assert_eq!(tier(&cache, 0, 0), Some(Tier::Ram));
    assert_eq!(cache.stats().ram_bytes, bytes);

    for round in 0..2 {
        let hit = cache.lookup(&device, &queue, &key(0, 0)).expect("hit");
        assert_eq!(hit.from, Tier::Ram);
        assert_eq!(hit.texture.format(), F16);
        let upload = hit.upload.expect("an upload cost");
        assert_eq!(upload.bytes, bytes);
        assert!(
            bytes_of(&device, &queue, &hit.texture) == texels,
            "round {round}"
        );
        cache.finish_spills(&device);
        // Push it down again: a re-uploaded frame spills just as exactly.
        cache.lookup(&device, &queue, &key(0, 1)).expect("hit");
        cache.finish_spills(&device);
        assert_eq!(tier(&cache, 0, 0), Some(Tier::Ram));
    }
}

#[test]
fn srgb8_spill_stores_encoded_bytes_and_reuploads_them_exactly() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let size = (37u32, 9u32);
    let n = (size.0 * size.1) as usize;
    // Linear values from -0.1 to 1.3, NaN-free.
    let pixels: Vec<[f32; 4]> = (0..n)
        .map(|i| {
            let t = i as f32 / n as f32;
            [
                t * 1.4 - 0.1,
                (t * 7.0).fract(),
                (1.0 - t) * 1.2,
                (t * 3.0).fract(),
            ]
        })
        .collect();
    let texture = common::rgba16f(&device, &queue, size, &pixels);
    let original = bytes_of(&device, &queue, &texture);
    let expected: Vec<u8> = original
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let v = f16_to_f32(u16::from_le_bytes(*c)).clamp(0.0, 1.0);
            let v = if i % 4 == 3 { v } else { srgb_encode(v) };
            (v * 255.0).round() as u8
        })
        .collect();

    let f16_bytes = u64::from(size.0 * size.1) * 8;
    let mut cache = FrameCache::new(config(f16_bytes, 4 * f16_bytes, SpillFormat::Srgb8));
    cache.insert(&device, &queue, key(0, 0), texture);
    cache.insert(&device, &queue, key(0, 1), frame(&device, &queue, size, 1));
    cache.finish_spills(&device);
    assert_eq!(cache.stats().ram_bytes, f16_bytes / 2, "half the bytes");

    let hit = cache.lookup(&device, &queue, &key(0, 0)).expect("hit");
    assert_eq!(hit.texture.format(), wgpu::TextureFormat::Rgba8UnormSrgb);
    let stored = bytes_of(&device, &queue, &hit.texture);
    assert!(stored == expected, "stored bytes are the sRGB encoding");
    cache.finish_spills(&device);
    // Spilling the re-uploaded 8-bit frame again keeps the same bytes.
    cache.lookup(&device, &queue, &key(0, 1)).expect("hit");
    cache.finish_spills(&device);
    let hit = cache.lookup(&device, &queue, &key(0, 0)).expect("hit");
    assert!(bytes_of(&device, &queue, &hit.texture) == expected);

    // 8-bit frames are kept as they are in this mode.
    let data: Vec<u8> = (0..16 * 16 * 4).map(|i| (i * 37 % 256) as u8).collect();
    let texture = common::texture(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        (16, 16),
        &data,
    );
    let mut cache = FrameCache::new(config(1024, 4096, SpillFormat::Srgb8));
    cache.insert(&device, &queue, key(0, 0), texture);
    cache.insert(
        &device,
        &queue,
        key(0, 1),
        common::blank(&device, wgpu::TextureFormat::Rgba8Unorm, (16, 16)),
    );
    cache.finish_spills(&device);
    let hit = cache.lookup(&device, &queue, &key(0, 0)).expect("hit");
    assert_eq!(hit.from, Tier::Ram);
    assert_eq!(hit.texture.format(), wgpu::TextureFormat::Rgba8Unorm);
    assert!(bytes_of(&device, &queue, &hit.texture) == data);
}

#[test]
fn invalidation_drops_a_segment_from_every_tier() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut cache = FrameCache::new(config(2 * SMALL, 100 * SMALL, SpillFormat::Native));
    for i in 0..4u64 {
        cache.insert(
            &device,
            &queue,
            key(10, i),
            small(&device, &queue, i as u32),
        );
        cache.insert(
            &device,
            &queue,
            key(20, i),
            small(&device, &queue, 100 + i as u32),
        );
    }
    cache.finish_spills(&device);
    // One more insert leaves a spill of segment 10 in flight.
    cache.insert(&device, &queue, key(20, 4), small(&device, &queue, 9));
    assert!(cache.stats().spilling > 0);
    let resident = cache
        .keys()
        .filter(|(k, _)| k.segment == SegmentHash::from(10))
        .count();
    assert_eq!(cache.invalidate_segment(SegmentHash::from(10)), resident);
    cache.finish_spills(&device);
    assert!(
        cache
            .keys()
            .all(|(k, _)| k.segment == SegmentHash::from(20))
    );
    for i in 0..5 {
        assert!(tier(&cache, 20, i).is_some(), "segment 20 frame {i}");
    }
    assert!(cache.lookup(&device, &queue, &key(10, 3)).is_none());

    cache.clear();
    cache.finish_spills(&device);
    let stats = cache.stats();
    assert_eq!(cache.keys().count(), 0);
    assert_eq!((stats.vram_bytes, stats.ram_bytes), (0, 0));
    assert_eq!(stats.invalidated, resident as u64 + 5);
}

#[test]
fn stats_count_what_happened() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut cache = FrameCache::new(config(2 * SMALL, SMALL, SpillFormat::Native));
    for i in 0..3 {
        cache.insert(&device, &queue, key(0, i), small(&device, &queue, i as u32));
    }
    cache.finish_spills(&device);
    let stats = cache.stats();
    assert_eq!(stats.inserts, 3);
    assert_eq!(
        (stats.vram_evictions, stats.spilled, stats.ram_evictions),
        (1, 1, 0)
    );
    assert_eq!((stats.vram_frames, stats.ram_frames), (2, 1));
    assert_eq!((stats.vram_bytes, stats.ram_bytes), (2 * SMALL, SMALL));
    assert_eq!((stats.vram_ceiling, stats.ram_ceiling), (2 * SMALL, SMALL));

    assert_eq!(
        cache.lookup(&device, &queue, &key(0, 2)).expect("hit").from,
        Tier::Vram
    );
    assert!(cache.lookup(&device, &queue, &key(0, 9)).is_none());
    // RAM hit on frame 0: frame 1 spills, and RAM (one frame) makes room for it.
    assert_eq!(
        cache.lookup(&device, &queue, &key(0, 0)).expect("hit").from,
        Tier::Ram
    );
    cache.finish_spills(&device);
    let stats = cache.stats();
    assert_eq!((stats.vram_hits, stats.ram_hits, stats.misses), (1, 1, 1));
    assert_eq!(stats.uploaded_bytes, SMALL);
    assert_eq!(
        (stats.vram_evictions, stats.spilled, stats.ram_evictions),
        (2, 2, 0)
    );
    assert_eq!(tier(&cache, 0, 1), Some(Tier::Ram));
    assert!((stats.hit_rate() - 2.0 / 3.0).abs() < 1e-9);

    // No COPY_SRC: evicting it cannot spill.
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 8,
            height: 8,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: F16,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    cache.insert(&device, &queue, key(1, 0), texture);
    cache.insert(&device, &queue, key(1, 1), small(&device, &queue, 5));
    cache.insert(&device, &queue, key(1, 2), small(&device, &queue, 6));
    cache.finish_spills(&device);
    let stats = cache.stats();
    assert_eq!(tier(&cache, 1, 0), None);
    assert!(stats.spill_dropped >= 1, "{stats:?}");
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

#[test]
fn frame_cache_4k_costs() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // Bookkeeping: insert and VRAM lookup of 2000 small frames.
    let textures: Vec<_> = (0..2000)
        .map(|_| common::blank(&device, F16, (4, 4)))
        .collect();
    let mut cache = FrameCache::new(config(u64::MAX / 2, 0, SpillFormat::Native));
    let start = Instant::now();
    for (i, texture) in textures.into_iter().enumerate() {
        cache.insert(&device, &queue, key(0, i as u64), texture);
    }
    let insert_us = start.elapsed().as_secs_f64() * 1e6 / 2000.0;
    let start = Instant::now();
    for i in 0..2000 {
        std::hint::black_box(cache.lookup(&device, &queue, &key(0, (i * 7919) % 2000)));
    }
    let lookup_us = start.elapsed().as_secs_f64() * 1e6 / 2000.0;
    println!("insert {insert_us:.2} us, VRAM lookup {lookup_us:.2} us (2000 frames resident)");

    let frame_bytes = 3840 * 2160 * 8;
    for format in [SpillFormat::Native, SpillFormat::Srgb8] {
        // VRAM holds one frame as re-uploaded (8-bit after an `Srgb8` spill).
        let vram = match format {
            SpillFormat::Native => frame_bytes,
            SpillFormat::Srgb8 => frame_bytes / 2,
        };
        let mut cache = FrameCache::new(config(vram, 4 * frame_bytes, format));
        cache.insert(
            &device,
            &queue,
            key(0, 0),
            common::noise_4k(&device, &queue, 1),
        );
        cache.insert(
            &device,
            &queue,
            key(0, 1),
            common::noise_4k(&device, &queue, 2),
        );
        cache.finish_spills(&device);
        let (mut record, mut copy, mut pack, mut upload, mut upload_gpu) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for round in 0..8u64 {
            // Each RAM hit re-uploads one frame and spills the other.
            let start = Instant::now();
            let hit = cache
                .lookup(&device, &queue, &key(0, round % 2))
                .expect("hit");
            let lookup = start.elapsed();
            let cpu = hit.upload.expect("RAM hit").cpu;
            upload.push(cpu.as_secs_f64() * 1e3);
            record.push((lookup - cpu).as_secs_f64() * 1e3);
            let start = Instant::now();
            queue.submit([]);
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            // Upload and spill copy share the submission; this is both on the GPU.
            copy.push(start.elapsed().as_secs_f64() * 1e3);
            upload_gpu.push(copy[copy.len() - 1]);
            let start = Instant::now();
            cache.maintain(&device);
            pack.push(start.elapsed().as_secs_f64() * 1e3);
            assert_eq!(cache.stats().spilling, 0);
        }
        let stats = cache.stats();
        println!(
            "4K {format:?}: re-upload CPU {:.2} ms, spill record + submit {:.2} ms, \
             GPU upload + spill copy {:.2} ms, spill pack into RAM {:.2} ms \
             ({} bytes in RAM per frame; mean upload {:.2} ms)",
            median(&mut upload),
            median(&mut record),
            median(&mut upload_gpu),
            median(&mut pack),
            stats.ram_bytes,
            stats.upload_time.as_secs_f64() * 1e3 / stats.ram_hits as f64,
        );
        assert!(stats.upload_time > Duration::ZERO);
    }
}
