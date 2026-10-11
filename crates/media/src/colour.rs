//! The resolved colour description of a picture: [`ColourSpace`] (primaries, transfer,
//! matrix, range, chroma siting, bit depth and HDR kind), with every field filled in.
//!
//! Files tag some of these values and leave others out ([`ColourInfo`] keeps the raw
//! tags). [`ColourSpace::resolve`] fills the gaps with one documented rule, so every
//! consumer (the GPU conversion, thumbnails, export) treats an untagged file the same way:
//!
//! | Field | Tagged | Untagged RGB (PNG, `gbrp`) | Untagged JPEG-range YUV (`yuvj*`) | Untagged YUV |
//! | --- | --- | --- | --- | --- |
//! | matrix | the tag | identity | BT.601 | BT.709 above 576 lines, BT.601 at or below; BT.2020 when the primaries or transfer are BT.2020, PQ or HLG |
//! | range | the tag | full | full | limited (a hardware surface's own full-range flag when it has one) |
//! | primaries | the tag | BT.709 (sRGB) | BT.709 | follow the matrix: BT.709, BT.2020, or BT.601 625-line at 576 and 288 lines and 525-line otherwise |
//! | transfer | the tag | sRGB | sRGB | BT.1886 (the BT.709 family) |
//! | chroma siting | the tag | centre (no subsampling) | centre | left (the MPEG-2, H.264 and HEVC default) |
//! | bit depth | from the pixel or surface format | | | |
//! | HDR kind | PQ or HLG from the transfer, else SDR | | | |
//!
//! Tags FFmpeg knows but Glass does not model (film primaries, `log100` transfer, YCgCo)
//! count as untagged. Which fields were filled in is kept in [`ColourSpace::defaulted`].

use crate::format::{ColourInfo, ColourRange, Hdr};

/// Colour primaries (the RGB gamut and white point).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Primaries {
    /// ITU-R BT.709, also sRGB's.
    Bt709,
    /// ITU-R BT.601 625-line (`bt470bg`, PAL and SECAM).
    Bt601Pal,
    /// ITU-R BT.601 525-line (`smpte170m`, `smpte240m`, NTSC).
    Bt601Ntsc,
    /// ITU-R BT.2020 and BT.2100.
    Bt2020,
    /// SMPTE RP 431-2, DCI white.
    DciP3,
    /// SMPTE EG 432-1, D65 white (Display P3).
    DisplayP3,
}

impl Primaries {
    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "bt709" => Self::Bt709,
            "bt470bg" => Self::Bt601Pal,
            "smpte170m" | "smpte240m" => Self::Bt601Ntsc,
            "bt2020" => Self::Bt2020,
            "smpte431" => Self::DciP3,
            "smpte432" => Self::DisplayP3,
            _ => return None,
        })
    }

    /// FFmpeg's name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bt709 => "bt709",
            Self::Bt601Pal => "bt470bg",
            Self::Bt601Ntsc => "smpte170m",
            Self::Bt2020 => "bt2020",
            Self::DciP3 => "smpte431",
            Self::DisplayP3 => "smpte432",
        }
    }
}

/// The transfer function between the coded signal and light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransferFn {
    /// ITU-R BT.1886 display EOTF (a 2.4 power with black at zero), used for every
    /// BT.709-family tag (`bt709`, `smpte170m`, `smpte240m`, `bt2020-10`, `bt2020-12`).
    Bt1886,
    /// IEC 61966-2-1 sRGB (`iec61966-2-1`).
    Srgb,
    /// A pure 2.2 power (`bt470m`).
    Gamma22,
    /// A pure 2.8 power (`bt470bg`).
    Gamma28,
    /// Linear light (`linear`).
    Linear,
    /// SMPTE ST 2084 perceptual quantizer (`smpte2084`).
    Pq,
    /// ARIB STD-B67 hybrid log-gamma (`arib-std-b67`).
    Hlg,
}

/// Luminance of SDR reference white in nits (ITU-R BT.2408). Linear light is expressed
/// relative to it: 1.0 is SDR white, PQ's 10,000 nits is about 49.3.
pub const REFERENCE_WHITE_NITS: f64 = 203.0;

/// The nominal peak of an HLG display (ITU-R BT.2100): the HLG OOTF is applied for it,
/// which puts HLG's 75% reference white at 203 nits.
pub const HLG_PEAK_NITS: f64 = 1000.0;

