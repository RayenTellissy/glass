//! Contract tests for clip properties (schema v4, Decision 45), written against the
//! contract rather than the implementation: grid rounding at its edges, the exact source
//! duration through chains of speed changes, the order of errors, clamping limits and the
//! names of refused values, no-op sharing, effect stack order, `Project::validate` on
//! hand-built trees, the v3 migration's refusals, and undo, redo and diffs of every edit.

use std::fs;
use std::sync::Arc;

use project::autosave::{self, AutosaveError, AutosaveWriter};
use project::clip::{
    MAX_CROP_PX, MAX_FEATHER_PX, MAX_GAIN_DB, MAX_OFFSET_PX, MAX_SCALE, MIN_GAIN_DB,
};
use project::diff::{Diff, Node, flatten, rebuild};
use project::params::builtin::BLEND_MODES;
use project::params::{ParamDef, ParamSchema, ParamValue, Range, effects};
use project::schema::{self, LoadError, SCHEMA_VERSION};
use project::{
    BlendMode, Clip, ClipSpec, Crop, EditError, EditLabel, EffectId, History, Item, ItemId,
    MediaId, NodeId, Project, Rational, SpeedKeep, Time, TimeRange, TrackId, TrackKind, Transform,
    ValidationError,
};
use serde_json::{Value, json};

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn range(start: i64, len: i64) -> TimeRange {
    TimeRange::new(frames(start), frames(len))
}

fn speed(num: i32, den: i32) -> Rational {
    Rational::new(num, den)
}

fn secs(num: i64, den: i64) -> Time {
    Time::new(num, den).expect("time")
}

fn plus(a: Time, b: Time) -> Time {
    a.checked_add(b).expect("add")
}

struct Fixture {
    project: Project,
    media: MediaId,
    v1: TrackId,
    /// V1: 0..50 frames from source 2 s.
    shot: ItemId,
    /// V1: 100..125 frames from source 0, with nothing after it.
    next: ItemId,
    /// A1: 0..2 s from source 0.
    sound: ItemId,
    subtitle: ItemId,
    adjustment: ItemId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-props-contract", "Props contract");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, s1) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let (p, fx) = p
        .add_track(seq, TrackKind::Adjustment, "FX", None)
        .expect("fx");
    let spec = |range, source_start| ClipSpec {
        media,
        range,
        source_start,
    };
    let (p, shot) = p
        .insert_clip(v1, spec(range(0, 50), Time::seconds(2)))
        .expect("shot");
    let (p, next) = p
        .insert_clip(v1, spec(range(100, 25), Time::ZERO))
        .expect("next");
    let (p, sound) = p
        .insert_clip(
            a1,
            spec(TimeRange::new(Time::ZERO, Time::seconds(2)), Time::ZERO),
        )
        .expect("sound");
    let (p, subtitle) = p.insert_subtitle(s1, range(0, 10), "Hi").expect("cue");
    let (p, adjustment) = p.insert_adjustment(fx, range(0, 10)).expect("adjustment");
    Fixture {
        project: p,
        media,
        v1,
        shot,
        next,
        sound,
        subtitle,
        adjustment,
    }
}

fn clip_of(p: &Project, item: ItemId) -> &Clip {
    p.find_item(item).expect("item").2.clip().expect("clip")
}

fn range_of(p: &Project, item: ItemId) -> TimeRange {
    p.find_item(item).expect("item").2.range
}

fn source_of(p: &Project, item: ItemId) -> TimeRange {
    clip_of(p, item)
        .source_range(range_of(p, item))
        .expect("source range")
}

/// The item's timeline range and its clip's source range after an edit, or the edit's
/// error.
fn ranges_after(
    edit: Result<Project, EditError>,
    item: ItemId,
) -> Result<(TimeRange, TimeRange), EditError> {
    edit.map(|p| (range_of(&p, item), source_of(&p, item)))
}

/// True when `new` is `old` unchanged: equal, and the track holding `item` is still the
/// same node.
fn untouched(old: &Project, new: &Project, item: ItemId) -> bool {
    new.content_eq(old)
        && Arc::ptr_eq(
            old.find_item(item).expect("old").1,
            new.find_item(item).expect("new").1,
        )
}

/// A third-party effect: radius 8 by default, 0 to 100.
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

fn effect_order(p: &Project, item: ItemId) -> Vec<EffectId> {
    clip_of(p, item).effects.iter().map(|e| e.id).collect()
}

#[test]
fn source_ranges_round_their_timeline_duration_half_up_and_to_at_least_one_frame() {
    let f = fixture();
    // At 2x a source duration takes half as long on the timeline.
    let p = f
        .project
        .set_clip_speed(f.shot, speed(2, 1), SpeedKeep::Timeline)
        .expect("2x");
    let ms = secs(1, 1000);
    let cases = [
        // 2.5 frames: a half rounds up, not to even and not down.
        (frames(5), 3),
        // Either side of that half.
        (frames(5).checked_sub(ms).expect("sub"), 2),
        (plus(frames(5), ms), 3),
        // 3.5 frames.
        (frames(7), 4),
        // Half a frame rounds up to one.
        (frames(1), 1),
        // 0.4 of a frame rounds to none, and is kept at one.
        (secs(4, 125), 1),
    ];
    for (duration, timeline) in cases {
        let source = TimeRange::new(Time::seconds(1), duration);
        // The source follows the rounded timeline duration, exactly.
        assert_eq!(
            ranges_after(p.set_clip_source_range(f.shot, source), f.shot),
            Ok((
                range(0, timeline),
                TimeRange::new(Time::seconds(1), frames(2 * timeline))
            )),
            "{duration}"
        );
    }
}

