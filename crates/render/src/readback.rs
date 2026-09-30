//! Asynchronous GPU readback for export and scopes: a ring of mappable staging buffers
//! that texture copies land in, mapped on submit and collected later without waiting,
//! in the style of [`crate::profiler::GpuProfiler`]'s timestamp ring.
//!
//! [`ReadbackRing::record`] copies a texture into a free buffer inside the caller's
//! command encoder and asks wgpu to map that buffer when the encoder is submitted
//! (`map_buffer_on_submit`), so the submitting thread never waits for the GPU.
//! [`ReadbackRing::poll`] checks the device once (`PollType::Poll`) and hands back the
//! oldest finished frame; frames always come out in submission order, each tagged with a
//! ring-assigned id, the caller's timestamp and the submit instant.
//!
//! When every buffer holds a frame the consumer has not taken yet, the [`Backpressure`]
//! policy decides: [`Backpressure::Lossless`] (export) refuses the new frame with
//! [`SubmitError::Full`] and records nothing, so the producer keeps the frame and retries
//! after the consumer drains (the export thread may [`ReadbackRing::wait`]; nothing is
//! ever dropped); [`Backpressure::DropOldest`] (scopes) discards the oldest finished
//! frame and reuses its buffer, or drops the new frame when every buffer is still
//! waiting for the GPU. [`ReadbackStats`] counts both outcomes.
//!
//! Buffer rows are padded to wgpu's 256-byte copy alignment; [`FrameView`] exposes the
//! padded rows without a copy and [`ReadbackFrame`] owns tightly packed rows. Both
//! convert to packed RGBA8 or RGBA16 for encoders (stored values, no transfer function:
//! the output node encodes before readback).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

/// What the ring does when the consumer falls behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backpressure {
    /// Never drop: a submit with every buffer occupied returns [`SubmitError::Full`].
    Lossless,
    /// Keep the newest frames: evict the oldest finished frame, or drop the new one when
    /// no buffer is free and none has finished.
    DropOldest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadbackConfig {
    /// Staging buffers in the ring (at least 1).
    pub slots: usize,
    pub policy: Backpressure,
}

impl ReadbackConfig {
    /// Export: four buffers in flight, nothing dropped.
    pub const EXPORT: Self = Self {
        slots: 4,
        policy: Backpressure::Lossless,
    };
    /// Scopes: three buffers, stale frames dropped.
    pub const SCOPES: Self = Self {
        slots: 3,
        policy: Backpressure::DropOldest,
    };
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    /// Every buffer holds an undelivered frame (lossless), or is still waiting for the GPU
    /// (drop-oldest, where the new frame counts as dropped). Nothing was recorded.
    #[error("every readback buffer is in use")]
    Full,
    #[error("readback does not support {0:?}")]
    UnsupportedFormat(wgpu::TextureFormat),
    #[error("the texture was not created with COPY_SRC")]
    NotCopySource,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReadbackError {
    #[error("mapping readback frame {id} failed")]
    MapFailed { id: u64 },
    /// The oldest frame's encoder was never submitted, so it can never arrive.
    #[error("readback frame {id} was recorded but its encoder was never submitted")]
    NotSubmitted { id: u64 },
}

/// Counters since the ring was created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadbackStats {
    pub submitted: u64,
    pub delivered: u64,
    /// Drop-oldest only: evicted undelivered frames plus new frames dropped.
    pub dropped: u64,
    /// Lossless only: submits refused because every buffer was occupied.
    pub refused: u64,
    pub failed: u64,
}

/// A finished frame read in place from its mapped buffer. Rows are `bytes_per_row` apart
/// (padded to 256 bytes); each row's first `width * bytes_per_pixel` bytes are pixels.
#[derive(Debug)]
pub struct FrameView<'a> {
    pub id: u64,
    pub timestamp: u64,
    pub submitted: Instant,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    pub bytes_per_row: usize,
    pub data: &'a [u8],
}

/// A finished frame with tightly packed rows (`width * bytes_per_pixel` bytes each).
#[derive(Clone, Debug)]
pub struct ReadbackFrame {
    pub id: u64,
    pub timestamp: u64,
    pub submitted: Instant,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    pub data: Vec<u8>,
}

