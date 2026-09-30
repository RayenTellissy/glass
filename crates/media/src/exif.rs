//! Camera EXIF from stills: the TIFF-structured block JPEG (APP1), PNG (`eXIf`), WebP and
//! TIFF files carry, read by a small forgiving parser. Only the fields the metadata panel
//! shows are kept; a malformed block gives what could be read before the damage, never an
//! error.

use std::io::BufReader;
use std::path::Path;

use image::{ImageDecoder, ImageReader};

/// The EXIF fields Glass keeps, as the camera stored them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Exif {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    /// `DateTimeOriginal` (else `DateTime`) as stored: `2026:08:12 19:48:07`.
    pub date_time: Option<String>,
    /// `OffsetTimeOriginal` (else `OffsetTime`): `+02:00`.
    pub offset: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub f_number: Option<f64>,
    pub iso: Option<u32>,
    /// Exposure time as a fraction of a second (`(1, 50)`).
    pub exposure_time: Option<(u32, u32)>,
    /// WGS 84 degrees, north and east positive.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Metres above sea level (negative below).
    pub altitude_m: Option<f64>,
}

impl Exif {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Reads a still's EXIF through its `image` decoder. `None` when the file has none or is
/// not a picture the `image` crate reads (HEIC goes through ImageIO and is not covered).
pub fn read_exif(path: &Path) -> Option<Exif> {
    let file = std::fs::File::open(path).ok()?;
    let reader = ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let block = decoder.exif_metadata().ok()??;
    parse(&block).filter(|e| !e.is_empty())
}

const EXIF_IFD: u16 = 0x8769;
const GPS_IFD: u16 = 0x8825;

/// Parses an EXIF block: a TIFF header and its IFDs, with or without the `Exif\0\0`
/// prefix JPEG's APP1 segment uses.
pub fn parse(block: &[u8]) -> Option<Exif> {
    let data = block.strip_prefix(b"Exif\0\0").unwrap_or(block);
    let tiff = Tiff::new(data)?;
    let mut exif = Exif::default();
    let ifd0 = tiff.u32(4)? as usize;
    let mut exif_ifd = None;
    let mut gps_ifd = None;
    let mut date_time = None;
    let mut offset = None;
    for e in tiff.entries(ifd0) {
        match e.tag {
            0x010F => exif.make = tiff.ascii(&e),
            0x0110 => exif.model = tiff.ascii(&e),
            0x0132 => date_time = tiff.ascii(&e),
            EXIF_IFD => exif_ifd = tiff.uint(&e, 0),
            GPS_IFD => gps_ifd = tiff.uint(&e, 0),
            _ => {}
        }
    }
    if let Some(at) = exif_ifd {
        let mut plain_offset = None;
        for e in tiff.entries(at as usize) {
            match e.tag {
                0x9003 => exif.date_time = tiff.ascii(&e),
                0x9011 => exif.offset = tiff.ascii(&e),
                0x9010 => plain_offset = tiff.ascii(&e),
                0x829A => exif.exposure_time = tiff.rational(&e, 0),
                0x829D => exif.f_number = tiff.real(&e, 0),
                0x8827 => exif.iso = tiff.uint(&e, 0).filter(|v| *v > 0),
                0x920A => exif.focal_length_mm = tiff.real(&e, 0),
                0xA433 => exif.lens_make = tiff.ascii(&e),
                0xA434 => exif.lens_model = tiff.ascii(&e),
                _ => {}
            }
        }
        offset = plain_offset;
    }
    if exif.date_time.is_none() {
        exif.date_time = date_time;
    }
    if exif.offset.is_none() {
        exif.offset = offset;
    }
    if let Some(at) = gps_ifd {
        let (mut lat_ref, mut lon_ref, mut alt_below) = (None, None, false);
        let (mut lat, mut lon, mut alt) = (None, None, None);
        for e in tiff.entries(at as usize) {
            match e.tag {
                1 => lat_ref = tiff.ascii(&e),
                2 => lat = tiff.degrees(&e),
                3 => lon_ref = tiff.ascii(&e),
                4 => lon = tiff.degrees(&e),
                5 => alt_below = tiff.uint(&e, 0) == Some(1),
                6 => alt = tiff.real(&e, 0),
                _ => {}
            }
        }
        if let (Some(lat), Some(lon)) = (lat, lon) {
            let south = lat_ref
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case("S"));
            let west = lon_ref
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case("W"));
            exif.latitude = Some(if south { -lat } else { lat });
            exif.longitude = Some(if west { -lon } else { lon });
            exif.altitude_m = alt.map(|a| if alt_below { -a } else { a });
        }
    }
    Some(exif)
}

struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    /// Where the value starts: inline in the entry or at its offset.
    at: usize,
}

struct Tiff<'a> {
    data: &'a [u8],
    big_endian: bool,
}

