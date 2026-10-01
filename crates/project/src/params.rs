//! Effect parameters: a typed schema per effect (what the inspector shows and keyframes
//! animate) and a container of values checked against it.
//!
//! Every parameter has a stable string id (`"position"`, `"crop.left"`), unique within its
//! schema; projects store values and keyframe curves under `<effect id>/<param id>`, so an
//! id never changes once shipped (rename the label instead). Labels are i18n keys looked
//! up with `t!`. Ranges are hard limits (values are clamped to them); soft ranges are
//! what sliders and scrubbing cover by default, and typing can go beyond them up to the
//! hard range.
//!
//! Values are plain serde data with no GPU types, so clip effect stacks can store them in
//! the project file; `render` reads them through the same schema.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What a number means, for display and scrub speed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    #[default]
    None,
    /// Stored as 0..1 (or beyond), shown times 100 with `%`.
    Percent,
    Pixels,
    Degrees,
    Seconds,
    Frames,
    Decibels,
}

impl Unit {
    /// The suffix shown in fields (not translated: symbols).
    pub fn symbol(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Percent => "%",
            Self::Pixels => "px",
            Self::Degrees => "°",
            Self::Seconds => "s",
            Self::Frames => "f",
            Self::Decibels => "dB",
        }
    }

    /// Stored value to displayed number.
    pub fn to_display(self, v: f64) -> f64 {
        if self == Self::Percent { v * 100.0 } else { v }
    }

    /// Displayed number to stored value.
    pub fn from_display(self, v: f64) -> f64 {
        if self == Self::Percent { v / 100.0 } else { v }
    }

    /// A stored value as the field shows it, with `decimals` places and no symbol
    /// (`0.86` in percent with 0 places is `86`). A value that rounds to zero never shows
    /// a minus sign.
    pub fn format(self, v: f64, decimals: usize) -> String {
        let shown = self.to_display(v);
        let text = format!("{shown:.decimals$}");
        if text.starts_with('-') && text[1..].chars().all(|c| c == '0' || c == '.') {
            text[1..].to_owned()
        } else {
            text
        }
    }
}

/// An inclusive range. An open end is infinite, written as `null` in JSON.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Range {
    #[serde(with = "open_min")]
    pub min: f64,
    #[serde(with = "open_max")]
    pub max: f64,
}

/// Serde for range ends: JSON has no infinity, so an open end is `null`.
macro_rules! open_end {
    ($name:ident, $open:expr) => {
        mod $name {
            use serde::{Deserialize, Deserializer, Serialize, Serializer};

            pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
                v.is_finite().then_some(*v).serialize(s)
            }

            pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
                Ok(Option::<f64>::deserialize(d)?.unwrap_or($open))
            }
        }
    };
}
open_end!(open_min, f64::NEG_INFINITY);
open_end!(open_max, f64::INFINITY);

impl Range {
    pub const fn new(min: f64, max: f64) -> Self {
        Self { min, max }
    }

    pub const UNBOUNDED: Self = Self::new(f64::NEG_INFINITY, f64::INFINITY);

    pub fn clamp(&self, v: f64) -> f64 {
        v.clamp(self.min, self.max)
    }

    pub fn contains(&self, v: f64) -> bool {
        (self.min..=self.max).contains(&v)
    }

    pub fn is_bounded(&self) -> bool {
        self.min.is_finite() && self.max.is_finite()
    }
}

/// One option of a choice parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChoiceOption {
    /// Stable id stored in projects.
    pub id: String,
    /// i18n key of the label.
    pub label: String,
}

/// The type of a parameter with its limits and default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Float {
        default: f64,
        range: Range,
        soft: Option<Range>,
        /// Decimal places shown.
        decimals: u8,
    },
    Int {
        default: i64,
        range: Range,
        soft: Option<Range>,
    },
    Bool {
        default: bool,
    },
    Choice {
        default: String,
        options: Vec<ChoiceOption>,
    },
    /// Linear RGBA in the working space, straight alpha.
    Colour {
        default: [f32; 4],
        alpha: bool,
    },
    Point {
        default: [f64; 2],
        range: Range,
        decimals: u8,
    },
    /// Degrees; unbounded unless a range is given, so animations can turn many times.
    Angle {
        default: f64,
        range: Range,
    },
    Text {
        default: String,
        multiline: bool,
    },
}

