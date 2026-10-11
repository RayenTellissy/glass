//! Clip properties: speed, enabled flag, audio gain, transform, crop, opacity, blend mode
//! and the effect stack, and the edits that set them (schema v4).
//!
//! A clip's timeline range is its item's `range` (sequence time, on the track's grid). Its
//! source range starts at `source_start` and lasts the timeline duration times the speed,
//! exactly: `source duration = timeline duration × speed`. The timeline range is the one
//! on a grid, so an edit that derives a timeline duration from a source duration
//! ([`Project::set_clip_source_range`], [`Project::set_clip_speed`] with
//! [`SpeedKeep::Source`]) rounds it to the nearest whole grid step (halves away from zero,
//! at least one step), and the source duration follows from the rounded value.
//!
//! The transform's position and anchor are offsets from the canvas centre and from the
//! layer centre, so the default transform (zero offsets, 100%, no rotation) shows any
//! layer centred on any canvas without the project knowing either size; `engine` adds
//! half the canvas and layer sizes for `render::transform::Transform2D`. Crop amounts and
//! the feather are in layer pixels, as `render::crop::Crop` takes them (Decisions 23, 24).
//!
//! Every clip carries every property: transform, crop, opacity, blend mode and effects
//! apply to clips on video tracks, gain to clips on audio tracks, and the others are kept
//! and ignored. Numbers given to an edit are clamped to their range (a value that is not
//! finite is refused); `Project::validate` refuses a loaded tree with a value outside its
//! range. The transform, crop, opacity, gain and numeric effect parameters can be animated
//! by keyframe curves ([`crate::keyframe`], schema v7); while one is, the value stored here
//! is ignored and the `*_at` readers give the animated value.
//!
//! The edits here follow [`crate::edit`]: pure, validated, sharing everything they do not
//! touch. Each copies only the clip's item and its track's spine, and an edit that changes
//! nothing returns this root unchanged, sharing every node.

use std::sync::Arc;

use imbl::OrdMap;
use serde::{Deserialize, Serialize};

use crate::edit::place;
use crate::error::EditError;
use crate::id::{EffectId, ItemId};
use crate::keyframe::{Curve, check_clip_curves};
use crate::model::{Clip, Item, ItemKind, Project};
use crate::params::{ParamSchema, ParamValue, ParamValues};
use crate::time::{Rational, Time, TimeRange};
use crate::transition::fit_track;

/// The slowest speed a clip plays at, 1%.
pub const MIN_SPEED: Rational = Rational::new(1, 100);
/// The fastest speed a clip plays at, 10,000%.
pub const MAX_SPEED: Rational = Rational::new(100, 1);
/// Quietest clip gain, in dB.
pub const MIN_GAIN_DB: f64 = -96.0;
/// Loudest clip gain, in dB.
pub const MAX_GAIN_DB: f64 = 24.0;
/// The largest position or anchor offset along an axis, in pixels.
pub const MAX_OFFSET_PX: f64 = 1_000_000.0;
/// The largest scale along an axis (10,000%); negative scales flip, down to `-MAX_SCALE`.
pub const MAX_SCALE: f64 = 100.0;
/// The largest crop of one edge, in layer pixels (the crop schema's range).
pub const MAX_CROP_PX: f64 = 16_384.0;
/// The widest crop feather, in layer pixels (the crop schema's range).
pub const MAX_FEATHER_PX: f64 = 1_000.0;

/// How fast a clip plays its source over time.
///
/// Keys are ordered by source time; each key's speed holds from its source time on. Schema
/// v4 holds exactly one key, at source time 0: a constant speed. Keyframed ramps (Phase 7)
/// add keys and how the speed moves between them to this same list, so files with a
/// constant speed keep their shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedMap {
    pub keys: Vec<SpeedKey>,
}

/// One key of a [`SpeedMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedKey {
    /// Source time (seconds from the start of the file) from which this speed holds.
    pub source_time: Time,
    /// Source seconds per timeline second: 2 plays twice as fast, 1/2 at half speed.
    /// Stored reduced, with both parts positive, between [`MIN_SPEED`] and [`MAX_SPEED`].
    pub speed: Rational,
}

