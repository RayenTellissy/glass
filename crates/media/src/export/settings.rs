//! What an export writes: container, codecs, rate control, pixel format and colour tags.

use crate::encode::{PcmFormat, encoder_available, encoder_pixel_formats};
use crate::format::{ColourInfo, ColourRange, PixelFormat};
use crate::subtitle::SubtitleCodec;
use crate::time::Rational;

use super::{ExportError, Result};

/// The file kind an export writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Container {
    Mp4,
    Mov,
    Matroska,
    WebM,
    /// One picture file per frame (PNG or TIFF), numbered.
    ImageSequence,
    Gif,
    Wav,
    Flac,
    /// AAC in an MPEG-4 audio file.
    M4a,
    Mp3,
}

impl Container {
    pub fn name(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Mov => "QuickTime",
            Self::Matroska => "Matroska",
            Self::WebM => "WebM",
            Self::ImageSequence => "image sequence",
            Self::Gif => "GIF",
            Self::Wav => "WAV",
            Self::Flac => "FLAC",
            Self::M4a => "M4A",
            Self::Mp3 => "MP3",
        }
    }

    /// The usual file extension (an image sequence's comes from its codec).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Matroska => "mkv",
            Self::WebM => "webm",
            Self::ImageSequence => "png",
            Self::Gif => "gif",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::M4a => "m4a",
            Self::Mp3 => "mp3",
        }
    }

    /// FFmpeg's muxer, for the containers FFmpeg writes.
    pub(crate) fn muxer(self) -> Option<&'static str> {
        match self {
            Self::Mp4 => Some("mp4"),
            Self::Mov => Some("mov"),
            Self::Matroska => Some("matroska"),
            Self::WebM => Some("webm"),
            Self::Wav => Some("wav"),
            Self::Flac => Some("flac"),
            Self::M4a => Some("ipod"),
            Self::Mp3 => Some("mp3"),
            Self::ImageSequence | Self::Gif => None,
        }
    }

    pub fn accepts_video(self, codec: VideoCodec) -> bool {
        use VideoCodec as V;
        match self {
            Self::Mp4 => matches!(codec, V::H264 | V::Hevc | V::Av1 | V::Vp9),
            Self::Mov => matches!(codec, V::H264 | V::Hevc | V::ProRes(_)),
            Self::Matroska => matches!(codec, V::H264 | V::Hevc | V::Av1 | V::Vp9 | V::ProRes(_)),
            Self::WebM => matches!(codec, V::Av1 | V::Vp9),
            Self::ImageSequence => matches!(codec, V::Png | V::Tiff),
            Self::Gif => codec == V::Gif,
            Self::Wav | Self::Flac | Self::M4a | Self::Mp3 => false,
        }
    }

    pub fn accepts_audio(self, codec: AudioCodec) -> bool {
        use AudioCodec as A;
        match self {
            Self::Mp4 => matches!(codec, A::Aac | A::Mp3),
            Self::Mov => matches!(codec, A::Aac | A::Pcm(_)),
            Self::Matroska => matches!(codec, A::Aac | A::Mp3 | A::Flac { .. } | A::Pcm(_)),
            Self::Wav => matches!(codec, A::Pcm(_)),
            Self::Flac => matches!(codec, A::Flac { .. }),
            Self::M4a => codec == A::Aac,
            Self::Mp3 => codec == A::Mp3,
            Self::WebM | Self::ImageSequence | Self::Gif => false,
        }
    }

    /// Whether the container can carry text subtitle streams (`mov_text` in MP4 and MOV,
    /// WebVTT in Matroska and WebM).
    pub fn accepts_subtitles(self) -> bool {
        self.subtitle_codec().is_some()
    }

    pub(crate) fn subtitle_codec(self) -> Option<SubtitleCodec> {
        match self {
            Self::Mp4 | Self::Mov => Some(SubtitleCodec::MovText),
            Self::Matroska | Self::WebM => Some(SubtitleCodec::WebVtt),
            _ => None,
        }
    }

    /// Whether the container holds pictures (audio-only containers do not).
    pub fn has_video(self) -> bool {
        !matches!(self, Self::Wav | Self::Flac | Self::M4a | Self::Mp3)
    }
}