impl ParamKind {
    pub fn default_value(&self) -> ParamValue {
        match self {
            Self::Float { default, .. } => ParamValue::Float(*default),
            Self::Int { default, .. } => ParamValue::Int(*default),
            Self::Bool { default } => ParamValue::Bool(*default),
            Self::Choice { default, .. } => ParamValue::Choice(default.clone()),
            Self::Colour { default, .. } => ParamValue::Colour(*default),
            Self::Point { default, .. } => ParamValue::Point(*default),
            Self::Angle { default, .. } => ParamValue::Angle(*default),
            Self::Text { default, .. } => ParamValue::Text(default.clone()),
        }
    }

    /// Whether values of this kind can be interpolated between keyframes (choices, text
    /// and booleans hold until the next keyframe instead).
    pub fn interpolates(&self) -> bool {
        matches!(
            self,
            Self::Float { .. }
                | Self::Int { .. }
                | Self::Colour { .. }
                | Self::Point { .. }
                | Self::Angle { .. }
        )
    }

    /// The hard range of a numeric kind.
    pub fn range(&self) -> Option<Range> {
        match self {
            Self::Float { range, .. }
            | Self::Int { range, .. }
            | Self::Point { range, .. }
            | Self::Angle { range, .. } => Some(*range),
            _ => None,
        }
    }

    /// The range scrubbing and sliders cover by default.
    pub fn soft_range(&self) -> Option<Range> {
        match self {
            Self::Float { range, soft, .. } | Self::Int { range, soft, .. } => {
                Some(soft.unwrap_or(*range))
            }
            Self::Point { range, .. } | Self::Angle { range, .. } => Some(*range),
            _ => None,
        }
    }

    /// Decimal places a numeric kind shows (angles 1, integers 0).
    pub fn decimals(&self) -> usize {
        match self {
            Self::Float { decimals, .. } | Self::Point { decimals, .. } => usize::from(*decimals),
            Self::Angle { .. } => 1,
            _ => 0,
        }
    }
}

/// One parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamDef {
    /// Stable id, unique in the schema.
    pub id: String,
    /// i18n key of the label.
    pub label: String,
    pub kind: ParamKind,
    pub unit: Unit,
    pub keyframeable: bool,
    /// Show a slider over the soft range beside the field (floats with a finite soft
    /// range only).
    #[serde(default)]
    pub slider: bool,
}

impl ParamDef {
    fn new(id: &str, label: &str, kind: ParamKind) -> Self {
        let keyframeable = kind.interpolates();
        Self {
            id: id.to_owned(),
            label: label.to_owned(),
            kind,
            unit: Unit::None,
            keyframeable,
            slider: false,
        }
    }

    pub fn float(id: &str, label: &str, default: f64, range: Range) -> Self {
        Self::new(
            id,
            label,
            ParamKind::Float {
                default,
                range,
                soft: None,
                decimals: 1,
            },
        )
    }

    pub fn int(id: &str, label: &str, default: i64, range: Range) -> Self {
        Self::new(
            id,
            label,
            ParamKind::Int {
                default,
                range,
                soft: None,
            },
        )
    }

    pub fn boolean(id: &str, label: &str, default: bool) -> Self {
        Self::new(id, label, ParamKind::Bool { default })
    }

    /// `options` are (id, label key) pairs; the default is the first unless set.
    pub fn choice(id: &str, label: &str, options: &[(&str, &str)]) -> Self {
        Self::new(
            id,
            label,
            ParamKind::Choice {
                default: options.first().map(|o| o.0.to_owned()).unwrap_or_default(),
                options: options
                    .iter()
                    .map(|(id, label)| ChoiceOption {
                        id: (*id).to_owned(),
                        label: (*label).to_owned(),
                    })
                    .collect(),
            },
        )
    }

