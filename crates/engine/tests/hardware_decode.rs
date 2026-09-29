//! End to end on macOS: VideoToolbox decode in `media`, zero-copy IOSurface import and
//! the Y'CbCr to RGB pass in `render`, checked against the software decode of the same
//! frame converted with swscale, plus hardware throughput. Skips without fixtures or a
//! GPU. The M2 here stands in for the M1 reference machine.

#![cfg(target_os = "macos")]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use media::{
    DecodePath, DecoderOptions, FrameReader, FrameStorage, HwApi, HwPreference, StreamSelect,
    VideoFrame,
};
use render::interop::SurfaceImporter;
use render::wgpu;
use render::yuv::{YuvConverter, YuvParams};

fn fixture(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/generated")
        .join(name);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipped: {name} missing; run tests/fixtures/generate.sh to create it");
        None
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    converter: YuvConverter,
}

fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    if adapter.get_info().backend != wgpu::Backend::Metal {
        return None;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
        ..Default::default()
    }))
    .ok()?;
    let converter = YuvConverter::new(&device);
    Some(Gpu {
        device,
        queue,
        converter,
    })
}

fn target(gpu: &Gpu, (w, h): (u32, u32), format: wgpu::TextureFormat) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("converted frame"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Reads an Rgba8Unorm or Rgba16Float texture back as 8-bit RGBA.
fn read_rgba8(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let (w, h) = (texture.width(), texture.height());
    let bytes = texture
        .format()
        .block_copy_size(None)
        .expect("plain format");
    let row = (w * bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let data = buffer
        .slice(..)
        .get_mapped_range()
        .expect("mapped")
        .to_vec();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let line = &data[y * row as usize..][..(w * bytes) as usize];
        if bytes == 4 {
            out.extend_from_slice(line);
        } else {
            for half in line.as_chunks::<2>().0 {
                let v = f16_to_f32(u16::from_le_bytes(*half));
                out.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
    }
    out
}

fn f16_to_f32(h: u16) -> f32 {
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f32::from(h & 0x3ff);
    let v = match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    };
    if h & 0x8000 != 0 { -v } else { v }
}

fn nth_frame(path: &PathBuf, hw: HwPreference, n: usize) -> VideoFrame {
    let mut reader =
        FrameReader::open_with(path, StreamSelect::BestVideo, DecoderOptions::with_hw(hw))
            .expect("open");
    reader
        .nth(n)
        .expect("frame")
        .expect("decode")
        .into_video()
        .expect("video")
}

struct Diff {
    mean: f64,
    p99: u8,
    max: u8,
}

fn diff(a: &[u8], b: &[u8]) -> Diff {
    assert_eq!(a.len(), b.len());
    let mut histogram = [0u64; 256];
    let mut total = 0u64;
    let mut count = 0u64;
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for c in 0..3 {
            let d = pa[c].abs_diff(pb[c]);
            histogram[d as usize] += 1;
            total += u64::from(d);
            count += 1;
        }
    }
    let mut seen = 0u64;
    let mut p99 = 0u8;
    for (d, n) in histogram.iter().enumerate() {
        seen += n;
        if seen * 100 >= count * 99 {
            p99 = d as u8;
            break;
        }
    }
    let max = histogram.iter().rposition(|n| *n > 0).unwrap_or(0) as u8;
    Diff {
        mean: total as f64 / count as f64,
        p99,
        max,
    }
}

/// Decodes frame `n` in hardware, imports it without a copy, converts it on the GPU and
/// compares it with the software decode of the same frame converted by swscale.
fn compare_with_software(file: &str, n: usize, formats: &[wgpu::TextureFormat]) {
    let Some(path) = fixture(file) else {
        return;
    };
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no Metal adapter");
        return;
    };
    let hw = nth_frame(&path, HwPreference::Require, n);
    let FrameStorage::Hardware(surface) = hw.storage() else {
        panic!("{file}: frame {n} is not a hardware surface");
    };
    assert_eq!(hw.plane_count(), 0, "no CPU planes on a hardware frame");
    let mut importer = SurfaceImporter::default();
    let imported = importer.import(&gpu.device, surface).expect("import");
    assert_eq!(
        imported.key.surface_id,
        surface.io_surface().expect("surface").id(),
        "the textures wrap the decoder's own surface"
    );
    let reference = nth_frame(&path, HwPreference::Off, n)
        .to_rgba8()
        .expect("swscale");
    let size = (surface.width(), surface.height());
    let params = YuvParams::for_frame(surface);
    for &format in formats {
        let out = target(&gpu, size, format);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        assert!(gpu.converter.encode(
            &gpu.device,
            &mut encoder,
            &imported.luma,
            &imported.chroma,
            &params,
            &out.create_view(&Default::default()),
            format,
        ));
        gpu.queue.submit([encoder.finish()]);
        let ours = read_rgba8(&gpu, &out);
        let d = diff(&ours, &reference.data);
        eprintln!(
            "{file} frame {n} ({}, {:?}) into {format:?}: mean abs difference {:.3}, p99 {}, max {} (8-bit levels)",
            surface.format(),
            params.matrix,
            d.mean,
            d.p99,
            d.max
        );
        assert!(d.mean < 0.05, "{file}: mean difference {}", d.mean);
        assert!(d.max <= 2, "{file}: max difference {}", d.max);
    }
}

