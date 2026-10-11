//! Transitions: a shader, a parameter schema and an on-demand thumbnail per transition,
//! discovered from `assets/transitions/<id>/` (see the README there) into a
//! [`TransitionRegistry`], and drawn between two pictures by a [`TransitionNode`].
//!
//! A folder holds `transition.json`, the manifest (id, i18n name key, category, offered
//! [`Direction`]s, thumbnail progress, and parameters in the `project::params::ParamDef`
//! JSON format), and `shader.wgsl`, compiled after `common.wgsl` and the shared prelude
//! `transition.wgsl`, which lays out the uniform every transition reads. Discovery
//! checks both; a broken folder is reported in [`TransitionRegistry::problems`] and
//! skipped, never fatal. Debug builds read the source tree's folder and reload an
//! edited shader through the [`crate::shaders::ShaderLibrary`] watcher; release builds
//! use the folders embedded at build time ([`assets::transitions::EMBEDDED`]).
//!
//! A manifest may list extra [`Pass`]es: fragment entry points of the same shader, each
//! drawn in order into its own target at a declared fraction of the output's size
//! before `fs` composes the result. Every later pass reads the earlier targets, so a
//! transition can blur at a reduced resolution or build a pyramid for minification.
//!
//! This crate reads the parameters only as far as it packs them into the uniform;
//! `project::params` owns the schema, and the engine builds a `ParamSchema` from
//! [`Transition::schema_params`] with the transition's id and name.

mod easing;
mod node;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;
use serde_json::Value as Json;

use crate::shaders::{self, ShaderLibrary};

pub use easing::Easing;
pub use node::TransitionNode;

/// The embedded shader every transition shader is compiled after (with `common`).
pub const PRELUDE: &str = "transition";

/// Parameter slots in the uniform: a schema may have at most this many parameters.
pub const MAX_PARAMS: usize = 8;

/// Extra passes a manifest may list before `fs`; the prelude binds one target each.
pub const MAX_PASSES: usize = 8;

/// The smallest fraction of the output's extent a pass's target may have.
pub const MIN_PASS_SCALE: f64 = 1.0 / 16.0;

/// The largest floor a pass may set on its target's extent, in pixels.
pub const MAX_PASS_MIN: u32 = 16384;

/// The way a transition travels (a wipe's edge, a push's motion), in picture
/// coordinates with y down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
    UpLeft,
    UpRight,
    DownLeft,
    DownRight,
}

impl Direction {
    /// In the order of their index in the uniform.
    pub const ALL: [Direction; 8] = [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
        Direction::UpLeft,
        Direction::UpRight,
        Direction::DownLeft,
        Direction::DownRight,
    ];

    /// The id manifests and projects use.
    pub fn id(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
            Self::UpLeft => "up_left",
            Self::UpRight => "up_right",
            Self::DownLeft => "down_left",
            Self::DownRight => "down_right",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.id() == id)
    }

    /// The index the shader sees in `tr.direction`.
    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|d| *d == self).unwrap_or(0) as u32
    }

    /// Unit vector of travel in uv (x right, y down).
    pub fn vector(self) -> [f32; 2] {
        let d = std::f32::consts::FRAC_1_SQRT_2;
        match self {
            Self::Left => [-1.0, 0.0],
            Self::Right => [1.0, 0.0],
            Self::Up => [0.0, -1.0],
            Self::Down => [0.0, 1.0],
            Self::UpLeft => [-d, -d],
            Self::UpRight => [d, -d],
            Self::DownLeft => [-d, d],
            Self::DownRight => [d, d],
        }
    }
}

/// What the shader receives for a parameter, by its `project::params::ParamKind`.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    /// `float` or `int`.
    Number,
    /// `angle`, given in degrees, packed in radians.
    Angle,
    Bool,
    /// `choice`: the option ids in order, packed as the index.
    Choice(Vec<String>),
    /// `colour`, straight-alpha linear RGBA.
    Colour,
    Point,
}