    pub fn colour(id: &str, label: &str, default: [f32; 4], alpha: bool) -> Self {
        Self::new(id, label, ParamKind::Colour { default, alpha })
    }

    pub fn point(id: &str, label: &str, default: [f64; 2]) -> Self {
        Self::new(
            id,
            label,
            ParamKind::Point {
                default,
                range: Range::UNBOUNDED,
                decimals: 1,
            },
        )
        .unit(Unit::Pixels)
    }

    pub fn angle(id: &str, label: &str, default: f64) -> Self {
        Self::new(
            id,
            label,
            ParamKind::Angle {
                default,
                range: Range::UNBOUNDED,
            },
        )
        .unit(Unit::Degrees)
    }

    pub fn text(id: &str, label: &str, default: &str, multiline: bool) -> Self {
        Self::new(
            id,
            label,
            ParamKind::Text {
                default: default.to_owned(),
                multiline,
            },
        )
    }

    pub fn unit(mut self, unit: Unit) -> Self {
        self.unit = unit;
        self
    }

    pub fn keyframeable(mut self, keyframeable: bool) -> Self {
        self.keyframeable = keyframeable;
        self
    }

    /// Adds a slider beside the field.
    pub fn slider(mut self) -> Self {
        self.slider = true;
        self
    }

    /// Sets the soft range of a float or int.
    pub fn soft(mut self, soft: Range) -> Self {
        if let ParamKind::Float { soft: s, .. } | ParamKind::Int { soft: s, .. } = &mut self.kind {
            *s = Some(soft);
        }
        self
    }

    /// Sets the hard range of a point or angle (floats and ints take it in their
    /// constructor).
    pub fn range(mut self, r: Range) -> Self {
        if let ParamKind::Point { range, .. } | ParamKind::Angle { range, .. } = &mut self.kind {
            *range = r;
        }
        self
    }

    pub fn decimals(mut self, n: u8) -> Self {
        if let ParamKind::Float { decimals, .. } | ParamKind::Point { decimals, .. } =
            &mut self.kind
        {
            *decimals = n;
        }
        self
    }

    /// Sets the default of a choice by option id.
    pub fn default_choice(mut self, id: &str) -> Self {
        if let ParamKind::Choice { default, .. } = &mut self.kind {
            *default = id.to_owned();
        }
        self
    }

    /// A stored number as its field shows it, without the unit symbol.
    pub fn format(&self, v: f64) -> String {
        self.unit.format(v, self.kind.decimals())
    }

    /// `value` made valid for this parameter (clamped, or the default when the type or
    /// choice does not fit, or a number is not finite).
    pub fn sanitise(&self, value: ParamValue) -> ParamValue {
        match (&self.kind, value) {
            (ParamKind::Float { range, .. }, ParamValue::Float(v)) if v.is_finite() => {
                ParamValue::Float(range.clamp(v))
            }
            (ParamKind::Int { range, .. }, ParamValue::Int(v)) => {
                ParamValue::Int(range.clamp(v as f64).round() as i64)
            }
            (ParamKind::Bool { .. }, v @ ParamValue::Bool(_)) => v,
            (ParamKind::Choice { options, .. }, ParamValue::Choice(id))
                if options.iter().any(|o| o.id == id) =>
            {
                ParamValue::Choice(id)
            }
            (ParamKind::Colour { alpha, .. }, ParamValue::Colour(mut c))
                if c.iter().all(|v| v.is_finite()) =>
            {
                for channel in &mut c[..3] {
                    *channel = channel.max(0.0);
                }
                c[3] = if *alpha { c[3].clamp(0.0, 1.0) } else { 1.0 };
                ParamValue::Colour(c)
            }
            (ParamKind::Point { range, .. }, ParamValue::Point(p))
                if p.iter().all(|v| v.is_finite()) =>
            {
                ParamValue::Point(p.map(|v| range.clamp(v)))
            }
            (ParamKind::Angle { range, .. }, ParamValue::Angle(v)) if v.is_finite() => {
                ParamValue::Angle(range.clamp(v))
            }
            (ParamKind::Text { .. }, v @ ParamValue::Text(_)) => v,
            _ => self.kind.default_value(),
        }
    }
}

