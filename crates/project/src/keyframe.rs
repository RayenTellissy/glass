//! Keyframe curves (schema v7, Decision 63): every animatable numeric parameter of a clip
//! (its transform, crop, opacity and gain, [`ClipParam`]) or of an effect in its stack
//! holds either its constant value or a [`Curve`] of keys, and the edits that change them.
//!
//! **Time base.** Key times are source time (seconds from the start of the clip's media
//! file), like clip markers and `source_start`. Keys therefore stay on the frame of media
//! they were set on through every clip edit: trimming either end, slipping, moving, and
//! changing the speed (keeping the source or the timeline range) never touch them, and a
//! key outside the clip's visible source range is kept and simply not reached.
//! [`Clip::source_time_at`] maps a timeline time to the source time to evaluate at, and
//! [`Clip::timeline_time_of`] maps a key back to the timeline.
//!
//! **Keys.** A curve's keys are sorted by time, unique, at or after zero, and all hold the
//! same kind of value: a float, int, point, angle or colour (booleans, choices and text do
//! not animate). Each key's [`Interpolation`] says how the value moves from it to the next
//! key: [`Interpolation::Linear`], [`Interpolation::Hold`] (the key's value until the next
//! key), or [`Interpolation::Bezier`] with this key's out handle and the next key's in
//! handle, After Effects style: each [`Handle`] is a speed per value component (value
//! units per second of source time, the slope of the curve at the key) and an influence
//! (how far into the segment the handle reaches, 0 to 1 of its duration). With duration
//! `D`, the cubic runs through `(t0 + influence_out × D, v0 + speed_out × influence_out ×
//! D)` and `(t1 − influence_in × D, v1 − speed_in × influence_in × D)`. Influences stay in
//! 0..1, so the time coordinate never goes backwards (the CSS `cubic-bezier` constraint)
//! and the curve stays a function of time however the keys move.
//!
//! **Evaluation** ([`Curve::value_at`]) is exact at keys, gives the first key's value
//! before it and the last key's after it, and interpolates each component on its own:
//! points per axis, colours per channel of their linear-light working-space RGBA (straight
//! alpha), angles as plain numbers (0 to 720 turns twice, never wrapped), ints rounded
//! after interpolating. Readers clamp the result through the parameter's range
//! ([`Clip::param_at`], [`Effect::param_at`]), so a bezier's overshoot never leaves it.
//!
//! **Constant or curve.** While a parameter has a curve its constant is ignored and kept
//! as it is (`set_clip_transform` and the other property edits still write constants).
//! [`Project::animate_param`] turns a constant into a curve with one key holding it;
//! [`Project::make_param_constant`] removes the curve, keeping its value at a given time;
//! removing a curve's only key does the same with that key's value.
//!
//! The edits follow [`crate::edit`]: pure, validated, sharing everything they do not touch.
//! Each copies only the clip's item and its track's spine (and the one effect node), and
//! an edit that changes nothing returns this root. Values given to an edit must be of the
//! parameter's kind and finite, and are clamped to its range; handle influences are
//! clamped to 0..1.
//!
//! The cubic-bezier solver here is the same as `render::transition::Easing`'s (`project`
//! may not depend on `render`); `crates/engine/tests/keyframe_easing.rs` checks that the
//! two agree.

use std::mem::discriminant;
use std::sync::Arc;

use imbl::OrdMap;
use serde::{Deserialize, Serialize};

use crate::clip::{
    Crop, Effect, MAX_CROP_PX, MAX_FEATHER_PX, MAX_GAIN_DB, MAX_OFFSET_PX, MAX_SCALE, MIN_GAIN_DB,
    Transform,
};
use crate::error::EditError;
use crate::id::{EffectId, ItemId};
use crate::model::{Clip, Project};
use crate::params::{ParamDef, ParamSchema, ParamValue, Range, Unit};
use crate::time::{Rational, Time};

/// The influence of an eased handle: a third of the segment, as After Effects' Easy Ease.
pub const EASE_INFLUENCE: f64 = 1.0 / 3.0;

/// One side of a bezier segment at a key. See the module docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Handle {
    /// The curve's slope at the key, in value units per second of source time, one per
    /// component of the value ([`components`]).
    pub speed: Vec<f64>,
    /// How far into the segment the handle reaches, as a fraction of its duration, 0 to 1.
    pub influence: f64,
}

impl Handle {
    pub fn new(speed: Vec<f64>, influence: f64) -> Self {
        Self { speed, influence }
    }

    /// A flat handle reaching a third of the segment: the curve eases in or out of the key.
    pub fn ease(components: usize) -> Self {
        Self::new(vec![0.0; components], EASE_INFLUENCE)
    }
}

/// How the value moves from a key to the next one. Serialised as `"linear"`, `"hold"` or
/// `{"bezier": {"out": {...}, "in": {...}}}`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    /// A straight line to the next key's value.
    #[default]
    Linear,
    /// This key's value until the next key.
    Hold,
    /// A cubic through this key's out handle and the next key's in handle.
    Bezier {
        out: Handle,
        #[serde(rename = "in")]
        into: Handle,
    },
}

impl Interpolation {
    /// A bezier easing out of this key and into the next one.
    pub fn ease(components: usize) -> Self {
        Self::Bezier {
            out: Handle::ease(components),
            into: Handle::ease(components),
        }
    }
}

/// One key of a [`Curve`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Key {
    /// Source time, seconds from the start of the clip's media file.
    pub time: Time,
    pub value: ParamValue,
    /// How the value moves from this key to the next one (unused on the last key).
    pub interpolation: Interpolation,
}

impl Key {
    /// A key with linear interpolation.
    pub fn new(time: Time, value: ParamValue) -> Self {
        Self {
            time,
            value,
            interpolation: Interpolation::Linear,
        }
    }
}

/// A parameter's animation: its keys in time order. See the module docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    pub keys: Vec<Key>,
}

impl Curve {
    /// A curve with a single key.
    pub fn single(time: Time, value: ParamValue) -> Self {
        Self {
            keys: vec![Key::new(time, value)],
        }
    }

    /// The key at exactly `time`.
    pub fn key_at(&self, time: Time) -> Option<&Key> {
        self.index_of(time).map(|i| &self.keys[i])
    }

    fn index_of(&self, time: Time) -> Option<usize> {
        self.keys.binary_search_by(|k| k.time.cmp(&time)).ok()
    }

    /// The number of components the keys' values have ([`components`]).
    pub fn components(&self) -> Option<usize> {
        components(&self.keys.first()?.value)
    }