impl Default for SpeedMap {
    fn default() -> Self {
        Self::constant_unchecked(Rational::ONE)
    }
}

impl SpeedMap {
    /// A constant speed. Refuses a speed outside [`MIN_SPEED`] to [`MAX_SPEED`] (zero,
    /// negative and a zero denominator included); stores it reduced.
    pub fn constant(speed: Rational) -> Result<Self, EditError> {
        Ok(Self::constant_unchecked(check_speed(speed)?))
    }

    fn constant_unchecked(speed: Rational) -> Self {
        Self {
            keys: vec![SpeedKey {
                source_time: Time::ZERO,
                speed,
            }],
        }
    }

    /// The speed of a constant map; `None` for a map with more than one key.
    pub fn constant_speed(&self) -> Option<Rational> {
        match self.keys.as_slice() {
            [key] => Some(key.speed),
            _ => None,
        }
    }

    /// The constant speed, or [`EditError::UnsupportedSpeedMap`] for a ramp.
    pub fn speed(&self) -> Result<Rational, EditError> {
        self.constant_speed().ok_or(EditError::UnsupportedSpeedMap)
    }

    /// The source duration a timeline duration plays: `timeline × speed`, exact.
    pub fn source_duration(&self, timeline: Time) -> Result<Time, EditError> {
        Ok(timeline.mul_rational(self.speed()?)?)
    }

    /// The timeline duration that plays a source duration, `source / speed` rounded to
    /// the nearest whole `grid` step (halves away from zero), at least one step.
    pub fn timeline_duration(&self, source: Time, grid: Time) -> Result<Time, EditError> {
        timeline_duration(source, self.speed()?, grid)
    }

    /// Exactly one key, at source time 0, with a reduced speed in range.
    pub(crate) fn check(&self) -> Result<(), EditError> {
        let [key] = self.keys.as_slice() else {
            return Err(EditError::UnsupportedSpeedMap);
        };
        if !key.source_time.is_zero() {
            return Err(EditError::UnsupportedSpeedMap);
        }
        if check_speed(key.speed)? != key.speed {
            return Err(EditError::SpeedNotReduced(key.speed));
        }
        Ok(())
    }
}

/// `speed` reduced with a positive denominator, if it is between [`MIN_SPEED`] and
/// [`MAX_SPEED`].
pub fn check_speed(speed: Rational) -> Result<Rational, EditError> {
    let out = EditError::SpeedOutOfRange(speed);
    if speed.den == 0 {
        return Err(out);
    }
    let (mut num, mut den) = (i64::from(speed.num), i64::from(speed.den));
    if den < 0 {
        (num, den) = (-num, -den);
    }
    let g = gcd(num.unsigned_abs(), den.unsigned_abs()).max(1) as i64;
    let (num, den) = (num / g, den / g);
    let at_least_min = num * i64::from(MIN_SPEED.den) >= i64::from(MIN_SPEED.num) * den;
    let at_most_max = num * i64::from(MAX_SPEED.den) <= i64::from(MAX_SPEED.num) * den;
    if !(at_least_min && at_most_max) {
        return Err(out);
    }
    // In range, so both parts fit: num ≤ 100 · den and den ≤ 100 · num.
    Ok(Rational::new(
        i32::try_from(num).map_err(|_| out.clone())?,
        i32::try_from(den).map_err(|_| out)?,
    ))
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// `source / speed` rounded to the nearest whole `grid` step, at least one.
fn timeline_duration(source: Time, speed: Rational, grid: Time) -> Result<Time, EditError> {
    let exact = source.mul_ratio(i64::from(speed.den), i64::from(speed.num))?;
    let steps = exact.round_to_multiple(grid)?.max(1);
    Ok(grid.mul_ratio(steps, 1)?)
}

/// What [`Project::set_clip_speed`] keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpeedKeep {
    /// The source range: the clip's timeline duration becomes its source duration over
    /// the new speed, rounded to the grid; its start stays.
    Source,
    /// The timeline range: the clip shows more or less of its source.
    Timeline,
}