/// Apple ProRes flavours, lightest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProResProfile {
    Proxy,
    Lt,
    Standard,
    Hq,
    P4444,
    Xq,
}

impl ProResProfile {
    pub fn is_444(self) -> bool {
        matches!(self, Self::P4444 | Self::Xq)
    }

    /// The profile value of `prores_videotoolbox` and `prores_ks`.
    fn option(self, encoder: &str) -> &'static str {
        let vt = encoder == "prores_videotoolbox";
        match self {
            Self::Proxy => "proxy",
            Self::Lt => "lt",
            Self::Standard => "standard",
            Self::Hq => "hq",
            Self::P4444 => "4444",
            Self::Xq if vt => "xq",
            Self::Xq => "4444xq",
        }
    }
}

/// A video codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
    Vp9,
    ProRes(ProResProfile),
    Png,
    Tiff,
    Gif,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Av1 => "AV1",
            Self::Vp9 => "VP9",
            Self::ProRes(_) => "ProRes",
            Self::Png => "PNG",
            Self::Tiff => "TIFF",
            Self::Gif => "GIF",
        }
    }

    /// Software FFmpeg encoders for this codec in this feature set, preferred first.
    /// Empty for GIF, which the session encodes itself.
    pub(crate) fn software_encoders(self) -> Vec<&'static str> {
        match self {
            Self::H264 => vec!["libx264"],
            Self::Hevc => vec!["libx265"],
            Self::Av1 => vec!["libsvtav1", "libaom-av1", "librav1e"],
            Self::Vp9 => vec!["libvpx-vp9"],
            Self::ProRes(_) if cfg!(feature = "codec-prores-sw") => vec!["prores_ks"],
            Self::ProRes(_) => Vec::new(),
            Self::Png => vec!["png"],
            Self::Tiff => vec!["tiff"],
            Self::Gif => Vec::new(),
        }
    }

    /// Hardware FFmpeg encoders for this codec on this platform, preferred first:
    /// VideoToolbox on macOS; NVENC, Quick Sync and AMF on Windows and Linux. A build may
    /// lack some of them, and a machine without the GPU fails to open them.
    pub(crate) fn hardware_encoders(self) -> Vec<&'static str> {
        let mac = cfg!(target_os = "macos");
        let pc = cfg!(any(target_os = "windows", target_os = "linux"));
        let list: &[(&'static str, bool)] = match self {
            Self::H264 => &[
                ("h264_videotoolbox", mac),
                ("h264_nvenc", pc),
                ("h264_qsv", pc),
                ("h264_amf", pc),
            ],
            Self::Hevc => &[
                ("hevc_videotoolbox", mac),
                ("hevc_nvenc", pc),
                ("hevc_qsv", pc),
                ("hevc_amf", pc),
            ],
            Self::ProRes(_) => &[("prores_videotoolbox", mac)],
            Self::Av1 | Self::Vp9 | Self::Png | Self::Tiff | Self::Gif => &[],
        };
        list.iter().filter(|(_, on)| *on).map(|(n, _)| *n).collect()
    }

    /// Encoders that take this codec's name in [`VideoSettings::encoder`] besides the
    /// software and hardware lists. The software ProRes encoder stays behind its feature
    /// flag.
    fn accepts_encoder(self, name: &str) -> bool {
        if self.software_encoders().contains(&name) || self.hardware_encoders().contains(&name) {
            return true;
        }
        match self {
            Self::H264 => name.starts_with("h264_"),
            Self::Hevc => name.starts_with("hevc_"),
            Self::Av1 => {
                name.starts_with("av1_") || name.starts_with("libaom") || name == "librav1e"
            }
            Self::Vp9 => name.starts_with("vp9_"),
            Self::ProRes(_) => name == "prores_videotoolbox" && cfg!(target_os = "macos"),
            Self::Png | Self::Tiff | Self::Gif => false,
        }
    }

    /// The constant rate factor the software encoder uses by default, which the hardware
    /// encoders' quality settings are mapped from.
    fn default_crf(self) -> f32 {
        match self {
            Self::Hevc => 28.0,
            _ => 23.0,
        }
    }

    /// Picture codecs written as RGB files (PNG, TIFF, GIF), not YUV video.
    pub fn is_rgb(self) -> bool {
        matches!(self, Self::Png | Self::Tiff | Self::Gif)
    }

    /// The pixel format an encoder gets when the settings name none.
    fn default_pixel_format(self, encoder: &str, hdr: bool) -> &'static str {
        match self {
            Self::H264 | Self::Hevc | Self::Av1 | Self::Vp9 => {
                match (is_hardware_encoder(encoder), hdr) {
                    // The semi-planar layouts every hardware encoder takes.
                    (true, false) => "nv12",
                    (true, true) => "p010le",
                    (false, false) => "yuv420p",
                    (false, true) => "yuv420p10le",
                }
            }
            Self::ProRes(p) => match (encoder == "prores_videotoolbox", p.is_444()) {
                (true, false) => "p210le",
                (true, true) => "p410le",
                (false, false) => "yuv422p10le",
                (false, true) => "yuv444p10le",
            },
            Self::Png | Self::Tiff => "rgb24",
            Self::Gif => "rgba",
        }
    }
}

