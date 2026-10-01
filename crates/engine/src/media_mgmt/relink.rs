//! Relinking offline media: finding where moved, renamed or copied source files went,
//! ranking what was found, and handing the choice to the project as one undoable edit.
//!
//! A search ([`search`], or [`RelinkSearch`] on its own `glass-relink` thread) walks the
//! folders the user picked with the import scan ([`super::scan`]), so cards, image
//! sequences and hidden files are read the same way as on import, then scores every
//! file of the right kind against every offline source ([`RelinkTarget`]):
//!
//! - **Content hash** (`media::ContentHash`, the key recorded at import) is decisive: an
//!   equal hash is the same file, whatever its name or folder ([`Confidence::Exact`]).
//!   A file whose hash differs (a re-encode or proxy under the same name) never ranks
//!   as exact.
//! - Otherwise the score weighs **name** similarity, **duration**, then resolution,
//!   frame rate and codec from a probe, against the facts the media pool kept from
//!   import ([`score`]). Scores fall into [`Confidence`] tiers.
//! - **One folder moved**: when a source is found exactly at a new place, the same
//!   relative move ([`FolderMove`]) is tried for every other offline source, so a
//!   renamed drive or a moved project folder relinks everything from one match. The
//!   user's own pick in the dialog feeds a move back the same way.
//!
//! Hashing reads up to 2.5 MiB per file, so the search hashes at most
//! [`SearchOptions::max_hashes`] files, the most likely first, and probes only a short
//! list per source. A candidate shorter than the source range its clips use is kept in
//! the list but marked [`Candidate::too_short`], and [`apply`] refuses it, as does
//! [`project::Project::relink_media`].
//!
//! Applying is [`apply`]: one `relink_media` edit for the whole batch, committed with
//! `EditLabel::Relink`; [`mark_relinked`] then brings the sources back online in the
//! availability map and the media pool.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use media::ContentHash;
use project::media_pool::{Facts, MediaKind, MediaPool, MediaStatus};
use project::{EditError, MediaId, Project, Relink, Time};

use super::classify::{FileKind, MediaClass, classify};
use super::import::facts_of;
use super::offline::{Availability, MediaAvailability};
use super::scan::{self, ScanOptions};

/// Scores below this are not offered at all.
pub const MIN_SCORE: f32 = 0.4;
/// The highest score a file without an equal content hash can reach: 100% means the
/// same bytes.
pub const MAX_INEXACT: f32 = 0.95;

/// One offline source to find.
#[derive(Clone, Debug, PartialEq)]
pub struct RelinkTarget {
    pub media: MediaId,
    /// What the pool calls it (`A014_C011`), or the file stem.
    pub name: String,
    /// Where it was last seen.
    pub path: PathBuf,
    /// The content hash recorded at import (hex); empty when none was.
    pub content_hash: String,
    /// The pool's facts from import, when the pool knows the source.
    pub facts: Option<Facts>,
    pub size_bytes: Option<u64>,
    /// `None` when neither the pool nor the file name says.
    pub class: Option<MediaClass>,
    /// The furthest source time its clips use, in seconds: a shorter file cannot stand
    /// in for it.
    pub used_until: Option<f64>,
    /// How many clips use it.
    pub clips: usize,
}

/// The offline sources of `project`, in id order, with what the pool knows of them.
pub fn targets(
    project: &Project,
    pool: Option<&MediaPool>,
    availability: &MediaAvailability,
) -> Vec<RelinkTarget> {
    availability
        .offline()
        .filter_map(|(id, _)| project.media().get(id))
        .map(|media| {
            let path = PathBuf::from(&media.path);
            let item = pool.and_then(|p| p.item(media.id));
            let class = item
                .map(|i| class_of_kind(i.kind))
                .or_else(|| match classify(&path) {
                    FileKind::Media(class) => Some(class),
                    _ => None,
                });
            RelinkTarget {
                media: media.id,
                name: item.map_or_else(|| stem(&path), |i| i.name.clone()),
                content_hash: media.content_hash.clone(),
                facts: item.map(|i| i.facts.clone()),
                size_bytes: item.map(|i| i.size_bytes).filter(|s| *s > 0),
                class,
                used_until: project
                    .source_used_until(media.id)
                    .ok()
                    .flatten()
                    .map(Time::as_secs_f64),
                clips: project.clip_count(media.id),
                path,
            }
        })
        .collect()
}