/// A clip's placement on the canvas. See the module docs for the offsets.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Where the anchor lands, as an offset from the canvas centre in canvas pixels
    /// (+x right, +y down).
    pub position: [f64; 2],
    /// 1.0 is 100%, per axis; negative flips.
    pub scale: [f64; 2],
    /// Degrees, clockwise.
    pub rotation: f64,
    /// The point scale and rotation happen about, as an offset from the layer centre in
    /// layer pixels.
    pub anchor: [f64; 2],
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// Centred, 100%, upright.
    pub const IDENTITY: Transform = Transform {
        position: [0.0; 2],
        scale: [1.0; 2],
        rotation: 0.0,
        anchor: [0.0; 2],
    };

    /// The position in canvas pixels from the canvas's top left, as `render` takes it.
    pub fn canvas_position(&self, canvas: (u32, u32)) -> [f64; 2] {
        [
            f64::from(canvas.0) / 2.0 + self.position[0],
            f64::from(canvas.1) / 2.0 + self.position[1],
        ]
    }

    /// The anchor in layer pixels from the layer's top left, as `render` takes it.
    pub fn layer_anchor(&self, layer: (u32, u32)) -> [f64; 2] {
        [
            f64::from(layer.0) / 2.0 + self.anchor[0],
            f64::from(layer.1) / 2.0 + self.anchor[1],
        ]
    }

    fn clamped(self) -> Result<Self, EditError> {
        let offset = |what, v: [f64; 2]| -> Result<[f64; 2], EditError> {
            Ok([
                clamp(what, v[0], -MAX_OFFSET_PX, MAX_OFFSET_PX)?,
                clamp(what, v[1], -MAX_OFFSET_PX, MAX_OFFSET_PX)?,
            ])
        };
        Ok(Self {
            position: offset("position", self.position)?,
            scale: [
                clamp("scale", self.scale[0], -MAX_SCALE, MAX_SCALE)?,
                clamp("scale", self.scale[1], -MAX_SCALE, MAX_SCALE)?,
            ],
            rotation: finite("rotation", self.rotation)?,
            anchor: offset("anchor", self.anchor)?,
        })
    }

    fn check(&self) -> Result<(), EditError> {
        for v in self.position {
            within("position", v, -MAX_OFFSET_PX, MAX_OFFSET_PX)?;
        }
        for v in self.scale {
            within("scale", v, -MAX_SCALE, MAX_SCALE)?;
        }
        finite("rotation", self.rotation)?;
        for v in self.anchor {
            within("anchor", v, -MAX_OFFSET_PX, MAX_OFFSET_PX)?;
        }
        Ok(())
    }
}

/// Per-edge crop in layer pixels, applied before the transform, and the width of the soft
/// edge inside the crop rectangle. All zero is no crop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub feather: f64,
}

impl Crop {
    pub const NONE: Crop = Crop {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        feather: 0.0,
    };

    fn clamped(self) -> Result<Self, EditError> {
        Ok(Self {
            left: clamp("crop.left", self.left, 0.0, MAX_CROP_PX)?,
            top: clamp("crop.top", self.top, 0.0, MAX_CROP_PX)?,
            right: clamp("crop.right", self.right, 0.0, MAX_CROP_PX)?,
            bottom: clamp("crop.bottom", self.bottom, 0.0, MAX_CROP_PX)?,
            feather: clamp("crop.feather", self.feather, 0.0, MAX_FEATHER_PX)?,
        })
    }

    fn check(&self) -> Result<(), EditError> {
        within("crop.left", self.left, 0.0, MAX_CROP_PX)?;
        within("crop.top", self.top, 0.0, MAX_CROP_PX)?;
        within("crop.right", self.right, 0.0, MAX_CROP_PX)?;
        within("crop.bottom", self.bottom, 0.0, MAX_CROP_PX)?;
        within("crop.feather", self.feather, 0.0, MAX_FEATHER_PX)
    }
}

/// How a clip's picture combines with what is below it. Serialised as the ids in
/// [`crate::params::builtin::BLEND_MODES`] (`"colour-burn"`), which the compositor reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    ColourBurn,
    Screen,
    Add,
    ColourDodge,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Subtract,
}

