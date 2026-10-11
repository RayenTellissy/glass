//! Helpers shared by the engine integration tests: fixture paths, a headless GPU with
//! the Y'CbCr converter, render targets, readback and image differences.

#![allow(dead_code)]

#[path = "../../../render/tests/common/timing.rs"]
pub mod timing;

use std::path::PathBuf;

use media::{DecoderOptions, FrameReader, HwPreference, StreamSelect, VideoFrame};
use render::wgpu;
use render::yuv::{YuvConverter, YuvParams, YuvPlanes};

pub fn fixture(name: &str) -> Option<PathBuf> {
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

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub converter: YuvConverter,
}

pub fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    if !adapter
        .features()
        .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
    {
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

pub fn target(gpu: &Gpu, (w, h): (u32, u32), format: wgpu::TextureFormat) -> wgpu::Texture {
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

/// Reads an Rgba8Unorm or Rgba16Float texture back as RGBA floats, four per pixel.
pub fn read_rgba(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<f32> {
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
            out.extend(line.iter().map(|v| f32::from(*v) / 255.0));
        } else {
            for half in line.as_chunks::<2>().0 {
                out.push(f16_to_f32(u16::from_le_bytes(*half)));
            }
        }
    }
    out
}

/// Reads an Rgba8Unorm or Rgba16Float texture back as 8-bit RGBA.
pub fn read_rgba8(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    read_rgba(gpu, texture)
        .into_iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect()
}

/// Converts `planes` into a new target of `size` and `format` and waits for the GPU.
pub fn convert(
    gpu: &Gpu,
    planes: YuvPlanes<'_>,
    params: &YuvParams,
    size: (u32, u32),
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    let out = target(gpu, size, format);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(gpu.converter.encode(
        &gpu.device,
        &mut encoder,
        planes,
        params,
        &out.create_view(&Default::default()),
        format,
    ));
    gpu.queue.submit([encoder.finish()]);
    out
}

pub fn f16_to_f32(h: u16) -> f32 {
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f32::from(h & 0x3ff);
    let v = match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    };
    if h & 0x8000 != 0 { -v } else { v }
}

pub fn nth_frame(path: &PathBuf, hw: HwPreference, n: usize) -> VideoFrame {
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

pub struct Diff {
    pub mean: f64,
    pub p99: u8,
    pub max: u8,
}

pub fn diff(a: &[u8], b: &[u8]) -> Diff {
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
