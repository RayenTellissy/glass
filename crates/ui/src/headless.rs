//! Headless UI harness for tests and benchmarks: drives an `egui::Context` and an
//! `egui_wgpu::Renderer` into an offscreen texture, timing every frame into a
//! [`FrameTimeMonitor`].
//!
//! eframe skips tessellation and paint while its window is occluded (a locked screen,
//! another Space), so frame budgets are checked here instead of in a window. A frame's
//! time covers the UI-thread work eframe does per frame: UI build, tessellation,
//! texture and buffer upload, render pass encoding and `Queue::submit`. Surface acquire
//! and present do not exist offscreen; at most two frames are in flight and waiting
//! for the older one happens outside the timed region, like eframe's acquire wait.
//!
//! This module creates its own wgpu device. It is for tests and benchmarks only; the
//! app renders through the single device eframe creates (docs/ARCHITECTURE.md).

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use egui::{Pos2, Rect, vec2};
use egui_wgpu::wgpu;

use crate::frame_time::{FrameStats, FrameTimeMonitor};

/// Set to `1` to make [`HeadlessUi::new_or_skip`] fail instead of skipping when there is
/// no GPU, or only a software rasteriser (for CI runners that are known to have one).
pub const REQUIRE_GPU_ENV: &str = "GLASS_REQUIRE_GPU";

const FRAMES_IN_FLIGHT: usize = 2;

/// The offscreen target's format: egui paints gamma-encoded colours, as eframe does into
/// a non-sRGB surface.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

fn create_target(device: &wgpu::Device, viewport: Viewport) -> (wgpu::Texture, wgpu::TextureView) {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glass headless target"),
        size: wgpu::Extent3d {
            width: viewport.width_px,
            height: viewport.height_px,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    (target, view)
}

/// A frame read back from the GPU: tightly packed RGBA8, gamma-encoded, rows top to
/// bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Why the harness could not start.
#[derive(Debug)]
pub enum HeadlessError {
    /// No wgpu adapter or device: no GPU, or a driver that refused.
    GpuUnavailable(String),
    Gpu(String),
}

impl fmt::Display for HeadlessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GpuUnavailable(reason) => write!(f, "no GPU available: {reason}"),
            Self::Gpu(reason) => write!(f, "GPU error: {reason}"),
        }
    }
}

impl std::error::Error for HeadlessError {}

/// Size and scale of the offscreen target.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub width_px: u32,
    pub height_px: u32,
    pub pixels_per_point: f32,
}

impl Default for Viewport {
    /// A 1440 x 748 point window at 2x, the size proto-ui measured.
    fn default() -> Self {
        Self {
            width_px: 2880,
            height_px: 1496,
            pixels_per_point: 2.0,
        }
    }
}

/// Whether a frame budget can be judged on an adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BudgetCheck {
    Enforce,
    /// The frames still render (so the UI code is exercised), but their times say
    /// nothing about a real GPU.
    Skip(String),
}

impl BudgetCheck {
    /// Software rasterisers (lavapipe, WARP, SwiftShader) report `DeviceType::Cpu`.
    pub fn for_device(device_type: wgpu::DeviceType, adapter: &str) -> Self {
        match device_type {
            wgpu::DeviceType::Cpu => Self::Skip(format!(
                "{adapter} is a software rasteriser, so frame times do not reflect a GPU"
            )),
            _ => Self::Enforce,
        }
    }
}

/// An egui context rendering into an offscreen wgpu texture.
pub struct HeadlessUi {
    ctx: egui::Context,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: egui_wgpu::Renderer,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    screen: egui_wgpu::ScreenDescriptor,
    viewport: Viewport,
    in_flight: VecDeque<wgpu::SubmissionIndex>,
    adapter: String,
    device_type: wgpu::DeviceType,
    has_text: bool,
    time: Option<f64>,
}

