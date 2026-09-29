//! Every `unsafe` block of the prototype lives here: FFmpeg hwaccel setup,
//! CVPixelBuffer access, CoreVideo Metal texture cache, wgpu-hal interop and
//! getrusage.

use std::ptr::{self, NonNull};

use ffmpeg::ffi::{
    AVBufferRef, AVCodecContext, AVHWDeviceType, AVPixelFormat, av_buffer_ref, av_buffer_unref,
    av_hwdevice_ctx_create,
};
use ffmpeg_next as ffmpeg;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetIOSurface, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetPlaneCount, CVPixelBufferGetWidthOfPlane, kCVReturnSuccess,
};
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage,
};

pub struct HwDevice(*mut AVBufferRef);

// The device context is reference counted by FFmpeg and only read after creation.
unsafe impl Send for HwDevice {}

impl HwDevice {
    pub fn videotoolbox() -> Result<Self, String> {
        let mut ctx: *mut AVBufferRef = ptr::null_mut();
        let ret = unsafe {
            av_hwdevice_ctx_create(
                &mut ctx,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
                ptr::null(),
                ptr::null_mut(),
                0,
            )
        };
        if ret < 0 || ctx.is_null() {
            return Err(format!("av_hwdevice_ctx_create failed: {ret}"));
        }
        Ok(Self(ctx))
    }
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        unsafe { av_buffer_unref(&mut self.0) };
    }
}

unsafe extern "C" fn pick_videotoolbox(
    _ctx: *mut AVCodecContext,
    mut fmts: *const AVPixelFormat,
) -> AVPixelFormat {
    unsafe {
        let first = *fmts;
        while *fmts != AVPixelFormat::AV_PIX_FMT_NONE {
            if *fmts == AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX {
                return AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
            }
            fmts = fmts.add(1);
        }
        first
    }
}

/// Attaches the VideoToolbox device to a decoder context before it is opened.
pub fn enable_hwaccel(
    ctx: &mut ffmpeg::codec::context::Context,
    device: &HwDevice,
    threads: i32,
) -> Result<(), String> {
    unsafe {
        let raw = ctx.as_mut_ptr();
        let r = av_buffer_ref(device.0);
        if r.is_null() {
            return Err("av_buffer_ref failed".into());
        }
        (*raw).hw_device_ctx = r;
        (*raw).get_format = Some(pick_videotoolbox);
        (*raw).thread_count = threads;
    }
    Ok(())
}

/// The CVPixelBuffer behind a decoded VideoToolbox frame, retained.
pub fn pixel_buffer(frame: &ffmpeg::frame::Video) -> Option<CFRetained<CVPixelBuffer>> {
    unsafe {
        let raw = frame.as_ptr();
        if (*raw).format != AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32 {
            return None;
        }
        let pb = (*raw).data[3] as *mut CVPixelBuffer;
        NonNull::new(pb).map(|p| CFRetained::retain(p))
    }
}

pub fn fourcc(pb: &CVPixelBuffer) -> String {
    let v = CVPixelBufferGetPixelFormatType(pb);
    String::from_utf8_lossy(&v.to_be_bytes()).into_owned()
}

pub struct TextureCache(CFRetained<CVMetalTextureCache>);

// CVMetalTextureCache is thread safe per Apple's documentation.
unsafe impl Send for TextureCache {}

impl TextureCache {
    pub fn new(device: &wgpu::Device) -> Result<Self, String> {
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or("wgpu device is not Metal")?;
        let mtl = hal.raw_device().clone();
        let mut out: *mut CVMetalTextureCache = ptr::null_mut();
        let ret =
            unsafe { CVMetalTextureCache::create(None, None, &mtl, None, NonNull::from(&mut out)) };
        if ret != kCVReturnSuccess {
            return Err(format!("CVMetalTextureCacheCreate failed: {ret}"));
        }
        let cache = NonNull::new(out).ok_or("null texture cache")?;
        Ok(Self(unsafe { CFRetained::from_raw(cache) }))
    }

    pub fn flush(&self) {
        self.0.flush(0);
    }
}

/// One NV12 plane wrapped as a wgpu texture without copying. The CoreVideo
/// texture must outlive every GPU use of `texture`.
pub struct PlaneTexture {
    pub texture: wgpu::Texture,
    _cv: CFRetained<CVMetalTexture>,
}