impl BlendMode {
    /// In the order of [`crate::params::builtin::BLEND_MODES`] (the blend menu's order).
    pub const ALL: [BlendMode; 11] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::ColourBurn,
        BlendMode::Screen,
        BlendMode::Add,
        BlendMode::ColourDodge,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::Difference,
        BlendMode::Subtract,
    ];

    /// The stable id, as in project files and the compositing schema's blend choice.
    pub fn id(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Multiply => "multiply",
            BlendMode::ColourBurn => "colour-burn",
            BlendMode::Screen => "screen",
            BlendMode::Add => "add",
            BlendMode::ColourDodge => "colour-dodge",
            BlendMode::Overlay => "overlay",
            BlendMode::SoftLight => "soft-light",
            BlendMode::HardLight => "hard-light",
            BlendMode::Difference => "difference",
            BlendMode::Subtract => "subtract",
        }
    }

    pub fn from_id(id: &str) -> Option<BlendMode> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// One effect in a clip's effect stack: an instance of the effect whose
/// [`ParamSchema::id`] is `schema`, with its own values.
///
/// The stack applies in order, the first effect to the source picture; its result then
/// goes through the clip's crop, transform, opacity and blend. A disabled effect is kept
/// with its values and skipped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub id: EffectId,
    /// The id of the schema the values follow (`"lut"`). An effect this build does not
    /// know (from a plugin that is not installed) is kept as it is.
    pub schema: String,
    pub enabled: bool,
    pub params: ParamValues,
    /// Curves of the animated parameters, by parameter id (schema v7; see
    /// [`crate::keyframe`]). While a parameter has a curve its value in `params` is
    /// ignored: read it through [`Effect::param_at`].
    pub keyframes: OrdMap<String, Arc<Curve>>,
}

/// A schema id is lowercase ASCII, digits, `.`, `_` or `-`, like parameter ids.
fn check_schema_id(id: &str) -> Result<(), EditError> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
    if valid {
        Ok(())
    } else {
        Err(EditError::InvalidEffectSchema {
            schema: id.to_owned(),
            reason: "the id is not lowercase ASCII, digits, '.', '_' or '-'".into(),
        })
    }
}

/// The checks `Project::validate` runs on a clip's properties.
pub(crate) fn check_clip(clip: &Clip) -> Result<(), EditError> {
    clip.speed.check()?;
    within("gain_db", clip.gain_db, MIN_GAIN_DB, MAX_GAIN_DB)?;
    clip.transform.check()?;
    clip.crop.check()?;
    within("opacity", clip.opacity, 0.0, 1.0)?;
    for effect in &clip.effects {
        check_schema_id(&effect.schema)?;
    }
    check_clip_curves(clip)
}

fn finite(what: &'static str, v: f64) -> Result<f64, EditError> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(EditError::NotFinite(what))
    }
}

fn clamp(what: &'static str, v: f64, min: f64, max: f64) -> Result<f64, EditError> {
    Ok(finite(what, v)?.clamp(min, max))
}

fn within(what: &'static str, v: f64, min: f64, max: f64) -> Result<(), EditError> {
    if (min..=max).contains(&finite(what, v)?) {
        Ok(())
    } else {
        Err(EditError::OutOfRange {
            what,
            value: v,
            min,
            max,
        })
    }
}

impl Project {
    /// The clip item holding `effect`, and the effect.
    pub fn find_effect(&self, effect: EffectId) -> Option<(ItemId, &Arc<Effect>)> {
        self.sequences.iter().find_map(|s| {
            s.tracks.iter().find_map(|t| {
                t.items
                    .iter()
                    .find_map(|i| i.clip()?.effect(effect).map(|e| (i.id, e)))
            })
        })
    }

    /// Changes a clip's speed. With [`SpeedKeep::Source`] the timeline duration becomes
    /// the source duration over `speed`, rounded to the track's grid (at least one step),
    /// and the clip must still fit before the next item; with [`SpeedKeep::Timeline`] the
    /// range stays and the source duration follows. Refuses a speed outside [`MIN_SPEED`]
    /// to [`MAX_SPEED`].
    pub fn set_clip_speed(
        &self,
        item: ItemId,
        speed: Rational,
        keep: SpeedKeep,
    ) -> Result<Project, EditError> {
        let speed = check_speed(speed)?;
        self.change_clip_range(item, |range, clip, grid| {
            if keep == SpeedKeep::Source {
                let source = clip.speed.source_duration(range.duration)?;
                range.duration = timeline_duration(source, speed, grid)?;
            }
            clip.speed = SpeedMap::constant_unchecked(speed);
            Ok(())
        })
    }

