//! Autosave: an append-only log of diffs with periodic full snapshots, and recovery.
//!
//! One folder per project, `<cache>/autosave/<project uid>/` ([`autosave_dir`]):
//!
//! - `snapshot-<g>.json`: the whole project at the start of generation `g`, written to a
//!   `.partial` name, flushed to disk and renamed, so a snapshot is either complete or
//!   absent.
//! - `log-<g>.jsonl`: a header line (`{"autosave_format":5,"generation":g}`), then one
//!   [`Record`] per line, each a [`Diff`] from the previous state, appended and flushed to
//!   disk (`sync_data`) before the next. A crash can only tear the last line; recovery
//!   ignores a torn or unreadable tail and reports it.
//! - `session.json`: the unclean-exit marker. Written when a session opens the folder and
//!   rewritten on every tick; removed with the folder by [`AutosaveWriter::close`] when
//!   the app quits normally. Finding it on launch means the last session did not end
//!   cleanly.
//!
//! Every file carries [`FORMAT_VERSION`], the version of this folder layout and record
//! format. It is separate from the project schema version: the snapshot embeds the
//! project in whatever schema the build writes, and an autosave of another format version
//! is not read (autosaves are never migrated; the saved project file is). Recovery reads a
//! snapshot's format version before the rest, so an older autosave is refused as
//! [`AutosaveError::Format`] rather than as damaged JSON. Version 2 began with project
//! schema v2 (`media` as `{"refs": [...]}`), whose snapshots version 1 builds cannot read
//! and the other way round; version 3 began with schema v3, version 4 with schema v4,
//! version 5 with schema v5.
//!
//! A new generation starts every [`SNAPSHOT_EVERY`] records and whenever the project is
//! saved; the previous generation is kept as a fallback, older ones are deleted.
//!
//! Threads: `project` starts none. The UI thread hands each new root (an O(1) clone) to
//! `glass-io` (`engine::autosave`), which owns the [`AutosaveWriter`] and calls
//! [`AutosaveWriter::tick`] every [`INTERVAL_MS`]. Diffing and writing never run on the UI
//! thread.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diff::{Diff, Flat, flatten, rebuild};
use crate::model::Project;
use crate::validate::ValidationError;

/// Autosave period.
pub const INTERVAL_MS: u64 = 10_000;
/// Records per generation before a new snapshot (five minutes of ticks with changes).
pub const SNAPSHOT_EVERY: u64 = 30;
/// Version of the autosave folder layout and record format (not the project schema).
/// Bumped whenever the snapshot's embedded project serialises differently. 3: project
/// schema v3 (markers, subtitle and adjustment tracks), with sequence markers as records
/// of their own (`diff::Node::Marker`). 4: project schema v4 (clip properties, which
/// travel inside their item's record). 5: project schema v5 (track transitions, as
/// records of their own, `diff::Node::Transition`).
pub const FORMAT_VERSION: u32 = 5;

/// How long the autosave of a session that did not quit normally is kept (the recovery
/// dialog promises seven days).
pub const KEEP_MS: u64 = 7 * 24 * 60 * 60 * 1000;

const SESSION: &str = "session.json";
const AUTOSAVE: &str = "autosave";

/// The folder holding every project's autosave folder inside a cache folder.
pub fn autosave_root(cache: &Path) -> PathBuf {
    cache.join(AUTOSAVE)
}

/// The autosave folder of a project inside its cache folder.
pub fn autosave_dir(cache: &Path, project_uid: &str) -> PathBuf {
    autosave_root(cache).join(project_uid)
}

