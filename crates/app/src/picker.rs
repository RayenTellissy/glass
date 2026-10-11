//! The system's open and save dialogs for project files, run like the import picker
//! (`ui::panels::file_dialog`): `rfd`'s asynchronous dialog on its own thread, the
//! chosen path back through a channel drained once per frame, so the UI thread never
//! waits on the dialog.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// The project file extension.
pub const EXTENSION: &str = "proj";

/// What the dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Open,
    Save,
}

/// An open dialog, or none.
#[derive(Debug, Default)]
pub struct ProjectPicker {
    pending: Option<(Purpose, Receiver<Option<PathBuf>>)>,
}

impl ProjectPicker {
    pub fn is_open(&self) -> bool {
        self.pending.is_some()
    }

    /// Opens the dialog unless one is already open. `name` is the file name a save
    /// dialog suggests.
    pub fn open(&mut self, purpose: Purpose, name: &str) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let title = match purpose {
            Purpose::Open => ui::t!("app.picker.open_title"),
            Purpose::Save => ui::t!("app.picker.save_title"),
        };
        let filter = ui::t!("app.picker.filter");
        let file_name = format!("{name}.{EXTENSION}");
        let spawned = std::thread::Builder::new()
            .name("glass-project-picker".into())
            .spawn(move || {
                let dialog = rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .add_filter(filter, &[EXTENSION]);
                let chosen = match purpose {
                    Purpose::Open => pollster::block_on(dialog.pick_file()),
                    Purpose::Save => {
                        pollster::block_on(dialog.set_file_name(file_name).save_file())
                    }
                };
                let _ = tx.send(chosen.map(|f| f.path().to_path_buf()));
            });
        match spawned {
            Ok(_) => self.pending = Some((purpose, rx)),
            Err(error) => tracing::error!(%error, "could not start the project picker thread"),
        }
    }

    /// The dialog's purpose and the chosen path once it closes (`None` when cancelled).
    pub fn poll(&mut self) -> Option<(Purpose, Option<PathBuf>)> {
        let (purpose, rx) = self.pending.as_ref()?;
        let purpose = *purpose;
        match rx.try_recv() {
            Ok(path) => {
                self.pending = None;
                Some((purpose, path))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                Some((purpose, None))
            }
        }
    }
}
