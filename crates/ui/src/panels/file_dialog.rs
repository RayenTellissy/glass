//! The system file picker for importing, run so the UI thread never waits on it: the
//! dialog is `rfd`'s asynchronous one, driven on its own thread, and the chosen paths
//! come back through a channel [`FilePicker::poll`] drains once per frame.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use crate::t;

/// What to ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
    /// One or more media files.
    Media,
    /// A folder (or a camera card).
    Folder,
}

/// An open picker, or none.
#[derive(Debug, Default)]
pub struct FilePicker {
    pending: Option<Receiver<Vec<PathBuf>>>,
}

impl FilePicker {
    pub fn is_open(&self) -> bool {
        self.pending.is_some()
    }

    /// Opens the picker unless one is already open.
    pub fn open(&mut self, kind: PickKind) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let title = match kind {
            PickKind::Media => t!("import.picker.title_media"),
            PickKind::Folder => t!("import.picker.title_folder"),
        };
        let filter = t!("import.picker.filter");
        let spawned = std::thread::Builder::new()
            .name("glass-file-picker".into())
            .spawn(move || {
                let dialog = rfd::AsyncFileDialog::new().set_title(title);
                let paths: Vec<PathBuf> = match kind {
                    PickKind::Media => {
                        let extensions = engine::media_mgmt::classify::importable_extensions();
                        pollster::block_on(dialog.add_filter(filter, &extensions).pick_files())
                            .unwrap_or_default()
                            .iter()
                            .map(|f| f.path().to_path_buf())
                            .collect()
                    }
                    PickKind::Folder => pollster::block_on(dialog.pick_folders())
                        .unwrap_or_default()
                        .iter()
                        .map(|f| f.path().to_path_buf())
                        .collect(),
                };
                let _ = tx.send(paths);
            });
        match spawned {
            Ok(_) => self.pending = Some(rx),
            Err(error) => tracing::error!(%error, "could not start the file picker thread"),
        }
    }

    /// The chosen paths once the picker closes (empty when it was cancelled).
    pub fn poll(&mut self) -> Option<Vec<PathBuf>> {
        let rx = self.pending.as_ref()?;
        match rx.try_recv() {
            Ok(paths) => {
                self.pending = None;
                Some(paths)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                Some(Vec::new())
            }
        }
    }
}
