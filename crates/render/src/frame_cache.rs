//! The rendered-frame cache: composited preview frames kept as GPU textures, keyed by
//! [`FrameKey`] (segment hash, frame index and the render settings that change pixels),
//! least recently used first out.
//!
//! Two tiers, each with a byte ceiling that can change at runtime:
//!
//! - **VRAM**: the textures themselves, handed over by the renderer on
//!   [`FrameCache::insert`]. A VRAM hit returns the texture as it is.
//! - **RAM**: frames evicted from VRAM are copied back to the CPU through a lossless
//!   [`ReadbackRing`] (no GPU wait: the copy is mapped on submit and collected by
//!   [`FrameCache::maintain`] a frame or two later) and stored packed in the configured
//!   [`SpillFormat`]. A RAM hit re-uploads the frame into a new texture
//!   (`queue.write_texture`, whose CPU cost the hit reports), promotes it back to VRAM and
//!   may push older frames down. Frames evicted from RAM are dropped.
//!
//! Recency is one clock across both tiers, so a spilled frame keeps its place: it lands in
//! RAM as old as it was when VRAM let it go. An eviction whose copy cannot start (every
//! staging buffer busy, a texture without `COPY_SRC`, a frame larger than the RAM ceiling)
//! drops the frame instead of waiting, and [`FrameCacheStats::spill_dropped`] counts it.
//!
//! Threading: the cache has one owner, the render thread (`glass-render`), which holds
//! the device and queue it needs for spills and uploads. Nothing here waits for the GPU
//! except [`FrameCache::finish_spills`] (tests and shutdown). The UI sees the cache only
//! through [`FrameCacheStats`] snapshots sent as latest-value messages; the scheduler asks
//! the render thread what is resident ([`FrameCache::contains`], [`FrameCache::keys`]).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::graph::TextureDesc;
use crate::interop::{self, MemoryReport};
use crate::readback::{self, Backpressure, FrameView, ReadbackConfig, ReadbackRing};

/// The segment a frame belongs to: an opaque hash of everything that renders it, supplied
/// by the scheduler's dirty-range tracking. A changed segment gets a new hash.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SegmentHash(pub [u8; 16]);

impl From<u64> for SegmentHash {
    fn from(value: u64) -> Self {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&value.to_le_bytes());
        Self(bytes)
    }
}

/// Render settings that change a frame's pixels without changing its segment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RenderVariant {
    pub width: u32,
    pub height: u32,
    /// Preview quality step (adaptive quality, sampling filter).
    pub quality: u8,
    /// Hash of any other pixel-changing setting (output transform, overlays).
    pub settings: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub segment: SegmentHash,
    pub frame: u64,
    pub variant: RenderVariant,
}

/// How spilled frames are stored in RAM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpillFormat {
    /// The texture's own texels, byte for byte (`Rgba16Float` stays f16): re-uploads
    /// are bit-exact.
    #[default]
    Native,
    /// `Rgba16Float` frames as 8-bit sRGB-encoded colour with linear alpha, re-uploaded
    /// as `Rgba8UnormSrgb` (sampling decodes back to linear light): half the bytes,
    /// values clipped to 0..1 and quantised. Other formats stay as they are.
    Srgb8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCacheConfig {
    /// Bytes of textures held in VRAM.
    pub vram_ceiling: u64,
    /// Bytes of spilled frames held in RAM.
    pub ram_ceiling: u64,
    pub spill_format: SpillFormat,
    /// Staging buffers for spills in flight; each grows to the largest frame spilled.
    pub spill_slots: usize,
}

impl FrameCacheConfig {
    /// Share of the GPU's working set the VRAM tier takes by default.
    pub const VRAM_SHARE: f64 = 1.0 / 6.0;
    /// Share of physical memory the RAM tier takes by default; halved on unified memory,
    /// where the VRAM tier already lives in the same memory.
    pub const RAM_SHARE: f64 = 1.0 / 8.0;
    /// Ceiling used when the platform does not report a size.
    pub const FALLBACK_CEILING: u64 = 1 << 30;
    /// Three spills in flight cover one eviction per frame landing two frames later.
    pub const SPILL_SLOTS: usize = 3;