#[test]
fn speed_keeping_the_source_rounds_half_up_and_to_at_least_one_step() {
    let f = fixture();
    let set = |item, s| ranges_after(f.project.set_clip_speed(item, s, SpeedKeep::Source), item);
    // 25 frames at 2x is 12.5 frames: 13, and the source becomes 26 frames.
    assert_eq!(
        set(f.next, speed(2, 1)),
        Ok((range(100, 13), TimeRange::new(Time::ZERO, frames(26))))
    );
    // 25 frames at 4x is 6.25 frames: 6.
    assert_eq!(
        set(f.next, speed(4, 1)),
        Ok((range(100, 6), TimeRange::new(Time::ZERO, frames(24))))
    );
    // 25 frames at 100x is a quarter of a frame, which rounds to none: kept at one frame,
    // which plays 100 frames of source.
    assert_eq!(
        set(f.next, speed(100, 1)),
        Ok((range(100, 1), TimeRange::new(Time::ZERO, frames(100))))
    );
    // On the 48 kHz sample grid: 2 s at 192000/1921 is exactly 960.5 samples, so 961, and
    // the source is 961 samples times the speed.
    let fast = speed(192_000, 1921);
    let samples = Time::from_samples(961, 48_000).expect("samples");
    assert_eq!(
        set(f.sound, fast),
        Ok((
            TimeRange::new(Time::ZERO, samples),
            TimeRange::new(Time::ZERO, secs(3844, 1921))
        ))
    );
    let p = f
        .project
        .set_clip_speed(f.sound, fast, SpeedKeep::Source)
        .expect("fast");
    assert_eq!(clip_of(&p, f.sound).speed.constant_speed(), Some(fast));
}

#[test]
fn the_source_duration_stays_exactly_the_timeline_duration_times_the_speed() {
    let f = fixture();
    let p = &f.project;
    // (speed given, kept, speed stored, timeline frames, source duration)
    let steps = [
        // 25 / 3 = 8.33 frames: 8, which plays 24 frames.
        (speed(3, 1), SpeedKeep::Source, speed(3, 1), 8, frames(24)),
        // 8 frames at 3/7 (given unreduced) play 24/7 frames.
        (
            speed(6, 14),
            SpeedKeep::Timeline,
            speed(3, 7),
            8,
            secs(24, 175),
        ),
        // 24/7 frames at 1x take 3.43 frames: 3, which play 3 frames.
        (speed(1, 1), SpeedKeep::Source, speed(1, 1), 3, frames(3)),
        // 3 frames at 1/10 take 30 frames.
        (speed(1, 10), SpeedKeep::Source, speed(1, 10), 30, frames(3)),
        // 30 frames at 7/3 play 70 frames.
        (
            speed(7, 3),
            SpeedKeep::Timeline,
            speed(7, 3),
            30,
            frames(70),
        ),
    ];
    let mut p = p.clone();
    for (i, (given, keep, stored, timeline, source)) in steps.into_iter().enumerate() {
        p = p.set_clip_speed(f.next, given, keep).expect("speed");
        // The timeline start never moves, nor does the source start.
        assert_eq!(range_of(&p, f.next), range(100, timeline), "step {i}");
        assert_eq!(
            source_of(&p, f.next),
            TimeRange::new(Time::ZERO, source),
            "step {i}"
        );
        assert_eq!(
            clip_of(&p, f.next).speed.constant_speed(),
            Some(stored),
            "step {i}"
        );
    }
    p.validate().expect("valid");
}

#[test]
fn head_trims_at_a_fractional_speed_move_the_source_start_exactly() {
    let f = fixture();
    let p = f
        .project
        .set_clip_speed(f.shot, speed(1, 3), SpeedKeep::Timeline)
        .expect("1/3");
    // One frame off the head at 1/3 skips a third of a frame of source: 2 s + 1/75 s.
    let trimmed = p.trim_item(f.shot, range(1, 49)).expect("trim");
    assert_eq!(clip_of(&trimmed, f.shot).source_start, secs(151, 75));
    // Extending the head again goes back to exactly 2 s.
    let back = trimmed.trim_item(f.shot, range(0, 50)).expect("extend");
    assert_eq!(clip_of(&back, f.shot).source_start, Time::seconds(2));
    // A tail trim leaves the source start.
    let tail = trimmed.trim_item(f.shot, range(1, 20)).expect("tail");
    assert_eq!(clip_of(&tail, f.shot).source_start, secs(151, 75));
}