#[test]
fn h264_4k_hardware_frame_matches_software() {
    compare_with_software(
        "h264_4k30.mp4",
        10,
        &[
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba16Float,
        ],
    );
}

#[test]
fn hevc_10bit_hardware_frame_matches_software() {
    compare_with_software(
        "hevc_10bit_1080p25.mp4",
        10,
        &[
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Rgba8Unorm,
        ],
    );
}

/// Frames per second decoding the whole file `passes` times, with `per_frame` run on each.
fn decode_fps(
    path: &PathBuf,
    options: DecoderOptions,
    passes: usize,
    mut per_frame: impl FnMut(VideoFrame),
) -> f64 {
    let mut reader = FrameReader::open_with(path, StreamSelect::BestVideo, options).expect("open");
    let start = Instant::now();
    let mut frames = 0usize;
    for _ in 0..passes {
        while let Some(frame) = reader.next_frame().expect("decode") {
            per_frame(frame.into_video().expect("video"));
            frames += 1;
        }
        reader.seek_to_start().expect("rewind");
    }
    frames as f64 / start.elapsed().as_secs_f64()
}

#[test]
fn h264_4k_hardware_throughput() {
    const PASSES: usize = 4;
    const IN_FLIGHT: usize = 3;
    let Some(path) = fixture("h264_4k30.mp4") else {
        return;
    };
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no Metal adapter");
        return;
    };
    let hw = DecoderOptions::with_hw(HwPreference::Require);
    let decode_only = decode_fps(&path, hw, PASSES, |f| assert!(f.is_hardware()));

    // Decode, import and convert every frame, keeping up to IN_FLIGHT conversions queued
    // on the GPU; each keeps its frame (and so its surface) until its submission is done.
    let mut importer = SurfaceImporter::default();
    let size = (3840, 2160);
    let outputs: Vec<wgpu::Texture> = (0..IN_FLIGHT)
        .map(|_| target(&gpu, size, wgpu::TextureFormat::Rgba16Float))
        .collect();
    let views: Vec<wgpu::TextureView> = outputs
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect();
    let mut in_flight = VecDeque::new();
    let mut n = 0usize;
    let full = decode_fps(&path, hw, PASSES, |frame| {
        let surface = frame.hardware().expect("hardware frame");
        let imported = importer.import(&gpu.device, surface).expect("import");
        if in_flight.len() == IN_FLIGHT {
            let (index, _done): (wgpu::SubmissionIndex, _) =
                in_flight.pop_front().expect("in flight");
            gpu.device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(index),
                    timeout: None,
                })
                .expect("poll");
        }
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        gpu.converter.encode(
            &gpu.device,
            &mut encoder,
            &imported.luma,
            &imported.chroma,
            &YuvParams::for_frame(surface),
            &views[n % IN_FLIGHT],
            wgpu::TextureFormat::Rgba16Float,
        );
        let index = gpu.queue.submit([encoder.finish()]);
        in_flight.push_back((index, imported));
        n += 1;
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    in_flight.clear();
    let (hits, misses) = importer.stats();

    let software = decode_fps(&path, DecoderOptions::default(), PASSES, |f| {
        assert!(!f.is_hardware())
    });
    let software_one_thread = decode_fps(
        &path,
        DecoderOptions {
            threads: 1,
            hw: HwPreference::Off,
        },
        1,
        |_| {},
    );
    eprintln!(
        "h264 4K, {} frames: VideoToolbox decode {decode_only:.0} fps; decode + zero-copy import + Rgba16Float conversion {full:.0} fps (surface cache {hits} hits, {misses} misses); software decode {software:.0} fps with a thread per core, {software_one_thread:.0} fps on one thread",
        n
    );
    assert!(
        misses <= 32,
        "surfaces are wrapped once and reused ({misses} misses)"
    );
    assert!(full >= 60.0, "4K hardware path at {full:.0} fps, under 60");
}

#[test]
fn hardware_path_has_no_cpu_download() {
    // The media crate's sources never call av_hwframe_transfer_data (the CPU download of
    // a hardware frame), so a hardware frame cannot silently turn into a copy.
    for source in [
        include_str!("../../media/src/ffi.rs"),
        include_str!("../../media/src/decode.rs"),
        include_str!("../../media/src/reader.rs"),
        include_str!("../../media/src/hw.rs"),
        include_str!("../../media/src/convert.rs"),
    ] {
        assert!(!source.contains("av_hwframe_transfer_data"));
        assert!(!source.contains("av_hwframe_map"));
    }
    let Some(path) = fixture("h264_1080p30.mp4") else {
        return;
    };
    let reader = FrameReader::open_with(
        &path,
        StreamSelect::BestVideo,
        DecoderOptions::with_hw(HwPreference::Auto),
    )
    .expect("open");
    assert_eq!(
        reader.decode_path(),
        &DecodePath::Hardware {
            api: HwApi::VideoToolbox
        }
    );
    for frame in reader {
        let frame = frame.expect("decode").into_video().expect("video");
        assert!(matches!(frame.storage(), FrameStorage::Hardware(_)));
    }
}
