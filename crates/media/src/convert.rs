//! CPU conversion of decoded frames to RGBA, for tests and thumbnails only. Playback and
//! export convert on the GPU from the native planes.

use ffmpeg_next as ffmpeg;

use crate::colour::{ColourSpace, Matrix, Range};
use crate::decode::VideoFrame;
use crate::error::{MediaError, Result};
use crate::ffi;

/// An 8-bit RGBA picture with tightly packed rows (`width * 4` bytes each).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// The RGBA value at (x, y), or `None` outside the picture.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = (y as usize * self.width as usize + x as usize) * 4;
        self.data.get(at..at + 4)?.try_into().ok()
    }
}

impl VideoFrame {
    /// Converts to full-range 8-bit RGBA (encoded R'G'B', no transfer function applied)
    /// with swscale (bilinear, accurate rounding, full chroma interpolation), at the same
    /// size, using the frame's [`ColourSpace`]: its matrix, range and chroma siting, with
    /// untagged values filled in by the rule in [`crate::colour`].
    pub fn to_rgba8(&self) -> Result<RgbaImage> {
        self.to_rgba8_as(&self.colour_space())
    }

    /// [`Self::to_rgba8`] with another colour description, for example a different chroma
    /// siting to compare against.
    pub fn to_rgba8_as(&self, colour: &ColourSpace) -> Result<RgbaImage> {
        self.convert(colour, None)
    }

    /// [`Self::to_rgba8`] scaled to `width` x `height` in the same pass (bilinear, with
    /// swscale's filter widened for downscaling, so it averages rather than skips): the
    /// thumbnail path, which never converts a full 4K frame.
    pub fn to_rgba8_scaled(&self, width: u32, height: u32) -> Result<RgbaImage> {
        self.convert(&self.colour_space(), Some((width, height)))
    }

    fn convert(&self, colour: &ColourSpace, size: Option<(u32, u32)>) -> Result<RgbaImage> {
        let frame = self.raw();
        let (width, height) = size.unwrap_or((frame.width(), frame.height()));
        let format = self.format();
        let error = |e: ffmpeg::Error| MediaError::Convert {
            format: format.name(),
            width,
            height,
            to: "rgba",
            source: e.into(),
        };
        if self.is_hardware() {
            // Hardware surfaces convert on the GPU (render::interop); there is no CPU copy.
            return Err(error(ffmpeg::Error::InvalidData));
        }
        let yuv = (!format.is_rgb()).then(|| {
            let matrix = match colour.matrix {
                Matrix::Bt709 | Matrix::Identity => ffmpeg::color::Space::BT709,
                Matrix::Bt601 => ffmpeg::color::Space::BT470BG,
                Matrix::Bt2020 => ffmpeg::color::Space::BT2020NCL,
                Matrix::Smpte240m => ffmpeg::color::Space::SMPTE240M,
            };
            let shift = format.chroma_shift();
            let (h, v) = colour.siting.position_256(shift);
            ffi::SwsYuv {
                matrix,
                full_range: colour.range == Range::Full,
                h_chroma_pos: (shift.0 > 0).then_some(h),
                v_chroma_pos: (shift.1 > 0).then_some(v),
            }
        });
        let data = ffi::scale_to_rgba(frame, yuv, size).map_err(error)?;
        Ok(RgbaImage {
            width,
            height,
            data,
        })
    }
}