const IDLE: u8 = 0;
const WAITING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

#[derive(Clone, Copy, Debug)]
struct Pending {
    id: u64,
    timestamp: u64,
    submitted: Instant,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    bytes_per_row: u32,
}

struct Slot {
    buffer: Option<wgpu::Buffer>,
    state: Arc<AtomicU8>,
    pending: Option<Pending>,
}

pub struct ReadbackRing {
    slots: Vec<Slot>,
    policy: Backpressure,
    /// Occupied slots, oldest first.
    order: VecDeque<usize>,
    next_id: u64,
    stats: ReadbackStats,
    spare: Vec<Vec<u8>>,
}

impl std::fmt::Debug for ReadbackRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadbackRing")
            .field("slots", &self.slots.len())
            .field("policy", &self.policy)
            .field("in_flight", &self.order.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

/// Bytes per pixel of a format the ring reads back, `None` for the others.
pub fn bytes_per_pixel(format: wgpu::TextureFormat) -> Option<u32> {
    use wgpu::TextureFormat as F;
    match format {
        F::Rgba16Float | F::Rgba16Unorm => Some(8),
        F::Rgba8Unorm | F::Rgba8UnormSrgb | F::Bgra8Unorm | F::Bgra8UnormSrgb => Some(4),
        F::Rgb10a2Unorm => Some(4),
        _ => None,
    }
}

/// Row pitch of a readback buffer: `width * bytes_per_pixel` rounded up to 256 bytes.
pub fn padded_bytes_per_row(width: u32, bytes_per_pixel: u32) -> u32 {
    (width * bytes_per_pixel).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

impl ReadbackRing {
    /// An empty ring; buffers are created on first use or by [`Self::reserve`].
    pub fn new(config: ReadbackConfig) -> Self {
        let slots = (0..config.slots.max(1))
            .map(|_| Slot {
                buffer: None,
                state: Arc::new(AtomicU8::new(IDLE)),
                pending: None,
            })
            .collect();
        Self {
            slots,
            policy: config.policy,
            order: VecDeque::new(),
            next_id: 0,
            stats: ReadbackStats::default(),
            spare: Vec::new(),
        }
    }

    /// Creates every free buffer at the size a `width` x `height` `format` frame needs, so
    /// later submits of that size allocate nothing.
    pub fn reserve(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<(), SubmitError> {
        let bpp = bytes_per_pixel(format).ok_or(SubmitError::UnsupportedFormat(format))?;
        let size = u64::from(padded_bytes_per_row(width, bpp)) * u64::from(height);
        for index in 0..self.slots.len() {
            if self.is_free(index) {
                self.ensure_buffer(device, index, size);
            }
        }
        Ok(())
    }

    pub fn stats(&self) -> ReadbackStats {
        self.stats
    }

    pub fn policy(&self) -> Backpressure {
        self.policy
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Frames recorded and not yet delivered, dropped or failed.
    pub fn in_flight(&self) -> usize {
        self.order.len()
    }

    /// Records a copy of `texture` (mip 0, layer 0) into a free buffer, to be mapped when
    /// `encoder` is submitted. Returns the frame's id. Never waits for the GPU; the
    /// encoder must be submitted for the frame to arrive.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        timestamp: u64,
    ) -> Result<u64, SubmitError> {
        let format = texture.format();
        let bpp = bytes_per_pixel(format).ok_or(SubmitError::UnsupportedFormat(format))?;
        if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
            return Err(SubmitError::NotCopySource);
        }
        let slot = match self.free_slot() {
            Some(slot) => slot,
            None => {
                let _ = device.poll(wgpu::PollType::Poll);
                self.make_room()?
            }
        };
        let (width, height) = (texture.width(), texture.height());
        let bytes_per_row = padded_bytes_per_row(width, bpp);
        let size = u64::from(bytes_per_row) * u64::from(height);
        self.ensure_buffer(device, slot, size);
        let entry = &mut self.slots[slot];
        let Some(buffer) = &entry.buffer else {
            unreachable!("ensure_buffer creates the buffer")
        };
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        entry.state.store(WAITING, Ordering::Release);
        let state = entry.state.clone();
        encoder.map_buffer_on_submit(buffer, wgpu::MapMode::Read, ..size, move |result| {
            let next = if result.is_ok() { MAPPED } else { FAILED };
            state.store(next, Ordering::Release);
        });
        let id = self.next_id;
        self.next_id += 1;
        entry.pending = Some(Pending {
            id,
            timestamp,
            submitted: Instant::now(),
            width,
            height,
            format,
            bytes_per_row,
        });
        self.order.push_back(slot);
        self.stats.submitted += 1;
        Ok(id)
    }

    /// [`Self::record`] in an encoder of its own, submitted at once.
    pub fn submit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        timestamp: u64,
    ) -> Result<u64, SubmitError> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback"),
        });
        let id = self.record(device, &mut encoder, texture, timestamp)?;
        queue.submit([encoder.finish()]);
        Ok(id)
    }

    /// The oldest frame if it has arrived, without waiting.
    pub fn poll(&mut self, device: &wgpu::Device) -> Result<Option<ReadbackFrame>, ReadbackError> {
        let _ = device.poll(wgpu::PollType::Poll);
        self.try_take()
    }

    /// Like [`Self::poll`], reading the frame in place instead of copying it out.
    pub fn poll_with<R>(
        &mut self,
        device: &wgpu::Device,
        read: impl FnOnce(FrameView<'_>) -> R,
    ) -> Result<Option<R>, ReadbackError> {
        let _ = device.poll(wgpu::PollType::Poll);
        self.take_with(read)
    }

    /// The oldest frame if it has arrived, without touching the device (for when another
    /// thread polls it).
    pub fn try_take(&mut self) -> Result<Option<ReadbackFrame>, ReadbackError> {
        let spare = self.spare.pop().unwrap_or_default();
        self.take_with(|view| view.to_frame_in(spare))
    }

    /// Waits for the oldest frame in flight; `Ok(None)` when nothing is in flight. For the
    /// export thread, never the UI thread.
    pub fn wait(&mut self, device: &wgpu::Device) -> Result<Option<ReadbackFrame>, ReadbackError> {
        let Some(&slot) = self.order.front() else {
            return Ok(None);
        };
        while self.slots[slot].state.load(Ordering::Acquire) == WAITING {
            match device.poll(wgpu::PollType::wait_indefinitely()) {
                Ok(status) if status.is_queue_empty() => {
                    if self.slots[slot].state.load(Ordering::Acquire) == WAITING {
                        let id = self.slots[slot].pending.map_or(0, |p| p.id);
                        self.release(slot);
                        self.order.pop_front();
                        return Err(ReadbackError::NotSubmitted { id });
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        self.try_take()
    }

    /// Hands a delivered frame's storage back for reuse by later frames.
    pub fn recycle(&mut self, frame: ReadbackFrame) {
        if self.spare.len() < self.slots.len() {
            self.spare.push(frame.data);
        }
    }

    fn take_with<R>(
        &mut self,
        read: impl FnOnce(FrameView<'_>) -> R,
    ) -> Result<Option<R>, ReadbackError> {
        let Some(&slot) = self.order.front() else {
            return Ok(None);
        };
        let entry = &self.slots[slot];
        match entry.state.load(Ordering::Acquire) {
            MAPPED => {}
            FAILED => {
                let id = entry.pending.map_or(0, |p| p.id);
                self.order.pop_front();
                self.release(slot);
                self.stats.failed += 1;
                return Err(ReadbackError::MapFailed { id });
            }
            _ => return Ok(None),
        }
        let (Some(pending), Some(buffer)) = (entry.pending, &entry.buffer) else {
            unreachable!("an occupied slot has a frame and a buffer")
        };
        let size = u64::from(pending.bytes_per_row) * u64::from(pending.height);
        let result = match buffer.slice(..size).get_mapped_range() {
            Ok(data) => Some(read(FrameView {
                id: pending.id,
                timestamp: pending.timestamp,
                submitted: pending.submitted,
                width: pending.width,
                height: pending.height,
                format: pending.format,
                bytes_per_row: pending.bytes_per_row as usize,
                data: &data,
            })),
            Err(_) => None,
        };
        self.order.pop_front();
        self.release(slot);
        match result {
            Some(result) => {
                self.stats.delivered += 1;
                Ok(Some(result))
            }
            None => {
                self.stats.failed += 1;
                Err(ReadbackError::MapFailed { id: pending.id })
            }
        }
    }

    fn is_free(&self, index: usize) -> bool {
        self.slots[index].state.load(Ordering::Acquire) == IDLE && !self.order.contains(&index)
    }

    fn free_slot(&self) -> Option<usize> {
        (0..self.slots.len()).find(|&i| self.is_free(i))
    }

    /// With no free slot: refuses (lossless) or frees the oldest finished slot.
    fn make_room(&mut self) -> Result<usize, SubmitError> {
        if self.policy == Backpressure::Lossless {
            self.stats.refused += 1;
            return Err(SubmitError::Full);
        }
        self.stats.dropped += 1;
        let Some(&oldest) = self.order.front() else {
            return Err(SubmitError::Full);
        };
        match self.slots[oldest].state.load(Ordering::Acquire) {
            MAPPED | FAILED => {
                self.order.pop_front();
                self.release(oldest);
                Ok(oldest)
            }
            _ => Err(SubmitError::Full),
        }
    }

    fn release(&mut self, index: usize) {
        let slot = &mut self.slots[index];
        if slot.state.load(Ordering::Acquire) == MAPPED
            && let Some(buffer) = &slot.buffer
        {
            buffer.unmap();
        }
        slot.state.store(IDLE, Ordering::Release);
        slot.pending = None;
    }

    fn ensure_buffer(&mut self, device: &wgpu::Device, index: usize, size: u64) {
        let slot = &mut self.slots[index];
        if slot.buffer.as_ref().is_some_and(|b| b.size() >= size) {
            return;
        }
        slot.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }
}

impl FrameView<'_> {
    pub fn bytes_per_pixel(&self) -> usize {
        bytes_per_pixel(self.format).unwrap_or(0) as usize
    }

    /// Row `y`'s pixels, without padding.
    pub fn row(&self, y: u32) -> &[u8] {
        let start = y as usize * self.bytes_per_row;
        &self.data[start..start + self.width as usize * self.bytes_per_pixel()]
    }

    fn rows(&self) -> impl Iterator<Item = &[u8]> {
        (0..self.height).map(|y| self.row(y))
    }

    /// Copies the frame out with tightly packed rows.
    pub fn to_frame(&self) -> ReadbackFrame {
        self.to_frame_in(Vec::new())
    }

    fn to_frame_in(&self, mut data: Vec<u8>) -> ReadbackFrame {
        data.clear();
        data.reserve(self.width as usize * self.height as usize * self.bytes_per_pixel());
        for row in self.rows() {
            data.extend_from_slice(row);
        }
        ReadbackFrame {
            id: self.id,
            timestamp: self.timestamp,
            submitted: self.submitted,
            width: self.width,
            height: self.height,
            format: self.format,
            data,
        }
    }

    /// Packed 8-bit RGBA (see [`to_rgba8`]).
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for row in self.rows() {
            to_rgba8(self.format, row, &mut out);
        }
        out
    }

    /// Packed 16-bit RGBA (see [`to_rgba16`]).
    pub fn to_rgba16(&self) -> Vec<u16> {
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for row in self.rows() {
            to_rgba16(self.format, row, &mut out);
        }
        out
    }
}

impl ReadbackFrame {
    pub fn view(&self) -> FrameView<'_> {
        FrameView {
            id: self.id,
            timestamp: self.timestamp,
            submitted: self.submitted,
            width: self.width,
            height: self.height,
            format: self.format,
            bytes_per_row: self.width as usize * bytes_per_pixel(self.format).unwrap_or(0) as usize,
            data: &self.data,
        }
    }

    pub fn to_rgba8(&self) -> Vec<u8> {
        self.view().to_rgba8()
    }

    pub fn to_rgba16(&self) -> Vec<u16> {
        self.view().to_rgba16()
    }
}

/// Appends `pixels` (tightly packed, in `format`) to `out` as 8-bit RGBA: BGRA swizzled,
/// 10 and 16-bit values rounded, floats clamped to 0..1 (NaN to 0). Stored values are
/// kept, so sRGB formats stay sRGB-encoded and linear floats stay linear.
pub fn to_rgba8(format: wgpu::TextureFormat, pixels: &[u8], out: &mut Vec<u8>) {
    use wgpu::TextureFormat as F;
    match format {
        F::Rgba8Unorm | F::Rgba8UnormSrgb => out.extend_from_slice(pixels),
        F::Bgra8Unorm | F::Bgra8UnormSrgb => {
            for p in pixels.as_chunks::<4>().0 {
                out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        F::Rgb10a2Unorm => {
            for p in pixels.as_chunks::<4>().0 {
                let v = u32::from_le_bytes(*p);
                let c = |shift: u32| ((((v >> shift) & 0x3ff) * 255 + 511) / 1023) as u8;
                out.extend_from_slice(&[c(0), c(10), c(20), ((v >> 30) * 85) as u8]);
            }
        }
        F::Rgba16Unorm => {
            for c in pixels.as_chunks::<2>().0 {
                out.push(((u32::from(u16::from_le_bytes(*c)) * 255 + 32767) / 65535) as u8);
            }
        }
        F::Rgba16Float => {
            for c in pixels.as_chunks::<2>().0 {
                out.push(unit_to(f16_to_f32(u16::from_le_bytes(*c)), 255.0) as u8);
            }
        }
        _ => {}
    }
}

/// Appends `pixels` (tightly packed, in `format`) to `out` as 16-bit RGBA: 8 and 10-bit
/// values widened by bit replication (full scale stays full scale), floats clamped to
/// 0..1 (NaN to 0). Stored values are kept, as in [`to_rgba8`].
pub fn to_rgba16(format: wgpu::TextureFormat, pixels: &[u8], out: &mut Vec<u16>) {
    use wgpu::TextureFormat as F;
    match format {
        F::Rgba8Unorm | F::Rgba8UnormSrgb => {
            out.extend(pixels.iter().map(|&v| u16::from(v) * 257));
        }
        F::Bgra8Unorm | F::Bgra8UnormSrgb => {
            for p in pixels.as_chunks::<4>().0 {
                out.extend([p[2], p[1], p[0], p[3]].map(|v| u16::from(v) * 257));
            }
        }
        F::Rgb10a2Unorm => {
            for p in pixels.as_chunks::<4>().0 {
                let v = u32::from_le_bytes(*p);
                let c = |shift: u32| {
                    let c = (v >> shift) & 0x3ff;
                    ((c << 6) | (c >> 4)) as u16
                };
                out.extend([c(0), c(10), c(20), ((v >> 30) * 21845) as u16]);
            }
        }
        F::Rgba16Unorm => {
            out.extend(
                pixels
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c)),
            );
        }
        F::Rgba16Float => {
            out.extend(
                pixels
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| unit_to(f16_to_f32(u16::from_le_bytes(*c)), 65535.0) as u16),
            );
        }
        _ => {}
    }
}

fn unit_to(v: f32, max: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        (v.clamp(0.0, 1.0) * max).round()
    }
}

/// IEEE 754 binary16 to f32.
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let mantissa = u32::from(bits & 0x3ff);
    let magnitude = match (exponent, mantissa) {
        (0, 0) => 0,
        // Subnormal: m * 2^-24.
        (0, m) => return sign_of(bits) * m as f32 * (-24f32).exp2(),
        (0x1f, m) => 0x7f80_0000 | (m << 13),
        (e, m) => ((e + 112) << 23) | (m << 13),
    };
    f32::from_bits(sign | magnitude)
}

fn sign_of(bits: u16) -> f32 {
    if bits & 0x8000 != 0 { -1.0 } else { 1.0 }
}
