//! The Glass user interface: egui panels and widgets for the timeline, preview, media
//! pool, inspector, browsers, colour, audio, export and settings, theme and i18n
//! application, and turning input into edit commands and engine requests.

pub mod brand;
pub mod dialogs;
pub mod fonts;
pub mod frame_time;
pub mod headless;
pub mod panels;
pub mod params;
pub mod shortcuts;
pub mod snapshot;
pub mod theme;

/// `t!("key")` looks up a user-facing string; see [`assets::i18n`].
pub use assets::t;