    /// Defaults scaled from what the platform reports (see [`interop::memory_report`]).
    pub fn from_memory(report: MemoryReport) -> Self {
        let share = |bytes: Option<u64>, share: f64| {
            bytes.map_or(Self::FALLBACK_CEILING, |b| (b as f64 * share) as u64)
        };
        let ram_share = if report.unified {
            Self::RAM_SHARE / 2.0
        } else {
            Self::RAM_SHARE
        };
        Self {
            vram_ceiling: share(report.gpu_working_set, Self::VRAM_SHARE),
            ram_ceiling: share(report.system, ram_share),
            spill_format: SpillFormat::Native,
            spill_slots: Self::SPILL_SLOTS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Vram,
    Ram,
}

/// CPU cost of re-uploading a frame from RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadCost {
    pub bytes: u64,
    /// Texture creation plus `queue.write_texture` (copying into wgpu's staging memory);
    /// the GPU copy itself runs with the next submission.
    pub cpu: Duration,
}

#[derive(Clone, Debug)]
pub struct CacheHit {
    pub texture: wgpu::Texture,
    pub from: Tier,
    /// Set for a RAM hit.
    pub upload: Option<UploadCost>,
}

/// Counters since the cache was created, and the current occupancy, for the debug panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCacheStats {
    pub vram_hits: u64,
    pub ram_hits: u64,
    pub misses: u64,
    pub inserts: u64,
    /// Frames evicted from VRAM (spilled or dropped).
    pub vram_evictions: u64,
    /// VRAM evictions that landed in RAM.
    pub spilled: u64,
    /// VRAM evictions that never reached RAM (see the module docs).
    pub spill_dropped: u64,
    /// Frames dropped from RAM by the LRU.
    pub ram_evictions: u64,
    /// Frames removed by segment invalidation or `clear`.
    pub invalidated: u64,
    /// Bytes and total CPU time of RAM hits' re-uploads.
    pub uploaded_bytes: u64,
    pub upload_time: Duration,
    pub vram_frames: usize,
    pub ram_frames: usize,
    /// Spills recorded and not yet landed.
    pub spilling: usize,
    pub vram_bytes: u64,
    pub ram_bytes: u64,
    pub vram_ceiling: u64,
    pub ram_ceiling: u64,
}

impl FrameCacheStats {
    /// Hits over lookups, 0 before the first lookup.
    pub fn hit_rate(&self) -> f64 {
        let hits = self.vram_hits + self.ram_hits;
        let lookups = hits + self.misses;
        if lookups == 0 {
            0.0
        } else {
            hits as f64 / lookups as f64
        }
    }
}

struct RamFrame {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    data: Vec<u8>,
}

enum Stored {
    Vram(wgpu::Texture),
    Ram(RamFrame),
}

struct Entry {
    tick: u64,
    bytes: u64,
    stored: Stored,
}

struct PendingSpill {
    id: u64,
    tick: u64,
    /// `None` once the frame was invalidated while its copy was in flight.
    key: Option<FrameKey>,
}

pub struct FrameCache {
    config: FrameCacheConfig,
    entries: HashMap<FrameKey, Entry>,
    vram_lru: BTreeMap<u64, FrameKey>,
    ram_lru: BTreeMap<u64, FrameKey>,
    clock: u64,
    vram_bytes: u64,
    ram_bytes: u64,
    ring: ReadbackRing,
    pending: VecDeque<PendingSpill>,
    stats: FrameCacheStats,
}

impl std::fmt::Debug for FrameCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameCache")
            .field("config", &self.config)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// Usage of re-uploaded textures: sampled, and copyable so they can spill again.
const UPLOAD_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

impl FrameCache {
    pub fn new(config: FrameCacheConfig) -> Self {
        Self {
            config,
            entries: HashMap::new(),
            vram_lru: BTreeMap::new(),
            ram_lru: BTreeMap::new(),
            clock: 0,
            vram_bytes: 0,
            ram_bytes: 0,
            ring: ReadbackRing::new(ReadbackConfig {
                slots: config.spill_slots,
                policy: Backpressure::Lossless,
            }),
            pending: VecDeque::new(),
            stats: FrameCacheStats::default(),
        }
    }

    /// A cache with ceilings scaled from the device's and the machine's memory.
    pub fn for_device(device: &wgpu::Device) -> Self {
        Self::new(FrameCacheConfig::from_memory(interop::memory_report(
            device,
        )))
    }

    pub fn config(&self) -> FrameCacheConfig {
        self.config
    }