impl<'a> Tiff<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        let big_endian = match data.get(..2)? {
            b"MM" => true,
            b"II" => false,
            _ => return None,
        };
        let tiff = Self { data, big_endian };
        (tiff.u16(2)? == 42).then_some(tiff)
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at + 2)?.try_into().ok()?;
        Some(if self.big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at + 4)?.try_into().ok()?;
        Some(if self.big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }

    /// The entries of the IFD at `at` (the first one only; thumbnails' IFD1 is skipped).
    fn entries(&self, at: usize) -> Vec<Entry> {
        let Some(n) = self.u16(at) else {
            return Vec::new();
        };
        (0..n as usize)
            .map_while(|i| {
                let e = at + 2 + i * 12;
                let tag = self.u16(e)?;
                let kind = self.u16(e + 2)?;
                let count = self.u32(e + 4)?;
                let size = type_size(kind)?.checked_mul(count as usize)?;
                let at = if size <= 4 {
                    e + 8
                } else {
                    self.u32(e + 8)? as usize
                };
                Some(Entry {
                    tag,
                    kind,
                    count,
                    at,
                })
            })
            .filter(|e| e.at < self.data.len())
            .collect()
    }

    fn ascii(&self, e: &Entry) -> Option<String> {
        if e.kind != 2 && e.kind != 7 {
            return None;
        }
        let bytes = self.data.get(e.at..e.at.checked_add(e.count as usize)?)?;
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        let text = String::from_utf8_lossy(&bytes[..end]);
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn uint(&self, e: &Entry, i: usize) -> Option<u32> {
        if i as u32 >= e.count {
            return None;
        }
        match e.kind {
            1 | 7 => self.data.get(e.at + i).map(|b| u32::from(*b)),
            3 => self.u16(e.at + i * 2).map(u32::from),
            4 | 9 => self.u32(e.at + i * 4),
            _ => None,
        }
    }

    fn rational(&self, e: &Entry, i: usize) -> Option<(u32, u32)> {
        if !matches!(e.kind, 5 | 10) || i as u32 >= e.count {
            return None;
        }
        let at = e.at + i * 8;
        let (n, d) = (self.u32(at)?, self.u32(at + 4)?);
        (d != 0).then_some((n, d))
    }

    fn real(&self, e: &Entry, i: usize) -> Option<f64> {
        match e.kind {
            5 => self.rational(e, i).map(|(n, d)| n as f64 / d as f64),
            10 => self
                .rational(e, i)
                .map(|(n, d)| n as i32 as f64 / d as i32 as f64),
            _ => self.uint(e, i).map(f64::from),
        }
    }

    /// Three rationals (degrees, minutes, seconds) in decimal degrees.
    fn degrees(&self, e: &Entry) -> Option<f64> {
        let d = self.real(e, 0)?;
        let m = self.real(e, 1).unwrap_or(0.0);
        let s = self.real(e, 2).unwrap_or(0.0);
        let v = d + m / 60.0 + s / 3600.0;
        v.is_finite().then_some(v)
    }
}