    /// Shows `source` of the clip's media: the source start becomes `source.start` and the
    /// timeline duration `source.duration` over the speed, rounded to the track's grid (at
    /// least one step). The timeline start stays; the clip must still fit before the next
    /// item.
    pub fn set_clip_source_range(
        &self,
        item: ItemId,
        source: TimeRange,
    ) -> Result<Project, EditError> {
        if source.start.is_negative() {
            return Err(EditError::SourceBeforeStart(source.start));
        }
        if !source.duration.is_positive() {
            return Err(EditError::EmptyRange(source.duration));
        }
        self.change_clip_range(item, |range, clip, grid| {
            range.duration = clip.speed.timeline_duration(source.duration, grid)?;
            clip.source_start = source.start;
            Ok(())
        })
    }

    /// Slips a clip: it shows its source from `source_start` on, over the same timeline
    /// range.
    pub fn slip_clip(&self, item: ItemId, source_start: Time) -> Result<Project, EditError> {
        if source_start.is_negative() {
            return Err(EditError::SourceBeforeStart(source_start));
        }
        self.change_clip(item, |clip| {
            clip.source_start = source_start;
            Ok(())
        })
    }

    /// Turns a clip on or off. A disabled clip keeps its place and properties but shows
    /// and plays nothing.
    pub fn set_clip_enabled(&self, item: ItemId, enabled: bool) -> Result<Project, EditError> {
        self.change_clip(item, |clip| {
            clip.enabled = enabled;
            Ok(())
        })
    }

    /// Sets a clip's audio gain in dB, clamped to [`MIN_GAIN_DB`] to [`MAX_GAIN_DB`].
    pub fn set_clip_gain(&self, item: ItemId, gain_db: f64) -> Result<Project, EditError> {
        let gain_db = clamp("gain_db", gain_db, MIN_GAIN_DB, MAX_GAIN_DB)?;
        self.change_clip(item, |clip| {
            clip.gain_db = gain_db;
            Ok(())
        })
    }

    /// Sets a clip's transform, each value clamped to its range.
    pub fn set_clip_transform(
        &self,
        item: ItemId,
        transform: Transform,
    ) -> Result<Project, EditError> {
        let transform = transform.clamped()?;
        self.change_clip(item, |clip| {
            clip.transform = transform;
            Ok(())
        })
    }

    /// Sets a clip's crop, each value clamped to its range.
    pub fn set_clip_crop(&self, item: ItemId, crop: Crop) -> Result<Project, EditError> {
        let crop = crop.clamped()?;
        self.change_clip(item, |clip| {
            clip.crop = crop;
            Ok(())
        })
    }

    /// Sets a clip's opacity, clamped to 0 to 1.
    pub fn set_clip_opacity(&self, item: ItemId, opacity: f64) -> Result<Project, EditError> {
        let opacity = clamp("opacity", opacity, 0.0, 1.0)?;
        self.change_clip(item, |clip| {
            clip.opacity = opacity;
            Ok(())
        })
    }

    pub fn set_clip_blend(&self, item: ItemId, blend: BlendMode) -> Result<Project, EditError> {
        self.change_clip(item, |clip| {
            clip.blend = blend;
            Ok(())
        })
    }

    /// Adds an effect following `schema`, with every parameter at its default and
    /// enabled, at `index` in the clip's stack (the end when `None`). Refuses a schema
    /// that does not validate.
    pub fn add_effect(
        &self,
        item: ItemId,
        schema: &ParamSchema,
        index: Option<usize>,
    ) -> Result<(Project, EffectId), EditError> {
        check_schema_id(&schema.id)?;
        schema
            .validate()
            .map_err(|e| EditError::InvalidEffectSchema {
                schema: schema.id.clone(),
                reason: e.to_string(),
            })?;
        let mut next = self.clone();
        let id = EffectId(next.alloc_id());
        let next = next.change_clip(item, |clip| {
            let len = clip.effects.len();
            let index = index.unwrap_or(len);
            if index > len {
                return Err(EditError::EffectIndex { index, len });
            }
            let effect = Effect {
                id,
                schema: schema.id.clone(),
                enabled: true,
                params: schema.defaults(),
                keyframes: OrdMap::new(),
            };
            clip.effects.insert(index, Arc::new(effect));
            Ok(())
        })?;
        Ok((next, id))
    }

