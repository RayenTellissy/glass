//! Camera card layouts. A card is read as a camera, not a folder: its clips are listed
//! flat (one per recording, spanned files joined), sidecar files travel with their clip,
//! and the folder maze around them (indexes, thumbnails, proxies) is left out.
//!
//! | Card | Looks for | Clips | Sidecars |
//! | --- | --- | --- | --- |
//! | AVCHD | `PRIVATE/AVCHD/BDMV/STREAM/*.MTS` (or `AVCHD/BDMV/STREAM`, `BDMV/STREAM`) | one per recording; files split at the FAT32 limit are joined | none read |
//! | XDCAM | `XDROOT/Clip/*.MXF`, or `Clip/` beside `MEDIAPRO.XML` | one per take; `C0014_01.MXF` continues `C0014.MXF` | `C0014M01.XML` |
//! | Sony XAVC | `PRIVATE/M4ROOT/CLIP/*.MP4` | one per file | `C0001M01.XML` (NonRealTimeMeta) |
//! | Canon | `DCIM/<nnn>CANON/` | videos and stills by file | none read |
//! | Canon Cinema | `CONTENTS/CLIPS<nnn>/*.MXF` or `.MP4` | one per file | same-stem `.XML` |
//! | DJI | `DCIM/DJI_<nnn>/` or `DCIM/<nnn>MEDIA/DJI_*` | videos and stills by file | same-stem `.SRT` telemetry |

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::classify::{FileKind, MediaClass, classify};

/// Which camera layout a card has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CardKind {
    Avchd,
    Xdcam,
    SonyXavc,
    Canon,
    CanonCinema,
    Dji,
}

impl CardKind {
    pub const ALL: [CardKind; 6] = [
        CardKind::Avchd,
        CardKind::Xdcam,
        CardKind::SonyXavc,
        CardKind::Canon,
        CardKind::CanonCinema,
        CardKind::Dji,
    ];

    /// The format's name as cameras print it (not translated).
    pub fn name(self) -> &'static str {
        match self {
            CardKind::Avchd => "AVCHD",
            CardKind::Xdcam => "XDCAM",
            CardKind::SonyXavc => "Sony XAVC",
            CardKind::Canon => "Canon",
            CardKind::CanonCinema => "Canon Cinema",
            CardKind::Dji => "DJI",
        }
    }
}

/// What a sidecar file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SidecarKind {
    /// Sony NonRealTimeMeta XML (XAVC and XDCAM).
    SonyXml,
    /// Canon Cinema clip XML.
    CanonXml,
    /// DJI flight telemetry subtitles.
    DjiSrt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sidecar {
    pub kind: SidecarKind,
    pub path: PathBuf,
}

/// One recording on a card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardClip {
    /// The camera's clip name (`C0014`, `00012`, `DJI_0001`).
    pub name: String,
    pub class: MediaClass,
    /// The media files in playback order; more than one when the camera split the
    /// recording across files.
    pub files: Vec<PathBuf>,
    pub sidecars: Vec<Sidecar>,
}

/// A card found in a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub kind: CardKind,
    /// The folder the layout starts in (the card's mount point, or where it was copied).
    pub root: PathBuf,
    pub clips: Vec<CardClip>,
    /// Folders inside `root` that belong to the layout, so a folder walk skips them.
    pub covered: Vec<PathBuf>,
}

/// Card reading settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardOptions {
    /// An AVCHD file at least this big that is followed by the next file number was cut
    /// at the FAT32 limit, so the two are one recording. Cameras cut just under 2 GiB.
    pub avchd_span_bytes: u64,
}

impl Default for CardOptions {
    fn default() -> Self {
        Self {
            avchd_span_bytes: 1_900_000_000,
        }
    }
}

/// A child of `dir` whose name matches `name` ignoring ASCII case (cards are FAT or
/// exFAT, and copies keep whatever case the reader showed).
pub(crate) fn child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.exists() {
        return Some(exact);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .map(|e| e.path())
}

fn child_path_ci(dir: &Path, parts: &[&str]) -> Option<PathBuf> {
    parts
        .iter()
        .try_fold(dir.to_path_buf(), |at, part| child_ci(&at, part))
}

/// Files in `dir`, sorted by name.
pub(crate) fn sorted_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Media files among `files` (by name), with their class.
fn media(files: &[PathBuf]) -> impl Iterator<Item = (&PathBuf, MediaClass)> {
    files.iter().filter_map(|f| match classify(f) {
        FileKind::Media(class) => Some((f, class)),
        _ => None,
    })
}