/// Which kind of encoder an export uses for codecs that have hardware encoders (H.264,
/// HEVC, ProRes on macOS); other codecs always use their software encoder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EncoderPreference {
    /// The platform's hardware encoder when it opens and takes the first frame, else the
    /// software one (x264, x265; `prores_ks` with `codec-prores-sw`).
    #[default]
    Auto,
    /// Hardware encoders only: the export fails rather than fall back.
    Hardware,
    /// Software encoders only.
    Software,
}

/// The hardware encoder family an FFmpeg encoder name belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HardwareFamily {
    VideoToolbox,
    Nvenc,
    Qsv,
    Amf,
}

impl HardwareFamily {
    pub(crate) fn of(encoder: &str) -> Option<Self> {
        if encoder.ends_with("_videotoolbox") {
            Some(Self::VideoToolbox)
        } else if encoder.ends_with("_nvenc") {
            Some(Self::Nvenc)
        } else if encoder.ends_with("_qsv") {
            Some(Self::Qsv)
        } else if encoder.ends_with("_amf") {
            Some(Self::Amf)
        } else {
            None
        }
    }
}

/// Whether an FFmpeg encoder name is a hardware encoder (VideoToolbox, NVENC, Quick Sync,
/// AMF).
pub fn is_hardware_encoder(name: &str) -> bool {
    HardwareFamily::of(name).is_some()
}

/// One encoder an export may open, with the pixel format it is fed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EncoderChoice {
    pub(crate) name: String,
    pub(crate) format: PixelFormat,
}