/// A parameter's value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamValue {
    Float(f64),
    Int(i64),
    Bool(bool),
    Choice(String),
    Colour([f32; 4]),
    Point([f64; 2]),
    Angle(f64),
    Text(String),
}

impl ParamValue {
    /// The number of a float, int or angle.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(v) | Self::Angle(v) => Some(*v),
            Self::Int(v) => Some(*v as f64),
            _ => None,
        }
    }
}

/// The parameters of one effect (or of a layer's built-in sections).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamSchema {
    /// Stable effect id.
    pub id: String,
    /// i18n key of the section title.
    pub label: String,
    pub params: Vec<ParamDef>,
    /// A boolean parameter shown as the section header's switch instead of a row; while
    /// it is off the other rows are disabled.
    #[serde(default)]
    pub header_switch: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemaError {
    #[error("parameter id {0:?} appears twice")]
    DuplicateId(String),
    #[error("parameter id {0:?} is not lowercase ASCII, digits, '.', '_' or '-'")]
    BadId(String),
    #[error("parameter {0:?}: the default is outside its range or options")]
    BadDefault(String),
    #[error("parameter {0:?}: the range is empty or inverted")]
    BadRange(String),
    #[error("header switch {0:?} is not a boolean parameter of the schema")]
    BadHeaderSwitch(String),
}

impl ParamSchema {
    pub fn new(id: &str, label: &str, params: Vec<ParamDef>) -> Self {
        Self {
            id: id.to_owned(),
            label: label.to_owned(),
            params,
            header_switch: None,
        }
    }

    /// Shows the boolean parameter `id` as the section header's switch.
    pub fn header_switch(mut self, id: &str) -> Self {
        self.header_switch = Some(id.to_owned());
        self
    }

    pub fn get(&self, id: &str) -> Option<&ParamDef> {
        self.params.iter().find(|p| p.id == id)
    }

    /// Checks ids, ranges and defaults; built-in schemas are tested with it, plugins' at
    /// load.
    pub fn validate(&self) -> Result<(), SchemaError> {
        let valid = |id: &str| {
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        };
        for (i, p) in self.params.iter().enumerate() {
            if !valid(&p.id) {
                return Err(SchemaError::BadId(p.id.clone()));
            }
            if self.params[..i].iter().any(|q| q.id == p.id) {
                return Err(SchemaError::DuplicateId(p.id.clone()));
            }
            let ranges = [
                p.kind.range(),
                match p.kind {
                    ParamKind::Float { soft, .. } | ParamKind::Int { soft, .. } => soft,
                    _ => None,
                },
            ];
            if ranges
                .iter()
                .flatten()
                .any(|r| r.min.is_nan() || r.max.is_nan() || r.min > r.max)
            {
                return Err(SchemaError::BadRange(p.id.clone()));
            }
            let default = p.kind.default_value();
            if p.sanitise(default.clone()) != default {
                return Err(SchemaError::BadDefault(p.id.clone()));
            }
        }
        if let Some(id) = &self.header_switch
            && !matches!(
                self.get(id),
                Some(ParamDef {
                    kind: ParamKind::Bool { .. },
                    ..
                })
            )
        {
            return Err(SchemaError::BadHeaderSwitch(id.clone()));
        }
        Ok(())
    }

    /// Every parameter at its default.
    pub fn defaults(&self) -> ParamValues {
        ParamValues {
            values: self
                .params
                .iter()
                .map(|p| (p.id.clone(), p.kind.default_value()))
                .collect(),
        }
    }
}

/// Values by parameter id. Values for ids a schema does not know are kept (a newer
/// version of an effect may have written them) but ignored. An unset id reads as its
/// default. Serialised as a plain map from id to value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamValues {
    values: BTreeMap<String, ParamValue>,
}

