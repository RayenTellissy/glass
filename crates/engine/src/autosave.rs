//! The autosave thread: `glass-io` owns a [`project::autosave::AutosaveWriter`] for the
//! open project and records the latest root every [`INTERVAL`] when the tree changed.
//!
//! The UI thread keeps the undo history. After each edit (or once per frame) it calls
//! [`Autosave::submit`] with the current root and [`project::History::changes`]: an O(1)
//! clone of the root sent over a channel, which never waits for a write in progress. The
//! thread keeps only the latest root; on each tick it diffs it against the last recorded
//! state and appends the diff (see `project::autosave` for the files). A save starts a
//! new generation ([`Autosave::saved`]); a normal quit removes the folder
//! ([`Autosave::close`]). If the app dies, the folder and its unclean-exit marker stay,
//! and the next launch offers to restore it (`app::recovery`).
//!
//! Where: `<cache>/autosave/<project uid>/`. Project settings (and with them the
//! per-project cache folder) do not exist yet, so the app passes its per-user cache
//! folder; when the project settings task adds the cache folder setting, the app passes
//! that folder instead and the launch scan also looks in the recent projects' folders.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use project::autosave::{self as files, AutosaveWriter};

pub use project::autosave::{AutosaveError, Recovered, RecoveryInfo, Tick};
pub use project::{EditLabel, History, Project};

/// The autosave period.
pub const INTERVAL: Duration = Duration::from_millis(files::INTERVAL_MS);

/// The thread's name, as the threading model in `docs/ARCHITECTURE.md` lists it.
pub const THREAD_NAME: &str = crate::threads::Role::Io.base_name();

/// The autosave folder of `project` inside the cache folder `cache`.
pub fn project_dir(cache: &Path, project: &Project) -> PathBuf {
    files::autosave_dir(cache, project.uid())
}

/// Milliseconds since the Unix epoch, the clock autosave files use.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// How to start autosaving one project.
#[derive(Clone, Debug)]
pub struct AutosaveConfig {
    /// The project's autosave folder ([`project_dir`]).
    pub dir: PathBuf,
    /// Time between ticks; [`INTERVAL`] in the app, shorter in tests.
    pub interval: Duration,
    /// When the project file was last saved, if ever.
    pub saved_at_ms: Option<u64>,
    /// Changes the first root already holds on top of the saved file (a restored
    /// autosave's edits), so a crash before the first tick still offers them.
    pub edits_since_save: u64,
}

impl AutosaveConfig {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            interval: INTERVAL,
            saved_at_ms: None,
            edits_since_save: 0,
        }
    }
}

/// What the thread reports, for the status bar and tests. Drain with `try_recv`.
#[derive(Clone, Debug, PartialEq)]
pub enum AutosaveEvent {
    /// The folder was opened and the first snapshot written.
    Started { took: Duration },
    /// A tick that found changes.
    Written { tick: Tick, took: Duration },
    /// The project was saved and a new generation started.
    Saved,
    /// Writing failed; the thread stops and the folder keeps what it had.
    Failed(String),
    /// A clean close removed the folder.
    Closed,
}

enum Message {
    Submit { root: Project, changes: u64 },
    Saved { root: Project, changes: u64 },
    Flush,
    Close,
    Stop,
}

/// The handle the UI thread holds while a project is open.
pub struct Autosave {
    sender: Sender<Message>,
    thread: Option<JoinHandle<()>>,
}

impl Autosave {
    /// Starts `glass-io` for `root`, whose history stands at `changes`. The first
    /// snapshot is written on the thread. Returns the handle and the event receiver.
    pub fn start(
        config: AutosaveConfig,
        root: Project,
        changes: u64,
    ) -> std::io::Result<(Self, Receiver<AutosaveEvent>)> {
        let (sender, messages) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let thread = crate::threads::spawn(crate::threads::Role::Io, None, move || {
            run(config, root, changes, messages, events_tx)
        })?;
        Ok((
            Self {
                sender,
                thread: Some(thread),
            },
            events,
        ))
    }

    /// Hands the current root to the thread. Never blocks: the root is an O(1) clone and
    /// the channel is unbounded; the thread keeps only the latest.
    pub fn submit(&self, root: &Project, changes: u64) {
        let _ = self.sender.send(Message::Submit {
            root: root.clone(),
            changes,
        });
    }

    /// The project was saved as `root`: the next recovery counts edits from here.
    pub fn saved(&self, root: &Project, changes: u64) {
        let _ = self.sender.send(Message::Saved {
            root: root.clone(),
            changes,
        });
    }

    /// Ticks now instead of at the next interval (tests, and before a risky operation).
    pub fn flush(&self) {
        let _ = self.sender.send(Message::Flush);
    }

    /// A normal quit or project close: the thread finishes what it was writing, removes
    /// the folder and exits. Waits for it (the app is quitting).
    pub fn close(mut self) {
        let _ = self.sender.send(Message::Close);
        self.join();
    }

