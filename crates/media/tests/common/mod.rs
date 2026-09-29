//! Helpers shared by the media integration tests: fixture paths and the manifest.
//! Tests skip (print and return) when `tests/fixtures/generate.sh` has not been run.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn generated_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated")
}

/// The manifest's `files` array, or `None` (after printing why) when fixtures are missing.
pub fn manifest() -> Option<Vec<serde_json::Value>> {
    let path = generated_dir().join("MANIFEST.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("skipped: no fixture manifest; run tests/fixtures/generate.sh to create it");
        return None;
    };
    let manifest: serde_json::Value = serde_json::from_str(&text).expect("manifest is JSON");
    Some(
        manifest["files"]
            .as_array()
            .expect("manifest has a files array")
            .clone(),
    )
}

/// Path of one generated fixture, or `None` when fixtures are missing.
pub fn fixture(name: &str) -> Option<PathBuf> {
    let path = generated_dir().join(name);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipped: {name} missing; run tests/fixtures/generate.sh to create it");
        None
    }
}