/// A parameter value handed to a [`TransitionNode`] (the engine converts the project's
/// values).
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    /// A number, or an angle in degrees.
    Number(f64),
    Bool(bool),
    Choice(String),
    Colour([f32; 4]),
    Point([f64; 2]),
}

/// One parameter as the uniform needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub id: String,
    pub kind: ParamKind,
    pub default: ParamValue,
}

impl Param {
    /// `value` as the shader's vec4, or `None` when it does not fit the kind.
    pub fn pack(&self, value: &ParamValue) -> Option<[f32; 4]> {
        Some(match (&self.kind, value) {
            (ParamKind::Number, ParamValue::Number(v)) => [*v as f32, 0.0, 0.0, 0.0],
            (ParamKind::Angle, ParamValue::Number(v)) => [v.to_radians() as f32, 0.0, 0.0, 0.0],
            (ParamKind::Bool, ParamValue::Bool(v)) => [f32::from(u8::from(*v)), 0.0, 0.0, 0.0],
            (ParamKind::Choice(options), ParamValue::Choice(id)) => {
                [options.iter().position(|o| o == id)? as f32, 0.0, 0.0, 0.0]
            }
            (ParamKind::Colour, ParamValue::Colour(c)) => *c,
            (ParamKind::Point, ParamValue::Point([x, y])) => [*x as f32, *y as f32, 0.0, 0.0],
            _ => return None,
        })
    }
}

/// An extra pass: the shader's fragment entry point `entry` drawn into a target of its
/// own, which `fs` and every later pass read as `pass<i>` (`sample_pass(i, uv)`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pass {
    pub entry: &'static str,
    /// The target's extent along the direction of travel as a fraction of the output's
    /// (x for a transition without a direction or travelling diagonally, y for up and
    /// down), from [`MIN_PASS_SCALE`] to 1.
    pub along: f64,
    /// The same across the direction of travel.
    pub across: f64,
    /// The least extent of the target on either axis, in pixels, unless the output's is
    /// smaller: a reduced resolution saves time on large frames, and small ones (previews,
    /// thumbnails) keep their detail.
    pub min: u32,
}

impl Pass {
    /// The target's size in pixels for an output of `size` travelling in `direction`
    /// (at least 1 by 1).
    pub fn size(&self, size: (u32, u32), direction: Option<Direction>) -> (u32, u32) {
        let scale = |extent: u32, f: f64| {
            ((f64::from(extent) * f).round() as u32)
                .max(self.min.min(extent))
                .max(1)
        };
        if matches!(direction, Some(Direction::Up | Direction::Down)) {
            (scale(size.0, self.across), scale(size.1, self.along))
        } else {
            (scale(size.0, self.along), scale(size.1, self.across))
        }
    }
}

/// One discovered transition.
#[derive(Debug)]
pub struct Transition {
    id: String,
    name: String,
    category: String,
    directions: Vec<Direction>,
    thumbnail: f64,
    params: Vec<Param>,
    schema_params: Vec<Json>,
    passes: Vec<Pass>,
    shader: &'static str,
    source: String,
    path: Option<PathBuf>,
}

impl Transition {
    /// The stable id (its folder name).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The i18n key of the display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The browser group; its label key is `transition.category.<category>`.
    pub fn category(&self) -> &str {
        &self.category
    }

    /// The directions offered, the default first; empty without a direction.
    pub fn directions(&self) -> &[Direction] {
        &self.directions
    }

    /// `direction` when offered, else the default (`None` without directions).
    pub fn direction_or_default(&self, direction: Option<Direction>) -> Option<Direction> {
        direction
            .filter(|d| self.directions.contains(d))
            .or_else(|| self.directions.first().copied())
    }

    /// The progress the browser's thumbnail is rendered at.
    pub fn thumbnail(&self) -> f64 {
        self.thumbnail
    }