fn class_of_kind(kind: MediaKind) -> MediaClass {
    match kind {
        MediaKind::Video => MediaClass::Video,
        MediaKind::Audio => MediaClass::Audio,
        MediaKind::Still => MediaClass::Still,
        MediaKind::Sequence => MediaClass::Sequence,
    }
}

/// Whether one property of a candidate agrees with the offline source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Match,
    Mismatch,
    /// Not known on one side (no recorded hash, no duration for a still).
    Unknown,
}

/// How sure a match is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    /// Below [`Confidence::Likely`]: shown, never picked for the user.
    Possible,
    /// Name and duration agree (score at least 0.7) but the bytes are not known to be
    /// the same: picked for the user when nothing better was found.
    Likely,
    /// The same content hash: the same file.
    Exact,
}

impl Confidence {
    fn of(score: f32, exact: bool) -> Self {
        if exact {
            Self::Exact
        } else if score >= 0.7 {
            Self::Likely
        } else {
            Self::Possible
        }
    }
}

/// A file that could be an offline source.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub path: PathBuf,
    /// 0 to 1; 1 only for an exact (content hash) match.
    pub score: f32,
    pub confidence: Confidence,
    pub name: Check,
    pub duration: Check,
    pub hash: Check,
    /// The file's content hash (hex), recorded on the reference when relinked.
    pub content_hash: String,
    /// What a probe read; `None` when the file did not probe (it can still match by
    /// hash).
    pub facts: Option<Facts>,
    /// Shorter than the source range the clips use: cannot be relinked.
    pub too_short: bool,
    /// Found by repeating another source's folder move rather than in a searched folder.
    pub from_move: bool,
}

impl Candidate {
    pub fn percent(&self) -> u8 {
        (self.score.clamp(0.0, 1.0) * 100.0).round() as u8
    }

    pub fn relinkable(&self) -> bool {
        !self.too_short
    }

    /// The candidate's length in seconds, when it has one.
    pub fn duration_s(&self) -> Option<f64> {
        self.facts.as_ref().and_then(|f| f.duration_s)
    }
}

/// The candidates for one offline source, best first.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetMatches {
    pub media: MediaId,
    pub candidates: Vec<Candidate>,
}

impl TargetMatches {
    /// The candidate to pick for the user: the best relinkable one that is at least
    /// [`Confidence::Likely`].
    pub fn best(&self) -> Option<usize> {
        self.candidates
            .iter()
            .position(|c| c.relinkable() && c.confidence >= Confidence::Likely)
    }
}

/// Search limits.
#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub scan: ScanOptions,
    /// Files hashed across the whole search, most likely first. Hashing reads up to
    /// 2.5 MiB per file.
    pub max_hashes: usize,
    /// Files probed and scored per offline source, besides exact matches.
    pub max_per_target: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            scan: ScanOptions::default(),
            max_hashes: 2000,
            max_per_target: 8,
        }
    }
}

/// What to search for and where.
#[derive(Clone, Debug, Default)]
pub struct SearchRequest {
    pub targets: Vec<RelinkTarget>,
    /// The folders the user picked; may be empty when only `moves` are tried.
    pub folders: Vec<PathBuf>,
    /// Folder moves to try for every source (from the user's own picks).
    pub moves: Vec<FolderMove>,
    pub options: SearchOptions,
}

/// Where a search is.
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    /// Listing a folder the user picked.
    Scanning { folder: PathBuf },
    /// Hashing and probing files: `done` of `total`, now `path`.
    Checking {
        done: usize,
        total: usize,
        path: PathBuf,
    },
}