/// Sidecars among `files` keyed by upper-case stem, so `C0001M01.XML` can be looked up
/// as `C0001M01` and `DJI_0001.SRT` as `DJI_0001`.
fn sidecars_by_stem(files: &[PathBuf], exts: &[&str]) -> HashMap<String, PathBuf> {
    files
        .iter()
        .filter(|f| has_ext(f, exts))
        .map(|f| (stem(f).to_ascii_uppercase(), f.clone()))
        .collect()
}

/// The cards whose layout starts in `dir`. A folder can hold more than one (a DJI
/// drone's card with a `DCIM` next to a copied XDCAM folder is rare, but the checks are
/// independent).
pub fn detect(dir: &Path, options: &CardOptions) -> Vec<Card> {
    let mut cards = Vec::new();
    cards.extend(avchd(dir, options));
    cards.extend(xdcam(dir));
    cards.extend(sony_xavc(dir));
    cards.extend(canon_cinema(dir));
    cards.extend(dcim(dir));
    cards.retain(|c| !c.clips.is_empty());
    cards
}

fn avchd(dir: &Path, options: &CardOptions) -> Option<Card> {
    let (stream, covered) = [
        (&["PRIVATE", "AVCHD", "BDMV", "STREAM"][..], 1),
        (&["AVCHD", "BDMV", "STREAM"][..], 1),
        (&["BDMV", "STREAM"][..], 1),
    ]
    .into_iter()
    .find_map(|(parts, keep)| {
        let stream = child_path_ci(dir, parts)?;
        let covered = child_path_ci(dir, &parts[..keep])?;
        Some((stream, covered))
    })?;
    let files: Vec<PathBuf> = sorted_files(&stream)
        .into_iter()
        .filter(|f| has_ext(f, &["mts", "m2ts"]))
        .collect();
    let mut clips: Vec<CardClip> = Vec::new();
    let mut previous: Option<(u64, u64)> = None;
    for file in files {
        let number = stem(&file).parse::<u64>().ok();
        let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        let continues = matches!(
            (previous, number),
            (Some((prev_number, prev_size)), Some(n))
                if n == prev_number + 1 && prev_size >= options.avchd_span_bytes
        );
        if continues && let Some(clip) = clips.last_mut() {
            clip.files.push(file);
        } else {
            clips.push(CardClip {
                name: stem(&file),
                class: MediaClass::Video,
                files: vec![file],
                sidecars: Vec::new(),
            });
        }
        previous = number.map(|n| (n, size));
    }
    Some(Card {
        kind: CardKind::Avchd,
        root: dir.to_path_buf(),
        clips,
        covered: vec![covered],
    })
}

/// Splits `C0014_01` into (`C0014`, 1); `C0014` is part 0.
fn span_part(name: &str) -> (&str, u32) {
    if let Some((base, part)) = name.rsplit_once('_')
        && part.len() == 2
        && let Ok(n) = part.parse::<u32>()
    {
        return (base, n);
    }
    (name, 0)
}

fn xdcam(dir: &Path) -> Option<Card> {
    let (root, clip_dir) = if let Some(xdroot) = child_ci(dir, "XDROOT")
        && let Some(clip) = child_ci(&xdroot, "Clip")
    {
        (vec![xdroot], clip)
    } else {
        let clip = child_ci(dir, "Clip")?;
        child_ci(dir, "MEDIAPRO.XML")?;
        let covered = ["Clip", "Sub", "Edit", "Take", "General"]
            .iter()
            .filter_map(|n| child_ci(dir, n))
            .collect();
        (covered, clip)
    };
    let files = sorted_files(&clip_dir);
    let xml = sidecars_by_stem(&files, &["xml"]);
    let mut clips: Vec<CardClip> = Vec::new();
    for file in files.iter().filter(|f| has_ext(f, &["mxf"])) {
        let name = stem(file);
        let (base, part) = span_part(&name);
        if part > 0
            && let Some(clip) = clips.iter_mut().rev().find(|c| c.name == base)
        {
            clip.files.push(file.clone());
            continue;
        }
        let sidecars = xml
            .get(&format!("{}M01", base.to_ascii_uppercase()))
            .map(|p| Sidecar {
                kind: SidecarKind::SonyXml,
                path: p.clone(),
            })
            .into_iter()
            .collect();
        clips.push(CardClip {
            name: base.to_string(),
            class: MediaClass::Video,
            files: vec![file.clone()],
            sidecars,
        });
    }
    Some(Card {
        kind: CardKind::Xdcam,
        root: dir.to_path_buf(),
        clips,
        covered: root,
    })
}

