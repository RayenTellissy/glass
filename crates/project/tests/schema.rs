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

use project::params::{ParamDef, ParamSchema, ParamValue, Range, effects};
use project::schema::{self, LoadError, OLDEST_VERSION, SCHEMA_VERSION};
use project::{
    Alignment, BlendMode, Clip, ClipSpec, ColourLabel, Crop, MarkerOwner, MarkerSpec, MediaId,
    Project, Rational, SpeedKeep, Time, TimeRange, TodoState, TrackKind, Transform, TransitionSpec,
};
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

/// The project in every `*-tracks.json` fixture (from v3): every track kind, implicit
/// gaps, sequence markers (point, range, chapter, to-do) and clip markers.
fn tracks() -> Project {
    let p = Project::new("fixture-tracks", "Night market");
    let (p, a) = p.add_media("0a0b0c", "/footage/market.mov").expect("media");
    let (p, seq) = p.add_sequence("Edit", FPS, 48_000).expect("sequence");
    let (p, fx) = p
        .add_track(seq, TrackKind::Adjustment, "FX", None)
        .expect("fx");
    let (p, s1) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, shot) = p
        .insert_clip(v1, clip(a, frames(25), frames(75), Time::seconds(2)))
        .expect("clip");
    let (p, _) = p
        .insert_clip(a1, clip(a, frames(25), frames(75), Time::seconds(2)))
        .expect("clip");
    let (p, _) = p
        .insert_subtitle(
            s1,
            TimeRange::new(frames(30), frames(40)),
            "The boats go out at five.",
        )
        .expect("cue");
    let (p, _) = p
        .insert_adjustment(fx, TimeRange::new(frames(50), frames(25)))
        .expect("adjustment");
    let (p, _) = p
        .add_marker(
            MarkerOwner::Sequence(seq),
            MarkerSpec {
                name: "Opening line".into(),
                chapter: true,
                ..MarkerSpec::at(frames(25), ColourLabel::Amber)
            },
        )
        .expect("marker");
    let (p, _) = p
        .add_marker(
            MarkerOwner::Sequence(seq),
            MarkerSpec {
                duration: frames(20),
                name: "Fix wind noise".into(),
                note: "Wind on the last word.".into(),
                todo: TodoState::Open,
                ..MarkerSpec::at(frames(60), ColourLabel::Red)
            },
        )
        .expect("marker");
    let (p, _) = p
        .add_marker(
            MarkerOwner::Clip(shot),
            MarkerSpec {
                name: "Harbourmaster".into(),
                ..MarkerSpec::at(Time::new(5, 2).expect("time"), ColourLabel::Blue)
            },
        )
        .expect("marker");
    p
}

/// A third-party effect this build has no schema for in `params::effects`.
fn glow() -> ParamSchema {
    ParamSchema::new(
        "acme.glow",
        "acme.glow",
        vec![ParamDef::float(
            "radius",
            "acme.glow.radius",
            8.0,
            Range::new(0.0, 100.0),
        )],
    )
}

/// The project in every `*-props.json` fixture (from v4): clips with every property away
/// from its default, and effect stacks.
fn props() -> Project {
    let p = Project::new("fixture-props", "Ferry crossing");
    let (p, a) = p.add_media("1f2e3d", "/footage/ferry.mov").expect("media");
    let (p, seq) = p.add_sequence("Crossing", FPS, 48_000).expect("sequence");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, wide) = p
        .insert_clip(v1, clip(a, Time::ZERO, frames(50), Time::seconds(1)))
        .expect("clip");
    let (p, close) = p
        .insert_clip(v1, clip(a, frames(110), frames(25), Time::seconds(9)))
        .expect("clip");
    let (p, sound) = p
        .insert_clip(a1, clip(a, Time::ZERO, Time::seconds(2), Time::seconds(1)))
        .expect("clip");
    let p = p
        .set_clip_speed(wide, Rational::new(1, 2), SpeedKeep::Source)
        .expect("speed");
    let p = p
        .set_clip_transform(
            wide,
            Transform {
                position: [-120.5, 64.0],
                scale: [1.25, -1.25],
                rotation: 12.5,
                anchor: [8.0, -4.0],
            },
        )
        .expect("transform");
    let p = p
        .set_clip_crop(
            wide,
            Crop {
                left: 32.0,
                top: 0.0,
                right: 32.0,
                bottom: 18.0,
                feather: 6.0,
            },
        )
        .expect("crop");
    let p = p.set_clip_opacity(wide, 0.75).expect("opacity");
    let p = p.set_clip_blend(wide, BlendMode::Screen).expect("blend");
    let lut = effects::lut();
    let (p, grade) = p.add_effect(wide, &lut, None).expect("lut");
    let p = p
        .set_effect_param(
            grade,
            &lut,
            "file",
            ParamValue::Text("/luts/ferry.cube".into()),
        )
        .expect("file");
    let p = p
        .set_effect_param(grade, &lut, "intensity", ParamValue::Float(0.6))
        .expect("intensity");
    let (p, halo) = p.add_effect(wide, &glow(), Some(0)).expect("glow");
    let p = p.set_effect_enabled(halo, false).expect("disable");
    let p = p
        .set_clip_speed(close, Rational::new(3, 2), SpeedKeep::Timeline)
        .expect("speed");
    let p = p.set_clip_enabled(close, false).expect("disable");
    let p = p.set_clip_gain(sound, -6.5).expect("gain");
    p.slip_clip(sound, Time::new(3, 2).expect("time"))
        .expect("slip")
}