/// What a search found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchResult {
    /// One entry per target, in the request's order.
    pub matches: Vec<TargetMatches>,
    /// Folder moves inferred from exact matches (and the ones requested), for the
    /// dialog to repeat when the user picks a file by hand.
    pub moves: Vec<FolderMove>,
    pub folders_searched: usize,
    /// Media files the scan listed.
    pub files_seen: usize,
    pub hashed: usize,
    pub probed: usize,
    pub elapsed: Duration,
}

impl SearchResult {
    pub fn get(&self, media: MediaId) -> Option<&TargetMatches> {
        self.matches.iter().find(|m| m.media == media)
    }

    /// Sources with a candidate picked for the user.
    pub fn matched(&self) -> usize {
        self.matches.iter().filter(|m| m.best().is_some()).count()
    }
}

/// The search was cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// The same relative move for every file under `from`: a moved or renamed folder or
/// drive. `/Volumes/Card A/Clip/A014.mov` found at `/Volumes/SSD/Harbour/Card A/Clip/
/// A014.mov` is the move `/Volumes` to `/Volumes/SSD/Harbour`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FolderMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

impl FolderMove {
    /// The move that took `old` to `new`: the paths without the trailing components they
    /// share (at least the file name, which may have been renamed). `None` when nothing
    /// moved.
    pub fn infer(old: &Path, new: &Path) -> Option<Self> {
        let a: Vec<Component> = old.components().collect();
        let b: Vec<Component> = new.components().collect();
        let mut common = a
            .iter()
            .rev()
            .zip(b.iter().rev())
            .take_while(|(x, y)| same_component(x, y))
            .count();
        // Keep at least one component on each side, and drop at least the file name.
        common = common.clamp(1, a.len().min(b.len()).saturating_sub(1).max(1));
        let from: PathBuf = a[..a.len().saturating_sub(common)].iter().collect();
        let to: PathBuf = b[..b.len().saturating_sub(common)].iter().collect();
        (from != to && !from.as_os_str().is_empty() && !to.as_os_str().is_empty())
            .then_some(Self { from, to })
    }

    /// Where `path` would be after this move; `None` when it is not under `from`.
    pub fn apply(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.from).ok().map(|rest| {
            if rest.as_os_str().is_empty() {
                self.to.clone()
            } else {
                self.to.join(rest)
            }
        })
    }
}

fn same_component(a: &Component, b: &Component) -> bool {
    match (a, b) {
        (Component::Normal(x), Component::Normal(y)) => x
            .to_string_lossy()
            .eq_ignore_ascii_case(&y.to_string_lossy()),
        _ => a == b,
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// How alike two file names are, 0 to 1, ignoring extension and case: 1 for the same
/// stem, a little less when one contains the other (`B002_C004_proxy`), otherwise one
/// minus the edit distance over the longer length.
pub fn name_similarity(a: &Path, b: &Path) -> f32 {
    let a = stem(a).to_lowercase();
    let b = stem(b).to_lowercase();
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longer = a.len().max(b.len()) as f32;
    let ratio = 1.0 - levenshtein(&a, &b) as f32 / longer;
    let contains = |x: &[char], y: &[char]| x.windows(y.len()).any(|w| w == y);
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    if contains(long, short) {
        ratio.max(0.6 + 0.3 * short.len() as f32 / longer)
    } else {
        ratio
    }
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if ca == cb {
                diagonal
            } else {
                1 + diagonal.min(above).min(row[j])
            };
            diagonal = above;
        }
    }
    row[b.len()]
}

/// Duration agreement: a match within half a frame at the source's rate (at least
/// 50 ms), a score that falls off smoothly with the difference so the nearer of two
/// near-duplicates ranks first.
fn duration_score(target: f64, found: f64, fps: Option<f64>) -> (Check, f32) {
    let tolerance = fps
        .filter(|f| *f > 0.0)
        .map_or(0.05, |f| (0.5 / f).max(0.05));
    let delta = (target - found).abs();
    let check = if delta <= tolerance {
        Check::Match
    } else {
        Check::Mismatch
    };
    let x = delta / 0.1;
    (check, (1.0 / (1.0 + x * x)) as f32)
}

