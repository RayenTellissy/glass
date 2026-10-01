//! Project files: every prior schema version's fixtures migrate to the current model, and
//! saving then loading the current version is the identity.
//!
//! `tests/fixtures/projects/v<N>-<name>.json` are files written by schema version N.
//! Every `<name>` has a fixture at the current version, saved by this build; the older
//! ones must load to the same project. `GLASS_UPDATE_FIXTURES=1` rewrites the current
//! version's fixtures (after a schema bump, the previous ones stay as they are).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use project::schema::{self, LoadError, OLDEST_VERSION, SCHEMA_VERSION};
use project::{ClipSpec, MediaId, Project, Rational, Time, TimeRange, TrackKind};
use serde_json::Value;

const FPS: Rational = Rational::new(25, 1);

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/projects")
}

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn clip(media: MediaId, start: Time, len: Time, source: Time) -> ClipSpec {
    ClipSpec {
        media,
        range: TimeRange::new(start, len),
        source_start: source,
    }
}

/// The project in every `*-basic.json` fixture: two media, one sequence, a video and an
/// audio track, three clips.
fn basic() -> Project {
    let p = Project::new("fixture-basic", "Harbour film");
    let (p, a) = p
        .add_media("a1b2c3", "/footage/harbour.mov")
        .expect("media");
    let (p, b) = p.add_media("d4e5f6", "/footage/gulls.wav").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("sequence");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let half = Time::new(1, 2).expect("time");
    let (p, _) = p
        .insert_clip(v1, clip(a, Time::ZERO, frames(50), half))
        .expect("clip");
    let (p, _) = p
        .insert_clip(v1, clip(a, frames(50), frames(15), Time::seconds(3)))
        .expect("clip");
    let (p, _) = p
        .insert_clip(a1, clip(b, Time::ZERO, Time::seconds(2), Time::ZERO))
        .expect("clip");
    p
}