impl TransferFn {
    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "bt709" | "smpte170m" | "smpte240m" | "bt2020-10" | "bt2020-12" => Self::Bt1886,
            "iec61966-2-1" | "iec61966-2-4" => Self::Srgb,
            "bt470m" => Self::Gamma22,
            "bt470bg" => Self::Gamma28,
            "linear" => Self::Linear,
            "smpte2084" => Self::Pq,
            "arib-std-b67" => Self::Hlg,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bt1886 => "bt709",
            Self::Srgb => "iec61966-2-1",
            Self::Gamma22 => "bt470m",
            Self::Gamma28 => "bt470bg",
            Self::Linear => "linear",
            Self::Pq => "smpte2084",
            Self::Hlg => "arib-std-b67",
        }
    }

    pub fn hdr(self) -> Hdr {
        match self {
            Self::Pq => Hdr::Pq,
            Self::Hlg => Hdr::Hlg,
            _ => Hdr::None,
        }
    }

    /// Linear light, relative to SDR reference white, of one encoded component in 0..1.
    ///
    /// For HLG this is only the inverse OETF (scene light, 0..1/12 scaled to 0..1); HLG's
    /// display light needs all three components, see [`Self::to_linear_rgb`].
    pub fn to_linear(self, v: f64) -> f64 {
        let v = v.clamp(0.0, 1.0);
        match self {
            Self::Bt1886 => v.powf(2.4),
            Self::Srgb => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Self::Gamma22 => v.powf(2.2),
            Self::Gamma28 => v.powf(2.8),
            Self::Linear => v,
            Self::Pq => pq_eotf_nits(v) / REFERENCE_WHITE_NITS,
            Self::Hlg => hlg_inverse_oetf(v),
        }
    }

    /// Linear light of an encoded R'G'B' triple, relative to SDR reference white. HLG
    /// applies the inverse OETF and then the BT.2100 OOTF for a 1000-nit display (system
    /// gamma 1.2, luminance from BT.2020 weights), so HLG 75% white lands at 1.0.
    pub fn to_linear_rgb(self, rgb: [f64; 3]) -> [f64; 3] {
        if self != Self::Hlg {
            return rgb.map(|v| self.to_linear(v));
        }
        let scene = rgb.map(hlg_inverse_oetf);
        let ys = 0.2627 * scene[0] + 0.6780 * scene[1] + 0.0593 * scene[2];
        let gain = if ys > 0.0 {
            HLG_PEAK_NITS * ys.powf(HLG_SYSTEM_GAMMA - 1.0) / REFERENCE_WHITE_NITS
        } else {
            0.0
        };
        scene.map(|e| e * gain)
    }
}

/// HLG system gamma for a 1000-nit display (ITU-R BT.2100).
pub const HLG_SYSTEM_GAMMA: f64 = 1.2;

const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

/// SMPTE ST 2084: display luminance in nits of a PQ signal in 0..1.
pub fn pq_eotf_nits(v: f64) -> f64 {
    let p = v.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    let num = (p - PQ_C1).max(0.0);
    let den = PQ_C2 - PQ_C3 * p;
    10_000.0 * (num / den).powf(1.0 / PQ_M1)
}

/// The PQ signal (0..1) for a luminance in nits.
pub fn pq_inverse_eotf(nits: f64) -> f64 {
    let y = (nits / 10_000.0).clamp(0.0, 1.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M2)
}

const HLG_A: f64 = 0.178_832_77;
const HLG_B: f64 = 1.0 - 4.0 * HLG_A;
// 0.5 - a * ln(4a)
const HLG_C: f64 = 0.559_910_73;

