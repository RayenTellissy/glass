//! The colour of a still picture from its embedded description: a PNG `cICP` chunk, an
//! ICC profile's `cicp` tag, or the ICC profile's colorants and tone curve.
//!
//! Pixels are never converted here. The picture keeps its encoding and its frames carry
//! the primaries and transfer found (sRGB when nothing is embedded), so the render graph's
//! gamut conversion (Phase 4) takes Display P3 or BT.2020 stills into the working space
//! like any video. A profile Glass cannot name (Adobe RGB, ProPhoto, a camera profile) is
//! reported as [`IccProfile::Other`] and treated as sRGB until ICC transforms exist.

use crate::colour::{Primaries, TransferFn};

/// What an embedded colour description says, as far as Glass models it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IccProfile {
    /// sRGB, or an ICC profile with sRGB (BT.709) colorants.
    Srgb,
    /// Display P3 (P3 primaries, D65 white, sRGB curve).
    DisplayP3,
    /// BT.2020 primaries (with the transfer from the profile or its `cicp` tag).
    Bt2020,
    /// A profile with other colorants; `description` is its `desc` text when readable.
    Other { description: Option<String> },
}

/// Colour found in a picture's metadata. `None` fields fall back to sRGB.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmbeddedColour {
    pub primaries: Option<Primaries>,
    pub transfer: Option<TransferFn>,
    /// The profile or tag it came from, for the metadata panel.
    pub profile: Option<IccProfile>,
    /// Where it came from: `"cICP"` (PNG chunk), `"icc-cicp"` or `"icc"`.
    pub source: Option<&'static str>,
}

impl EmbeddedColour {
    /// From an ITU-T H.273 code point triple (PNG `cICP`, ICC `cicp`).
    fn from_cicp(primaries: u8, transfer: u8, source: &'static str) -> Self {
        let primaries = match primaries {
            1 => Some(Primaries::Bt709),
            5 => Some(Primaries::Bt601Pal),
            6 | 7 => Some(Primaries::Bt601Ntsc),
            9 => Some(Primaries::Bt2020),
            11 => Some(Primaries::DciP3),
            12 => Some(Primaries::DisplayP3),
            _ => None,
        };
        let transfer = match transfer {
            1 | 6 | 14 | 15 => Some(TransferFn::Bt1886),
            4 => Some(TransferFn::Gamma22),
            5 => Some(TransferFn::Gamma28),
            8 => Some(TransferFn::Linear),
            13 => Some(TransferFn::Srgb),
            16 => Some(TransferFn::Pq),
            18 => Some(TransferFn::Hlg),
            _ => None,
        };
        let profile = match primaries {
            Some(Primaries::DisplayP3) => IccProfile::DisplayP3,
            Some(Primaries::Bt2020) => IccProfile::Bt2020,
            Some(Primaries::Bt709) => IccProfile::Srgb,
            _ => IccProfile::Other { description: None },
        };
        Self {
            primaries,
            transfer,
            profile: Some(profile),
            source: Some(source),
        }
    }
}

/// The `cICP` chunk of a PNG file, if it has one before its image data.
pub(crate) fn png_cicp(bytes: &[u8]) -> Option<EmbeddedColour> {
    let mut rest = bytes.strip_prefix(b"\x89PNG\r\n\x1a\n")?;
    while rest.len() >= 12 {
        let len = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let kind = &rest[4..8];
        let data = rest.get(8..8 + len)?;
        match kind {
            b"cICP" if len == 4 => {
                return Some(EmbeddedColour::from_cicp(data[0], data[1], "cICP"));
            }
            b"IDAT" | b"IEND" => return None,
            _ => {}
        }
        rest = rest.get(12 + len..)?;
    }
    None
}

/// Reads an ICC profile: its `cicp` tag when present, otherwise its colorants (compared
/// with sRGB, Display P3 and BT.2020 after the profile's adaptation to D50) and red tone
/// curve. `None` when the bytes are not an RGB ICC profile.
pub(crate) fn parse_icc(bytes: &[u8]) -> Option<EmbeddedColour> {
    if bytes.len() < 132 || &bytes[36..40] != b"acsp" {
        return None;
    }
    let colour_space = &bytes[16..20];
    if colour_space != b"RGB " {
        return None;
    }
    let count = be_u32(bytes, 128)? as usize;
    let mut tags = Vec::with_capacity(count.min(64));
    for i in 0..count.min(256) {
        let at = 132 + i * 12;
        let signature: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        let offset = be_u32(bytes, at + 4)? as usize;
        let size = be_u32(bytes, at + 8)? as usize;
        if let Some(data) = bytes.get(offset..offset.checked_add(size)?) {
            tags.push((signature, data));
        }
    }
    let tag = |sig: &[u8; 4]| tags.iter().find(|(s, _)| s == sig).map(|(_, d)| *d);

    let description = tag(b"desc").and_then(description_text);
    if let Some(cicp) = tag(b"cicp").filter(|d| d.len() >= 12 && &d[..4] == b"cicp") {
        let mut colour = EmbeddedColour::from_cicp(cicp[8], cicp[9], "icc-cicp");
        if let Some(IccProfile::Other { description: d }) = &mut colour.profile {
            *d = description;
        }
        return Some(colour);
    }

    let colorant = |sig: &[u8; 4]| -> Option<(f64, f64)> {
        let d = tag(sig)?;
        if d.len() < 20 || &d[..4] != b"XYZ " {
            return None;
        }
        let x = s15_fixed16(d, 8)?;
        let y = s15_fixed16(d, 12)?;
        let z = s15_fixed16(d, 16)?;
        let sum = x + y + z;
        (sum > 0.0).then(|| (x / sum, y / sum))
    };
    let transfer = tag(b"rTRC").and_then(curve_transfer);
    let (primaries, profile) = match (colorant(b"rXYZ"), colorant(b"gXYZ"), colorant(b"bXYZ")) {
        (Some(r), Some(g), Some(b)) => match known_primaries([r, g, b]) {
            Some(p @ Primaries::Bt709) => (Some(p), IccProfile::Srgb),
            Some(p @ Primaries::DisplayP3) => (Some(p), IccProfile::DisplayP3),
            Some(p @ Primaries::Bt2020) => (Some(p), IccProfile::Bt2020),
            _ => (None, IccProfile::Other { description }),
        },
        _ => (None, IccProfile::Other { description }),
    };
    Some(EmbeddedColour {
        primaries,
        transfer,
        profile: Some(profile),
        source: Some("icc"),
    })
}