#[test]
fn argument_errors_come_before_looking_up_the_item_or_effect() {
    let f = fixture();
    let p = &f.project;
    let unknown = ItemId(999);
    let (s, adj) = (f.subtitle, f.adjustment);
    let (p, fx) = p.add_effect(f.shot, &glow(), None).expect("fx");
    let ok = |r: Result<Project, EditError>| r.map(|_| ());

    // Argument checks win over an unknown item and over an item that is not a clip.
    for item in [unknown, s, adj] {
        for keep in [SpeedKeep::Source, SpeedKeep::Timeline] {
            assert_eq!(
                ok(p.set_clip_speed(item, speed(0, 1), keep)),
                Err(EditError::SpeedOutOfRange(speed(0, 1)))
            );
        }
        // Reported as given, not reduced.
        assert_eq!(
            ok(p.set_clip_speed(item, speed(202, 2), SpeedKeep::Source)),
            Err(EditError::SpeedOutOfRange(speed(202, 2)))
        );
        assert_eq!(
            ok(p.set_clip_source_range(item, TimeRange::new(-frames(1), frames(5)))),
            Err(EditError::SourceBeforeStart(-frames(1)))
        );
        assert_eq!(
            ok(p.set_clip_source_range(item, TimeRange::new(Time::ZERO, -frames(2)))),
            Err(EditError::EmptyRange(-frames(2)))
        );
        assert_eq!(
            ok(p.set_clip_source_range(item, TimeRange::new(Time::ZERO, Time::ZERO))),
            Err(EditError::EmptyRange(Time::ZERO))
        );
        assert_eq!(
            ok(p.slip_clip(item, -frames(1))),
            Err(EditError::SourceBeforeStart(-frames(1)))
        );
        assert_eq!(
            ok(p.set_clip_gain(item, f64::NAN)),
            Err(EditError::NotFinite("gain_db"))
        );
        assert_eq!(
            ok(p.set_clip_opacity(item, f64::INFINITY)),
            Err(EditError::NotFinite("opacity"))
        );
        let turned = Transform {
            rotation: f64::NAN,
            ..Transform::IDENTITY
        };
        assert_eq!(
            ok(p.set_clip_transform(item, turned)),
            Err(EditError::NotFinite("rotation"))
        );
        let cropped = Crop {
            left: f64::NEG_INFINITY,
            ..Crop::NONE
        };
        assert_eq!(
            ok(p.set_clip_crop(item, cropped)),
            Err(EditError::NotFinite("crop.left"))
        );
        let bad_schema = ParamSchema::new("Acme Glow", "x", vec![]);
        assert!(
            matches!(
                p.add_effect(item, &bad_schema, Some(99)),
                Err(EditError::InvalidEffectSchema { ref schema, .. }) if schema == "Acme Glow"
            ),
            "{item}"
        );
    }

    // Then the lookups: an unknown item, an item that is not a clip, then the index.
    assert_eq!(
        p.add_effect(unknown, &glow(), Some(99)).map(|_| ()),
        Err(EditError::UnknownItem(unknown))
    );
    for item in [s, adj] {
        assert_eq!(
            p.add_effect(item, &glow(), Some(99)).map(|_| ()),
            Err(EditError::NotAClip(item))
        );
    }
    // An unknown effect before its index or its schema.
    let gone = EffectId(998);
    assert_eq!(
        ok(p.move_effect(gone, 99)),
        Err(EditError::UnknownEffect(gone))
    );
    assert_eq!(
        ok(p.set_effect_param(gone, &effects::lut(), "nope", ParamValue::Bool(true))),
        Err(EditError::UnknownEffect(gone))
    );
    // A schema mismatch before an unknown parameter.
    assert_eq!(
        ok(p.set_effect_param(fx, &effects::lut(), "nope", ParamValue::Bool(true))),
        Err(EditError::EffectSchemaMismatch {
            effect: fx,
            expected: "acme.glow".into(),
            found: "lut".into(),
        })
    );
}

/// The fixture with a second speed key on `shot`, as a file from a later version might
/// hold (deserialised without validation).
fn with_speed_ramp(f: &Fixture) -> Project {
    let mut json = serde_json::to_value(&f.project).expect("ser");
    let keys = json["sequences"][0]["tracks"][0]["items"][0]["speed"]["keys"]
        .as_array_mut()
        .expect("keys");
    keys.push(json!({ "source_time": "1", "speed": { "num": 2, "den": 1 } }));
    serde_json::from_value(json).expect("de")
}

#[test]
fn range_edits_refuse_a_speed_ramp_after_checking_their_arguments() {
    let f = fixture();
    let p = with_speed_ramp(&f);
    assert_eq!(clip_of(&p, f.shot).speed.constant_speed(), None);
    let ok = |r: Result<Project, EditError>| r.map(|_| ());
    assert_eq!(
        ok(p.set_clip_source_range(f.shot, TimeRange::new(Time::ZERO, frames(10)))),
        Err(EditError::UnsupportedSpeedMap)
    );
    assert_eq!(
        ok(p.set_clip_speed(f.shot, speed(2, 1), SpeedKeep::Source)),
        Err(EditError::UnsupportedSpeedMap)
    );
    assert_eq!(
        ok(p.trim_item(f.shot, range(5, 45))),
        Err(EditError::UnsupportedSpeedMap)
    );
    assert_eq!(
        clip_of(&p, f.shot).source_range(range(0, 50)),
        Err(EditError::UnsupportedSpeedMap)
    );
    // Argument errors still come first.
    assert_eq!(
        ok(p.set_clip_source_range(f.shot, TimeRange::new(-frames(1), frames(10)))),
        Err(EditError::SourceBeforeStart(-frames(1)))
    );
    assert_eq!(
        ok(p.set_clip_speed(f.shot, speed(1, 0), SpeedKeep::Source)),
        Err(EditError::SpeedOutOfRange(speed(1, 0)))
    );
    assert_eq!(
        p.validate(),
        Err(ValidationError::Track {
            track: f.v1,
            error: EditError::UnsupportedSpeedMap
        })
    );
}

#[test]
fn speeds_are_accepted_between_the_limits_in_any_form_and_stored_reduced() {
    let f = fixture();
    let stored = |s| {
        f.project
            .set_clip_speed(f.shot, s, SpeedKeep::Timeline)
            .map(|p| clip_of(&p, f.shot).speed.constant_speed())
    };
    assert_eq!(stored(speed(2, 200)), Ok(Some(speed(1, 100))));
    assert_eq!(stored(speed(-200, -2)), Ok(Some(speed(100, 1))));
    assert_eq!(stored(speed(i32::MIN, i32::MIN)), Ok(Some(speed(1, 1))));
    assert_eq!(stored(speed(i32::MIN, i32::MIN / 4)), Ok(Some(speed(4, 1))));
    for bad in [
        speed(3, 301),
        speed(10_001, 100),
        speed(1, -2),
        speed(-1, 2),
        speed(i32::MIN, 1),
        speed(i32::MIN, -1),
        speed(0, 0),
    ] {
        assert_eq!(stored(bad), Err(EditError::SpeedOutOfRange(bad)), "{bad}");
    }
}

