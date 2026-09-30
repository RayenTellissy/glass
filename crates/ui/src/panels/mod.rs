//! Panels as self-contained egui widgets: each takes a view model (plain data the app
//! fills from the project and the engine) and returns what the user did, so it can be
//! docked anywhere once the app shell exists and tested headless now.

pub mod file_dialog;
pub mod format;
pub mod import;
pub mod media_pool;
pub mod widgets;
