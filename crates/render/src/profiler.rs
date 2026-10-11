//! GPU time per render-graph pass from timestamp queries (the proto-gpu approach):
//! begin and end-of-pass timestamps written by each pass, resolved once the frame's GPU
//! work has finished and read back through a small ring of mappable buffers, so reading
//! never waits for the GPU. Results arrive a frame or two late, for the debug panel and
//! benchmarks.
//!
//! The resolve is a separate submission because Metal on Apple M5 can resolve before
//! the last pass's end-of-pass sample lands when both are in the same command buffer,
//! which reads 0 or a stale value.
//!
//! Apple GPUs overlap consecutive passes, so per-pass spans overlap and do not add up;
//! [`FrameTimings::total_ms`] (first begin to last end) is the number to judge.
//! Without `wgpu::Features::TIMESTAMP_QUERY` the profiler is disabled and returns nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// One pass's GPU time.
#[derive(Clone, Debug, PartialEq)]
pub struct PassTiming {
    pub label: String,
    /// `None` when the pass wrote no timestamps.
    pub gpu_ms: Option<f64>,
}

/// GPU times of one executed graph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameTimings {
    pub passes: Vec<PassTiming>,
    /// From the first pass's start to the last pass's end.
    pub total_ms: Option<f64>,
}

const IDLE: u8 = 0;
/// The frame's passes are submitted; the GPU has not finished them.
const RUNNING: u8 = 1;
/// The frame's GPU work has finished; its queries are ready to resolve.
const DONE: u8 = 2;
/// The resolve is submitted and the readback buffer is being mapped.
const WAITING: u8 = 3;
const MAPPED: u8 = 4;
const FAILED: u8 = 5;

struct Readback {
    query_set: wgpu::QuerySet,
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    labels: Vec<String>,
    submitted: bool,
}

pub struct GpuProfiler {
    device: wgpu::Device,
    queue: wgpu::Queue,
    resolve: Option<wgpu::Buffer>,
    ring: Vec<Readback>,
    capacity: u32,
    period_ns: f64,
    labels: Vec<String>,
    current: Option<usize>,
    order: std::collections::VecDeque<usize>,
}

impl std::fmt::Debug for GpuProfiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuProfiler")
            .field("enabled", &self.enabled())
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

const RING: usize = 3;

impl GpuProfiler {
    /// The device features the profiler needs.
    pub const FEATURES: wgpu::Features = wgpu::Features::TIMESTAMP_QUERY;

