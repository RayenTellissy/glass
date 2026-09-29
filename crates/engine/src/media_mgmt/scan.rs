//! The import scan: turns dropped or picked paths into an [`ImportPlan`] by walking
//! folders recursively, reading camera cards as cards, grouping numbered stills into
//! image sequences and setting aside files Glass cannot read. It reads directory
//! listings and file sizes only; nothing is opened or probed.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::cards::{self, CardKind, CardOptions, Sidecar};
use super::classify::{FileKind, MediaClass, Unreadable, classify, is_hidden};

/// A numbered run of stills needs at least this many files to import as one clip.
pub const SEQUENCE_MIN_FILES: usize = 8;

/// Folder depth the walk goes to; deeper folders are left out (and logged).
pub const MAX_DEPTH: usize = 32;

/// One clip to import, before it is probed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSource {
    /// Clip name: the camera's name on a card, the file stem otherwise, the shared
    /// prefix for an image sequence.
    pub name: String,
    pub class: MediaClass,
    /// Media files in playback order: more than one for a recording the camera split,
    /// every frame for an image sequence.
    pub files: Vec<PathBuf>,
    pub sidecars: Vec<Sidecar>,
    pub card: Option<CardKind>,
    /// Folder names from the dropped item down to the clip's folder: the bins it lands
    /// in when folders are kept as bins. Empty for a file dropped on its own.
    pub bin: Vec<String>,
    pub size_bytes: u64,
    /// The first file is already in the project (by path).
    pub already_in_project: bool,
}

impl ImportSource {
    pub fn primary(&self) -> &Path {
        &self.files[0]
    }
}

/// A folder the plan would turn into a bin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedFolder {
    pub path: PathBuf,
    pub bin: Vec<String>,
    /// Clips inside it, subfolders included.
    pub clips: usize,
    /// Clips inside that are not in the project yet.
    pub new_clips: usize,
    /// The folder is a camera card (its clips are listed flat).
    pub card: Option<CardKind>,
}

impl PlannedFolder {
    /// Folders with no video, audio or pictures are left out of the import.
    pub fn left_out(&self) -> bool {
        self.clips == 0
    }

    pub fn depth(&self) -> usize {
        self.bin.len().saturating_sub(1)
    }
}

/// A card found during the scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardSummary {
    pub kind: CardKind,
    pub root: PathBuf,
    pub clips: usize,
}

/// Everything an import would do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportPlan {
    pub roots: Vec<PathBuf>,
    pub sources: Vec<ImportSource>,
    /// Folders in walk order (a parent before its children).
    pub folders: Vec<PlannedFolder>,
    pub cards: Vec<CardSummary>,
    /// Files that look like media but cannot be read, so the user is told.
    pub unreadable: Vec<(PathBuf, Unreadable)>,
    /// Folders deeper than [`MAX_DEPTH`] or unreadable, left out.
    pub unvisited: Vec<PathBuf>,
}

impl ImportPlan {
    /// Clips not already in the project.
    pub fn new_sources(&self) -> impl Iterator<Item = &ImportSource> {
        self.sources.iter().filter(|s| !s.already_in_project)
    }

    pub fn new_count(&self) -> usize {
        self.new_sources().count()
    }

    pub fn already_in_project(&self) -> usize {
        self.sources.len() - self.new_count()
    }

    pub fn new_bytes(&self) -> u64 {
        self.new_sources().map(|s| s.size_bytes).sum()
    }

    /// Bins the import would create when folders are kept as bins.
    pub fn bins(&self) -> usize {
        self.folders.iter().filter(|f| !f.left_out()).count()
    }

    pub fn unreadable_by_kind(&self) -> Vec<(Unreadable, usize)> {
        let mut counts: BTreeMap<Unreadable, usize> = BTreeMap::new();
        for (_, kind) in &self.unreadable {
            *counts.entry(*kind).or_default() += 1;
        }
        counts.into_iter().collect()
    }
}

/// Scan settings.
#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    pub cards: CardOptions,
    /// Files already in the project, by path.
    pub known_paths: HashSet<PathBuf>,
}

/// The scan was cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