    pub fn stats(&self) -> FrameCacheStats {
        let vram_frames = self.vram_lru.len();
        FrameCacheStats {
            vram_frames,
            ram_frames: self.entries.len() - vram_frames,
            spilling: self.pending.len(),
            vram_bytes: self.vram_bytes,
            ram_bytes: self.ram_bytes,
            vram_ceiling: self.config.vram_ceiling,
            ram_ceiling: self.config.ram_ceiling,
            ..self.stats
        }
    }

    /// Where `key` is resident, without counting a lookup or refreshing its recency.
    pub fn contains(&self, key: &FrameKey) -> Option<Tier> {
        self.entries.get(key).map(|entry| entry.tier())
    }

    /// Every resident frame and its tier, in no particular order.
    pub fn keys(&self) -> impl Iterator<Item = (FrameKey, Tier)> + '_ {
        self.entries.iter().map(|(key, entry)| (*key, entry.tier()))
    }

    /// Caches a rendered frame, replacing any frame with the same key. The cache owns the
    /// texture from here on: nothing may render into it again. It needs `COPY_SRC` to
    /// spill (otherwise it is dropped when VRAM evicts it). A frame larger than the VRAM
    /// ceiling goes straight to the RAM tier.
    pub fn insert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: FrameKey,
        texture: wgpu::Texture,
    ) {
        self.stats.inserts += 1;
        self.remove(&key);
        let bytes = TextureDesc::of(&texture).bytes();
        let tick = self.tick();
        if bytes > self.config.vram_ceiling {
            self.stats.vram_evictions += 1;
            self.spill(device, queue, vec![(key, tick, texture)]);
            return;
        }
        self.add_vram(key, tick, bytes, texture);
        self.evict_vram(device, queue);
    }

    /// The frame for `key`, refreshing its recency. A RAM hit uploads it into a new
    /// texture (`Rgba8UnormSrgb` for frames spilled as [`SpillFormat::Srgb8`]) and moves
    /// it back to VRAM.
    pub fn lookup(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: &FrameKey,
    ) -> Option<CacheHit> {
        let tick = self.tick();
        let Some(entry) = self.entries.get_mut(key) else {
            self.stats.misses += 1;
            return None;
        };
        let old_tick = std::mem::replace(&mut entry.tick, tick);
        let frame = match &entry.stored {
            Stored::Vram(texture) => {
                let texture = texture.clone();
                self.vram_lru.remove(&old_tick);
                self.vram_lru.insert(tick, *key);
                self.stats.vram_hits += 1;
                return Some(CacheHit {
                    texture,
                    from: Tier::Vram,
                    upload: None,
                });
            }
            Stored::Ram(frame) => frame,
        };
        let started = Instant::now();
        let texture = upload(device, queue, frame);
        let cost = UploadCost {
            bytes: frame.data.len() as u64,
            cpu: started.elapsed(),
        };
        self.stats.ram_hits += 1;
        self.stats.uploaded_bytes += cost.bytes;
        self.stats.upload_time += cost.cpu;
        let vram_bytes = TextureDesc::of(&texture).bytes();
        self.ram_lru.remove(&old_tick);
        if vram_bytes > self.config.vram_ceiling {
            // Too large for VRAM: it stays in RAM, now the most recent frame there.
            self.ram_lru.insert(tick, *key);
        } else {
            self.ram_bytes -= entry.bytes;
            entry.bytes = vram_bytes;
            entry.stored = Stored::Vram(texture.clone());
            self.vram_bytes += vram_bytes;
            self.vram_lru.insert(tick, *key);
            self.evict_vram(device, queue);
        }
        Some(CacheHit {
            texture,
            from: Tier::Ram,
            upload: Some(cost),
        })
    }

    /// Moves finished spills into RAM. Never waits; call once per rendered frame.
    pub fn maintain(&mut self, device: &wgpu::Device) {
        while !self.pending.is_empty() {
            let format = self.config.spill_format;
            match self
                .ring
                .poll_with(device, |view| (view.id, pack(&view, format)))
            {
                Ok(Some((id, frame))) => {
                    let spill = self.pop_pending(id);
                    if let Some(key) = spill.key {
                        self.land(key, spill.tick, frame);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "frame cache spill failed");
                    let id = match error {
                        readback::ReadbackError::MapFailed { id }
                        | readback::ReadbackError::NotSubmitted { id } => id,
                    };
                    self.pop_pending(id);
                    self.stats.spill_dropped += 1;
                }
            }
        }
    }

    /// Waits until every spill in flight has landed. For tests and shutdown, never the UI
    /// thread.
    pub fn finish_spills(&mut self, device: &wgpu::Device) {
        while !self.pending.is_empty() {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            let before = self.pending.len();
            self.maintain(device);
            if self.pending.len() == before {
                break;
            }
        }
    }

    /// Changes the VRAM ceiling; lowering it spills the oldest frames at once.
    pub fn set_vram_ceiling(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: u64) {
        self.config.vram_ceiling = bytes;
        self.evict_vram(device, queue);
    }

    /// Changes the RAM ceiling; lowering it drops the oldest spilled frames at once.
    pub fn set_ram_ceiling(&mut self, bytes: u64) {
        self.config.ram_ceiling = bytes;
        self.evict_ram();
    }

    /// Applies to frames spilled from now on; frames already in RAM keep their format.
    pub fn set_spill_format(&mut self, format: SpillFormat) {
        self.config.spill_format = format;
    }

    /// Drops every frame of `segment` from both tiers and from the spills in flight.
    /// Returns how many resident frames were removed.
    pub fn invalidate_segment(&mut self, segment: SegmentHash) -> usize {
        let keys: Vec<FrameKey> = self
            .entries
            .keys()
            .filter(|key| key.segment == segment)
            .copied()
            .collect();
        for key in &keys {
            self.remove(key);
        }
        for spill in &mut self.pending {
            if spill.key.is_some_and(|key| key.segment == segment) {
                spill.key = None;
            }
        }
        self.stats.invalidated += keys.len() as u64;
        keys.len()
    }

    /// Drops every frame, including spills in flight.
    pub fn clear(&mut self) {
        self.stats.invalidated += self.entries.len() as u64;
        self.entries.clear();
        self.vram_lru.clear();
        self.ram_lru.clear();
        self.vram_bytes = 0;
        self.ram_bytes = 0;
        for spill in &mut self.pending {
            spill.key = None;
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn add_vram(&mut self, key: FrameKey, tick: u64, bytes: u64, texture: wgpu::Texture) {
        self.entries.insert(
            key,
            Entry {
                tick,
                bytes,
                stored: Stored::Vram(texture),
            },
        );
        self.vram_lru.insert(tick, key);
        self.vram_bytes += bytes;
    }

    fn remove(&mut self, key: &FrameKey) {
        let Some(entry) = self.entries.remove(key) else {
            return;
        };
        match entry.stored {
            Stored::Vram(_) => {
                self.vram_lru.remove(&entry.tick);
                self.vram_bytes -= entry.bytes;
            }
            Stored::Ram(_) => {
                self.ram_lru.remove(&entry.tick);
                self.ram_bytes -= entry.bytes;
            }
        }
    }

    fn evict_vram(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut victims = Vec::new();
        while self.vram_bytes > self.config.vram_ceiling {
            let Some((tick, key)) = self.vram_lru.pop_first() else {
                break;
            };
            let Some(Entry {
                bytes,
                stored: Stored::Vram(texture),
                ..
            }) = self.entries.remove(&key)
            else {
                unreachable!("the VRAM list holds VRAM entries")
            };
            self.vram_bytes -= bytes;
            self.stats.vram_evictions += 1;
            victims.push((key, tick, texture));
        }
        if !victims.is_empty() {
            self.spill(device, queue, victims);
        }
    }

    /// Records copies of evicted textures into the spill ring, in one submission.
    fn spill(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        victims: Vec<(FrameKey, u64, wgpu::Texture)>,
    ) {
        // Landed spills free their staging buffers for this batch.
        self.maintain(device);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame cache spill"),
        });
        let mut recorded = false;
        for (key, tick, texture) in victims {
            let fits = spilled_bytes(&texture, self.config.spill_format)
                .is_some_and(|bytes| bytes <= self.config.ram_ceiling);
            let id = if fits {
                self.ring
                    .record(device, &mut encoder, &texture, key.frame)
                    .ok()
            } else {
                None
            };
            match id {
                Some(id) => {
                    recorded = true;
                    self.pending.push_back(PendingSpill {
                        id,
                        tick,
                        key: Some(key),
                    });
                }
                None => self.stats.spill_dropped += 1,
            }
        }
        if recorded {
            queue.submit([encoder.finish()]);
        }
    }

    fn pop_pending(&mut self, id: u64) -> PendingSpill {
        let spill = self
            .pending
            .pop_front()
            .expect("the ring only delivers recorded spills");
        debug_assert_eq!(spill.id, id, "spills land in submission order");
        spill
    }

    fn land(&mut self, key: FrameKey, tick: u64, frame: RamFrame) {
        let bytes = frame.data.len() as u64;
        // A newer copy was inserted while this one was in flight, or it can never fit.
        if self.entries.contains_key(&key) || bytes > self.config.ram_ceiling {
            self.stats.spill_dropped += 1;
            return;
        }
        self.entries.insert(
            key,
            Entry {
                tick,
                bytes,
                stored: Stored::Ram(frame),
            },
        );
        self.ram_lru.insert(tick, key);
        self.ram_bytes += bytes;
        self.stats.spilled += 1;
        self.evict_ram();
    }

    fn evict_ram(&mut self) {
        while self.ram_bytes > self.config.ram_ceiling {
            let Some((_, key)) = self.ram_lru.pop_first() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&key) {
                self.ram_bytes -= entry.bytes;
            }
            self.stats.ram_evictions += 1;
        }
    }
}

