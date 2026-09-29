//! Offscreen variant of the frame loop (`--offscreen`).
//!
//! eframe skips tessellation and paint while its window is occluded (a locked
//! screen, another Space), so the windowed run measures nothing then. This
//! mode drives the same `egui::Context` + `egui_wgpu::Renderer` path eframe
//! uses, into a 2880 x 1496 texture instead of a surface: UI build,
//! tessellation, texture and buffer upload, render pass encoding and
//! `Queue::submit`, all timed on this thread. There is no surface acquire or
//! present; at most two frames are in flight, and waiting for the older one
//! happens outside the timed region, like eframe's acquire wait.

use std::collections::VecDeque;
use std::error::Error;
use std::time::Instant;

use eframe::egui::{self, Pos2, Rect, vec2};
use eframe::{egui_wgpu, wgpu};

use crate::Samples;

pub const WIDTH_PX: u32 = 2880;
pub const HEIGHT_PX: u32 = 1496;
pub const PIXELS_PER_POINT: f32 = 2.0;
const FRAMES_IN_FLIGHT: usize = 2;

pub fn run(
    frames: usize,
    warmup: usize,
    samples: &mut Samples,
    mut draw: impl FnMut(&mut egui::Ui, usize) -> usize,
) -> Result<String, Box<dyn Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))?;
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("proto-ui offscreen"),
        ..Default::default()
    }))?;

    let format = wgpu::TextureFormat::Bgra8Unorm;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("proto-ui target"),
        size: wgpu::Extent3d {
            width: WIDTH_PX,
            height: HEIGHT_PX,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer =
        egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [WIDTH_PX, HEIGHT_PX],
        pixels_per_point: PIXELS_PER_POINT,
    };

    let ctx = egui::Context::default();
    let points = vec2(
        WIDTH_PX as f32 / PIXELS_PER_POINT,
        HEIGHT_PX as f32 / PIXELS_PER_POINT,
    );
    let mut in_flight = VecDeque::new();
    let mut last_start: Option<Instant> = None;

    for frame in 0..warmup + frames {
        if in_flight.len() >= FRAMES_IN_FLIGHT
            && let Some(oldest) = in_flight.pop_front()
        {
            device.poll(wgpu::PollType::Wait {
                submission_index: Some(oldest),
                timeout: None,
            })?;
        }

        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, points)),
            ..Default::default()
        };
        raw.viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(PIXELS_PER_POINT);

        let start = Instant::now();
        let mut build_ms = 0.0;
        let mut shapes = 0;
        let out = ctx.run_ui(raw, |ui| {
            let t = Instant::now();
            shapes = draw(ui, frame);
            build_ms = t.elapsed().as_secs_f32() * 1000.0;
        });
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                renderer.update_texture(&device, &queue, *id, delta);
            }
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let user_bufs = renderer.update_buffers(&device, &queue, &mut encoder, &prims, &screen);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui_render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass.forget_lifetime(), &prims, &screen);
        }
        let encoded = encoder.finish();
        let submit_start = Instant::now();
        in_flight.push_back(queue.submit(user_bufs.into_iter().chain([encoded])));
        let submit_ms = submit_start.elapsed().as_secs_f32() * 1000.0;
        for id in &out.textures_delta.free {
            renderer.free_texture(id);
        }
        let total_ms = start.elapsed().as_secs_f32() * 1000.0;

        if frame >= warmup {
            samples.ui_thread_ms.push(total_ms);
            samples.cpu_ms.push(total_ms - submit_ms);
            samples.submit_ms.push(submit_ms);
            samples.build_ms.push(build_ms);
            samples.shapes.push(shapes);
            if let Some(last) = last_start {
                samples
                    .interval_ms
                    .push((start - last).as_secs_f32() * 1000.0);
            }
        }
        last_start = Some(start);
    }
    device.poll(wgpu::PollType::wait_indefinitely())?;
    Ok(format!(
        "wgpu {:?} on {} (offscreen)",
        info.backend, info.name
    ))
}