#[derive(Debug, thiserror::Error)]
pub enum AutosaveError {
    #[error("autosave I/O: {0}")]
    Io(#[from] io::Error),
    #[error("autosave format: {0}")]
    Json(#[from] serde_json::Error),
    #[error("autosave format version {0} is not supported")]
    Format(u32),
    #[error("autosave does not rebuild into a valid project: {0}")]
    Invalid(#[from] ValidationError),
    #[error("no autosave snapshot could be read")]
    NoSnapshot,
}

#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    autosave_format: u32,
    generation: u64,
    written_at_ms: u64,
    /// When the project was last saved by the user, if ever.
    saved_at_ms: Option<u64>,
    /// Edits since the last save at the time of the snapshot.
    edits: u64,
    project: Project,
}

/// The version field every autosave file starts with, read on its own so a file of
/// another version is refused before its body is parsed.
#[derive(Debug, Deserialize)]
struct FormatVersion {
    autosave_format: u32,
}

/// The first line of a log.
#[derive(Debug, Serialize, Deserialize)]
struct LogHeader {
    autosave_format: u32,
    generation: u64,
}

/// One line of a log after its header.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    /// 1, 2, 3... within the generation.
    pub n: u64,
    pub at_ms: u64,
    /// Undo-history changes since the previous record.
    pub edits: u64,
    pub diff: Diff,
}

#[derive(Debug, Serialize, Deserialize)]
struct Session {
    autosave_format: u32,
    pid: u32,
    started_ms: u64,
    /// Last tick; the app was alive at least until then.
    alive_ms: u64,
}

/// What [`AutosaveWriter::tick`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// Nothing changed since the last tick.
    Unchanged,
    /// A diff was appended; `bytes` is the length of its log line.
    Appended {
        upserts: usize,
        removals: usize,
        bytes: usize,
    },
    /// A new generation started with a full snapshot of `bytes`.
    Snapshot { bytes: usize },
}

/// Writes the autosave folder of one open project. Owned by the `glass-io` thread.
#[derive(Debug)]
pub struct AutosaveWriter {
    dir: PathBuf,
    generation: u64,
    records: u64,
    /// The state the log has reached.
    flat: Flat,
    log: File,
    session: Session,
    saved_at_ms: Option<u64>,
    edits_since_save: u64,
}

impl AutosaveWriter {
    /// Starts autosaving `root` into `dir`, replacing whatever is there (check
    /// [`find_recovery`] first). `saved_at_ms` is when the file was last saved, if ever,
    /// and `edits_since_save` how many changes `root` already holds on top of it (a
    /// restored autosave holds some).
    pub fn open(
        dir: impl Into<PathBuf>,
        root: &Project,
        saved_at_ms: Option<u64>,
        edits_since_save: u64,
        now_ms: u64,
    ) -> Result<Self, AutosaveError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let previous = generations(&dir)?;
        let generation = previous.last().map_or(1, |g| g + 1);
        let session = Session {
            autosave_format: FORMAT_VERSION,
            pid: std::process::id(),
            started_ms: now_ms,
            alive_ms: now_ms,
        };
        write_atomic(&dir, SESSION, &serde_json::to_vec(&session)?)?;
        let (log, _) = start_generation(
            &dir,
            generation,
            root,
            saved_at_ms,
            edits_since_save,
            now_ms,
        )?;
        for g in previous {
            remove_generation(&dir, g)?;
        }
        Ok(Self {
            dir,
            generation,
            records: 0,
            flat: flatten(root),
            log,
            session,
            saved_at_ms,
            edits_since_save,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Records `root` if it differs from the last recorded state. `edits` is the number of
    /// undo-history changes since the previous tick.
    pub fn tick(&mut self, root: &Project, edits: u64, now_ms: u64) -> Result<Tick, AutosaveError> {
        self.session.alive_ms = now_ms;
        write_atomic(&self.dir, SESSION, &serde_json::to_vec(&self.session)?)?;
        let flat = flatten(root);
        let diff = Diff::between(&self.flat, &flat);
        if diff.is_empty() {
            return Ok(Tick::Unchanged);
        }
        self.edits_since_save += edits;
        if self.records >= SNAPSHOT_EVERY {
            let bytes = self.roll(root, now_ms)?;
            self.flat = flat;
            return Ok(Tick::Snapshot { bytes });
        }
        self.records += 1;
        let record = Record {
            n: self.records,
            at_ms: now_ms,
            edits,
            diff,
        };
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        self.log.write_all(&line)?;
        self.log.sync_data()?;
        self.flat = flat;
        Ok(Tick::Appended {
            upserts: record.diff.upserts.len(),
            removals: record.diff.removals.len(),
            bytes: line.len(),
        })
    }

    /// The user saved the project as `root`: start a new generation from it, so a later
    /// recovery counts edits from this save.
    pub fn mark_saved(&mut self, root: &Project, now_ms: u64) -> Result<(), AutosaveError> {
        self.edits_since_save = 0;
        self.saved_at_ms = Some(now_ms);
        self.roll(root, now_ms)?;
        self.flat = flatten(root);
        Ok(())
    }

    /// The app is quitting normally: nothing needs recovering, remove the folder.
    pub fn close(self) -> Result<(), AutosaveError> {
        discard(&self.dir)
    }

    /// Starts the next generation from `root`; returns the snapshot's size.
    fn roll(&mut self, root: &Project, now_ms: u64) -> Result<usize, AutosaveError> {
        let next = self.generation + 1;
        let (log, bytes) = start_generation(
            &self.dir,
            next,
            root,
            self.saved_at_ms,
            self.edits_since_save,
            now_ms,
        )?;
        self.log = log;
        // Keep the generation before as a fallback.
        for g in generations(&self.dir)? {
            if g + 1 < next {
                remove_generation(&self.dir, g)?;
            }
        }
        self.generation = next;
        self.records = 0;
        Ok(bytes)
    }
}

/// Writes the snapshot of a new generation and creates its log with the header line.
/// Returns the open log and the snapshot's size.
fn start_generation(
    dir: &Path,
    generation: u64,
    root: &Project,
    saved_at_ms: Option<u64>,
    edits: u64,
    now_ms: u64,
) -> Result<(File, usize), AutosaveError> {
    let snapshot = Snapshot {
        autosave_format: FORMAT_VERSION,
        generation,
        written_at_ms: now_ms,
        saved_at_ms,
        edits,
        project: root.clone(),
    };
    let bytes = serde_json::to_vec(&snapshot)?;
    write_atomic(dir, &snapshot_name(generation), &bytes)?;
    let mut log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(dir.join(log_name(generation)))?;
    let mut header = serde_json::to_vec(&LogHeader {
        autosave_format: FORMAT_VERSION,
        generation,
    })?;
    header.push(b'\n');
    log.write_all(&header)?;
    log.sync_all()?;
    sync_dir(dir)?;
    Ok((log, bytes.len()))
}

fn snapshot_name(generation: u64) -> String {
    format!("snapshot-{generation:08}.json")
}

fn log_name(generation: u64) -> String {
    format!("log-{generation:08}.jsonl")
}

/// Generations with a snapshot in `dir`, ascending.
fn generations(dir: &Path) -> io::Result<Vec<u64>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(g) = name
            .strip_prefix("snapshot-")
            .and_then(|r| r.strip_suffix(".json"))
            .and_then(|g| g.parse().ok())
        {
            found.push(g);
        }
    }
    found.sort_unstable();
    Ok(found)
}

