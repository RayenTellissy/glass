//! Stable node ids.
//!
//! Every node in a project (media reference, sequence, track, item, marker, effect, and
//! later transitions) gets a `u64` from the project's own counter, so ids are unique
//! across node kinds within a project, never reused, and stable across save and load.
//! Autosave diffs and undo compare nodes by these ids. Pasting into another project
//! remaps them.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Any node id, whatever its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

macro_rules! id_type {
    ($($(#[$doc:meta])+ $name:ident),* $(,)?) => {$(
        $(#[$doc])+
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            pub fn node(self) -> NodeId {
                NodeId(self.0)
            }
        }

        impl From<$name> for NodeId {
            fn from(id: $name) -> NodeId {
                NodeId(id.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "#{}", self.0)
            }
        }
    )*};
}

id_type!(
    /// A source media reference.
    MediaId,
    /// A sequence (timeline).
    SequenceId,
    /// A track in a sequence.
    TrackId,
    /// An item on a track: a clip, a subtitle or an adjustment item. Gaps are empty
    /// space between items and have no id.
    ItemId,
    /// A marker on a sequence or on a clip.
    MarkerId,
    /// A bin (folder) in the media pool.
    BinId,
    /// An effect instance in a clip's effect stack.
    EffectId,
);