pub fn wrap_plane(
    device: &wgpu::Device,
    cache: &TextureCache,
    pb: &CVPixelBuffer,
    plane: usize,
) -> Result<PlaneTexture, String> {
    if CVPixelBufferGetPlaneCount(pb) != 2 {
        return Err(format!("expected biplanar NV12, got {}", fourcc(pb)));
    }
    let width = CVPixelBufferGetWidthOfPlane(pb, plane);
    let height = CVPixelBufferGetHeightOfPlane(pb, plane);
    let (mtl_format, format) = if plane == 0 {
        (MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm)
    } else {
        (MTLPixelFormat::RG8Unorm, wgpu::TextureFormat::Rg8Unorm)
    };
    let mut out: *mut CVMetalTexture = ptr::null_mut();
    let ret = unsafe {
        CVMetalTextureCache::create_texture_from_image(
            None,
            &cache.0,
            pb,
            None,
            mtl_format,
            width,
            height,
            plane,
            NonNull::from(&mut out),
        )
    };
    if ret != kCVReturnSuccess {
        return Err(format!(
            "CVMetalTextureCacheCreateTextureFromImage failed: {ret}"
        ));
    }
    let cv = unsafe { CFRetained::from_raw(NonNull::new(out).ok_or("null CVMetalTexture")?) };
    let mtl: Retained<ProtocolObject<dyn MTLTexture>> =
        CVMetalTextureGetTexture(&cv).ok_or("CVMetalTextureGetTexture returned null")?;
    let size = wgpu::Extent3d {
        width: width as u32,
        height: height as u32,
        depth_or_array_layers: 1,
    };
    let texture = unsafe {
        let hal_tex = wgpu::hal::metal::Device::texture_from_raw(
            mtl,
            format,
            MTLTextureType::Type2D,
            1,
            1,
            wgpu::hal::CopyExtent {
                width: size.width,
                height: size.height,
                depth: 1,
            },
            None,
        );
        device.create_texture_from_hal::<wgpu::hal::api::Metal>(
            hal_tex,
            &wgpu::TextureDescriptor {
                label: Some("nv12 plane"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::RESOURCE,
        )
    };
    Ok(PlaneTexture { texture, _cv: cv })
}

/// User plus system CPU seconds consumed by this process so far.
pub fn process_cpu_seconds() -> f64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return 0.0;
    }
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    tv(usage.ru_utime) + tv(usage.ru_stime)
}

/// Identifies the IOSurface behind a pixel buffer; VideoToolbox recycles a
/// small pool of them, so textures can be cached per surface.
pub fn surface_id(pb: &CVPixelBuffer) -> Option<u32> {
    CVPixelBufferGetIOSurface(Some(pb)).map(|s| s.id())
}

/// Both NV12 planes of an IOSurface as wgpu textures created straight from the
/// surface (no CoreVideo texture cache). The Metal textures retain the surface.
pub fn wrap_surface(
    device: &wgpu::Device,
    pb: &CVPixelBuffer,
) -> Result<(wgpu::Texture, wgpu::Texture), String> {
    if CVPixelBufferGetPlaneCount(pb) != 2 {
        return Err(format!("expected biplanar NV12, got {}", fourcc(pb)));
    }
    let surface = CVPixelBufferGetIOSurface(Some(pb)).ok_or("pixel buffer has no IOSurface")?;
    let hal =
        unsafe { device.as_hal::<wgpu::hal::api::Metal>() }.ok_or("wgpu device is not Metal")?;
    let mtl_device = hal.raw_device().clone();
    let mut planes = Vec::with_capacity(2);
    for plane in 0..2 {
        let width = CVPixelBufferGetWidthOfPlane(pb, plane);
        let height = CVPixelBufferGetHeightOfPlane(pb, plane);
        let (mtl_format, format) = if plane == 0 {
            (MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm)
        } else {
            (MTLPixelFormat::RG8Unorm, wgpu::TextureFormat::Rg8Unorm)
        };
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                mtl_format, width, height, false,
            )
        };
        desc.setUsage(MTLTextureUsage::ShaderRead);
        desc.setStorageMode(MTLStorageMode::Shared);
        let mtl = mtl_device
            .newTextureWithDescriptor_iosurface_plane(&desc, &surface, plane)
            .ok_or("newTextureWithDescriptor:iosurface:plane: failed")?;
        let size = wgpu::Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        };
        let texture = unsafe {
            let hal_tex = wgpu::hal::metal::Device::texture_from_raw(
                mtl,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width: size.width,
                    height: size.height,
                    depth: 1,
                },
                None,
            );
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal_tex,
                &wgpu::TextureDescriptor {
                    label: Some("nv12 plane"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::TextureUses::RESOURCE,
            )
        };
        planes.push(texture);
    }
    let uv = planes.pop().ok_or("missing plane")?;
    let y = planes.pop().ok_or("missing plane")?;
    Ok((y, uv))
}
