//! Stream kinds, pixel and sample formats, and colour metadata, described without FFmpeg
//! types so callers (`render`, `engine`) never depend on `ffmpeg-next`.

use std::fmt;

use ffmpeg_next as ffmpeg;

use crate::ffi;

/// What a stream carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
    Unknown,
}

impl MediaKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Subtitle => "subtitle",
            Self::Data => "data",
            Self::Attachment => "attachment",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for MediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl From<ffmpeg::media::Type> for MediaKind {
    fn from(t: ffmpeg::media::Type) -> Self {
        use ffmpeg::media::Type;
        match t {
            Type::Video => Self::Video,
            Type::Audio => Self::Audio,
            Type::Subtitle => Self::Subtitle,
            Type::Data => Self::Data,
            Type::Attachment => Self::Attachment,
            Type::Unknown => Self::Unknown,
        }
    }
}

/// A pixel layout, named as FFmpeg names it (`yuv420p`, `yuv420p10le`, `nv12`, `rgb24`).
///
/// The name is the stable identifier; the other accessors describe the memory layout so a
/// GPU upload path can handle formats it has not seen by name.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat(ffmpeg::format::Pixel);

impl std::hash::Hash for PixelFormat {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let raw: ffmpeg::ffi::AVPixelFormat = self.0.into();
        (raw as i32).hash(state);
    }
}

impl PixelFormat {
    pub(crate) fn from_ffmpeg(p: ffmpeg::format::Pixel) -> Self {
        Self(p)
    }

    /// The format with this FFmpeg name, if FFmpeg knows it.
    pub fn from_name(name: &str) -> Option<Self> {
        name.parse::<ffmpeg::format::Pixel>()
            .ok()
            .filter(|p| *p != ffmpeg::format::Pixel::None)
            .map(Self)
    }

    pub fn name(self) -> &'static str {
        self.0.descriptor().map_or("none", |d| d.name())
    }

    /// Bits per component of the first component (8 for `yuv420p`, 10 for `yuv420p10le`).
    pub fn bit_depth(self) -> u8 {
        ffi::pix_fmt_info(self.0).map_or(0, |i| i.depth)
    }

    /// Number of memory planes (3 for `yuv420p`, 2 for `nv12`, 1 for `rgb24`).
    pub fn plane_count(self) -> usize {
        ffi::pix_fmt_info(self.0).map_or(0, |i| i.planes)
    }

    /// Number of colour components, alpha included.
    pub fn components(self) -> u8 {
        self.0.descriptor().map_or(0, |d| d.nb_components())
    }

    /// log2 of the horizontal and vertical chroma subsampling: (1, 1) for 4:2:0,
    /// (1, 0) for 4:2:2, (0, 0) for 4:4:4 and RGB.
    pub fn chroma_shift(self) -> (u8, u8) {
        self.0
            .descriptor()
            .map_or((0, 0), |d| (d.log2_chroma_w(), d.log2_chroma_h()))
    }

    pub fn is_rgb(self) -> bool {
        ffi::pix_fmt_info(self.0).is_some_and(|i| i.rgb)
    }

    pub fn has_alpha(self) -> bool {
        ffi::pix_fmt_info(self.0).is_some_and(|i| i.alpha)
    }

    /// A hardware surface format (`videotoolbox_vld`, `d3d11`, ...) with no CPU planes.
    pub fn is_hardware(self) -> bool {
        ffi::pix_fmt_info(self.0).is_some_and(|i| i.hardware)
    }
}

impl fmt::Debug for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PixelFormat({})", self.name())
    }
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The numeric type of one audio sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleType {
    U8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl SampleType {
    pub fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::I16 => 2,
            Self::I32 | Self::F32 => 4,
            Self::I64 | Self::F64 => 8,
        }
    }
}

/// An audio sample layout: the sample type and whether channels are planar (one plane
/// per channel) or interleaved in one plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SampleFormat {
    pub sample: SampleType,
    pub planar: bool,
}

impl SampleFormat {
    pub(crate) fn from_ffmpeg(s: ffmpeg::format::Sample) -> Option<Self> {
        use ffmpeg::format::Sample;
        use ffmpeg::format::sample::Type;
        let (sample, t) = match s {
            Sample::None => return None,
            Sample::U8(t) => (SampleType::U8, t),
            Sample::I16(t) => (SampleType::I16, t),
            Sample::I32(t) => (SampleType::I32, t),
            Sample::I64(t) => (SampleType::I64, t),
            Sample::F32(t) => (SampleType::F32, t),
            Sample::F64(t) => (SampleType::F64, t),
        };
        Some(Self {
            sample,
            planar: t == Type::Planar,
        })
    }