#[test]
fn numbers_are_clamped_to_their_limits_and_kept_at_them() {
    let f = fixture();
    let p = &f.project;
    let gain = |db| {
        p.set_clip_gain(f.sound, db)
            .map(|p| clip_of(&p, f.sound).gain_db)
    };
    for (given, kept) in [
        (MIN_GAIN_DB, MIN_GAIN_DB),
        (MAX_GAIN_DB, MAX_GAIN_DB),
        (-96.5, MIN_GAIN_DB),
        (24.5, MAX_GAIN_DB),
        (f64::MAX, MAX_GAIN_DB),
        (f64::MIN, MIN_GAIN_DB),
    ] {
        assert_eq!(gain(given), Ok(kept), "gain {given}");
    }
    let opacity = |v| {
        p.set_clip_opacity(f.shot, v)
            .map(|p| clip_of(&p, f.shot).opacity)
    };
    for (given, kept) in [(0.0, 0.0), (-1e-9, 0.0), (1.000_001, 1.0), (0.5, 0.5)] {
        assert_eq!(opacity(given), Ok(kept), "opacity {given}");
    }

    let transform = |t| {
        p.set_clip_transform(f.shot, t)
            .map(|p| clip_of(&p, f.shot).transform)
    };
    assert_eq!(
        transform(Transform {
            position: [-MAX_OFFSET_PX, 1_000_000.5],
            scale: [-100.5, MAX_SCALE],
            rotation: 1e300,
            anchor: [-1e7, MAX_OFFSET_PX],
        }),
        Ok(Transform {
            position: [-MAX_OFFSET_PX, MAX_OFFSET_PX],
            scale: [-MAX_SCALE, MAX_SCALE],
            // Rotation is unbounded.
            rotation: 1e300,
            anchor: [-MAX_OFFSET_PX, MAX_OFFSET_PX],
        })
    );
    assert_eq!(
        transform(Transform {
            rotation: -7200.5,
            ..Transform::IDENTITY
        })
        .map(|t| t.rotation),
        Ok(-7200.5)
    );

    let crop = |c| p.set_clip_crop(f.shot, c).map(|p| clip_of(&p, f.shot).crop);
    assert_eq!(
        crop(Crop {
            left: MAX_CROP_PX,
            top: 16_384.5,
            right: -0.5,
            bottom: -1e9,
            feather: -3.0,
        }),
        Ok(Crop {
            left: MAX_CROP_PX,
            top: MAX_CROP_PX,
            right: 0.0,
            bottom: 0.0,
            feather: 0.0,
        })
    );
    assert_eq!(
        crop(Crop {
            feather: 1000.5,
            ..Crop::NONE
        })
        .map(|c| c.feather),
        Ok(MAX_FEATHER_PX)
    );
}

type Setter<'a> = Box<dyn Fn(f64) -> Result<Project, EditError> + 'a>;

fn set_transform(p: &Project, item: ItemId, change: fn(&mut Transform, f64)) -> Setter<'_> {
    Box::new(move |v| {
        let mut t = Transform::IDENTITY;
        change(&mut t, v);
        p.set_clip_transform(item, t)
    })
}

fn set_crop(p: &Project, item: ItemId, change: fn(&mut Crop, f64)) -> Setter<'_> {
    Box::new(move |v| {
        let mut c = Crop::NONE;
        change(&mut c, v);
        p.set_clip_crop(item, c)
    })
}

#[test]
fn every_value_that_is_not_finite_is_refused_with_its_name() {
    let f = fixture();
    let (p, shot, sound) = (&f.project, f.shot, f.sound);
    let t = |change| set_transform(p, shot, change);
    let c = |change| set_crop(p, shot, change);
    let slots: Vec<(&'static str, Setter)> = vec![
        ("gain_db", Box::new(move |v| p.set_clip_gain(sound, v))),
        ("opacity", Box::new(move |v| p.set_clip_opacity(shot, v))),
        ("position", t(|t, v| t.position[0] = v)),
        ("position", t(|t, v| t.position[1] = v)),
        ("scale", t(|t, v| t.scale[0] = v)),
        ("scale", t(|t, v| t.scale[1] = v)),
        ("rotation", t(|t, v| t.rotation = v)),
        ("anchor", t(|t, v| t.anchor[0] = v)),
        ("anchor", t(|t, v| t.anchor[1] = v)),
        ("crop.left", c(|c, v| c.left = v)),
        ("crop.top", c(|c, v| c.top = v)),
        ("crop.right", c(|c, v| c.right = v)),
        ("crop.bottom", c(|c, v| c.bottom = v)),
        ("crop.feather", c(|c, v| c.feather = v)),
    ];
    for (name, set) in &slots {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                set(v).map(|_| ()),
                Err(EditError::NotFinite(name)),
                "{name} = {v}"
            );
        }
        // A finite value in the same slot is fine.
        assert!(set(0.5).is_ok(), "{name}");
    }
}

#[test]
fn edits_that_end_where_they_started_return_the_same_tree() {
    let f = fixture();
    let p = &f.project;
    let same = |r: Result<Project, EditError>, item| untouched(p, &r.expect("edit"), item);
    // The same speed, given unreduced, keeping the timeline.
    assert!(same(
        p.set_clip_speed(f.shot, speed(2, 2), SpeedKeep::Timeline),
        f.shot
    ));
    // A source range whose duration rounds back to the clip's own 50 frames.
    let ms = secs(1, 1000);
    assert!(same(
        p.set_clip_source_range(
            f.shot,
            TimeRange::new(Time::seconds(2), plus(frames(50), ms))
        ),
        f.shot
    ));
    // The same source start, written another way.
    assert!(same(p.slip_clip(f.shot, secs(50, 25)), f.shot));
    // Values that clamp to what the clip already holds.
    assert!(same(
        p.set_clip_crop(
            f.shot,
            Crop {
                left: -1.0,
                top: -2.0,
                right: -3.0,
                bottom: -4.0,
                feather: -5.0,
            }
        ),
        f.shot
    ));
    assert!(same(p.set_clip_opacity(f.shot, 2.0), f.shot));
    let loud = p.set_clip_gain(f.sound, MAX_GAIN_DB).expect("loud");
    assert!(untouched(
        &loud,
        &loud.set_clip_gain(f.sound, 1000.0).expect("louder"),
        f.sound
    ));

    let glow = glow();
    let (p, a) = p.add_effect(f.shot, &glow, None).expect("a");
    assert!(untouched(
        &p,
        &p.set_effect_enabled(a, true).expect("on"),
        f.shot
    ));
    assert!(untouched(
        &p,
        &p.move_effect(a, 0).expect("in place"),
        f.shot
    ));
    let big = p
        .set_effect_param(a, &glow, "radius", ParamValue::Float(500.0))
        .expect("big");
    assert!(untouched(
        &big,
        &big.set_effect_param(a, &glow, "radius", ParamValue::Float(100.0))
            .expect("max"),
        f.shot
    ));
}

