//! Camera sidecar files read into [`Metadata`]: Sony NonRealTimeMeta XML (XAVC and
//! XDCAM), Canon Cinema clip XML and DJI telemetry subtitles. The readers are forgiving:
//! a field that is missing or malformed is left empty, never an error.

use project::media_pool::{Gps, Metadata};

use super::cards::{Sidecar, SidecarKind};

/// Sidecars are small (a few KB of XML, or a few MB of SRT for a long flight); anything
/// bigger is not what it claims to be.
const MAX_SIDECAR_BYTES: u64 = 64 * 1024 * 1024;

/// Reads a sidecar file. `None` when it cannot be read.
pub fn read(sidecar: &Sidecar) -> Option<Metadata> {
    let len = std::fs::metadata(&sidecar.path).ok()?.len();
    if len > MAX_SIDECAR_BYTES {
        return None;
    }
    let bytes = std::fs::read(&sidecar.path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(match sidecar.kind {
        SidecarKind::SonyXml => sony_xml(&text),
        SidecarKind::CanonXml => canon_xml(&text),
        SidecarKind::DjiSrt => dji_srt(&text),
    })
}

/// Reads every sidecar of a clip, earlier ones winning.
pub fn read_all(sidecars: &[Sidecar]) -> Metadata {
    let mut metadata = Metadata::default();
    for sidecar in sidecars {
        if let Some(m) = read(sidecar) {
            metadata.fill_from(&m);
        }
    }
    metadata
}

/// One XML element's attributes, from a light scan that is enough for camera sidecars
/// (no entities beyond the five predefined ones, no namespaces needed).
struct Element<'a> {
    name: &'a str,
    attrs: Vec<(&'a str, String)>,
}

impl Element<'_> {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn elements(xml: &str) -> impl Iterator<Item = Element<'_>> {
    xml.split('<').skip(1).filter_map(|chunk| {
        let tag = chunk.split('>').next()?;
        if tag.starts_with(['/', '?', '!']) {
            return None;
        }
        let tag = tag.trim_end_matches('/');
        let name_end = tag.find(|c: char| c.is_whitespace()).unwrap_or(tag.len());
        let name = &tag[..name_end];
        let name = name.rsplit(':').next().unwrap_or(name);
        let mut attrs = Vec::new();
        let mut rest = &tag[name_end..];
        while let Some(eq) = rest.find('=') {
            let key = rest[..eq].trim();
            let after = rest[eq + 1..].trim_start();
            let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                break;
            };
            let Some(close) = after[1..].find(quote) else {
                break;
            };
            attrs.push((key, unescape(&after[1..1 + close])));
            rest = &after[close + 2..];
        }
        Some(Element { name, attrs })
    })
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// The leading number in `value` (`35.00mm` → 35.0, `F2.8` → 2.8).
fn leading_number(value: &str) -> Option<f64> {
    let start = value.find(|c: char| c.is_ascii_digit())?;
    let rest = &value[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn bcd(byte: u8) -> u8 {
    (byte >> 4) * 10 + (byte & 0x0F)
}

/// Sony's LTC value: four BCD bytes, frames first (`11074819` is 19:48:07:11). Bit 6
/// of the frames byte marks drop-frame.
fn sony_ltc(value: &str) -> Option<String> {
    if value.len() != 8 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).ok();
    let (ff, ss, mm, hh) = (byte(0)?, byte(1)?, byte(2)?, byte(3)?);
    let drop = ff & 0x40 != 0;
    Some(format!(
        "{:02}:{:02}:{:02}{}{:02}",
        bcd(hh & 0x3F),
        bcd(mm & 0x7F),
        bcd(ss & 0x7F),
        if drop { ';' } else { ':' },
        bcd(ff & 0x3F)
    ))
}

/// `60:23:28.000` (degrees, minutes, seconds) or `60.3911` in degrees.
fn dms(value: &str) -> Option<f64> {
    let parts: Vec<f64> = value
        .split([':', ',', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    match parts.as_slice() {
        [d] => Some(*d),
        [d, m] => Some(d + m / 60.0),
        [d, m, s] => Some(d + m / 60.0 + s / 3600.0),
        _ => None,
    }
}

/// Sony NonRealTimeMeta (XAVC `C0001M01.XML`, XDCAM `C0001M01.XML`).
pub fn sony_xml(xml: &str) -> Metadata {
    let mut m = Metadata::default();
    let (mut lat, mut lat_ref, mut lon, mut lon_ref, mut alt) = (None, None, None, None, None);
    let mut first_ltc = true;
    for e in elements(xml) {
        match e.name {
            "Device" => {
                m.camera_make = non_empty(e.attr("manufacturer"));
                m.camera_model = non_empty(e.attr("modelName"));
            }
            "Lens" => m.lens = m.lens.take().or_else(|| non_empty(e.attr("modelName"))),
            "CreationDate" => m.created = non_empty(e.attr("value")),
            "LtcChange" if first_ltc => {
                first_ltc = false;
                m.timecode = e.attr("value").and_then(sony_ltc);
            }
            "Item" => {
                let value = e.attr("value").unwrap_or_default();
                match e.attr("name").unwrap_or_default() {
                    "ISOSensitivity" => m.iso = leading_number(value).map(|v| v as u32),
                    "IrisFNumber" => m.f_number = leading_number(value).map(|v| v as f32),
                    "LensZoomActualFocalLength" | "FocalLength" => {
                        m.focal_length_mm = leading_number(value).map(|v| v as f32)
                    }
                    "ShutterSpeedTime" | "ExposureTime" | "ShutterSpeedAngle" => {
                        m.shutter = m.shutter.take().or_else(|| non_empty(Some(value)))
                    }
                    "LensModelName" => m.lens = m.lens.take().or_else(|| non_empty(Some(value))),
                    "Latitude" => lat = dms(value),
                    "LatitudeRef" => lat_ref = Some(value.to_ascii_uppercase()),
                    "Longitude" => lon = dms(value),
                    "LongitudeRef" => lon_ref = Some(value.to_ascii_uppercase()),
                    "Altitude" => alt = leading_number(value),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if let (Some(mut latitude), Some(mut longitude)) = (lat, lon) {
        if lat_ref.as_deref() == Some("S") {
            latitude = -latitude;
        }
        if lon_ref.as_deref() == Some("W") {
            longitude = -longitude;
        }
        m.gps = Some(Gps {
            latitude,
            longitude,
            altitude_m: alt,
        });
    }
    m
}

/// Canon Cinema clip XML: camera model, lens and reel from the `Camera`, `Lens` and
/// `Reel`-like elements and attributes Canon writes.
pub fn canon_xml(xml: &str) -> Metadata {
    let mut m = Metadata::default();
    for e in elements(xml) {
        let value = || non_empty(e.attr("value")).or_else(|| non_empty(e.attr("name")));
        match e.name {
            "Camera" | "Device" => {
                m.camera_make = non_empty(e.attr("manufacturer")).or(Some("Canon".into()));
                m.camera_model =
                    non_empty(e.attr("modelName")).or_else(|| non_empty(e.attr("model")));
            }
            "Lens" | "LensModel" => m.lens = non_empty(e.attr("modelName")).or_else(value),
            "ReelName" | "Reel" => m.reel = value(),
            "CreationDate" | "StartDate" => m.created = value(),
            "StartTimecode" | "Timecode" => m.timecode = value(),
            "ISO" | "ISOSensitivity" => {
                m.iso = e.attr("value").and_then(leading_number).map(|v| v as u32)
            }
            _ => {}
        }
    }
    m
}

/// DJI telemetry subtitles: the first cue that has a position, in either the bracketed
/// format (`[latitude: 60.391] [longitude: 5.322] [rel_alt: 30.1 abs_alt: 110.3]`) or
/// the older `GPS(lon,lat,alt)` one, plus exposure settings and the date line.
pub fn dji_srt(srt: &str) -> Metadata {
    let mut m = Metadata {
        camera_make: Some("DJI".into()),
        ..Metadata::default()
    };
    let field = |line: &str, key: &str| -> Option<String> {
        let at = line.find(key)?;
        let rest = line[at + key.len()..].trim_start();
        let rest = rest.strip_prefix(':').unwrap_or(rest).trim_start();
        let end = rest.find([']', ' ', ',']).unwrap_or(rest.len());
        non_empty(Some(&rest[..end]))
    };
    for line in srt.lines().map(str::trim) {
        if m.created.is_none()
            && line.len() >= 19
            && line.as_bytes()[4] == b'-'
            && line.as_bytes()[10] == b' '
            && line[..4].bytes().all(|b| b.is_ascii_digit())
        {
            m.created = Some(line[..19].replace(' ', "T"));
        }
        if m.iso.is_none() {
            m.iso = field(line, "iso")
                .or_else(|| field(line, "ISO"))
                .and_then(|v| leading_number(&v))
                .map(|v| v as u32);
        }
        if m.shutter.is_none() {
            m.shutter = field(line, "shutter").or_else(|| field(line, "Shutter"));
        }
        if m.f_number.is_none() {
            m.f_number = field(line, "fnum")
                .or_else(|| field(line, "Fnum"))
                .and_then(|v| leading_number(&v))
                .map(|v| v as f32);
        }
        if m.focal_length_mm.is_none() {
            m.focal_length_mm = field(line, "focal_len")
                .and_then(|v| leading_number(&v))
                .map(|v| v as f32);
        }
        if m.gps.is_none() {
            let lat = field(line, "[latitude").and_then(|v| v.parse::<f64>().ok());
            let lon = field(line, "[longitude").and_then(|v| v.parse::<f64>().ok());
            let alt = field(line, "abs_alt").and_then(|v| v.parse::<f64>().ok());
            if let (Some(latitude), Some(longitude)) = (lat, lon)
                && (latitude != 0.0 || longitude != 0.0)
            {
                m.gps = Some(Gps {
                    latitude,
                    longitude,
                    altitude_m: alt,
                });
            } else if let Some(start) = line.find("GPS(") {
                let inner = &line[start + 4..];
                let inner = &inner[..inner.find(')').unwrap_or(inner.len())];
                let v: Vec<f64> = inner
                    .split(',')
                    .filter_map(|p| p.trim().parse().ok())
                    .collect();
                if v.len() >= 2 && (v[0] != 0.0 || v[1] != 0.0) {
                    m.gps = Some(Gps {
                        latitude: v[1],
                        longitude: v[0],
                        altitude_m: v.get(2).copied(),
                    });
                }
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    const SONY_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<NonRealTimeMeta xmlns="urn:schemas-professionalDisc:nonRealTimeMeta:ver.2.20" lastUpdate="2026-08-12T19:48:40+02:00">
	<Duration value="752"/>
	<LtcChangeTable tcFps="25" halfStep="false">
		<LtcChange frameCount="0" value="11074819" status="increment"/>
		<LtcChange frameCount="751" value="12374819" status="end"/>
	</LtcChangeTable>
	<CreationDate value="2026-08-12T19:48:07+02:00"/>
	<Device manufacturer="Sony" modelName="ILME-FX6V" serialNo="4012345"/>
	<Lens modelName="FE 35mm F1.4 GM"/>
	<AcquisitionRecord>
		<Group name="CameraUnitMetadataSet">
			<Item name="IrisFNumber" value="2.8"/>
			<Item name="ISOSensitivity" value="800"/>
			<Item name="ShutterSpeedTime" value="1/50"/>
			<Item name="LensZoomActualFocalLength" value="35.00mm"/>
		</Group>
		<Group name="ExifGPS">
			<Item name="LatitudeRef" value="N"/>
			<Item name="Latitude" value="60:23:27.600"/>
			<Item name="LongitudeRef" value="E"/>
			<Item name="Longitude" value="5:19:19.200"/>
			<Item name="Altitude" value="12"/>
		</Group>
	</AcquisitionRecord>
</NonRealTimeMeta>
"#;

    #[test]
    fn sony_xml_fields() {
        let m = sony_xml(SONY_XML);
        assert_eq!(m.camera_make.as_deref(), Some("Sony"));
        assert_eq!(m.camera_model.as_deref(), Some("ILME-FX6V"));
        assert_eq!(m.lens.as_deref(), Some("FE 35mm F1.4 GM"));
        assert_eq!(m.timecode.as_deref(), Some("19:48:07:11"));
        assert_eq!(m.created.as_deref(), Some("2026-08-12T19:48:07+02:00"));
        assert_eq!(m.iso, Some(800));
        assert_eq!(m.f_number, Some(2.8));
        assert_eq!(m.focal_length_mm, Some(35.0));
        assert_eq!(m.shutter.as_deref(), Some("1/50"));
        let gps = m.gps.expect("gps");
        assert!((gps.latitude - 60.391).abs() < 1e-6, "{}", gps.latitude);
        assert!((gps.longitude - 5.322).abs() < 1e-6, "{}", gps.longitude);
        assert_eq!(gps.altitude_m, Some(12.0));
    }

    #[test]
    fn sony_ltc_drop_frame() {
        assert_eq!(sony_ltc("41000010").as_deref(), Some("10:00:00;01"));
        assert_eq!(sony_ltc("nonsense"), None);
    }

    const DJI_SRT: &str = "1
00:00:00,000 --> 00:00:00,033
<font size=\"28\">FrameCnt: 1, DiffTime: 33ms
2026-08-12 19:48:07.123
[iso: 100] [shutter: 1/1000.0] [fnum: 2.8] [ev: 0] [ct: 5500] [color_md: default] [focal_len: 24.00] [latitude: 60.391234] [longitude: 5.322345] [rel_alt: 30.100 abs_alt: 110.300] </font>

2
00:00:00,033 --> 00:00:00,066
<font size=\"28\">FrameCnt: 2, DiffTime: 33ms
2026-08-12 19:48:07.156
[iso: 100] [shutter: 1/1000.0] [fnum: 2.8] [ev: 0] [latitude: 60.391240] [longitude: 5.322350] [rel_alt: 30.200 abs_alt: 110.400] </font>
";

    #[test]
    fn dji_srt_fields() {
        let m = dji_srt(DJI_SRT);
        assert_eq!(m.camera_make.as_deref(), Some("DJI"));
        assert_eq!(m.created.as_deref(), Some("2026-08-12T19:48:07"));
        assert_eq!(m.iso, Some(100));
        assert_eq!(m.shutter.as_deref(), Some("1/1000.0"));
        assert_eq!(m.f_number, Some(2.8));
        assert_eq!(m.focal_length_mm, Some(24.0));
        assert_eq!(
            m.gps,
            Some(Gps {
                latitude: 60.391234,
                longitude: 5.322345,
                altitude_m: Some(110.3)
            })
        );
    }

    #[test]
    fn dji_srt_old_format() {
        let m = dji_srt(
            "1\n00:00:00,000 --> 00:00:01,000\nHOME(5.3200,60.3900) 2026.08.12 19:48:07\nGPS(5.3223,60.3912,110) BAROMETER:30.1\nISO:100 Shutter:1000 EV:0 Fnum:F2.8\n",
        );
        let gps = m.gps.expect("gps");
        assert_eq!((gps.latitude, gps.longitude), (60.3912, 5.3223));
        assert_eq!(m.iso, Some(100));
        assert_eq!(m.f_number, Some(2.8));
    }

    #[test]
    fn canon_xml_fields() {
        let m = canon_xml(
            r#"<ClipMetadata><Camera modelName="EOS C70"/><Lens modelName="RF24-70mm F2.8 L IS USM"/><ReelName value="A001"/></ClipMetadata>"#,
        );
        assert_eq!(m.camera_make.as_deref(), Some("Canon"));
        assert_eq!(m.camera_model.as_deref(), Some("EOS C70"));
        assert_eq!(m.lens.as_deref(), Some("RF24-70mm F2.8 L IS USM"));
        assert_eq!(m.reel.as_deref(), Some("A001"));
    }
}
