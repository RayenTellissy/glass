//! Hardware decode: the preference a caller passes in [`DecoderOptions`], the path a
//! decoder ended up on ([`DecodePath`]), and the platform frame handle a hardware decoder
//! hands out ([`HwFrame`]). `media` never touches the GPU; `render::interop` imports an
//! [`HwFrame`] as textures without copying it.
//!
//! [`DecoderOptions`]: crate::DecoderOptions

use std::fmt;

use crate::ffi;
use crate::format::ColourInfo;
use crate::time::Timestamp;

/// Whether a video decoder should use the platform's hardware decoder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HwPreference {
    /// Hardware when the platform can decode the stream, software otherwise, and a switch
    /// to software (at the same position) when the hardware decoder fails mid-stream.
    /// The reason is kept in [`DecodePath::Software`].
    Auto,
    /// Software only. The default, so thumbnails, analysis and tests get CPU planes.
    #[default]
    Off,
    /// Hardware or an error ([`MediaError::Hardware`](crate::MediaError::Hardware)).
    Require,
}

/// The platform decode API behind a hardware decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HwApi {
    /// Apple VideoToolbox (macOS).
    VideoToolbox,
}

impl HwApi {
    pub fn name(self) -> &'static str {
        match self {
            Self::VideoToolbox => "VideoToolbox",
        }
    }
}

/// How a decoder is decoding its stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodePath {
    Hardware { api: HwApi },
    Software { reason: FallbackReason },
}

impl DecodePath {
    pub fn is_hardware(&self) -> bool {
        matches!(self, Self::Hardware { .. })
    }

    /// Why this stream decodes in software, if it does.
    pub fn fallback_reason(&self) -> Option<&FallbackReason> {
        match self {
            Self::Software { reason } => Some(reason),
            Self::Hardware { .. } => None,
        }
    }
}

/// Why a video stream decodes in software. Codec names are FFmpeg's (`h264`, `vp9`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// Hardware decoding was not asked for ([`HwPreference::Off`]), or the stream is audio.
    NotRequested,
    /// This build has no hardware decoder for the platform yet.
    NoHardwareApi,
    /// The platform has no hardware decoder for the codec (VP9 or AV1 on M1 and M2), or
    /// FFmpeg cannot drive it.
    CodecUnsupported { codec: String },
    /// The hardware decoder does not take this profile, chroma format or bit depth
    /// (RGB-coded H.264, for example). `format` is FFmpeg's pixel format name of
    /// the software output, or the platform's surface format.
    FormatUnsupported { codec: String, format: String },
    /// The hardware device could not be created.
    DeviceFailed { detail: String },
    /// The hardware decoder failed mid-stream; decoding continued in software from `at`,
    /// the last frame it delivered.
    DecodeFailed {
        codec: String,
        detail: String,
        at: Option<Timestamp>,
    },
}

impl FallbackReason {
    /// The `assets/i18n` key of the message the UI shows for this reason.
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::NotRequested => "decode.fallback.not_requested",
            Self::NoHardwareApi => "decode.fallback.no_hardware_api",
            Self::CodecUnsupported { .. } => "decode.fallback.codec_unsupported",
            Self::FormatUnsupported { .. } => "decode.fallback.format_unsupported",
            Self::DeviceFailed { .. } => "decode.fallback.device_failed",
            Self::DecodeFailed { .. } => "decode.fallback.decode_failed",
        }
    }

    /// The placeholder values of the message: `codec` as people name it (`H.264`),
    /// `format` as bit depth and chroma format (`10-bit 4:2:2`), `time` as `m:ss.cc`.
    pub fn i18n_args(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::NotRequested | Self::NoHardwareApi | Self::DeviceFailed { .. } => Vec::new(),
            Self::CodecUnsupported { codec } => vec![("codec", codec_display_name(codec))],
            Self::FormatUnsupported { codec, format } => vec![
                ("format", format_display_name(format)),
                ("codec", codec_display_name(codec)),
            ],
            Self::DecodeFailed { codec, at, .. } => vec![
                ("time", at.map_or_else(|| "0:00.00".to_string(), clock_time)),
                ("codec", codec_display_name(codec)),
            ],
        }
    }

    /// The message in the current locale, for the UI and the log.
    pub fn message(&self) -> String {
        let args = self.i18n_args();
        let refs: Vec<(&str, &dyn fmt::Display)> = args
            .iter()
            .map(|(name, value)| (*name, value as &dyn fmt::Display))
            .collect();
        assets::i18n::text_with(self.i18n_key(), &refs)
    }
}