fn size_of(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Builds the plan for `paths`. Checks `cancel` between folders.
pub fn scan(
    paths: &[PathBuf],
    options: &ScanOptions,
    cancel: &AtomicBool,
) -> Result<ImportPlan, Cancelled> {
    let mut plan = ImportPlan {
        roots: paths.to_vec(),
        ..ImportPlan::default()
    };
    let mut loose = Vec::new();
    let mut covered = HashSet::new();
    for path in paths {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        if path.is_dir() {
            let bin = vec![file_name(path)];
            visit(path, bin, 0, options, cancel, &mut covered, &mut plan)?;
        } else if path.is_file() {
            loose.push(path.clone());
        }
    }
    add_files(&loose, &[], options, &mut plan);
    Ok(plan)
}

/// Walks `dir`, returning its clip counts (all, new).
fn visit(
    dir: &Path,
    bin: Vec<String>,
    depth: usize,
    options: &ScanOptions,
    cancel: &AtomicBool,
    covered: &mut HashSet<PathBuf>,
    plan: &mut ImportPlan,
) -> Result<(usize, usize), Cancelled> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }
    let folder_index = plan.folders.len();
    plan.folders.push(PlannedFolder {
        path: dir.to_path_buf(),
        bin: bin.clone(),
        clips: 0,
        new_clips: 0,
        card: None,
    });
    let before = plan.sources.len();

    // Card folders found here may sit several levels down (`DCIM/100CANON`), so the
    // set of covered folders is shared by the whole walk.
    let found = cards::detect(dir, &options.cards);
    covered.extend(found.iter().flat_map(|c| c.covered.clone()));
    for card in found {
        plan.cards.push(CardSummary {
            kind: card.kind,
            root: card.root.clone(),
            clips: card.clips.len(),
        });
        plan.folders[folder_index].card.get_or_insert(card.kind);
        for clip in card.clips {
            let size_bytes = clip.files.iter().map(|f| size_of(f)).sum();
            let already_in_project = options.known_paths.contains(&clip.files[0]);
            plan.sources.push(ImportSource {
                name: clip.name,
                class: clip.class,
                files: clip.files,
                sidecars: clip.sidecars,
                card: Some(card.kind),
                bin: bin.clone(),
                size_bytes,
                already_in_project,
            });
        }
    }

    let mut files = Vec::new();
    let mut dirs = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                if is_hidden(&path) || covered.contains(&path) {
                    continue;
                }
                match entry.file_type() {
                    Ok(t) if t.is_dir() => dirs.push(path),
                    Ok(t) if t.is_file() => files.push(path),
                    _ => {}
                }
            }
        }
        Err(error) => {
            tracing::warn!(dir = %dir.display(), %error, "import: folder not readable");
            plan.unvisited.push(dir.to_path_buf());
        }
    }
    files.sort();
    dirs.sort();
    add_files(&files, &bin, options, plan);

    let direct = plan.sources[before..].len();
    let direct_new = plan.sources[before..]
        .iter()
        .filter(|s| !s.already_in_project)
        .count();
    let (mut clips, mut new_clips) = (direct, direct_new);
    for sub in dirs {
        if depth + 1 >= MAX_DEPTH {
            tracing::warn!(dir = %sub.display(), "import: folder too deep, left out");
            plan.unvisited.push(sub);
            continue;
        }
        let mut sub_bin = bin.clone();
        sub_bin.push(file_name(&sub));
        let (c, n) = visit(&sub, sub_bin, depth + 1, options, cancel, covered, plan)?;
        clips += c;
        new_clips += n;
    }
    plan.folders[folder_index].clips = clips;
    plan.folders[folder_index].new_clips = new_clips;
    Ok((clips, new_clips))
}

/// Splits `name_0001.png` into (`name_`, `0001`, `.png`): the last run of digits in the
/// stem.
fn numbered(path: &Path) -> Option<(String, usize, String)> {
    let name = file_name(path);
    let dot = name.rfind('.').unwrap_or(name.len());
    let stem = &name[..dot];
    let end = stem.rfind(|c: char| c.is_ascii_digit())? + 1;
    let start = stem[..end]
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    Some((
        stem[..start].to_string(),
        end - start,
        format!("{}{}", &stem[end..], &name[dot..]).to_ascii_lowercase(),
    ))
}

fn add_files(files: &[PathBuf], bin: &[String], options: &ScanOptions, plan: &mut ImportPlan) {
    // Numbered stills sharing a prefix, digit count and suffix form a sequence once there
    // are enough of them; a handful of IMG_0001.JPG photos stay separate stills.
    let mut groups: BTreeMap<(String, usize, String), Vec<PathBuf>> = BTreeMap::new();
    let mut singles: Vec<(PathBuf, MediaClass)> = Vec::new();
    for file in files {
        match classify(file) {
            FileKind::Media(MediaClass::Still) => match numbered(file) {
                Some(key) => groups.entry(key).or_default().push(file.clone()),
                None => singles.push((file.clone(), MediaClass::Still)),
            },
            FileKind::Media(class) => singles.push((file.clone(), class)),
            FileKind::Unreadable(kind) => plan.unreadable.push((file.clone(), kind)),
            FileKind::Junk | FileKind::Other => {}
        }
    }
    for ((prefix, _, _), members) in groups {
        if members.len() >= SEQUENCE_MIN_FILES {
            let name = prefix.trim_end_matches(['_', '-', '.', ' ']).to_string();
            let name = if name.is_empty() {
                bin.last().cloned().unwrap_or_else(|| stem(&members[0]))
            } else {
                name
            };
            plan.sources.push(ImportSource {
                name,
                class: MediaClass::Sequence,
                size_bytes: members.iter().map(|f| size_of(f)).sum(),
                already_in_project: options.known_paths.contains(&members[0]),
                files: members,
                sidecars: Vec::new(),
                card: None,
                bin: bin.to_vec(),
            });
        } else {
            singles.extend(members.into_iter().map(|f| (f, MediaClass::Still)));
        }
    }
    singles.sort();
    for (file, class) in singles {
        plan.sources.push(ImportSource {
            name: stem(&file),
            class,
            size_bytes: size_of(&file),
            already_in_project: options.known_paths.contains(&file),
            files: vec![file],
            sidecars: Vec::new(),
            card: None,
            bin: bin.to_vec(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_names() {
        assert_eq!(
            numbered(Path::new("/a/shot_0012.png")),
            Some(("shot_".into(), 4, ".png".into()))
        );
        assert_eq!(
            numbered(Path::new("/a/0001.PNG")),
            Some(("".into(), 4, ".png".into()))
        );
        assert_eq!(numbered(Path::new("/a/poster.png")), None);
    }
}
