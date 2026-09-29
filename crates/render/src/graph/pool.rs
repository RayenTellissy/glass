//! Transient textures kept across frames. A compiled graph asks for `n` textures of each
//! description (its aliased physical slots); the pool hands back the same textures every
//! frame and creates more only when a graph needs more than it has seen. Frames on one
//! queue run in submission order, so reusing a texture in the next frame is safe.

use std::collections::HashMap;

/// Size and format of a transient texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

impl TextureDesc {
    /// The working format: linear light, premultiplied, `Rgba16Float`.
    pub const WORKING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

    pub fn working(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: Self::WORKING_FORMAT,
        }
    }

    pub fn of(texture: &wgpu::Texture) -> Self {
        Self {
            width: texture.width(),
            height: texture.height(),
            format: texture.format(),
        }
    }

    pub fn with_format(self, format: wgpu::TextureFormat) -> Self {
        Self { format, ..self }
    }

    /// Bytes of the texture's single mip level.
    pub fn bytes(&self) -> u64 {
        let texel = u64::from(self.format.block_copy_size(None).unwrap_or(4));
        u64::from(self.width) * u64::from(self.height) * texel
    }
}

/// Usage of every pooled texture: rendered to, sampled, and copied for readback.
pub const POOL_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

struct Entry {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    last_frame: u64,
}

#[derive(Default)]
pub struct TexturePool {
    textures: HashMap<TextureDesc, Vec<Entry>>,
    frame: u64,
    created: u64,
}

impl std::fmt::Debug for TexturePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TexturePool")
            .field("textures", &self.len())
            .field("bytes", &self.resident_bytes())
            .finish()
    }
}

impl TexturePool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Views for one frame's slots, in order; slots with equal descriptions get distinct
    /// textures.
    pub(crate) fn acquire(
        &mut self,
        device: &wgpu::Device,
        slots: &[TextureDesc],
    ) -> Vec<wgpu::TextureView> {
        self.frame += 1;
        let mut taken: HashMap<TextureDesc, usize> = HashMap::new();
        let mut views = Vec::with_capacity(slots.len());
        for desc in slots {
            let index = taken.entry(*desc).or_default();
            let entries = self.textures.entry(*desc).or_default();
            if entries.len() <= *index {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("graph transient"),
                    size: wgpu::Extent3d {
                        width: desc.width,
                        height: desc.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: desc.format,
                    usage: POOL_USAGE,
                    view_formats: &[],
                });
                let view = texture.create_view(&Default::default());
                entries.push(Entry {
                    _texture: texture,
                    view,
                    last_frame: 0,
                });
                self.created += 1;
            }
            let entry = &mut entries[*index];
            entry.last_frame = self.frame;
            views.push(entry.view.clone());
            *index += 1;
        }
        views
    }

    /// Drops textures no frame has used in the last `frames` frames.
    pub fn trim(&mut self, frames: u64) {
        let frame = self.frame;
        for entries in self.textures.values_mut() {
            entries.retain(|e| frame - e.last_frame < frames.max(1));
        }
        self.textures.retain(|_, v| !v.is_empty());
    }

    /// Number of textures held.
    pub fn len(&self) -> usize {
        self.textures.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Textures created since the pool was made (allocation churn).
    pub fn created(&self) -> u64 {
        self.created
    }

    /// Bytes of VRAM the pool holds.
    pub fn resident_bytes(&self) -> u64 {
        self.textures
            .iter()
            .map(|(d, v)| d.bytes() * v.len() as u64)
            .sum()
    }
}
