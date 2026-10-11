//! The recovery prompt at launch: finds autosaves an unclean exit left behind and acts on
//! what the user chooses in `ui::dialogs::recovery`.
//!
//! The launch scan (`engine::autosave::launch_scan`) runs before the window opens and
//! reads only the autosave folders: the per-user cache folder and the cache folders of
//! recently opened projects ([`crate::recent`]). Opening a project checks its own
//! autosave folder too ([`LaunchRecovery::for_project`]). The window shows one
//! [`LaunchRecovery`] dialog at a time and passes the choice to
//! [`LaunchRecovery::resolve`]:
//! Restore opens the recovered project with a fresh undo history and keeps autosaving
//! into the same folder (so its unsaved edits stay recoverable), Discard deletes the
//! folder, and Later keeps it for the next launch.

use std::path::{Path, PathBuf};

use engine::autosave::{self, AutosaveConfig, AutosaveError, History, Recovered};
use engine::project::autosave as project_autosave;
use ui::dialogs::recovery::{RecoveryAction, RecoveryDialog, RecoveryPrompt};

use crate::recent::Recent;

/// One autosave waiting for the user's decision, with its dialog.
pub struct LaunchRecovery {
    pub recovered: Recovered,
    pub prompt: RecoveryPrompt,
    pub dialog: RecoveryDialog,
    /// The project file the autosave belongs to, when known (it was saved and is in the
    /// recent list, or it was being opened).
    pub file: Option<PathBuf>,
}

/// What resolving the prompt led to.
pub enum Resolution {
    /// Open this project; its history starts at the recovered root, and autosave goes on
    /// with `autosave` (start it with `history.changes()`).
    Restored {
        history: History,
        autosave: AutosaveConfig,
    },
    /// The autosave was deleted.
    Discarded,
    /// Nothing decided; the autosave stays for next time.
    Kept,
    /// Show this folder in the file manager; the dialog stays open.
    ShowFolder(PathBuf),
}

impl LaunchRecovery {
    /// Every autosave under the cache folder that an unclean exit left with unsaved
    /// edits, as prompts to show one after another.
    pub fn scan(cache: &Path, now_ms: u64) -> Vec<Self> {
        autosave::launch_scan(cache, now_ms)
            .into_iter()
            .map(|recovered| Self::new(recovered, None, now_ms))
            .collect()
    }

    fn new(recovered: Recovered, file: Option<PathBuf>, now_ms: u64) -> Self {
        Self {
            prompt: RecoveryPrompt::from_recovered(&recovered, now_ms),
            recovered,
            dialog: RecoveryDialog::new(),
            file,
        }
    }

    /// [`LaunchRecovery::scan`] over the per-user cache folder and every recent
    /// project's cache folder, each autosave once, with its project file when the
    /// recent list names it.
    pub fn scan_all(user_cache: &Path, recent: &Recent, now_ms: u64) -> Vec<Self> {
        let mut caches = vec![user_cache];
        caches.extend(recent.caches().into_iter().filter(|c| *c != user_cache));
        let mut found: Vec<Self> = Vec::new();
        for cache in caches {
            for recovered in autosave::launch_scan(cache, now_ms) {
                if found
                    .iter()
                    .any(|f| f.recovered.info.dir == recovered.info.dir)
                {
                    continue;
                }
                let file = recent
                    .file_for_autosave(&recovered.info.dir)
                    .map(Path::to_path_buf);
                found.push(Self::new(recovered, file, now_ms));
            }
        }
        found
    }

