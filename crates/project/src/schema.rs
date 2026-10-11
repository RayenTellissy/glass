//! The project file: a versioned envelope around the edit tree, and the migrations that
//! bring files written by older versions up to the current schema.
//!
//! A project file is JSON:
//!
//! ```json
//! {
//!   "format": "glass.project",
//!   "schema_version": 4,
//!   "project": { "uid": "...", "name": "...", "next_id": 7, "media": {...}, "sequences": [...] }
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
//! Key order is stable: the envelope and every node serialise as structs (fields in
//! declaration order) and every collection is ordered (by position, or by id for media),
//! so saving the same tree twice gives the same bytes.
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

use serde::Serialize;
use serde_json::{Map, Value};

use crate::model::Project;
use crate::validate::ValidationError;

/// The `format` field every project file carries.
pub const FORMAT: &str = "glass.project";

/// The schema version this build writes, and the newest it reads.
pub const SCHEMA_VERSION: u32 = 4;

/// The oldest schema version this build reads.
pub const OLDEST_VERSION: u32 = 1;

/// One migration step: the `project` value of version N to that of version N+1.
pub type Migration = fn(Value) -> Result<Value, String>;

/// `MIGRATIONS[i]` takes version `OLDEST_VERSION + i` to the next one.
pub const MIGRATIONS: [Migration; (SCHEMA_VERSION - OLDEST_VERSION) as usize] =
    [migrate_v1_to_v2, migrate_v2_to_v3, migrate_v3_to_v4];

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
            let Some(items) = track.get_mut("items") else {
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
            let Some(items) = track.get_mut("items") else {
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
