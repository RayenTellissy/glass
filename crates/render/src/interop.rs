//! Platform frame handles as wgpu textures, without a CPU copy. Every `unsafe` call of
//! `render` lives here.
//!
//! macOS: a VideoToolbox frame's `IOSurface` becomes one Metal texture per plane
//! (`newTextureWithDescriptor:iosurface:plane:`), wrapped as a wgpu texture through
//! wgpu-hal (`create_texture_from_hal`). VideoToolbox recycles a small pool of surfaces,
//! so the wrapped textures are cached by [`SurfaceKey`]: (surface id, width, height,
//! format). A cached Metal texture retains its surface, so an id cannot be reused by a
//! different surface while its entry lives; the entry also remembers the surface pointer
//! and is rebuilt if it ever differs.

use std::collections::HashMap;

use media::{HwFrame, HwSurfaceFormat};

/// Why a frame could not be imported.
#[derive(Debug, thiserror::Error)]
pub enum InteropError {
    #[error("the wgpu device does not use the {0} backend")]
    WrongBackend(&'static str),
    #[error("the frame has no {0} surface")]
    NoSurface(&'static str),
    #[error("{format} surfaces need the wgpu feature {feature:?}")]
    MissingFeature {
        format: HwSurfaceFormat,
        feature: wgpu::Features,
    },
    #[error("creating a texture for plane {plane} of a {format} surface failed")]
    Texture {
        plane: usize,
        format: HwSurfaceFormat,
    },
    #[error("importing {0} frames is not supported on this platform yet")]
    Unsupported(&'static str),
}

/// What identifies a cached surface import.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceKey {
    pub surface_id: u32,
    pub width: u32,
    pub height: u32,
    pub format: HwSurfaceFormat,
}

/// A hardware frame's planes as wgpu textures: plane 0 is luma (`R8Unorm` or `R16Unorm`),
/// plane 1 interleaved chroma (`Rg8Unorm` or `Rg16Unorm`), 16-bit formats holding
/// 10-bit samples in their high bits.
///
/// It holds the frame, which keeps the surface out of the decoder's pool: keep it alive
/// until the GPU work that reads the textures has completed, then drop it.
#[derive(Debug)]
pub struct ImportedFrame {
    pub luma: wgpu::TextureView,
    pub chroma: wgpu::TextureView,
    pub luma_texture: wgpu::Texture,
    pub chroma_texture: wgpu::Texture,
    pub key: SurfaceKey,
    frame: HwFrame,
}

impl ImportedFrame {
    pub fn frame(&self) -> &HwFrame {
        &self.frame
    }
}

struct CachedSurface {
    surface: usize,
    luma: wgpu::Texture,
    chroma: wgpu::Texture,
    last_used: u64,
}

/// Imports hardware frames as textures and caches the wrappers per surface.
///
/// Owned by the render thread. The cache holds at most `capacity` surfaces (least
/// recently used out first); VideoToolbox pools hold 5 to 20.
pub struct SurfaceImporter {
    cache: HashMap<SurfaceKey, CachedSurface>,
    capacity: usize,
    clock: u64,
    hits: u64,
    misses: u64,
}

impl std::fmt::Debug for SurfaceImporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceImporter")
            .field("cached", &self.cache.len())
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .finish()
    }
}

impl Default for SurfaceImporter {
    fn default() -> Self {
        Self::new(32)
    }
}

