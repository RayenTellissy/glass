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
    Alignment, BlendMode, CacheFolder, ChannelLayout, Clip, ClipParam, ClipSource, ClipSpec,
    ColourLabel, ColourSpace, Crop, Handle, HandleSide, Interpolation, KeyTarget, MarkerOwner,
    MarkerSpec, MediaId, Project, ProjectSettings, Rational, Resolution, SequenceFormat, SpeedKeep,
    Time, TimeRange, TodoState, TrackKind, Transform, TransitionSpec,
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

/// The settings in the `*-settings.json` fixtures: every field away from its default.
fn custom_settings() -> ProjectSettings {
    ProjectSettings {
        sequence: SequenceFormat {
            resolution: Resolution::UHD,
            frame_rate: Rational::new(30000, 1001),
            drop_frame: true,
            sample_rate: 96_000,
            channel_layout: ChannelLayout::Surround51,
        },
        colour_space: ColourSpace::Rec2020Hlg,
        cache_folder: CacheFolder::Relative("../caches/settings film".into()),
    }
}

/// The project in every `*-settings.json` fixture (from v6): custom project settings, a
/// sequence in the project's format with a clip, and a square mono sequence at 24 fps.
fn settings() -> Project {
    let p = Project::new("fixture-settings", "Settings film");
    let p = p.set_project_settings(custom_settings()).expect("settings");
    let (p, media) = p.add_media("0a1b2c", "/footage/street.mov").expect("media");
    let format = p.settings().sequence;
    let (p, main) = p.add_sequence_with("Main", format).expect("main");
    let (p, v1) = p.add_track(main, TrackKind::Video, "V1", None).expect("v1");
    let ntsc = Rational::new(30000, 1001);
    let (p, _) = p
        .insert_clip(
            v1,
            ClipSpec {
                media,
                range: TimeRange::new(Time::ZERO, Time::from_frames(60, ntsc).expect("frames")),
                source_start: Time::ZERO,
            },
        )
        .expect("clip");
    let (p, square) = p
        .add_sequence("Square", Rational::new(24, 1), 44_100)
        .expect("square");
    let square_format = SequenceFormat {
        resolution: Resolution::SQUARE,
        channel_layout: ChannelLayout::Mono,
        ..p.sequence(square).expect("square").format()
    };
    p.set_sequence_format(square, square_format)
        .expect("square format")
}

fn tint() -> ParamSchema {
    ParamSchema::new(
        "acme.tint",
        "acme.tint",
        vec![ParamDef::colour(
            "colour",
            "acme.tint.colour",
            [1.0, 1.0, 1.0, 1.0],
            true,
        )],
    )
}