/// Scores a probed file against an offline source, without the content hash (exact
/// matches are scored by [`search`]): name 0.35, duration 0.35, resolution, frame rate
/// and codec 0.1 each, over the parts both sides know, scaled down when only a few are
/// known, times 0.9 when the hash is known to differ, capped at [`MAX_INEXACT`].
pub fn score(
    target: &RelinkTarget,
    path: &Path,
    facts: Option<&Facts>,
    hash: Check,
) -> (f32, Check, Check) {
    let name = name_similarity(&target.path, path);
    let name_check = if name >= 1.0 {
        Check::Match
    } else {
        Check::Mismatch
    };
    let mut sum = 0.35 * name;
    let mut known = 0.35;
    let mut duration_check = Check::Unknown;
    if let (Some(t), Some(f)) = (target.facts.as_ref(), facts) {
        if let (Some(td), Some(fd)) = (t.duration_s, f.duration_s) {
            let (check, s) = duration_score(td, fd, t.fps());
            duration_check = check;
            sum += 0.35 * s;
            known += 0.35;
        }
        if t.has_video() && f.has_video() {
            sum += 0.1 * f32::from(t.width == f.width && t.height == f.height);
            known += 0.1;
        }
        if let (Some(a), Some(b)) = (t.fps(), f.fps()) {
            sum += 0.1 * f32::from((a - b).abs() < 0.01);
            known += 0.1;
        }
        let codec = |f: &Facts| f.video_codec.clone().or_else(|| f.audio_codec.clone());
        if let (Some(a), Some(b)) = (codec(t), codec(f)) {
            sum += 0.1 * f32::from(a == b);
            known += 0.1;
        }
    }
    // `known` is out of 1.0: every weight together.
    let coverage = 0.6 + 0.4 * known;
    let mut s = sum / known * coverage;
    if hash == Check::Mismatch {
        s *= 0.9;
    }
    (s.min(MAX_INEXACT), name_check, duration_check)
}

/// A file the scan found, as the search sees it.
#[derive(Clone, Debug)]
struct Found {
    /// The path a reference would point at (the folder for an image sequence).
    path: PathBuf,
    class: MediaClass,
    size: u64,
}

/// Hashes and probes, each done once per path.
#[derive(Default)]
struct Probes {
    hashes: HashMap<PathBuf, Option<String>>,
    facts: HashMap<PathBuf, Option<Facts>>,
    hashed: usize,
    probed: usize,
}

impl Probes {
    fn hash(&mut self, path: &Path) -> Option<String> {
        if let Some(h) = self.hashes.get(path) {
            return h.clone();
        }
        let hash = if path.is_dir() {
            ContentHash::of_folder(path)
        } else {
            ContentHash::of_file(path)
        };
        let hex = hash.ok().map(|h| h.to_hex());
        self.hashed += 1;
        self.hashes.insert(path.to_path_buf(), hex.clone());
        hex
    }

    fn facts(&mut self, path: &Path, class: MediaClass) -> Option<Facts> {
        if let Some(f) = self.facts.get(path) {
            return f.clone();
        }
        let file = if path.is_dir() {
            first_file(path)
        } else {
            Some(path.to_path_buf())
        };
        let facts = file
            .and_then(|f| media::probe(f).ok())
            .map(|info| facts_of(&info))
            .map(|mut f| {
                if matches!(class, MediaClass::Still | MediaClass::Sequence) {
                    f.duration_s = None;
                }
                f
            });
        self.probed += 1;
        self.facts.insert(path.to_path_buf(), facts.clone());
        facts
    }
}

fn first_file(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| matches!(classify(p), FileKind::Media(_)))
        .collect();
    files.sort();
    files.into_iter().next()
}