#[test]
fn edits_that_change_a_clip_share_every_other_item_and_track() {
    let f = fixture();
    let old = &f.project;
    let shares = |new: &Project| {
        let item = |p: &Project, i| p.find_item(i).expect("item").2.clone();
        let track = |p: &Project, i| p.find_item(i).expect("item").1.clone();
        assert!(Arc::ptr_eq(&item(old, f.next), &item(new, f.next)));
        for other in [f.sound, f.subtitle, f.adjustment] {
            assert!(Arc::ptr_eq(&track(old, other), &track(new, other)));
        }
        assert!(new.media().ptr_eq(old.media()));
        assert!(!Arc::ptr_eq(&item(old, f.shot), &item(new, f.shot)));
    };
    shares(
        &old.set_clip_speed(f.shot, speed(2, 1), SpeedKeep::Source)
            .expect("speed"),
    );
    shares(
        &old.set_clip_source_range(f.shot, TimeRange::new(Time::ZERO, frames(80)))
            .expect("source range"),
    );
    shares(&old.add_effect(f.shot, &glow(), None).expect("effect").0);
    shares(&old.slip_clip(f.shot, frames(1)).expect("slip"));
}

#[test]
fn effects_move_to_their_final_index_and_removal_keeps_the_others_in_order() {
    let f = fixture();
    let glow = glow();
    let (p, a) = f.project.add_effect(f.shot, &glow, None).expect("a");
    let (p, b) = p.add_effect(f.shot, &glow, None).expect("b");
    let (p, c) = p.add_effect(f.shot, &glow, None).expect("c");
    // Adding at the length appends.
    let (p, d) = p.add_effect(f.shot, &effects::lut(), Some(3)).expect("d");
    assert_eq!(effect_order(&p, f.shot), [a, b, c, d]);
    // Disabled and with its own value, so a move must carry both.
    let p = p
        .set_effect_enabled(b, false)
        .and_then(|p| p.set_effect_param(b, &glow, "radius", ParamValue::Float(3.0)))
        .expect("b");
    let b_before = p.find_effect(b).expect("b").1.clone();

    for (effect, index, order) in [
        (a, 1, [b, a, c, d]),
        (b, 2, [a, c, b, d]),
        (d, 1, [a, d, b, c]),
        (c, 3, [a, b, d, c]),
        (a, 3, [b, c, d, a]),
        (d, 0, [d, a, b, c]),
    ] {
        let moved = p.move_effect(effect, index).expect("move");
        assert_eq!(effect_order(&moved, f.shot), order, "{effect} to {index}");
        assert_eq!(
            moved.find_effect(b).expect("b").1.as_ref(),
            b_before.as_ref()
        );
    }
    assert_eq!(
        p.move_effect(a, 4).map(|_| ()),
        Err(EditError::EffectIndex { index: 4, len: 4 })
    );

    for (effect, order) in [(a, [b, c, d]), (d, [a, b, c]), (c, [a, b, d])] {
        let removed = p.remove_effect(effect).expect("remove");
        assert_eq!(effect_order(&removed, f.shot), order, "remove {effect}");
        assert_eq!(removed.find_effect(effect), None);
    }

    // A stack of one.
    let (one, e) = f.project.add_effect(f.next, &glow, Some(0)).expect("one");
    assert_eq!(
        one.move_effect(e, 1).map(|_| ()),
        Err(EditError::EffectIndex { index: 1, len: 1 })
    );
    let empty = one.remove_effect(e).expect("remove");
    assert!(clip_of(&empty, f.next).effects.is_empty());
    assert_eq!(
        empty.move_effect(e, 0).map(|_| ()),
        Err(EditError::UnknownEffect(e))
    );
}

#[test]
fn effect_parameters_clamp_within_range_and_fall_back_to_the_default_otherwise() {
    let f = fixture();
    let glow = glow();
    let (p, fx) = f.project.add_effect(f.shot, &glow, None).expect("fx");
    let radius = |v| {
        p.set_effect_param(fx, &glow, "radius", v).map(|p| {
            p.find_effect(fx)
                .expect("fx")
                .1
                .params
                .float(&glow, "radius")
        })
    };
    for (given, stored) in [
        (ParamValue::Float(40.0), 40.0),
        (ParamValue::Float(500.0), 100.0),
        (ParamValue::Float(-1.0), 0.0),
        // Not finite: the default, not a limit.
        (ParamValue::Float(f64::NAN), 8.0),
        (ParamValue::Float(f64::INFINITY), 8.0),
        (ParamValue::Float(f64::NEG_INFINITY), 8.0),
        // The wrong type: the default.
        (ParamValue::Bool(true), 8.0),
        (ParamValue::Int(50), 8.0),
        (ParamValue::Text("50".into()), 8.0),
    ] {
        assert_eq!(radius(given.clone()), Ok(Some(stored)), "{given:?}");
    }
    let lut = effects::lut();
    let (p, l) = p.add_effect(f.shot, &lut, None).expect("lut");
    let p = p
        .set_effect_param(l, &lut, "input", ParamValue::Choice("no-such-space".into()))
        .expect("choice");
    assert_eq!(
        p.find_effect(l).expect("l").1.params.choice(&lut, "input"),
        lut.defaults().choice(&lut, "input")
    );
}

