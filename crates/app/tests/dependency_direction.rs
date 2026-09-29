//! Keeps inter-crate dependencies in line with the direction diagram in
//! docs/ARCHITECTURE.md, section 2.

use std::collections::BTreeSet;
use std::path::Path;

const ALLOWED: &[(&str, &[&str])] = &[
    ("app", &["ui", "engine"]),
    ("smoke", &["engine"]),
    ("ui", &["engine", "project", "render", "assets"]),
    (
        "engine",
        &["project", "media", "render", "audio", "plugins", "assets"],
    ),
    ("render", &["media", "assets"]),
    ("media", &["assets"]),
    ("plugins", &["project"]),
    ("project", &[]),
    ("audio", &[]),
    ("assets", &[]),
];

fn workspace_deps(manifest: &str, crates: &BTreeSet<&str>) -> BTreeSet<String> {
    let mut in_deps = false;
    let mut found = BTreeSet::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_deps = line.ends_with("dependencies]");
            continue;
        }
        if !in_deps {
            continue;
        }
        let name = line
            .split(['.', '=', ' '])
            .next()
            .unwrap_or_default()
            .trim_matches('"');
        if crates.contains(name) {
            found.insert(name.to_string());
        }
    }
    found
}

#[test]
fn crates_follow_the_architecture_diagram() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let names: BTreeSet<&str> = ALLOWED.iter().map(|(name, _)| *name).collect();

    let mut on_disk = BTreeSet::new();
    for entry in std::fs::read_dir(&crates_dir).expect("read crates dir") {
        let entry = entry.expect("read crates entry");
        if entry.path().join("Cargo.toml").is_file() {
            on_disk.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }
    let expected: BTreeSet<String> = names.iter().map(|n| n.to_string()).collect();
    assert_eq!(on_disk, expected, "crate list differs from ARCHITECTURE.md");

    for (name, allowed) in ALLOWED {
        let manifest = std::fs::read_to_string(crates_dir.join(name).join("Cargo.toml"))
            .expect("read crate manifest");
        let actual = workspace_deps(&manifest, &names);
        let allowed: BTreeSet<String> = allowed.iter().map(|d| d.to_string()).collect();
        let extra: Vec<_> = actual.difference(&allowed).collect();
        assert!(
            extra.is_empty(),
            "{name} depends on {extra:?}, which ARCHITECTURE.md does not allow"
        );
    }
}
