//! Numbered image sequences (`shot_0001.png`, `shot_0002.png`, ...): detection from one
//! file, a `%0Nd` pattern or a folder, gap reporting, and frame timing at a caller-chosen
//! rate (the project's by default; FFmpeg's 25 fps image default is never used).
//!
//! Frame `n` of the sequence is file number `first + n`, presented at `n / rate`. A
//! missing number is a gap: the timeline has no frame there, so the nearest-earlier rule
//! of [`crate::timemap`] holds the last frame before it on screen.

use std::path::{Path, PathBuf};

use crate::error::{MediaError, Result};
use crate::time::Rational;
use crate::timemap::{FrameTimes, SourceTimeline};

/// A numbered run of picture files in one folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSequence {
    pub dir: PathBuf,
    /// Text before the number (`shot_`).
    pub prefix: String,
    /// Text after the number, extension included (`.png`).
    pub suffix: String,
    /// Digits of zero padding (`4` for `0001`), or `None` for unpadded numbers.
    pub padding: Option<usize>,
    /// The numbers present, sorted.
    pub numbers: Vec<u64>,
}

impl ImageSequence {
    /// The sequence `file` belongs to: its siblings with the same prefix, suffix and
    /// padding. `None` when the file name has no number before its extension.
    pub fn detect(file: &Path) -> Result<Option<Self>> {
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
            return Ok(None);
        };
        let Some((prefix, digits, suffix)) = split_number(name) else {
            return Ok(None);
        };
        let dir = parent(file);
        // `frame_1000.png` may belong to a padded run (`frame_0999.png`) or an unpadded
        // one (`frame_999.png`): padded when the name or a sibling shows zero padding.
        let padded = Self::scan(&dir, prefix, suffix, Some(digits.len()))?;
        let is_padded = digits.starts_with('0')
            || padded
                .numbers
                .iter()
                .any(|n| n.to_string().len() < digits.len());
        let sequence = if is_padded {
            padded
        } else {
            Self::scan(&dir, prefix, suffix, None)?
        };
        Ok((!sequence.numbers.is_empty()).then_some(sequence))
    }

    /// The sequence a printf pattern names (`dir/shot_%04d.png`, or `%d` unpadded).
    pub fn from_pattern(pattern: &Path) -> Result<Option<Self>> {
        let Some(name) = pattern.file_name().and_then(|n| n.to_str()) else {
            return Ok(None);
        };
        let Some(at) = name.find('%') else {
            return Ok(None);
        };
        let (prefix, rest) = name.split_at(at);
        let rest = &rest[1..];
        let Some(d) = rest.find('d') else {
            return Ok(None);
        };
        let spec = &rest[..d];
        let suffix = &rest[d + 1..];
        let padding = match spec {
            "" => None,
            s if s.starts_with('0') => match s[1..].parse::<usize>() {
                Ok(n) if n > 0 => Some(n),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        let sequence = Self::scan(&parent(pattern), prefix, suffix, padding)?;
        Ok((!sequence.numbers.is_empty()).then_some(sequence))
    }

    /// The longest numbered run of pictures directly in `dir`.
    pub fn in_folder(dir: &Path) -> Result<Option<Self>> {
        let mut best: Option<Self> = None;
        for name in list_dir(dir)? {
            let Some((prefix, digits, suffix)) = split_number(&name) else {
                continue;
            };
            if !is_picture_name(suffix) {
                continue;
            }
            let padding = (digits.len() > 1 && digits.starts_with('0')).then_some(digits.len());
            if best
                .as_ref()
                .is_some_and(|b| b.prefix == prefix && b.suffix == suffix && b.padding == padding)
            {
                continue;
            }
            let found = Self::scan(dir, prefix, suffix, padding)?;
            if best
                .as_ref()
                .is_none_or(|b| found.numbers.len() > b.numbers.len())
            {
                best = Some(found);
            }
        }
        Ok(best.filter(|b| !b.numbers.is_empty()))
    }

    fn scan(dir: &Path, prefix: &str, suffix: &str, padding: Option<usize>) -> Result<Self> {
        let mut numbers = Vec::new();
        for name in list_dir(dir)? {
            let Some(middle) = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
            else {
                continue;
            };
            if middle.is_empty() || !middle.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let fits = match padding {
                Some(width) => middle.len() == width,
                None => middle.len() == 1 || !middle.starts_with('0'),
            };
            if let (true, Ok(n)) = (fits, middle.parse::<u64>()) {
                numbers.push(n);
            }
        }
        numbers.sort_unstable();
        numbers.dedup();
        Ok(Self {
            dir: dir.to_path_buf(),
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
            padding,
            numbers,
        })
    }

    pub fn first(&self) -> u64 {
        self.numbers.first().copied().unwrap_or(0)
    }

    pub fn last(&self) -> u64 {
        self.numbers.last().copied().unwrap_or(0)
    }

    /// Files present.
    pub fn len(&self) -> usize {
        self.numbers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.numbers.is_empty()
    }

    /// Missing numbers between the first and the last, as inclusive ranges.
    pub fn gaps(&self) -> Vec<(u64, u64)> {
        self.numbers
            .windows(2)
            .filter(|w| w[1] > w[0] + 1)
            .map(|w| (w[0] + 1, w[1] - 1))
            .collect()
    }

    /// The file of number `n`.
    pub fn path_of(&self, n: u64) -> PathBuf {
        let number = match self.padding {
            Some(width) => format!("{n:0width$}"),
            None => n.to_string(),
        };
        self.dir
            .join(format!("{}{}{}", self.prefix, number, self.suffix))
    }

    /// The printf pattern of the sequence (`dir/shot_%04d.png`).
    pub fn pattern(&self) -> PathBuf {
        let spec = match self.padding {
            Some(width) => format!("%0{width}d"),
            None => "%d".to_string(),
        };
        self.dir
            .join(format!("{}{}{}", self.prefix, spec, self.suffix))
    }

    /// Frame times at `rate` frames per second: file number `first + n` at `n / rate`.
    /// Closed form without gaps; with gaps, the present numbers' times.
    pub fn timeline(&self, rate: Rational) -> SourceTimeline {
        let rate = rate.reduced();
        let time_base = rate.invert();
        let first = self.first();
        let frames = if self.gaps().is_empty() {
            FrameTimes::Constant {
                start: 0,
                rate,
                count: self.numbers.len() as u64,
            }
        } else {
            FrameTimes::Variable {
                pts: self.numbers.iter().map(|n| (n - first) as i64).collect(),
                end: (self.last() - first + 1) as i64,
            }
        };
        SourceTimeline {
            stream_index: 0,
            time_base,
            frames,
        }
    }
}

fn parent(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Splits `name_0001.png` into (`name_`, `0001`, `.png`): the last run of digits before
/// the extension.
fn split_number(name: &str) -> Option<(&str, &str, &str)> {
    let dot = name.rfind('.').unwrap_or(name.len());
    let stem = &name[..dot];
    let digits_start = stem
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let digits = &stem[digits_start..];
    (!digits.is_empty()).then(|| (&name[..digits_start], digits, &name[dot..]))
}

fn is_picture_name(suffix: &str) -> bool {
    let ext = suffix.trim_start_matches('.').to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "webp" | "gif" | "heic" | "heif" | "avif"
    )
}

fn list_dir(dir: &Path) -> Result<Vec<String>> {
    let entries = std::fs::read_dir(dir).map_err(|e| MediaError::Image {
        path: dir.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_numbered_names() {
        assert_eq!(
            split_number("shot_0001.png"),
            Some(("shot_", "0001", ".png"))
        );
        assert_eq!(split_number("a12b7.tiff"), Some(("a12b", "7", ".tiff")));
        assert_eq!(split_number("0042.jpg"), Some(("", "0042", ".jpg")));
        assert_eq!(split_number("still.png"), None);
    }

    #[test]
    fn gaps_and_paths() {
        let seq = ImageSequence {
            dir: PathBuf::from("d"),
            prefix: "f_".into(),
            suffix: ".png".into(),
            padding: Some(4),
            numbers: vec![1, 2, 3, 4, 7, 8, 9, 11, 12],
        };
        assert_eq!(seq.gaps(), vec![(5, 6), (10, 10)]);
        assert_eq!(seq.path_of(7), PathBuf::from("d/f_0007.png"));
        assert_eq!(seq.pattern(), PathBuf::from("d/f_%04d.png"));
        let t = seq.timeline(Rational::new(24, 1));
        assert_eq!(t.frame_count(), 9);
        assert_eq!(t.frame_pts(4).map(|p| p.value), Some(6));
        assert_eq!(t.duration().seconds(), 0.5);
    }
}