/// ARIB STD-B67 inverse OETF: normalised scene light (0..1) of an HLG signal (0..1).
pub fn hlg_inverse_oetf(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.5 {
        v * v / 3.0
    } else {
        (((v - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

/// The Y'CbCr to R'G'B' matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Matrix {
    /// No matrix: the planes already hold G, B and R (`gbr`, RGB pixel formats).
    Identity,
    /// ITU-R BT.601 (`bt470bg`, `smpte170m`, `fcc`).
    Bt601,
    /// ITU-R BT.709.
    Bt709,
    /// ITU-R BT.2020 non-constant luminance (`bt2020nc`; the rare constant-luminance
    /// `bt2020c` is decoded with the same weights).
    Bt2020,
    /// SMPTE 240M.
    Smpte240m,
}

impl Matrix {
    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "gbr" => Self::Identity,
            "bt470bg" | "smpte170m" | "fcc" => Self::Bt601,
            "bt709" => Self::Bt709,
            "bt2020nc" | "bt2020c" => Self::Bt2020,
            "smpte240m" => Self::Smpte240m,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Identity => "gbr",
            Self::Bt601 => "smpte170m",
            Self::Bt709 => "bt709",
            Self::Bt2020 => "bt2020nc",
            Self::Smpte240m => "smpte240m",
        }
    }

    /// Luma weights (Kr, Kb); `None` for the identity.
    pub fn weights(self) -> Option<(f64, f64)> {
        match self {
            Self::Identity => None,
            Self::Bt601 => Some((0.299, 0.114)),
            Self::Bt709 => Some((0.2126, 0.0722)),
            Self::Bt2020 => Some((0.2627, 0.0593)),
            Self::Smpte240m => Some((0.212, 0.087)),
        }
    }
}

/// Signal range of the coded values, resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Range {
    /// 16..235 luma and 16..240 chroma at 8 bits, scaled by 2^(n-8) at n bits.
    Limited,
    /// 0..2^n-1.
    Full,
}

/// Where chroma samples sit relative to luma in subsampled pictures (FFmpeg's names;
/// see ITU-T H.273 chroma sample location types).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChromaSiting {
    /// Co-sited with the left luma sample horizontally, between rows vertically (type 0;
    /// MPEG-2, H.264 and HEVC default).
    Left,
    /// Centred between luma samples in both directions (type 1; MPEG-1, JPEG).
    Center,
    /// Co-sited with the top-left luma sample (type 2; common in BT.2020 masters).
    TopLeft,
    /// Centred horizontally, co-sited with the top row (type 3).
    Top,
    /// Co-sited with the left sample of the bottom row (type 4).
    BottomLeft,
    /// Centred horizontally, co-sited with the bottom row (type 5).
    Bottom,
}

impl ChromaSiting {
    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "left" => Self::Left,
            "center" => Self::Center,
            "topleft" => Self::TopLeft,
            "top" => Self::Top,
            "bottomleft" => Self::BottomLeft,
            "bottom" => Self::Bottom,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::TopLeft => "topleft",
            Self::Top => "top",
            Self::BottomLeft => "bottomleft",
            Self::Bottom => "bottom",
        }
    }

    /// Where the first chroma sample sits, in luma samples right of and below the first
    /// luma sample, for chroma subsampled by `2^shift` in each direction: co-sited 0,
    /// centred `(2^shift - 1) / 2`, bottom `2^shift - 1`. A direction without
    /// subsampling is always 0.
    pub fn offset(self, shift: (u8, u8)) -> (f32, f32) {
        let span = |s: u8| f32::from((1u16 << s.min(4)) - 1);
        let (h, v) = match self {
            Self::Left => (0.0, 0.5),
            Self::Center => (0.5, 0.5),
            Self::TopLeft => (0.0, 0.0),
            Self::Top => (0.5, 0.0),
            Self::BottomLeft => (0.0, 1.0),
            Self::Bottom => (0.5, 1.0),
        };
        (h * span(shift.0), v * span(shift.1))
    }

    /// [`Self::offset`] in 1/256 luma samples, as swscale's `src_h_chr_pos` and
    /// `src_v_chr_pos` (and FFmpeg's `av_chroma_location_enum_to_pos` for 4:2:0) take it.
    pub fn position_256(self, shift: (u8, u8)) -> (i32, i32) {
        let (h, v) = self.offset(shift);
        ((h * 256.0).round() as i32, (v * 256.0).round() as i32)
    }
}

/// Which fields of a [`ColourSpace`] were filled in by the defaulting rule rather than
/// read from a tag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Defaulted {
    pub primaries: bool,
    pub transfer: bool,
    pub matrix: bool,
    pub range: bool,
    pub siting: bool,
}

impl Defaulted {
    pub fn any(self) -> bool {
        self.primaries || self.transfer || self.matrix || self.range || self.siting
    }
}

