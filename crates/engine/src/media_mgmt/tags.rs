//! Camera metadata from the file itself, next to the sidecars: QuickTime and MP4 container
//! tags (`com.apple.quicktime.*`, `location`, `creation_time`, the timecode track) from
//! [`media::probe`] and EXIF from stills. Also the parsers the metadata panel shares:
//! ISO 6709 positions, dates in the forms cameras write and SMPTE timecode.
//!
//! Like the sidecar readers these are forgiving: a value that does not parse is left out,
//! never an error.

use media::MediaInfo;
use media::exif::Exif;
use project::media_pool::{Gps, Metadata};

/// Container tags in the order they are trusted, for each field.
const MAKE_KEYS: &[&str] = &[
    "com.apple.quicktime.make",
    "make",
    "com.android.manufacturer",
];
const MODEL_KEYS: &[&str] = &["com.apple.quicktime.model", "model", "com.android.model"];
const LENS_KEYS: &[&str] = &["com.apple.quicktime.camera.lens_model"];
const LOCATION_KEYS: &[&str] = &[
    "com.apple.quicktime.location.ISO6709",
    "location",
    "location-eng",
];

/// Metadata from a probe's container and stream tags.
pub fn from_probe(info: &MediaInfo) -> Metadata {
    let tag = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| info.tag(k))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let date_tag = info.tag("date").and_then(parse_date);
    let created = info
        .tag("com.apple.quicktime.creationdate")
        .and_then(parse_date)
        .or(date_tag.filter(|d| d.time.is_some()))
        .or_else(|| info.creation_time.as_deref().and_then(parse_date))
        .or(date_tag);
    Metadata {
        camera_make: tag(MAKE_KEYS),
        camera_model: tag(MODEL_KEYS),
        lens: tag(LENS_KEYS),
        timecode: info.timecode.as_deref().and_then(normalise_timecode),
        gps: LOCATION_KEYS
            .iter()
            .filter_map(|k| info.tag(k))
            .find_map(parse_iso6709),
        created: created.map(|d| d.to_iso()),
        ..Metadata::default()
    }
}

/// Metadata from a still's EXIF.
pub fn from_exif(exif: &Exif) -> Metadata {
    let created = exif.date_time.as_deref().and_then(|d| {
        let with_offset = exif.offset.as_deref().map(|o| format!("{d}{o}"));
        with_offset
            .as_deref()
            .and_then(parse_date)
            .or_else(|| parse_date(d))
    });
    // The lens maker is named only when it is not the camera's and the model leaves it out
    // (`SIGMA` + `35mm F1.4 DG DN | Art 021`, but not `Canon` + `RF35mm F1.8`).
    let same_maker = |make: &str| {
        exif.make
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case(make))
    };
    let lens = match (&exif.lens_make, &exif.lens_model) {
        (Some(make), Some(model))
            if !same_maker(make)
                && !model
                    .to_ascii_lowercase()
                    .starts_with(&make.to_ascii_lowercase()) =>
        {
            Some(format!("{make} {model}"))
        }
        (_, Some(model)) => Some(model.clone()),
        _ => None,
    };
    Metadata {
        camera_make: exif.make.clone(),
        camera_model: exif.model.clone(),
        lens,
        focal_length_mm: exif.focal_length_mm.map(|f| f as f32).filter(|f| *f > 0.0),
        f_number: exif.f_number.map(|f| f as f32).filter(|f| *f > 0.0),
        iso: exif.iso,
        shutter: exif.exposure_time.and_then(shutter),
        gps: match (exif.latitude, exif.longitude) {
            (Some(latitude), Some(longitude)) if valid_position(latitude, longitude) => Some(Gps {
                latitude,
                longitude,
                altitude_m: exif.altitude_m,
            }),
            _ => None,
        },
        created: created.map(|d| d.to_iso()),
        ..Metadata::default()
    }
}

/// `1/250` for fractions of a second, `2.5` for longer exposures.
fn shutter((n, d): (u32, u32)) -> Option<String> {
    if n == 0 || d == 0 {
        return None;
    }
    Some(if n < d {
        let denominator = d as f64 / n as f64;
        format!("1/{}", denominator.round() as u64)
    } else {
        let seconds = n as f64 / d as f64;
        let s = format!("{seconds:.1}");
        s.trim_end_matches(".0").to_string()
    })
}

/// A recording date as far as the file states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreatedDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    /// Hours, minutes, seconds, as the camera's clock read them.
    pub time: Option<(u8, u8, u8)>,
    /// Offset from UTC in minutes (`Z` is 0), when the file gives one.
    pub offset_minutes: Option<i32>,
}

