//! Sidecar subtitle files: a subtitle track's cues as SubRip (`.srt`) or WebVTT
//! (`.vtt`) text.
//!
//! [`track_cues`] turns a track's subtitle items into [`Cue`]s in whole milliseconds,
//! relative to the start of the export range: cues outside the range are left out, cues
//! crossing its edges are cut at them. Exact times become milliseconds by rounding to
//! the nearest, halves away from zero; rounding is monotonic, so cues that did not
//! overlap still do not, and a cue that rounds to nothing (shorter than a millisecond at
//! a frame rate above 1000 fps) is left out rather than ending at or before its start.
//! Text lines are separated by `\n` (`\r\n` and `\r` become `\n`); blank lines, which
//! would end the cue in both formats, are dropped, and so is a cue with no text left.
//!
//! [`to_srt`] and [`to_vtt`] write UTF-8 without a byte order mark and with `\n` line
//! ends. SubRip numbers its cues from 1 and carries the text as it is; WebVTT escapes
//! `&`, `<` and `>` (which also keeps `-->` out of a cue's text). The model has no cue
//! position or style yet (Phase 10), so no WebVTT cue settings are written.

use std::fmt::Write as _;

use crate::model::{ItemKind, Track};
use crate::time::{Time, TimeError, TimeRange};

/// One cue in whole milliseconds from the start of the export range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: i64,
    /// After `start_ms`.
    pub end_ms: i64,
    /// One or more non-empty lines separated by `\n`.
    pub text: String,
}

/// The subtitle items of `track` inside `range` (the whole track from zero when `None`),
/// in time order. Items of other kinds are skipped, so any track may be passed.
pub fn track_cues(track: &Track, range: Option<TimeRange>) -> Result<Vec<Cue>, TimeError> {
    let (from, to) = match range {
        Some(r) => (r.start, Some(r.end()?)),
        None => (Time::ZERO, None),
    };
    let ms = Time::new(1, 1000)?;
    let mut cues: Vec<Cue> = Vec::new();
    for item in &track.items {
        let ItemKind::Subtitle(subtitle) = &item.kind else {
            continue;
        };
        let start = item.range.start.max(from);
        let end = match to {
            Some(to) => item.range.end()?.min(to),
            None => item.range.end()?,
        };
        if end <= start {
            continue;
        }
        let Some(text) = cue_text(&subtitle.text) else {
            continue;
        };
        let start_ms = start.checked_sub(from)?.round_to_multiple(ms)?;
        let end_ms = end.checked_sub(from)?.round_to_multiple(ms)?;
        if end_ms <= start_ms {
            continue;
        }
        // Items never overlap and rounding is monotonic, so this only guards the rule.
        if let Some(last) = cues.last_mut()
            && last.end_ms > start_ms
        {
            last.end_ms = start_ms;
        }
        cues.push(Cue {
            start_ms,
            end_ms,
            text,
        });
    }
    cues.retain(|c| c.end_ms > c.start_ms);
    Ok(cues)
}

/// The text's non-blank lines joined by `\n`, or `None` when nothing is left.
fn cue_text(text: &str) -> Option<String> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text
        .split('\n')
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// `HH:MM:SS,mmm` (SubRip).
pub fn srt_timestamp(ms: i64) -> String {
    timestamp(ms, ',')
}

/// `HH:MM:SS.mmm` (WebVTT).
pub fn vtt_timestamp(ms: i64) -> String {
    timestamp(ms, '.')
}

/// Hours take two digits or more; negative times are written as zero.
fn timestamp(ms: i64, separator: char) -> String {
    let ms = ms.max(0);
    let (h, rest) = (ms / 3_600_000, ms % 3_600_000);
    let (m, rest) = (rest / 60_000, rest % 60_000);
    let (s, ms) = (rest / 1000, rest % 1000);
    format!("{h:02}:{m:02}:{s:02}{separator}{ms:03}")
}

/// A SubRip file: numbered cues separated by blank lines.
pub fn to_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = writeln!(
            out,
            "{}\n{} --> {}\n{}",
            i + 1,
            srt_timestamp(cue.start_ms),
            srt_timestamp(cue.end_ms),
            cue.text
        );
    }
    out
}

/// A WebVTT file: the `WEBVTT` header, then cues separated by blank lines.
pub fn to_vtt(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n");
    for cue in cues {
        let _ = writeln!(
            out,
            "\n{} --> {}\n{}",
            vtt_timestamp(cue.start_ms),
            vtt_timestamp(cue.end_ms),
            vtt_escape(&cue.text)
        );
    }
    out
}