fn compatible(target: Option<MediaClass>, found: MediaClass) -> bool {
    target.is_none_or(|t| t == found)
}

/// Searches `request.folders` (and the folder moves) for every target. Blocking: run it
/// off the UI thread ([`RelinkSearch`]). Checks `cancel` between files.
pub fn search(
    request: &SearchRequest,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<SearchResult, Cancelled> {
    let start = Instant::now();
    let options = &request.options;
    let mut found: Vec<Found> = Vec::new();
    let mut seen_paths = HashSet::new();
    for folder in &request.folders {
        progress(Progress::Scanning {
            folder: folder.clone(),
        });
        let plan = scan::scan(std::slice::from_ref(folder), &options.scan, cancel)
            .map_err(|_| Cancelled)?;
        for source in plan.sources {
            let path = if source.class == MediaClass::Sequence {
                source
                    .primary()
                    .parent()
                    .map_or_else(|| source.primary().to_path_buf(), Path::to_path_buf)
            } else {
                source.primary().to_path_buf()
            };
            if seen_paths.insert(path.clone()) {
                found.push(Found {
                    path,
                    class: source.class,
                    size: source.size_bytes,
                });
            }
        }
    }

    // A first guess per (target, file) from the name and size alone.
    let quick = |t: &RelinkTarget, f: &Found| -> f32 {
        let size = t.size_bytes.is_some_and(|s| s == f.size);
        name_similarity(&t.path, &f.path) + if size { 1.0 } else { 0.0 }
    };
    let mut best_quick = vec![0.0f32; found.len()];
    for (i, f) in found.iter().enumerate() {
        for t in &request.targets {
            if compatible(t.class, f.class) {
                best_quick[i] = best_quick[i].max(quick(t, f) + 0.01);
            }
        }
    }
    // Hash the most likely files first, up to the budget.
    let mut order: Vec<usize> = (0..found.len()).filter(|&i| best_quick[i] > 0.0).collect();
    order.sort_by(|&a, &b| best_quick[b].total_cmp(&best_quick[a]).then(a.cmp(&b)));
    order.truncate(options.max_hashes);

    // Short lists: by name and size, plus every file with an equal hash.
    let mut short: Vec<Vec<usize>> = request
        .targets
        .iter()
        .map(|t| {
            let mut list: Vec<(f32, usize)> = found
                .iter()
                .enumerate()
                .filter(|(_, f)| compatible(t.class, f.class))
                .map(|(i, f)| (quick(t, f), i))
                .filter(|(q, _)| *q >= 0.5)
                .collect();
            list.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            list.truncate(options.max_per_target);
            list.into_iter().map(|(_, i)| i).collect()
        })
        .collect();
    let total = order.len() + short.iter().map(Vec::len).sum::<usize>();
    let mut done = 0;
    let mut probes = Probes::default();
    let mut by_hash: HashMap<String, Vec<usize>> = HashMap::new();
    for &i in &order {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        progress(Progress::Checking {
            done,
            total,
            path: found[i].path.clone(),
        });
        done += 1;
        if let Some(hex) = probes.hash(&found[i].path) {
            by_hash.entry(hex).or_default().push(i);
        }
    }
    for (t, list) in request.targets.iter().zip(short.iter_mut()) {
        if t.content_hash.is_empty() {
            continue;
        }
        for &i in by_hash
            .get(&t.content_hash.to_ascii_lowercase())
            .into_iter()
            .flatten()
        {
            if compatible(t.class, found[i].class) && !list.contains(&i) {
                list.insert(0, i);
            }
        }
    }

    let mut matches = Vec::with_capacity(request.targets.len());
    for (t, list) in request.targets.iter().zip(&short) {
        let mut candidates = Vec::new();
        for &i in list {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled);
            }
            progress(Progress::Checking {
                done,
                total,
                path: found[i].path.clone(),
            });
            done += 1;
            let f = &found[i];
            if let Some(c) = evaluate(t, &f.path, f.class, false, &mut probes) {
                candidates.push(c);
            }
        }
        matches.push(TargetMatches {
            media: t.media,
            candidates,
        });
    }

    // One folder moved: repeat every exact match's move (and the requested ones) for
    // the sources that have no exact match yet.
    let mut moves: Vec<FolderMove> = request.moves.clone();
    for (t, m) in request.targets.iter().zip(&matches) {
        for c in m
            .candidates
            .iter()
            .filter(|c| c.confidence == Confidence::Exact)
        {
            if let Some(mv) = FolderMove::infer(&t.path, &c.path)
                && !moves.contains(&mv)
            {
                moves.push(mv);
            }
        }
    }
    for (t, m) in request.targets.iter().zip(matches.iter_mut()) {
        if m.candidates
            .iter()
            .any(|c| c.confidence == Confidence::Exact)
        {
            continue;
        }
        for mv in &moves {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled);
            }
            let Some(path) = mv.apply(&t.path) else {
                continue;
            };
            if m.candidates.iter().any(|c| c.path == path) || !path.exists() {
                continue;
            }
            let class = t.class.unwrap_or(if path.is_dir() {
                MediaClass::Sequence
            } else {
                MediaClass::Video
            });
            if let Some(c) = evaluate(t, &path, class, true, &mut probes) {
                m.candidates.push(c);
            }
        }
    }
    for m in &mut matches {
        m.candidates.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(b.from_move.cmp(&a.from_move))
                .then(a.path.cmp(&b.path))
        });
    }
    Ok(SearchResult {
        matches,
        moves,
        folders_searched: request.folders.len(),
        files_seen: found.len(),
        hashed: probes.hashed,
        probed: probes.probed,
        elapsed: start.elapsed(),
    })
}