    /// Removes an effect from its clip's stack.
    pub fn remove_effect(&self, effect: EffectId) -> Result<Project, EditError> {
        let (item, index) = self.locate_effect(effect)?;
        self.change_clip(item, |clip| {
            clip.effects.remove(index);
            Ok(())
        })
    }

    /// Moves an effect to `index` in its clip's stack (its index once moved).
    pub fn move_effect(&self, effect: EffectId, index: usize) -> Result<Project, EditError> {
        let (item, from) = self.locate_effect(effect)?;
        self.change_clip(item, |clip| {
            let len = clip.effects.len();
            if index >= len {
                return Err(EditError::EffectIndex { index, len });
            }
            let moved = clip.effects.remove(from);
            clip.effects.insert(index, moved);
            Ok(())
        })
    }

    /// Turns an effect on or off, keeping its values.
    pub fn set_effect_enabled(
        &self,
        effect: EffectId,
        enabled: bool,
    ) -> Result<Project, EditError> {
        let (item, index) = self.locate_effect(effect)?;
        self.change_clip(item, |clip| {
            Arc::make_mut(&mut clip.effects[index]).enabled = enabled;
            Ok(())
        })
    }

    /// Sets one parameter of an effect, made valid for it by [`ParamValues::set`]
    /// (clamped to its range; the default when the type or choice does not fit or a number
    /// is not finite). `schema` must be the effect's own.
    pub fn set_effect_param(
        &self,
        effect: EffectId,
        schema: &ParamSchema,
        param: &str,
        value: ParamValue,
    ) -> Result<Project, EditError> {
        let (item, index) = self.locate_effect(effect)?;
        self.change_clip(item, |clip| {
            let node = Arc::make_mut(&mut clip.effects[index]);
            if node.schema != schema.id {
                return Err(EditError::EffectSchemaMismatch {
                    effect,
                    expected: node.schema.clone(),
                    found: schema.id.clone(),
                });
            }
            node.params
                .set(schema, param, value)
                .ok_or_else(|| EditError::UnknownParam {
                    schema: schema.id.clone(),
                    param: param.to_owned(),
                })?;
            Ok(())
        })
    }

    pub(crate) fn locate_effect(&self, effect: EffectId) -> Result<(ItemId, usize), EditError> {
        self.sequences
            .iter()
            .flat_map(|s| s.tracks.iter())
            .flat_map(|t| t.items.iter())
            .find_map(|i| {
                let index = i.clip()?.effects.iter().position(|e| e.id == effect)?;
                Some((i.id, index))
            })
            .ok_or(EditError::UnknownEffect(effect))
    }

    /// Applies `change` to a copy of the clip `item` and puts the copy in its place. A
    /// change that leaves the clip equal returns this root.
    pub(crate) fn change_clip(
        &self,
        item: ItemId,
        change: impl FnOnce(&mut Clip) -> Result<(), EditError>,
    ) -> Result<Project, EditError> {
        self.change_clip_range(item, |_, clip, _| change(clip))
    }