    /// The parameters in manifest order (their uniform slots).
    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// The manifest's parameter definitions as written, in the
    /// `project::params::ParamDef` JSON format.
    pub fn schema_params(&self) -> &[Json] {
        &self.schema_params
    }

    /// The extra passes drawn before `fs`, in order; empty for a single-pass transition.
    pub fn passes(&self) -> &[Pass] {
        &self.passes
    }

    /// The fragment entry points the shader must define: `fs`, then each pass's once.
    pub fn entries(&self) -> Vec<&'static str> {
        let mut entries = vec!["fs"];
        for p in &self.passes {
            if !entries.contains(&p.entry) {
                entries.push(p.entry);
            }
        }
        entries
    }

    /// The shader's name in the [`ShaderLibrary`], `transition/<id>`.
    pub fn shader(&self) -> &'static str {
        self.shader
    }

    /// The shader's source as discovered.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The shader file, when discovered from a folder on disk.
    pub fn shader_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// A folder that could not be loaded, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub folder: String,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transition {:?}: {}", self.folder, self.message)
    }
}

/// Every transition that loaded, sorted by id, and the folders that did not.
#[derive(Debug, Default)]
pub struct TransitionRegistry {
    transitions: Vec<Arc<Transition>>,
    problems: Vec<Problem>,
}

impl TransitionRegistry {
    /// The folders embedded at build time.
    pub fn bundled() -> Self {
        let mut registry = Self::default();
        for e in assets::transitions::EMBEDDED {
            registry.load(e.folder, e.manifest, e.shader, None);
        }
        registry
    }