impl ParamValues {
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of `id` made valid for the schema, or the default when unset; `None`
    /// for an id the schema does not have.
    pub fn get(&self, schema: &ParamSchema, id: &str) -> Option<ParamValue> {
        let def = schema.get(id)?;
        Some(
            self.values
                .get(id)
                .map(|v| def.sanitise(v.clone()))
                .unwrap_or_else(|| def.kind.default_value()),
        )
    }

    /// Sets `id` to `value` made valid for it; returns what was stored, or `None` for
    /// an id the schema does not have.
    pub fn set(&mut self, schema: &ParamSchema, id: &str, value: ParamValue) -> Option<ParamValue> {
        let def = schema.get(id)?;
        let value = def.sanitise(value);
        self.values.insert(id.to_owned(), value.clone());
        Some(value)
    }

    /// Back to the default.
    pub fn reset(&mut self, id: &str) {
        self.values.remove(id);
    }

    pub fn is_default(&self, schema: &ParamSchema, id: &str) -> bool {
        schema
            .get(id)
            .is_some_and(|d| self.get(schema, id) == Some(d.kind.default_value()))
    }

    pub fn float(&self, schema: &ParamSchema, id: &str) -> Option<f64> {
        self.get(schema, id)?.as_f64()
    }

    pub fn int(&self, schema: &ParamSchema, id: &str) -> Option<i64> {
        match self.get(schema, id)? {
            ParamValue::Int(v) => Some(v),
            _ => None,
        }
    }

    pub fn point(&self, schema: &ParamSchema, id: &str) -> Option<[f64; 2]> {
        match self.get(schema, id)? {
            ParamValue::Point(p) => Some(p),
            _ => None,
        }
    }