/// The encoders an export tries in order: [`VideoSettings::encoder`] alone when set,
/// else the hardware encoders and then the software ones as the
/// [`EncoderPreference`] allows, keeping those this build has and whose pixel format
/// fits the settings.
pub(crate) fn video_encoder_choices(settings: &VideoSettings) -> Result<Vec<EncoderChoice>> {
    let codec = settings.codec;
    if let Some(name) = &settings.encoder {
        if !codec.accepts_encoder(name) {
            return Err(ExportError::Settings(format!(
                "{name} is not a {} encoder this build may use",
                codec.name()
            )));
        }
        if !encoder_available(name) {
            return Err(ExportError::Unavailable {
                codec: codec.name(),
                tried: vec![leak_free_name(name)],
            });
        }
        let format = settings.pixel_format_for(name)?;
        return Ok(vec![EncoderChoice {
            name: name.clone(),
            format,
        }]);
    }
    let hardware = codec.hardware_encoders();
    let (use_hardware, use_software) = match settings.encoder_preference {
        EncoderPreference::Auto => (true, true),
        // Codecs without hardware encoders ignore the preference.
        EncoderPreference::Hardware => (true, hardware.is_empty()),
        EncoderPreference::Software => (false, true),
    };
    let mut tried = Vec::new();
    let mut choices = Vec::new();
    let mut first_error = None;
    let names = use_hardware
        .then_some(hardware)
        .into_iter()
        .flatten()
        .chain(
            use_software
                .then(|| codec.software_encoders())
                .into_iter()
                .flatten(),
        );
    for name in names {
        tried.push(name);
        if !encoder_available(name) {
            continue;
        }
        match settings.pixel_format_for(name) {
            Ok(format) => choices.push(EncoderChoice {
                name: name.to_string(),
                format,
            }),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    if choices.is_empty() {
        return Err(first_error.unwrap_or(ExportError::Unavailable {
            codec: codec.name(),
            tried,
        }));
    }
    Ok(choices)
}

/// The FFmpeg encoder an export tries first: [`VideoSettings::encoder`], or the first of
/// the codec's encoders the [`EncoderPreference`] allows that this build has. Under
/// [`EncoderPreference::Auto`] a hardware encoder that fails to open or to encode the
/// first frame is replaced by the next; [`ExportSession::video_encoder`]
/// (super::ExportSession::video_encoder) says which one runs.
pub fn video_encoder_for(settings: &VideoSettings) -> Result<String> {
    let mut choices = video_encoder_choices(settings)?;
    Ok(choices.swap_remove(0).name)
}

/// FFmpeg's `FF_QP2LAMBDA`: `global_quality` is a quantiser in lambda units.
const FF_QP2LAMBDA: u32 = 118;

/// VideoToolbox's constant quality (1 to 100) for an x264-style CRF: CRF 23 lands near
/// 60, which on the test pictures matches x264's CRF 23 in PSNR at a similar size.
fn videotoolbox_quality(crf: f32) -> u32 {
    (100.0 - 1.75 * crf).round().clamp(1.0, 100.0) as u32
}

/// A requested encoder name as a static string for [`ExportError::Unavailable`]; names
/// outside the known lists read as "the requested encoder".
fn leak_free_name(name: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "libx264",
        "libx265",
        "h264_videotoolbox",
        "hevc_videotoolbox",
        "h264_nvenc",
        "hevc_nvenc",
        "h264_qsv",
        "hevc_qsv",
        "h264_amf",
        "hevc_amf",
        "prores_videotoolbox",
        "prores_ks",
        "libsvtav1",
        "libaom-av1",
        "librav1e",
        "libvpx-vp9",
    ];
    KNOWN
        .iter()
        .find(|k| **k == name)
        .copied()
        .unwrap_or("the requested encoder")
}

/// How the encoder spends bits.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum RateControl {
    /// The encoder's own default (x264 CRF 23, x265 CRF 28, hardware encoders the
    /// quality those map to; ProRes, PNG, TIFF and GIF have no rate setting).
    #[default]
    Default,
    /// Constant quality (x264, x265, AV1 and VP9 encoders): lower is better. Hardware
    /// H.264 and HEVC encoders map it onto their own quality scale.
    Crf(f32),
    /// Average bit rate in bits per second, with an optional peak.
    Bitrate { average: u64, max: Option<u64> },
}

/// The colour tags written to the stream, which also choose the RGB to YUV matrix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColourTags {
    /// BT.709 primaries, transfer and matrix, limited range: SDR video (Decision 21).
    #[default]
    Bt709,
    /// BT.2020 primaries and non-constant-luminance matrix with the PQ transfer.
    Bt2020Pq,
    /// BT.2020 primaries and non-constant-luminance matrix with the HLG transfer.
    Bt2020Hlg,
    /// BT.709 primaries with the sRGB transfer, full range: stills and GIFs.
    Srgb,
}

impl ColourTags {
    pub fn is_hdr(self) -> bool {
        matches!(self, Self::Bt2020Pq | Self::Bt2020Hlg)
    }

    /// The tags for a YUV stream (`rgb` false) or an RGB one.
    pub fn info(self, rgb: bool) -> ColourInfo {
        let (primaries, transfer, matrix) = match self {
            Self::Bt709 => ("bt709", "bt709", "bt709"),
            Self::Bt2020Pq => ("bt2020", "smpte2084", "bt2020nc"),
            Self::Bt2020Hlg => ("bt2020", "arib-std-b67", "bt2020nc"),
            Self::Srgb => ("bt709", "iec61966-2-1", "bt709"),
        };
        if rgb {
            ColourInfo {
                primaries: Some(primaries),
                transfer: Some(transfer),
                matrix: Some("gbr"),
                range: ColourRange::Full,
                chroma_location: None,
            }
        } else {
            ColourInfo {
                primaries: Some(primaries),
                transfer: Some(transfer),
                matrix: Some(matrix),
                range: if self == Self::Srgb {
                    ColourRange::Full
                } else {
                    ColourRange::Limited
                },
                chroma_location: Some("left"),
            }
        }
    }
}

