//! GPU helpers shared by the compositor tests: a device (with timestamps when the adapter
//! has them), textures from pixel arrays, and readback of any output format as `f32`.

#![allow(dead_code)]

pub mod timing;

use render::wgpu;

pub fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .ok()?;
    let features = adapter.features()
        & (wgpu::Features::TEXTURE_FORMAT_16BIT_NORM | wgpu::Features::TIMESTAMP_QUERY);
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        ..Default::default()
    }))
    .ok()
}

pub const USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

pub fn blank(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    (w, h): (u32, u32),
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
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
        usage: USAGE,
        view_formats: &[],
    })
}

pub fn texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    (w, h): (u32, u32),
    data: &[u8],
) -> wgpu::Texture {
    let bytes = format.block_copy_size(None).expect("plain format");
    let texture = blank(device, format, (w, h));
    queue.write_texture(
        texture.as_image_copy(),
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * bytes),
            rows_per_image: Some(h),
        },
        texture.size(),
    );
    texture
}

/// An `Rgba16Float` texture from linear premultiplied pixels, row-major.
pub fn rgba16f(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    (w, h): (u32, u32),
    pixels: &[[f32; 4]],
) -> wgpu::Texture {
    let mut data = Vec::with_capacity(pixels.len() * 8);
    for px in pixels {
        for c in px {
            data.extend_from_slice(&f32_to_f16(*c).to_le_bytes());
        }
    }
    texture(
        device,
        queue,
        wgpu::TextureFormat::Rgba16Float,
        (w, h),
        &data,
    )
}

/// Every pixel of `texture` as normalised or float RGBA.
pub fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    let (w, h) = (texture.width(), texture.height());
    let format = texture.format();
    let bytes = format.block_copy_size(None).expect("plain format");
    let row = (w * bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
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
    queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let data = buffer
        .slice(..)
        .get_mapped_range()
        .expect("mapped")
        .to_vec();
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let at = y * row as usize + x * bytes as usize;
            let px = &data[at..at + bytes as usize];
            let u16s = |i: usize| u16::from_le_bytes([px[2 * i], px[2 * i + 1]]);
            out.push(match format {
                wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {
                    [0, 1, 2, 3].map(|i| f32::from(px[i]) / 255.0)
                }
                wgpu::TextureFormat::Bgra8Unorm => [2, 1, 0, 3].map(|i| f32::from(px[i]) / 255.0),
                wgpu::TextureFormat::Rgba16Float => [0, 1, 2, 3].map(|i| f16_to_f32(u16s(i))),
                wgpu::TextureFormat::Rgba16Unorm => {
                    [0, 1, 2, 3].map(|i| f32::from(u16s(i)) / 65535.0)
                }
                wgpu::TextureFormat::Rgb10a2Unorm => {
                    let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                    [
                        (v & 1023) as f32 / 1023.0,
                        ((v >> 10) & 1023) as f32 / 1023.0,
                        ((v >> 20) & 1023) as f32 / 1023.0,
                        (v >> 30) as f32 / 3.0,
                    ]
                }
                other => panic!("no readback for {other:?}"),
            });
        }
    }
    out
}

/// A noise tile of premultiplied pixels (alpha 0.25 to 1, some values above 1) as f16
/// bytes, repeated over a 4K texture from a different offset per layer, so lossless
/// texture compression cannot shortcut the reads.
pub fn noise_4k(device: &wgpu::Device, queue: &wgpu::Queue, seed: u32) -> wgpu::Texture {
    const TILE: (usize, usize) = (509, 251);
    const SIZE: (usize, usize) = (3840, 2160);
    let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state >> 8) as f32 / (1 << 24) as f32
    };
    let mut tile = Vec::with_capacity(TILE.0 * TILE.1 * 8);
    for _ in 0..TILE.0 * TILE.1 {
        let a = 0.25 + 0.75 * next();
        let px = [next() * 1.2 * a, next() * a, next() * a, a];
        for c in px {
            tile.extend_from_slice(&f32_to_f16(c).to_le_bytes());
        }
    }
    let mut data = Vec::with_capacity(SIZE.0 * SIZE.1 * 8);
    for y in 0..SIZE.1 {
        let row = &tile[((y + seed as usize * 37) % TILE.1) * TILE.0 * 8..][..TILE.0 * 8];
        let mut x = seed as usize * 101 % TILE.0;
        let mut left = SIZE.0;
        while left > 0 {
            let n = (TILE.0 - x).min(left);
            data.extend_from_slice(&row[x * 8..(x + n) * 8]);
            left -= n;
            x = 0;
        }
    }
    texture(
        device,
        queue,
        wgpu::TextureFormat::Rgba16Float,
        (SIZE.0 as u32, SIZE.1 as u32),
        &data,
    )
}

pub fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f32::from(h & 0x3ff);
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

/// Round to nearest even; enough for test inputs (no NaN handling).
pub fn f32_to_f16(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let a = v.abs();
    if a == 0.0 {
        return sign;
    }
    if a >= 65520.0 {
        return sign | 0x7c00;
    }
    if a < 6.103_515_6e-5 {
        // Subnormal: units of 2^-24.
        return sign | (a / 2f32.powi(-24)).round_ties_even() as u16;
    }
    let exp = a.log2().floor() as i32;
    let mut e = exp;
    let mut m = (a / 2f32.powi(e) - 1.0) * 1024.0;
    m = m.round_ties_even();
    if m >= 1024.0 {
        m = 0.0;
        e += 1;
    }
    sign | (((e + 15) as u16) << 10) | m as u16
}