    /// Every folder in `dir`. An unreadable `dir` is one problem.
    pub fn discover(dir: &Path) -> Self {
        let mut registry = Self::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                registry.problems.push(Problem {
                    folder: dir.display().to_string(),
                    message: format!("the folder cannot be read: {e}"),
                });
                return registry;
            }
        };
        let mut folders: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        folders.sort();
        for folder in folders {
            let name = folder
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let read = |file: &str| match std::fs::read_to_string(folder.join(file)) {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("{file} cannot be read: {e}")),
            };
            match (
                read(assets::transitions::MANIFEST),
                read(assets::transitions::SHADER),
            ) {
                (Ok(manifest), Ok(shader)) => registry.load(
                    &name,
                    manifest.as_deref(),
                    shader.as_deref(),
                    Some(folder.join(assets::transitions::SHADER)),
                ),
                (Err(message), _) | (_, Err(message)) => registry.problems.push(Problem {
                    folder: name,
                    message,
                }),
            }
        }
        registry
    }

    /// The app's transitions, discovered once per process: the source tree's
    /// `assets/transitions/` in debug builds (the embedded set when it is gone), the
    /// embedded set in release builds. Problems are logged.
    pub fn standard() -> Arc<Self> {
        static STANDARD: OnceLock<Arc<TransitionRegistry>> = OnceLock::new();
        STANDARD
            .get_or_init(|| {
                #[cfg(debug_assertions)]
                let registry = {
                    let dir = assets::transitions::source_dir();
                    if dir.is_dir() {
                        Self::discover(&dir)
                    } else {
                        Self::bundled()
                    }
                };
                #[cfg(not(debug_assertions))]
                let registry = Self::bundled();
                for problem in &registry.problems {
                    tracing::warn!("{problem}");
                }
                Arc::new(registry)
            })
            .clone()
    }

    pub fn get(&self, id: &str) -> Option<&Arc<Transition>> {
        self.transitions.iter().find(|t| t.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<Transition>> {
        self.transitions.iter()
    }

    pub fn len(&self) -> usize {
        self.transitions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty()
    }

    /// The folders that did not load.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// Adds every transition's shader to `library` (watched there when it came from a
    /// folder on disk). A shader the library rejects is logged.
    pub fn add_shaders(&self, library: &ShaderLibrary) {
        for t in &self.transitions {
            if let Err(e) = library.add_external(
                t.shader,
                PRELUDE,
                t.source.clone(),
                t.path.clone(),
                &t.entries(),
            ) {
                tracing::warn!("transition {:?}: {e}", t.id);
            }
        }
    }

    fn load(
        &mut self,
        folder: &str,
        manifest: Option<&str>,
        shader: Option<&str>,
        path: Option<PathBuf>,
    ) {
        match parse(folder, manifest, shader, path) {
            Ok(t) => {
                self.transitions.push(Arc::new(t));
                self.transitions.sort_by(|a, b| a.id.cmp(&b.id));
            }
            Err(message) => self.problems.push(Problem {
                folder: folder.to_owned(),
                message,
            }),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    name: String,
    category: String,
    #[serde(default)]
    directions: Vec<String>,
    #[serde(default = "half")]
    thumbnail: f64,
    #[serde(default)]
    params: Vec<Json>,
    #[serde(default)]
    passes: Vec<Json>,
}

fn half() -> f64 {
    0.5
}

fn parse(
    folder: &str,
    manifest: Option<&str>,
    shader: Option<&str>,
    path: Option<PathBuf>,
) -> Result<Transition, String> {
    let manifest_file = assets::transitions::MANIFEST;
    let manifest = manifest.ok_or_else(|| format!("{manifest_file} is missing"))?;
    let m: Manifest =
        serde_json::from_str(manifest).map_err(|e| format!("{manifest_file}: {e}"))?;
    let bad = |what: String| format!("{manifest_file}: {what}");
    let is_id = |s: &str, extra: u8| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == extra)
    };
    if !is_id(&m.id, b'-') {
        return Err(bad(format!(
            "id {:?} is not lowercase ASCII letters, digits and '-'",
            m.id
        )));
    }
    if m.id != folder {
        return Err(bad(format!(
            "id {:?} differs from the folder name {folder:?}",
            m.id
        )));
    }
    if !assets::i18n::is_valid_key(&m.name) {
        return Err(bad(format!("name {:?} is not an i18n key", m.name)));
    }
    if !is_id(&m.category, b'_') {
        return Err(bad(format!(
            "category {:?} is not lowercase ASCII letters, digits and '_'",
            m.category
        )));
    }
    let mut directions = Vec::new();
    for id in &m.directions {
        let d = Direction::from_id(id).ok_or_else(|| bad(format!("unknown direction {id:?}")))?;
        if directions.contains(&d) {
            return Err(bad(format!("direction {id:?} is listed twice")));
        }
        directions.push(d);
    }
    if !(0.0..=1.0).contains(&m.thumbnail) {
        return Err(bad(format!(
            "thumbnail progress {} is outside 0..1",
            m.thumbnail
        )));
    }
    if m.params.len() > MAX_PARAMS {
        return Err(bad(format!(
            "{} parameters; the uniform holds {MAX_PARAMS}",
            m.params.len()
        )));
    }
    let mut params: Vec<Param> = Vec::new();
    for json in &m.params {
        let p = param(json).map_err(&bad)?;
        if params.iter().any(|q| q.id == p.id) {
            return Err(bad(format!("parameter {:?} appears twice", p.id)));
        }
        params.push(p);
    }
    if m.passes.len() > MAX_PASSES {
        return Err(bad(format!(
            "{} passes; at most {MAX_PASSES} run before fs",
            m.passes.len()
        )));
    }
    let passes = m
        .passes
        .iter()
        .enumerate()
        .map(|(i, json)| pass(json).map_err(|e| bad(format!("pass {i}: {e}"))))
        .collect::<Result<Vec<Pass>, String>>()?;

    let shader_file = assets::transitions::SHADER;
    let source = shader.ok_or_else(|| format!("{shader_file} is missing"))?;
    let name = intern(format!("{PRELUDE}/{}", m.id));
    let embedded = |n| shaders::embedded(n).unwrap_or_default();
    let transition = Transition {
        id: m.id,
        name: m.name,
        category: m.category,
        directions,
        thumbnail: m.thumbnail,
        params,
        schema_params: m.params,
        passes,
        shader: name,
        source: source.to_owned(),
        path,
    };
    shaders::validate_external(
        name,
        &shaders::compose_external(embedded("common"), embedded(PRELUDE), source),
        &transition.entries(),
    )
    .map_err(|e| format!("{shader_file}: {e}"))?;
    Ok(transition)
}

/// A pass as the manifest writes it: `{"entry": "blur", "scale": 0.25, "min": 256}`, or
/// with `"scale": {"along": 0.25, "across": 1}`; the scale defaults to 1, the floor to
/// none.
fn pass(json: &Json) -> Result<Pass, String> {
    let fields = json.as_object().ok_or("not an object")?;
    if let Some(field) = fields
        .keys()
        .find(|k| !["entry", "scale", "min"].contains(&k.as_str()))
    {
        return Err(format!("unknown field {field:?}"));
    }
    let entry = fields
        .get("entry")
        .and_then(Json::as_str)
        .ok_or("no string \"entry\"")?;
    let identifier = entry
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic())
        && entry
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !identifier {
        return Err(format!(
            "entry {entry:?} is not ASCII letters, digits and '_' starting with a letter"
        ));
    }
    if entry == "fs" {
        return Err("entry \"fs\" is the final pass, drawn after the listed ones".to_owned());
    }
    let factor = |v: &Json| {
        v.as_f64()
            .filter(|f| (MIN_PASS_SCALE..=1.0).contains(f))
            .ok_or_else(|| format!("scale {v} is not a number from 1/16 to 1"))
    };
    let (along, across) = match fields.get("scale") {
        None => (1.0, 1.0),
        Some(Json::Object(axes)) => {
            if let Some(axis) = axes
                .keys()
                .find(|k| !["along", "across"].contains(&k.as_str()))
            {
                return Err(format!("unknown scale axis {axis:?}"));
            }
            let axis = |name| axes.get(name).map_or(Ok(1.0), factor);
            (axis("along")?, axis("across")?)
        }
        Some(v) => {
            let f = factor(v)?;
            (f, f)
        }
    };
    let min = match fields.get("min") {
        None => 1,
        Some(v) => v
            .as_u64()
            .filter(|m| (1..=u64::from(MAX_PASS_MIN)).contains(m))
            .ok_or_else(|| format!("min {v} is not a whole number from 1 to {MAX_PASS_MIN}"))?
            as u32,
    };
    Ok(Pass {
        entry: intern(entry.to_owned()),
        along,
        across,
        min,
    })
}