impl Entry {
    fn tier(&self) -> Tier {
        match self.stored {
            Stored::Vram(_) => Tier::Vram,
            Stored::Ram(_) => Tier::Ram,
        }
    }
}

/// Format a texture is stored in once spilled with `format`.
fn spilled_format(texture: wgpu::TextureFormat, format: SpillFormat) -> wgpu::TextureFormat {
    match (texture, format) {
        (wgpu::TextureFormat::Rgba16Float, SpillFormat::Srgb8) => {
            wgpu::TextureFormat::Rgba8UnormSrgb
        }
        (texture, _) => texture,
    }
}

/// RAM bytes of `texture` once spilled, `None` if the ring cannot read it back.
fn spilled_bytes(texture: &wgpu::Texture, format: SpillFormat) -> Option<u64> {
    if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
        return None;
    }
    readback::bytes_per_pixel(texture.format())?;
    let stored = spilled_format(texture.format(), format);
    let texel = u64::from(readback::bytes_per_pixel(stored)?);
    Some(u64::from(texture.width()) * u64::from(texture.height()) * texel)
}

/// Packs a landed spill into its RAM form.
fn pack(view: &FrameView<'_>, format: SpillFormat) -> RamFrame {
    let stored = spilled_format(view.format, format);
    let texel = readback::bytes_per_pixel(stored).unwrap_or(0) as usize;
    let mut data = Vec::with_capacity(view.width as usize * view.height as usize * texel);
    if stored == view.format {
        for y in 0..view.height {
            data.extend_from_slice(view.row(y));
        }
    } else {
        let (colour, alpha) = srgb8_tables();
        for y in 0..view.height {
            for px in view.row(y).as_chunks::<8>().0 {
                let c = |i: usize| usize::from(u16::from_le_bytes([px[2 * i], px[2 * i + 1]]));
                data.extend_from_slice(&[colour[c(0)], colour[c(1)], colour[c(2)], alpha[c(3)]]);
            }
        }
    }
    RamFrame {
        width: view.width,
        height: view.height,
        format: stored,
        data,
    }
}