fn remove_generation(dir: &Path, generation: u64) -> io::Result<()> {
    for name in [log_name(generation), snapshot_name(generation)] {
        match fs::remove_file(dir.join(name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Writes `name` in `dir` so it is either the old file or the complete new one.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let partial = dir.join(format!("{name}.partial"));
    let mut file = File::create(&partial)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&partial, dir.join(name))?;
    sync_dir(dir)
}

/// Flushes a folder's entries (new and renamed names). Windows cannot open a folder as a
/// file; NTFS journals the rename itself.
fn sync_dir(dir: &Path) -> io::Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    File::open(dir)?.sync_all()
}

fn read_session(dir: &Path) -> Option<Session> {
    fs::read(dir.join(SESSION))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// Removes an autosave folder (the user discarded it, or quit normally). A missing
/// folder is fine.
pub fn discard(dir: &Path) -> Result<(), AutosaveError> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Deletes the autosave folders under `<cache>/autosave/` whose session was last alive
/// more than [`KEEP_MS`] before `now_ms`. Folders it cannot date are left alone. Returns
/// how many were deleted.
pub fn prune(cache: &Path, now_ms: u64) -> Result<usize, AutosaveError> {
    let mut removed = 0;
    for dir in project_dirs(cache)? {
        if read_session(&dir).is_some_and(|s| now_ms.saturating_sub(s.alive_ms) > KEEP_MS) {
            fs::remove_dir_all(&dir)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// The project folders under `<cache>/autosave/`, sorted.
fn project_dirs(cache: &Path) -> Result<Vec<PathBuf>, AutosaveError> {
    let entries = match fs::read_dir(autosave_root(cache)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

/// What an unclean previous session left behind.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryInfo {
    pub dir: PathBuf,
    pub project_name: String,
    /// Time of the newest readable state.
    pub autosaved_at_ms: u64,
    /// When the app was last known alive.
    pub alive_ms: u64,
    pub saved_at_ms: Option<u64>,
    /// Undo-history changes since the last save.
    pub edits_since_save: u64,
}

/// The recovered project and how it was reached.
#[derive(Clone, Debug)]
pub struct Recovered {
    pub project: Project,
    pub info: RecoveryInfo,
    /// Log records that could not be read (a torn last line, or damage) and were left out.
    pub dropped_records: usize,
    /// True when the newest generation was unreadable and an older one was used.
    pub fell_back: bool,
}

/// Looks for an autosave left by a session that did not quit normally. `None` when the
/// folder is absent, the last session closed cleanly, or nothing changed since the last
/// save.
pub fn find_recovery(dir: &Path) -> Result<Option<RecoveryInfo>, AutosaveError> {
    if !dir.join(SESSION).exists() {
        return Ok(None);
    }
    let recovered = recover(dir)?;
    Ok((recovered.info.edits_since_save > 0).then_some(recovered.info))
}

/// An autosave folder found by [`pending`]: recoverable, or marked unclean but unreadable.
pub type Pending = (PathBuf, Result<RecoveryInfo, AutosaveError>);

/// Every autosave under `<cache>/autosave/` that [`find_recovery`] would offer, plus the
/// folders that have an unclean-exit marker but could not be read (with why). Folders of
/// sessions that closed cleanly or hold no unsaved edits are left out.
pub fn pending(cache: &Path) -> Result<Vec<Pending>, AutosaveError> {
    let mut found = Vec::new();
    for dir in project_dirs(cache)? {
        match find_recovery(&dir) {
            Ok(None) => {}
            Ok(Some(info)) => found.push((dir, Ok(info))),
            Err(error) => found.push((dir, Err(error))),
        }
    }
    Ok(found)
}

/// Rebuilds the newest readable state from the folder: the newest generation whose
/// snapshot reads and validates, plus every log record up to the first unreadable one.
pub fn recover(dir: &Path) -> Result<Recovered, AutosaveError> {
    let session = read_session(dir);
    let mut fell_back = false;
    let mut last_error = AutosaveError::NoSnapshot;
    for generation in generations(dir)?.into_iter().rev() {
        match recover_generation(dir, generation) {
            Ok((project, mut info, dropped)) => {
                info.alive_ms = session.as_ref().map_or(info.autosaved_at_ms, |s| {
                    s.alive_ms.max(info.autosaved_at_ms)
                });
                return Ok(Recovered {
                    project,
                    info,
                    dropped_records: dropped,
                    fell_back,
                });
            }
            Err(e) => {
                last_error = e;
                fell_back = true;
            }
        }
    }
    Err(last_error)
}

fn recover_generation(
    dir: &Path,
    generation: u64,
) -> Result<(Project, RecoveryInfo, usize), AutosaveError> {
    let bytes = fs::read(dir.join(snapshot_name(generation)))?;
    let version = serde_json::from_slice::<FormatVersion>(&bytes)?.autosave_format;
    if version != FORMAT_VERSION {
        return Err(AutosaveError::Format(version));
    }
    let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
    snapshot.project.validate()?;
    let mut flat = flatten(&snapshot.project);
    let mut project = snapshot.project;
    let mut info = RecoveryInfo {
        dir: dir.to_path_buf(),
        project_name: project.name.clone(),
        autosaved_at_ms: snapshot.written_at_ms,
        alive_ms: snapshot.written_at_ms,
        saved_at_ms: snapshot.saved_at_ms,
        edits_since_save: snapshot.edits,
    };
    let mut dropped = 0;
    if let Ok(file) = File::open(dir.join(log_name(generation))) {
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let header_ok = reader.read_until(b'\n', &mut line)? > 0
            && line.last() == Some(&b'\n')
            && serde_json::from_slice::<LogHeader>(&line)
                .is_ok_and(|h| h.autosave_format == FORMAT_VERSION && h.generation == generation);
        if !header_ok {
            // A log whose header is missing or foreign: trust none of its lines.
            dropped += reader.split(b'\n').count();
        } else {
            let mut expected = 1;
            loop {
                line.clear();
                if reader.read_until(b'\n', &mut line)? == 0 {
                    break;
                }
                // A line without its newline was torn by a crash mid-write.
                let complete = line.last() == Some(&b'\n');
                let record = complete
                    .then(|| serde_json::from_slice::<Record>(&line).ok())
                    .flatten()
                    .filter(|r| r.n == expected);
                let Some(record) = record else {
                    dropped += 1 + reader.split(b'\n').count();
                    break;
                };
                let mut next = flat.clone();
                record.diff.apply(&mut next);
                let Ok(rebuilt) = rebuild(&next) else {
                    dropped += 1 + reader.split(b'\n').count();
                    break;
                };
                flat = next;
                project = rebuilt;
                info.autosaved_at_ms = record.at_ms;
                info.edits_since_save += record.edits;
                expected += 1;
            }
        }
    }
    info.project_name = project.name.clone();
    Ok((project, info, dropped))
}