/// SMPTE ST 2086 mastering display colour volume, in the units HEVC's SEI message and
/// x265 use: chromaticity coordinates in 1/50000, luminance in 1/10000 cd/m².
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MasteringDisplay {
    pub red: (u16, u16),
    pub green: (u16, u16),
    pub blue: (u16, u16),
    pub white: (u16, u16),
    pub max_luminance: u32,
    pub min_luminance: u32,
}

impl MasteringDisplay {
    /// P3 primaries, D65 white, 1000 to 0.0001 cd/m²: the usual HDR10 grading display.
    pub const P3_D65_1000: Self = Self {
        red: (34_000, 16_000),
        green: (13_250, 34_500),
        blue: (7_500, 3_000),
        white: (15_635, 16_450),
        max_luminance: 10_000_000,
        min_luminance: 1,
    };

    /// x265's `master-display` value.
    fn x265_value(&self) -> String {
        let Self {
            red: (rx, ry),
            green: (gx, gy),
            blue: (bx, by),
            white: (wx, wy),
            max_luminance,
            min_luminance,
        } = *self;
        format!(
            "G({gx},{gy})B({bx},{by})R({rx},{ry})WP({wx},{wy})L({max_luminance},{min_luminance})"
        )
    }
}

/// CTA-861.3 content light levels in cd/m²: the brightest pixel (MaxCLL) and the
/// brightest frame average (MaxFALL).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentLight {
    pub max_cll: u16,
    pub max_fall: u16,
}

/// Static HDR metadata for PQ exports: written into the container (MP4 and MOV `mdcv`
/// and `clli` boxes, Matroska colour elements) and, by x265, into the HEVC stream's SEI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HdrMetadata {
    pub mastering: Option<MasteringDisplay>,
    pub content_light: Option<ContentLight>,
}

/// Settings of the video stream.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoSettings {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    /// Constant frame rate; frames are numbered at it.
    pub frame_rate: Rational,
    /// The encoder's pixel format; `None` picks the codec's (4:2:0 8-bit, 10-bit for HDR
    /// tags, 4:2:2 or 4:4:4 10-bit for ProRes, RGB for pictures).
    pub pixel_format: Option<PixelFormat>,
    pub rate: RateControl,
    pub colour: ColourTags,
    /// Mastering display and content light levels; HDR tags only.
    pub hdr_metadata: Option<HdrMetadata>,
    /// Keyframe interval in frames; `None` keeps the encoder's.
    pub gop: Option<u32>,
    pub max_b_frames: Option<u32>,
    /// Encoder speed preset (`medium`, `slow` for x264 and x265).
    pub preset: Option<String>,
    /// Encoder threads; zero lets FFmpeg choose.
    pub threads: usize,
    /// Hardware or software encoding when [`encoder`](Self::encoder) names none.
    pub encoder_preference: EncoderPreference,
    /// One FFmpeg encoder for the codec (`h264_videotoolbox`, `libx264`), used without
    /// fallback; `None` picks by [`encoder_preference`](Self::encoder_preference).
    pub encoder: Option<String>,
    /// Further encoder options, as FFmpeg's command line would pass them; they go to
    /// whichever encoder opens, a fallback included.
    pub options: Vec<(String, String)>,
}

impl VideoSettings {
    /// Settings with the codec's defaults: BT.709 tags for video, sRGB for pictures.
    pub fn new(codec: VideoCodec, width: u32, height: u32, frame_rate: Rational) -> Self {
        Self {
            codec,
            width,
            height,
            frame_rate,
            pixel_format: None,
            rate: RateControl::Default,
            colour: if codec.is_rgb() {
                ColourTags::Srgb
            } else {
                ColourTags::Bt709
            },
            hdr_metadata: None,
            gop: None,
            max_b_frames: None,
            preset: None,
            threads: 0,
            encoder_preference: EncoderPreference::Auto,
            encoder: None,
            options: Vec::new(),
        }
    }