/// Hashes, probes and scores one file for `target`; `None` when it scores too low.
fn evaluate(
    target: &RelinkTarget,
    path: &Path,
    class: MediaClass,
    from_move: bool,
    probes: &mut Probes,
) -> Option<Candidate> {
    let hex = probes.hash(path).unwrap_or_default();
    let hash = if target.content_hash.is_empty() || hex.is_empty() {
        Check::Unknown
    } else if hex.eq_ignore_ascii_case(&target.content_hash) {
        Check::Match
    } else {
        Check::Mismatch
    };
    let facts = probes.facts(path, class);
    let (mut s, name, mut duration) = score(target, path, facts.as_ref(), hash);
    if hash == Check::Match {
        s = 1.0;
        if duration == Check::Unknown
            && target
                .facts
                .as_ref()
                .is_some_and(|f| f.duration_s.is_some())
        {
            duration = Check::Match;
        }
    } else if from_move {
        // The same move as a file that matched exactly: as good as a matching name.
        s = (s + 0.1).min(MAX_INEXACT);
    }
    if s < MIN_SCORE {
        return None;
    }
    let length = facts.as_ref().and_then(|f| f.duration_s);
    let too_short = match (length, target.used_until) {
        (Some(have), Some(need)) => have + 0.001 < need,
        _ => false,
    };
    Some(Candidate {
        path: path.to_path_buf(),
        score: s,
        confidence: Confidence::of(s, hash == Check::Match),
        name,
        duration,
        hash,
        content_hash: hex,
        facts,
        too_short,
        from_move,
    })
}

/// A search on its own thread.
#[derive(Debug)]
pub enum RelinkEvent {
    Progress(Progress),
    Finished(Box<SearchResult>),
    Cancelled,
}

/// A [`search`] running on a `glass-relink` thread, reporting through a channel the UI
/// drains once per frame.
pub struct RelinkSearch {
    cancel: Arc<AtomicBool>,
    events: Receiver<RelinkEvent>,
    thread: Option<JoinHandle<()>>,
}