/// The fixture with a glow on `shot`, as JSON (not validated on the way back).
fn props_json() -> (Fixture, EffectId, Value) {
    let f = fixture();
    let (p, fx) = f.project.add_effect(f.shot, &glow(), None).expect("fx");
    let json = serde_json::to_value(&p).expect("ser");
    (Fixture { project: p, ..f }, fx, json)
}

fn shot_json(project: &mut Value) -> &mut Value {
    &mut project["sequences"][0]["tracks"][0]["items"][0]
}

fn with(json: &Value, pointer: &str, value: Value) -> Project {
    let mut json = json.clone();
    *shot_json(&mut json)
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("{pointer}")) = value;
    serde_json::from_value(json).expect("de")
}

#[test]
fn validate_refuses_clip_properties_outside_the_contract() {
    let (f, _, json) = props_json();
    let in_v1 = |error| ValidationError::Track { track: f.v1, error };
    let out = |what, value, min, max| {
        in_v1(EditError::OutOfRange {
            what,
            value,
            min,
            max,
        })
    };
    let key = |time: &str, num: i32, den: i32| json!({ "source_time": time, "speed": { "num": num, "den": den } });
    let (crop, feather) = (MAX_CROP_PX, MAX_FEATHER_PX);
    let cases: Vec<(&str, Value, ValidationError)> = vec![
        (
            "/speed",
            json!({ "keys": [] }),
            in_v1(EditError::UnsupportedSpeedMap),
        ),
        (
            "/speed",
            json!({ "keys": [key("0", 1, 1), key("0", 2, 1)] }),
            in_v1(EditError::UnsupportedSpeedMap),
        ),
        (
            "/speed",
            json!({ "keys": [key("1/25", 1, 1)] }),
            in_v1(EditError::UnsupportedSpeedMap),
        ),
        (
            "/speed/keys/0/speed",
            json!({ "num": -1, "den": -2 }),
            in_v1(EditError::SpeedNotReduced(speed(-1, -2))),
        ),
        (
            "/speed/keys/0/speed",
            json!({ "num": 3, "den": 6 }),
            in_v1(EditError::SpeedNotReduced(speed(3, 6))),
        ),
        (
            "/speed/keys/0/speed",
            json!({ "num": 1, "den": -2 }),
            in_v1(EditError::SpeedOutOfRange(speed(1, -2))),
        ),
        (
            "/speed/keys/0/speed",
            json!({ "num": 101, "den": 1 }),
            in_v1(EditError::SpeedOutOfRange(speed(101, 1))),
        ),
        (
            "/speed/keys/0/speed",
            json!({ "num": 1, "den": 101 }),
            in_v1(EditError::SpeedOutOfRange(speed(1, 101))),
        ),
        (
            "/speed/keys/0/speed",
            json!({ "num": 1, "den": 0 }),
            in_v1(EditError::SpeedOutOfRange(speed(1, 0))),
        ),
        (
            "/speed/keys/0/speed",
            json!({ "num": 0, "den": 1 }),
            in_v1(EditError::SpeedOutOfRange(speed(0, 1))),
        ),
        ("/gain_db", json!(24.5), out("gain_db", 24.5, -96.0, 24.0)),
        ("/gain_db", json!(-96.5), out("gain_db", -96.5, -96.0, 24.0)),
        ("/opacity", json!(-0.25), out("opacity", -0.25, 0.0, 1.0)),
        ("/opacity", json!(1.25), out("opacity", 1.25, 0.0, 1.0)),
        (
            "/transform/position/1",
            json!(1_000_000.5),
            out("position", 1_000_000.5, -1e6, 1e6),
        ),
        (
            "/transform/anchor/0",
            json!(-1_000_001.0),
            out("anchor", -1_000_001.0, -1e6, 1e6),
        ),
        (
            "/transform/scale/0",
            json!(-100.5),
            out("scale", -100.5, -100.0, 100.0),
        ),
        (
            "/transform/scale/1",
            json!(101.0),
            out("scale", 101.0, -100.0, 100.0),
        ),
        ("/crop/left", json!(-1.0), out("crop.left", -1.0, 0.0, crop)),
        (
            "/crop/top",
            json!(16_384.5),
            out("crop.top", 16_384.5, 0.0, crop),
        ),
        (
            "/crop/right",
            json!(-0.5),
            out("crop.right", -0.5, 0.0, crop),
        ),
        (
            "/crop/bottom",
            json!(20_000.0),
            out("crop.bottom", 20_000.0, 0.0, crop),
        ),
        (
            "/crop/feather",
            json!(1000.5),
            out("crop.feather", 1000.5, 0.0, feather),
        ),
        (
            "/effects/0/id",
            json!(f.project.next_id()),
            ValidationError::Unallocated {
                id: NodeId(f.project.next_id()),
                next_id: f.project.next_id(),
            },
        ),
        (
            "/effects/0/id",
            json!(0),
            ValidationError::Unallocated {
                id: NodeId(0),
                next_id: f.project.next_id(),
            },
        ),
        // Ids are unique across node kinds.
        (
            "/effects/0/id",
            json!(f.shot.0),
            ValidationError::DuplicateId(f.shot.node()),
        ),
        (
            "/effects/0/id",
            json!(f.media.0),
            ValidationError::DuplicateId(f.media.node()),
        ),
    ];
    for (pointer, value, error) in cases {
        let p = with(&json, pointer, value.clone());
        assert_eq!(p.validate(), Err(error), "{pointer} = {value}");
    }
    for schema in ["Acme Glow", "", "acme/glow", "ACME.glow"] {
        let p = with(&json, "/effects/0/schema", json!(schema));
        assert!(
            matches!(
                p.validate(),
                Err(ValidationError::Track {
                    track,
                    error: EditError::InvalidEffectSchema { schema: ref s, .. },
                }) if track == f.v1 && s == schema
            ),
            "{schema:?}: {:?}",
            p.validate()
        );
    }
}

