//! Typed errors for edit operations.

use crate::id::{ItemId, MarkerId, MediaId, SequenceId, TrackId};
use crate::model::TrackKind;
use crate::time::{Time, TimeError};

/// Why an edit was rejected. A rejected edit leaves the project unchanged (edits return a
/// new root and never modify the old one).
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum EditError {
    #[error("no sequence {0}")]
    UnknownSequence(SequenceId),
    #[error("no track {0}")]
    UnknownTrack(TrackId),
    #[error("no item {0}")]
    UnknownItem(ItemId),
    #[error("no media {0}")]
    UnknownMedia(MediaId),
    #[error("no marker {0}")]
    UnknownMarker(MarkerId),
    #[error("item {0} is not a clip")]
    NotAClip(ItemId),
    #[error("a {kind:?} track cannot hold a {item} item")]
    ItemNotAllowed { kind: TrackKind, item: &'static str },
    #[error("item would overlap item {other} on track {track}")]
    Overlap { track: TrackId, other: ItemId },
    #[error("{time} s is not on the {grid} s grid of a {kind:?} track")]
    OffGrid {
        time: Time,
        grid: Time,
        kind: TrackKind,
    },
    #[error("duration must be positive, got {0} s")]
    EmptyRange(Time),
    #[error("items cannot start before zero, got {0} s")]
    NegativeStart(Time),
    #[error("markers cannot start before zero, got {0} s")]
    MarkerBeforeZero(Time),
    #[error("a marker's duration cannot be negative, got {0} s")]
    NegativeDuration(Time),
    #[error("{time} s is not on the {grid} s frame grid of the sequence")]
    MarkerOffGrid { time: Time, grid: Time },
    #[error("invalid frame rate {num}/{den}")]
    InvalidFrameRate { num: i32, den: i32 },
    #[error("invalid sample rate {0}")]
    InvalidSampleRate(u32),
    #[error("track index {index} out of range (sequence has {len} tracks)")]
    TrackIndex { index: usize, len: usize },
    #[error("an item cannot move from a {from:?} track to a {to:?} track")]
    TrackKindMismatch { from: TrackKind, to: TrackKind },
    #[error("source time would go before the start of the media: {0} s")]
    SourceBeforeStart(Time),
    #[error(transparent)]
    Time(#[from] TimeError),
}
