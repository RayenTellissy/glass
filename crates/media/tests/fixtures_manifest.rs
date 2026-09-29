//! Checks that every file listed in the generated fixture manifest exists. Skips when
//! `tests/fixtures/generate.sh` has not been run.

use std::path::{Path, PathBuf};

fn generated_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated")
}

/// Expands an image sequence pattern such as `frame_%04d.png` for frame numbers
/// starting at 1.
fn sequence_paths(pattern: &str, frames: u64) -> Vec<String> {
    let (prefix, rest) = pattern.split_once("%0").expect("sequence pattern has %0Nd");
    let (width, suffix) = rest.split_once('d').expect("sequence pattern has %0Nd");
    let width: usize = width.parse().expect("sequence pattern width");
    (1..=frames)
        .map(|n| format!("{prefix}{n:0width$}{suffix}"))
        .collect()
}

#[test]
fn manifest_files_exist() {
    let dir = generated_dir();
    let manifest_path = dir.join("MANIFEST.json");
    let Ok(text) = std::fs::read_to_string(&manifest_path) else {
        eprintln!("skipped: no fixture manifest; run tests/fixtures/generate.sh to create it");
        return;
    };
    let manifest: serde_json::Value = serde_json::from_str(&text).expect("manifest is JSON");
    let files = manifest["files"]
        .as_array()
        .expect("manifest has a files array");
    assert!(!files.is_empty(), "manifest lists no files");

    let mut missing = Vec::new();
    for entry in files {
        let file = entry["file"].as_str().expect("entry has a file name");
        let paths = if entry["kind"] == "image_sequence" || entry["kind"] == "sequence" {
            let frames = entry["frames"]
                .as_u64()
                .expect("sequence has a frame count");
            let first = entry["first"].as_u64().unwrap_or(1);
            let last = entry["last"].as_u64().unwrap_or(first + frames - 1);
            let missing: Vec<u64> = entry["missing"]
                .as_array()
                .map(|m| m.iter().filter_map(|n| n.as_u64()).collect())
                .unwrap_or_default();
            let paths: Vec<String> = sequence_paths(file, last)
                .into_iter()
                .enumerate()
                .filter(|(i, _)| {
                    let n = *i as u64 + 1;
                    n >= first && !missing.contains(&n)
                })
                .map(|(_, p)| p)
                .collect();
            assert_eq!(paths.len() as u64, frames, "{file}: frame count");
            paths
        } else {
            vec![file.to_string()]
        };
        missing.extend(paths.into_iter().filter(|p| !dir.join(p).is_file()));
    }
    assert!(
        missing.is_empty(),
        "missing fixtures (rerun tests/fixtures/generate.sh): {missing:?}"
    );
}

#[test]
fn sequence_pattern_expands() {
    assert_eq!(
        sequence_paths("seq/frame_%04d.png", 2),
        ["seq/frame_0001.png", "seq/frame_0002.png"]
    );
}