#[test]
fn validate_accepts_values_exactly_at_their_limits() {
    let (_, _, json) = props_json();
    let mut low = json.clone();
    let clip = shot_json(&mut low);
    clip["speed"]["keys"][0]["speed"] = json!({ "num": 1, "den": 100 });
    clip["gain_db"] = json!(MIN_GAIN_DB);
    clip["opacity"] = json!(0.0);
    clip["transform"] = json!({
        "position": [-1e6, 1e6],
        "scale": [-100.0, 100.0],
        "rotation": -1e300,
        "anchor": [1e6, -1e6]
    });
    clip["crop"] = json!({
        "left": MAX_CROP_PX, "top": MAX_CROP_PX, "right": MAX_CROP_PX,
        "bottom": MAX_CROP_PX, "feather": MAX_FEATHER_PX
    });
    clip["effects"][0]["schema"] = json!("a-b_c.9");
    let low: Project = serde_json::from_value(low).expect("de");
    assert_eq!(low.validate(), Ok(()));

    let mut high = json;
    let clip = shot_json(&mut high);
    clip["speed"]["keys"][0]["speed"] = json!({ "num": 100, "den": 1 });
    clip["gain_db"] = json!(MAX_GAIN_DB);
    clip["opacity"] = json!(1.0);
    let high: Project = serde_json::from_value(high).expect("de");
    assert_eq!(high.validate(), Ok(()));
}

fn envelope(version: u32, project: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "format": "glass.project",
        "schema_version": version,
        "project": project,
    }))
    .expect("JSON")
}