    /// Stops the thread and keeps the folder with its unclean-exit marker, as a crash
    /// would. Waits for it.
    pub fn abandon(mut self) {
        let _ = self.sender.send(Message::Stop);
        self.join();
    }

    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Autosave {
    /// Dropping without [`Autosave::close`] keeps the folder (the thread stops after its
    /// current write); it does not wait.
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Stop);
    }
}

fn run(
    config: AutosaveConfig,
    root: Project,
    mut written_changes: u64,
    messages: Receiver<Message>,
    events: Sender<AutosaveEvent>,
) {
    let fail = |what: &str, error: files::AutosaveError| {
        tracing::error!("autosave {what} in {}: {error}", config.dir.display());
        let _ = events.send(AutosaveEvent::Failed(error.to_string()));
    };
    let started = Instant::now();
    let mut writer = match AutosaveWriter::open(
        &config.dir,
        &root,
        config.saved_at_ms,
        config.edits_since_save,
        now_ms(),
    ) {
        Ok(writer) => writer,
        Err(error) => return fail("could not start", error),
    };
    let _ = events.send(AutosaveEvent::Started {
        took: started.elapsed(),
    });
    let mut latest: Option<(Project, u64)> = None;
    let mut deadline = Instant::now() + config.interval;
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        let tick_now = match crate::threads::idle(|| messages.recv_timeout(wait)) {
            Ok(Message::Submit { root, changes }) => {
                latest = Some((root, changes));
                false
            }
            Ok(Message::Saved { root, changes }) => {
                latest = None;
                written_changes = changes;
                match writer.mark_saved(&root, now_ms()) {
                    Ok(()) => {
                        let _ = events.send(AutosaveEvent::Saved);
                    }
                    Err(error) => return fail("could not start a generation", error),
                }
                false
            }
            Ok(Message::Flush) => true,
            Ok(Message::Close) => {
                match writer.close() {
                    Ok(()) => {
                        let _ = events.send(AutosaveEvent::Closed);
                    }
                    Err(error) => fail("could not remove its folder", error),
                }
                return;
            }
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                deadline = Instant::now() + config.interval;
                true
            }
        };
        // Only the latest root matters: skip ahead to it before writing.
        if !tick_now {
            continue;
        }
        let Some((root, changes)) = latest.take() else {
            continue;
        };
        let started = Instant::now();
        let edits = changes.saturating_sub(written_changes);
        match writer.tick(&root, edits, now_ms()) {
            Ok(Tick::Unchanged) => {}
            Ok(tick) => {
                written_changes = changes;
                let took = started.elapsed();
                tracing::debug!("autosave {tick:?} in {took:?}");
                let _ = events.send(AutosaveEvent::Written { tick, took });
            }
            Err(error) => return fail("could not write", error),
        }
    }
}

/// The launch scan: deletes autosaves older than seven days, then rebuilds every
/// autosave under `cache` that an unclean exit left with unsaved edits, oldest folder
/// name first. Unreadable ones are logged and left on disk for the user to inspect.
pub fn launch_scan(cache: &Path, now_ms: u64) -> Vec<Recovered> {
    match files::prune(cache, now_ms) {
        Ok(0) => {}
        Ok(n) => tracing::info!("removed {n} autosaves older than seven days"),
        Err(error) => tracing::warn!("could not prune old autosaves: {error}"),
    }
    let pending = match files::pending(cache) {
        Ok(pending) => pending,
        Err(error) => {
            tracing::warn!(
                "could not look for autosaves in {}: {error}",
                cache.display()
            );
            return Vec::new();
        }
    };
    let mut found = Vec::new();
    for (dir, info) in pending {
        match info.and_then(|_| files::recover(&dir)) {
            Ok(recovered) => {
                if recovered.dropped_records > 0 || recovered.fell_back {
                    tracing::warn!(
                        "autosave {}: {} damaged records left out{}",
                        dir.display(),
                        recovered.dropped_records,
                        if recovered.fell_back {
                            ", older generation used"
                        } else {
                            ""
                        }
                    );
                }
                found.push(recovered);
            }
            Err(error) => tracing::warn!("autosave {} cannot be read: {error}", dir.display()),
        }
    }
    found
}

/// Restoring an autosave: the recovered root as the start of a new undo history, and the
/// autosave settings that keep counting its unsaved edits.
pub fn restore(recovered: Recovered) -> (History, AutosaveConfig) {
    let config = AutosaveConfig {
        saved_at_ms: recovered.info.saved_at_ms,
        edits_since_save: recovered.info.edits_since_save,
        ..AutosaveConfig::new(recovered.info.dir.clone())
    };
    (History::new(recovered.project), config)
}

/// Discarding an autosave: removes its folder.
pub fn discard(recovered: &Recovered) -> Result<(), AutosaveError> {
    files::discard(&recovered.info.dir)
}