    /// Applies `change` to copies of the clip `item` and its timeline range (given the
    /// track's grid) and places the result on the track, checking the grid and the
    /// neighbours. A change to the range, source start or speed fits the track's
    /// transitions afterwards ([`crate::transition`]). A change that leaves both equal
    /// returns this root.
    fn change_clip_range(
        &self,
        item: ItemId,
        change: impl FnOnce(&mut TimeRange, &mut Clip, Time) -> Result<(), EditError>,
    ) -> Result<Project, EditError> {
        let (si, ti, ii) = self.locate_item(item)?;
        let seq = &self.sequences[si];
        let track = &seq.tracks[ti];
        let old = &track.items[ii];
        let clip = old.clip().ok_or(EditError::NotAClip(item))?;
        let grid = seq.grid(track.kind)?;
        let mut range = old.range;
        let mut changed = clip.clone();
        change(&mut range, &mut changed, grid)?;
        if range == old.range && changed == *clip {
            return Ok(self.clone());
        }
        let refit = range != old.range
            || changed.source_start != clip.source_start
            || changed.speed != clip.speed;
        let node = Arc::new(Item {
            id: item,
            range,
            kind: ItemKind::Clip(changed),
        });
        let mut next = self.clone();
        let (_, track) = next.track_at_mut(si, ti);
        if range == old.range {
            track.items[ii] = node;
        } else {
            track.items.remove(ii);
            place(grid, track, node)?;
        }
        if refit {
            fit_track(track, grid);
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::builtin::BLEND_MODES;

    #[test]
    fn blend_ids_follow_the_compositing_schema() {
        let ids: Vec<&str> = BlendMode::ALL.iter().map(|m| m.id()).collect();
        assert_eq!(ids, BLEND_MODES);
        for mode in BlendMode::ALL {
            let json = serde_json::to_string(&mode).expect("ser");
            assert_eq!(json, format!("\"{}\"", mode.id()));
            assert_eq!(BlendMode::from_id(mode.id()), Some(mode));
        }
        assert_eq!(BlendMode::from_id("colour_burn"), None);
    }

    #[test]
    fn speeds_are_reduced_and_bounded() {
        assert_eq!(check_speed(Rational::new(4, 2)), Ok(Rational::new(2, 1)));
        assert_eq!(check_speed(Rational::new(-1, -2)), Ok(Rational::new(1, 2)));
        assert_eq!(check_speed(MIN_SPEED), Ok(MIN_SPEED));
        assert_eq!(check_speed(MAX_SPEED), Ok(MAX_SPEED));
        for bad in [
            Rational::new(0, 1),
            Rational::new(-1, 1),
            Rational::new(1, 0),
            Rational::new(1, 101),
            Rational::new(101, 1),
            Rational::new(i32::MAX, 1),
        ] {
            assert_eq!(check_speed(bad), Err(EditError::SpeedOutOfRange(bad)));
        }
    }

    #[test]
    fn durations_follow_the_speed() {
        let frame = Time::new(1, 25).expect("time");
        let half = SpeedMap::constant(Rational::new(1, 2)).expect("speed");
        assert_eq!(half.source_duration(Time::seconds(2)), Ok(Time::seconds(1)));
        // 7 frames of source at half speed is 14 frames; at 3x it is 2.33, so 2 frames.
        let seven = Time::new(7, 25).expect("time");
        assert_eq!(
            half.timeline_duration(seven, frame),
            Ok(Time::new(14, 25).expect("time"))
        );
        let triple = SpeedMap::constant(Rational::new(3, 1)).expect("speed");
        assert_eq!(
            triple.timeline_duration(seven, frame),
            Ok(Time::new(2, 25).expect("time"))
        );
        // Never less than one step.
        assert_eq!(
            triple.timeline_duration(Time::new(1, 100).expect("time"), frame),
            Ok(frame)
        );
        let ramp = SpeedMap {
            keys: vec![half.keys[0], half.keys[0]],
        };
        assert_eq!(ramp.speed(), Err(EditError::UnsupportedSpeedMap));
        assert_eq!(ramp.check(), Err(EditError::UnsupportedSpeedMap));
        let unreduced = SpeedMap {
            keys: vec![SpeedKey {
                source_time: Time::ZERO,
                speed: Rational::new(2, 4),
            }],
        };
        assert_eq!(
            unreduced.check(),
            Err(EditError::SpeedNotReduced(Rational::new(2, 4)))
        );
        let late = SpeedMap {
            keys: vec![SpeedKey {
                source_time: Time::seconds(1),
                speed: Rational::ONE,
            }],
        };
        assert_eq!(late.check(), Err(EditError::UnsupportedSpeedMap));
        assert_eq!(SpeedMap::default().check(), Ok(()));
    }

    #[test]
    fn transform_offsets_are_from_the_centres() {
        let t = Transform {
            position: [10.0, -20.0],
            anchor: [-5.0, 0.0],
            ..Transform::IDENTITY
        };
        assert_eq!(t.canvas_position((1920, 1080)), [970.0, 520.0]);
        assert_eq!(t.layer_anchor((1280, 720)), [635.0, 360.0]);
    }
}
