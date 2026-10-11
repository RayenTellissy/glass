//! The app shell behind the `glass` binary: where Glass keeps its files, the settings
//! read before the window opens, the About details, launch recovery, opening and saving
//! projects, the [`state::AppState`] that owns the open project, its undo history,
//! playback and the keymap, and the eframe window ([`shell::Shell`]) with the docked
//! workspace. `main.rs` adds the crash reporter (the only unsafe code) and starts the
//! window.

pub mod about;
pub mod locations;
pub mod picker;
pub mod pool;
pub mod preview;
pub mod project_io;
pub mod recent;
pub mod recovery;
pub mod settings;
pub mod shell;
pub mod state;