/// How people name a codec, from FFmpeg's name.
fn codec_display_name(codec: &str) -> String {
    match codec {
        "h264" => "H.264",
        "hevc" => "HEVC",
        "vp8" => "VP8",
        "vp9" => "VP9",
        "av1" => "AV1",
        "prores" => "ProRes",
        "mpeg1video" => "MPEG-1",
        "mpeg2video" => "MPEG-2",
        "mpeg4" => "MPEG-4",
        "h263" => "H.263",
        "dnxhd" => "DNxHD",
        other => return other.to_uppercase(),
    }
    .to_string()
}

/// `10-bit 4:2:2` for an FFmpeg pixel format name; other names (a platform surface code)
/// are returned as they are.
fn format_display_name(format: &str) -> String {
    let Some(pixel) = crate::format::PixelFormat::from_name(format) else {
        return format.to_string();
    };
    if pixel.is_rgb() {
        return format!("{}-bit RGB", pixel.bit_depth());
    }
    let chroma = match pixel.chroma_shift() {
        (1, 1) => "4:2:0",
        (1, 0) => "4:2:2",
        (0, 0) => "4:4:4",
        (2, 0) => "4:1:1",
        _ => return format!("{}-bit", pixel.bit_depth()),
    };
    format!("{}-bit {chroma}", pixel.bit_depth())
}

fn clock_time(at: Timestamp) -> String {
    let centis = (at.seconds().max(0.0) * 100.0).round() as u64;
    format!(
        "{}:{:02}.{:02}",
        centis / 6000,
        centis / 100 % 60,
        centis % 100
    )
}

impl fmt::Display for FallbackReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRequested => f.write_str("hardware decoding not requested"),
            Self::NoHardwareApi => f.write_str("no hardware decoder on this platform yet"),
            Self::CodecUnsupported { codec } => write!(f, "no hardware decoder for {codec}"),
            Self::FormatUnsupported { codec, format } => {
                write!(f, "the hardware decoder does not take {format} {codec}")
            }
            Self::DeviceFailed { detail } => {
                write!(f, "the hardware device could not start: {detail}")
            }
            Self::DecodeFailed { codec, detail, at } => {
                write!(f, "the hardware decoder failed on {codec}")?;
                if let Some(at) = at {
                    write!(f, " after {at}")?;
                }
                write!(f, ": {detail}")
            }
        }
    }
}

/// Layout of a hardware surface: biplanar Y plus interleaved CbCr, 8 bits per sample or
/// 10 bits in the high bits of 16.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HwSurfaceFormat {
    /// 8-bit 4:2:0 (`420v`, `420f`).
    Nv12,
    /// 8-bit 4:2:2 (`422v`, `422f`).
    Nv16,
    /// 8-bit 4:4:4 (`444v`, `444f`).
    Nv24,
    /// 10-bit 4:2:0 in 16-bit samples (`x420`, `xf20`).
    P010,
    /// 10-bit 4:2:2 in 16-bit samples (`x422`, `xf22`).
    P210,
    /// 10-bit 4:4:4 in 16-bit samples (`x444`, `xf44`).
    P410,
}

impl HwSurfaceFormat {
    /// The format and whether it is full range, from a CoreVideo pixel format code.
    pub fn from_fourcc(code: [u8; 4]) -> Option<(Self, bool)> {
        Some(match &code {
            b"420v" => (Self::Nv12, false),
            b"420f" => (Self::Nv12, true),
            b"422v" => (Self::Nv16, false),
            b"422f" => (Self::Nv16, true),
            b"444v" => (Self::Nv24, false),
            b"444f" => (Self::Nv24, true),
            b"x420" => (Self::P010, false),
            b"xf20" => (Self::P010, true),
            b"x422" => (Self::P210, false),
            b"xf22" => (Self::P210, true),
            b"x444" => (Self::P410, false),
            b"xf44" => (Self::P410, true),
            _ => return None,
        })
    }

    /// Significant bits per sample (8 or 10).
    pub fn bit_depth(self) -> u8 {
        match self {
            Self::Nv12 | Self::Nv16 | Self::Nv24 => 8,
            Self::P010 | Self::P210 | Self::P410 => 10,
        }
    }

