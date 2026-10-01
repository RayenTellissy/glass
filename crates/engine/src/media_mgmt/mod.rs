//! Media management: importing files, folders and camera cards into the media pool,
//! reading camera sidecars, relinking moved media and standing in for offline media.
//! Everything that touches the disk runs off the UI thread and reports back through
//! channels the UI drains once per frame.

pub mod cards;
pub mod classify;
pub mod import;
pub mod offline;
pub mod scan;
pub mod sidecar;
pub mod tags;

pub use cards::{Card, CardClip, CardKind, CardOptions, Sidecar, SidecarKind};
pub use classify::{DropSummary, FileKind, MediaClass, Unreadable, classify};
pub use import::{
    AfterImport, ImportEvent, ImportOptions, ImportSummary, ImportedClip, Importer,
    SceneDetectionOnImport, SkipReason,
};
pub use scan::{CardSummary, ImportPlan, ImportSource, PlannedFolder, ScanOptions};
