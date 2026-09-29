//! GPU time per render-graph pass from timestamp queries (the proto-gpu approach):
//! begin and end-of-pass timestamps written by each pass, resolved at the end of the
//! frame and read back through a small ring of mappable buffers, so reading never waits
//! for the GPU. Results arrive a frame or two late, for the debug panel and benchmarks.
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
const WAITING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

struct Readback {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    labels: Vec<String>,
    submitted: bool,
}

pub struct GpuProfiler {
    query_set: Option<wgpu::QuerySet>,
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
        let query_set = enabled.then(|| {
            device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("pass timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count,
            })
        });
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
            query_set,
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
        self.query_set.is_some()
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
        self.current?;
        let index = u32::try_from(self.labels.len()).ok()?;
        if index >= self.capacity {
            return None;
        }
        self.labels.push(label.to_owned());
        self.query_set
            .as_ref()
            .map(|query_set| wgpu::RenderPassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(index * 2),
                end_of_pass_write_index: Some(index * 2 + 1),
            })
    }

    /// Resolves this frame's queries into its readback buffer.
    pub(crate) fn end_frame(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let (Some(slot), Some(query_set), Some(resolve)) =
            (self.current, &self.query_set, &self.resolve)
        else {
            return;
        };
        let count = u32::try_from(self.labels.len() * 2).unwrap_or(0);
        if count == 0 {
            self.current = None;
            return;
        }
        encoder.resolve_query_set(query_set, 0..count, resolve, 0);
        let readback = &mut self.ring[slot];
        encoder.copy_buffer_to_buffer(resolve, 0, &readback.buffer, 0, u64::from(count) * 8);
        readback.labels = std::mem::take(&mut self.labels);
        readback.submitted = true;
    }

    /// Call after the frame's commands are submitted: starts mapping its readback.
    pub(crate) fn after_submit(&mut self) {
        let Some(slot) = self.current.take() else {
            return;
        };
        let readback = &mut self.ring[slot];
        if !readback.submitted {
            return;
        }
        readback.state.store(WAITING, Ordering::Release);
        let state = readback.state.clone();
        readback
            .buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let next = if result.is_ok() { MAPPED } else { FAILED };
                state.store(next, Ordering::Release);
            });
        self.order.push_back(slot);
    }

    /// The oldest finished frame's timings, without waiting.
    pub fn poll(&mut self, device: &wgpu::Device) -> Option<FrameTimings> {
        let _ = device.poll(wgpu::PollType::Poll);
        self.take_ready()
    }

    /// Waits for the oldest submitted frame's timings.
    pub fn wait(&mut self, device: &wgpu::Device) -> Option<FrameTimings> {
        let &slot = self.order.front()?;
        while self.ring[slot].state.load(Ordering::Acquire) == WAITING {
            if device.poll(wgpu::PollType::wait_indefinitely()).is_err() {
                break;
            }
        }
        self.take_ready()
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