/// The packing-relevant part of a `ParamDef`: `{"id": .., "kind": {"<kind>": {"default":
/// .., ..}}, ..}`.
fn param(json: &Json) -> Result<Param, String> {
    let id = json
        .get("id")
        .and_then(Json::as_str)
        .ok_or("a parameter has no string \"id\"")?
        .to_owned();
    let valid_id = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if !valid_id {
        return Err(format!(
            "parameter id {id:?} is not lowercase ASCII, digits, '.', '_' or '-'"
        ));
    }
    let wrong = |what: &str| format!("parameter {id:?}: {what}");
    if !json.get("label").is_some_and(Json::is_string) {
        return Err(wrong("no string \"label\""));
    }
    let kind = json
        .get("kind")
        .and_then(Json::as_object)
        .filter(|k| k.len() == 1)
        .ok_or_else(|| wrong("\"kind\" is not an object with one kind"))?;
    let (kind, body) = kind.iter().next().expect("one entry");
    let default = body
        .get("default")
        .ok_or_else(|| wrong("the kind has no \"default\""))?;
    let number = || {
        default
            .as_f64()
            .ok_or_else(|| wrong("the default is not a number"))
    };
    let floats = |n: usize| {
        default
            .as_array()
            .filter(|a| a.len() == n)
            .and_then(|a| a.iter().map(Json::as_f64).collect::<Option<Vec<f64>>>())
            .ok_or_else(|| wrong(&format!("the default is not {n} numbers")))
    };
    let (kind, default) = match kind.as_str() {
        "float" | "int" => (ParamKind::Number, ParamValue::Number(number()?)),
        "angle" => (ParamKind::Angle, ParamValue::Number(number()?)),
        "bool" => (
            ParamKind::Bool,
            ParamValue::Bool(
                default
                    .as_bool()
                    .ok_or_else(|| wrong("the default is not a boolean"))?,
            ),
        ),
        "choice" => {
            let options: Vec<String> = body
                .get("options")
                .and_then(Json::as_array)
                .and_then(|o| {
                    o.iter()
                        .map(|o| o.get("id").and_then(Json::as_str).map(str::to_owned))
                        .collect()
                })
                .ok_or_else(|| wrong("\"options\" is not a list of options with ids"))?;
            let default = default
                .as_str()
                .filter(|d| options.iter().any(|o| o == d))
                .ok_or_else(|| wrong("the default is not one of the options"))?
                .to_owned();
            (ParamKind::Choice(options), ParamValue::Choice(default))
        }
        "colour" => {
            let c = floats(4)?;
            (
                ParamKind::Colour,
                ParamValue::Colour([c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]),
            )
        }
        "point" => {
            let p = floats(2)?;
            (ParamKind::Point, ParamValue::Point([p[0], p[1]]))
        }
        "text" => return Err(wrong("text parameters cannot reach a shader")),
        other => return Err(wrong(&format!("unknown kind {other:?}"))),
    };
    Ok(Param { id, kind, default })
}