impl RelinkSearch {
    pub fn start(request: SearchRequest) -> std::io::Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, events) = mpsc::channel::<RelinkEvent>();
        let flag = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("glass-relink".into())
            .spawn(move || run(&request, &tx, &flag))?;
        Ok(Self {
            cancel,
            events,
            thread: Some(thread),
        })
    }

    /// Events so far, without waiting.
    pub fn poll(&self) -> Vec<RelinkEvent> {
        self.events.try_iter().collect()
    }

    /// Stops at the next file; `Cancelled` follows.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Blocks until the search ends and returns the remaining events (tests; never on
    /// the UI thread).
    pub fn wait(mut self) -> Vec<RelinkEvent> {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.events.try_iter().collect()
    }
}

impl Drop for RelinkSearch {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn run(request: &SearchRequest, tx: &Sender<RelinkEvent>, cancel: &AtomicBool) {
    // Progress is sent at most every 50 ms so a fast scan does not flood the channel.
    let mut last = None::<Instant>;
    let mut report = |p: Progress| {
        let now = Instant::now();
        if last.is_none_or(|l| now.duration_since(l) >= Duration::from_millis(50)) {
            last = Some(now);
            let _ = tx.send(RelinkEvent::Progress(p));
        }
    };
    let event = match search(request, &mut report, cancel) {
        Ok(result) => {
            tracing::info!(
                targets = request.targets.len(),
                matched = result.matched(),
                files = result.files_seen,
                hashed = result.hashed,
                ms = result.elapsed.as_millis() as u64,
                "relink search finished"
            );
            RelinkEvent::Finished(Box::new(result))
        }
        Err(Cancelled) => RelinkEvent::Cancelled,
    };
    let _ = tx.send(event);
}

/// The user's choice for one source.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub media: MediaId,
    pub candidate: Candidate,
}

/// Seconds to an exact time, to the microsecond.
fn time_of(seconds: f64) -> Option<Time> {
    Time::new((seconds * 1_000_000.0).round() as i64, 1_000_000).ok()
}

/// The relink as one validated project edit: every reference points at its chosen
/// file, nothing else changes. Commit the result with `EditLabel::Relink`. Refuses the
/// whole batch when a candidate is too short for its clips.
pub fn apply(project: &Project, choices: &[Choice]) -> Result<Project, EditError> {
    let relinks: Vec<Relink> = choices
        .iter()
        .map(|c| Relink {
            media: c.media,
            path: c.candidate.path.to_string_lossy().into_owned(),
            content_hash: c.candidate.content_hash.clone(),
            duration: c.candidate.duration_s().and_then(time_of),
        })
        .collect();
    project.relink_media(&relinks)
}

/// After [`apply`]: the chosen sources are online, and the pool's items point at their
/// new files.
pub fn mark_relinked(
    choices: &[Choice],
    availability: &mut MediaAvailability,
    pool: Option<&mut MediaPool>,
) {
    for c in choices {
        availability.set(c.media, Availability::Online);
    }
    if let Some(pool) = pool {
        for c in choices {
            let _ = pool.update_item(c.media, |item| {
                item.path = c.candidate.path.clone();
                if !c.candidate.content_hash.is_empty() {
                    item.content_hash = Some(c.candidate.content_hash.clone());
                }
                item.status = MediaStatus::Online;
            });
        }
    }
}