#[test]
fn loading_rebuilding_and_recovering_all_refuse_an_invalid_clip() {
    let (f, _, json) = props_json();
    let unreduced = json!({ "num": 2, "den": 4 });
    let expected = ValidationError::Track {
        track: f.v1,
        error: EditError::SpeedNotReduced(speed(2, 4)),
    };

    // Load.
    let mut bad = json.clone();
    shot_json(&mut bad)["speed"]["keys"][0]["speed"] = unreduced.clone();
    match schema::load(&envelope(SCHEMA_VERSION, bad.clone())) {
        Err(LoadError::Invalid(error)) => assert_eq!(error, expected),
        other => panic!("{other:?}"),
    }

    // Rebuilding from a diff's flat form.
    let mut flat = flatten(&f.project);
    let item: Item = serde_json::from_value(shot_json(&mut bad).clone()).expect("item");
    let track = f.v1;
    flat.insert(
        f.shot.node(),
        Node::Item {
            track,
            item: Arc::new(item),
        },
    );
    assert_eq!(rebuild(&flat).map(|_| ()), Err(expected.clone()));

    // Recovering an autosave.
    let dir =
        std::env::temp_dir().join(format!("glass-clip-props-contract-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    drop(AutosaveWriter::open(&dir, &f.project, None, 0, 0).expect("open"));
    let snapshot = dir.join("snapshot-00000001.json");
    let mut written: Value =
        serde_json::from_slice(&fs::read(&snapshot).expect("snapshot")).expect("JSON");
    // The untouched snapshot recovers, so the refusal below is the speed's.
    assert!(autosave::recover(&dir).is_ok());
    shot_json(&mut written["project"])["speed"]["keys"][0]["speed"] = unreduced;
    fs::write(&snapshot, serde_json::to_vec(&written).expect("JSON")).expect("write");
    let recovered = autosave::recover(&dir);
    fs::remove_dir_all(&dir).expect("cleanup");
    match recovered {
        Err(AutosaveError::Invalid(error)) => assert_eq!(error, expected),
        other => panic!("{other:?}"),
    }
}

#[test]
fn blend_modes_load_by_their_kebab_ids_in_the_compositing_order() {
    let (_, _, json) = props_json();
    assert_eq!(BlendMode::ALL.len(), BLEND_MODES.len());
    for (mode, id) in BlendMode::ALL.into_iter().zip(BLEND_MODES) {
        let mut file = json.clone();
        shot_json(&mut file)["blend"] = json!(id);
        let loaded = schema::load(&envelope(SCHEMA_VERSION, file)).expect(id);
        let shot = &loaded.sequences()[0].tracks[0].items[0];
        assert_eq!(shot.clip().expect("clip").blend, mode, "{id}");
        assert_eq!(BlendMode::from_id(id), Some(mode));
    }
    for id in ["colour_burn", "ColourBurn", "color-burn"] {
        let mut file = json.clone();
        shot_json(&mut file)["blend"] = json!(id);
        assert!(
            matches!(
                schema::load(&envelope(SCHEMA_VERSION, file)),
                Err(LoadError::Model { .. })
            ),
            "{id}"
        );
    }
}

const V4_CLIP_KEYS: [&str; 8] = [
    "speed",
    "enabled",
    "gain_db",
    "transform",
    "crop",
    "opacity",
    "blend",
    "effects",
];

/// The fixture as a v3 `project` value: clips without their v4 properties, tracks without
/// their v5 transitions.
fn v3_json(f: &Fixture) -> Value {
    let mut json = serde_json::to_value(&f.project).expect("ser");
    for track in json["sequences"][0]["tracks"]
        .as_array_mut()
        .expect("tracks")
    {
        let transitions = track
            .as_object_mut()
            .expect("track")
            .remove("transitions")
            .expect("a v5 track has transitions");
        assert_eq!(transitions, json!([]), "the fixture has no transitions");
        for item in track["items"].as_array_mut().expect("items") {
            if item["type"] == "clip" {
                for key in V4_CLIP_KEYS {
                    item.as_object_mut().expect("item").remove(key).expect(key);
                }
            }
        }
    }
    json
}

#[test]
fn a_v3_clip_already_holding_a_v4_property_is_refused_by_the_migration() {
    let f = fixture();
    let v3 = v3_json(&f);
    // Without one, it loads as the fixture.
    let loaded = schema::load(&envelope(3, v3.clone())).expect("loads");
    assert!(loaded.content_eq(&f.project));

    let defaults = serde_json::to_value(clip_of(&f.project, f.next)).expect("ser");
    for key in V4_CLIP_KEYS {
        // The second clip on V1 holds the key, with its default value.
        let mut file = v3.clone();
        file["sequences"][0]["tracks"][0]["items"][1][key] = defaults[key].clone();
        match schema::load(&envelope(3, file)) {
            Err(LoadError::Migration { from: 3, message }) => {
                assert!(message.contains(key), "{key}: {message}")
            }
            other => panic!("{key}: {other:?}"),
        }
    }

    // Subtitle and adjustment items are left as they are, whatever they hold.
    let mut file = v3.clone();
    let tracks = &mut file["sequences"][0]["tracks"];
    tracks[2]["items"][0]["opacity"] = json!(0.5);
    tracks[3]["items"][0]["enabled"] = json!(false);
    let migrated = schema::migrate(3, file.clone()).expect("migrates");
    for t in [2, 3] {
        assert_eq!(
            migrated["sequences"][0]["tracks"][t]["items"],
            file["sequences"][0]["tracks"][t]["items"]
        );
    }
    assert_eq!(migrated["next_id"], v3["next_id"]);
}

#[test]
fn the_v3_migration_refuses_a_track_that_is_not_an_object() {
    for track in [json!(5), json!("V1"), json!([])] {
        let v3 = json!({ "sequences": [{ "tracks": [track] }] });
        assert!(
            matches!(
                schema::migrate(3, v3),
                Err(LoadError::Migration { from: 3, .. })
            ),
            "{track}"
        );
    }
}

#[test]
fn every_clip_edit_undoes_redoes_and_diffs_both_ways_to_the_same_tree() {
    let f = fixture();
    let (glow, lut) = (glow(), effects::lut());
    let (next, shot) = (f.next, f.shot);
    let mut roots = vec![(None, f.project.clone())];
    let mut push = |label: EditLabel, p: Project| {
        roots.push((Some(label), p.clone()));
        p
    };
    let p = f.project.clone();
    let p = push(
        EditLabel::Speed,
        p.set_clip_speed(next, speed(3, 1), SpeedKeep::Source)
            .expect("source"),
    );
    let p = push(
        EditLabel::Speed,
        p.set_clip_speed(next, speed(1, 2), SpeedKeep::Timeline)
            .expect("timeline"),
    );
    let p = push(
        EditLabel::Trim,
        p.set_clip_source_range(next, TimeRange::new(Time::seconds(1), frames(10)))
            .expect("source range"),
    );
    let p = push(
        EditLabel::Slip,
        p.slip_clip(next, secs(3, 1000)).expect("slip"),
    );
    let p = push(
        EditLabel::Trim,
        p.trim_item(next, range(102, 10)).expect("trim"),
    );
    let p = push(
        EditLabel::Disable,
        p.set_clip_enabled(shot, false).expect("off"),
    );
    let p = push(
        EditLabel::Enable,
        p.set_clip_enabled(shot, true).expect("on"),
    );
    let p = push(
        EditLabel::Properties,
        p.set_clip_gain(f.sound, -12.0).expect("gain"),
    );
    let turned = Transform {
        position: [10.0, -4.5],
        scale: [-1.0, 2.0],
        rotation: 45.0,
        anchor: [3.0, 3.0],
    };
    let p = push(
        EditLabel::Properties,
        p.set_clip_transform(shot, turned).expect("transform"),
    );
    let cropped = Crop {
        top: 12.0,
        feather: 2.5,
        ..Crop::NONE
    };
    let p = push(
        EditLabel::Properties,
        p.set_clip_crop(shot, cropped).expect("crop"),
    );
    let p = push(
        EditLabel::Properties,
        p.set_clip_opacity(shot, 0.25).expect("opacity"),
    );
    let p = push(
        EditLabel::Properties,
        p.set_clip_blend(shot, BlendMode::Overlay).expect("blend"),
    );
    let (p, a) = p.add_effect(shot, &lut, None).expect("a");
    let p = push(EditLabel::AddEffect, p);
    let (p, b) = p.add_effect(shot, &glow, Some(0)).expect("b");
    let p = push(EditLabel::AddEffect, p);
    let p = push(
        EditLabel::Properties,
        p.set_effect_param(b, &glow, "radius", ParamValue::Float(40.0))
            .expect("param"),
    );
    let p = push(
        EditLabel::Properties,
        p.set_effect_enabled(a, false).expect("effect off"),
    );
    let p = push(
        EditLabel::ReorderEffects,
        p.move_effect(b, 1).expect("move"),
    );
    push(EditLabel::RemoveEffect, p.remove_effect(a).expect("remove"));

    // Each root's file bytes when it was made, so a later edit that changed an older
    // root in place would show.
    let bytes: Vec<Vec<u8>> = roots.iter().map(|(_, p)| schema::save(p)).collect();
    let mut h = History::new(f.project.clone());
    for (label, p) in roots.iter().skip(1) {
        h.commit(label.expect("label"), p.clone());
    }
    for i in (0..roots.len() - 1).rev() {
        h.undo().expect("undo");
        assert_eq!(schema::save(h.current()), bytes[i], "undo to {i}");
    }
    for (i, saved) in bytes.iter().enumerate().skip(1) {
        h.redo().expect("redo");
        assert_eq!(&schema::save(h.current()), saved, "redo to {i}");
    }

    // An autosave diff between neighbours rebuilds the newer root, and the reverse diff
    // the older one.
    for (i, pair) in roots.windows(2).enumerate() {
        let (old, new) = (flatten(&pair[0].1), flatten(&pair[1].1));
        for (from, to, target) in [(&old, &new, &pair[1].1), (&new, &old, &pair[0].1)] {
            let diff = Diff::between(from, to);
            assert!(!diff.is_empty(), "step {i}");
            let mut flat = from.clone();
            diff.apply(&mut flat);
            let rebuilt = rebuild(&flat).expect("rebuilds");
            assert_eq!(schema::save(&rebuilt), schema::save(target), "step {i}");
        }
    }
}