/// What the defaulting rule needs to know about the picture's sample layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutHint {
    /// Significant bits per sample.
    pub bit_depth: u8,
    /// RGB samples (no matrix).
    pub rgb: bool,
    /// A JPEG-range YUV format (`yuvj420p` and siblings): full range and BT.601.
    pub jpeg: bool,
    /// The surface's own range flag, for hardware surfaces that have one.
    pub surface_full_range: Option<bool>,
    /// Picture height in lines, which picks the untagged matrix and primaries.
    pub height: u32,
}

/// The complete colour description of a picture, with nothing left unspecified. Carried
/// on every decoded frame ([`VideoFrame::colour_space`](crate::VideoFrame::colour_space),
/// [`HwFrame::colour_space`](crate::HwFrame::colour_space)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColourSpace {
    pub primaries: Primaries,
    pub transfer: TransferFn,
    pub matrix: Matrix,
    pub range: Range,
    pub siting: ChromaSiting,
    /// Significant bits per sample: 8, 10, 12 (or 16).
    pub bit_depth: u8,
    pub hdr: Hdr,
    /// Fields the file did not tag, filled in by the rule in the module docs.
    pub defaulted: Defaulted,
}

impl ColourSpace {
    /// Standard dynamic range BT.709 HD video: limited range, left siting, 8-bit.
    pub const BT709: Self = Self {
        primaries: Primaries::Bt709,
        transfer: TransferFn::Bt1886,
        matrix: Matrix::Bt709,
        range: Range::Limited,
        siting: ChromaSiting::Left,
        bit_depth: 8,
        hdr: Hdr::None,
        defaulted: Defaulted {
            primaries: false,
            transfer: false,
            matrix: false,
            range: false,
            siting: false,
        },
    };

    /// These values as explicit tags (FFmpeg's names), for writing a file that must be
    /// read the same way whatever its size (an untagged picture's defaults depend on it).
    pub fn tags(&self) -> ColourInfo {
        ColourInfo {
            primaries: Some(self.primaries.name()),
            transfer: Some(self.transfer.name()),
            matrix: Some(self.matrix.name()),
            range: match self.range {
                Range::Limited => ColourRange::Limited,
                Range::Full => ColourRange::Full,
            },
            chroma_location: Some(self.siting.name()),
        }
    }

    /// Fills in the untagged fields of `tags` for a picture laid out as `layout` (see the
    /// module docs for the rule).
    pub fn resolve(tags: &ColourInfo, layout: LayoutHint) -> Self {
        let tagged_primaries = tags.primaries.and_then(Primaries::from_tag);
        let tagged_transfer = tags.transfer.and_then(TransferFn::from_tag);
        let tagged_matrix = tags.matrix.and_then(Matrix::from_tag);
        let tagged_siting = tags.chroma_location.and_then(ChromaSiting::from_tag);
        let tagged_range = match tags.range {
            ColourRange::Limited => Some(Range::Limited),
            ColourRange::Full => Some(Range::Full),
            ColourRange::Unspecified => None,
        };
        let wide = tagged_primaries == Some(Primaries::Bt2020)
            || matches!(tagged_transfer, Some(TransferFn::Pq | TransferFn::Hlg));

        let matrix = tagged_matrix.unwrap_or(if layout.rgb {
            Matrix::Identity
        } else if layout.jpeg {
            Matrix::Bt601
        } else if wide {
            Matrix::Bt2020
        } else if layout.height > 576 {
            Matrix::Bt709
        } else {
            Matrix::Bt601
        });
        let range = tagged_range.unwrap_or(
            if layout.rgb || layout.jpeg || layout.surface_full_range == Some(true) {
                Range::Full
            } else {
                Range::Limited
            },
        );
        let primaries = tagged_primaries.unwrap_or(match matrix {
            Matrix::Identity | Matrix::Bt709 => Primaries::Bt709,
            _ if layout.jpeg => Primaries::Bt709,
            Matrix::Bt2020 => Primaries::Bt2020,
            Matrix::Bt601 | Matrix::Smpte240m => {
                if matches!(layout.height, 576 | 288) {
                    Primaries::Bt601Pal
                } else {
                    Primaries::Bt601Ntsc
                }
            }
        });
        let transfer = tagged_transfer.unwrap_or(if layout.rgb || layout.jpeg {
            TransferFn::Srgb
        } else {
            TransferFn::Bt1886
        });
        let siting = tagged_siting.unwrap_or(if layout.rgb || layout.jpeg {
            ChromaSiting::Center
        } else {
            ChromaSiting::Left
        });
        Self {
            primaries,
            transfer,
            matrix,
            range,
            siting,
            bit_depth: layout.bit_depth,
            hdr: transfer.hdr(),
            defaulted: Defaulted {
                primaries: tagged_primaries.is_none(),
                transfer: tagged_transfer.is_none(),
                matrix: tagged_matrix.is_none(),
                range: tagged_range.is_none(),
                siting: tagged_siting.is_none(),
            },
        }
    }