/// The project in every `*-keyframes.json` fixture (from v7): clip properties and effect
/// parameters animated with every interpolation, bezier handles, keys at fractional source
/// times outside the visible range, and a curve on a slowed clip.
fn keyframes() -> Project {
    let p = Project::new("fixture-keyframes", "Lighthouse");
    let (p, a) = p
        .add_media("7e8f90", "/footage/lighthouse.mov")
        .expect("media");
    let (p, seq) = p.add_sequence("Night", FPS, 48_000).expect("sequence");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, beam) = p
        .insert_clip(v1, clip(a, Time::ZERO, frames(100), Time::seconds(4)))
        .expect("clip");
    let (p, sea) = p
        .insert_clip(a1, clip(a, Time::ZERO, Time::seconds(4), Time::seconds(4)))
        .expect("clip");
    let p = p
        .set_clip_speed(beam, Rational::new(1, 2), SpeedKeep::Timeline)
        .expect("speed");
    let key = |n, d| Time::new(n, d).expect("time");
    let target = |param| KeyTarget::Clip { item: beam, param };
    let opacity = target(ClipParam::Opacity);
    let p = p.animate_param(opacity, Time::seconds(4)).expect("opacity");
    let p = p
        .set_key(opacity, Time::seconds(4), ParamValue::Float(0.0))
        .expect("key");
    let p = p
        .set_key(opacity, key(9, 2), ParamValue::Float(1.0))
        .expect("key");
    let p = p
        .set_key(opacity, key(11, 2), ParamValue::Float(0.25))
        .expect("key");
    let p = p
        .set_key_interpolation(opacity, key(9, 2), Interpolation::Hold)
        .expect("hold");
    let position = target(ClipParam::Position);
    let p = p
        .animate_param(position, Time::seconds(4))
        .expect("position");
    let p = p
        .set_key(
            position,
            Time::seconds(6),
            ParamValue::Point([-240.0, 96.5]),
        )
        .expect("key");
    let p = p
        .set_key_interpolation(position, Time::seconds(4), Interpolation::ease(2))
        .expect("ease");
    let p = p
        .set_key_handle(
            position,
            Time::seconds(6),
            HandleSide::In,
            Handle::new(vec![-60.0, 12.0], 0.6),
        )
        .expect("handle");
    let rotation = target(ClipParam::Rotation);
    let p = p.animate_param(rotation, Time::ZERO).expect("rotation");
    let p = p
        .set_key(rotation, Time::seconds(8), ParamValue::Angle(720.0))
        .expect("key");
    let feather = target(ClipParam::CropFeather);
    let p = p.animate_param(feather, key(13, 3)).expect("feather");
    let gain = KeyTarget::Clip {
        item: sea,
        param: ClipParam::GainDb,
    };
    let p = p.set_clip_gain(sea, -12.0).expect("gain");
    let p = p.animate_param(gain, Time::seconds(4)).expect("gain");
    let p = p
        .set_key(gain, Time::seconds(7), ParamValue::Float(0.0))
        .expect("key");
    let lut = effects::lut();
    let (p, grade) = p.add_effect(beam, &lut, None).expect("lut");
    let intensity = KeyTarget::Effect {
        effect: grade,
        schema: &lut,
        param: "intensity",
    };
    let p = p
        .animate_param(intensity, Time::seconds(4))
        .expect("intensity");
    let p = p
        .set_key(intensity, Time::seconds(5), ParamValue::Float(0.3))
        .expect("key");
    let p = p
        .set_key_interpolation(
            intensity,
            Time::seconds(4),
            Interpolation::Bezier {
                out: Handle::new(vec![-0.5], 0.75),
                into: Handle::new(vec![0.0], 0.1),
            },
        )
        .expect("bezier");
    let tint = tint();
    let (p, warm) = p.add_effect(beam, &tint, None).expect("tint");
    let colour = KeyTarget::Effect {
        effect: warm,
        schema: &tint,
        param: "colour",
    };
    let p = p.animate_param(colour, Time::seconds(4)).expect("colour");
    p.set_key(
        colour,
        Time::seconds(6),
        ParamValue::Colour([1.0, 0.5, 0.125, 0.5]),
    )
    .expect("key")
}

/// The project in every `*-nested.json` fixture (from v8): a sequence at 24 fps nested in
/// one at 25 fps, slipped and with a transition out of it, and a compound clip made from
/// a selection (a clip and a subtitle) holding a clip with an effect and a marker.
fn nested() -> Project {
    let p = Project::new("fixture-nested", "Ferry crossing");
    let (p, a) = p.add_media("7a8b9c", "/footage/ferry.mov").expect("media");
    let (p, titles) = p
        .add_sequence("Titles", Rational::new(24, 1), 48_000)
        .expect("titles");
    let (p, tv1) = p
        .add_track(titles, TrackKind::Video, "V1", None)
        .expect("tv1");
    let (p, _) = p
        .insert_clip(
            tv1,
            clip(a, Time::ZERO, Time::seconds(3), Time::seconds(10)),
        )
        .expect("title clip");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("main");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).expect("v2");
    let (p, s1) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let (p, shot) = p
        .insert_clip(v1, clip(a, Time::ZERO, frames(50), Time::seconds(1)))
        .expect("shot");
    let (p, _) = p.add_effect(shot, &effects::lut(), None).expect("lut");
    let (p, _) = p
        .add_marker(
            MarkerOwner::Clip(shot),
            MarkerSpec {
                name: "Gull".into(),
                ..MarkerSpec::at(Time::seconds(1), ColourLabel::Blue)
            },
        )
        .expect("marker");
    let (p, cue) = p
        .insert_subtitle(s1, TimeRange::new(frames(10), frames(20)), "Morning")
        .expect("cue");
    let (p, _, _) = p.nest_items(&[shot, cue], "Opening").expect("compound");
    let (p, card) = p
        .insert_sequence_clip(
            v2,
            titles,
            TimeRange::new(frames(50), frames(25)),
            Time::new(1, 2).expect("t"),
        )
        .expect("titles clip");
    p.add_transition(TransitionSpec {
        alignment: Alignment::Start,
        ..TransitionSpec::new(Some(card), None, frames(10))
    })
    .expect("fade")
    .0
}

