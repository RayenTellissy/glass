//! The Y'CbCr to RGB pass on synthetic biplanar planes uploaded from the CPU: known
//! colour bars come out as the RGB the matrix and range define, for 8-bit NV12 and
//! 10-bit P010, into both output formats. Skips when no GPU adapter exists.

use media::{ColourInfo, ColourRange, HwSurfaceFormat};
use render::interop::plane_formats;
use render::wgpu;
use render::yuv::{YuvConverter, YuvParams};

const W: u32 = 64;
const H: u32 = 16;

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    let features = adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        ..Default::default()
    }))
    .ok()
}

fn texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    (w, h): (u32, u32),
    data: &[u8],
) -> wgpu::TextureView {
    let bytes = format.block_copy_size(None).expect("plain format");
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
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
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
    texture.create_view(&Default::default())
}

fn read_rgba(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    encoder: wgpu::CommandEncoder,
) -> Vec<[f32; 4]> {
    let bytes = target.format().block_copy_size(None).expect("plain format");
    let row = (W * bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * H),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = encoder;
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(H),
            },
        },
        target.size(),
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
    let mut out = Vec::new();
    for y in 0..H as usize {
        for x in 0..W as usize {
            let at = y * row as usize + x * bytes as usize;
            let px = &data[at..at + bytes as usize];
            out.push(if bytes == 4 {
                [0, 1, 2, 3].map(|i| f32::from(px[i]) / 255.0)
            } else {
                [0, 1, 2, 3].map(|i| f16_to_f32(u16::from_le_bytes([px[2 * i], px[2 * i + 1]])))
            });
        }
    }
    out
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f32::from(h & 0x3ff);
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

/// Four vertical bars (black, white, BT.709 red, BT.709 blue) as limited-range codes.
const BARS: [(u16, u16, u16, [f32; 3]); 4] = [
    (16, 128, 128, [0.0, 0.0, 0.0]),
    (235, 128, 128, [1.0, 1.0, 1.0]),
    (63, 102, 240, [1.0, 0.0, 0.0]),
    (32, 240, 118, [0.0, 0.0, 1.0]),
];

fn run(format: HwSurfaceFormat, target_format: wgpu::TextureFormat) {
    let Some((device, queue)) = device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    if !device
        .features()
        .contains(render::interop::SurfaceImporter::required_features(format))
    {
        eprintln!("skipped: the adapter lacks 16-bit normalised textures");
        return;
    }
    let ten_bit = format.bit_depth() == 10;
    let code = |c: u16| if ten_bit { (c * 4) << 6 } else { c };
    let push = |out: &mut Vec<u8>, v: u16| {
        if ten_bit {
            out.extend_from_slice(&v.to_le_bytes());
        } else {
            out.push(v as u8);
        }
    };
    let bar = |x: u32| &BARS[(x * 4 / W) as usize];
    let mut luma = Vec::new();
    for _ in 0..H {
        for x in 0..W {
            push(&mut luma, code(bar(x).0));
        }
    }
    let mut chroma = Vec::new();
    for _ in 0..H / 2 {
        for x in 0..W / 2 {
            let b = bar(x * 2);
            push(&mut chroma, code(b.1));
            push(&mut chroma, code(b.2));
        }
    }
    let (lf, cf) = plane_formats(format);
    let luma = texture(&device, &queue, lf, (W, H), &luma);
    let chroma = texture(&device, &queue, cf, (W / 2, H / 2), &chroma);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let colour = ColourInfo {
        primaries: Some("bt709"),
        transfer: Some("bt709"),
        matrix: Some("bt709"),
        range: ColourRange::Limited,
        chroma_location: Some("center"),
    };
    let params = YuvParams::new(&colour, format, false, H);
    let converter = YuvConverter::new(&device);
    let mut encoder = device.create_command_encoder(&Default::default());
    assert!(converter.encode(
        &device,
        &mut encoder,
        &luma,
        &chroma,
        &params,
        &target.create_view(&Default::default()),
        target_format,
    ));
    let pixels = read_rgba(&device, &queue, &target, encoder);
    // Away from the bar edges, where chroma is interpolated between bars.
    let tolerance = if target_format == wgpu::TextureFormat::Rgba8Unorm {
        2.5 / 255.0
    } else {
        0.01
    };
    for y in 0..H {
        for x in [2, W / 4 + 4, W / 2 + 4, 3 * W / 4 + 4] {
            let px = pixels[(y * W + x) as usize];
            let want = bar(x).3;
            for c in 0..3 {
                assert!(
                    (px[c] - want[c]).abs() < tolerance,
                    "{format} into {target_format:?} at ({x}, {y}): {px:?}, want {want:?}"
                );
            }
            assert_eq!(px[3], 1.0);
        }
    }
}

#[test]
fn nv12_bars_to_rgba8() {
    run(HwSurfaceFormat::Nv12, wgpu::TextureFormat::Rgba8Unorm);
}

#[test]
fn nv12_bars_to_rgba16f() {
    run(HwSurfaceFormat::Nv12, wgpu::TextureFormat::Rgba16Float);
}

#[test]
fn p010_bars_to_rgba16f() {
    run(HwSurfaceFormat::P010, wgpu::TextureFormat::Rgba16Float);
}