impl SurfaceImporter {
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: HashMap::new(),
            capacity: capacity.max(1),
            clock: 0,
            hits: 0,
            misses: 0,
        }
    }

    /// Cache hits and misses so far.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    /// Drops every cached wrapper (for example when the device changes).
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// The wgpu features a device needs to import `format`.
    pub fn required_features(format: HwSurfaceFormat) -> wgpu::Features {
        if format.bytes_per_sample() == 2 {
            wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
        } else {
            wgpu::Features::empty()
        }
    }

    /// Wraps the frame's planes as textures on `device`, reusing the cached wrappers of
    /// its surface when there are any. No pixel is copied.
    pub fn import(
        &mut self,
        device: &wgpu::Device,
        frame: &HwFrame,
    ) -> Result<ImportedFrame, InteropError> {
        let format = frame.format();
        let feature = Self::required_features(format);
        if !device.features().contains(feature) {
            return Err(InteropError::MissingFeature { format, feature });
        }
        self.import_platform(device, frame)
    }

    fn remember(&mut self, key: SurfaceKey, entry: CachedSurface) {
        if self.cache.len() >= self.capacity
            && let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| *k)
        {
            self.cache.remove(&oldest);
        }
        self.cache.insert(key, entry);
    }

    fn lookup(
        &mut self,
        key: &SurfaceKey,
        surface: usize,
    ) -> Option<(wgpu::Texture, wgpu::Texture)> {
        self.clock += 1;
        let clock = self.clock;
        match self.cache.get_mut(key) {
            Some(entry) if entry.surface == surface => {
                entry.last_used = clock;
                self.hits += 1;
                Some((entry.luma.clone(), entry.chroma.clone()))
            }
            _ => {
                self.misses += 1;
                None
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn import_platform(
        &mut self,
        device: &wgpu::Device,
        frame: &HwFrame,
    ) -> Result<ImportedFrame, InteropError> {
        let surface = frame
            .io_surface()
            .ok_or(InteropError::NoSurface("IOSurface"))?;
        let key = SurfaceKey {
            surface_id: surface.id(),
            width: frame.width(),
            height: frame.height(),
            format: frame.format(),
        };
        let pointer = objc2_core_foundation::CFRetained::as_ptr(&surface).as_ptr() as usize;
        let (luma, chroma) = match self.lookup(&key, pointer) {
            Some(planes) => planes,
            None => {
                let luma = metal::wrap_plane(device, frame, &surface, 0)?;
                let chroma = metal::wrap_plane(device, frame, &surface, 1)?;
                self.remember(
                    key,
                    CachedSurface {
                        surface: pointer,
                        luma: luma.clone(),
                        chroma: chroma.clone(),
                        last_used: self.clock,
                    },
                );
                (luma, chroma)
            }
        };
        Ok(ImportedFrame {
            luma: luma.create_view(&wgpu::TextureViewDescriptor::default()),
            chroma: chroma.create_view(&wgpu::TextureViewDescriptor::default()),
            luma_texture: luma,
            chroma_texture: chroma,
            key,
            frame: frame.clone(),
        })
    }

    #[cfg(not(target_os = "macos"))]
    fn import_platform(
        &mut self,
        _device: &wgpu::Device,
        frame: &HwFrame,
    ) -> Result<ImportedFrame, InteropError> {
        Err(InteropError::Unsupported(frame.api().name()))
    }
}

/// wgpu formats of a surface's luma and chroma planes.
pub fn plane_formats(format: HwSurfaceFormat) -> (wgpu::TextureFormat, wgpu::TextureFormat) {
    if format.bytes_per_sample() == 2 {
        (
            wgpu::TextureFormat::R16Unorm,
            wgpu::TextureFormat::Rg16Unorm,
        )
    } else {
        (wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm)
    }
}

#[cfg(target_os = "macos")]
mod metal {
    use media::HwFrame;
    use objc2_io_surface::IOSurfaceRef;
    use objc2_metal::{
        MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureType,
        MTLTextureUsage,
    };

    use super::{InteropError, plane_formats};

    /// One plane of the frame's IOSurface as a wgpu texture sharing the surface's memory.
    pub(super) fn wrap_plane(
        device: &wgpu::Device,
        frame: &HwFrame,
        surface: &IOSurfaceRef,
        plane: usize,
    ) -> Result<wgpu::Texture, InteropError> {
        let format = frame.format();
        let (width, height) = frame
            .plane_size(plane)
            .ok_or(InteropError::Texture { plane, format })?;
        let (luma, chroma) = plane_formats(format);
        let (wgpu_format, mtl_format) = match (plane, format.bytes_per_sample()) {
            (0, 1) => (luma, MTLPixelFormat::R8Unorm),
            (0, _) => (luma, MTLPixelFormat::R16Unorm),
            (_, 1) => (chroma, MTLPixelFormat::RG8Unorm),
            (_, _) => (chroma, MTLPixelFormat::RG16Unorm),
        };
        // SAFETY: as_hal only borrows the device's Metal object for this call.
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or(InteropError::WrongBackend("Metal"))?;
        // SAFETY: a plain descriptor constructor with valid sizes.
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                mtl_format,
                width as usize,
                height as usize,
                false,
            )
        };
        desc.setUsage(MTLTextureUsage::ShaderRead);
        desc.setStorageMode(MTLStorageMode::Shared);
        let texture = hal
            .raw_device()
            .newTextureWithDescriptor_iosurface_plane(&desc, surface, plane)
            .ok_or(InteropError::Texture { plane, format })?;
        drop(hal);
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        // SAFETY: `texture` is a 2D, single-mip, single-layer Metal texture of exactly this
        // size and a pixel format matching `wgpu_format`, created on this device. wgpu
        // takes ownership of the Metal object; the texture is only ever sampled
        // (RESOURCE state), never written, so wgpu's state tracking holds.
        let texture = unsafe {
            let raw = wgpu::hal::metal::Device::texture_from_raw(
                texture,
                wgpu_format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
                None,
            );
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                raw,
                &wgpu::TextureDescriptor {
                    label: Some("hardware frame plane"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu_format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::TextureUses::RESOURCE,
            )
        };
        Ok(texture)
    }
}