/// `&`, `<` and `>` as character references.
pub fn vtt_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::id::{ItemId, MediaId, TrackId};
    use crate::model::{Clip, Item, Subtitle, TrackKind};

    fn t(s: &str) -> Time {
        s.parse().expect("time")
    }

    fn track(items: &[(&str, &str, &str)]) -> Track {
        Track {
            items: items
                .iter()
                .enumerate()
                .map(|(i, (start, duration, text))| {
                    Arc::new(Item {
                        id: ItemId(i as u64 + 10),
                        range: TimeRange::new(t(start), t(duration)),
                        kind: ItemKind::Subtitle(Subtitle {
                            text: (*text).to_string(),
                        }),
                    })
                })
                .collect(),
            ..Track::new(TrackId(1), TrackKind::Subtitle, "Subtitles")
        }
    }

    fn golden(name: &str) -> String {
        let path = format!(
            "{}/../../tests/fixtures/subtitles/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// Cues at 29.97 fps (frame 1001/30000 s), with a two-line cue, markup characters,
    /// CRLF and blank lines, a gap, and a cue past an hour.
    fn sample() -> Track {
        track(&[
            ("1001/30000", "2002/1000", "Hello"),
            ("3003/1000", "1", "Two\r\nlines"),
            ("5", "3/2", "Fish & chips <b>not bold</b> -->"),
            ("7", "1", "\n  \nBlank lines\n\n dropped\n"),
            ("3723/1", "1/2", "Past an hour"),
        ])
    }

    #[test]
    fn timestamps_have_two_digit_hours_and_milliseconds() {
        assert_eq!(srt_timestamp(0), "00:00:00,000");
        assert_eq!(vtt_timestamp(3_723_004), "01:02:03.004");
        assert_eq!(srt_timestamp(100 * 3_600_000 + 59_999), "100:00:59,999");
        assert_eq!(vtt_timestamp(-5), "00:00:00.000");
    }

    #[test]
    fn rational_times_round_to_the_nearest_millisecond() {
        let cues = track_cues(&sample(), None).expect("cues");
        // 1001/30000 s = 33.3667 ms; 2002/1000 + 1001/30000 = 2035.367 ms.
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (33, 2035));
        // Exactly 3003 ms to 4003 ms.
        assert_eq!((cues[1].start_ms, cues[1].end_ms), (3003, 4003));
        // Halves round away from zero: 1/2000 s = 0.5 ms.
        let half = track(&[("1/2000", "1/1000", "a"), ("3/2000", "1/1000", "b")]);
        let cues = track_cues(&half, None).expect("cues");
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (1, 2));
        assert_eq!((cues[1].start_ms, cues[1].end_ms), (2, 3));
    }

    #[test]
    fn rounding_never_overlaps_or_inverts_cues() {
        // Back-to-back cues at 30000/1001 fps for a minute: each ends where the next
        // starts, before and after rounding.
        let frame = t("1001/30000");
        let mut items = Vec::new();
        for i in 0..1800 {
            let start = frame.mul_ratio(i, 1).expect("start").to_string();
            items.push((start, frame.to_string(), format!("{i}")));
        }
        let items: Vec<(&str, &str, &str)> = items
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
            .collect();
        let cues = track_cues(&track(&items), None).expect("cues");
        assert_eq!(cues.len(), 1800);
        for pair in cues.windows(2) {
            assert!(pair[0].start_ms < pair[0].end_ms);
            assert_eq!(pair[0].end_ms, pair[1].start_ms);
        }
        // Shorter than a millisecond: left out.
        let tiny = track(&[("0", "1/4000", "gone"), ("1", "1", "kept")]);
        let cues = track_cues(&tiny, None).expect("cues");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "kept");
    }

    #[test]
    fn the_export_range_shifts_and_cuts_cues() {
        // Range 3 s to 6 s: the first cue is out, the second starts at the range start,
        // the third is cut at its end, the rest are out.
        let range = TimeRange::new(t("3"), t("3"));
        let cues = track_cues(&sample(), Some(range)).expect("cues");
        let spans: Vec<(i64, i64)> = cues.iter().map(|c| (c.start_ms, c.end_ms)).collect();
        assert_eq!(spans, [(3, 1003), (2000, 3000)]);
        // A range starting inside a cue cuts its start.
        let range = TimeRange::new(t("1"), t("10"));
        let cues = track_cues(&sample(), Some(range)).expect("cues");
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (0, 1035));
        assert_eq!(cues[0].text, "Hello");
        // Non-subtitle items are skipped.
        let mut other = sample();
        other.items.push_back(Arc::new(Item {
            id: ItemId(99),
            range: TimeRange::new(t("9"), t("1")),
            kind: ItemKind::Clip(Clip::new(MediaId(1), Time::ZERO)),
        }));
        assert_eq!(track_cues(&other, None).expect("cues").len(), 5);
    }

    #[test]
    fn srt_matches_its_golden() {
        let cues = track_cues(&sample(), None).expect("cues");
        let srt = to_srt(&cues);
        assert!(!srt.starts_with('\u{feff}'));
        assert_eq!(srt, golden("sample.srt"));
    }

    #[test]
    fn vtt_matches_its_golden() {
        let cues = track_cues(&sample(), None).expect("cues");
        let vtt = to_vtt(&cues);
        assert!(!vtt.starts_with('\u{feff}'));
        assert_eq!(vtt, golden("sample.vtt"));
        assert_eq!(to_vtt(&[]), "WEBVTT\n");
        assert_eq!(to_srt(&[]), "");
    }
}
