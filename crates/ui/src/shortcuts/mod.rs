//! Keyboard shortcuts: the command registry, keymaps (a preset plus the user's
//! overrides) with clash checks and settings storage, the four presets, and dispatch of
//! egui key events to command ids.
//!
//! Where the keymap file lives is the app's settings to decide; [`Keymap::load`] and
//! [`Keymap::save`] take a path. The editor for it is
//! [`crate::dialogs::shortcuts`].

pub mod commands;
pub mod dispatch;
pub mod keymap;
pub mod presets;

pub use commands::{COMMANDS, Command, Group, command};
pub use dispatch::Dispatcher;
pub use keymap::{Chord, Conflict, Keymap, KeymapError, Mods, Platform};
pub use presets::PresetId;
