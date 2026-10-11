//! The project file: a versioned envelope around the edit tree, and the migrations that
//! bring files written by older versions up to the current schema.
//!
//! A project file is JSON:
//!
//! ```json
//! {
//!   "format": "glass.project",
//!   "schema_version": 9,
//!   "project": { "uid": "...", "name": "...", "next_id": 7, "settings": {...}, "media": {...}, "sequences": [...] }
//! }
//! ```
//!
//! [`save`] always writes [`SCHEMA_VERSION`]. [`load`] reads the envelope, then runs the
//! migrations from the file's version up to the current one, one step at a time, on the
//! untyped `project` value (`migrate_v1_to_v2`, then `migrate_v2_to_v3`, and so on), and
//! only then deserialises the current [`Project`] and validates it. A file from a newer
//! version fails with [`LoadError::NewerVersion`] instead of being guessed at.
//!
//! Each migration is a pure function of the previous version's `project` value, so it can
//! be tested on its own, and it is never changed once released: a later change to the
//! model is a new version with a new migration. `tests/fixtures/projects/` holds a file
//! written by every version (`v<N>-<name>.json`); `crates/project/tests/schema.rs` loads
//! each of them.
//!
//! Adding version N+1: bump [`SCHEMA_VERSION`], write `migrate_vN_to_vN+1`, append it to
//! [`MIGRATIONS`], change the model, and add `v<N+1>-<name>.json` fixtures saved by the new
//! build next to the existing `v<N>` ones (which stay, unchanged, as prior-version
//! fixtures).
//!
//! [`save`] writes one canonical form, so a project file diffs cleanly in git and a
//! saved file loads and saves again to the same bytes:
//!
//! - pretty JSON: a 2-space indent, one key or array element per line, `\n` line ends
//!   and one trailing newline;
//! - the envelope and every node serialise as structs, fields in declaration order (the
//!   order in the model's source; Decision 45 lists the item, clip and track orders);
//! - every map has its keys in ascending byte order: clip `keyframes` by property id
//!   (not [`crate::keyframe::ClipParam`]'s declaration order), effect `keyframes` and
//!   `params` by parameter id;
//! - lists keep model order (sequences, tracks, items, transitions, markers by position,
//!   media references by id); ids are numbers, times reduced fractions;
//! - floats in their shortest form that parses back to the same value (`serde_json`'s
//!   `float_roundtrip` feature makes the parse exact);
//! - nothing from the machine or the clock: no timestamps, and no hash-ordered
//!   collection reaches the output.
//!
//! [`load`] reads any layout and key order (compact files, files written by other
//! tools), and saving such a file writes the canonical form. The form is formatting
//! only, so it did not change [`SCHEMA_VERSION`] (Decision 74).
//!
//! Versions:
//!
//! - v1: the first edit tree. `media` is a list of references.
//! - v2: `media` is an object, `{"refs": [...]}`, so the media pool can add its bins and
//!   metadata next to the references.
//! - v3: sequences and clips carry `markers` (sequence markers in sequence time, clip
//!   markers in source time); tracks may also be `subtitle` or `adjustment`, holding
//!   `subtitle` and `adjustment` items. Gaps stay implicit (no gap items).
//! - v4: clips carry their properties (`crate::clip`): `speed` (a speed map of keys, one
//!   at source time 0 for a constant speed), `enabled`, `gain_db`, `transform` (offsets
//!   from the canvas and layer centres), `crop`, `opacity`, `blend` and `effects` (the
//!   effect stack, each effect with its own node id).
//! - v5: tracks carry `transitions` (`crate::transition`), each a node with its own id on
//!   a cut between two clips, or between a clip and nothing next to a gap.
//! - v6: the project carries `settings` (`crate::settings`: the format new sequences
//!   start with, the colour space and the cache folder), and every sequence its own
//!   `resolution`, `drop_frame` and `channel_layout` beside its frame and sample rates.
//! - v7: clips and effects carry `keyframes` (`crate::keyframe`): curves of keys in source
//!   time by animated parameter, under the clip property's id (`"opacity"`,
//!   `"crop.left"`) or the effect parameter's id.
//! - v8: sequences carry `compound` (a compound clip's contents, hidden from the browser),
//!   and a clip names its source as `"media": <id>` as before or `"sequence": <id>` for a
//!   nested sequence (`crate::nest`).
//! - v9: `media` is the media pool (`crate::media_pool`), `{"bins": [...], "items": [...],
//!   "label_names": {...}}`: each reference became an item in a bin with its name, kind,
//!   files, content hash, size, card, import time, facts, metadata, colour label and tags;
//!   the root bin (id 0) is named after the project. Tracks carry their switches
//!   (`crate::switches`): `muted`, `solo`, `visible`, `locked` and `height`.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::model::Project;
use crate::validate::ValidationError;

/// The `format` field every project file carries.
pub const FORMAT: &str = "glass.project";

/// The schema version this build writes, and the newest it reads.
pub const SCHEMA_VERSION: u32 = 9;

/// The oldest schema version this build reads.
pub const OLDEST_VERSION: u32 = 1;

/// One migration step: the `project` value of version N to that of version N+1.
pub type Migration = fn(Value) -> Result<Value, String>;