    /// A profiler for up to `max_passes` passes per frame.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, max_passes: u32) -> Self {
        let enabled = device.features().contains(Self::FEATURES);
        let count = (max_passes * 2).clamp(2, wgpu::QUERY_SET_MAX_QUERIES);
        let size = u64::from(count) * 8;
        let resolve = enabled.then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("timestamp resolve"),
                size,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        });
        let ring = if enabled {
            (0..RING)
                .map(|_| Readback {
                    query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                        label: Some("pass timestamps"),
                        ty: wgpu::QueryType::Timestamp,
                        count,
                    }),
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("timestamp readback"),
                        size,
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    state: Arc::new(AtomicU8::new(IDLE)),
                    labels: Vec::new(),
                    submitted: false,
                })
                .collect()
        } else {
            Vec::new()
        };
        Self {
            device: device.clone(),
            queue: queue.clone(),
            resolve,
            ring,
            capacity: count / 2,
            period_ns: f64::from(queue.get_timestamp_period()),
            labels: Vec::new(),
            current: None,
            order: Default::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        !self.ring.is_empty()
    }

    /// Starts a frame. Returns `false` (and times nothing) when every readback buffer is
    /// still waiting for the GPU.
    pub(crate) fn begin_frame(&mut self) -> bool {
        self.labels.clear();
        self.current = self
            .ring
            .iter()
            .position(|r| r.state.load(Ordering::Acquire) == IDLE && !r.submitted);
        self.current.is_some()
    }

    /// Timestamp writes for the next pass, or `None` when the profiler is off, the frame
    /// is not timed or the frame has used every query.
    pub(crate) fn pass(&mut self, label: &str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let slot = self.current?;
        let index = u32::try_from(self.labels.len()).ok()?;
        if index >= self.capacity {
            return None;
        }
        self.labels.push(label.to_owned());
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.ring[slot].query_set,
            beginning_of_pass_write_index: Some(index * 2),
            end_of_pass_write_index: Some(index * 2 + 1),
        })
    }

    /// Ends the frame's recording; its queries are resolved after the GPU finishes it.
    pub(crate) fn end_frame(&mut self) {
        let Some(slot) = self.current else {
            return;
        };
        if self.labels.is_empty() {
            self.current = None;
            return;
        }
        let readback = &mut self.ring[slot];
        readback.labels = std::mem::take(&mut self.labels);
        readback.submitted = true;
    }

    /// Call after the frame's commands are submitted: marks its queries ready to resolve
    /// once the GPU has finished the submission.
    pub(crate) fn after_submit(&mut self) {
        let Some(slot) = self.current.take() else {
            return;
        };
        let readback = &mut self.ring[slot];
        if !readback.submitted {
            return;
        }
        readback.state.store(RUNNING, Ordering::Release);
        let state = readback.state.clone();
        self.queue.on_submitted_work_done(move || {
            state.store(DONE, Ordering::Release);
        });
        self.order.push_back(slot);
    }

    /// The oldest finished frame's timings, without waiting.
    pub fn poll(&mut self, device: &wgpu::Device) -> Option<FrameTimings> {
        let _ = device.poll(wgpu::PollType::Poll);
        if self.resolve_finished() {
            let _ = device.poll(wgpu::PollType::Poll);
        }
        self.take_ready()
    }

    /// Waits for the oldest submitted frame's timings.
    pub fn wait(&mut self, device: &wgpu::Device) -> Option<FrameTimings> {
        let &slot = self.order.front()?;
        loop {
            self.resolve_finished();
            match self.ring[slot].state.load(Ordering::Acquire) {
                RUNNING | WAITING => {
                    if device.poll(wgpu::PollType::wait_indefinitely()).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
        self.take_ready()
    }

    /// Submits the resolve of every frame whose GPU work has finished and starts mapping
    /// its readback. Returns whether anything was submitted.
    fn resolve_finished(&mut self) -> bool {
        let Some(resolve) = &self.resolve else {
            return false;
        };
        let mut encoder = None;
        let mut mapping = Vec::new();
        for &slot in &self.order {
            let readback = &self.ring[slot];
            if readback.state.load(Ordering::Acquire) != DONE {
                continue;
            }
            let count = u32::try_from(readback.labels.len() * 2).unwrap_or(0);
            let encoder = encoder.get_or_insert_with(|| {
                self.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("timestamp resolve"),
                    })
            });
            encoder.resolve_query_set(&readback.query_set, 0..count, resolve, 0);
            encoder.copy_buffer_to_buffer(resolve, 0, &readback.buffer, 0, u64::from(count) * 8);
            readback.state.store(WAITING, Ordering::Release);
            mapping.push(slot);
        }
        let Some(encoder) = encoder else {
            return false;
        };
        self.queue.submit([encoder.finish()]);
        for slot in mapping {
            let readback = &self.ring[slot];
            let state = readback.state.clone();
            readback
                .buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let next = if result.is_ok() { MAPPED } else { FAILED };
                    state.store(next, Ordering::Release);
                });
        }
        true
    }

    fn take_ready(&mut self) -> Option<FrameTimings> {
        let &slot = self.order.front()?;
        let readback = &mut self.ring[slot];
        match readback.state.load(Ordering::Acquire) {
            MAPPED => {}
            FAILED => {
                readback.state.store(IDLE, Ordering::Release);
                readback.submitted = false;
                self.order.pop_front();
                return None;
            }
            _ => return None,
        }
        let ticks: Vec<u64> = match readback.buffer.slice(..).get_mapped_range() {
            Ok(data) => data
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c))
                .collect(),
            Err(_) => Vec::new(),
        };
        readback.buffer.unmap();
        readback.state.store(IDLE, Ordering::Release);
        readback.submitted = false;
        self.order.pop_front();
        let labels = std::mem::take(&mut readback.labels);
        Some(self.timings(&labels, &ticks))
    }

    fn timings(&self, labels: &[String], ticks: &[u64]) -> FrameTimings {
        let ms = |a: u64, b: u64| b.saturating_sub(a) as f64 * self.period_ns / 1e6;
        let mut first = None::<u64>;
        let mut last = None::<u64>;
        let passes = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let (begin, end) = (ticks.get(i * 2).copied(), ticks.get(i * 2 + 1).copied());
                let gpu_ms = match (begin, end) {
                    (Some(b), Some(e)) if b != 0 && e >= b => {
                        first = Some(first.map_or(b, |f| f.min(b)));
                        last = Some(last.map_or(e, |l| l.max(e)));
                        Some(ms(b, e))
                    }
                    _ => None,
                };
                PassTiming {
                    label: label.clone(),
                    gpu_ms,
                }
            })
            .collect();
        FrameTimings {
            passes,
            total_ms: first.zip(last).map(|(f, l)| ms(f, l)),
        }
    }
}