impl CreatedDate {
    /// ISO 8601: `2026-08-12T19:48:07+02:00`, `2026-08-12T17:48:07Z`, `2026-08-12`.
    pub fn to_iso(&self) -> String {
        let mut s = format!("{:04}-{:02}-{:02}", self.year, self.month, self.day);
        if let Some((h, m, sec)) = self.time {
            s.push_str(&format!("T{h:02}:{m:02}:{sec:02}"));
            match self.offset_minutes {
                Some(0) => s.push('Z'),
                Some(o) => {
                    let sign = if o < 0 { '-' } else { '+' };
                    let o = o.abs();
                    s.push_str(&format!("{sign}{:02}:{:02}", o / 60, o % 60));
                }
                None => {}
            }
        }
        s
    }
}

/// Parses the date forms cameras and muxers write: ISO 8601 with or without seconds,
/// fraction and offset (`2026-08-12T17:48:07.000000Z`, `2026-08-12T19:48:07+0200`),
/// EXIF's `2026:08:12 19:48:07`, a bare date, and FFmpeg's `a;b` joins of duplicate tags
/// (the first wins). Placeholder dates (all zeros, the 1904 and 1970 epochs at midnight)
/// are `None`.
pub fn parse_date(value: &str) -> Option<CreatedDate> {
    let value = value.split(';').next()?.trim();
    let b = value.as_bytes();
    let digits = |from: usize, len: usize| -> Option<u32> {
        let part = value.get(from..from + len)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    if b.len() < 10 || !matches!(b[4], b'-' | b':') || b[7] != b[4] {
        return None;
    }
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year == 0 {
        return None;
    }
    let mut date = CreatedDate {
        year: year as i32,
        month: month as u8,
        day: day as u8,
        time: None,
        offset_minutes: None,
    };
    let mut rest = &value[10..];
    if !rest.is_empty() {
        rest = rest.strip_prefix(['T', ' '])?;
        let h = digits_of(rest, 0)?;
        let m = rest
            .get(2..3)
            .filter(|c| *c == ":")
            .and(digits_of(rest, 3))?;
        let mut used = 5;
        let mut s = 0;
        if rest.get(5..6) == Some(":") {
            s = digits_of(rest, 6)?;
            used = 8;
        }
        if h > 23 || m > 59 || s > 60 {
            return None;
        }
        date.time = Some((h as u8, m as u8, s.min(59) as u8));
        rest = &rest[used..];
        if let Some(fraction) = rest.strip_prefix(['.', ',']) {
            let end = fraction
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(fraction.len());
            rest = &fraction[end..];
        }
        date.offset_minutes = parse_offset(rest.trim())?;
    }
    let epoch = matches!(date.year, 1904 | 1970)
        && date.month == 1
        && date.day == 1
        && matches!(date.time, None | Some((0, 0, 0)));
    (!epoch).then_some(date)
}

fn digits_of(s: &str, from: usize) -> Option<u32> {
    let part = s.get(from..from + 2)?;
    part.bytes()
        .all(|c| c.is_ascii_digit())
        .then(|| part.parse().ok())?
}

/// `Z`, `+02:00`, `+0200`, `-05` or nothing (`Some(None)`); `None` when malformed.
fn parse_offset(s: &str) -> Option<Option<i32>> {
    if s.is_empty() {
        return Some(None);
    }
    if s.eq_ignore_ascii_case("z") {
        return Some(Some(0));
    }
    let sign = match s.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = s[1..].replace(':', "");
    if !body.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (h, m) = match body.len() {
        2 => (body.parse::<i32>().ok()?, 0),
        4 => (
            body[..2].parse::<i32>().ok()?,
            body[2..].parse::<i32>().ok()?,
        ),
        _ => return None,
    };
    (h <= 14 && m < 60).then_some(Some(sign * (h * 60 + m)))
}

/// Parses an ISO 6709 position as QuickTime and MP4 store it: `+60.3912+005.3223+012.000/`,
/// with latitude as `±DD.D`, `±DDMM.M` or `±DDMMSS.S`, longitude as `±DDD.D`, `±DDDMM.M`
/// or `±DDDMMSS.S`, an optional altitude in metres and an optional `CRS...` suffix.
pub fn parse_iso6709(value: &str) -> Option<Gps> {
    let value = value.trim();
    let value = value.split("CRS").next()?.trim_end_matches('/');
    let mut numbers = Vec::new();
    let mut start = None;
    for (i, c) in value.char_indices() {
        if c == '+' || c == '-' {
            if let Some(s) = start {
                numbers.push(&value[s..i]);
            }
            start = Some(i);
        } else if !(c.is_ascii_digit() || c == '.') {
            return None;
        }
    }
    numbers.push(&value[start?..]);
    if numbers.len() < 2 || numbers.len() > 3 {
        return None;
    }
    let latitude = angle(numbers[0], false)?;
    let longitude = angle(numbers[1], true)?;
    let altitude_m = match numbers.get(2) {
        Some(a) => Some(a.parse::<f64>().ok().filter(|a| a.is_finite())?),
        None => None,
    };
    valid_position(latitude, longitude).then_some(Gps {
        latitude,
        longitude,
        altitude_m,
    })
}

/// One signed ISO 6709 angle in degrees, its form told by the digits before the point.
fn angle(token: &str, longitude: bool) -> Option<f64> {
    let (sign, body) = match token.as_bytes().first()? {
        b'+' => (1.0, &token[1..]),
        b'-' => (-1.0, &token[1..]),
        _ => return None,
    };
    let int_len = body.find('.').unwrap_or(body.len());
    if int_len == 0 {
        return None;
    }
    let number: f64 = body.parse().ok()?;
    let deg_digits = if longitude { 3 } else { 2 };
    let degrees = if int_len <= deg_digits {
        number
    } else if int_len == deg_digits + 2 {
        let d = (number / 100.0).trunc();
        let m = number - d * 100.0;
        if m >= 60.0 {
            return None;
        }
        d + m / 60.0
    } else if int_len == deg_digits + 4 {
        let d = (number / 10000.0).trunc();
        let m = ((number - d * 10000.0) / 100.0).trunc();
        let s = number - d * 10000.0 - m * 100.0;
        if m >= 60.0 || s >= 60.0 {
            return None;
        }
        d + m / 60.0 + s / 3600.0
    } else {
        return None;
    };
    Some(sign * degrees)
}

fn valid_position(latitude: f64, longitude: f64) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && latitude.abs() <= 90.0
        && longitude.abs() <= 180.0
        // Cameras without a fix write zeros.
        && (latitude != 0.0 || longitude != 0.0)
}

