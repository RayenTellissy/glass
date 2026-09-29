//! Single pictures: PNG, JPEG, TIFF, BMP, WebP and GIF through the `image` crate, HEIC,
//! HEIF and AVIF through ImageIO on macOS. A decoded picture becomes a planar RGB
//! [`VideoFrame`] (`gbrp`, `gbrap`, or their 16-bit little-endian forms), which the GPU
//! converter reads like a YUV frame with the identity matrix.

use std::io::{BufReader, Read};
use std::path::Path;

use ffmpeg_next as ffmpeg;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

#[cfg(not(target_os = "macos"))]
use super::Unsupported;
use super::icc::{self, EmbeddedColour};
use crate::colour::{Primaries, TransferFn};
use crate::decode::VideoFrame;
use crate::error::{MediaError, Result};
use crate::time::Timestamp;

/// Picture file formats Glass reads without FFmpeg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageKind {
    Png,
    Jpeg,
    Tiff,
    Bmp,
    Gif,
    WebP,
    /// HEIC or HEIF (HEVC in an ISO BMFF image container).
    Heif,
    Avif,
}

impl ImageKind {
    /// Recognises a picture file by its first bytes.
    pub fn sniff(header: &[u8]) -> Option<Self> {
        let starts = |magic: &[u8]| header.starts_with(magic);
        if starts(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if starts(b"\xff\xd8\xff") {
            Some(Self::Jpeg)
        } else if starts(b"II*\0") || starts(b"MM\0*") {
            Some(Self::Tiff)
        } else if starts(b"BM") && header.len() >= 18 {
            Some(Self::Bmp)
        } else if starts(b"GIF87a") || starts(b"GIF89a") {
            Some(Self::Gif)
        } else if starts(b"RIFF") && header.get(8..12) == Some(b"WEBP") {
            Some(Self::WebP)
        } else if header.get(4..8) == Some(b"ftyp") {
            let brand = header.get(8..12)?;
            match brand {
                b"avif" | b"avis" => Some(Self::Avif),
                b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => {
                    Some(Self::Heif)
                }
                _ => None,
            }
        } else {
            None
        }
    }

    /// The short name probe and the metadata panel use (`png`, `jpeg`, `heif`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
            Self::Gif => "gif",
            Self::WebP => "webp",
            Self::Heif => "heif",
            Self::Avif => "avif",
        }
    }

    fn image_format(self) -> Option<ImageFormat> {
        Some(match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Tiff => ImageFormat::Tiff,
            Self::Bmp => ImageFormat::Bmp,
            Self::Gif => ImageFormat::Gif,
            Self::WebP => ImageFormat::WebP,
            Self::Heif | Self::Avif => return None,
        })
    }

    /// Decoded by the platform (ImageIO) rather than the `image` crate.
    pub fn needs_platform_decoder(self) -> bool {
        matches!(self, Self::Heif | Self::Avif)
    }
}

/// The first bytes of a file, enough for [`ImageKind::sniff`].
pub(crate) fn read_header(path: &Path) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path).map_err(|e| io_error(path, &e))?;
    let mut header = vec![0u8; 32];
    let mut filled = 0;
    while filled < header.len() {
        match file.read(&mut header[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(io_error(path, &e)),
        }
    }
    header.truncate(filled);
    Ok(header)
}