    pub fn boolean(&self, schema: &ParamSchema, id: &str) -> Option<bool> {
        match self.get(schema, id)? {
            ParamValue::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn choice(&self, schema: &ParamSchema, id: &str) -> Option<String> {
        match self.get(schema, id)? {
            ParamValue::Choice(c) => Some(c),
            _ => None,
        }
    }

    pub fn colour(&self, schema: &ParamSchema, id: &str) -> Option<[f32; 4]> {
        match self.get(schema, id)? {
            ParamValue::Colour(c) => Some(c),
            _ => None,
        }
    }

    pub fn text(&self, schema: &ParamSchema, id: &str) -> Option<String> {
        match self.get(schema, id)? {
            ParamValue::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// The schemas of a video layer's built-in sections, as `Sheet / Inspector video` shows
/// them.
pub mod builtin {
    use super::*;

    /// Blend mode option ids in the order the blend menu lists them; the compositor's
    /// blend node reads these ids.
    pub const BLEND_MODES: [&str; 11] = [
        "normal",
        "multiply",
        "colour-burn",
        "screen",
        "add",
        "colour-dodge",
        "overlay",
        "soft-light",
        "hard-light",
        "difference",
        "subtract",
    ];

    pub fn transform() -> ParamSchema {
        ParamSchema::new(
            "transform",
            "inspector.transform",
            vec![
                ParamDef::point("position", "inspector.transform.position", [960.0, 540.0]),
                ParamDef::float(
                    "scale",
                    "inspector.transform.scale",
                    1.0,
                    Range::new(0.0, 100.0),
                )
                .soft(Range::new(0.0, 4.0))
                .unit(Unit::Percent),
                ParamDef::angle("rotation", "inspector.transform.rotation", 0.0),
                ParamDef::point("anchor", "inspector.transform.anchor", [0.0, 0.0]),
            ],
        )
    }

    pub fn compositing() -> ParamSchema {
        // i18n keys allow `_` but not `-`.
        let labels: Vec<String> = BLEND_MODES
            .iter()
            .map(|m| format!("blend.{}", m.replace('-', "_")))
            .collect();
        let options: Vec<(&str, &str)> = BLEND_MODES
            .iter()
            .zip(&labels)
            .map(|(id, label)| (*id, label.as_str()))
            .collect();
        ParamSchema::new(
            "compositing",
            "inspector.compositing",
            vec![
                ParamDef::float(
                    "opacity",
                    "inspector.compositing.opacity",
                    1.0,
                    Range::new(0.0, 1.0),
                )
                .unit(Unit::Percent)
                .decimals(0)
                .slider(),
                ParamDef::choice("blend", "inspector.compositing.blend", &options),
            ],
        )
    }

    pub fn crop() -> ParamSchema {
        let side = |id: &str, label: &str| {
            ParamDef::float(id, label, 0.0, Range::new(0.0, 16384.0))
                .soft(Range::new(0.0, 1920.0))
                .unit(Unit::Pixels)
                .decimals(0)
        };
        ParamSchema::new(
            "crop",
            "inspector.crop",
            vec![
                ParamDef::boolean("enabled", "inspector.crop.enabled", true),
                side("left", "inspector.crop.left"),
                side("right", "inspector.crop.right"),
                side("top", "inspector.crop.top"),
                side("bottom", "inspector.crop.bottom"),
                ParamDef::float(
                    "feather",
                    "inspector.crop.feather",
                    0.0,
                    Range::new(0.0, 1000.0),
                )
                .soft(Range::new(0.0, 100.0))
                .unit(Unit::Pixels)
                .decimals(0)
                .slider(),
            ],
        )
        .header_switch("enabled")
    }

    pub fn motion_blur() -> ParamSchema {
        ParamSchema::new(
            "motion-blur",
            "inspector.motion_blur",
            vec![
                ParamDef::boolean("enabled", "inspector.motion_blur.enabled", false),
                ParamDef::float(
                    "shutter",
                    "inspector.motion_blur.shutter",
                    180.0,
                    Range::new(0.0, 720.0),
                )
                .soft(Range::new(0.0, 360.0))
                .unit(Unit::Degrees)
                .decimals(0),
                ParamDef::int(
                    "samples",
                    "inspector.motion_blur.samples",
                    8,
                    Range::new(2.0, 16.0),
                )
                .keyframeable(false),
            ],
        )
        .header_switch("enabled")
    }

    pub fn all() -> Vec<ParamSchema> {
        vec![transform(), compositing(), crop(), motion_blur()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_schemas_are_valid() {
        for schema in builtin::all() {
            schema
                .validate()
                .unwrap_or_else(|e| panic!("{}: {e}", schema.id));
        }
    }

    #[test]
    fn values_are_clamped_typed_and_default_when_unset() {
        let schema = builtin::compositing();
        let mut values = ParamValues::new();
        assert_eq!(values.float(&schema, "opacity"), Some(1.0));
        assert_eq!(
            values.set(&schema, "opacity", ParamValue::Float(1.5)),
            Some(ParamValue::Float(1.0))
        );
        assert_eq!(
            values.set(&schema, "opacity", ParamValue::Float(-0.2)),
            Some(ParamValue::Float(0.0))
        );
        assert_eq!(
            values.set(&schema, "blend", ParamValue::Choice("nope".into())),
            Some(ParamValue::Choice("normal".into()))
        );
        assert_eq!(
            values.set(&schema, "blend", ParamValue::Bool(true)),
            Some(ParamValue::Choice("normal".into()))
        );
        values.set(&schema, "blend", ParamValue::Choice("screen".into()));
        assert_eq!(values.choice(&schema, "blend").as_deref(), Some("screen"));
        assert!(
            values
                .set(&schema, "missing", ParamValue::Bool(true))
                .is_none()
        );
        assert!(values.get(&schema, "missing").is_none());
        values.reset("blend");
        assert!(values.is_default(&schema, "blend"));
    }

    #[test]
    fn clamping_per_kind() {
        let schema = ParamSchema::new(
            "x",
            "x",
            vec![
                ParamDef::int("n", "n", 8, Range::new(2.0, 16.0)),
                ParamDef::colour("c", "c", [1.0, 1.0, 1.0, 1.0], false),
                ParamDef::colour("ca", "ca", [1.0, 1.0, 1.0, 1.0], true),
                ParamDef::point("p", "p", [0.0, 0.0]).range(Range::new(-10.0, 10.0)),
                ParamDef::angle("a", "a", 0.0),
                ParamDef::float("f", "f", 0.5, Range::new(0.0, 1.0)),
            ],
        );
        schema.validate().expect("valid");
        let mut v = ParamValues::new();
        assert_eq!(
            v.set(&schema, "n", ParamValue::Int(40)),
            Some(ParamValue::Int(16))
        );
        assert_eq!(
            v.set(&schema, "n", ParamValue::Int(-3)),
            Some(ParamValue::Int(2))
        );
        // No alpha: forced opaque. Negative channels are clamped; above 1 is HDR and kept.
        assert_eq!(
            v.set(&schema, "c", ParamValue::Colour([-1.0, 2.0, 0.5, 0.3])),
            Some(ParamValue::Colour([0.0, 2.0, 0.5, 1.0]))
        );
        assert_eq!(
            v.set(&schema, "ca", ParamValue::Colour([0.5, 0.5, 0.5, 1.7])),
            Some(ParamValue::Colour([0.5, 0.5, 0.5, 1.0]))
        );
        assert_eq!(
            v.set(&schema, "p", ParamValue::Point([25.0, -3.0])),
            Some(ParamValue::Point([10.0, -3.0]))
        );
        // Angles are unbounded by default so animations can turn many times.
        assert_eq!(
            v.set(&schema, "a", ParamValue::Angle(725.0)),
            Some(ParamValue::Angle(725.0))
        );
        // Non-finite numbers fall back to the default instead of poisoning renders.
        assert_eq!(
            v.set(&schema, "f", ParamValue::Float(f64::NAN)),
            Some(ParamValue::Float(0.5))
        );
        assert_eq!(
            v.set(&schema, "f", ParamValue::Float(f64::INFINITY)),
            Some(ParamValue::Float(0.5))
        );
        assert_eq!(
            v.set(&schema, "p", ParamValue::Point([f64::NAN, 1.0])),
            Some(ParamValue::Point([0.0, 0.0]))
        );
        // Values stored under a later, narrower schema are clamped when read.
        let mut raw = ParamValues::new();
        raw.values.insert("f".into(), ParamValue::Float(3.0));
        assert_eq!(raw.float(&schema, "f"), Some(1.0));
    }

    #[test]
    fn defaults_cover_every_param() {
        for schema in builtin::all() {
            let defaults = schema.defaults();
            for p in &schema.params {
                assert_eq!(defaults.get(&schema, &p.id), Some(p.kind.default_value()));
                assert!(defaults.is_default(&schema, &p.id));
                assert!(ParamValues::new().is_default(&schema, &p.id));
            }
        }
        let c = builtin::compositing();
        assert_eq!(c.defaults().choice(&c, "blend").as_deref(), Some("normal"));
        let m = builtin::motion_blur();
        assert_eq!(m.defaults().int(&m, "samples"), Some(8));
        assert_eq!(m.defaults().boolean(&m, "enabled"), Some(false));
        let picked = ParamDef::choice("q", "q", &[("a", "a"), ("b", "b")]).default_choice("b");
        assert_eq!(picked.kind.default_value(), ParamValue::Choice("b".into()));
    }

    #[test]
    fn units_format_for_display() {
        assert_eq!(Unit::Percent.format(0.86, 0), "86");
        assert_eq!(Unit::Percent.format(1.12, 1), "112.0");
        assert_eq!(Unit::Pixels.format(1012.44, 1), "1012.4");
        assert_eq!(Unit::Degrees.format(-0.01, 1), "0.0");
        assert_eq!(Unit::None.format(-0.4, 0), "0");
        assert_eq!(Unit::Decibels.format(-3.0, 1), "-3.0");
        assert_eq!(Unit::Percent.to_display(0.5), 50.0);
        assert_eq!(Unit::Percent.from_display(50.0), 0.5);
        assert_eq!(Unit::Seconds.from_display(2.5), 2.5);
        assert_eq!(Unit::Degrees.symbol(), "°");
        assert_eq!(Unit::None.symbol(), "");
        let schema = builtin::transform();
        assert_eq!(schema.get("scale").expect("scale").format(1.12), "112.0");
        assert_eq!(
            schema.get("rotation").expect("rotation").format(45.0),
            "45.0"
        );
        let c = builtin::compositing();
        assert_eq!(c.get("opacity").expect("opacity").format(0.86), "86");
        let m = builtin::motion_blur();
        assert_eq!(m.get("samples").expect("samples").format(16.0), "16");
    }

    #[test]
    fn schema_errors() {
        let dup = ParamSchema::new(
            "x",
            "x",
            vec![
                ParamDef::boolean("a", "a", true),
                ParamDef::boolean("a", "a", true),
            ],
        );
        assert_eq!(dup.validate(), Err(SchemaError::DuplicateId("a".into())));
        let bad = ParamSchema::new("x", "x", vec![ParamDef::boolean("A b", "a", true)]);
        assert!(matches!(bad.validate(), Err(SchemaError::BadId(_))));
        let default = ParamSchema::new(
            "x",
            "x",
            vec![ParamDef::float("a", "a", 5.0, Range::new(0.0, 1.0))],
        );
        assert_eq!(default.validate(), Err(SchemaError::BadDefault("a".into())));
        let inverted = ParamSchema::new(
            "x",
            "x",
            vec![ParamDef::float("a", "a", 0.0, Range::new(0.0, 1.0)).soft(Range::new(1.0, 0.0))],
        );
        assert_eq!(inverted.validate(), Err(SchemaError::BadRange("a".into())));
        let switch = ParamSchema::new(
            "x",
            "x",
            vec![ParamDef::float("a", "a", 0.0, Range::new(0.0, 1.0))],
        )
        .header_switch("a");
        assert_eq!(
            switch.validate(),
            Err(SchemaError::BadHeaderSwitch("a".into()))
        );
    }

    #[test]
    fn keyframeable_follows_the_kind() {
        let t = builtin::transform();
        assert!(t.params.iter().all(|p| p.keyframeable));
        let m = builtin::motion_blur();
        assert!(!m.get("enabled").expect("enabled").keyframeable);
        assert!(m.get("shutter").expect("shutter").keyframeable);
        let c = builtin::compositing();
        assert!(!c.get("blend").expect("blend").keyframeable);
        let text = ParamDef::text("t", "t", "", false);
        assert!(!text.keyframeable);
        assert!(text.keyframeable(true).keyframeable);
    }

    #[test]
    fn values_round_trip_through_json() {
        let schema = builtin::transform();
        let mut values = schema.defaults();
        values.set(&schema, "rotation", ParamValue::Angle(725.0));
        values.set(&schema, "position", ParamValue::Point([1012.4, 548.0]));
        let json = serde_json::to_string(&values).expect("serialises");
        assert!(json.contains(r#""rotation":{"angle":725.0}"#), "{json}");
        let back: ParamValues = serde_json::from_str(&json).expect("parses");
        assert_eq!(back, values);
        // Unknown ids from a newer effect version survive a round trip.
        let newer: ParamValues =
            serde_json::from_str(r#"{"scale":{"float":2.0},"skew":{"float":0.1}}"#)
                .expect("parses");
        assert_eq!(newer.float(&schema, "scale"), Some(2.0));
        let again: ParamValues =
            serde_json::from_str(&serde_json::to_string(&newer).expect("ser")).expect("de");
        assert_eq!(again, newer);
        for schema in builtin::all() {
            let json = serde_json::to_string(&schema).expect("schema serialises");
            let back: ParamSchema = serde_json::from_str(&json).expect("schema parses");
            assert_eq!(back, schema);
        }
    }
}