    /// The pixel format for `encoder`: the settings' or the codec's default. Hardware
    /// encoders of 4:2:0 video get the semi-planar layout of the same depth (NV12, P010),
    /// and hardware H.264 is 8-bit only.
    pub(crate) fn pixel_format_for(&self, encoder: &str) -> Result<PixelFormat> {
        let hardware_video = is_hardware_encoder(encoder)
            && matches!(self.codec, VideoCodec::H264 | VideoCodec::Hevc);
        let format = match self.pixel_format {
            Some(f) if hardware_video => {
                let semi_planar = match f.name() {
                    "yuv420p" => Some("nv12"),
                    "yuv420p10le" => Some("p010le"),
                    _ => None,
                };
                semi_planar.and_then(PixelFormat::from_name).unwrap_or(f)
            }
            Some(f) => f,
            None => {
                let name = self
                    .codec
                    .default_pixel_format(encoder, self.colour.is_hdr());
                PixelFormat::from_name(name)
                    .ok_or_else(|| ExportError::Settings(format!("FFmpeg does not know {name}")))?
            }
        };
        if hardware_video && self.codec == VideoCodec::H264 && format.bit_depth() > 8 {
            return Err(ExportError::Settings(format!(
                "{encoder} encodes 8-bit H.264 only, not {format}"
            )));
        }
        if self.codec != VideoCodec::Gif {
            let accepted = encoder_pixel_formats(encoder);
            if !accepted.is_empty() && !accepted.contains(&format) {
                return Err(ExportError::Settings(format!(
                    "{encoder} does not take {format}"
                )));
            }
        }
        Ok(format)
    }

    /// Encoder options for the codec's rate control, preset and profile. Hardware H.264
    /// and HEVC encoders get a constant quality mapped from the CRF (the software
    /// encoder's default CRF for [`RateControl::Default`]); the preset is a software
    /// encoder setting.
    pub(crate) fn encoder_options(&self, encoder: &str) -> Vec<(String, String)> {
        let mut options = Vec::new();
        let mut push = |k: &str, v: String| options.push((k.to_string(), v));
        let hardware = match self.codec {
            VideoCodec::H264 | VideoCodec::Hevc => HardwareFamily::of(encoder),
            _ => None,
        };
        if let Some(preset) = &self.preset
            && hardware.is_none()
        {
            push("preset", preset.clone());
        }
        match (self.rate, hardware) {
            (RateControl::Bitrate { max, .. }, _) => {
                if let Some(max) = max {
                    push("maxrate", max.to_string());
                    push("bufsize", (max * 2).to_string());
                }
            }
            (RateControl::Default, None) => {}
            (RateControl::Crf(crf), None) => push("crf", format!("{crf}")),
            (RateControl::Default | RateControl::Crf(_), Some(family)) => {
                let crf = match self.rate {
                    RateControl::Crf(crf) => crf,
                    _ => self.codec.default_crf(),
                };
                let qp = crf.round().clamp(0.0, 51.0) as u32;
                match family {
                    HardwareFamily::VideoToolbox => {
                        // Constant quality (Apple silicon): 0 to 100, as FFmpeg's
                        // `-q:v`, which arrives as `global_quality` in lambda units.
                        push("flags", "+qscale".to_string());
                        let quality = videotoolbox_quality(crf);
                        push("global_quality", (quality * FF_QP2LAMBDA).to_string());
                    }
                    HardwareFamily::Nvenc => {
                        push("rc", "vbr".to_string());
                        push("cq", qp.to_string());
                        push("b", "0".to_string());
                    }
                    HardwareFamily::Qsv => {
                        // Intelligent constant quality.
                        push("global_quality", qp.max(1).to_string());
                    }
                    HardwareFamily::Amf => {
                        push("rc", "cqp".to_string());
                        for frame in ["qp_i", "qp_p", "qp_b"] {
                            push(frame, qp.to_string());
                        }
                    }
                }
            }
        }
        if encoder == "libx265" {
            let mut params = vec!["log-level=error".to_string()];
            let hdr = self.hdr_metadata.unwrap_or_default();
            if let Some(m) = hdr.mastering {
                params.push(format!("master-display={}", m.x265_value()));
            }
            if let Some(c) = hdr.content_light {
                params.push(format!("max-cll={},{}", c.max_cll, c.max_fall));
            }
            push("x265-params", params.join(":"));
        }
        if let VideoCodec::ProRes(profile) = self.codec {
            push("profile", profile.option(encoder).to_string());
            if encoder == "prores_videotoolbox" {
                // Apple's software ProRes encoder where the chip has no ProRes engine (M1).
                push("allow_sw", "1".to_string());
            } else {
                push("vendor", "apl0".to_string());
            }
        }
        options.extend(self.options.iter().cloned());
        options
    }