pub(crate) fn io_error(path: &Path, error: &std::io::Error) -> MediaError {
    MediaError::Image {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn image_error(path: &Path, error: impl std::fmt::Display) -> MediaError {
    MediaError::Image {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// What a picture file says about itself before its pixels are decoded.
#[derive(Clone, Debug)]
pub(crate) struct StillHeader {
    /// Stored size, before orientation.
    pub stored: (u32, u32),
    pub alpha: bool,
    pub bit_depth: u8,
    pub orientation: u8,
    pub colour: EmbeddedColour,
}

impl StillHeader {
    /// The upright size, after the EXIF orientation.
    pub fn size(&self) -> (u32, u32) {
        if self.orientation >= 5 {
            (self.stored.1, self.stored.0)
        } else {
            self.stored
        }
    }
}

/// A decoded picture, upright, with its colour.
pub(crate) struct DecodedStill {
    pub header: StillHeader,
    pub image: DynamicImage,
}

/// Reads a picture's header: size, alpha, bit depth, orientation and colour.
pub(crate) fn read_still_header(path: &Path, kind: ImageKind) -> Result<StillHeader> {
    if kind.needs_platform_decoder() {
        return platform_header(path, kind);
    }
    let mut decoder = image_decoder(path, kind)?;
    let colour = embedded_colour(path, kind, &mut decoder);
    header_from_decoder(path, &mut decoder, colour)
}

/// Decodes a picture and applies its orientation.
pub(crate) fn decode_still(path: &Path, kind: ImageKind) -> Result<DecodedStill> {
    if kind.needs_platform_decoder() {
        return platform_decode(path, kind);
    }
    let mut decoder = image_decoder(path, kind)?;
    let colour = embedded_colour(path, kind, &mut decoder);
    let header = header_from_decoder(path, &mut decoder, colour)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| image_error(path, e))?;
    if let Some(orientation) = image::metadata::Orientation::from_exif(header.orientation) {
        image.apply_orientation(orientation);
    }
    Ok(DecodedStill { header, image })
}

fn image_decoder(path: &Path, kind: ImageKind) -> Result<impl ImageDecoder + use<>> {
    let file = std::fs::File::open(path).map_err(|e| io_error(path, &e))?;
    let mut reader = ImageReader::new(BufReader::new(file));
    if let Some(format) = kind.image_format() {
        reader.set_format(format);
    }
    // Camera stills can exceed the default 512 MiB allocation limit (a 100 MP 16-bit TIFF).
    reader.no_limits();
    reader.into_decoder().map_err(|e| image_error(path, e))
}

fn header_from_decoder(
    path: &Path,
    decoder: &mut impl ImageDecoder,
    colour: EmbeddedColour,
) -> Result<StillHeader> {
    let colour_type = decoder.color_type();
    let orientation = decoder
        .orientation()
        .map(|o| o.to_exif())
        .unwrap_or_else(|e| {
            tracing::debug!(path = %path.display(), %e, "unreadable orientation, taken as upright");
            1
        });
    Ok(StillHeader {
        stored: decoder.dimensions(),
        alpha: colour_type.has_alpha(),
        bit_depth: if colour_type.bytes_per_pixel() / colour_type.channel_count() > 1 {
            16
        } else {
            8
        },
        orientation,
        colour,
    })
}

/// The colour a picture declares: a PNG `cICP` chunk first (the PNG specification puts it
/// above an ICC profile), then the ICC profile.
fn embedded_colour(
    path: &Path,
    kind: ImageKind,
    decoder: &mut impl ImageDecoder,
) -> EmbeddedColour {
    if kind == ImageKind::Png
        && let Some(colour) = png_chunks(path).as_deref().and_then(icc::png_cicp)
    {
        return colour;
    }
    match decoder.icc_profile() {
        Ok(Some(profile)) => icc::parse_icc(&profile).unwrap_or_default(),
        Ok(None) => EmbeddedColour::default(),
        Err(e) => {
            tracing::debug!(path = %path.display(), %e, "unreadable ICC profile, taken as sRGB");
            EmbeddedColour::default()
        }
    }
}

/// The start of a PNG file, up to its first image data: where `cICP` must be.
fn png_chunks(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(64 * 1024).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

#[cfg(target_os = "macos")]
fn platform_header(path: &Path, _kind: ImageKind) -> Result<StillHeader> {
    let bytes = std::fs::read(path).map_err(|e| io_error(path, &e))?;
    let info = crate::ffi::imageio::probe(&bytes).map_err(|m| image_error(path, m))?;
    Ok(StillHeader {
        stored: (info.width, info.height),
        alpha: info.alpha,
        bit_depth: if info.deep { 16 } else { 8 },
        orientation: info.orientation,
        colour: info
            .icc
            .as_deref()
            .and_then(icc::parse_icc)
            .unwrap_or_default(),
    })
}

#[cfg(target_os = "macos")]
fn platform_decode(path: &Path, _kind: ImageKind) -> Result<DecodedStill> {
    let bytes = std::fs::read(path).map_err(|e| io_error(path, &e))?;
    let picture = crate::ffi::imageio::decode(&bytes).map_err(|m| image_error(path, m))?;
    let header = StillHeader {
        stored: (picture.width, picture.height),
        alpha: picture.alpha,
        bit_depth: if picture.deep { 16 } else { 8 },
        orientation: picture.orientation,
        colour: picture
            .icc
            .as_deref()
            .and_then(icc::parse_icc)
            .unwrap_or_default(),
    };
    let bad_size = || image_error(path, "ImageIO returned a buffer of the wrong size");
    let mut image = match picture.pixels {
        crate::ffi::imageio::Pixels::Rgba8(data) => DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(picture.width, picture.height, data).ok_or_else(bad_size)?,
        ),
        crate::ffi::imageio::Pixels::Rgba16(data) => DynamicImage::ImageRgba16(
            image::ImageBuffer::from_raw(picture.width, picture.height, data)
                .ok_or_else(bad_size)?,
        ),
    };
    if !picture.alpha {
        image = if picture.deep {
            DynamicImage::ImageRgb16(image.to_rgb16())
        } else {
            DynamicImage::ImageRgb8(image.to_rgb8())
        };
    }
    if let Some(orientation) = image::metadata::Orientation::from_exif(header.orientation) {
        image.apply_orientation(orientation);
    }
    Ok(DecodedStill { header, image })
}

/// HEIC, HEIF and AVIF need ImageIO, which only macOS has. Windows could use WIC with the
/// HEIF and AV1 extensions (not built yet); Linux has no system decoder.
#[cfg(not(target_os = "macos"))]
fn heif_unsupported(path: &Path, kind: ImageKind) -> MediaError {
    MediaError::Unsupported {
        path: path.to_path_buf(),
        reason: if kind == ImageKind::Avif {
            Unsupported::Avif
        } else {
            Unsupported::Heif
        },
    }
}

#[cfg(not(target_os = "macos"))]
fn platform_header(path: &Path, kind: ImageKind) -> Result<StillHeader> {
    Err(heif_unsupported(path, kind))
}

#[cfg(not(target_os = "macos"))]
fn platform_decode(path: &Path, kind: ImageKind) -> Result<DecodedStill> {
    Err(heif_unsupported(path, kind))
}

/// Builds a planar RGB frame from a decoded picture: `gbrp` or `gbrap` for 8-bit
/// pictures, `gbrp16le` or `gbrap16le` for deeper ones, tagged full range, identity
/// matrix, sRGB transfer and BT.709 primaries unless the picture declares others.
pub(crate) fn frame_from_image(
    image: &DynamicImage,
    colour: &EmbeddedColour,
    pts: Timestamp,
    duration: Option<i64>,
) -> VideoFrame {
    let colour_type = image.color();
    let alpha = colour_type.has_alpha();
    let deep = colour_type.bytes_per_pixel() / colour_type.channel_count() > 1;
    let (width, height) = (image.width(), image.height());
    let format = match (deep, alpha) {
        (false, false) => ffmpeg::format::Pixel::GBRP,
        (false, true) => ffmpeg::format::Pixel::GBRAP,
        (true, false) => ffmpeg::format::Pixel::GBRP16LE,
        (true, true) => ffmpeg::format::Pixel::GBRAP16LE,
    };
    let mut frame = ffmpeg::frame::Video::new(format, width, height);
    match (deep, alpha) {
        (false, false) => match image.as_rgb8() {
            Some(buf) => fill_planes(&mut frame, buf.as_raw(), 3),
            None => fill_planes(&mut frame, image.to_rgb8().as_raw(), 3),
        },
        (false, true) => match image.as_rgba8() {
            Some(buf) => fill_planes(&mut frame, buf.as_raw(), 4),
            None => fill_planes(&mut frame, image.to_rgba8().as_raw(), 4),
        },
        (true, false) => match image.as_rgb16() {
            Some(buf) => fill_planes(&mut frame, buf.as_raw(), 3),
            None => fill_planes(&mut frame, image.to_rgb16().as_raw(), 3),
        },
        (true, true) => match image.as_rgba16() {
            Some(buf) => fill_planes(&mut frame, buf.as_raw(), 4),
            None => fill_planes(&mut frame, image.to_rgba16().as_raw(), 4),
        },
    }
    frame.set_color_space(ffmpeg::color::Space::RGB);
    frame.set_color_range(ffmpeg::color::Range::JPEG);
    frame.set_color_primaries(ffmpeg_primaries(
        colour.primaries.unwrap_or(Primaries::Bt709),
    ));
    frame.set_color_transfer_characteristic(ffmpeg_transfer(
        colour.transfer.unwrap_or(TransferFn::Srgb),
    ));
    frame.set_pts(Some(pts.value));
    if let Some(d) = duration {
        crate::ffi::set_frame_duration(&mut frame, d);
    }
    VideoFrame::from_cpu(frame, pts.time_base)
}

/// A sample type that can be written to a plane in little-endian order.
trait Sample: Copy {
    const BYTES: usize;
    fn write(self, out: &mut [u8]);
}

impl Sample for u8 {
    const BYTES: usize = 1;
    fn write(self, out: &mut [u8]) {
        out[0] = self;
    }
}

impl Sample for u16 {
    const BYTES: usize = 2;
    fn write(self, out: &mut [u8]) {
        out[..2].copy_from_slice(&self.to_le_bytes());
    }
}

/// Scatters interleaved R, G, B (and A) samples into the G, B, R (and A) planes.
fn fill_planes<T: Sample>(frame: &mut ffmpeg::frame::Video, samples: &[T], channels: usize) {
    let width = frame.width() as usize;
    let height = frame.height() as usize;
    // Plane order of gbrp: G, B, R, then A; source order R, G, B, A.
    let source_channel = [1usize, 2, 0, 3];
    for (plane, &channel) in source_channel.iter().enumerate().take(channels) {
        let stride = frame.stride(plane);
        let data = frame.data_mut(plane);
        for y in 0..height {
            let row = &mut data[y * stride..y * stride + width * T::BYTES];
            let source = &samples[y * width * channels..(y + 1) * width * channels];
            for x in 0..width {
                source[x * channels + channel].write(&mut row[x * T::BYTES..]);
            }
        }
    }
}

fn ffmpeg_primaries(p: Primaries) -> ffmpeg::color::Primaries {
    use ffmpeg::color::Primaries as F;
    match p {
        Primaries::Bt709 => F::BT709,
        Primaries::Bt601Pal => F::BT470BG,
        Primaries::Bt601Ntsc => F::SMPTE170M,
        Primaries::Bt2020 => F::BT2020,
        Primaries::DciP3 => F::SMPTE431,
        Primaries::DisplayP3 => F::SMPTE432,
    }
}

fn ffmpeg_transfer(t: TransferFn) -> ffmpeg::color::TransferCharacteristic {
    use ffmpeg::color::TransferCharacteristic as F;
    match t {
        TransferFn::Bt1886 => F::BT709,
        TransferFn::Srgb => F::IEC61966_2_1,
        TransferFn::Gamma22 => F::GAMMA22,
        TransferFn::Gamma28 => F::GAMMA28,
        TransferFn::Linear => F::Linear,
        TransferFn::Pq => F::SMPTE2084,
        TransferFn::Hlg => F::ARIB_STD_B67,
    }
}
