//! The Glass document model: the immutable edit tree of sequences, tracks, clips,
//! markers, transitions, keyframes and effect parameter schemas, plus undo history, save
//! and load, and schema migrations. Pure data: no I/O threads, no GPU, and no dependencies
//! on other Glass crates.

pub mod autosave;
pub mod clip;
pub mod diff;
pub mod edit;
pub mod error;
pub mod history;
pub mod id;
pub mod marker;
pub mod media_pool;
pub mod model;
pub mod params;
pub mod relink;
pub mod schema;
pub mod settings;
pub mod subtitle_export;
pub mod time;
pub mod transition;
pub mod validate;

pub use clip::{BlendMode, Crop, Effect, SpeedKeep, SpeedKey, SpeedMap, Transform};
pub use edit::ClipSpec;
pub use error::EditError;
pub use history::{EditLabel, History, MemoryStats};
pub use id::{
    BinId, EffectId, ItemId, MarkerId, MediaId, NodeId, SequenceId, TrackId, TransitionId,
};
pub use marker::{Marker, MarkerOwner, MarkerSpec, TodoState};
pub use media_pool::ColourLabel;
pub use model::{
    Adjustment, Clip, Item, ItemKind, MediaRef, MediaRefs, Project, Sequence, Subtitle, Track,
    TrackKind,
};
pub use relink::Relink;
pub use schema::LoadError;
pub use settings::{
    CacheFolder, ChannelLayout, ColourSpace, ProjectSettings, Resolution, SequenceFormat,
    WorkingPrimaries,
};
pub use time::{Rational, Time, TimeError, TimeRange};
pub use transition::{
    Alignment, CutSide, HandleNeeds, Segment, Transition, TransitionAt, TransitionSpec,
};
pub use validate::ValidationError;