/// Chromaticities of the D50-adapted (Bradford) colorants ICC profiles store.
const D50_COLORANTS: [(Primaries, [(f64, f64); 3]); 3] = [
    (
        Primaries::Bt709,
        [(0.6484, 0.3309), (0.3212, 0.5978), (0.1559, 0.0660)],
    ),
    (
        Primaries::DisplayP3,
        [(0.6822, 0.3193), (0.2845, 0.6747), (0.1559, 0.0661)],
    ),
    (
        Primaries::Bt2020,
        [(0.7085, 0.2935), (0.1902, 0.7753), (0.1292, 0.0472)],
    ),
];

fn known_primaries(colorants: [(f64, f64); 3]) -> Option<Primaries> {
    D50_COLORANTS.iter().find_map(|(primaries, expected)| {
        let close = colorants
            .iter()
            .zip(expected)
            .all(|(a, b)| (a.0 - b.0).abs() < 0.004 && (a.1 - b.1).abs() < 0.004);
        close.then_some(*primaries)
    })
}

/// The transfer a `curv` or `para` tone curve describes, when it is one Glass models.
fn curve_transfer(d: &[u8]) -> Option<TransferFn> {
    match d.get(..4)? {
        b"curv" => {
            let entries = be_u32(d, 8)?;
            match entries {
                0 => Some(TransferFn::Linear),
                1 => {
                    let gamma =
                        f64::from(u16::from_be_bytes(d.get(12..14)?.try_into().ok()?)) / 256.0;
                    gamma_transfer(gamma)
                }
                // A sampled curve: compare its midpoint with the sRGB curve.
                n if n >= 16 => {
                    let n = n as usize;
                    let mid = n / 2;
                    let at = 12 + mid * 2;
                    let v = f64::from(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
                        / 65535.0;
                    let x = mid as f64 / (n - 1) as f64;
                    ((v - TransferFn::Srgb.to_linear(x)).abs() < 0.01).then_some(TransferFn::Srgb)
                }
                _ => None,
            }
        }
        b"para" => {
            let function = u16::from_be_bytes(d.get(8..10)?.try_into().ok()?);
            let g = s15_fixed16(d, 12)?;
            match function {
                0 => gamma_transfer(g),
                3 | 4 if (g - 2.4).abs() < 0.01 => {
                    let a = s15_fixed16(d, 16)?;
                    ((a - 1.0 / 1.055).abs() < 0.001).then_some(TransferFn::Srgb)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn gamma_transfer(gamma: f64) -> Option<TransferFn> {
    if (gamma - 1.0).abs() < 0.01 {
        Some(TransferFn::Linear)
    } else if (gamma - 2.2).abs() < 0.02 {
        Some(TransferFn::Gamma22)
    } else {
        None
    }
}

/// The text of a `desc` tag (ICC v2 `desc` ASCII or v4 `mluc` UTF-16, first record).
fn description_text(d: &[u8]) -> Option<String> {
    match d.get(..4)? {
        b"desc" => {
            let len = be_u32(d, 8)? as usize;
            let text = d.get(12..12 + len)?;
            let text = text.split(|b| *b == 0).next()?;
            Some(String::from_utf8_lossy(text).into_owned())
        }
        b"mluc" => {
            let records = be_u32(d, 8)?;
            if records == 0 {
                return None;
            }
            let len = be_u32(d, 20)? as usize;
            let offset = be_u32(d, 24)? as usize;
            let units: Vec<u16> = d
                .get(offset..offset + len)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        _ => None,
    }
}

fn be_u32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn s15_fixed16(d: &[u8], at: usize) -> Option<f64> {
    Some(f64::from(i32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?)) / 65536.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ICC v2 RGB profile with the given D50 colorants and a `para` sRGB curve.
    fn profile(colorants: [(f64, f64, f64); 3], cicp: Option<[u8; 4]>) -> Vec<u8> {
        let fixed = |v: f64| ((v * 65536.0).round() as i32).to_be_bytes();
        let mut tags: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        for (sig, (x, y, z)) in [*b"rXYZ", *b"gXYZ", *b"bXYZ"].into_iter().zip(colorants) {
            let mut d = b"XYZ \0\0\0\0".to_vec();
            for v in [x, y, z] {
                d.extend_from_slice(&fixed(v));
            }
            tags.push((sig, d));
        }
        let mut para = b"para\0\0\0\0\0\x03\0\0".to_vec();
        for v in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
            para.extend_from_slice(&fixed(v));
        }
        tags.push((*b"rTRC", para));
        if let Some(c) = cicp {
            let mut d = b"cicp\0\0\0\0".to_vec();
            d.extend_from_slice(&c);
            tags.push((*b"cicp", d));
        }
        let mut out = vec![0u8; 128];
        out[16..20].copy_from_slice(b"RGB ");
        out[36..40].copy_from_slice(b"acsp");
        out.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut offset = 132 + tags.len() * 12;
        let mut data = Vec::new();
        for (sig, d) in &tags {
            out.extend_from_slice(sig);
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(d.len() as u32).to_be_bytes());
            offset += d.len();
            data.extend_from_slice(d);
        }
        out.extend(data);
        out
    }

    const SRGB: [(f64, f64, f64); 3] = [
        (0.4361, 0.2225, 0.0139),
        (0.3851, 0.7169, 0.0971),
        (0.1431, 0.0606, 0.7141),
    ];
    const P3: [(f64, f64, f64); 3] = [
        (0.5151, 0.2412, -0.0011),
        (0.2919, 0.6922, 0.0419),
        (0.1572, 0.0666, 0.7841),
    ];
    const ADOBE: [(f64, f64, f64); 3] = [
        (0.6097, 0.3111, 0.0195),
        (0.2053, 0.6257, 0.0609),
        (0.1492, 0.0632, 0.7446),
    ];

    #[test]
    fn colorants_name_the_gamut() {
        let srgb = parse_icc(&profile(SRGB, None)).expect("parses");
        assert_eq!(srgb.primaries, Some(Primaries::Bt709));
        assert_eq!(srgb.transfer, Some(TransferFn::Srgb));
        assert_eq!(srgb.profile, Some(IccProfile::Srgb));
        let p3 = parse_icc(&profile(P3, None)).expect("parses");
        assert_eq!(p3.primaries, Some(Primaries::DisplayP3));
        assert_eq!(p3.profile, Some(IccProfile::DisplayP3));
        let adobe = parse_icc(&profile(ADOBE, None)).expect("parses");
        assert_eq!(adobe.primaries, None);
        assert!(matches!(adobe.profile, Some(IccProfile::Other { .. })));
    }

    #[test]
    fn cicp_tag_wins_over_colorants() {
        let pq = parse_icc(&profile(SRGB, Some([9, 16, 0, 1]))).expect("parses");
        assert_eq!(pq.primaries, Some(Primaries::Bt2020));
        assert_eq!(pq.transfer, Some(TransferFn::Pq));
        assert_eq!(pq.source, Some("icc-cicp"));
    }

    #[test]
    fn not_a_profile() {
        assert_eq!(parse_icc(b"not a profile"), None);
        assert_eq!(parse_icc(&[0; 200]), None);
    }

    #[test]
    fn png_cicp_chunk() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0; 13 + 4]);
        png.extend_from_slice(&4u32.to_be_bytes());
        png.extend_from_slice(b"cICP");
        png.extend_from_slice(&[12, 13, 0, 1, 0, 0, 0, 0]);
        let colour = png_cicp(&png).expect("cICP found");
        assert_eq!(colour.primaries, Some(Primaries::DisplayP3));
        assert_eq!(colour.transfer, Some(TransferFn::Srgb));
        assert_eq!(png_cicp(b"\x89PNG\r\n\x1a\n"), None);
    }

    /// The system's own profiles, where the OS ships them (macOS).
    #[test]
    fn system_profiles() {
        let dir = std::path::Path::new("/System/Library/ColorSync/Profiles");
        for (file, expected) in [
            ("sRGB Profile.icc", IccProfile::Srgb),
            ("Display P3.icc", IccProfile::DisplayP3),
        ] {
            let Ok(bytes) = std::fs::read(dir.join(file)) else {
                eprintln!("skipped: {file} not on this system");
                continue;
            };
            let colour = parse_icc(&bytes).expect("parses");
            assert_eq!(colour.profile, Some(expected), "{file}");
            assert_eq!(colour.transfer, Some(TransferFn::Srgb), "{file}");
        }
        if let Ok(bytes) = std::fs::read(dir.join("AdobeRGB1998.icc")) {
            let colour = parse_icc(&bytes).expect("parses");
            assert!(
                matches!(&colour.profile, Some(IccProfile::Other { description: Some(d) }) if d.contains("Adobe")),
                "{colour:?}"
            );
        }
    }
}