/// f16 bit pattern to an 8-bit code: sRGB-encoded (colour) and linear (alpha), values
/// clamped to 0..1, NaN to 0.
fn srgb8_tables() -> &'static (Box<[u8]>, Box<[u8]>) {
    static TABLES: OnceLock<(Box<[u8]>, Box<[u8]>)> = OnceLock::new();
    TABLES.get_or_init(|| {
        let unit = |bits: usize| {
            let v = readback::f16_to_f32(bits as u16);
            if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
        };
        let colour = (0..1 << 16)
            .map(|bits| (srgb_encode(unit(bits)) * 255.0).round() as u8)
            .collect();
        let alpha = (0..1 << 16)
            .map(|bits| (unit(bits) * 255.0).round() as u8)
            .collect();
        (colour, alpha)
    })
}

/// The sRGB transfer function (IEC 61966-2-1), linear 0..1 to encoded 0..1.
pub fn srgb_encode(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn upload(device: &wgpu::Device, queue: &wgpu::Queue, frame: &RamFrame) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width: frame.width,
        height: frame.height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cached frame"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: frame.format,
        usage: UPLOAD_USAGE,
        view_formats: &[],
    });
    let texel = readback::bytes_per_pixel(frame.format).unwrap_or(0);
    queue.write_texture(
        texture.as_image_copy(),
        &frame.data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(frame.width * texel),
            rows_per_image: Some(frame.height),
        },
        size,
    );
    texture
}