fn type_size(kind: u16) -> Option<usize> {
    Some(match kind {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a little-endian or big-endian EXIF block for tests: IFD0 (make, model,
    /// pointers), the Exif IFD and the GPS IFD.
    pub(crate) fn sample_block(big_endian: bool) -> Vec<u8> {
        let mut w = Writer::new(big_endian);
        w.ifd(vec![
            (0x010F, Val::Ascii("Canon")),
            (0x0110, Val::Ascii("Canon EOS R5")),
            (0x0132, Val::Ascii("2026:08:13 09:00:00")),
            (EXIF_IFD, Val::Ifd(1)),
            (GPS_IFD, Val::Ifd(2)),
        ]);
        w.ifd(vec![
            (0x829A, Val::Rational(vec![(1, 250)])),
            (0x829D, Val::Rational(vec![(28, 10)])),
            (0x8827, Val::Short(800)),
            (0x9003, Val::Ascii("2026:08:12 19:48:07")),
            (0x9011, Val::Ascii("+02:00")),
            (0x920A, Val::Rational(vec![(35, 1)])),
            (0xA434, Val::Ascii("RF35mm F1.8 MACRO IS STM")),
        ]);
        w.ifd(vec![
            (1, Val::Ascii("N")),
            (2, Val::Rational(vec![(60, 1), (23, 1), (2760, 100)])),
            (3, Val::Ascii("W")),
            (4, Val::Rational(vec![(5, 1), (19, 1), (1920, 100)])),
            (5, Val::Byte(0)),
            (6, Val::Rational(vec![(12, 1)])),
        ]);
        w.finish()
    }

    enum Val {
        Ascii(&'static str),
        Short(u16),
        Byte(u8),
        Rational(Vec<(u32, u32)>),
        /// Offset of the IFD with this index.
        Ifd(usize),
    }

    struct Writer {
        big_endian: bool,
        ifds: Vec<Vec<(u16, Val)>>,
    }

    impl Writer {
        fn new(big_endian: bool) -> Self {
            Self {
                big_endian,
                ifds: Vec::new(),
            }
        }

        fn ifd(&mut self, entries: Vec<(u16, Val)>) {
            self.ifds.push(entries);
        }

        fn finish(self) -> Vec<u8> {
            let be = self.big_endian;
            let u16b = |v: u16| if be { v.to_be_bytes() } else { v.to_le_bytes() };
            let u32b = |v: u32| if be { v.to_be_bytes() } else { v.to_le_bytes() };
            // Layout: header, then each IFD followed by its out-of-line data.
            let payload = |v: &Val| -> Vec<u8> {
                match v {
                    Val::Ascii(s) => {
                        let mut b = s.as_bytes().to_vec();
                        b.push(0);
                        b
                    }
                    Val::Short(s) => u16b(*s).to_vec(),
                    Val::Byte(b) => vec![*b],
                    Val::Rational(r) => r
                        .iter()
                        .flat_map(|(n, d)| [u32b(*n), u32b(*d)].concat())
                        .collect(),
                    Val::Ifd(_) => vec![0; 4],
                }
            };
            let mut starts = Vec::new();
            let mut at = 8usize;
            for ifd in &self.ifds {
                starts.push(at);
                let extra: usize = ifd
                    .iter()
                    .map(|(_, v)| payload(v).len())
                    .filter(|l| *l > 4)
                    .sum();
                at += 2 + ifd.len() * 12 + 4 + extra;
            }
            let mut out = Vec::new();
            out.extend_from_slice(if be { b"MM" } else { b"II" });
            out.extend_from_slice(&u16b(42));
            out.extend_from_slice(&u32b(8));
            for (i, ifd) in self.ifds.iter().enumerate() {
                let mut data_at = starts[i] + 2 + ifd.len() * 12 + 4;
                let mut data = Vec::new();
                out.extend_from_slice(&u16b(ifd.len() as u16));
                for (tag, v) in ifd {
                    let (kind, count) = match v {
                        Val::Ascii(s) => (2u16, s.len() as u32 + 1),
                        Val::Short(_) => (3, 1),
                        Val::Byte(_) => (1, 1),
                        Val::Rational(r) => (5, r.len() as u32),
                        Val::Ifd(_) => (4, 1),
                    };
                    out.extend_from_slice(&u16b(*tag));
                    out.extend_from_slice(&u16b(kind));
                    out.extend_from_slice(&u32b(count));
                    let mut bytes = payload(v);
                    if let Val::Ifd(n) = v {
                        bytes = u32b(starts[*n] as u32).to_vec();
                    }
                    if bytes.len() <= 4 {
                        bytes.resize(4, 0);
                        out.extend_from_slice(&bytes);
                    } else {
                        out.extend_from_slice(&u32b(data_at as u32));
                        data_at += bytes.len();
                        data.extend_from_slice(&bytes);
                    }
                }
                out.extend_from_slice(&u32b(0));
                out.extend_from_slice(&data);
            }
            out
        }
    }

    fn check(exif: &Exif) {
        assert_eq!(exif.make.as_deref(), Some("Canon"));
        assert_eq!(exif.model.as_deref(), Some("Canon EOS R5"));
        assert_eq!(exif.lens_model.as_deref(), Some("RF35mm F1.8 MACRO IS STM"));
        assert_eq!(exif.date_time.as_deref(), Some("2026:08:12 19:48:07"));
        assert_eq!(exif.offset.as_deref(), Some("+02:00"));
        assert_eq!(exif.exposure_time, Some((1, 250)));
        assert_eq!(exif.f_number, Some(2.8));
        assert_eq!(exif.iso, Some(800));
        assert_eq!(exif.focal_length_mm, Some(35.0));
        assert!((exif.latitude.unwrap() - 60.391).abs() < 1e-9);
        assert!((exif.longitude.unwrap() + 5.322).abs() < 1e-9);
        assert_eq!(exif.altitude_m, Some(12.0));
    }

    #[test]
    fn parses_both_byte_orders() {
        check(&parse(&sample_block(false)).expect("little-endian"));
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend(sample_block(true));
        check(&parse(&prefixed).expect("big-endian"));
    }

    #[test]
    fn damaged_blocks_give_what_was_read() {
        assert_eq!(parse(b"not exif"), None);
        let block = sample_block(false);
        // Cut inside the Exif IFD: IFD0 still reads.
        let cut = parse(&block[..90]).expect("header");
        assert_eq!(cut.make.as_deref(), Some("Canon"));
        for len in 0..block.len() {
            let _ = parse(&block[..len]);
        }
    }

    #[test]
    fn reads_exif_from_a_jpeg() {
        let dir = std::env::temp_dir().join(format!("glass-exif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tagged.jpg");
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode(
                &[128u8; 16 * 16 * 3],
                16,
                16,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        let block = sample_block(true);
        let mut file = jpeg[..2].to_vec();
        file.extend_from_slice(&[0xFF, 0xE1]);
        file.extend_from_slice(&((block.len() + 8) as u16).to_be_bytes());
        file.extend_from_slice(b"Exif\0\0");
        file.extend_from_slice(&block);
        file.extend_from_slice(&jpeg[2..]);
        std::fs::write(&path, file).unwrap();
        let exif = read_exif(&path).expect("exif");
        check(&exif);
        std::fs::remove_dir_all(&dir).ok();
    }
}