/// A shader name that lives for the process, as pipeline keys need: each distinct name
/// is allocated once however many registries are built.
fn intern(name: String) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(n) = names.get(name.as_str()) {
        return n;
    }
    let leaked: &'static str = Box::leak(name.into_boxed_str());
    names.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directions_round_trip_and_are_unit_vectors() {
        for (i, d) in Direction::ALL.into_iter().enumerate() {
            assert_eq!(Direction::from_id(d.id()), Some(d));
            assert_eq!(d.index() as usize, i);
            let [x, y] = d.vector();
            assert!((x * x + y * y - 1.0).abs() < 1e-6, "{d:?}");
        }
        assert_eq!(Direction::from_id("sideways"), None);
    }

    #[test]
    fn values_pack_by_kind() {
        let p = |kind, default| Param {
            id: "p".into(),
            kind,
            default,
        };
        let angle = p(ParamKind::Angle, ParamValue::Number(0.0));
        let [r, ..] = angle.pack(&ParamValue::Number(180.0)).expect("fits");
        assert!((r - std::f32::consts::PI).abs() < 1e-6);
        let choice = p(
            ParamKind::Choice(vec!["a".into(), "b".into()]),
            ParamValue::Choice("a".into()),
        );
        assert_eq!(
            choice.pack(&ParamValue::Choice("b".into())),
            Some([1.0, 0.0, 0.0, 0.0])
        );
        assert_eq!(choice.pack(&ParamValue::Choice("c".into())), None);
        let flag = p(ParamKind::Bool, ParamValue::Bool(false));
        assert_eq!(
            flag.pack(&ParamValue::Bool(true)),
            Some([1.0, 0.0, 0.0, 0.0])
        );
        assert_eq!(flag.pack(&ParamValue::Number(1.0)), None, "wrong kind");
        let point = p(ParamKind::Point, ParamValue::Point([0.0, 0.0]));
        assert_eq!(
            point.pack(&ParamValue::Point([3.0, -4.0])),
            Some([3.0, -4.0, 0.0, 0.0])
        );
    }

    #[test]
    fn interned_names_are_shared() {
        let a = intern("transition/x".to_owned());
        let b = intern("transition/x".to_owned());
        assert!(std::ptr::eq(a, b));
    }
}