/// `MIGRATIONS[i]` takes version `OLDEST_VERSION + i` to the next one.
pub const MIGRATIONS: [Migration; (SCHEMA_VERSION - OLDEST_VERSION) as usize] = [
    migrate_v1_to_v2,
    migrate_v2_to_v3,
    migrate_v3_to_v4,
    migrate_v4_to_v5,
    migrate_v5_to_v6,
    migrate_v6_to_v7,
    migrate_v7_to_v8,
    migrate_v8_to_v9,
];

/// Why a project file could not be read. [`LoadError::i18n_key`] names the message to
/// show.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("not JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("not a Glass project file: {0}")]
    NotAProject(&'static str),
    #[error("schema version {found} is newer than this build reads ({supported})")]
    NewerVersion { found: u64, supported: u32 },
    #[error("schema version {found} is older than this build reads ({oldest})")]
    OlderVersion { found: u64, oldest: u32 },
    #[error("migrating from schema version {from} failed: {message}")]
    Migration { from: u32, message: String },
    #[error("the project does not match schema version {version}: {error}")]
    Model {
        version: u32,
        #[source]
        error: serde_json::Error,
    },
    #[error("the project is inconsistent: {0}")]
    Invalid(#[from] ValidationError),
}

impl LoadError {
    /// The key of the user-facing message in `assets/i18n/en.json`.
    /// `project.open_error.newer_version` takes the placeholders `{found}` and
    /// `{supported}`; the others take none.
    pub fn i18n_key(&self) -> &'static str {
        match self {
            LoadError::NotAProject(_) => "project.open_error.not_a_project",
            LoadError::NewerVersion { .. } => "project.open_error.newer_version",
            LoadError::OlderVersion { .. } => "project.open_error.older_version",
            LoadError::Json(_)
            | LoadError::Migration { .. }
            | LoadError::Model { .. }
            | LoadError::Invalid(_) => "project.open_error.damaged",
        }
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    format: &'static str,
    schema_version: u32,
    project: &'a Project,
}

/// The project as a file at [`SCHEMA_VERSION`]: pretty JSON with a trailing newline.
pub fn save(project: &Project) -> Vec<u8> {
    let envelope = Envelope {
        format: FORMAT,
        schema_version: SCHEMA_VERSION,
        project,
    };
    let mut bytes =
        serde_json::to_vec_pretty(&envelope).expect("a project always serialises to JSON");
    bytes.push(b'\n');
    bytes
}

/// Reads a project file of any supported version, migrating it to the current schema,
/// and validates the tree.
pub fn load(bytes: &[u8]) -> Result<Project, LoadError> {
    let value = serde_json::from_slice(bytes).map_err(LoadError::Json)?;
    let (version, project) = open_envelope(value)?;
    let project = migrate(version, project)?;
    let project: Project = serde_json::from_value(project).map_err(|error| LoadError::Model {
        version: SCHEMA_VERSION,
        error,
    })?;
    project.validate()?;
    Ok(project)
}

/// The schema version and the `project` value of a file, checked against the versions
/// this build reads.
fn open_envelope(value: Value) -> Result<(u32, Value), LoadError> {
    let Value::Object(mut envelope) = value else {
        return Err(LoadError::NotAProject("not a JSON object"));
    };
    if envelope.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(LoadError::NotAProject("no \"format\": \"glass.project\""));
    }
    let found = envelope
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or(LoadError::NotAProject("no schema_version"))?;
    if found > u64::from(SCHEMA_VERSION) {
        return Err(LoadError::NewerVersion {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    if found < u64::from(OLDEST_VERSION) {
        return Err(LoadError::OlderVersion {
            found,
            oldest: OLDEST_VERSION,
        });
    }
    let project = envelope
        .remove("project")
        .ok_or(LoadError::NotAProject("no project"))?;
    Ok((found as u32, project))
}

/// Runs every migration from `version` to [`SCHEMA_VERSION`] in order.
pub fn migrate(version: u32, mut project: Value) -> Result<Value, LoadError> {
    for from in version..SCHEMA_VERSION {
        let step = MIGRATIONS[(from - OLDEST_VERSION) as usize];
        project = step(project).map_err(|message| LoadError::Migration { from, message })?;
    }
    Ok(project)
}

/// The `project` object of a value, for migrations.
fn object(value: &mut Value) -> Result<&mut Map<String, Value>, String> {
    value
        .as_object_mut()
        .ok_or_else(|| "project is not an object".to_string())
}

/// v1 → v2: `media`, a list of references, becomes `{"refs": [...]}`.
pub fn migrate_v1_to_v2(mut project: Value) -> Result<Value, String> {
    let root = object(&mut project)?;
    let refs = match root.remove("media") {
        Some(list @ Value::Array(_)) => list,
        None => Value::Array(Vec::new()),
        Some(_) => return Err("media is not a list".to_string()),
    };
    let mut media = Map::new();
    media.insert("refs".to_string(), refs);
    root.insert("media".to_string(), Value::Object(media));
    Ok(project)
}

/// v2 → v3: every sequence and every clip gets an empty `markers` list. v2 had only
/// video and audio tracks holding clips, so nothing else changes.
pub fn migrate_v2_to_v3(mut project: Value) -> Result<Value, String> {
    let root = object(&mut project)?;
    let Some(sequences) = root.get_mut("sequences") else {
        return Ok(project);
    };
    let sequences = sequences
        .as_array_mut()
        .ok_or_else(|| "sequences is not a list".to_string())?;
    for sequence in sequences {
        let sequence = sequence
            .as_object_mut()
            .ok_or_else(|| "a sequence is not an object".to_string())?;
        add_markers(sequence)?;
        let Some(tracks) = sequence.get_mut("tracks") else {
            continue;
        };
        for track in tracks
            .as_array_mut()
            .ok_or_else(|| "tracks is not a list".to_string())?
        {
            let Some(items) = track
                .as_object_mut()
                .ok_or_else(|| "a track is not an object".to_string())?
                .get_mut("items")
            else {
                continue;
            };
            for item in items
                .as_array_mut()
                .ok_or_else(|| "items is not a list".to_string())?
            {
                let item = item
                    .as_object_mut()
                    .ok_or_else(|| "an item is not an object".to_string())?;
                if item.get("type").and_then(Value::as_str) == Some("clip") {
                    add_markers(item)?;
                }
            }
        }
    }
    Ok(project)
}

/// Adds an empty `markers` list to a v2 node, which never has one.
fn add_markers(node: &mut Map<String, Value>) -> Result<(), String> {
    if node.contains_key("markers") {
        return Err("a v2 node already has markers".to_string());
    }
    node.insert("markers".to_string(), Value::Array(Vec::new()));
    Ok(())
}

/// v3 → v4: every clip gets its properties at their defaults (speed 1, enabled, 0 dB,
/// centred at 100%, uncropped, opaque, normal blend, no effects). Subtitle and adjustment
/// items do not change, nor does `next_id` (no effect nodes are created).
pub fn migrate_v3_to_v4(mut project: Value) -> Result<Value, String> {
    let root = object(&mut project)?;
    let Some(sequences) = root.get_mut("sequences") else {
        return Ok(project);
    };
    let sequences = sequences
        .as_array_mut()
        .ok_or_else(|| "sequences is not a list".to_string())?;
    for sequence in sequences {
        let Some(tracks) = sequence
            .as_object_mut()
            .ok_or_else(|| "a sequence is not an object".to_string())?
            .get_mut("tracks")
        else {
            continue;
        };
        for track in tracks
            .as_array_mut()
            .ok_or_else(|| "tracks is not a list".to_string())?
        {
            let Some(items) = track
                .as_object_mut()
                .ok_or_else(|| "a track is not an object".to_string())?
                .get_mut("items")
            else {
                continue;
            };
            for item in items
                .as_array_mut()
                .ok_or_else(|| "items is not a list".to_string())?
            {
                let item = item
                    .as_object_mut()
                    .ok_or_else(|| "an item is not an object".to_string())?;
                if item.get("type").and_then(Value::as_str) == Some("clip") {
                    add_clip_properties(item)?;
                }
            }
        }
    }
    Ok(project)
}

/// The v4 clip properties at their defaults, written out so the migration never follows
/// later model changes.
fn add_clip_properties(clip: &mut Map<String, Value>) -> Result<(), String> {
    let properties = serde_json::json!({
        "speed": { "keys": [{ "source_time": "0", "speed": { "num": 1, "den": 1 } }] },
        "enabled": true,
        "gain_db": 0.0,
        "transform": {
            "position": [0.0, 0.0],
            "scale": [1.0, 1.0],
            "rotation": 0.0,
            "anchor": [0.0, 0.0]
        },
        "crop": { "left": 0.0, "top": 0.0, "right": 0.0, "bottom": 0.0, "feather": 0.0 },
        "opacity": 1.0,
        "blend": "normal",
        "effects": []
    });
    let Value::Object(properties) = properties else {
        unreachable!("an object literal");
    };
    for (key, value) in properties {
        if clip.contains_key(&key) {
            return Err(format!("a v3 clip already has {key}"));
        }
        clip.insert(key, value);
    }
    Ok(())
}

/// v4 → v5: every track gets an empty `transitions` list. Nothing else changes, nor does
/// `next_id`.
pub fn migrate_v4_to_v5(mut project: Value) -> Result<Value, String> {
    let root = object(&mut project)?;
    let Some(sequences) = root.get_mut("sequences") else {
        return Ok(project);
    };
    let sequences = sequences
        .as_array_mut()
        .ok_or_else(|| "sequences is not a list".to_string())?;
    for sequence in sequences {
        let Some(tracks) = sequence
            .as_object_mut()
            .ok_or_else(|| "a sequence is not an object".to_string())?
            .get_mut("tracks")
        else {
            continue;
        };
        for track in tracks
            .as_array_mut()
            .ok_or_else(|| "tracks is not a list".to_string())?
        {
            let track = track
                .as_object_mut()
                .ok_or_else(|| "a track is not an object".to_string())?;
            if track.contains_key("transitions") {
                return Err("a v4 track already has transitions".to_string());
            }
            track.insert("transitions".to_string(), Value::Array(Vec::new()));
        }
    }
    Ok(project)
}

/// v5 → v6: the project gets `settings` at their defaults (1920 x 1080 at 30 fps without
/// drop-frame, 48 kHz stereo for new sequences, Rec.709, the cache folder `.glass-cache`
/// beside the project file), and every sequence a 1920 x 1080 canvas, drop-frame off and
/// stereo. Frame and sample rates are kept as they are (validation then checks them), nor
/// does `next_id` change.
pub fn migrate_v5_to_v6(mut project: Value) -> Result<Value, String> {
    let root = object(&mut project)?;
    if root.contains_key("settings") {
        return Err("a v5 project already has settings".to_string());
    }
    let settings = serde_json::json!({
        "sequence": {
            "resolution": { "width": 1920, "height": 1080 },
            "frame_rate": { "num": 30, "den": 1 },
            "drop_frame": false,
            "sample_rate": 48000,
            "channel_layout": "stereo"
        },
        "colour_space": "rec709",
        "cache_folder": { "relative": ".glass-cache" }
    });
    root.insert("settings".to_string(), settings);
    let Some(sequences) = root.get_mut("sequences") else {
        return Ok(project);
    };
    let sequences = sequences
        .as_array_mut()
        .ok_or_else(|| "sequences is not a list".to_string())?;
    for sequence in sequences {
        let sequence = sequence
            .as_object_mut()
            .ok_or_else(|| "a sequence is not an object".to_string())?;
        let format = [
            (
                "resolution",
                serde_json::json!({ "width": 1920, "height": 1080 }),
            ),
            ("drop_frame", Value::Bool(false)),
            ("channel_layout", Value::from("stereo")),
        ];
        for (key, value) in format {
            if sequence.contains_key(key) {
                return Err(format!("a v5 sequence already has {key}"));
            }
            sequence.insert(key.to_string(), value);
        }
    }
    Ok(project)
}

/// v6 → v7: every clip and every effect in a clip's stack gets an empty `keyframes` map
/// (nothing is animated). Nothing else changes, nor does `next_id`.
pub fn migrate_v6_to_v7(mut project: Value) -> Result<Value, String> {
    let root = object(&mut project)?;
    let Some(sequences) = root.get_mut("sequences") else {
        return Ok(project);
    };
    let sequences = sequences
        .as_array_mut()
        .ok_or_else(|| "sequences is not a list".to_string())?;
    for sequence in sequences {
        let Some(tracks) = sequence
            .as_object_mut()
            .ok_or_else(|| "a sequence is not an object".to_string())?
            .get_mut("tracks")
        else {
            continue;
        };
        for track in tracks
            .as_array_mut()
            .ok_or_else(|| "tracks is not a list".to_string())?
        {
            let Some(items) = track
                .as_object_mut()
                .ok_or_else(|| "a track is not an object".to_string())?
                .get_mut("items")
            else {
                continue;
            };
            for item in items
                .as_array_mut()
                .ok_or_else(|| "items is not a list".to_string())?
            {
                let item = item
                    .as_object_mut()
                    .ok_or_else(|| "an item is not an object".to_string())?;
                if item.get("type").and_then(Value::as_str) != Some("clip") {
                    continue;
                }
                add_keyframes(item, "clip")?;
                let Some(effects) = item.get_mut("effects") else {
                    continue;
                };
                for effect in effects
                    .as_array_mut()
                    .ok_or_else(|| "effects is not a list".to_string())?
                {
                    let effect = effect
                        .as_object_mut()
                        .ok_or_else(|| "an effect is not an object".to_string())?;
                    add_keyframes(effect, "effect")?;
                }
            }
        }
    }
    Ok(project)
}

/// Adds an empty `keyframes` map to a v6 clip or effect, which never has one.
fn add_keyframes(node: &mut Map<String, Value>, what: &str) -> Result<(), String> {
    if node.contains_key("keyframes") {
        return Err(format!("a v6 {what} already has keyframes"));
    }
    node.insert("keyframes".to_string(), Value::Object(Map::new()));
    Ok(())
}

/// v7 → v8: every sequence gets `"compound": false` (v7 has no compound clips). Clips
/// keep their `media` field, which v8 reads as a media source. Nothing else changes, nor
/// does `next_id`.
pub fn migrate_v7_to_v8(mut project: Value) -> Result<Value, String> {
    let root = object(&mut project)?;
    let Some(sequences) = root.get_mut("sequences") else {
        return Ok(project);
    };
    let sequences = sequences
        .as_array_mut()
        .ok_or_else(|| "sequences is not a list".to_string())?;
    for sequence in sequences {
        let sequence = sequence
            .as_object_mut()
            .ok_or_else(|| "a sequence is not an object".to_string())?;
        if sequence.contains_key("compound") {
            return Err("a v7 sequence already has compound".to_string());
        }
        sequence.insert("compound".to_string(), Value::Bool(false));
    }
    Ok(project)
}

/// v8 → v9: `media`, `{"refs": [...]}`, becomes the media pool: a root bin (id 0) named
/// after the project, and one item per reference in it, keeping its id, path and content
/// hash, named after the file without its extension, its kind guessed from the extension
/// (audio and still image extensions, anything else video), with nothing else known.
/// Every track gets its switches at their defaults: heard, unsoloed, visible, unlocked,
/// medium height. `next_id` does not change (the root bin is not a node).
pub fn migrate_v8_to_v9(mut project: Value) -> Result<Value, String> {
    let root = object(&mut project)?;
    let name = root
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let refs = match root.remove("media") {
        None => Vec::new(),
        Some(Value::Object(mut media)) => match media.remove("refs") {
            None => Vec::new(),
            Some(Value::Array(refs)) => refs,
            Some(_) => return Err("media refs is not a list".to_string()),
        },
        Some(_) => return Err("media is not an object".to_string()),
    };
    let mut items = Vec::with_capacity(refs.len());
    for media in refs {
        let Value::Object(mut media) = media else {
            return Err("a media reference is not an object".to_string());
        };
        let id = media
            .remove("id")
            .ok_or_else(|| "a media reference has no id".to_string())?;
        let path = match media.remove("path") {
            Some(Value::String(path)) => path,
            _ => return Err("a media reference has no path".to_string()),
        };
        let content_hash = media
            .remove("content_hash")
            .unwrap_or_else(|| Value::from(""));
        items.push(serde_json::json!({
            "id": id,
            "bin": 0,
            "name": v8_file_stem(&path),
            "kind": v8_media_kind(&path),
            "path": path,
            "spanned": [],
            "sidecars": [],
            "content_hash": content_hash,
            "size_bytes": 0,
            "card": null,
            "added_unix": 0,
            "facts": {
                "duration_s": null,
                "width": 0,
                "height": 0,
                "frame_rate": null,
                "video_codec": null,
                "video_profile": null,
                "bit_depth": 0,
                "chroma": null,
                "primaries": null,
                "hdr": null,
                "audio_codec": null,
                "audio_channels": 0,
                "sample_rate": 0,
                "audio_bit_depth": null,
                "sequence_frames": null,
                "container": null
            },
            "metadata": {
                "camera_make": null,
                "camera_model": null,
                "lens": null,
                "focal_length_mm": null,
                "f_number": null,
                "iso": null,
                "shutter": null,
                "timecode": null,
                "reel": null,
                "gps": null,
                "created": null
            },
            "label": null,
            "tags": []
        }));
    }
    let pool = serde_json::json!({
        "bins": [{ "id": 0, "name": name, "parent": null }],
        "items": items,
        "label_names": {}
    });
    root.insert("media".to_string(), pool);
    let Some(sequences) = root.get_mut("sequences") else {
        return Ok(project);
    };
    let sequences = sequences
        .as_array_mut()
        .ok_or_else(|| "sequences is not a list".to_string())?;
    for sequence in sequences {
        let Some(tracks) = sequence
            .as_object_mut()
            .ok_or_else(|| "a sequence is not an object".to_string())?
            .get_mut("tracks")
        else {
            continue;
        };
        for track in tracks
            .as_array_mut()
            .ok_or_else(|| "tracks is not a list".to_string())?
        {
            let track = track
                .as_object_mut()
                .ok_or_else(|| "a track is not an object".to_string())?;
            let switches = [
                ("muted", Value::Bool(false)),
                ("solo", Value::Bool(false)),
                ("visible", Value::Bool(true)),
                ("locked", Value::Bool(false)),
                ("height", Value::from("medium")),
            ];
            for (key, value) in switches {
                if track.contains_key(key) {
                    return Err(format!("a v8 track already has {key}"));
                }
                track.insert(key.to_string(), value);
            }
        }
    }
    Ok(project)
}

/// The file name without its last extension, splitting on `/` and `\` (the v8 → v9
/// item name; kept as written for the migration).
fn v8_file_stem(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

/// The v8 → v9 item kind from the file's extension (kept as written for the migration).
fn v8_media_kind(path: &str) -> &'static str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "wav" | "aif" | "aiff" | "mp3" | "m4a" | "aac" | "flac" | "ogg" | "opus" | "caf" => "audio",
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "heic" | "heif" | "bmp" | "gif" | "webp" | "exr"
        | "dpx" => "still",
        _ => "video",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn v1_to_v2_wraps_the_media_list() {
        let v1 = json!({
            "uid": "u",
            "media": [{ "id": 1, "content_hash": "ab", "path": "/a.mov" }],
            "sequences": []
        });
        let v2 = migrate_v1_to_v2(v1).expect("migrates");
        assert_eq!(
            v2,
            json!({
                "uid": "u",
                "media": { "refs": [{ "id": 1, "content_hash": "ab", "path": "/a.mov" }] },
                "sequences": []
            })
        );
    }

    #[test]
    fn v1_to_v2_rejects_a_media_object() {
        let err = migrate_v1_to_v2(json!({ "media": { "refs": [] } })).expect_err("rejects");
        assert_eq!(err, "media is not a list");
        assert!(migrate_v1_to_v2(json!([])).is_err());
    }

    #[test]
    fn v2_to_v3_adds_markers_to_sequences_and_clips() {
        let v2 = json!({
            "media": { "refs": [] },
            "sequences": [{
                "id": 2,
                "tracks": [{
                    "id": 3,
                    "kind": "video",
                    "items": [{ "id": 4, "type": "clip", "media": 1 }]
                }]
            }]
        });
        let v3 = migrate_v2_to_v3(v2).expect("migrates");
        assert_eq!(
            v3,
            json!({
                "media": { "refs": [] },
                "sequences": [{
                    "id": 2,
                    "markers": [],
                    "tracks": [{
                        "id": 3,
                        "kind": "video",
                        "items": [{ "id": 4, "type": "clip", "media": 1, "markers": [] }]
                    }]
                }]
            })
        );
    }

    #[test]
    fn v3_to_v4_gives_clips_their_default_properties() {
        let v3 = json!({
            "next_id": 9,
            "sequences": [{
                "id": 2,
                "markers": [],
                "tracks": [
                    {
                        "id": 3,
                        "kind": "video",
                        "items": [{ "id": 4, "type": "clip", "media": 1, "markers": [] }]
                    },
                    {
                        "id": 5,
                        "kind": "subtitle",
                        "items": [{ "id": 6, "type": "subtitle", "text": "Hi" }]
                    },
                    { "id": 7, "kind": "adjustment", "items": [{ "id": 8, "type": "adjustment" }] }
                ]
            }]
        });
        let v4 = migrate_v3_to_v4(v3.clone()).expect("migrates");
        let clip = &v4["sequences"][0]["tracks"][0]["items"][0];
        assert_eq!(
            clip["speed"],
            json!({ "keys": [{ "source_time": "0", "speed": { "num": 1, "den": 1 } }] })
        );
        assert_eq!(clip["enabled"], json!(true));
        assert_eq!(clip["gain_db"], json!(0.0));
        assert_eq!(clip["transform"]["scale"], json!([1.0, 1.0]));
        assert_eq!(clip["crop"]["feather"], json!(0.0));
        assert_eq!(clip["opacity"], json!(1.0));
        assert_eq!(clip["blend"], json!("normal"));
        assert_eq!(clip["effects"], json!([]));
        assert_eq!(clip["markers"], json!([]));
        // Other items and the id counter are untouched.
        assert_eq!(
            v4["sequences"][0]["tracks"][1],
            v3["sequences"][0]["tracks"][1]
        );
        assert_eq!(
            v4["sequences"][0]["tracks"][2],
            v3["sequences"][0]["tracks"][2]
        );
        assert_eq!(v4["next_id"], json!(9));
    }

    #[test]
    fn v3_to_v4_rejects_malformed_trees() {
        assert!(migrate_v3_to_v4(json!([])).is_err());
        assert!(migrate_v3_to_v4(json!({ "sequences": {} })).is_err());
        assert!(migrate_v3_to_v4(json!({ "sequences": [1] })).is_err());
        assert!(migrate_v3_to_v4(json!({ "sequences": [{ "tracks": {} }] })).is_err());
        for track in [json!(5), json!("V1"), json!([])] {
            let tracks = json!({ "sequences": [{ "tracks": [track] }] });
            assert_eq!(
                migrate_v3_to_v4(tracks.clone()),
                Err("a track is not an object".to_string())
            );
            assert_eq!(
                migrate_v2_to_v3(tracks.clone()),
                Err("a track is not an object".to_string())
            );
            assert_eq!(
                migrate_v4_to_v5(tracks),
                Err("a track is not an object".to_string())
            );
        }
        assert!(
            migrate_v3_to_v4(json!({ "sequences": [{ "tracks": [{ "items": [2] }] }] })).is_err()
        );
        let err = migrate_v3_to_v4(json!({
            "sequences": [{ "tracks": [{ "items": [{ "type": "clip", "opacity": 0.5 }] }] }]
        }))
        .expect_err("a v3 clip has no opacity");
        assert_eq!(err, "a v3 clip already has opacity");
        assert_eq!(
            migrate_v3_to_v4(json!({ "uid": "u" })).expect("no sequences"),
            json!({ "uid": "u" })
        );
    }

    #[test]
    fn v4_to_v5_gives_tracks_empty_transition_lists() {
        let v4 = json!({
            "next_id": 9,
            "sequences": [
                {
                    "id": 2,
                    "tracks": [
                        { "id": 3, "kind": "video", "items": [{ "id": 4, "type": "clip" }] },
                        { "id": 5, "kind": "subtitle", "items": [] }
                    ]
                },
                { "id": 6, "tracks": [] }
            ]
        });
        let v5 = migrate_v4_to_v5(v4.clone()).expect("migrates");
        let mut expected = v4;
        for track in expected["sequences"][0]["tracks"]
            .as_array_mut()
            .expect("tracks")
        {
            track["transitions"] = json!([]);
        }
        assert_eq!(v5, expected);
    }

    #[test]
    fn v4_to_v5_rejects_malformed_trees() {
        assert!(migrate_v4_to_v5(json!([])).is_err());
        assert!(migrate_v4_to_v5(json!({ "sequences": {} })).is_err());
        assert!(migrate_v4_to_v5(json!({ "sequences": [1] })).is_err());
        assert!(migrate_v4_to_v5(json!({ "sequences": [{ "tracks": {} }] })).is_err());
        assert!(migrate_v4_to_v5(json!({ "sequences": [{ "tracks": [3] }] })).is_err());
        let err = migrate_v4_to_v5(json!({ "sequences": [{ "tracks": [{ "transitions": [] }] }] }))
            .expect_err("a v4 track has no transitions");
        assert_eq!(err, "a v4 track already has transitions");
        assert_eq!(
            migrate_v4_to_v5(json!({ "uid": "u" })).expect("no sequences"),
            json!({ "uid": "u" })
        );
        assert_eq!(
            migrate_v4_to_v5(json!({ "sequences": [{ "id": 1 }] })).expect("no tracks"),
            json!({ "sequences": [{ "id": 1 }] })
        );
    }

    #[test]
    fn v5_to_v6_adds_default_settings_and_sequence_formats() {
        let v5 = json!({
            "next_id": 4,
            "sequences": [
                { "id": 2, "frame_rate": { "num": 25, "den": 1 }, "sample_rate": 44100, "tracks": [] },
                { "id": 3, "tracks": [] }
            ]
        });
        let v6 = migrate_v5_to_v6(v5).expect("migrates");
        assert_eq!(v6["next_id"], json!(4));
        assert_eq!(v6["settings"]["colour_space"], json!("rec709"));
        assert_eq!(
            v6["settings"]["cache_folder"],
            json!({ "relative": ".glass-cache" })
        );
        assert_eq!(
            v6["settings"]["sequence"]["frame_rate"],
            json!({ "num": 30, "den": 1 })
        );
        for seq in v6["sequences"].as_array().expect("sequences") {
            assert_eq!(seq["resolution"], json!({ "width": 1920, "height": 1080 }));
            assert_eq!(seq["drop_frame"], json!(false));
            assert_eq!(seq["channel_layout"], json!("stereo"));
        }
        // The rates stay as they were.
        assert_eq!(
            v6["sequences"][0]["frame_rate"],
            json!({ "num": 25, "den": 1 })
        );
        assert_eq!(v6["sequences"][0]["sample_rate"], json!(44100));
    }

    #[test]
    fn v5_to_v6_rejects_malformed_trees() {
        assert!(migrate_v5_to_v6(json!([])).is_err());
        assert_eq!(
            migrate_v5_to_v6(json!({ "settings": {} })),
            Err("a v5 project already has settings".to_string())
        );
        assert!(migrate_v5_to_v6(json!({ "sequences": {} })).is_err());
        assert_eq!(
            migrate_v5_to_v6(json!({ "sequences": [1] })),
            Err("a sequence is not an object".to_string())
        );
        assert_eq!(
            migrate_v5_to_v6(json!({ "sequences": [{ "drop_frame": true }] })),
            Err("a v5 sequence already has drop_frame".to_string())
        );
        let bare = migrate_v5_to_v6(json!({ "uid": "u" })).expect("no sequences");
        assert_eq!(bare["uid"], json!("u"));
        assert!(bare["settings"].is_object());
    }

    #[test]
    fn v6_to_v7_gives_clips_and_effects_empty_keyframes() {
        let v6 = json!({
            "next_id": 9,
            "sequences": [
                {
                    "id": 2,
                    "tracks": [
                        {
                            "id": 3,
                            "kind": "video",
                            "items": [
                                {
                                    "id": 4,
                                    "type": "clip",
                                    "effects": [{ "id": 5, "schema": "lut" }, { "id": 6, "schema": "x" }]
                                },
                                { "id": 7, "type": "clip", "effects": [] }
                            ]
                        },
                        { "id": 8, "kind": "subtitle", "items": [{ "id": 1, "type": "subtitle" }] }
                    ]
                },
                { "id": 10 }
            ]
        });
        let v7 = migrate_v6_to_v7(v6.clone()).expect("migrates");
        let mut expected = v6;
        let items = &mut expected["sequences"][0]["tracks"][0]["items"];
        for item in items.as_array_mut().expect("items") {
            item["keyframes"] = json!({});
            for effect in item["effects"].as_array_mut().expect("effects") {
                effect["keyframes"] = json!({});
            }
        }
        assert_eq!(v7, expected);
    }

    #[test]
    fn v6_to_v7_rejects_malformed_trees() {
        assert!(migrate_v6_to_v7(json!([])).is_err());
        assert!(migrate_v6_to_v7(json!({ "sequences": {} })).is_err());
        assert!(migrate_v6_to_v7(json!({ "sequences": [1] })).is_err());
        assert!(migrate_v6_to_v7(json!({ "sequences": [{ "tracks": [3] }] })).is_err());
        let clip = |extra: Value| json!({ "sequences": [{ "tracks": [{ "items": [extra] }] }] });
        assert_eq!(
            migrate_v6_to_v7(clip(json!({ "type": "clip", "keyframes": {} }))),
            Err("a v6 clip already has keyframes".to_string())
        );
        assert_eq!(
            migrate_v6_to_v7(clip(
                json!({ "type": "clip", "effects": [{ "keyframes": {} }] })
            )),
            Err("a v6 effect already has keyframes".to_string())
        );
        assert_eq!(
            migrate_v6_to_v7(clip(json!({ "type": "clip", "effects": {} }))),
            Err("effects is not a list".to_string())
        );
        assert_eq!(
            migrate_v6_to_v7(clip(json!({ "type": "clip", "effects": [2] }))),
            Err("an effect is not an object".to_string())
        );
        assert_eq!(
            migrate_v6_to_v7(json!({ "uid": "u" })).expect("no sequences"),
            json!({ "uid": "u" })
        );
    }

    #[test]
    fn v7_to_v8_marks_every_sequence_not_compound() {
        let v7 = json!({
            "next_id": 9,
            "sequences": [
                {
                    "id": 2,
                    "tracks": [{
                        "id": 3,
                        "kind": "video",
                        "items": [{ "id": 4, "type": "clip", "media": 1 }]
                    }]
                },
                { "id": 5 }
            ]
        });
        let v8 = migrate_v7_to_v8(v7.clone()).expect("migrates");
        let mut expected = v7;
        for seq in expected["sequences"].as_array_mut().expect("sequences") {
            seq["compound"] = json!(false);
        }
        assert_eq!(v8, expected);
    }

    #[test]
    fn v7_to_v8_rejects_malformed_trees() {
        assert!(migrate_v7_to_v8(json!([])).is_err());
        assert_eq!(
            migrate_v7_to_v8(json!({ "sequences": {} })),
            Err("sequences is not a list".to_string())
        );
        assert_eq!(
            migrate_v7_to_v8(json!({ "sequences": [1] })),
            Err("a sequence is not an object".to_string())
        );
        assert_eq!(
            migrate_v7_to_v8(json!({ "sequences": [{ "compound": true }] })),
            Err("a v7 sequence already has compound".to_string())
        );
        assert_eq!(
            migrate_v7_to_v8(json!({ "uid": "u" })).expect("no sequences"),
            json!({ "uid": "u" })
        );
    }

    #[test]
    fn v8_to_v9_turns_references_into_pool_items_and_adds_track_switches() {
        let v8 = json!({
            "name": "Harbour film",
            "next_id": 9,
            "media": { "refs": [
                { "id": 1, "content_hash": "ab", "path": "/a/Dusk.MOV" },
                { "id": 2, "content_hash": "", "path": "C:\\b\\gulls.wav" },
                { "id": 3, "content_hash": "cd", "path": "/c/still.tiff" }
            ] },
            "sequences": [
                {
                    "id": 4,
                    "tracks": [
                        { "id": 5, "kind": "video", "items": [] },
                        { "id": 6, "kind": "audio", "items": [] }
                    ]
                },
                { "id": 7 }
            ]
        });
        let v9 = migrate_v8_to_v9(v8.clone()).expect("migrates");
        assert_eq!(v9["next_id"], json!(9));
        assert_eq!(
            v9["media"]["bins"],
            json!([{ "id": 0, "name": "Harbour film", "parent": null }])
        );
        assert_eq!(v9["media"]["label_names"], json!({}));
        let items = v9["media"]["items"].as_array().expect("items");
        let short: Vec<Value> = items
            .iter()
            .map(|i| json!([i["id"], i["name"], i["kind"], i["path"], i["content_hash"], i["bin"]]))
            .collect();
        assert_eq!(
            short,
            [
                json!([1, "Dusk", "video", "/a/Dusk.MOV", "ab", 0]),
                json!([2, "gulls", "audio", "C:\\b\\gulls.wav", "", 0]),
                json!([3, "still", "still", "/c/still.tiff", "cd", 0]),
            ]
        );
        assert_eq!(items[0]["facts"]["width"], json!(0));
        assert!(items[0]["metadata"]["camera_model"].is_null());
        assert_eq!(items[0]["tags"], json!([]));
        for track in v9["sequences"][0]["tracks"].as_array().expect("tracks") {
            assert_eq!(track["muted"], json!(false));
            assert_eq!(track["solo"], json!(false));
            assert_eq!(track["visible"], json!(true));
            assert_eq!(track["locked"], json!(false));
            assert_eq!(track["height"], json!("medium"));
        }
        assert_eq!(v9["sequences"][1], v8["sequences"][1]);
        // An empty project gets a root bin and nothing else.
        let bare = migrate_v8_to_v9(json!({ "name": "x" })).expect("bare");
        assert_eq!(bare["media"]["items"], json!([]));
        assert_eq!(bare["media"]["bins"][0]["name"], json!("x"));
    }

    #[test]
    fn v8_to_v9_rejects_malformed_trees() {
        assert!(migrate_v8_to_v9(json!([])).is_err());
        assert_eq!(
            migrate_v8_to_v9(json!({ "media": [] })),
            Err("media is not an object".to_string())
        );
        assert_eq!(
            migrate_v8_to_v9(json!({ "media": { "refs": {} } })),
            Err("media refs is not a list".to_string())
        );
        assert_eq!(
            migrate_v8_to_v9(json!({ "media": { "refs": [3] } })),
            Err("a media reference is not an object".to_string())
        );
        assert_eq!(
            migrate_v8_to_v9(json!({ "media": { "refs": [{ "path": "/a" }] } })),
            Err("a media reference has no id".to_string())
        );
        assert_eq!(
            migrate_v8_to_v9(json!({ "media": { "refs": [{ "id": 1 }] } })),
            Err("a media reference has no path".to_string())
        );
        assert_eq!(
            migrate_v8_to_v9(json!({ "sequences": [{ "tracks": [3] }] })),
            Err("a track is not an object".to_string())
        );
        assert_eq!(
            migrate_v8_to_v9(json!({ "sequences": [{ "tracks": [{ "locked": true }] }] })),
            Err("a v8 track already has locked".to_string())
        );
        assert_eq!(
            migrate_v8_to_v9(json!({ "sequences": {} })),
            Err("sequences is not a list".to_string())
        );
    }

    #[test]
    fn v2_to_v3_rejects_malformed_trees() {
        assert!(migrate_v2_to_v3(json!({ "sequences": {} })).is_err());
        assert!(migrate_v2_to_v3(json!({ "sequences": [1] })).is_err());
        assert!(migrate_v2_to_v3(json!({ "sequences": [{ "markers": [] }] })).is_err());
        assert!(
            migrate_v2_to_v3(json!({ "sequences": [{ "tracks": [{ "items": [2] }] }] })).is_err()
        );
        assert_eq!(
            migrate_v2_to_v3(json!({ "uid": "u" })).expect("no sequences"),
            json!({ "uid": "u" })
        );
    }
}