    /// The value at source time `time`: a key's own value at its time, the first key's
    /// before it, the last key's after it, and the interpolation out of the key before
    /// `time` in between. `None` for a curve without keys. The result is not clamped to
    /// the parameter's range.
    pub fn value_at(&self, time: Time) -> Option<ParamValue> {
        let first = self.keys.first()?;
        let next = self.keys.partition_point(|k| k.time <= time);
        if next == 0 {
            return Some(first.value.clone());
        }
        let key = &self.keys[next - 1];
        match self.keys.get(next) {
            Some(after) if key.time != time => Some(segment_value(key, after, time)),
            _ => Some(key.value.clone()),
        }
    }

    /// The structural invariants every curve keeps: at least one key; times at or after
    /// zero, strictly increasing; values of one animatable kind, finite; bezier handles
    /// with one finite speed per component and an influence from 0 to 1.
    pub fn check(&self) -> Result<(), EditError> {
        let first = self.keys.first().ok_or(EditError::EmptyCurve)?;
        let n = components(&first.value).ok_or(EditError::KeyValueKind {
            expected: "a float, int, point, angle or colour",
            found: kind_name(&first.value),
        })?;
        let mut previous: Option<Time> = None;
        for key in &self.keys {
            check_key_time(key.time)?;
            if previous.is_some_and(|p| p >= key.time) {
                return Err(EditError::KeysOutOfOrder(key.time));
            }
            previous = Some(key.time);
            if !same_kind(&first.value, &key.value) {
                return Err(EditError::KeyValueKind {
                    expected: kind_name(&first.value),
                    found: kind_name(&key.value),
                });
            }
            check_finite(&key.value)?;
            if let Interpolation::Bezier { out, into } = &key.interpolation {
                for handle in [out, into] {
                    check_handle(handle, n)?;
                    if !(0.0..=1.0).contains(&handle.influence) {
                        return Err(EditError::OutOfRange {
                            what: "handle influence",
                            value: handle.influence,
                            min: 0.0,
                            max: 1.0,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// The value between key `a` and the next key `b` at `time`, strictly between their times.
fn segment_value(a: &Key, b: &Key, time: Time) -> ParamValue {
    let (Some((v0, n)), Some((v1, _))) = (parts(&a.value), parts(&b.value)) else {
        return a.value.clone();
    };
    if !same_kind(&a.value, &b.value) {
        return a.value.clone();
    }
    let duration = seconds_between(a.time, b.time);
    let x = (seconds_between(a.time, time) / duration).clamp(0.0, 1.0);
    let mut out = [0.0; 4];
    match &a.interpolation {
        Interpolation::Hold => return a.value.clone(),
        Interpolation::Linear => {
            for i in 0..n {
                out[i] = v0[i] + (v1[i] - v0[i]) * x;
            }
        }
        Interpolation::Bezier { out: from, into } => {
            let x1 = unit(from.influence);
            let x2 = 1.0 - unit(into.influence);
            let s = solve_x(x1, x2, x);
            for i in 0..n {
                let p1 = v0[i] + speed(from, i) * x1 * duration;
                let p2 = v1[i] - speed(into, i) * (1.0 - x2) * duration;
                out[i] = cubic(v0[i], p1, p2, v1[i], s);
            }
        }
    }
    rebuild(&a.value, out)
}

fn speed(handle: &Handle, i: usize) -> f64 {
    handle
        .speed
        .get(i)
        .copied()
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}

/// Seconds from `a` to `b`, exact before converting when the difference fits.
fn seconds_between(a: Time, b: Time) -> f64 {
    b.checked_sub(a)
        .map(Time::as_secs_f64)
        .unwrap_or_else(|_| b.as_secs_f64() - a.as_secs_f64())
}

fn cubic(p0: f64, p1: f64, p2: f64, p3: f64, s: f64) -> f64 {
    let u = 1.0 - s;
    u * u * u * p0 + 3.0 * u * u * s * p1 + 3.0 * u * s * s * p2 + s * s * s * p3
}

/// A CSS `cubic-bezier(x1, y1, x2, y2)` easing at progress `x`: 0 at or before 0, 1 at or
/// after 1, the control x clamped to 0..1. The same function as
/// `render::transition::Easing::CubicBezier`, bit for bit.
pub fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    if x.is_nan() || x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let (x1, x2) = (unit(x1), unit(x2));
    bezier(y1, y2, solve_x(x1, x2, x))
}

fn unit(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// One coordinate of the cubic from 0 through control values `c1`, `c2` to 1 at `s`.
fn bezier(c1: f64, c2: f64, s: f64) -> f64 {
    let u = 1.0 - s;
    3.0 * u * u * s * c1 + 3.0 * u * s * s * c2 + s * s * s
}

fn bezier_slope(c1: f64, c2: f64, s: f64) -> f64 {
    let u = 1.0 - s;
    3.0 * u * u * c1 + 6.0 * u * s * (c2 - c1) + 3.0 * s * s * (1.0 - c2)
}

/// The curve parameter whose x is `x`. With both control x in 0..1, x is monotonic in
/// the parameter, so Newton's method converges from `x` in a few steps; where the slope
/// vanishes (a flat start or end), bisection finishes the job.
fn solve_x(x1: f64, x2: f64, x: f64) -> f64 {
    const EPSILON: f64 = 1e-12;
    let mut s = x;
    for _ in 0..8 {
        let error = bezier(x1, x2, s) - x;
        if error.abs() < EPSILON {
            return s;
        }
        let slope = bezier_slope(x1, x2, s);
        if slope.abs() < 1e-9 {
            break;
        }
        s = (s - error / slope).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    s = x;
    for _ in 0..64 {
        let value = bezier(x1, x2, s);
        if (value - x).abs() < EPSILON {
            break;
        }
        if value < x {
            lo = s;
        } else {
            hi = s;
        }
        s = 0.5 * (lo + hi);
    }
    s
}

/// How many numbers a value interpolates: 1 for a float, int or angle, 2 for a point, 4
/// for a colour; `None` for a value that does not animate (boolean, choice, text).
pub fn components(value: &ParamValue) -> Option<usize> {
    parts(value).map(|(_, n)| n)
}

fn parts(value: &ParamValue) -> Option<([f64; 4], usize)> {
    Some(match value {
        ParamValue::Float(v) | ParamValue::Angle(v) => ([*v, 0.0, 0.0, 0.0], 1),
        ParamValue::Int(v) => ([*v as f64, 0.0, 0.0, 0.0], 1),
        ParamValue::Point(p) => ([p[0], p[1], 0.0, 0.0], 2),
        ParamValue::Colour(c) => (c.map(f64::from), 4),
        ParamValue::Bool(_) | ParamValue::Choice(_) | ParamValue::Text(_) => return None,
    })
}

/// A value of `like`'s kind from its components.
fn rebuild(like: &ParamValue, p: [f64; 4]) -> ParamValue {
    match like {
        ParamValue::Float(_) => ParamValue::Float(p[0]),
        ParamValue::Angle(_) => ParamValue::Angle(p[0]),
        ParamValue::Int(_) => ParamValue::Int(p[0].round() as i64),
        ParamValue::Point(_) => ParamValue::Point([p[0], p[1]]),
        ParamValue::Colour(_) => ParamValue::Colour(p.map(|v| v as f32)),
        other => other.clone(),
    }
}

fn same_kind(a: &ParamValue, b: &ParamValue) -> bool {
    discriminant(a) == discriminant(b)
}

fn kind_name(value: &ParamValue) -> &'static str {
    match value {
        ParamValue::Float(_) => "float",
        ParamValue::Int(_) => "int",
        ParamValue::Bool(_) => "bool",
        ParamValue::Choice(_) => "choice",
        ParamValue::Colour(_) => "colour",
        ParamValue::Point(_) => "point",
        ParamValue::Angle(_) => "angle",
        ParamValue::Text(_) => "text",
    }
}

fn check_finite(value: &ParamValue) -> Result<(), EditError> {
    match parts(value) {
        Some((p, n)) if p[..n].iter().all(|v| v.is_finite()) => Ok(()),
        Some(_) => Err(EditError::NotFinite("key value")),
        None => Ok(()),
    }
}

fn check_key_time(time: Time) -> Result<(), EditError> {
    if time.is_negative() {
        Err(EditError::KeyBeforeZero(time))
    } else {
        Ok(())
    }
}

/// One finite speed per component and a finite influence.
fn check_handle(handle: &Handle, n: usize) -> Result<(), EditError> {
    if handle.speed.len() != n {
        return Err(EditError::HandleComponents {
            expected: n,
            found: handle.speed.len(),
        });
    }
    if !handle.speed.iter().all(|v| v.is_finite()) {
        return Err(EditError::NotFinite("handle speed"));
    }
    if !handle.influence.is_finite() {
        return Err(EditError::NotFinite("handle influence"));
    }
    Ok(())
}

/// `handle` checked, its influence clamped to 0..1.
fn clamp_handle(handle: Handle, n: usize) -> Result<Handle, EditError> {
    check_handle(&handle, n)?;
    Ok(Handle {
        influence: handle.influence.clamp(0.0, 1.0),
        ..handle
    })
}

/// An animatable property of a clip itself. Serialised as its id, the key of the clip's
/// `keyframes` map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ClipParam {
    /// The transform's position offset, a point.
    #[serde(rename = "position")]
    Position,
    /// The transform's per-axis scale, a point.
    #[serde(rename = "scale")]
    Scale,
    /// The transform's rotation in degrees, an angle.
    #[serde(rename = "rotation")]
    Rotation,
    /// The transform's anchor offset, a point.
    #[serde(rename = "anchor")]
    Anchor,
    #[serde(rename = "crop.left")]
    CropLeft,
    #[serde(rename = "crop.top")]
    CropTop,
    #[serde(rename = "crop.right")]
    CropRight,
    #[serde(rename = "crop.bottom")]
    CropBottom,
    #[serde(rename = "crop.feather")]
    CropFeather,
    #[serde(rename = "opacity")]
    Opacity,
    /// Audio gain in dB.
    #[serde(rename = "gain_db")]
    GainDb,
}

impl ClipParam {
    pub const ALL: [ClipParam; 11] = [
        ClipParam::Position,
        ClipParam::Scale,
        ClipParam::Rotation,
        ClipParam::Anchor,
        ClipParam::CropLeft,
        ClipParam::CropTop,
        ClipParam::CropRight,
        ClipParam::CropBottom,
        ClipParam::CropFeather,
        ClipParam::Opacity,
        ClipParam::GainDb,
    ];

    /// The stable id, as in project files.
    pub fn id(self) -> &'static str {
        match self {
            ClipParam::Position => "position",
            ClipParam::Scale => "scale",
            ClipParam::Rotation => "rotation",
            ClipParam::Anchor => "anchor",
            ClipParam::CropLeft => "crop.left",
            ClipParam::CropTop => "crop.top",
            ClipParam::CropRight => "crop.right",
            ClipParam::CropBottom => "crop.bottom",
            ClipParam::CropFeather => "crop.feather",
            ClipParam::Opacity => "opacity",
            ClipParam::GainDb => "gain_db",
        }
    }

    pub fn from_id(id: &str) -> Option<ClipParam> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    /// The parameter's kind, range and default, as the clip property edits clamp it.
    pub fn def(self) -> ParamDef {
        let offset = Range::new(-MAX_OFFSET_PX, MAX_OFFSET_PX);
        let crop = |label: &str| {
            ParamDef::float(self.id(), label, 0.0, Range::new(0.0, MAX_CROP_PX)).unit(Unit::Pixels)
        };
        match self {
            ClipParam::Position => {
                ParamDef::point(self.id(), "inspector.transform.position", [0.0; 2]).range(offset)
            }
            ClipParam::Scale => ParamDef::point(self.id(), "inspector.transform.scale", [1.0; 2])
                .range(Range::new(-MAX_SCALE, MAX_SCALE))
                .unit(Unit::Percent),
            ClipParam::Rotation => ParamDef::angle(self.id(), "inspector.transform.rotation", 0.0),
            ClipParam::Anchor => {
                ParamDef::point(self.id(), "inspector.transform.anchor", [0.0; 2]).range(offset)
            }
            ClipParam::CropLeft => crop("inspector.crop.left"),
            ClipParam::CropTop => crop("inspector.crop.top"),
            ClipParam::CropRight => crop("inspector.crop.right"),
            ClipParam::CropBottom => crop("inspector.crop.bottom"),
            ClipParam::CropFeather => ParamDef::float(
                self.id(),
                "inspector.crop.feather",
                0.0,
                Range::new(0.0, MAX_FEATHER_PX),
            )
            .unit(Unit::Pixels),
            ClipParam::Opacity => ParamDef::float(
                self.id(),
                "inspector.compositing.opacity",
                1.0,
                Range::new(0.0, 1.0),
            )
            .unit(Unit::Percent),
            ClipParam::GainDb => ParamDef::float(
                self.id(),
                "inspector.audio.gain",
                0.0,
                Range::new(MIN_GAIN_DB, MAX_GAIN_DB),
            )
            .unit(Unit::Decibels),
        }
    }
}

impl Clip {
    /// The curve animating `param`, if it is animated.
    pub fn curve(&self, param: ClipParam) -> Option<&Arc<Curve>> {
        self.keyframes.get(&param)
    }

    /// The constant value of `param` (ignored while it has a curve).
    pub fn constant(&self, param: ClipParam) -> ParamValue {
        let t = &self.transform;
        let c = &self.crop;
        match param {
            ClipParam::Position => ParamValue::Point(t.position),
            ClipParam::Scale => ParamValue::Point(t.scale),
            ClipParam::Rotation => ParamValue::Angle(t.rotation),
            ClipParam::Anchor => ParamValue::Point(t.anchor),
            ClipParam::CropLeft => ParamValue::Float(c.left),
            ClipParam::CropTop => ParamValue::Float(c.top),
            ClipParam::CropRight => ParamValue::Float(c.right),
            ClipParam::CropBottom => ParamValue::Float(c.bottom),
            ClipParam::CropFeather => ParamValue::Float(c.feather),
            ClipParam::Opacity => ParamValue::Float(self.opacity),
            ClipParam::GainDb => ParamValue::Float(self.gain_db),
        }
    }

    /// Writes a constant of `param`'s kind; a value of another kind is ignored.
    fn set_constant(&mut self, param: ClipParam, value: &ParamValue) {
        match (param, value) {
            (ClipParam::Position, ParamValue::Point(p)) => self.transform.position = *p,
            (ClipParam::Scale, ParamValue::Point(p)) => self.transform.scale = *p,
            (ClipParam::Rotation, ParamValue::Angle(v)) => self.transform.rotation = *v,
            (ClipParam::Anchor, ParamValue::Point(p)) => self.transform.anchor = *p,
            (ClipParam::CropLeft, ParamValue::Float(v)) => self.crop.left = *v,
            (ClipParam::CropTop, ParamValue::Float(v)) => self.crop.top = *v,
            (ClipParam::CropRight, ParamValue::Float(v)) => self.crop.right = *v,
            (ClipParam::CropBottom, ParamValue::Float(v)) => self.crop.bottom = *v,
            (ClipParam::CropFeather, ParamValue::Float(v)) => self.crop.feather = *v,
            (ClipParam::Opacity, ParamValue::Float(v)) => self.opacity = *v,
            (ClipParam::GainDb, ParamValue::Float(v)) => self.gain_db = *v,
            _ => {}
        }
    }

    /// The value of `param` at `source_time`: its curve's value clamped to its range, or
    /// its constant.
    pub fn param_at(&self, param: ClipParam, source_time: Time) -> ParamValue {
        let constant = self.constant(param);
        match self.curve(param).and_then(|c| c.value_at(source_time)) {
            Some(v) if same_kind(&v, &constant) => param.def().sanitise(v),
            _ => constant,
        }
    }

    /// The transform at `source_time`, each animated field from its curve.
    pub fn transform_at(&self, source_time: Time) -> Transform {
        let point = |param, constant| match self.param_at(param, source_time) {
            ParamValue::Point(p) => p,
            _ => constant,
        };
        Transform {
            position: point(ClipParam::Position, self.transform.position),
            scale: point(ClipParam::Scale, self.transform.scale),
            rotation: self
                .param_at(ClipParam::Rotation, source_time)
                .as_f64()
                .unwrap_or(self.transform.rotation),
            anchor: point(ClipParam::Anchor, self.transform.anchor),
        }
    }

    /// The crop at `source_time`, each animated edge and the feather from its curve.
    pub fn crop_at(&self, source_time: Time) -> Crop {
        let float = |param: ClipParam, constant: f64| {
            self.param_at(param, source_time)
                .as_f64()
                .unwrap_or(constant)
        };
        Crop {
            left: float(ClipParam::CropLeft, self.crop.left),
            top: float(ClipParam::CropTop, self.crop.top),
            right: float(ClipParam::CropRight, self.crop.right),
            bottom: float(ClipParam::CropBottom, self.crop.bottom),
            feather: float(ClipParam::CropFeather, self.crop.feather),
        }
    }

    /// The opacity at `source_time`, 0 to 1.
    pub fn opacity_at(&self, source_time: Time) -> f64 {
        self.param_at(ClipParam::Opacity, source_time)
            .as_f64()
            .unwrap_or(self.opacity)
    }

    /// The gain in dB at `source_time`.
    pub fn gain_db_at(&self, source_time: Time) -> f64 {
        self.param_at(ClipParam::GainDb, source_time)
            .as_f64()
            .unwrap_or(self.gain_db)
    }

    /// The timeline time at which an item starting at `item_start` shows `source_time`
    /// (where a key at that source time plays): `item_start + (source_time − source_start)
    /// / speed`, exact.
    pub fn timeline_time_of(&self, item_start: Time, source_time: Time) -> Result<Time, EditError> {
        let speed = self.speed.speed()?;
        let offset = source_time
            .checked_sub(self.source_start)?
            .mul_ratio(i64::from(speed.den), i64::from(speed.num))?;
        Ok(item_start.checked_add(offset)?)
    }
}

impl Effect {
    /// The curve animating parameter `param`, if it is animated.
    pub fn curve(&self, param: &str) -> Option<&Arc<Curve>> {
        self.keyframes.get(param)
    }

    /// The value of `param` at `source_time`, made valid for `schema` (the effect's own):
    /// its curve's value when the parameter is keyframeable and has one, else its
    /// constant. `None` for a parameter the schema does not have.
    pub fn param_at(
        &self,
        schema: &ParamSchema,
        param: &str,
        source_time: Time,
    ) -> Option<ParamValue> {
        let def = schema.get(param)?;
        let animated = (def.keyframeable && def.kind.interpolates())
            .then(|| self.curve(param)?.value_at(source_time))
            .flatten()
            .filter(|v| same_kind(v, &def.kind.default_value()));
        match animated {
            Some(v) => Some(def.sanitise(v)),
            None => self.params.get(schema, param),
        }
    }
}

/// The parameter a keyframe edit works on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KeyTarget<'a> {
    /// A property of the clip `item`.
    Clip { item: ItemId, param: ClipParam },
    /// Parameter `param` of `effect`, whose schema is `schema`.
    Effect {
        effect: EffectId,
        schema: &'a ParamSchema,
        param: &'a str,
    },
}

impl KeyTarget<'_> {
    /// The parameter's name in errors: the clip property's id, or `<effect schema
    /// id>/<param id>`.
    pub fn name(&self) -> String {
        match self {
            KeyTarget::Clip { param, .. } => param.id().to_owned(),
            KeyTarget::Effect { schema, param, .. } => format!("{}/{param}", schema.id),
        }
    }
}

/// Which handle of a key [`Project::set_key_handle`] sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HandleSide {
    /// The handle arriving at the key, held by the previous key's bezier.
    In,
    /// The handle leaving the key, held by its own bezier.
    Out,
}

/// A parameter being edited: its definition, its constant and its curve.
struct Slot {
    name: String,
    def: ParamDef,
    constant: ParamValue,
    curve: Option<Curve>,
}

impl Slot {
    fn animatable(&self) -> Result<(), EditError> {
        if self.def.keyframeable && self.def.kind.interpolates() {
            Ok(())
        } else {
            Err(EditError::NotAnimatable(self.name.clone()))
        }
    }

    /// The curve of an animatable, animated parameter.
    fn curve_mut(&mut self) -> Result<&mut Curve, EditError> {
        self.animatable()?;
        let name = &self.name;
        self.curve
            .as_mut()
            .ok_or_else(|| EditError::NotAnimated(name.clone()))
    }

    /// `value` of the parameter's kind and finite, else refused.
    fn check_value(&self, value: &ParamValue) -> Result<(), EditError> {
        let expected = self.def.kind.default_value();
        if !same_kind(value, &expected) {
            return Err(EditError::KeyValueKind {
                expected: kind_name(&expected),
                found: kind_name(value),
            });
        }
        check_finite(value)
    }

    /// `value` checked and clamped to the parameter's range.
    fn key_value(&self, value: ParamValue) -> Result<ParamValue, EditError> {
        self.check_value(&value)?;
        Ok(self.def.sanitise(value))
    }

    /// The constant becomes the curve's value at `time`.
    fn keep_value_at(&mut self, curve: &Curve, time: Time) {
        if let Some(v) = curve.value_at(time)
            && same_kind(&v, &self.constant)
        {
            self.constant = self.def.sanitise(v);
        }
    }
}

/// For each key of `curve`, whether `times` names it; refuses a time with no key.
fn selection(curve: &Curve, times: &[Time]) -> Result<Vec<bool>, EditError> {
    let mut selected = vec![false; curve.keys.len()];
    for &time in times {
        let i = curve.index_of(time).ok_or(EditError::NoKeyAt(time))?;
        selected[i] = true;
    }
    Ok(selected)
}

/// Multiplies the speeds of the handles at the selected keys (each one's out handle and
/// the in handle the previous key holds for it) by `factor`.
fn scale_speeds(keys: &mut [Key], selected: &[bool], factor: f64) {
    let len = keys.len();
    for i in 0..len {
        let next_selected = i + 1 < len && selected[i + 1];
        if let Interpolation::Bezier { out, into } = &mut keys[i].interpolation {
            if selected[i] {
                out.speed.iter_mut().for_each(|v| *v *= factor);
            }
            if next_selected {
                into.speed.iter_mut().for_each(|v| *v *= factor);
            }
        }
    }
}

/// `curve`'s keys sorted by time, refusing two at one time.
fn sort_keys(keys: &mut [Key]) -> Result<(), EditError> {
    keys.sort_by_key(|k| k.time);
    match keys.windows(2).find(|w| w[0].time == w[1].time) {
        Some(w) => Err(EditError::KeyTimeTaken(w[1].time)),
        None => Ok(()),
    }
}

fn write_curve<K: Ord + Clone>(
    map: &mut OrdMap<K, Arc<Curve>>,
    key: K,
    old: Option<&Arc<Curve>>,
    new: Option<Curve>,
) {
    match new {
        None => {
            map.remove(&key);
        }
        Some(curve) if old.is_some_and(|o| **o == curve) => {}
        Some(curve) => {
            map.insert(key, Arc::new(curve));
        }
    }
}

/// A keyframe effect's id is lowercase ASCII, digits, `.`, `_` or `-`, like parameter ids.
pub(crate) fn check_param_id(id: &str) -> Result<(), EditError> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
    if valid {
        Ok(())
    } else {
        Err(EditError::InvalidParamId(id.to_owned()))
    }
}

/// The checks `Project::validate` runs on a clip's curves and its effects' curves: each
/// curve's invariants ([`Curve::check`]), clip curves holding their property's kind
/// within its range, and effect curves under valid parameter ids (effect values are
/// clamped through their schema when read).
pub(crate) fn check_clip_curves(clip: &Clip) -> Result<(), EditError> {
    for (param, curve) in &clip.keyframes {
        curve.check()?;
        let def = param.def();
        let expected = def.kind.default_value();
        for key in &curve.keys {
            if !same_kind(&key.value, &expected) {
                return Err(EditError::KeyValueKind {
                    expected: kind_name(&expected),
                    found: kind_name(&key.value),
                });
            }
            if let (Some(range), Some((p, n))) = (def.kind.range(), parts(&key.value))
                && let Some(&value) = p[..n].iter().find(|v| !range.contains(**v))
            {
                return Err(EditError::OutOfRange {
                    what: param.id(),
                    value,
                    min: range.min,
                    max: range.max,
                });
            }
        }
    }
    for effect in &clip.effects {
        for (id, curve) in &effect.keyframes {
            check_param_id(id)?;
            curve.check()?;
        }
    }
    Ok(())
}

impl Project {
    /// The value of the target parameter at `source_time` (its curve's, clamped, or its
    /// constant).
    pub fn param_value_at(
        &self,
        target: KeyTarget<'_>,
        source_time: Time,
    ) -> Result<ParamValue, EditError> {
        match target {
            KeyTarget::Clip { item, param } => {
                let (_, _, node) = self.find_item(item).ok_or(EditError::UnknownItem(item))?;
                let clip = node.clip().ok_or(EditError::NotAClip(item))?;
                Ok(clip.param_at(param, source_time))
            }
            KeyTarget::Effect {
                effect,
                schema,
                param,
            } => {
                let (_, node) = self
                    .find_effect(effect)
                    .ok_or(EditError::UnknownEffect(effect))?;
                check_effect_schema(effect, node, schema)?;
                node.param_at(schema, param, source_time)
                    .ok_or_else(|| unknown_param(schema, param))
            }
        }
    }

    /// The target parameter's curve, or `None` while it is constant.
    pub fn param_curve(&self, target: KeyTarget<'_>) -> Result<Option<Arc<Curve>>, EditError> {
        match target {
            KeyTarget::Clip { item, param } => {
                let (_, _, node) = self.find_item(item).ok_or(EditError::UnknownItem(item))?;
                let clip = node.clip().ok_or(EditError::NotAClip(item))?;
                Ok(clip.curve(param).cloned())
            }
            KeyTarget::Effect {
                effect,
                schema,
                param,
            } => {
                let (_, node) = self
                    .find_effect(effect)
                    .ok_or(EditError::UnknownEffect(effect))?;
                check_effect_schema(effect, node, schema)?;
                schema
                    .get(param)
                    .ok_or_else(|| unknown_param(schema, param))?;
                Ok(node.curve(param).cloned())
            }
        }
    }

    /// Turns a constant parameter into a curve with one linear key at source time `at`
    /// holding the constant. Refuses a parameter that cannot animate (not keyframeable,
    /// or a boolean, choice or text) or already has a curve.
    pub fn animate_param(&self, target: KeyTarget<'_>, at: Time) -> Result<Project, EditError> {
        check_key_time(at)?;
        self.edit_param(target, |slot| {
            slot.animatable()?;
            if slot.curve.is_some() {
                return Err(EditError::AlreadyAnimated(slot.name.clone()));
            }
            slot.curve = Some(Curve::single(at, slot.constant.clone()));
            Ok(())
        })
    }

    /// Removes the parameter's curve; its constant becomes the curve's value at source
    /// time `at`, clamped to its range.
    pub fn make_param_constant(
        &self,
        target: KeyTarget<'_>,
        at: Time,
    ) -> Result<Project, EditError> {
        self.edit_param(target, |slot| {
            let curve = slot
                .curve
                .take()
                .ok_or_else(|| EditError::NotAnimated(slot.name.clone()))?;
            slot.keep_value_at(&curve, at);
            Ok(())
        })
    }

    /// Sets the value of the key at source time `time`, or adds a linear key there.
    /// The value must be of the parameter's kind and finite; it is clamped to the range.
    pub fn set_key(
        &self,
        target: KeyTarget<'_>,
        time: Time,
        value: ParamValue,
    ) -> Result<Project, EditError> {
        check_key_time(time)?;
        self.edit_param(target, |slot| {
            slot.animatable()?;
            let value = slot.key_value(value)?;
            let curve = slot.curve_mut()?;
            match curve.index_of(time) {
                Some(i) => curve.keys[i].value = value,
                None => {
                    let at = curve.keys.partition_point(|k| k.time < time);
                    curve.keys.insert(at, Key::new(time, value));
                }
            }
            Ok(())
        })
    }

    /// Removes the key at `time`. Removing the only key makes the parameter constant at
    /// that key's value.
    pub fn remove_key(&self, target: KeyTarget<'_>, time: Time) -> Result<Project, EditError> {
        self.edit_param(target, |slot| {
            let curve = slot
                .curve
                .as_mut()
                .ok_or_else(|| EditError::NotAnimated(slot.name.clone()))?;
            let index = curve.index_of(time).ok_or(EditError::NoKeyAt(time))?;
            let key = curve.keys.remove(index);
            if curve.keys.is_empty() {
                slot.curve = None;
                slot.keep_value_at(&Curve::single(key.time, key.value), key.time);
            }
            Ok(())
        })
    }

    /// Moves the key at `from` to source time `to` with its value and interpolation,
    /// past other keys if need be. Refuses a time another key holds.
    pub fn move_key(
        &self,
        target: KeyTarget<'_>,
        from: Time,
        to: Time,
    ) -> Result<Project, EditError> {
        check_key_time(to)?;
        self.edit_param(target, |slot| {
            let curve = slot.curve_mut()?;
            let index = curve.index_of(from).ok_or(EditError::NoKeyAt(from))?;
            if from != to && curve.index_of(to).is_some() {
                return Err(EditError::KeyTimeTaken(to));
            }
            curve.keys[index].time = to;
            sort_keys(&mut curve.keys)
        })
    }

    /// Sets how the value moves from the key at `time` to the next one. A bezier's
    /// handles need one finite speed per component of the value; influences are clamped
    /// to 0..1.
    pub fn set_key_interpolation(
        &self,
        target: KeyTarget<'_>,
        time: Time,
        interpolation: Interpolation,
    ) -> Result<Project, EditError> {
        self.edit_param(target, |slot| {
            let curve = slot.curve_mut()?;
            let n = curve.components().unwrap_or(1);
            let index = curve.index_of(time).ok_or(EditError::NoKeyAt(time))?;
            let interpolation = match interpolation {
                Interpolation::Bezier { out, into } => Interpolation::Bezier {
                    out: clamp_handle(out, n)?,
                    into: clamp_handle(into, n)?,
                },
                other => other,
            };
            curve.keys[index].interpolation = interpolation;
            Ok(())
        })
    }

    /// Sets one handle of the key at `time`: its out handle (its own interpolation must be
    /// a bezier) or its in handle (the previous key's must be). The speed needs one finite
    /// number per component; the influence is clamped to 0..1.
    pub fn set_key_handle(
        &self,
        target: KeyTarget<'_>,
        time: Time,
        side: HandleSide,
        handle: Handle,
    ) -> Result<Project, EditError> {
        self.edit_param(target, |slot| {
            let curve = slot.curve_mut()?;
            let n = curve.components().unwrap_or(1);
            let handle = clamp_handle(handle, n)?;
            let index = curve.index_of(time).ok_or(EditError::NoKeyAt(time))?;
            let holder = match side {
                HandleSide::Out => Some(index),
                HandleSide::In => index.checked_sub(1),
            };
            let interpolation = holder.map(|i| &mut curve.keys[i].interpolation);
            match (interpolation, side) {
                (Some(Interpolation::Bezier { out, .. }), HandleSide::Out) => *out = handle,
                (Some(Interpolation::Bezier { into, .. }), HandleSide::In) => *into = handle,
                _ => return Err(EditError::NotBezier { time, side }),
            }
            Ok(())
        })
    }

    /// Scales the times of the keys at `times` about `anchor` by `factor` (positive):
    /// `anchor + (t − anchor) × factor`, exact. The keys keep their values; their handles
    /// keep their shape stretched (speeds divided by the factor, influences kept).
    /// Refuses a key landing before zero or on another key.
    pub fn scale_key_times(
        &self,
        target: KeyTarget<'_>,
        times: &[Time],
        anchor: Time,
        factor: Rational,
    ) -> Result<Project, EditError> {
        let positive = factor.num != 0 && factor.den != 0 && (factor.num > 0) == (factor.den > 0);
        if !positive {
            return Err(EditError::TimeScale(factor));
        }
        let ratio = f64::from(factor.num) / f64::from(factor.den);
        self.edit_param(target, |slot| {
            let curve = slot.curve_mut()?;
            let selected = selection(curve, times)?;
            let mut keys = curve.keys.clone();
            scale_speeds(&mut keys, &selected, 1.0 / ratio);
            for (key, _) in keys.iter_mut().zip(&selected).filter(|(_, s)| **s) {
                let offset = key.time.checked_sub(anchor)?.mul_rational(factor)?;
                key.time = anchor.checked_add(offset)?;
                check_key_time(key.time)?;
            }
            sort_keys(&mut keys)?;
            curve.keys = keys;
            Ok(())
        })
    }

    /// Scales the values of the keys at `times` about `anchor` (of the parameter's kind)
    /// by `factor`, per component: `anchor + (v − anchor) × factor`, clamped to the range.
    /// Their handles' speeds scale by the factor too.
    pub fn scale_key_values(
        &self,
        target: KeyTarget<'_>,
        times: &[Time],
        anchor: ParamValue,
        factor: f64,
    ) -> Result<Project, EditError> {
        if !factor.is_finite() {
            return Err(EditError::NotFinite("value scale"));
        }
        self.edit_param(target, |slot| {
            slot.animatable()?;
            slot.check_value(&anchor)?;
            let def = slot.def.clone();
            let curve = slot.curve_mut()?;
            let selected = selection(curve, times)?;
            let Some((a, n)) = parts(&anchor) else {
                return Ok(());
            };
            scale_speeds(&mut curve.keys, &selected, factor);
            for (key, _) in curve.keys.iter_mut().zip(&selected).filter(|(_, s)| **s) {
                let Some((mut v, _)) = parts(&key.value) else {
                    continue;
                };
                for i in 0..n {
                    v[i] = a[i] + (v[i] - a[i]) * factor;
                }
                key.value = def.sanitise(rebuild(&key.value, v));
            }
            Ok(())
        })
    }

    /// Applies `change` to the target parameter's definition, constant and curve, and
    /// writes back what changed (only the clip's item and, for an effect parameter, the
    /// effect node are copied). A change that leaves both equal returns this root.
    fn edit_param(
        &self,
        target: KeyTarget<'_>,
        change: impl FnOnce(&mut Slot) -> Result<(), EditError>,
    ) -> Result<Project, EditError> {
        let name = target.name();
        match target {
            KeyTarget::Clip { item, param } => self.change_clip(item, |clip| {
                let old = clip.keyframes.get(&param).cloned();
                let mut slot = Slot {
                    name,
                    def: param.def(),
                    constant: clip.constant(param),
                    curve: old.as_deref().cloned(),
                };
                change(&mut slot)?;
                clip.set_constant(param, &slot.constant);
                write_curve(&mut clip.keyframes, param, old.as_ref(), slot.curve);
                Ok(())
            }),
            KeyTarget::Effect {
                effect,
                schema,
                param,
            } => {
                let (item, index) = self.locate_effect(effect)?;
                self.change_clip(item, |clip| {
                    let node = &clip.effects[index];
                    check_effect_schema(effect, node, schema)?;
                    let def = schema
                        .get(param)
                        .ok_or_else(|| unknown_param(schema, param))?
                        .clone();
                    let constant = node
                        .params
                        .get(schema, param)
                        .ok_or_else(|| unknown_param(schema, param))?;
                    let old = node.keyframes.get(param).cloned();
                    let mut slot = Slot {
                        name,
                        def,
                        constant: constant.clone(),
                        curve: old.as_deref().cloned(),
                    };
                    change(&mut slot)?;
                    let constant_changed = slot.constant != constant;
                    let curve_changed = old.as_deref() != slot.curve.as_ref();
                    if constant_changed || curve_changed {
                        let node = Arc::make_mut(&mut clip.effects[index]);
                        if constant_changed {
                            node.params.set(schema, param, slot.constant);
                        }
                        write_curve(
                            &mut node.keyframes,
                            param.to_owned(),
                            old.as_ref(),
                            slot.curve,
                        );
                    }
                    Ok(())
                })
            }
        }
    }
}

fn check_effect_schema(
    effect: EffectId,
    node: &Effect,
    schema: &ParamSchema,
) -> Result<(), EditError> {
    if node.schema == schema.id {
        Ok(())
    } else {
        Err(EditError::EffectSchemaMismatch {
            effect,
            expected: node.schema.clone(),
            found: schema.id.clone(),
        })
    }
}

fn unknown_param(schema: &ParamSchema, param: &str) -> EditError {
    EditError::UnknownParam {
        schema: schema.id.clone(),
        param: param.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(num: i64, den: i64) -> Time {
        Time::new(num, den).expect("time")
    }

    fn float(v: f64) -> ParamValue {
        ParamValue::Float(v)
    }

    fn curve(keys: Vec<Key>) -> Curve {
        let c = Curve { keys };
        c.check().expect("valid curve");
        c
    }

    fn at(c: &Curve, time: Time) -> f64 {
        c.value_at(time).and_then(|v| v.as_f64()).expect("number")
    }

    #[test]
    fn linear_is_exact_at_keys_and_holds_outside() {
        let c = curve(vec![
            Key::new(Time::seconds(1), float(10.0)),
            Key::new(Time::seconds(3), float(30.0)),
        ]);
        assert_eq!(at(&c, Time::ZERO), 10.0);
        assert_eq!(at(&c, Time::seconds(1)), 10.0);
        assert_eq!(at(&c, Time::seconds(2)), 20.0);
        assert_eq!(at(&c, t(5, 2)), 25.0);
        assert_eq!(at(&c, Time::seconds(3)), 30.0);
        assert_eq!(at(&c, Time::seconds(9)), 30.0);
        assert_eq!(Curve { keys: vec![] }.value_at(Time::ZERO), None);
    }

    #[test]
    fn hold_keeps_the_value_until_the_next_key() {
        let mut first = Key::new(Time::ZERO, float(1.0));
        first.interpolation = Interpolation::Hold;
        let c = curve(vec![first, Key::new(Time::seconds(1), float(5.0))]);
        assert_eq!(at(&c, t(999, 1000)), 1.0);
        assert_eq!(at(&c, Time::seconds(1)), 5.0);
    }

    #[test]
    fn a_linear_bezier_is_a_line_and_an_ease_is_an_s() {
        // Speed equal to the segment's slope with a third of influence is a straight line.
        let mut a = Key::new(Time::ZERO, float(0.0));
        a.interpolation = Interpolation::Bezier {
            out: Handle::new(vec![10.0], EASE_INFLUENCE),
            into: Handle::new(vec![10.0], EASE_INFLUENCE),
        };
        let c = curve(vec![a, Key::new(Time::seconds(1), float(10.0))]);
        for i in 1..10 {
            let v = at(&c, t(i, 10));
            assert!((v - i as f64).abs() < 1e-9, "{i}: {v}");
        }
        let mut a = Key::new(Time::ZERO, float(0.0));
        a.interpolation = Interpolation::ease(1);
        let c = curve(vec![a, Key::new(Time::seconds(1), float(10.0))]);
        assert!((at(&c, t(1, 2)) - 5.0).abs() < 1e-9);
        assert!(at(&c, t(1, 10)) < 1.0);
        assert!(at(&c, t(9, 10)) > 9.0);
    }

    #[test]
    fn the_solver_matches_css_cubic_bezier_ends() {
        assert_eq!(cubic_bezier(0.42, 0.0, 0.58, 1.0, 0.0), 0.0);
        assert_eq!(cubic_bezier(0.42, 0.0, 0.58, 1.0, 1.0), 1.0);
        assert!((cubic_bezier(0.42, 0.0, 0.58, 1.0, 0.5) - 0.5).abs() < 1e-12);
        assert_eq!(cubic_bezier(0.42, 0.0, 0.58, 1.0, f64::NAN), 0.0);
    }

    #[test]
    fn colours_and_points_interpolate_per_component_and_ints_round() {
        let c = curve(vec![
            Key::new(Time::ZERO, ParamValue::Colour([0.0, 1.0, 0.5, 1.0])),
            Key::new(Time::seconds(2), ParamValue::Colour([1.0, 0.0, 0.5, 0.0])),
        ]);
        assert_eq!(
            c.value_at(Time::seconds(1)),
            Some(ParamValue::Colour([0.5, 0.5, 0.5, 0.5]))
        );
        let p = curve(vec![
            Key::new(Time::ZERO, ParamValue::Point([0.0, 100.0])),
            Key::new(Time::seconds(4), ParamValue::Point([40.0, 0.0])),
        ]);
        assert_eq!(
            p.value_at(Time::seconds(1)),
            Some(ParamValue::Point([10.0, 75.0]))
        );
        let i = curve(vec![
            Key::new(Time::ZERO, ParamValue::Int(0)),
            Key::new(Time::seconds(4), ParamValue::Int(3)),
        ]);
        assert_eq!(i.value_at(Time::seconds(1)), Some(ParamValue::Int(1)));
        assert_eq!(i.value_at(Time::seconds(2)), Some(ParamValue::Int(2)));
    }

    #[test]
    fn checks_refuse_broken_curves() {
        assert_eq!(Curve { keys: vec![] }.check(), Err(EditError::EmptyCurve));
        let twice = Curve {
            keys: vec![
                Key::new(Time::ZERO, float(0.0)),
                Key::new(Time::ZERO, float(1.0)),
            ],
        };
        assert_eq!(twice.check(), Err(EditError::KeysOutOfOrder(Time::ZERO)));
        let mixed = Curve {
            keys: vec![
                Key::new(Time::ZERO, float(0.0)),
                Key::new(Time::seconds(1), ParamValue::Angle(1.0)),
            ],
        };
        assert!(matches!(
            mixed.check(),
            Err(EditError::KeyValueKind {
                expected: "float",
                found: "angle"
            })
        ));
        let text = Curve::single(Time::ZERO, ParamValue::Text("a".into()));
        assert!(matches!(text.check(), Err(EditError::KeyValueKind { .. })));
        let nan = Curve::single(Time::ZERO, float(f64::NAN));
        assert_eq!(nan.check(), Err(EditError::NotFinite("key value")));
        let early = Curve::single(-Time::seconds(1), float(0.0));
        assert_eq!(
            early.check(),
            Err(EditError::KeyBeforeZero(-Time::seconds(1)))
        );
        let mut wide = Key::new(Time::ZERO, ParamValue::Point([0.0, 0.0]));
        wide.interpolation = Interpolation::ease(1);
        assert_eq!(
            Curve { keys: vec![wide] }.check(),
            Err(EditError::HandleComponents {
                expected: 2,
                found: 1
            })
        );
        let mut far = Key::new(Time::ZERO, float(0.0));
        far.interpolation = Interpolation::Bezier {
            out: Handle::new(vec![0.0], 1.5),
            into: Handle::ease(1),
        };
        assert!(matches!(
            Curve { keys: vec![far] }.check(),
            Err(EditError::OutOfRange {
                what: "handle influence",
                ..
            })
        ));
    }

    #[test]
    fn clip_param_ids_round_trip() {
        for p in ClipParam::ALL {
            assert_eq!(ClipParam::from_id(p.id()), Some(p));
            assert_eq!(
                serde_json::to_string(&p).expect("ser"),
                format!("\"{}\"", p.id())
            );
            assert_eq!(p.def().id, p.id());
            assert!(p.def().keyframeable);
            assert_eq!(
                p.def().sanitise(p.def().kind.default_value()),
                p.def().kind.default_value()
            );
        }
        let defaults = Clip::new(crate::id::MediaId(1), Time::ZERO);
        for p in ClipParam::ALL {
            assert_eq!(defaults.constant(p), p.def().kind.default_value(), "{p:?}");
        }
    }

    #[test]
    fn interpolation_serialises_by_name() {
        let json = serde_json::to_value(Interpolation::Hold).expect("ser");
        assert_eq!(json, serde_json::json!("hold"));
        let json = serde_json::to_value(Interpolation::ease(1)).expect("ser");
        assert_eq!(
            json,
            serde_json::json!({ "bezier": {
                "out": { "speed": [0.0], "influence": EASE_INFLUENCE },
                "in": { "speed": [0.0], "influence": EASE_INFLUENCE }
            } })
        );
        let back: Interpolation = serde_json::from_value(json).expect("de");
        assert_eq!(back, Interpolation::ease(1));
    }
}