/// SMPTE timecode as `HH:MM:SS:FF`, with `;` before the frames when drop-frame (`.` and
/// `,` are other drop-frame spellings). `None` when it is not a timecode.
pub fn normalise_timecode(value: &str) -> Option<String> {
    let value = value.trim();
    let b = value.as_bytes();
    if b.len() != 11 || b[2] != b':' || b[5] != b':' {
        return None;
    }
    let drop = match b[8] {
        b':' => false,
        b';' | b'.' | b',' => true,
        _ => return None,
    };
    let (h, m, s, f) = (
        digits_of(value, 0)?,
        digits_of(value, 3)?,
        digits_of(value, 6)?,
        digits_of(value, 9)?,
    );
    if h > 23 || m > 59 || s > 59 {
        return None;
    }
    Some(format!(
        "{h:02}:{m:02}:{s:02}{}{f:02}",
        if drop { ';' } else { ':' }
    ))
}

/// The metadata of an imported clip: sidecars first (the camera's own account), then the
/// container's tags, then EXIF; dates and timecode brought to one spelling.
pub fn merge(
    sidecars: Metadata,
    container: Option<&Metadata>,
    exif: Option<&Metadata>,
) -> Metadata {
    let mut m = sidecars;
    if let Some(c) = container {
        m.fill_from(c);
    }
    if let Some(e) = exif {
        m.fill_from(e);
    }
    if let Some(created) = m.created.as_deref().and_then(parse_date) {
        m.created = Some(created.to_iso());
    }
    if let Some(tc) = m.timecode.as_deref().and_then(normalise_timecode) {
        m.timecode = Some(tc);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gps(value: &str) -> (f64, f64, Option<f64>) {
        let g = parse_iso6709(value).unwrap_or_else(|| panic!("{value} should parse"));
        (g.latitude, g.longitude, g.altitude_m)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn iso6709_forms() {
        assert_eq!(
            gps("+60.3912+005.3223+012.000/"),
            (60.3912, 5.3223, Some(12.0))
        );
        assert_eq!(gps("+35.6586+139.7454/"), (35.6586, 139.7454, None));
        assert_eq!(
            gps("-33.8568-070.6483-004.5/"),
            (-33.8568, -70.6483, Some(-4.5))
        );
        assert_eq!(
            gps("+40.20361-075.00417CRSWGS_84/"),
            (40.20361, -75.00417, None)
        );
        // Degrees and minutes, then degrees, minutes and seconds.
        let (lat, lon, _) = gps("+6023.472+00519.338/");
        assert!(close(lat, 60.0 + 23.472 / 60.0) && close(lon, 5.0 + 19.338 / 60.0));
        let (lat, lon, _) = gps("+602327.6+0051919.2/");
        assert!(close(lat, 60.391) && close(lon, 5.322), "{lat} {lon}");
        // Without zero padding (some Android phones).
        assert_eq!(gps("+60.39+5.32/"), (60.39, 5.32, None));
    }

    #[test]
    fn iso6709_rejects() {
        for bad in [
            "",
            "/",
            "+60.39/",
            "60.39+5.32/",
            "+91.0+005.0/",
            "+60.0+181.0/",
            "+6075.0+00519.0/",
            "+0.0+0.0/",
            "+60.39+5.32+1+2/",
            "+60.39 +5.32/",
            "+60.3x+5.32/",
        ] {
            assert_eq!(parse_iso6709(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn dates() {
        let iso = |v: &str| parse_date(v).map(|d| d.to_iso());
        assert_eq!(
            iso("2026-08-12T17:48:07.000000Z").as_deref(),
            Some("2026-08-12T17:48:07Z")
        );
        assert_eq!(
            iso("2026-08-12T19:48:07+0200").as_deref(),
            Some("2026-08-12T19:48:07+02:00")
        );
        assert_eq!(
            iso("2026:08:12 19:48:07").as_deref(),
            Some("2026-08-12T19:48:07")
        );
        assert_eq!(
            iso("2026:08:12 19:48:07-05:30").as_deref(),
            Some("2026-08-12T19:48:07-05:30")
        );
        assert_eq!(
            iso("2026-08-12T19:48").as_deref(),
            Some("2026-08-12T19:48:00")
        );
        assert_eq!(iso("2026-08-12").as_deref(), Some("2026-08-12"));
        assert_eq!(
            iso("2026-08-12T17:48:07.000000Z;2026-08-12T17:48:07.000000Z").as_deref(),
            Some("2026-08-12T17:48:07Z")
        );
        let d = parse_date("2026-08-12T19:48:07+02:00").unwrap();
        assert_eq!((d.year, d.month, d.day), (2026, 8, 12));
        assert_eq!((d.time, d.offset_minutes), (Some((19, 48, 7)), Some(120)));
        for bad in [
            "",
            "yesterday",
            "0000:00:00 00:00:00",
            "1904-01-01T00:00:00.000000Z",
            "1970-01-01T00:00:00Z",
            "2026-13-01",
            "2026-08-12T25:00:00",
            "2026-08-12T19:48:07+2",
            "2026-08-12X19:48",
            "2026-08:12",
        ] {
            assert_eq!(parse_date(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn timecodes() {
        assert_eq!(
            normalise_timecode("19:48:07:11").as_deref(),
            Some("19:48:07:11")
        );
        assert_eq!(
            normalise_timecode("10:00:00;02").as_deref(),
            Some("10:00:00;02")
        );
        assert_eq!(
            normalise_timecode(" 10:00:00.02 ").as_deref(),
            Some("10:00:00;02")
        );
        for bad in [
            "",
            "1:00:00:00",
            "24:00:00:00",
            "10:60:00:00",
            "10:00:00/00",
            "aa:bb:cc:dd",
        ] {
            assert_eq!(normalise_timecode(bad), None, "{bad:?}");
        }
    }

    fn info(tags: &[(&str, &str)], timecode: Option<&str>, creation: Option<&str>) -> MediaInfo {
        MediaInfo {
            path: "clip.mov".into(),
            size_bytes: None,
            container: "mov,mp4,m4a,3gp,3g2,mj2".into(),
            container_long_name: String::new(),
            duration: None,
            start_time: None,
            bit_rate: None,
            creation_time: creation.map(str::to_string),
            timecode: timecode.map(str::to_string),
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            streams: Vec::new(),
        }
    }

    #[test]
    fn quicktime_tags() {
        let m = from_probe(&info(
            &[
                ("com.apple.quicktime.make", "Apple"),
                ("com.apple.quicktime.model", "iPhone 15 Pro"),
                (
                    "com.apple.quicktime.camera.lens_model",
                    "iPhone 15 Pro back camera 6.765mm f/1.78",
                ),
                (
                    "com.apple.quicktime.location.ISO6709",
                    "+60.3912+005.3223+012.000/",
                ),
                (
                    "com.apple.quicktime.creationdate",
                    "2026-08-12T19:48:07+0200",
                ),
            ],
            Some("19:48:07:11"),
            Some("2026-08-12T17:48:07.000000Z"),
        ));
        assert_eq!(m.camera_make.as_deref(), Some("Apple"));
        assert_eq!(m.camera_model.as_deref(), Some("iPhone 15 Pro"));
        assert_eq!(
            m.lens.as_deref(),
            Some("iPhone 15 Pro back camera 6.765mm f/1.78")
        );
        assert_eq!(m.timecode.as_deref(), Some("19:48:07:11"));
        // The local time with its offset beats the UTC creation_time.
        assert_eq!(m.created.as_deref(), Some("2026-08-12T19:48:07+02:00"));
        assert_eq!(
            m.gps,
            Some(Gps {
                latitude: 60.3912,
                longitude: 5.3223,
                altitude_m: Some(12.0)
            })
        );
    }

    #[test]
    fn mp4_tags() {
        let m = from_probe(&info(
            &[("location", "+35.6586+139.7454/"), ("date", "2026-08-12")],
            None,
            Some("2026-08-12T10:00:00.000000Z"),
        ));
        assert_eq!(m.gps.map(|g| g.longitude), Some(139.7454));
        assert_eq!(m.created.as_deref(), Some("2026-08-12T10:00:00Z"));
        assert_eq!(m.camera_make, None);
        // A bare date is used when nothing better is there; epoch placeholders are not.
        let m = from_probe(&info(
            &[("date", "2026-08-12")],
            None,
            Some("1970-01-01T00:00:00.000000Z"),
        ));
        assert_eq!(m.created.as_deref(), Some("2026-08-12"));
        assert!(from_probe(&info(&[], None, None)).is_empty());
    }

    #[test]
    fn exif_mapping() {
        let exif = Exif {
            make: Some("Canon".into()),
            model: Some("Canon EOS R5".into()),
            lens_make: Some("Canon".into()),
            lens_model: Some("RF35mm F1.8 MACRO IS STM".into()),
            date_time: Some("2026:08:12 19:48:07".into()),
            offset: Some("+02:00".into()),
            focal_length_mm: Some(35.0),
            f_number: Some(1.8),
            iso: Some(400),
            exposure_time: Some((10, 2500)),
            latitude: Some(60.391),
            longitude: Some(-5.322),
            altitude_m: None,
        };
        let m = from_exif(&exif);
        assert_eq!(m.lens.as_deref(), Some("RF35mm F1.8 MACRO IS STM"));
        assert_eq!(m.created.as_deref(), Some("2026-08-12T19:48:07+02:00"));
        assert_eq!(m.shutter.as_deref(), Some("1/250"));
        assert_eq!(m.f_number, Some(1.8));
        assert_eq!(m.gps.map(|g| g.longitude), Some(-5.322));
        assert_eq!(shutter((5, 2)).as_deref(), Some("2.5"));
        assert_eq!(shutter((2, 1)).as_deref(), Some("2"));
        let other_lens = Exif {
            lens_make: Some("SIGMA".into()),
            lens_model: Some("35mm F1.4 DG DN | Art 021".into()),
            latitude: Some(0.0),
            longitude: Some(0.0),
            ..Exif::default()
        };
        let m = from_exif(&other_lens);
        assert_eq!(m.lens.as_deref(), Some("SIGMA 35mm F1.4 DG DN | Art 021"));
        assert_eq!(m.gps, None);
    }

    #[test]
    fn sidecars_win_and_spellings_are_unified() {
        let sidecar = Metadata {
            camera_model: Some("ILME-FX6V".into()),
            created: Some("2026:08:12 19:48:07".into()),
            ..Metadata::default()
        };
        let container = Metadata {
            camera_make: Some("Sony".into()),
            camera_model: Some("FX6".into()),
            timecode: Some("19:48:07.11".into()),
            ..Metadata::default()
        };
        let m = merge(sidecar, Some(&container), None);
        assert_eq!(m.camera_make.as_deref(), Some("Sony"));
        assert_eq!(m.camera_model.as_deref(), Some("ILME-FX6V"));
        assert_eq!(m.created.as_deref(), Some("2026-08-12T19:48:07"));
        assert_eq!(m.timecode.as_deref(), Some("19:48:07;11"));
    }
}