    /// Bytes per sample in each plane (1 or 2).
    pub fn bytes_per_sample(self) -> u8 {
        if self.bit_depth() > 8 { 2 } else { 1 }
    }

    /// log2 of the horizontal and vertical chroma subsampling.
    pub fn chroma_shift(self) -> (u8, u8) {
        match self {
            Self::Nv12 | Self::P010 => (1, 1),
            Self::Nv16 | Self::P210 => (1, 0),
            Self::Nv24 | Self::P410 => (0, 0),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Nv12 => "nv12",
            Self::Nv16 => "nv16",
            Self::Nv24 => "nv24",
            Self::P010 => "p010",
            Self::P210 => "p210",
            Self::P410 => "p410",
        }
    }
}

impl fmt::Display for HwSurfaceFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The platform handle of a hardware-decoded frame.
#[derive(Clone)]
enum Handle {
    #[cfg(target_os = "macos")]
    VideoToolbox(ffi::PixelBuffer),
}

/// A decoded frame that lives in a hardware surface, with no CPU copy: on macOS a
/// retained `CVPixelBuffer` backed by an `IOSurface`.
///
/// `Send + Sync` and cheap to clone (a retain). Holding it keeps the surface out of the
/// decoder's pool, so a renderer holds it only until the GPU work reading it completes.
#[derive(Clone)]
pub struct HwFrame {
    handle: Handle,
    format: HwSurfaceFormat,
    fourcc: [u8; 4],
    full_range: bool,
    width: u32,
    height: u32,
    colour: ColourInfo,
}