    /// Frames are timed in `1 / frame_rate`.
    pub(crate) fn time_base(&self) -> Rational {
        Rational::new(self.frame_rate.den, self.frame_rate.num)
    }
}

/// An audio codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioCodec {
    /// Uncompressed little-endian PCM.
    Pcm(PcmFormat),
    Aac,
    Mp3,
    /// Lossless, 16 or 24 bits per sample.
    Flac {
        bits: u8,
    },
}

impl AudioCodec {
    pub fn name(self) -> &'static str {
        match self {
            Self::Pcm(_) => "PCM",
            Self::Aac => "AAC",
            Self::Mp3 => "MP3",
            Self::Flac { .. } => "FLAC",
        }
    }

    pub(crate) fn encoder(self) -> &'static str {
        match self {
            Self::Pcm(PcmFormat::S16) => "pcm_s16le",
            Self::Pcm(PcmFormat::S24) => "pcm_s24le",
            Self::Pcm(PcmFormat::F32) => "pcm_f32le",
            Self::Aac => "aac",
            Self::Mp3 => "libmp3lame",
            Self::Flac { .. } => "flac",
        }
    }
}

/// Settings of the audio stream. Samples go in as planar f32 at this rate and channel
/// count (1 mono, 2 stereo, 6 for 5.1).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSettings {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: usize,
    /// Bits per second for lossy codecs; `None` is 128 kbit/s per channel.
    pub bit_rate: Option<u64>,
}

impl AudioSettings {
    pub fn new(codec: AudioCodec, sample_rate: u32, channels: usize) -> Self {
        Self {
            codec,
            sample_rate,
            channels,
            bit_rate: None,
        }
    }
}

/// Everything an export writes.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSettings {
    pub container: Container,
    pub video: Option<VideoSettings>,
    pub audio: Option<AudioSettings>,
    /// Text subtitle streams, in stream order; cues go to
    /// [`ExportSession::push_subtitle`](super::ExportSession::push_subtitle).
    pub subtitles: Vec<SubtitleSettings>,
    /// MP4, MOV and M4A: move the index to the front when finishing, so playback can
    /// start before the whole file has arrived (one more pass over the file).
    pub fast_start: bool,
    /// Image sequences: the number of the first file.
    pub first_number: u64,
}

impl ExportSettings {
    pub fn new(container: Container) -> Self {
        Self {
            container,
            video: None,
            audio: None,
            subtitles: Vec::new(),
            fast_start: true,
            first_number: 0,
        }
    }

    pub fn with_video(mut self, video: VideoSettings) -> Self {
        self.video = Some(video);
        self
    }

    pub fn with_audio(mut self, audio: AudioSettings) -> Self {
        self.audio = Some(audio);
        self
    }

    /// Adds a text subtitle stream after the ones already added.
    pub fn with_subtitles(mut self, subtitles: SubtitleSettings) -> Self {
        self.subtitles.push(subtitles);
        self
    }

