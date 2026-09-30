//! The GPU glyph atlas: one `R8Unorm` coverage texture holding rasterised glyph fills and
//! outline strokes, packed in shelves. A glyph is rasterised once and reused across
//! frames. When a new glyph does not fit, the atlas doubles (up to its maximum size),
//! copying what it holds; at the maximum it evicts every glyph not used in the current
//! frame and repacks the rest on the GPU.
//!
//! Each glyph is stored with a one-pixel empty border, so bilinear sampling of a scaled or
//! rotated quad fades to zero at its edges.

use std::collections::HashMap;

use cosmic_text::CacheKey;

use super::layout::{TextEngine, TextLayout};
use super::nodes::GlyphPipeline;
use super::{GlyphInfo, GlyphTransform, TextPaint};

/// A rasterised image in the atlas: a glyph fill (`stroke == 0`) or an outline stroke of
/// `stroke / 16` pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub glyph: CacheKey,
    pub stroke: u32,
}

impl GlyphKey {
    /// Stroke widths are kept in sixteenths of a pixel.
    pub const STROKE_UNITS: f32 = 16.0;

    pub fn fill(glyph: CacheKey) -> Self {
        Self { glyph, stroke: 0 }
    }

    /// A stroke `width` pixels wide centred on the outline.
    pub fn stroke(glyph: CacheKey, width: f32) -> Self {
        Self {
            glyph,
            stroke: (width * Self::STROKE_UNITS).round().max(1.0) as u32,
        }
    }

    pub fn stroke_width(&self) -> f32 {
        self.stroke as f32 / Self::STROKE_UNITS
    }
}

/// Where a glyph image sits in the atlas and relative to its origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasEntry {
    /// Top-left of the image (inside its border) in atlas pixels.
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Offset of the image's top-left from the glyph origin: `left` right, `top` up.
    pub left: i32,
    pub top: i32,
    last_used: u64,
}

impl AtlasEntry {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Counters since the atlas was created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasStats {
    /// Glyph lookups served from the atlas.
    pub hits: u64,
    /// Glyphs rasterised and uploaded.
    pub misses: u64,
    /// Glyphs dropped to make room.
    pub evictions: u64,
    /// Times the texture doubled.
    pub grows: u32,
    /// Times the atlas was repacked after evicting.
    pub repacks: u32,
    /// Glyphs left out because even an emptied atlas could not hold this frame's glyphs.
    pub overflowed: u64,
}

/// Shelf packing: rows of a fixed height filled left to right.
#[derive(Clone, Debug)]
struct Packer {
    width: u32,
    height: u32,
    shelves: Vec<Shelf>,
    bottom: u32,
}

#[derive(Clone, Copy, Debug)]
struct Shelf {
    y: u32,
    height: u32,
    x: u32,
}

impl Packer {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            shelves: Vec::new(),
            bottom: 0,
        }
    }

    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.width || h > self.height {
            return None;
        }
        // Rows come in heights of 4 so glyphs of similar size share them.
        let row = h.next_multiple_of(4);
        let best = self
            .shelves
            .iter_mut()
            .filter(|s| s.height >= h && s.height <= row * 2 && s.x + w <= self.width)
            .min_by_key(|s| s.height);
        if let Some(shelf) = best {
            let at = (shelf.x, shelf.y);
            shelf.x += w;
            return Some(at);
        }
        if self.bottom + row > self.height {
            return None;
        }
        let shelf = Shelf {
            y: self.bottom,
            height: row,
            x: w,
        };
        self.bottom += row;
        self.shelves.push(shelf);
        Some((0, shelf.y))
    }

    fn grow(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
    }

    fn reset(&mut self) {
        self.shelves.clear();
        self.bottom = 0;
    }
}

/// One glyph image to draw: which atlas image, where its origin lands on the canvas, and
/// its per-glyph transform.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedGlyph {
    pub key: GlyphKey,
    /// The glyph origin (pen position on the baseline) in canvas pixels.
    pub origin: [i32; 2],
    /// The point the transform scales and rotates about, in canvas pixels.
    pub pivot: [f32; 2],
    pub transform: GlyphTransform,
    /// 0 for the fill coverage channel, 1 for the outline channel.
    pub channel: u32,
}

/// A text ready to draw: its glyph images are in the atlas. Valid until the atlas's next
/// [`GlyphAtlas::begin_frame`].
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedText {
    pub glyphs: Vec<PreparedGlyph>,
    pub paint: TextPaint,
    /// The frame it was prepared in.
    pub frame: u64,
}

