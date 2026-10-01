//! The project file: a versioned envelope around the edit tree, and the migrations that
//! bring files written by older versions up to the current schema.
//!
//! A project file is JSON:
//!
//! ```json
//! {
//!   "format": "glass.project",
//!   "schema_version": 2,
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

use serde::Serialize;
use serde_json::{Map, Value};

use crate::model::Project;
use crate::validate::ValidationError;

/// The `format` field every project file carries.
pub const FORMAT: &str = "glass.project";

/// The schema version this build writes, and the newest it reads.
pub const SCHEMA_VERSION: u32 = 2;

/// The oldest schema version this build reads.
pub const OLDEST_VERSION: u32 = 1;

/// One migration step: the `project` value of version N to that of version N+1.
pub type Migration = fn(Value) -> Result<Value, String>;

/// `MIGRATIONS[i]` takes version `OLDEST_VERSION + i` to the next one.
pub const MIGRATIONS: [Migration; (SCHEMA_VERSION - OLDEST_VERSION) as usize] = [migrate_v1_to_v2];

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
}