/// Fixture files by name (`basic`), then by version.
fn fixtures() -> BTreeMap<String, BTreeMap<u32, PathBuf>> {
    let mut out: BTreeMap<String, BTreeMap<u32, PathBuf>> = BTreeMap::new();
    for entry in fs::read_dir(fixtures_dir()).expect("fixtures folder") {
        let path = entry.expect("entry").path();
        let stem = path.file_stem().and_then(|s| s.to_str()).expect("name");
        let (version, name) = stem
            .strip_prefix('v')
            .and_then(|s| s.split_once('-'))
            .unwrap_or_else(|| panic!("{} is not v<N>-<name>.json", path.display()));
        let version: u32 = version.parse().expect("version number");
        out.entry(name.to_string())
            .or_default()
            .insert(version, path);
    }
    out
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn current_basic() -> Vec<u8> {
    read(&fixtures_dir().join(format!("v{SCHEMA_VERSION}-basic.json")))
}

#[test]
fn every_version_has_fixtures_and_their_versions_match_the_names() {
    let all = fixtures();
    let basic = all.get("basic").expect("basic fixtures");
    for version in OLDEST_VERSION..=SCHEMA_VERSION {
        assert!(basic.contains_key(&version), "no v{version}-basic.json");
    }
    for (name, versions) in &all {
        assert!(
            versions.contains_key(&SCHEMA_VERSION),
            "{name} has no fixture at the current version {SCHEMA_VERSION}"
        );
        for (version, path) in versions {
            let value: Value = serde_json::from_slice(&read(path)).expect("JSON");
            assert_eq!(
                value["schema_version"].as_u64(),
                Some(u64::from(*version)),
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn the_current_version_fixture_is_what_save_writes() {
    let bytes = schema::save(&basic());
    let path = fixtures_dir().join(format!("v{SCHEMA_VERSION}-basic.json"));
    if std::env::var_os("GLASS_UPDATE_FIXTURES").is_some() {
        fs::write(&path, &bytes).expect("write fixture");
    }
    assert_eq!(
        String::from_utf8(bytes).expect("UTF-8"),
        String::from_utf8(read(&path)).expect("UTF-8"),
        "save output changed; a model change needs a schema bump and a migration"
    );
}

#[test]
fn every_prior_version_migrates_to_the_current_project() {
    for (name, versions) in fixtures() {
        let current = schema::load(&read(&versions[&SCHEMA_VERSION]))
            .unwrap_or_else(|e| panic!("v{SCHEMA_VERSION}-{name}: {e}"));
        for (version, path) in &versions {
            let loaded =
                schema::load(&read(path)).unwrap_or_else(|e| panic!("{name} v{version}: {e}"));
            assert!(
                loaded.content_eq(&current),
                "v{version}-{name} migrates to a different project"
            );
            // Saving writes the current version.
            assert_eq!(schema::save(&loaded), schema::save(&current));
        }
    }
}

#[test]
fn v1_media_list_becomes_media_refs() {
    let path = fixtures_dir().join("v1-basic.json");
    let v1: Value = serde_json::from_slice(&read(&path)).expect("JSON");
    assert!(v1["project"]["media"].is_array());
    let migrated = schema::migrate(1, v1["project"].clone()).expect("migrates");
    assert_eq!(migrated["media"]["refs"], v1["project"]["media"]);

    let loaded = schema::load(&read(&path)).expect("loads");
    let paths: Vec<_> = loaded
        .media()
        .iter()
        .map(|m| (m.id.0, m.path.as_str()))
        .collect();
    assert_eq!(
        paths,
        vec![(1, "/footage/harbour.mov"), (2, "/footage/gulls.wav")]
    );
    assert!(loaded.content_eq(&basic()));
}

#[test]
fn saving_then_loading_the_current_version_is_the_identity() {
    let p = basic();
    let (p, seq) = p
        .add_sequence("Alt", Rational::new(30000, 1001), 44_100)
        .expect("seq");
    let (p, _) = p
        .add_track(seq, TrackKind::Video, "V1", None)
        .expect("track");
    let saved = schema::save(&p);
    let loaded = schema::load(&saved).expect("loads");
    assert!(loaded.content_eq(&p));
    assert_eq!(
        schema::save(&loaded),
        saved,
        "the same tree saves to the same bytes"
    );
}

fn with_version(bytes: &[u8], version: Value) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(bytes).expect("JSON");
    value["schema_version"] = version;
    serde_json::to_vec(&value).expect("JSON")
}

#[test]
fn a_newer_file_is_refused_with_its_version() {
    let newer = with_version(&current_basic(), Value::from(SCHEMA_VERSION + 1));
    let err = schema::load(&newer).expect_err("refused");
    assert!(
        matches!(err, LoadError::NewerVersion { found, supported }
            if found == u64::from(SCHEMA_VERSION + 1) && supported == SCHEMA_VERSION),
        "{err:?}"
    );
    assert_eq!(err.i18n_key(), "project.open_error.newer_version");
}

#[test]
fn files_that_are_not_readable_projects_fail_with_their_message() {
    let cases: [(&[u8], &str); 5] = [
        (b"{ nope", "project.open_error.damaged"),
        (b"[1, 2]", "project.open_error.not_a_project"),
        (
            br#"{"format": "other", "schema_version": 1, "project": {}}"#,
            "project.open_error.not_a_project",
        ),
        (
            br#"{"format": "glass.project", "project": {}}"#,
            "project.open_error.not_a_project",
        ),
        (
            br#"{"format": "glass.project", "schema_version": 1, "project": {"media": []}}"#,
            "project.open_error.damaged",
        ),
    ];
    for (bytes, key) in cases {
        let err = schema::load(bytes).expect_err("fails");
        assert_eq!(err.i18n_key(), key, "{err}");
    }
    let older = with_version(&current_basic(), Value::from(0));
    let err = schema::load(&older).expect_err("fails");
    assert!(
        matches!(err, LoadError::OlderVersion { found: 0, .. }),
        "{err:?}"
    );
    assert_eq!(err.i18n_key(), "project.open_error.older_version");
}

#[test]
fn a_migrated_tree_is_validated() {
    // A v1 file whose clip points at media that is not in the project.
    let mut v1: Value =
        serde_json::from_slice(&read(&fixtures_dir().join("v1-basic.json"))).expect("JSON");
    v1["project"]["media"]
        .as_array_mut()
        .expect("list")
        .remove(1);
    let err = schema::load(&serde_json::to_vec(&v1).expect("JSON")).expect_err("invalid");
    assert!(matches!(err, LoadError::Invalid(_)), "{err:?}");
    assert_eq!(err.i18n_key(), "project.open_error.damaged");
}