    /// Checks what can be checked before any file is created.
    pub fn validate(&self) -> Result<()> {
        let container = self.container;
        if self.video.is_none() && self.audio.is_none() {
            return Err(ExportError::Settings("nothing to export".into()));
        }
        if let Some(v) = &self.video {
            if !container.accepts_video(v.codec) {
                return Err(ExportError::Incompatible {
                    container: container.name(),
                    codec: v.codec.name(),
                });
            }
            if v.width == 0 || v.height == 0 {
                return Err(ExportError::Settings(format!(
                    "picture size {}x{}",
                    v.width, v.height
                )));
            }
            let fps = v.frame_rate.to_f64().filter(|f| f.is_finite() && *f > 0.0);
            let Some(fps) = fps.filter(|_| v.frame_rate.num > 0 && v.frame_rate.den > 0) else {
                return Err(ExportError::Settings(format!(
                    "frame rate {}/{}",
                    v.frame_rate.num, v.frame_rate.den
                )));
            };
            if v.codec == VideoCodec::Gif {
                if v.width > u32::from(u16::MAX) || v.height > u32::from(u16::MAX) {
                    return Err(ExportError::Settings(
                        "GIF pictures are at most 65535 pixels a side".into(),
                    ));
                }
                // Players stretch delays under 2/100 s to 1/10 s.
                if fps > 50.0 {
                    return Err(ExportError::Settings(
                        "GIF frame rates are at most 50 fps".into(),
                    ));
                }
            }
            if v.hdr_metadata.is_some() && !v.colour.is_hdr() {
                return Err(ExportError::Settings(
                    "HDR metadata needs PQ or HLG colour tags".into(),
                ));
            }
            if v.codec.is_rgb() && v.colour != ColourTags::Srgb {
                return Err(ExportError::Settings(format!(
                    "{} is written with sRGB tags",
                    v.codec.name()
                )));
            }
            if let Some(format) = v.pixel_format {
                if v.colour.is_hdr() && format.bit_depth() < 10 {
                    return Err(ExportError::Settings(format!(
                        "HDR needs 10 bits or more, {format} has {}",
                        format.bit_depth()
                    )));
                }
                let (sx, sy) = format.chroma_shift();
                if v.width % (1 << sx) != 0 || v.height % (1 << sy) != 0 {
                    return Err(ExportError::Settings(format!(
                        "{format} needs a width divisible by {} and a height by {}",
                        1 << sx,
                        1 << sy
                    )));
                }
            } else if !v.codec.is_rgb() && (v.width % 2 != 0 || v.height % 2 != 0) {
                return Err(ExportError::Settings(format!(
                    "{} needs an even picture size, not {}x{}",
                    v.codec.name(),
                    v.width,
                    v.height
                )));
            }
        } else if container.has_video() {
            return Err(ExportError::Settings(format!(
                "{} needs a video stream",
                container.name()
            )));
        }
        if let Some(a) = &self.audio {
            if !container.accepts_audio(a.codec) {
                return Err(ExportError::Incompatible {
                    container: container.name(),
                    codec: a.codec.name(),
                });
            }
            if a.sample_rate == 0 || !(1..=8).contains(&a.channels) {
                return Err(ExportError::Settings(format!(
                    "{} Hz with {} channels",
                    a.sample_rate, a.channels
                )));
            }
            if let AudioCodec::Flac { bits } = a.codec
                && bits != 16
                && bits != 24
            {
                return Err(ExportError::Settings(format!("FLAC at {bits} bits")));
            }
        }
        if !self.subtitles.is_empty() && !container.accepts_subtitles() {
            return Err(ExportError::Incompatible {
                container: container.name(),
                codec: "text subtitles",
            });
        }
        for subtitles in &self.subtitles {
            let language = &subtitles.language;
            if language.len() != 3 || !language.bytes().all(|b| b.is_ascii_lowercase()) {
                return Err(ExportError::Settings(format!(
                    "subtitle language {language:?} is not an ISO 639-2 code"
                )));
            }
        }
        Ok(())
    }
}

/// A text subtitle stream: `mov_text` (3GPP timed text) in MP4 and MOV, WebVTT in
/// Matroska and WebM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleSettings {
    /// ISO 639-2 code in lowercase (`eng`, `fra`, `deu`), `und` when unknown.
    pub language: String,
    /// The stream's name in players' subtitle menus.
    pub title: Option<String>,
}

impl SubtitleSettings {
    pub fn new(language: impl Into<String>) -> Self {
        Self {
            language: language.into(),
            title: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}