    /// FFmpeg's name: `s16`, `fltp`, `s32p` and so on.
    pub fn name(self) -> &'static str {
        match (self.sample, self.planar) {
            (SampleType::U8, false) => "u8",
            (SampleType::U8, true) => "u8p",
            (SampleType::I16, false) => "s16",
            (SampleType::I16, true) => "s16p",
            (SampleType::I32, false) => "s32",
            (SampleType::I32, true) => "s32p",
            (SampleType::I64, false) => "s64",
            (SampleType::I64, true) => "s64p",
            (SampleType::F32, false) => "flt",
            (SampleType::F32, true) => "fltp",
            (SampleType::F64, false) => "dbl",
            (SampleType::F64, true) => "dblp",
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Signal range of the coded values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColourRange {
    Unspecified,
    /// Limited ("TV", "MPEG") range: 16 to 235 for 8-bit luma.
    Limited,
    /// Full ("PC", "JPEG") range.
    Full,
}

/// High dynamic range transfer, derived from the transfer characteristic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hdr {
    None,
    /// SMPTE ST 2084 perceptual quantizer (`smpte2084`).
    Pq,
    /// ARIB STD-B67 hybrid log-gamma (`arib-std-b67`).
    Hlg,
}

/// Colour metadata of a stream or frame, as FFmpeg names each value (`bt709`, `bt2020nc`,
/// `smpte2084`, `left`). `None` means the file leaves it unspecified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColourInfo {
    pub primaries: Option<&'static str>,
    pub transfer: Option<&'static str>,
    /// The YUV to RGB matrix (FFmpeg's "colorspace").
    pub matrix: Option<&'static str>,
    pub range: ColourRange,
    pub chroma_location: Option<&'static str>,
}

impl ColourInfo {
    pub(crate) fn from_ffmpeg(
        primaries: ffmpeg::color::Primaries,
        transfer: ffmpeg::color::TransferCharacteristic,
        matrix: ffmpeg::color::Space,
        range: ffmpeg::color::Range,
        chroma: ffmpeg::chroma::Location,
    ) -> Self {
        Self {
            primaries: primaries.name(),
            transfer: transfer.name(),
            matrix: matrix.name(),
            range: match range {
                ffmpeg::color::Range::Unspecified => ColourRange::Unspecified,
                ffmpeg::color::Range::MPEG => ColourRange::Limited,
                ffmpeg::color::Range::JPEG => ColourRange::Full,
            },
            chroma_location: ffi::chroma_location_name(chroma),
        }
    }

    pub fn hdr(&self) -> Hdr {
        match self.transfer {
            Some("smpte2084") => Hdr::Pq,
            Some("arib-std-b67") => Hdr::Hlg,
            _ => Hdr::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_layouts() {
        crate::init();
        let yuv = PixelFormat::from_name("yuv420p10le").expect("known format");
        assert_eq!(yuv.name(), "yuv420p10le");
        assert_eq!(yuv.bit_depth(), 10);
        assert_eq!(yuv.plane_count(), 3);
        assert_eq!(yuv.chroma_shift(), (1, 1));
        assert!(!yuv.is_rgb());
        let nv12 = PixelFormat::from_name("nv12").expect("known format");
        assert_eq!(nv12.plane_count(), 2);
        let rgba = PixelFormat::from_name("rgba").expect("known format");
        assert!(rgba.is_rgb() && rgba.has_alpha());
        assert!(PixelFormat::from_name("videotoolbox_vld").is_some_and(|p| p.is_hardware()));
        assert!(PixelFormat::from_name("not-a-format").is_none());
    }

    #[test]
    fn sample_format_names_match_ffmpeg() {
        use ffmpeg::format::Sample;
        use ffmpeg::format::sample::Type;
        for s in [
            Sample::U8(Type::Packed),
            Sample::I16(Type::Planar),
            Sample::I32(Type::Packed),
            Sample::F32(Type::Planar),
            Sample::F64(Type::Packed),
        ] {
            let ours = SampleFormat::from_ffmpeg(s).expect("a real format");
            assert_eq!(ours.name(), s.name());
            assert_eq!(ours.sample.bytes(), s.bytes());
        }
        assert_eq!(SampleFormat::from_ffmpeg(Sample::None), None);
    }

    #[test]
    fn hdr_from_transfer() {
        let mut c = ColourInfo {
            primaries: Some("bt2020"),
            transfer: Some("smpte2084"),
            matrix: Some("bt2020nc"),
            range: ColourRange::Limited,
            chroma_location: None,
        };
        assert_eq!(c.hdr(), Hdr::Pq);
        c.transfer = Some("arib-std-b67");
        assert_eq!(c.hdr(), Hdr::Hlg);
        c.transfer = Some("bt709");
        assert_eq!(c.hdr(), Hdr::None);
    }
}