impl fmt::Debug for HwFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HwFrame")
            .field("api", &self.api())
            .field("format", &self.format)
            .field("fourcc", &String::from_utf8_lossy(&self.fourcc))
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl HwFrame {
    #[cfg(target_os = "macos")]
    pub(crate) fn from_videotoolbox(
        buffer: ffi::PixelBuffer,
        colour: ColourInfo,
    ) -> Result<Self, [u8; 4]> {
        let fourcc = buffer.fourcc();
        let (format, full_range) = HwSurfaceFormat::from_fourcc(fourcc).ok_or(fourcc)?;
        if buffer.plane_count() != 2 {
            return Err(fourcc);
        }
        let (width, height) = buffer.plane_size(0);
        Ok(Self {
            handle: Handle::VideoToolbox(buffer),
            format,
            fourcc,
            full_range,
            width,
            height,
            colour,
        })
    }

    pub fn api(&self) -> HwApi {
        match self.handle {
            #[cfg(target_os = "macos")]
            Handle::VideoToolbox(_) => HwApi::VideoToolbox,
        }
    }

    pub fn format(&self) -> HwSurfaceFormat {
        self.format
    }

    /// The platform's pixel format code (CoreVideo: `420v`, `x420`, ...).
    pub fn fourcc(&self) -> [u8; 4] {
        self.fourcc
    }

    /// Full-range samples, from the surface format.
    pub fn is_full_range(&self) -> bool {
        self.full_range
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Size in samples of plane 0 (luma) or 1 (interleaved chroma).
    pub fn plane_size(&self, plane: usize) -> Option<(u32, u32)> {
        let (sx, sy) = self.format.chroma_shift();
        match plane {
            0 => Some((self.width, self.height)),
            1 => Some((self.width.div_ceil(1 << sx), self.height.div_ceil(1 << sy))),
            _ => None,
        }
    }

    /// Colour metadata of the frame, as the bitstream tags it.
    pub fn colour(&self) -> ColourInfo {
        self.colour
    }

    /// The retained `CVPixelBuffer`.
    #[cfg(target_os = "macos")]
    pub fn pixel_buffer(&self) -> &objc2_core_video::CVPixelBuffer {
        match &self.handle {
            Handle::VideoToolbox(b) => b.buffer(),
        }
    }

    /// The `IOSurface` behind the pixel buffer (VideoToolbox output always has one).
    #[cfg(target_os = "macos")]
    pub fn io_surface(
        &self,
    ) -> Option<objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>> {
        match &self.handle {
            Handle::VideoToolbox(b) => b.io_surface(),
        }
    }
}

/// The VideoToolbox codec type for an FFmpeg codec that FFmpeg can decode through
/// VideoToolbox. `codec_tag` is the container's tag (FFmpeg byte order), which tells the
/// ProRes flavours apart.
#[cfg(target_os = "macos")]
pub(crate) fn videotoolbox_codec_type(codec: &str, codec_tag: u32) -> Option<[u8; 4]> {
    Some(match codec {
        "h264" => *b"avc1",
        "hevc" => *b"hvc1",
        "vp9" => *b"vp09",
        "av1" => *b"av01",
        "mpeg1video" => *b"mp1v",
        "mpeg2video" => *b"mp2v",
        "mpeg4" => *b"mp4v",
        "h263" => *b"h263",
        "prores" => {
            let tag = codec_tag.to_le_bytes();
            if tag.starts_with(b"ap") {
                tag
            } else {
                *b"apcn"
            }
        }
        _ => return None,
    })
}

/// Why hardware decoding cannot be tried for a codec on this machine, or `Ok` when the
/// decoder should try it.
pub(crate) fn hardware_candidate(codec: &str, codec_tag: u32) -> Result<HwApi, FallbackReason> {
    #[cfg(target_os = "macos")]
    {
        let unsupported = || FallbackReason::CodecUnsupported {
            codec: codec.to_string(),
        };
        let codec_type = videotoolbox_codec_type(codec, codec_tag).ok_or_else(unsupported)?;
        if ffi::videotoolbox_hardware_decode_supported(codec_type) {
            Ok(HwApi::VideoToolbox)
        } else {
            Err(unsupported())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (codec, codec_tag);
        Err(FallbackReason::NoHardwareApi)
    }
}

/// Whether this machine has a hardware decoder for an FFmpeg codec name (`h264`, `vp9`).
pub fn hardware_decoder_available(codec: &str) -> bool {
    hardware_candidate(codec, 0).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_formats_from_fourcc() {
        assert_eq!(
            HwSurfaceFormat::from_fourcc(*b"420v"),
            Some((HwSurfaceFormat::Nv12, false))
        );
        assert_eq!(
            HwSurfaceFormat::from_fourcc(*b"xf20"),
            Some((HwSurfaceFormat::P010, true))
        );
        assert_eq!(HwSurfaceFormat::from_fourcc(*b"BGRA"), None);
        assert_eq!(HwSurfaceFormat::P210.chroma_shift(), (1, 0));
        assert_eq!(HwSurfaceFormat::P010.bytes_per_sample(), 2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prores_codec_type_follows_the_tag() {
        let tag = u32::from_le_bytes(*b"apch");
        assert_eq!(videotoolbox_codec_type("prores", tag), Some(*b"apch"));
        assert_eq!(videotoolbox_codec_type("prores", 0), Some(*b"apcn"));
        assert_eq!(videotoolbox_codec_type("png", 0), None);
    }

    #[test]
    fn reasons_have_english_messages() {
        use crate::time::Rational;
        let reasons = [
            FallbackReason::NotRequested,
            FallbackReason::NoHardwareApi,
            FallbackReason::CodecUnsupported {
                codec: "vp9".into(),
            },
            FallbackReason::FormatUnsupported {
                codec: "h264".into(),
                format: "yuv422p10le".into(),
            },
            FallbackReason::DeviceFailed {
                detail: "no device".into(),
            },
            FallbackReason::DecodeFailed {
                codec: "hevc".into(),
                detail: "error".into(),
                at: Some(Timestamp::new(12_345, Rational::new(1, 1000))),
            },
        ];
        let english = assets::i18n::Locale::english();
        for reason in &reasons {
            let template = english
                .get(reason.i18n_key())
                .unwrap_or_else(|| panic!("{} missing from en.json", reason.i18n_key()));
            let mut placeholders = assets::i18n::placeholders(template).expect("balanced");
            placeholders.sort_unstable();
            let mut names: Vec<&str> = reason.i18n_args().iter().map(|(n, _)| *n).collect();
            names.sort_unstable();
            assert_eq!(placeholders, names, "{}", reason.i18n_key());
        }
        assert_eq!(
            reasons[2].message(),
            "Hardware decoding isn't available for VP9 on this Mac, so Glass is decoding it in software"
        );
        assert_eq!(
            reasons[3].message(),
            "Hardware decoding doesn't support 10-bit 4:2:2 H.264 on this Mac, so Glass is decoding it in software"
        );
        assert!(
            reasons[5].message().contains("0:12.35"),
            "{}",
            reasons[5].message()
        );
    }

    #[test]
    fn hardware_frames_move_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HwFrame>();
    }
}
