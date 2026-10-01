//! The Glass document model: the immutable edit tree of sequences, tracks, clips,
//! transitions, keyframes and effect parameter schemas, plus undo history, save and
//! load, and schema migrations. Pure data: no I/O threads, no GPU, and no dependencies
//! on other Glass crates.

pub mod autosave;
pub mod diff;
pub mod edit;
pub mod error;
pub mod history;
pub mod id;
pub mod media_pool;
pub mod model;
pub mod params;
pub mod schema;
pub mod time;
pub mod validate;

pub use edit::ClipSpec;
pub use error::EditError;
pub use history::{EditLabel, History, MemoryStats};
pub use id::{BinId, ItemId, MediaId, NodeId, SequenceId, TrackId};
pub use model::{Clip, Item, ItemKind, MediaRef, MediaRefs, Project, Sequence, Track, TrackKind};
pub use schema::LoadError;
pub use time::{Rational, Time, TimeError, TimeRange};
pub use validate::ValidationError;
