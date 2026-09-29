//! CPU conversion of decoded frames to RGBA, for tests and thumbnails only. Playback and
//! export convert on the GPU from the native planes.

use ffmpeg_next as ffmpeg;

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
    /// Converts to full-range 8-bit RGBA with swscale (bilinear, accurate rounding, full
    /// chroma interpolation), at the same size.
    ///
    /// YUV uses the frame's matrix and range. An untagged matrix is taken as BT.709 above
    /// 576 lines and BT.601 at or below, and an untagged range as limited, which is what
    /// cameras and encoders that omit the tags produce.
    pub fn to_rgba8(&self) -> Result<RgbaImage> {
        let frame = self.raw();
        let (width, height) = (frame.width(), frame.height());
        let format = self.format();
        if self.is_hardware() {
            // Hardware surfaces convert on the GPU (render::interop); there is no CPU copy.
            return Err(MediaError::Convert {
                format: format.name(),
                width,
                height,
                to: "rgba",
                source: ffmpeg::Error::InvalidData.into(),
            });
        }
        let error = |e: ffmpeg::Error| MediaError::Convert {
            format: format.name(),
            width,
            height,
            to: "rgba",
            source: e.into(),
        };
        let mut scaler = ffmpeg::software::scaling::Context::get(
            frame.format(),
            width,
            height,
            ffmpeg::format::Pixel::RGBA,
            width,
            height,
            ffmpeg::software::scaling::Flags::BILINEAR
                | ffmpeg::software::scaling::Flags::ACCURATE_RND
                | ffmpeg::software::scaling::Flags::FULL_CHR_H_INT,
        )
        .map_err(error)?;
        if !format.is_rgb() {
            let matrix = match frame.color_space() {
                ffmpeg::color::Space::Unspecified if height > 576 => ffmpeg::color::Space::BT709,
                ffmpeg::color::Space::Unspecified => ffmpeg::color::Space::BT470BG,
                tagged => tagged,
            };
            let full = frame.color_range() == ffmpeg::color::Range::JPEG
                || format.name().starts_with("yuvj");
            ffi::set_scaler_colourspace(&mut scaler, matrix, full).map_err(error)?;
        }
        let mut out = ffmpeg::frame::Video::empty();
        scaler.run(frame, &mut out).map_err(error)?;
        let row = width as usize * 4;
        let stride = out.stride(0);
        let mut data = Vec::with_capacity(row * height as usize);
        for line in out.data(0).chunks(stride).take(height as usize) {
            data.extend_from_slice(&line[..row]);
        }
        Ok(RgbaImage {
            width,
            height,
            data,
        })
    }
}