/// Folders the found files live in, deduplicated, for "Look in the same folders for
/// other offline clips".
pub fn folders_of(choices: &[Choice]) -> Vec<PathBuf> {
    let set: BTreeMap<PathBuf, ()> = choices
        .iter()
        .filter_map(|c| c.candidate.path.parent().map(|p| (p.to_path_buf(), ())))
        .collect();
    set.into_keys().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(path: &str, duration: Option<f64>) -> RelinkTarget {
        RelinkTarget {
            media: MediaId(1),
            name: stem(Path::new(path)),
            path: path.into(),
            content_hash: "aa".into(),
            facts: Some(Facts {
                duration_s: duration,
                width: 3840,
                height: 2160,
                frame_rate: Some((24000, 1001)),
                video_codec: Some("h264".into()),
                ..Facts::default()
            }),
            size_bytes: None,
            class: Some(MediaClass::Video),
            used_until: None,
            clips: 1,
        }
    }

    fn facts(duration: f64, width: u32) -> Facts {
        Facts {
            duration_s: Some(duration),
            width,
            height: width * 9 / 16,
            frame_rate: Some((24000, 1001)),
            video_codec: Some("h264".into()),
            ..Facts::default()
        }
    }

    #[test]
    fn names() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(
            name_similarity(&p("/a/A014_C011.mov"), &p("/b/a014_c011.MP4")),
            1.0
        );
        let proxy = name_similarity(&p("/a/B002_C004.mp4"), &p("/b/B002_C004_proxy.mov"));
        assert!((0.6..1.0).contains(&proxy), "{proxy}");
        let other = name_similarity(&p("/a/B002_C004.mp4"), &p("/b/sunset.mp4"));
        assert!(other < 0.3, "{other}");
        let typo = name_similarity(&p("/a/A014_C011.mov"), &p("/b/A014_C012.mov"));
        assert!(typo > 0.85 && typo < 1.0, "{typo}");
    }

    #[test]
    fn near_duplicate_durations_rank_the_nearer_first() {
        let t = target("/Volumes/Card A/A014_C011.mov", Some(12.012));
        let same = score(
            &t,
            Path::new("/x/A014_C011.mov"),
            Some(&facts(12.012, 3840)),
            Check::Unknown,
        );
        let near = score(
            &t,
            Path::new("/y/A014_C011.mov"),
            Some(&facts(12.054, 3840)),
            Check::Unknown,
        );
        let far = score(
            &t,
            Path::new("/z/A014_C011.mov"),
            Some(&facts(13.5, 3840)),
            Check::Unknown,
        );
        assert!(
            same.0 > near.0 && near.0 > far.0,
            "{same:?} {near:?} {far:?}"
        );
        assert_eq!(same.2, Check::Match);
        // 42 ms is within half a frame at 23.976 (and the 50 ms floor): still a match.
        assert_eq!(near.2, Check::Match);
        assert_eq!(far.2, Check::Mismatch);
        assert!(same.0 <= MAX_INEXACT);
        // A known hash mismatch costs a tenth (of the uncapped 1.0 here).
        let differs = score(
            &t,
            Path::new("/x/A014_C011.mov"),
            Some(&facts(12.012, 3840)),
            Check::Mismatch,
        );
        assert!((differs.0 - 0.9).abs() < 1e-6 && differs.0 < same.0);
    }

    #[test]
    fn folder_moves() {
        let mv = FolderMove::infer(
            Path::new("/Volumes/Card A/Clip/A014.mov"),
            Path::new("/Volumes/SSD/Harbour/Card A/Clip/A014.mov"),
        )
        .expect("moved");
        assert_eq!(mv.from, PathBuf::from("/Volumes"));
        assert_eq!(mv.to, PathBuf::from("/Volumes/SSD/Harbour"));
        assert_eq!(
            mv.apply(Path::new("/Volumes/Card B/B002.mp4")),
            Some(PathBuf::from("/Volumes/SSD/Harbour/Card B/B002.mp4"))
        );
        assert_eq!(mv.apply(Path::new("/Users/me/x.mov")), None);
        // A renamed file: the move is between the parents.
        let mv =
            FolderMove::infer(Path::new("/a/b/one.mov"), Path::new("/c/two.mov")).expect("moved");
        assert_eq!(
            (mv.from, mv.to),
            (PathBuf::from("/a/b"), PathBuf::from("/c"))
        );
        assert_eq!(
            FolderMove::infer(Path::new("/a/x.mov"), Path::new("/a/y.mov")),
            None
        );
        assert_eq!(
            FolderMove::infer(Path::new("/a/x.mov"), Path::new("/a/x.mov")),
            None
        );
    }
}
