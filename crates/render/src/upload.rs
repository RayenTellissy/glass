//! CPU-decoded frames to textures: each plane of a software-decoded [`VideoFrame`] is
//! written to its own texture (`R8Unorm`/`R16Unorm` per plane, `Rg8Unorm`/`Rg16Unorm` for
//! interleaved chroma) with `Queue::write_texture`, ready for [`YuvConverter`].
//!
//! [`YuvConverter`]: crate::yuv::YuvConverter

use media::VideoFrame;

use crate::yuv::{PlaneLayout, YuvParams, YuvPlanes};

/// Why a frame could not be uploaded.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("{0} frames are converted on the CPU or not at all; the GPU path reads YUV")]
    UnsupportedFormat(&'static str),
    #[error("the frame is a hardware surface; import it with render::interop")]
    Hardware,
    #[error("16-bit planes need the wgpu feature TEXTURE_FORMAT_16BIT_NORM")]
    MissingFeature,
    #[error("plane {0} of the frame is shorter than its size says")]
    ShortPlane(usize),
}

/// A CPU frame's planes on the GPU and the parameters to convert them.
#[derive(Debug)]
pub struct UploadedFrame {
    pub textures: Vec<wgpu::Texture>,
    pub views: Vec<wgpu::TextureView>,
    pub params: YuvParams,
    /// The frame's primaries, for gamut conversion into the working space.
    pub primaries: media::Primaries,
}

impl UploadedFrame {
    pub fn planes(&self) -> YuvPlanes<'_> {
        if self.params.layout.planar {
            YuvPlanes::Planar {
                y: &self.views[0],
                cb: &self.views[1],
                cr: &self.views[2],
            }
        } else {
            YuvPlanes::Biplanar {
                luma: &self.views[0],
                chroma: &self.views[1],
            }
        }
    }
}

/// Uploads the planes of a software-decoded YUV frame.
pub fn upload_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &VideoFrame,
) -> Result<UploadedFrame, UploadError> {
    if frame.is_hardware() {
        return Err(UploadError::Hardware);
    }
    let format = frame.format();
    let layout = PlaneLayout::from_pixel_format(format)
        .ok_or(UploadError::UnsupportedFormat(format.name()))?;
    if layout.container_bits > 8
        && !device
            .features()
            .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
    {
        return Err(UploadError::MissingFeature);
    }
    let params =
        YuvParams::for_cpu_frame(frame).ok_or(UploadError::UnsupportedFormat(format.name()))?;
    let (luma_format, chroma_format) = layout.texture_formats();
    let mut textures = Vec::new();
    for (index, plane) in frame.planes().iter().enumerate() {
        let texture_format = if index == 0 {
            luma_format
        } else {
            chroma_format
        };
        let texel = texture_format.block_copy_size(None).unwrap_or(1);
        let row = plane.width as usize * texel as usize;
        let needed = plane.stride * (plane.height as usize).saturating_sub(1) + row;
        if plane.data.len() < needed {
            return Err(UploadError::ShortPlane(index));
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video plane"),
            size: wgpu::Extent3d {
                width: plane.width,
                height: plane.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texture_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &plane.data[..needed],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(plane.stride as u32),
                rows_per_image: Some(plane.height),
            },
            texture.size(),
        );
        textures.push(texture);
    }
    let views = textures
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect();
    Ok(UploadedFrame {
        textures,
        views,
        params,
        primaries: frame.colour_space().primaries,
    })
}