fn sony_xavc(dir: &Path) -> Option<Card> {
    let m4root = child_path_ci(dir, &["PRIVATE", "M4ROOT"])?;
    let clip_dir = child_ci(&m4root, "CLIP")?;
    let files = sorted_files(&clip_dir);
    let xml = sidecars_by_stem(&files, &["xml"]);
    let clips = files
        .iter()
        .filter(|f| has_ext(f, &["mp4", "mxf", "mov"]))
        .map(|file| {
            let name = stem(file);
            let sidecars = xml
                .get(&format!("{}M01", name.to_ascii_uppercase()))
                .map(|p| Sidecar {
                    kind: SidecarKind::SonyXml,
                    path: p.clone(),
                })
                .into_iter()
                .collect();
            CardClip {
                name,
                class: MediaClass::Video,
                files: vec![file.clone()],
                sidecars,
            }
        })
        .collect();
    let covered = child_ci(dir, "PRIVATE").unwrap_or(m4root);
    Some(Card {
        kind: CardKind::SonyXavc,
        root: dir.to_path_buf(),
        clips,
        covered: vec![covered],
    })
}

fn canon_cinema(dir: &Path) -> Option<Card> {
    let contents = child_ci(dir, "CONTENTS")?;
    let clip_dirs: Vec<PathBuf> = sorted_dirs(&contents)
        .into_iter()
        .filter(|d| {
            d.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.to_ascii_uppercase().starts_with("CLIPS"))
        })
        .collect();
    if clip_dirs.is_empty() {
        return None;
    }
    let mut clips = Vec::new();
    for clip_dir in &clip_dirs {
        let files = sorted_files(clip_dir);
        let xml = sidecars_by_stem(&files, &["xml"]);
        for file in files.iter().filter(|f| has_ext(f, &["mxf", "mp4"])) {
            let name = stem(file);
            let sidecars = xml
                .get(&name.to_ascii_uppercase())
                .map(|p| Sidecar {
                    kind: SidecarKind::CanonXml,
                    path: p.clone(),
                })
                .into_iter()
                .collect();
            clips.push(CardClip {
                name,
                class: MediaClass::Video,
                files: vec![file.clone()],
                sidecars,
            });
        }
    }
    Some(Card {
        kind: CardKind::CanonCinema,
        root: dir.to_path_buf(),
        clips,
        covered: vec![contents],
    })
}

/// `DCIM` cards: Canon (`100CANON`) and DJI (`DJI_001`, or `100MEDIA` holding `DJI_*`
/// files). Other `DCIM` cameras are left to the plain folder walk.
fn dcim(dir: &Path) -> Vec<Card> {
    let Some(dcim) = child_ci(dir, "DCIM") else {
        return Vec::new();
    };
    let mut canon = Card {
        kind: CardKind::Canon,
        root: dir.to_path_buf(),
        clips: Vec::new(),
        covered: Vec::new(),
    };
    let mut dji = Card {
        kind: CardKind::Dji,
        root: dir.to_path_buf(),
        clips: Vec::new(),
        covered: Vec::new(),
    };
    for folder in sorted_dirs(&dcim) {
        let name = folder
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_uppercase())
            .unwrap_or_default();
        let files = sorted_files(&folder);
        let numbered = name.len() >= 3 && name.as_bytes()[..3].iter().all(u8::is_ascii_digit);
        if numbered && name.ends_with("CANON") {
            canon.clips.extend(media(&files).map(|(f, class)| CardClip {
                name: stem(f),
                class,
                files: vec![f.clone()],
                sidecars: Vec::new(),
            }));
            canon.covered.push(folder);
        } else if name.starts_with("DJI_")
            || (numbered
                && name.ends_with("MEDIA")
                && files
                    .iter()
                    .any(|f| stem(f).to_ascii_uppercase().starts_with("DJI_")))
        {
            let srt = sidecars_by_stem(&files, &["srt"]);
            dji.clips.extend(media(&files).map(|(f, class)| {
                CardClip {
                    name: stem(f),
                    class,
                    files: vec![f.clone()],
                    sidecars: srt
                        .get(&stem(f).to_ascii_uppercase())
                        .map(|p| Sidecar {
                            kind: SidecarKind::DjiSrt,
                            path: p.clone(),
                        })
                        .into_iter()
                        .collect(),
                }
            }));
            dji.covered.push(folder);
        }
    }
    [canon, dji]
        .into_iter()
        .filter(|c| !c.clips.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_parts() {
        assert_eq!(span_part("C0014_01"), ("C0014", 1));
        assert_eq!(span_part("C0014"), ("C0014", 0));
        assert_eq!(span_part("A_B_C"), ("A_B_C", 0));
        assert_eq!(span_part("DJI_0001"), ("DJI_0001", 0));
    }
}
