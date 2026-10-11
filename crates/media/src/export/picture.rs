//! Pictures in caller memory and their conversion into encoder frames.

use ffmpeg_next as ffmpeg;

use crate::decode::VideoFrame;
use crate::ffi::{self, SrcPlane, Sws, SwsYuv};
use crate::format::{ColourInfo, ColourRange, PixelFormat};

use super::{ExportError, Result};

/// One plane of a [`Picture`]: its bytes and the distance between rows in bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct PicturePlane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

/// A picture in caller memory, in any CPU pixel format FFmpeg knows.
///
/// The render readback ring's rows map onto the packed constructors: `Rgba8Unorm` onto
/// [`rgba8`](Self::rgba8), `Bgra8Unorm` onto [`bgra8`](Self::bgra8), `Rgba16Unorm` onto
/// [`rgba16`](Self::rgba16) and `Rgb10a2Unorm` onto [`rgb10a2`](Self::rgb10a2), with the
/// view's `bytes_per_row` as the stride. Each plane must hold `stride * rows` bytes.
#[derive(Clone, Copy, Debug)]
pub struct Picture<'a> {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    /// Planes in FFmpeg's order (one for packed RGB, three for planar YUV, two for NV12).
    pub planes: [PicturePlane<'a>; 4],
}

impl<'a> Picture<'a> {
    fn packed(name: &str, width: u32, height: u32, data: &'a [u8], stride: usize) -> Self {
        let format = PixelFormat::from_name(name).expect("FFmpeg knows its packed RGB formats");
        let mut planes = [PicturePlane::default(); 4];
        planes[0] = PicturePlane { data, stride };
        Self {
            format,
            width,
            height,
            planes,
        }
    }

    /// 8-bit RGBA, R first in memory.
    pub fn rgba8(width: u32, height: u32, data: &'a [u8], stride: usize) -> Self {
        Self::packed("rgba", width, height, data, stride)
    }

    /// 8-bit BGRA, B first in memory.
    pub fn bgra8(width: u32, height: u32, data: &'a [u8], stride: usize) -> Self {
        Self::packed("bgra", width, height, data, stride)
    }

    /// 16-bit RGBA, each value a little-endian `u16`.
    pub fn rgba16(width: u32, height: u32, data: &'a [u8], stride: usize) -> Self {
        Self::packed("rgba64le", width, height, data, stride)
    }

    /// 10-bit RGB with 2-bit alpha in a little-endian `u32`, R in the low bits (wgpu's
    /// `Rgb10a2Unorm`); the alpha is ignored.
    pub fn rgb10a2(width: u32, height: u32, data: &'a [u8], stride: usize) -> Self {
        Self::packed("x2bgr10le", width, height, data, stride)
    }