pub struct GlyphAtlas {
    device: wgpu::Device,
    queue: wgpu::Queue,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: u32,
    max_size: u32,
    packer: Packer,
    entries: HashMap<GlyphKey, AtlasEntry>,
    frame: u64,
    stats: AtlasStats,
    pipeline: GlyphPipeline,
}

impl std::fmt::Debug for GlyphAtlas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlyphAtlas")
            .field("size", &self.size)
            .field("entries", &self.entries.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl GlyphAtlas {
    pub const INITIAL_SIZE: u32 = 512;
    pub const MAX_SIZE: u32 = 4096;

    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::with_sizes(device, queue, Self::INITIAL_SIZE, Self::MAX_SIZE)
    }

    /// An atlas starting at `initial` pixels square that grows up to `max` (both clamped
    /// to the device's texture limit).
    pub fn with_sizes(device: &wgpu::Device, queue: &wgpu::Queue, initial: u32, max: u32) -> Self {
        let limit = device.limits().max_texture_dimension_2d;
        let max_size = max.clamp(16, limit);
        let size = initial.clamp(16, max_size);
        let texture = create_texture(device, size);
        Self {
            device: device.clone(),
            queue: queue.clone(),
            view: texture.create_view(&Default::default()),
            texture,
            size,
            max_size,
            packer: Packer::new(size, size),
            entries: HashMap::new(),
            frame: 0,
            stats: AtlasStats::default(),
            pipeline: GlyphPipeline::new(device),
        }
    }

    /// Starts a new frame: glyphs used from now on are protected from eviction until the
    /// next call; texts prepared before it may no longer draw.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Side of the square texture in pixels.
    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn stats(&self) -> AtlasStats {
        self.stats
    }

    pub(super) fn pipeline(&self) -> &GlyphPipeline {
        &self.pipeline
    }

    pub fn entry(&self, key: &GlyphKey) -> Option<&AtlasEntry> {
        self.entries.get(key)
    }

    /// Puts every glyph of `layout` (and its outline, when `paint` has one) in the atlas
    /// and returns the draw list. `origin` in `paint` places the layout's top-left on the
    /// canvas; `transform`, when given, is asked for each glyph's offset, scale, rotation
    /// and opacity (the text animator's hook).
    pub fn prepare(
        &mut self,
        engine: &mut TextEngine,
        layout: &TextLayout,
        paint: &TextPaint,
        transform: Option<&dyn Fn(&GlyphInfo) -> GlyphTransform>,
    ) -> PreparedText {
        let outline = paint.outline.filter(|o| o.width > 0.0);
        let mut glyphs =
            Vec::with_capacity(layout.glyphs.len() * (1 + usize::from(outline.is_some())));
        let [ox, oy] = paint.origin;
        for (index, laid) in layout.glyphs.iter().enumerate() {
            let line = &layout.lines[laid.line];
            let physical = laid.glyph.physical((ox, oy + line.baseline), 1.0);
            let info = GlyphInfo {
                index,
                line: laid.line,
                cluster: laid.cluster(),
                rtl: laid.rtl(),
                advance: laid.glyph.w,
                pivot: [
                    ox + laid.glyph.x + laid.glyph.w / 2.0,
                    oy + line.top + line.height / 2.0,
                ],
            };
            let t = transform.map_or(GlyphTransform::IDENTITY, |f| f(&info));
            if t.opacity <= 0.0 {
                continue;
            }
            let origin = [physical.x, physical.y];
            if let Some(outline) = outline {
                // The stroke is centred on the outline, so twice the width puts `width`
                // pixels outside the glyph.
                let key = GlyphKey::stroke(physical.cache_key, outline.width * 2.0);
                if self.ensure(engine, key) {
                    glyphs.push(PreparedGlyph {
                        key,
                        origin,
                        pivot: info.pivot,
                        transform: t,
                        channel: 1,
                    });
                }
            }
            let key = GlyphKey::fill(physical.cache_key);
            if self.ensure(engine, key) {
                glyphs.push(PreparedGlyph {
                    key,
                    origin,
                    pivot: info.pivot,
                    transform: t,
                    channel: 0,
                });
            }
        }
        PreparedText {
            glyphs,
            paint: paint.clone(),
            frame: self.frame,
        }
    }

    /// Makes sure `key` is in the atlas and marks it used this frame. `false` when it has
    /// nothing to draw (a space, a missing font) or does not fit.
    fn ensure(&mut self, engine: &mut TextEngine, key: GlyphKey) -> bool {
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = self.frame;
            self.stats.hits += 1;
            return !entry.is_empty();
        }
        self.stats.misses += 1;
        let mask = engine
            .rasterize(key.glyph, key.stroke_width())
            .unwrap_or_default();
        let mut entry = AtlasEntry {
            x: 0,
            y: 0,
            width: mask.width,
            height: mask.height,
            left: mask.left,
            top: mask.top,
            last_used: self.frame,
        };
        if mask.is_empty() {
            self.entries.insert(key, entry);
            return false;
        }
        let (pw, ph) = (mask.width + 2, mask.height + 2);
        let Some((x, y)) = self.alloc(pw, ph) else {
            self.stats.overflowed += 1;
            tracing::warn!(
                width = mask.width,
                height = mask.height,
                "glyph does not fit in the atlas"
            );
            return false;
        };
        entry.x = x + 1;
        entry.y = y + 1;
        let mut padded = vec![0u8; (pw * ph) as usize];
        for row in 0..mask.height as usize {
            let src = &mask.data[row * mask.width as usize..][..mask.width as usize];
            padded[(row + 1) * pw as usize + 1..][..mask.width as usize].copy_from_slice(src);
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pw),
                rows_per_image: Some(ph),
            },
            wgpu::Extent3d {
                width: pw,
                height: ph,
                depth_or_array_layers: 1,
            },
        );
        self.entries.insert(key, entry);
        true
    }

    /// A `w` x `h` rectangle, growing or repacking the atlas when needed.
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        loop {
            if let Some(at) = self.packer.alloc(w, h) {
                return Some(at);
            }
            if self.size < self.max_size {
                self.grow((self.size * 2).min(self.max_size));
            } else if self.entries.values().any(|e| e.last_used < self.frame) {
                self.evict_and_repack();
            } else {
                return None;
            }
        }
    }

    fn grow(&mut self, size: u32) {
        let texture = create_texture(&self.device, size);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glyph atlas grow"),
            });
        encoder.copy_texture_to_texture(
            self.texture.as_image_copy(),
            texture.as_image_copy(),
            self.texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        self.view = texture.create_view(&Default::default());
        self.texture = texture;
        self.size = size;
        self.packer.grow(size, size);
        self.stats.grows += 1;
    }

    /// Drops every glyph not used this frame and copies the rest, tallest first, into a
    /// fresh texture.
    fn evict_and_repack(&mut self) {
        let before = self.entries.len();
        let frame = self.frame;
        self.entries.retain(|_, e| e.last_used >= frame);
        self.stats.evictions += (before - self.entries.len()) as u64;
        self.stats.repacks += 1;

        let texture = create_texture(&self.device, self.size);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glyph atlas repack"),
            });
        self.packer.reset();
        let mut keys: Vec<GlyphKey> = self
            .entries
            .iter()
            .filter(|(_, e)| !e.is_empty())
            .map(|(k, _)| *k)
            .collect();
        keys.sort_by_key(|k| std::cmp::Reverse(self.entries[k].height));
        for key in keys {
            let entry = self.entries[&key];
            let (pw, ph) = (entry.width + 2, entry.height + 2);
            // Everything fitted before at this size, so it fits again.
            let Some((x, y)) = self.packer.alloc(pw, ph) else {
                self.entries.remove(&key);
                continue;
            };
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: entry.x - 1,
                        y: entry.y - 1,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: pw,
                    height: ph,
                    depth_or_array_layers: 1,
                },
            );
            if let Some(e) = self.entries.get_mut(&key) {
                e.x = x + 1;
                e.y = y + 1;
            }
        }
        self.queue.submit([encoder.finish()]);
        self.view = texture.create_view(&Default::default());
        self.texture = texture;
    }
}

fn create_texture(device: &wgpu::Device, size: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph atlas"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::Packer;

    #[test]
    fn packer_fills_shelves_and_refuses_when_full() {
        let mut p = Packer::new(32, 32);
        assert_eq!(p.alloc(10, 7), Some((0, 0)));
        assert_eq!(p.alloc(10, 8), Some((10, 0)), "same row height");
        assert_eq!(p.alloc(20, 16), Some((0, 8)), "new shelf");
        assert_eq!(p.alloc(40, 4), None, "wider than the atlas");
        assert_eq!(p.alloc(8, 8), Some((20, 0)));
        assert_eq!(p.alloc(30, 9), None, "no height left");
        p.grow(64, 64);
        assert_eq!(
            p.alloc(30, 9),
            Some((20, 8)),
            "the wider shelf has room now"
        );
        p.reset();
        assert_eq!(p.alloc(5, 5), Some((0, 0)));
    }
}