    /// The autosave in `autosave_dir` that an unclean exit left while `file` was open,
    /// if any: checked when a project is opened.
    pub fn for_project(autosave_dir: &Path, file: &Path, now_ms: u64) -> Option<Self> {
        match project_autosave::find_recovery(autosave_dir) {
            Ok(Some(_)) => {}
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!("autosave {}: {error}", autosave_dir.display());
                return None;
            }
        }
        match project_autosave::recover(autosave_dir) {
            Ok(recovered) => Some(Self::new(recovered, Some(file.to_path_buf()), now_ms)),
            Err(error) => {
                tracing::warn!(
                    "autosave {} cannot be read: {error}",
                    autosave_dir.display()
                );
                None
            }
        }
    }

    /// Acts on the user's choice. `None` while the dialog stays up without a decision
    /// ([`RecoveryAction::CancelRestore`]: restoring is instant here, so there is nothing
    /// to cancel).
    pub fn resolve(&self, action: RecoveryAction) -> Result<Option<Resolution>, AutosaveError> {
        Ok(match action {
            RecoveryAction::Restore => {
                let (history, autosave) = autosave::restore(self.recovered.clone());
                Some(Resolution::Restored { history, autosave })
            }
            RecoveryAction::Discard => {
                autosave::discard(&self.recovered)?;
                Some(Resolution::Discarded)
            }
            RecoveryAction::Later => Some(Resolution::Kept),
            RecoveryAction::ShowFolder => {
                Some(Resolution::ShowFolder(self.recovered.info.dir.clone()))
            }
            RecoveryAction::CancelRestore => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use engine::autosave::{Autosave, AutosaveEvent, Project, Tick};

    use super::*;

    fn temp_cache(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("glass-app-recovery-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn written(events: &Receiver<AutosaveEvent>) {
        loop {
            match events.recv_timeout(Duration::from_secs(20)).expect("event") {
                AutosaveEvent::Written {
                    tick: Tick::Appended { .. },
                    ..
                } => return,
                AutosaveEvent::Started { .. } => {}
                other => panic!("{other:?}"),
            }
        }
    }

    /// A session that makes three edits and then dies without closing its autosave.
    fn crashed_session(cache: &Path) -> History {
        let p = Project::new("uid-launch", "Harbour film");
        let mut history = History::new(p.clone());
        let config = AutosaveConfig {
            interval: Duration::from_millis(20),
            ..AutosaveConfig::new(autosave::project_dir(cache, &p))
        };
        let (handle, events) = Autosave::start(config, p, 0).expect("start");
        for i in 0..3 {
            let next = history
                .current()
                .add_media(format!("hash{i}"), format!("/footage/{i}.mov"))
                .expect("media")
                .0;
            history.commit(engine::autosave::EditLabel::AddMedia, next);
            handle.submit(history.current(), history.changes());
            written(&events);
        }
        handle.abandon();
        history
    }

    #[test]
    fn a_crash_is_offered_at_launch_and_restores_the_last_autosave() {
        let cache = temp_cache("restore");
        let history = crashed_session(&cache);
        let now = autosave::now_ms();
        let found = LaunchRecovery::scan(&cache, now);
        assert_eq!(found.len(), 1);
        let launch = &found[0];
        assert_eq!(launch.prompt.project_name, "Harbour film");
        assert_eq!(
            launch.prompt.sequence_name,
            ui::t!("recovery.untitled_sequence")
        );
        assert_eq!(launch.prompt.sequence_duration, "0:00");
        assert_eq!(launch.prompt.edits, 3);

        let Some(Resolution::Restored {
            history: restored,
            autosave: config,
        }) = launch.resolve(RecoveryAction::Restore).expect("restore")
        else {
            panic!("expected a restored project");
        };
        assert_eq!(restored.current(), history.current());
        assert_eq!(config.edits_since_save, 3);
        // Autosaving the restored project into the same folder keeps it recoverable
        // until a clean close.
        let changes = restored.changes();
        let (handle, events) =
            Autosave::start(config, restored.current().clone(), changes).expect("start");
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(20)).expect("event"),
            AutosaveEvent::Started { .. }
        ));
        assert_eq!(LaunchRecovery::scan(&cache, now).len(), 1);
        handle.close();
        assert!(LaunchRecovery::scan(&cache, now).is_empty());
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn discard_deletes_and_later_keeps() {
        let cache = temp_cache("discard");
        crashed_session(&cache);
        let now = autosave::now_ms();
        let found = LaunchRecovery::scan(&cache, now);
        assert!(matches!(
            found[0].resolve(RecoveryAction::Later).expect("later"),
            Some(Resolution::Kept)
        ));
        assert!(matches!(
            found[0].resolve(RecoveryAction::ShowFolder).expect("folder"),
            Some(Resolution::ShowFolder(dir)) if dir == found[0].recovered.info.dir
        ));
        assert!(
            found[0]
                .resolve(RecoveryAction::CancelRestore)
                .expect("cancel")
                .is_none()
        );
        assert_eq!(LaunchRecovery::scan(&cache, now).len(), 1);
        assert!(matches!(
            found[0].resolve(RecoveryAction::Discard).expect("discard"),
            Some(Resolution::Discarded)
        ));
        assert!(LaunchRecovery::scan(&cache, now).is_empty());
        let _ = std::fs::remove_dir_all(&cache);
    }
}