    /// Any CPU format with its planes in FFmpeg's order.
    pub fn planar(
        format: PixelFormat,
        width: u32,
        height: u32,
        planes: &[PicturePlane<'a>],
    ) -> Self {
        let mut all = [PicturePlane::default(); 4];
        for (to, from) in all.iter_mut().zip(planes) {
            *to = *from;
        }
        Self {
            format,
            width,
            height,
            planes: all,
        }
    }

    /// The planes of a decoded CPU frame, or `None` for a hardware frame.
    pub fn from_frame(frame: &'a VideoFrame) -> Option<Self> {
        if frame.is_hardware() {
            return None;
        }
        let mut planes = [PicturePlane::default(); 4];
        for (i, plane) in frame.planes().into_iter().take(4).enumerate() {
            planes[i] = PicturePlane {
                data: plane.data,
                stride: plane.stride,
            };
        }
        Some(Self {
            format: frame.format(),
            width: frame.width(),
            height: frame.height(),
            planes,
        })
    }
}

/// Turns pictures into frames of one encoder pixel format and size, keeping one swscale
/// context per input format.
pub(crate) struct Converter {
    format: PixelFormat,
    width: u32,
    height: u32,
    colour: ColourInfo,
    yuv: Option<SwsYuv>,
    sws: Option<(PixelFormat, Sws)>,
}

impl std::fmt::Debug for Converter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Converter")
            .field("format", &self.format)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl Converter {
    /// Frames of `format` at `width` x `height`, tagged with `colour`; RGB input becomes
    /// YUV with `colour`'s matrix, range and chroma siting.
    pub(crate) fn new(format: PixelFormat, width: u32, height: u32, colour: ColourInfo) -> Self {
        let yuv = (!format.is_rgb()).then(|| {
            let (_, _, matrix) = ffi::colour_enums(None, None, colour.matrix);
            let (sx, sy) = format.chroma_shift();
            // Left siting: chroma on the even luma columns, between the rows of a pair.
            SwsYuv {
                matrix,
                full_range: colour.range == ColourRange::Full,
                h_chroma_pos: (sx > 0).then_some(0),
                v_chroma_pos: (sy > 0).then_some(128),
            }
        });
        Self {
            format,
            width,
            height,
            colour,
            yuv,
            sws: None,
        }
    }

    pub(crate) fn format(&self) -> PixelFormat {
        self.format
    }

    /// A new frame holding `picture` converted, with the colour tags set and no timing.
    pub(crate) fn convert(&mut self, picture: &Picture<'_>) -> Result<ffmpeg::frame::Video> {
        if picture.width != self.width || picture.height != self.height {
            return Err(ExportError::Input(format!(
                "a {}x{} picture for a {}x{} export",
                picture.width, picture.height, self.width, self.height
            )));
        }
        if picture.format.is_hardware() {
            return Err(ExportError::Input(format!(
                "{} is a hardware surface format",
                picture.format
            )));
        }
        if self.sws.as_ref().is_none_or(|(f, _)| *f != picture.format) {
            let sws = Sws::new(
                picture.format.to_ffmpeg(),
                self.format.to_ffmpeg(),
                self.width,
                self.height,
                self.yuv,
            )
            .map_err(|e| crate::MediaError::Convert {
                format: picture.format.name(),
                width: self.width,
                height: self.height,
                to: self.format.name(),
                source: e.into(),
            })?;
            self.sws = Some((picture.format, sws));
        }
        let Some((_, sws)) = self.sws.as_mut() else {
            unreachable!("set above");
        };
        let planes = picture.planes.map(|p| SrcPlane {
            data: p.data,
            stride: p.stride,
        });
        let mut frame = ffmpeg::frame::Video::new(self.format.to_ffmpeg(), self.width, self.height);
        sws.scale(&planes, &mut frame).map_err(|e| {
            ExportError::Input(format!(
                "a {} picture whose planes do not hold {}x{} pixels ({e})",
                picture.format, self.width, self.height
            ))
        })?;
        let (primaries, transfer, matrix) = ffi::colour_enums(
            self.colour.primaries,
            self.colour.transfer,
            self.colour.matrix,
        );
        frame.set_color_primaries(primaries);
        frame.set_color_transfer_characteristic(transfer);
        frame.set_color_space(matrix);
        frame.set_color_range(match self.colour.range {
            ColourRange::Full => ffmpeg::color::Range::JPEG,
            ColourRange::Limited => ffmpeg::color::Range::MPEG,
            ColourRange::Unspecified => ffmpeg::color::Range::Unspecified,
        });
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::ColourTags;

    /// Y, Cb and Cr of a flat RGB colour converted to 8-bit 4:2:0 with `tags`.
    fn yuv_of(rgb: [u8; 3], tags: ColourTags) -> [u8; 3] {
        crate::init();
        let (w, h) = (16u32, 8u32);
        let data: Vec<u8> = (0..w * h)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect();
        let format = PixelFormat::from_name("yuv420p").expect("yuv420p");
        let mut converter = Converter::new(format, w, h, tags.info(false));
        let frame = converter
            .convert(&Picture::rgba8(w, h, &data, w as usize * 4))
            .expect("convert");
        [frame.data(0)[0], frame.data(1)[0], frame.data(2)[0]]
    }

    fn close(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
    }

    #[test]
    fn rgb_becomes_limited_range_yuv_in_the_tagged_matrix() {
        let cases = [
            ([255, 255, 255], ColourTags::Bt709, [235, 128, 128]),
            ([0, 0, 0], ColourTags::Bt709, [16, 128, 128]),
            // BT.709 red: Y' 0.2126, Cb -0.1146, Cr 0.5.
            ([255, 0, 0], ColourTags::Bt709, [63, 102, 240]),
            ([0, 0, 255], ColourTags::Bt709, [32, 240, 118]),
            // BT.2020 red: Y' 0.2627, Cb -0.1396, Cr 0.5.
            ([255, 0, 0], ColourTags::Bt2020Pq, [74, 97, 240]),
        ];
        for (rgb, tags, expected) in cases {
            let got = yuv_of(rgb, tags);
            assert!(
                close(got, expected),
                "{rgb:?} {tags:?}: {got:?}, expected {expected:?}"
            );
        }
    }
}