/// The project each fixture name was saved from.
fn builder(name: &str) -> Project {
    match name {
        "basic" => basic(),
        "tracks" => tracks(),
        "props" => props(),
        "transitions" => transitions(),
        "settings" => settings(),
        "keyframes" => keyframes(),
        "nested" => nested(),
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
    for name in [
        "basic",
        "tracks",
        "props",
        "transitions",
        "settings",
        "keyframes",
        "nested",
    ] {
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
                        ..Clip::new(clip.source, clip.source_start)
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
fn v5_projects_gain_default_settings_and_sequence_formats() {
    for name in ["basic", "tracks", "props", "transitions"] {
        let path = fixtures_dir().join(format!("v5-{name}.json"));
        let v5: Value = serde_json::from_slice(&read(&path)).expect("JSON");
        assert!(v5["project"].get("settings").is_none());
        let migrated = schema::migrate(5, v5["project"].clone()).expect("migrates");
        assert_eq!(
            migrated["settings"]["cache_folder"],
            serde_json::json!({ "relative": ".glass-cache" })
        );
        let loaded = schema::load(&read(&path)).expect("loads");
        assert_eq!(*loaded.settings(), ProjectSettings::default(), "{name}");
        assert_eq!(
            loaded.next_id(),
            v5["project"]["next_id"].as_u64().expect("next id")
        );
        for (seq, old) in loaded
            .sequences()
            .iter()
            .zip(v5["project"]["sequences"].as_array().expect("sequences"))
        {
            assert_eq!(seq.resolution, Resolution::HD, "{name}");
            assert!(!seq.drop_frame);
            assert_eq!(seq.channel_layout, ChannelLayout::Stereo);
            // The rates stay as the v5 file had them.
            assert_eq!(Value::from(seq.sample_rate), old["sample_rate"], "{name}");
            assert_eq!(
                serde_json::to_value(seq.frame_rate).expect("JSON"),
                old["frame_rate"]
            );
        }
        assert!(loaded.content_eq(&builder(name)), "{name}");
    }
}

#[test]
fn v6_clips_and_effects_gain_empty_keyframes() {
    for name in ["basic", "tracks", "props", "transitions", "settings"] {
        let path = fixtures_dir().join(format!("v6-{name}.json"));
        let v6: Value = serde_json::from_slice(&read(&path)).expect("JSON");
        let loaded = schema::load(&read(&path)).expect("loads");
        assert_eq!(
            loaded.next_id(),
            v6["project"]["next_id"].as_u64().expect("next id")
        );
        let mut clips = 0;
        for seq in loaded.sequences() {
            for track in &seq.tracks {
                for item in &track.items {
                    if let Some(clip) = item.clip() {
                        assert!(clip.keyframes.is_empty(), "{name}: item {}", item.id);
                        assert!(clip.effects.iter().all(|e| e.keyframes.is_empty()));
                        clips += 1;
                    }
                }
            }
        }
        assert!(clips >= 1, "{name}");
        assert!(loaded.content_eq(&builder(name)), "{name}");
    }
}

#[test]
fn v7_sequences_gain_compound_false() {
    for name in [
        "basic",
        "tracks",
        "props",
        "transitions",
        "settings",
        "keyframes",
    ] {
        let path = fixtures_dir().join(format!("v7-{name}.json"));
        let v7: Value = serde_json::from_slice(&read(&path)).expect("JSON");
        let loaded = schema::load(&read(&path)).expect("loads");
        assert_eq!(
            loaded.next_id(),
            v7["project"]["next_id"].as_u64().expect("next id")
        );
        assert!(!loaded.sequences().is_empty(), "{name}");
        for seq in loaded.sequences() {
            assert!(!seq.compound, "{name}: sequence {}", seq.id);
            for item in seq.tracks.iter().flat_map(|t| t.items.iter()) {
                if let Some(clip) = item.clip() {
                    assert!(matches!(clip.source, ClipSource::Media(_)), "{name}");
                }
            }
        }
        assert!(loaded.content_eq(&builder(name)), "{name}");
    }
}

#[test]
fn the_nested_fixture_round_trips_sources_and_compounds() {
    let path = fixtures_dir().join(format!("v{SCHEMA_VERSION}-nested.json"));
    let loaded = schema::load(&read(&path)).expect("loads");
    assert!(loaded.content_eq(&nested()));
    let names: Vec<(&str, bool)> = loaded
        .sequences()
        .iter()
        .map(|s| (s.name.as_str(), s.compound))
        .collect();
    assert_eq!(
        names,
        [("Titles", false), ("Main", false), ("Opening", true)]
    );
    let main = &loaded.sequences()[1];
    let sources: Vec<ClipSource> = main
        .tracks
        .iter()
        .flat_map(|t| t.items.iter().filter_map(|i| i.clip().map(|c| c.source)))
        .collect();
    let ids: Vec<_> = loaded.sequences().iter().map(|s| s.id).collect();
    assert_eq!(
        sources,
        [ClipSource::Sequence(ids[2]), ClipSource::Sequence(ids[0])]
    );
    let opening = &loaded.sequences()[2];
    let inner = opening.tracks[0].items[0].clip().expect("shot");
    assert_eq!((inner.effects.len(), inner.markers.len()), (1, 1));
    assert_eq!(opening.tracks[2].items.len(), 1, "the subtitle moved in");
    assert_eq!(main.tracks[1].transitions.len(), 1);
    assert_eq!(schema::save(&loaded), read(&path));
}

#[test]
fn the_keyframes_fixture_round_trips_every_curve() {
    let path = fixtures_dir().join(format!("v{SCHEMA_VERSION}-keyframes.json"));
    let loaded = schema::load(&read(&path)).expect("loads");
    assert!(loaded.content_eq(&keyframes()));
    let clips: Vec<&Clip> = loaded.sequences()[0]
        .tracks
        .iter()
        .flat_map(|t| t.items.iter().filter_map(|i| i.clip()))
        .collect();
    let beam = clips[0];
    let animated: Vec<ClipParam> = beam.keyframes.keys().copied().collect();
    assert_eq!(
        animated,
        [
            ClipParam::Position,
            ClipParam::Rotation,
            ClipParam::CropFeather,
            ClipParam::Opacity
        ]
    );
    let opacity = beam.curve(ClipParam::Opacity).expect("opacity");
    assert_eq!(opacity.keys.len(), 3);
    assert_eq!(opacity.keys[1].interpolation, Interpolation::Hold);
    let position = beam.curve(ClipParam::Position).expect("position");
    assert!(matches!(
        position.keys[0].interpolation,
        Interpolation::Bezier { .. }
    ));
    assert_eq!(beam.effects.len(), 2);
    assert!(beam.effects.iter().all(|e| e.keyframes.len() == 1));
    assert!(clips[1].curve(ClipParam::GainDb).is_some());
    // The same bytes again.
    assert_eq!(schema::save(&loaded), read(&path));
}

#[test]
fn the_settings_fixture_round_trips_every_setting() {
    let p = settings();
    let bytes = schema::save(&p);
    let loaded = schema::load(&bytes).expect("loads");
    assert!(loaded.content_eq(&p));
    assert_eq!(schema::save(&loaded), bytes);
    assert_eq!(*loaded.settings(), custom_settings());
    let main = &loaded.sequences()[0];
    assert_eq!(main.format(), custom_settings().sequence);
    let square = &loaded.sequences()[1];
    assert_eq!(square.resolution, Resolution::SQUARE);
    assert_eq!(square.frame_rate, Rational::new(24, 1));
    assert_eq!(square.sample_rate, 44_100);
    assert_eq!(square.channel_layout, ChannelLayout::Mono);
    let json: Value = serde_json::from_slice(&bytes).expect("JSON");
    let s = &json["project"]["settings"];
    assert_eq!(s["colour_space"], "rec2020_hlg");
    assert_eq!(
        s["cache_folder"],
        serde_json::json!({ "relative": "../caches/settings film" })
    );
    assert_eq!(s["sequence"]["channel_layout"], "surround51");
    assert_eq!(s["sequence"]["drop_frame"], true);
    assert_eq!(
        s["sequence"]["resolution"],
        serde_json::json!({ "width": 3840, "height": 2160 })
    );
    let seq = &json["project"]["sequences"][1];
    assert_eq!(seq["channel_layout"], "mono");
    assert_eq!(seq["drop_frame"], false);
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