impl HeadlessUi {
    /// Creates the device, the target and the renderer, and installs the fonts from
    /// [`crate::fonts`] (bundled Geist, else a system font) when any are found.
    pub fn new(viewport: Viewport) -> Result<Self, HeadlessError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|e| HeadlessError::GpuUnavailable(e.to_string()))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("glass headless ui"),
            ..Default::default()
        }))
        .map_err(|e| HeadlessError::GpuUnavailable(e.to_string()))?;

        let (target, view) = create_target(&device, viewport);
        let renderer = egui_wgpu::Renderer::new(
            &device,
            TARGET_FORMAT,
            egui_wgpu::RendererOptions::default(),
        );

        let ctx = egui::Context::default();
        let has_text = crate::fonts::install(&ctx);

        Ok(Self {
            ctx,
            device,
            queue,
            renderer,
            target,
            view,
            screen: egui_wgpu::ScreenDescriptor {
                size_in_pixels: [viewport.width_px, viewport.height_px],
                pixels_per_point: viewport.pixels_per_point,
            },
            viewport,
            in_flight: VecDeque::with_capacity(FRAMES_IN_FLIGHT),
            adapter: format!("wgpu {:?} on {}", info.backend, info.name),
            device_type: info.device_type,
            has_text,
            time: None,
        })
    }

    /// Like [`new`](Self::new), but when there is no GPU it prints why the caller is
    /// skipped and returns `None`, unless `GLASS_REQUIRE_GPU=1`, in which case it
    /// panics. With `GLASS_REQUIRE_GPU=1` a software adapter panics too. Any other
    /// error panics.
    #[track_caller]
    pub fn new_or_skip(what: &str, viewport: Viewport) -> Option<Self> {
        let require = std::env::var(REQUIRE_GPU_ENV).as_deref() == Ok("1");
        match Self::new(viewport) {
            Ok(ui) => {
                if require && let BudgetCheck::Skip(reason) = ui.budget_check() {
                    panic!("{what}: {REQUIRE_GPU_ENV}=1 but {reason}");
                }
                Some(ui)
            }
            Err(HeadlessError::GpuUnavailable(reason)) if !require => {
                eprintln!(
                    "SKIPPED {what}: no GPU available ({reason}); set {REQUIRE_GPU_ENV}=1 to fail instead"
                );
                None
            }
            Err(error) => panic!("{what}: {error}"),
        }
    }

    pub fn context(&self) -> &egui::Context {
        &self.ctx
    }

    /// Where the fonts came from, for test and benchmark output.
    pub fn font_source(&self) -> String {
        crate::fonts::loaded().summary()
    }

    /// The backend and adapter name, for test and benchmark output.
    pub fn adapter(&self) -> &str {
        &self.adapter
    }

    /// Whether this adapter's frame times can be held to a budget.
    pub fn budget_check(&self) -> BudgetCheck {
        BudgetCheck::for_device(self.device_type, &self.adapter)
    }

    /// Asserts the p99 budget on a hardware adapter; on a software one prints why the
    /// budget is not judged and returns.
    #[track_caller]
    pub fn assert_p99_within_budget(&self, what: &str, stats: &FrameStats) {
        match self.budget_check() {
            BudgetCheck::Enforce => stats.assert_p99_within_budget(),
            BudgetCheck::Skip(reason) => {
                eprintln!("BUDGET NOT CHECKED {what}: {reason} (p99 {:?})", stats.p99);
            }
        }
    }

    /// Whether fonts were found, so the UI may draw text. Without fonts egui cannot lay
    /// out text at all, and callers draw shapes only.
    pub fn has_text(&self) -> bool {
        self.has_text
    }

    /// Pins egui's input time for the following frames, so anything drawn from
    /// `i.time` (spinners) looks the same whatever ran before; `None` lets egui advance
    /// it by 1/60 s per frame.
    pub fn set_time(&mut self, time: Option<f64>) {
        self.time = time;
    }

    /// Runs one frame and returns its UI-thread time.
    pub fn frame(&mut self, draw: impl FnMut(&mut egui::Ui)) -> Result<Duration, HeadlessError> {
        if self.in_flight.len() >= FRAMES_IN_FLIGHT
            && let Some(oldest) = self.in_flight.pop_front()
        {
            self.device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(oldest),
                    timeout: None,
                })
                .map_err(|e| HeadlessError::Gpu(e.to_string()))?;
        }

        let points = vec2(
            self.viewport.width_px as f32 / self.viewport.pixels_per_point,
            self.viewport.height_px as f32 / self.viewport.pixels_per_point,
        );
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, points)),
            time: self.time,
            ..Default::default()
        };
        raw.viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(self.viewport.pixels_per_point);

        let start = Instant::now();
        let egui::FullOutput {
            shapes,
            pixels_per_point,
            mut textures_delta,
            ..
        } = self.ctx.run_ui(raw, draw);
        let prims = self.ctx.tessellate(shapes, pixels_per_point);
        for (id, deltas) in &textures_delta.set {
            for delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let user_buffers = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &prims,
            &self.screen,
        );
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui_render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
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
            self.renderer
                .render(&mut pass.forget_lifetime(), &prims, &self.screen);
        }
        self.in_flight.push_back(
            self.queue
                .submit(user_buffers.into_iter().chain([encoder.finish()])),
        );
        for id in &textures_delta.free {
            self.renderer.free_texture(id);
        }
        // Every delta has been applied; egui asserts that none is dropped unhandled.
        textures_delta.clear();
        Ok(start.elapsed())
    }

    /// Whether the adapter is a software rasteriser (`DeviceType::Cpu`).
    pub fn is_software(&self) -> bool {
        self.device_type == wgpu::DeviceType::Cpu
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// Replaces the offscreen target with one of a new size and scale. Waits for the
    /// frames in flight, which still draw into the old target.
    pub fn set_viewport(&mut self, viewport: Viewport) -> Result<(), HeadlessError> {
        self.wait_idle()?;
        let (target, view) = create_target(&self.device, viewport);
        self.target = target;
        self.view = view;
        self.viewport = viewport;
        self.screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [viewport.width_px, viewport.height_px],
            pixels_per_point: viewport.pixels_per_point,
        };
        Ok(())
    }

    /// Runs `settle` frames of `draw` (egui lays some things out over two frames), then
    /// reads the last one back.
    pub fn capture(
        &mut self,
        settle: usize,
        mut draw: impl FnMut(&mut egui::Ui),
    ) -> Result<Capture, HeadlessError> {
        for _ in 0..settle.max(1) {
            self.frame(&mut draw)?;
        }
        self.read_back()
    }

    /// Copies the target into a mappable buffer and waits for it.
    fn read_back(&mut self) -> Result<Capture, HeadlessError> {
        let Viewport {
            width_px: width,
            height_px: height,
            ..
        } = self.viewport;
        let row = width * 4;
        let padded_row =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass headless readback"),
            size: u64::from(padded_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.wait_idle()?;
        rx.recv()
            .map_err(|e| HeadlessError::Gpu(e.to_string()))?
            .map_err(|e| HeadlessError::Gpu(e.to_string()))?;
        let mut rgba = Vec::with_capacity((row * height) as usize);
        {
            let mapped = slice
                .get_mapped_range()
                .map_err(|e| HeadlessError::Gpu(e.to_string()))?;
            for line in mapped.chunks_exact(padded_row as usize) {
                for bgra in line[..row as usize].as_chunks::<4>().0.iter() {
                    rgba.extend_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
                }
            }
        }
        buffer.unmap();
        Ok(Capture {
            width,
            height,
            rgba,
        })
    }

    fn wait_idle(&mut self) -> Result<(), HeadlessError> {
        self.in_flight.clear();
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map(|_| ())
            .map_err(|e| HeadlessError::Gpu(e.to_string()))
    }

    /// Runs `warmup` untimed frames, then `frames` frames recorded into `monitor`. The
    /// frame index is passed to `draw` so the UI can change from frame to frame.
    pub fn run(
        &mut self,
        warmup: usize,
        frames: usize,
        monitor: &mut FrameTimeMonitor,
        mut draw: impl FnMut(&mut egui::Ui, usize),
    ) -> Result<(), HeadlessError> {
        for i in 0..warmup + frames {
            let elapsed = self.frame(|ui| draw(ui, i))?;
            if i >= warmup {
                monitor.record(elapsed);
            }
        }
        self.wait_idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_adapters_skip_only_the_budget() {
        assert!(matches!(
            BudgetCheck::for_device(wgpu::DeviceType::Cpu, "wgpu Vulkan on llvmpipe"),
            BudgetCheck::Skip(reason) if reason.contains("llvmpipe")
        ));
        for hardware in [
            wgpu::DeviceType::IntegratedGpu,
            wgpu::DeviceType::DiscreteGpu,
            wgpu::DeviceType::VirtualGpu,
            wgpu::DeviceType::Other,
        ] {
            assert_eq!(
                BudgetCheck::for_device(hardware, "gpu"),
                BudgetCheck::Enforce,
                "{hardware:?}"
            );
        }
    }
}