    pub fn is_full_range(&self) -> bool {
        self.range == Range::Full
    }

    /// The same description with another chroma siting (tests compare sitings).
    pub fn with_siting(self, siting: ChromaSiting) -> Self {
        Self { siting, ..self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(
        primaries: Option<&'static str>,
        transfer: Option<&'static str>,
        matrix: Option<&'static str>,
        range: ColourRange,
        siting: Option<&'static str>,
    ) -> ColourInfo {
        ColourInfo {
            primaries,
            transfer,
            matrix,
            range,
            chroma_location: siting,
        }
    }

    const UNTAGGED: ColourInfo = ColourInfo {
        primaries: None,
        transfer: None,
        matrix: None,
        range: ColourRange::Unspecified,
        chroma_location: None,
    };

    fn yuv(bit_depth: u8, height: u32) -> LayoutHint {
        LayoutHint {
            bit_depth,
            rgb: false,
            jpeg: false,
            surface_full_range: None,
            height,
        }
    }

    #[test]
    fn untagged_hd_is_bt709_limited_left() {
        let c = ColourSpace::resolve(&UNTAGGED, yuv(8, 1080));
        assert_eq!(
            c,
            ColourSpace {
                defaulted: Defaulted {
                    primaries: true,
                    transfer: true,
                    matrix: true,
                    range: true,
                    siting: true,
                },
                ..ColourSpace::BT709
            }
        );
        assert!(c.defaulted.any());
        assert_eq!(
            ColourSpace::resolve(&UNTAGGED, yuv(10, 2160)).matrix,
            Matrix::Bt709
        );
    }

    #[test]
    fn untagged_sd_is_bt601_with_line_count_primaries() {
        let pal = ColourSpace::resolve(&UNTAGGED, yuv(8, 576));
        assert_eq!(pal.matrix, Matrix::Bt601);
        assert_eq!(pal.primaries, Primaries::Bt601Pal);
        assert_eq!(pal.range, Range::Limited);
        let ntsc = ColourSpace::resolve(&UNTAGGED, yuv(8, 480));
        assert_eq!(ntsc.matrix, Matrix::Bt601);
        assert_eq!(ntsc.primaries, Primaries::Bt601Ntsc);
        assert_eq!(ntsc.transfer, TransferFn::Bt1886);
    }

    #[test]
    fn untagged_rgb_is_srgb_full() {
        let rgb = LayoutHint {
            rgb: true,
            ..yuv(8, 1080)
        };
        let c = ColourSpace::resolve(&UNTAGGED, rgb);
        assert_eq!(c.matrix, Matrix::Identity);
        assert_eq!(c.range, Range::Full);
        assert_eq!(c.primaries, Primaries::Bt709);
        assert_eq!(c.transfer, TransferFn::Srgb);
        assert_eq!(c.hdr, Hdr::None);
        // PNG frames arrive tagged `gbr` and full range by FFmpeg; the result is the same.
        let tagged = tags(None, None, Some("gbr"), ColourRange::Full, None);
        assert_eq!(ColourSpace::resolve(&tagged, rgb).matrix, Matrix::Identity);
    }

    #[test]
    fn jpeg_range_yuv_is_full_bt601_srgb_centred() {
        let jpeg = LayoutHint {
            jpeg: true,
            ..yuv(8, 1080)
        };
        let c = ColourSpace::resolve(&UNTAGGED, jpeg);
        assert_eq!(
            (c.matrix, c.range, c.transfer, c.siting),
            (
                Matrix::Bt601,
                Range::Full,
                TransferFn::Srgb,
                ChromaSiting::Center
            )
        );
    }

    #[test]
    fn hdr_tags_resolve_to_bt2020() {
        let pq = tags(
            Some("bt2020"),
            Some("smpte2084"),
            Some("bt2020nc"),
            ColourRange::Limited,
            Some("topleft"),
        );
        let c = ColourSpace::resolve(&pq, yuv(10, 2160));
        assert_eq!(c.hdr, Hdr::Pq);
        assert_eq!(c.siting, ChromaSiting::TopLeft);
        assert_eq!(c.bit_depth, 10);
        assert!(!c.defaulted.any());
        // A transfer tag alone pulls the untagged matrix and primaries to BT.2020.
        let hlg_only = tags(
            None,
            Some("arib-std-b67"),
            None,
            ColourRange::Unspecified,
            None,
        );
        let c = ColourSpace::resolve(&hlg_only, yuv(10, 1080));
        assert_eq!(
            (c.matrix, c.primaries, c.hdr),
            (Matrix::Bt2020, Primaries::Bt2020, Hdr::Hlg)
        );
    }

    #[test]
    fn tags_win_and_unknown_tags_count_as_untagged() {
        let t = tags(
            Some("film"),
            Some("log100"),
            Some("bt709"),
            ColourRange::Full,
            None,
        );
        let c = ColourSpace::resolve(&t, yuv(8, 480));
        assert_eq!(c.matrix, Matrix::Bt709);
        assert_eq!(c.range, Range::Full);
        assert_eq!(c.primaries, Primaries::Bt709);
        assert_eq!(c.transfer, TransferFn::Bt1886);
        assert!(c.defaulted.primaries && c.defaulted.transfer && !c.defaulted.matrix);
    }

    #[test]
    fn surface_range_applies_only_when_untagged() {
        let hint = LayoutHint {
            surface_full_range: Some(true),
            ..yuv(8, 1080)
        };
        assert_eq!(ColourSpace::resolve(&UNTAGGED, hint).range, Range::Full);
        let limited = tags(None, None, None, ColourRange::Limited, None);
        assert_eq!(ColourSpace::resolve(&limited, hint).range, Range::Limited);
    }

    #[test]
    fn transfer_functions_at_known_points() {
        let close = |a: f64, b: f64, tol: f64| assert!((a - b).abs() <= tol, "{a} vs {b}");
        close(TransferFn::Bt1886.to_linear(0.75), 0.75f64.powf(2.4), 1e-12);
        close(TransferFn::Srgb.to_linear(0.5), 0.214_041, 1e-6);
        close(TransferFn::Srgb.to_linear(0.04), 0.04 / 12.92, 1e-12);
        close(pq_eotf_nits(1.0), 10_000.0, 1e-6);
        close(pq_eotf_nits(0.0), 0.0, 1e-9);
        // BT.2408: PQ 58% is 203 nits.
        close(pq_eotf_nits(pq_inverse_eotf(203.0)), 203.0, 1e-6);
        close(pq_inverse_eotf(203.0), 0.580_7, 1e-3);
        close(TransferFn::Pq.to_linear(1.0), 10_000.0 / 203.0, 1e-9);
        close(hlg_inverse_oetf(0.5), 1.0 / 12.0, 1e-12);
        close(hlg_inverse_oetf(1.0), 1.0, 1e-6);
        // BT.2408: HLG 75% white on a 1000-nit display is 203 nits.
        let white = TransferFn::Hlg.to_linear_rgb([0.75; 3]);
        close(white[0], 1.0, 0.01);
        let peak = TransferFn::Hlg.to_linear_rgb([1.0; 3]);
        close(peak[1], 1000.0 / 203.0, 1e-3);
        assert_eq!(TransferFn::Hlg.to_linear_rgb([0.0; 3]), [0.0; 3]);
    }

    #[test]
    fn siting_positions_match_ffmpeg() {
        assert_eq!(ChromaSiting::Left.position_256((1, 1)), (0, 128));
        assert_eq!(ChromaSiting::Center.position_256((1, 1)), (128, 128));
        assert_eq!(ChromaSiting::TopLeft.position_256((1, 1)), (0, 0));
        assert_eq!(ChromaSiting::Bottom.position_256((1, 1)), (128, 256));
        // 4:2:2 has no vertical subsampling; 4:1:1 centres 1.5 samples in.
        assert_eq!(ChromaSiting::Left.position_256((1, 0)), (0, 0));
        assert_eq!(ChromaSiting::Center.position_256((2, 0)), (384, 0));
    }
}
