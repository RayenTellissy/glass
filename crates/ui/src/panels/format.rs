//! Numbers as the panels show them: sizes, durations, frame rates, time left.

use crate::t;

/// `171.3 GB`, `2.1 GB`, `840 MB`, `12 KB` (decimal units, as Finder shows them).
pub fn bytes(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        let value = format!("{:.1}", n / 1e9);
        t!("format.size.gb", value = value)
    } else if n >= 1e6 {
        let value = format!("{:.0}", n / 1e6);
        t!("format.size.mb", value = value)
    } else {
        let value = format!("{:.0}", (n / 1e3).max(1.0));
        t!("format.size.kb", value = value)
    }
}

/// Time left for a running job: `about 1 min left`, `about 20 s left`.
pub fn time_left(seconds: f64) -> String {
    if seconds >= 90.0 {
        let count = (seconds / 60.0).round() as u64;
        t!("format.time_left.minutes", count = count)
    } else if seconds >= 45.0 {
        t!("format.time_left.one_minute")
    } else {
        let count = ((seconds / 5.0).ceil() * 5.0).max(5.0) as u64;
        t!("format.time_left.seconds", count = count)
    }
}

/// Grid thumbnails: `00:31`, `18:42`, `1:04:22`.
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Short lengths in bin headers and footers: `1:48`, `0:51`.
pub fn short_clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// List view: minutes, seconds and frames (`00:12:04`), hours first past an hour.
pub fn timecode(seconds: f64, fps: Option<f64>) -> String {
    let fps = fps.filter(|f| *f > 0.0).unwrap_or(25.0);
    let nominal = fps.round().max(1.0) as u64;
    let frames = (seconds.max(0.0) * fps).round() as u64;
    let (secs, ff) = (frames / nominal, frames % nominal);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}:{ff:02}")
    } else {
        format!("{m:02}:{s:02}:{ff:02}")
    }
}

/// `23.976`, `25`, `59.94`.
pub fn fps(fps: f64) -> String {
    let rounded = (fps * 1000.0).round() / 1000.0;
    if (rounded - rounded.round()).abs() < 1e-6 {
        format!("{}", rounded.round() as u64)
    } else {
        let s = format!("{rounded:.3}");
        s.trim_end_matches('0').to_string()
    }
}

/// The resolution class the grid shows: `4K`, `HD`, `720p`, or `W×H`.
pub fn resolution_class(width: u32, height: u32) -> String {
    let (long, short) = (width.max(height), width.min(height));
    if long >= 3800 {
        t!("format.resolution.uhd")
    } else if long >= 1900 && short >= 1000 {
        t!("format.resolution.hd")
    } else if short >= 700 && long >= 1200 {
        t!("format.resolution.720p")
    } else {
        format!("{width}×{height}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(clock(31.4), "00:31");
        assert_eq!(clock(18.0 * 60.0 + 42.0), "18:42");
        assert_eq!(clock(3862.0), "1:04:22");
        assert_eq!(short_clock(108.0), "1:48");
        assert_eq!(timecode(12.0 + 4.0 / 25.0, Some(25.0)), "00:12:04");
        assert_eq!(timecode(754.0 / 23.976, Some(23.976)), "00:31:10");
        assert_eq!(fps(24000.0 / 1001.0), "23.976");
        assert_eq!(fps(25.0), "25");
        assert_eq!(fps(60000.0 / 1001.0), "59.94");
        assert_eq!(bytes(171_300_000_000), "171.3 GB");
        assert_eq!(bytes(2_100_000_000), "2.1 GB");
        assert_eq!(resolution_class(3840, 2160), "4K");
        assert_eq!(resolution_class(1920, 1080), "HD");
        assert_eq!(resolution_class(640, 360), "640×360");
        assert_eq!(time_left(60.0), "about 1 min left");
        assert_eq!(time_left(18.0), "about 20 s left");
    }
}