/// The project in every `*-transitions.json` fixture (from v5): two-sided transitions
/// in each alignment, a fade from nothing at the track's start and one to nothing into a
/// gap, an audio crossfade, a kind with parameters and an easing.
fn transitions() -> Project {
    let p = Project::new("fixture-transitions", "Coastline");
    let (p, a) = p.add_media("5e6f70", "/footage/coast.mov").expect("media");
    let (p, seq) = p.add_sequence("Cut", FPS, 48_000).expect("sequence");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, first) = p
        .insert_clip(v1, clip(a, Time::ZERO, frames(50), Time::seconds(2)))
        .expect("clip");
    let (p, second) = p
        .insert_clip(v1, clip(a, frames(50), frames(50), Time::seconds(10)))
        .expect("clip");
    let (p, third) = p
        .insert_clip(v1, clip(a, frames(100), frames(25), Time::seconds(20)))
        .expect("clip");
    let (p, last) = p
        .insert_clip(v1, clip(a, frames(150), frames(50), Time::seconds(30)))
        .expect("clip");
    let (p, s1) = p
        .insert_clip(a1, clip(a, Time::ZERO, Time::seconds(2), Time::seconds(2)))
        .expect("clip");
    let (p, s2) = p
        .insert_clip(
            a1,
            clip(a, Time::seconds(2), Time::seconds(2), Time::seconds(10)),
        )
        .expect("clip");
    let at = |o, i, n, alignment| TransitionSpec {
        alignment,
        ..TransitionSpec::new(o, i, frames(n))
    };
    let (p, _) = p
        .add_transition(at(None, Some(first), 10, Alignment::Start))
        .expect("fade in");
    let (p, _) = p
        .add_transition(at(Some(first), Some(second), 12, Alignment::Centre))
        .expect("centre");
    let iris = ParamSchema::new(
        "iris",
        "transition.name.iris",
        vec![ParamDef::float(
            "softness",
            "transition.iris.softness",
            0.2,
            Range::new(0.0, 1.0),
        )],
    );
    let (p, wipe) = p
        .add_transition(TransitionSpec {
            kind: "iris".into(),
            direction: Some("up_left".into()),
            easing: "ease-in-out".into(),
            ..at(Some(second), Some(third), 8, Alignment::End)
        })
        .expect("end");
    let p = p
        .set_transition_param(wipe, &iris, "softness", ParamValue::Float(0.65))
        .expect("param");
    let (p, _) = p
        .add_transition(at(Some(third), None, 20, Alignment::Start))
        .expect("fade out");
    let (p, _) = p
        .add_transition(at(None, Some(last), 5, Alignment::Centre))
        .expect("fade in from the gap");
    let (p, _) = p
        .add_transition(TransitionSpec::new(
            Some(s1),
            Some(s2),
            Time::new(1, 2).expect("time"),
        ))
        .expect("audio");
    p
}

