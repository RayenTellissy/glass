//! Typed errors for edit operations.

use crate::id::{EffectId, ItemId, MarkerId, MediaId, SequenceId, TrackId, TransitionId};
use crate::keyframe::HandleSide;
use crate::model::TrackKind;
use crate::settings::CacheFolder;
use crate::time::{Rational, Time, TimeError};
use crate::transition::CutSide;

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
    #[error("invalid frame rate {num}/{den}: it must be from 1 to 240 frames per second")]
    InvalidFrameRate { num: i32, den: i32 },
    #[error("invalid sample rate {0}: it must be 44100, 48000 or 96000 Hz")]
    InvalidSampleRate(u32),
    #[error("invalid resolution {width}x{height}: both sides must be even, from 16 to 8192")]
    InvalidResolution { width: u32, height: u32 },
    #[error("drop-frame timecode needs 29.97 or 59.94 fps, not {0}")]
    DropFrameNotAllowed(Rational),
    #[error("{0:?} is not a valid cache folder")]
    InvalidCacheFolder(CacheFolder),
    #[error("track index {index} out of range (sequence has {len} tracks)")]
    TrackIndex { index: usize, len: usize },
    #[error("an item cannot move from a {from:?} track to a {to:?} track")]
    TrackKindMismatch { from: TrackKind, to: TrackKind },
    #[error("source time would go before the start of the media: {0} s")]
    SourceBeforeStart(Time),
    #[error("speed {0} is outside 1/100 to 100")]
    SpeedOutOfRange(Rational),
    #[error("speed {0} is not stored reduced with a positive denominator")]
    SpeedNotReduced(Rational),
    #[error("speed ramps are not supported yet: a speed map holds exactly one key at 0 s")]
    UnsupportedSpeedMap,
    #[error("{0} is not a finite number")]
    NotFinite(&'static str),
    #[error("{what} is {value}, outside {min} to {max}")]
    OutOfRange {
        what: &'static str,
        value: f64,
        min: f64,
        max: f64,
    },
    #[error("no effect {0}")]
    UnknownEffect(EffectId),
    #[error("effect schema {schema:?} is not valid: {reason}")]
    InvalidEffectSchema { schema: String, reason: String },
    #[error("effect {effect} follows schema {expected:?}, not {found:?}")]
    EffectSchemaMismatch {
        effect: EffectId,
        expected: String,
        found: String,
    },
    #[error("effect schema {schema:?} has no parameter {param:?}")]
    UnknownParam { schema: String, param: String },
    #[error("effect index {index} out of range (the stack holds {len})")]
    EffectIndex { index: usize, len: usize },
    #[error("{0} cannot be animated")]
    NotAnimatable(String),
    #[error("{0} is already animated")]
    AlreadyAnimated(String),
    #[error("{0} is not animated")]
    NotAnimated(String),
    #[error("a curve needs at least one key")]
    EmptyCurve,
    #[error("no key at {0} s")]
    NoKeyAt(Time),
    #[error("another key is already at {0} s")]
    KeyTimeTaken(Time),
    #[error("keys must be in time order without two at one time, but one is at {0} s")]
    KeysOutOfOrder(Time),
    #[error("keys cannot be before zero, got {0} s")]
    KeyBeforeZero(Time),
    #[error("a key value must be a {expected}, not a {found}")]
    KeyValueKind {
        expected: &'static str,
        found: &'static str,
    },
    #[error("a handle needs {expected} speeds, one per value component, not {found}")]
    HandleComponents { expected: usize, found: usize },
    #[error("the {side:?} handle of the key at {time} s is not part of a bezier")]
    NotBezier { time: Time, side: HandleSide },
    #[error("key times scale by a positive factor, not {0}")]
    TimeScale(Rational),
    #[error("parameter id {0:?} is not lowercase ASCII, digits, '.', '_' or '-'")]
    InvalidParamId(String),
    #[error("no transition {0}")]
    UnknownTransition(TransitionId),
    #[error("transition kind {0:?} is not lowercase ASCII letters, digits and '-'")]
    InvalidTransitionKind(String),
    #[error("direction {0:?} is not lowercase ASCII letters, digits, '_' and '-'")]
    InvalidDirection(String),
    #[error("easing {0:?} is not lowercase ASCII letters, digits and '-'")]
    InvalidEasing(String),
    #[error("a transition needs an outgoing clip, an incoming clip or both")]
    TransitionNeedsClip,
    #[error("clips {outgoing} and {incoming} do not meet at a cut on one track")]
    NotAdjacent { outgoing: ItemId, incoming: ItemId },
    #[error("item {item} meets item {other}: a transition from or to nothing needs a gap")]
    CutNotFree { item: ItemId, other: ItemId },
    #[error("the cut already has transition {0}")]
    TransitionExists(TransitionId),
    #[error("the transition needs {needed} s {side} the cut, but there is room for {available} s")]
    TransitionRoom {
        side: CutSide,
        needed: Time,
        available: Time,
    },
    #[error(
        "clip {item} has {available} s of media past its edge, but the transition needs {needed} s"
    )]
    TransitionHandle {
        item: ItemId,
        needed: Time,
        available: Time,
    },
    #[error("transition {transition} is a {expected:?}, not a {found:?}")]
    TransitionSchemaMismatch {
        transition: TransitionId,
        expected: String,
        found: String,
    },
    #[error("media {0} is relinked twice in one batch")]
    RelinkTwice(MediaId),
    #[error("media {0} cannot be relinked to an empty path")]
    RelinkNoPath(MediaId),
    #[error("the new file for media {media} is {found} s long, but its clips use {needed} s")]
    RelinkTooShort {
        media: MediaId,
        needed: Time,
        found: Time,
    },
    #[error(transparent)]
    Time(#[from] TimeError),
}