/// The project each fixture name was saved from.
fn builder(name: &str) -> Project {
    match name {
        "basic" => basic(),
        "tracks" => tracks(),
        "props" => props(),
        "transitions" => transitions(),
        other => panic!("no builder for {other} fixtures"),
    }
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
fn the_current_version_fixtures_are_what_save_writes() {
    for name in ["basic", "tracks", "props", "transitions"] {
        let bytes = schema::save(&builder(name));
        let path = fixtures_dir().join(format!("v{SCHEMA_VERSION}-{name}.json"));
        if std::env::var_os("GLASS_UPDATE_FIXTURES").is_some() {
            fs::write(&path, &bytes).expect("write fixture");
        }
        assert_eq!(
            String::from_utf8(bytes).expect("UTF-8"),
            String::from_utf8(read(&path)).expect("UTF-8"),
            "{name}: save output changed; a model change needs a schema bump and a migration"
        );
    }
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
fn v2_gains_empty_marker_lists() {
    let path = fixtures_dir().join("v2-basic.json");
    let v2: Value = serde_json::from_slice(&read(&path)).expect("JSON");
    let seq = &v2["project"]["sequences"][0];
    assert!(seq.get("markers").is_none());
    let migrated = schema::migrate(2, v2["project"].clone()).expect("migrates");
    let seq = &migrated["sequences"][0];
    assert_eq!(seq["markers"], Value::Array(Vec::new()));
    let mut clips = 0;
    for track in seq["tracks"].as_array().expect("tracks") {
        for item in track["items"].as_array().expect("items") {
            assert_eq!(item["markers"], Value::Array(Vec::new()));
            clips += 1;
        }
    }
    assert_eq!(clips, 3);
    // Migrated, it is the project the v3 build saves, and saving it writes v3.
    let loaded = schema::load(&read(&path)).expect("loads");
    assert!(loaded.content_eq(&basic()));
    let saved: Value = serde_json::from_slice(&schema::save(&loaded)).expect("JSON");
    assert_eq!(saved["schema_version"], Value::from(SCHEMA_VERSION));
}

#[test]
fn v3_clips_gain_their_default_properties() {
    for name in ["basic", "tracks"] {
        let path = fixtures_dir().join(format!("v3-{name}.json"));
        let v3: Value = serde_json::from_slice(&read(&path)).expect("JSON");
        let loaded = schema::load(&read(&path)).expect("loads");
        assert_eq!(
            loaded.next_id(),
            v3["project"]["next_id"].as_u64().expect("next id")
        );
        let mut clips = 0;
        for track in &loaded.sequences()[0].tracks {
            for item in &track.items {
                if let Some(clip) = item.clip() {
                    let defaults = Clip {
                        markers: clip.markers.clone(),
                        ..Clip::new(clip.media, clip.source_start)
                    };
                    assert_eq!(*clip, defaults, "{name}: item {}", item.id);
                    clips += 1;
                }
            }
        }
        assert!(clips >= 2, "{name}");
        assert!(loaded.content_eq(&builder(name)));
    }
}

#[test]
fn v4_tracks_gain_empty_transition_lists() {
    for name in ["basic", "tracks", "props"] {
        let path = fixtures_dir().join(format!("v4-{name}.json"));
        let v4: Value = serde_json::from_slice(&read(&path)).expect("JSON");
        let tracks = v4["project"]["sequences"][0]["tracks"]
            .as_array()
            .expect("tracks");
        assert!(tracks.iter().all(|t| t.get("transitions").is_none()));
        let migrated = schema::migrate(4, v4["project"].clone()).expect("migrates");
        for track in migrated["sequences"][0]["tracks"]
            .as_array()
            .expect("tracks")
        {
            assert_eq!(track["transitions"], Value::Array(Vec::new()), "{name}");
        }
        let loaded = schema::load(&read(&path)).expect("loads");
        assert_eq!(
            loaded.next_id(),
            v4["project"]["next_id"].as_u64().expect("next id")
        );
        assert!(loaded.content_eq(&builder(name)), "{name}");
    }
}

#[test]
fn the_transitions_fixture_round_trips_every_transition_field() {
    let p = transitions();
    let bytes = schema::save(&p);
    let loaded = schema::load(&bytes).expect("loads");
    assert!(loaded.content_eq(&p));
    assert_eq!(schema::save(&loaded), bytes);
    let seq = &loaded.sequences()[0];
    assert_eq!(seq.tracks[0].transitions.len(), 5);
    assert_eq!(seq.tracks[1].transitions.len(), 1);
    let json: Value = serde_json::from_slice(&bytes).expect("JSON");
    let wipe = &json["project"]["sequences"][0]["tracks"][0]["transitions"][2];
    assert_eq!(wipe["kind"], "iris");
    assert_eq!(wipe["alignment"], "end");
    assert_eq!(wipe["easing"], "ease-in-out");
    assert_eq!(wipe["direction"], "up_left");
    assert_eq!(wipe["duration"], "8/25");
    assert_eq!(
        wipe["params"],
        serde_json::json!({ "softness": { "float": 0.65 } })
    );
    let fade_in = &json["project"]["sequences"][0]["tracks"][0]["transitions"][0];
    assert_eq!(fade_in["outgoing"], Value::Null);
    assert_eq!(fade_in["alignment"], "start");
    assert_eq!(fade_in["direction"], Value::Null);
}

#[test]
fn the_props_fixture_round_trips_every_clip_property() {
    let p = props();
    let bytes = schema::save(&p);
    let loaded = schema::load(&bytes).expect("loads");
    assert!(loaded.content_eq(&p));
    assert_eq!(schema::save(&loaded), bytes);
    let seq = &loaded.sequences()[0];
    let wide = seq.tracks[0].items[0].clip().expect("clip");
    assert_eq!(wide.speed.constant_speed(), Some(Rational::new(1, 2)));
    assert_eq!(seq.tracks[0].items[0].range.duration, frames(100));
    assert_eq!(wide.blend, BlendMode::Screen);
    let schemas: Vec<_> = wide.effects.iter().map(|e| e.schema.as_str()).collect();
    assert_eq!(schemas, ["acme.glow", "lut"]);
    assert!(!wide.effects[0].enabled);
    let lut = effects::lut();
    assert_eq!(wide.effects[1].params.float(&lut, "intensity"), Some(0.6));
    let json: Value = serde_json::from_slice(&bytes).expect("JSON");
    let item = &json["project"]["sequences"][0]["tracks"][0]["items"][0];
    assert_eq!(item["blend"], "screen");
    assert_eq!(
        item["speed"]["keys"][0]["speed"],
        serde_json::json!({ "num": 1, "den": 2 })
    );
    let close = seq.tracks[0].items[1].clip().expect("clip");
    assert!(!close.enabled);
    let sound = seq.tracks[1].items[0].clip().expect("clip");
    assert_eq!(sound.gain_db, -6.5);
    assert_eq!(sound.source_start, Time::new(3, 2).expect("time"));
}

#[test]
fn clip_properties_out_of_range_are_refused_on_load() {
    let bytes = read(&fixtures_dir().join(format!("v{SCHEMA_VERSION}-props.json")));
    let cases: [(&str, Value); 7] = [
        ("opacity", Value::from(1.5)),
        ("gain_db", Value::from(30.0)),
        ("blend", Value::from("dissolve")),
        (
            "speed",
            serde_json::json!({ "keys": [{ "source_time": "0", "speed": { "num": 0, "den": 1 } }] }),
        ),
        (
            "speed",
            serde_json::json!({ "keys": [{ "source_time": "0", "speed": { "num": 2, "den": 4 } }] }),
        ),
        (
            "speed",
            serde_json::json!({ "keys": [{ "source_time": "1", "speed": { "num": 1, "den": 1 } }] }),
        ),
        ("speed", serde_json::json!({ "keys": [] })),
    ];
    for (key, value) in cases {
        let mut json: Value = serde_json::from_slice(&bytes).expect("JSON");
        json["project"]["sequences"][0]["tracks"][0]["items"][0][key] = value.clone();
        let err = schema::load(&serde_json::to_vec(&json).expect("JSON")).expect_err("refused");
        assert!(
            matches!(err, LoadError::Invalid(_) | LoadError::Model { .. }),
            "{key} = {value}: {err:?}"
        );
        assert_eq!(err.i18n_key(), "project.open_error.damaged");
    }
    // Two effects with one id.
    let mut json: Value = serde_json::from_slice(&bytes).expect("JSON");
    let effects = &mut json["project"]["sequences"][0]["tracks"][0]["items"][0]["effects"];
    effects[1]["id"] = effects[0]["id"].clone();
    let err = schema::load(&serde_json::to_vec(&json).expect("JSON")).expect_err("refused");
    assert!(matches!(err, LoadError::Invalid(_)), "{err:?}");
}

#[test]
fn the_tracks_fixture_round_trips_markers_and_every_track_kind() {
    let p = tracks();
    let loaded = schema::load(&schema::save(&p)).expect("loads");
    assert!(loaded.content_eq(&p));
    let seq = &loaded.sequences()[0];
    let kinds: Vec<_> = seq.tracks.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        [
            TrackKind::Adjustment,
            TrackKind::Subtitle,
            TrackKind::Video,
            TrackKind::Audio
        ]
    );
    assert_eq!(seq.markers.len(), 2);
    let video = seq
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .expect("v1");
    assert_eq!(
        video.gaps().expect("gaps"),
        vec![TimeRange::new(Time::ZERO, frames(25))]
    );
    assert_eq!(video.items[0].clip().expect("clip").markers.len(), 1);
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
