//! Clip properties (schema v4): speed, source and timeline ranges, enabled flag, gain,
//! transform, crop, opacity, blend mode and the effect stack, each set by a pure edit that
//! undo and redo cover.

use std::sync::Arc;

use project::clip::{
    MAX_CROP_PX, MAX_FEATHER_PX, MAX_GAIN_DB, MAX_OFFSET_PX, MAX_SCALE, MAX_SPEED, MIN_GAIN_DB,
    MIN_SPEED,
};
use project::diff::{flatten, rebuild};
use project::params::{ParamDef, ParamSchema, ParamValue, Range, builtin, effects};
use project::{
    BlendMode, Clip, ClipSpec, Crop, EditError, EditLabel, History, ItemId, Project, Rational,
    SpeedKeep, Time, TimeRange, TrackKind, Transform,
};

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

struct Fixture {
    project: Project,
    /// V1: 0..50 from source 2 s, then `next` at 100..125.
    shot: ItemId,
    next: ItemId,
    /// A1: 0..2 s.
    sound: ItemId,
    subtitle: ItemId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-props", "Props");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, s1) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("s1");
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
    Fixture {
        project: p,
        shot,
        next,
        sound,
        subtitle,
    }
}

fn clip_of(p: &Project, item: ItemId) -> &Clip {
    p.find_item(item).expect("item").2.clip().expect("clip")
}

fn range_of(p: &Project, item: ItemId) -> TimeRange {
    p.find_item(item).expect("item").2.range
}

/// True when `new` is `old` untouched: equal, with the track holding `item` still the
/// same node (an edit that changed anything would have copied it).
fn untouched(old: &Project, new: &Project, item: ItemId) -> bool {
    new.content_eq(old)
        && Arc::ptr_eq(
            old.find_item(item).expect("old").1,
            new.find_item(item).expect("new").1,
        )
}

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

#[test]
fn new_clips_have_every_property_at_its_default() {
    let f = fixture();
    let clip = clip_of(&f.project, f.shot);
    assert_eq!(clip.speed.constant_speed(), Some(Rational::ONE));
    assert_eq!(clip.speed.keys[0].source_time, Time::ZERO);
    assert!(clip.enabled);
    assert_eq!(clip.gain_db, 0.0);
    assert_eq!(clip.transform, Transform::IDENTITY);
    assert_eq!(clip.crop, Crop::NONE);
    assert_eq!(clip.opacity, 1.0);
    assert_eq!(clip.blend, BlendMode::Normal);
    assert!(clip.effects.is_empty());
    assert_eq!(*clip, Clip::new(clip.source, Time::seconds(2)));
}

#[test]
fn speed_keeping_the_source_range_rescales_the_timeline_duration() {
    let f = fixture();
    // 50 frames of source at 2x take 25 frames.
    let p = f
        .project
        .set_clip_speed(f.shot, speed(2, 1), SpeedKeep::Source)
        .expect("2x");
    assert_eq!(range_of(&p, f.shot), range(0, 25));
    let clip = clip_of(&p, f.shot);
    assert_eq!(clip.speed.constant_speed(), Some(speed(2, 1)));
    assert_eq!(clip.source_start, Time::seconds(2));
    assert_eq!(
        clip.source_range(range_of(&p, f.shot)),
        Ok(TimeRange::new(Time::seconds(2), Time::seconds(2)))
    );
    // Stored reduced: 6/9 is 2/3. 50 frames of source at 2/3 is exactly 75 frames.
    let p = f
        .project
        .set_clip_speed(f.shot, speed(6, 9), SpeedKeep::Source)
        .expect("2/3");
    assert_eq!(
        clip_of(&p, f.shot).speed.constant_speed(),
        Some(speed(2, 3))
    );
    assert_eq!(range_of(&p, f.shot), range(0, 75));
    // 50 frames at 3x is 16.67 frames: rounded to 17, and the source follows the rounding.
    let p = f
        .project
        .set_clip_speed(f.shot, speed(3, 1), SpeedKeep::Source)
        .expect("3x");
    assert_eq!(range_of(&p, f.shot), range(0, 17));
    assert_eq!(
        clip_of(&p, f.shot).source_range(range(0, 17)),
        Ok(TimeRange::new(Time::seconds(2), frames(51)))
    );
    // 50 frames at 4x is 12.5 frames: halves round up.
    let p = f
        .project
        .set_clip_speed(f.shot, speed(4, 1), SpeedKeep::Source)
        .expect("4x");
    assert_eq!(range_of(&p, f.shot), range(0, 13));
    // Never shorter than one frame.
    let p = f
        .project
        .set_clip_speed(f.shot, MAX_SPEED, SpeedKeep::Source)
        .expect("100x");
    assert_eq!(range_of(&p, f.shot), range(0, 1));
    // Going slower must still fit before the next clip at frame 100.
    assert_eq!(
        f.project
            .set_clip_speed(f.shot, speed(1, 3), SpeedKeep::Source)
            .map(|_| ()),
        Err(EditError::Overlap {
            track: f.project.find_item(f.shot).expect("item").1.id,
            other: f.next,
        })
    );
}

#[test]
fn speed_on_the_sample_grid_rounds_to_samples() {
    let f = fixture();
    // 2 s at 7x is 13714.28 samples at 48 kHz: rounded to 13714.
    let p = f
        .project
        .set_clip_speed(f.sound, speed(7, 1), SpeedKeep::Source)
        .expect("7x");
    assert_eq!(
        range_of(&p, f.sound).duration,
        Time::from_samples(13_714, 48_000).expect("samples")
    );
}

#[test]
fn speed_keeping_the_timeline_range_changes_the_source_duration() {
    let f = fixture();
    let p = f
        .project
        .set_clip_speed(f.shot, speed(1, 2), SpeedKeep::Timeline)
        .expect("half");
    assert_eq!(range_of(&p, f.shot), range(0, 50));
    assert_eq!(
        clip_of(&p, f.shot).source_range(range(0, 50)),
        Ok(TimeRange::new(Time::seconds(2), Time::seconds(1)))
    );
}

#[test]
fn speeds_outside_the_limits_are_refused() {
    let f = fixture();
    for bad in [
        speed(0, 1),
        speed(-1, 1),
        speed(1, 0),
        speed(1, 101),
        speed(101, 1),
    ] {
        for keep in [SpeedKeep::Source, SpeedKeep::Timeline] {
            assert_eq!(
                f.project.set_clip_speed(f.shot, bad, keep).map(|_| ()),
                Err(EditError::SpeedOutOfRange(bad))
            );
        }
    }
    assert!(
        f.project
            .set_clip_speed(f.shot, MIN_SPEED, SpeedKeep::Timeline)
            .is_ok()
    );
}

#[test]
fn head_trims_move_the_source_by_the_trim_times_the_speed() {
    let f = fixture();
    let p = f
        .project
        .set_clip_speed(f.shot, speed(2, 1), SpeedKeep::Timeline)
        .expect("2x");
    // Trimming 5 frames off the head skips 10 frames of source at 2x.
    let p = p.trim_item(f.shot, range(5, 45)).expect("trim");
    assert_eq!(
        clip_of(&p, f.shot).source_start,
        Time::seconds(2).checked_add(frames(10)).expect("add")
    );
    // Extending the head back past the source start is refused.
    let p = f
        .project
        .set_clip_speed(f.shot, speed(4, 1), SpeedKeep::Timeline)
        .expect("4x")
        .move_item(
            f.shot,
            f.project.find_item(f.shot).expect("i").1.id,
            frames(20),
        )
        .expect("move");
    assert_eq!(
        p.trim_item(f.shot, range(7, 63)).map(|_| ()),
        Err(EditError::SourceBeforeStart(-frames(2)))
    );
}

#[test]
fn source_range_sets_the_source_start_and_the_rounded_duration() {
    let f = fixture();
    let p = f
        .project
        .set_clip_source_range(f.shot, TimeRange::new(Time::seconds(5), frames(30)))
        .expect("source range");
    assert_eq!(range_of(&p, f.shot), range(0, 30));
    assert_eq!(clip_of(&p, f.shot).source_start, Time::seconds(5));
    // At 3x, 31 source frames take 10.33 timeline frames: 10.
    let p = p
        .set_clip_speed(f.shot, speed(3, 1), SpeedKeep::Timeline)
        .expect("3x")
        .set_clip_source_range(f.shot, TimeRange::new(Time::ZERO, frames(31)))
        .expect("source range");
    assert_eq!(range_of(&p, f.shot), range(0, 10));
    assert_eq!(clip_of(&p, f.shot).source_start, Time::ZERO);
    // A source range shorter than half a frame of timeline still takes one frame.
    let p = p
        .set_clip_source_range(
            f.shot,
            TimeRange::new(Time::ZERO, Time::new(1, 1000).expect("t")),
        )
        .expect("tiny");
    assert_eq!(range_of(&p, f.shot), range(0, 1));

    let set = |source| f.project.set_clip_source_range(f.shot, source).map(|_| ());
    assert_eq!(
        set(TimeRange::new(-frames(1), frames(10))),
        Err(EditError::SourceBeforeStart(-frames(1)))
    );
    assert_eq!(
        set(TimeRange::new(Time::ZERO, Time::ZERO)),
        Err(EditError::EmptyRange(Time::ZERO))
    );
    assert!(matches!(
        set(TimeRange::new(Time::ZERO, frames(101))),
        Err(EditError::Overlap { other, .. }) if other == f.next
    ));
}

#[test]
fn slipping_changes_only_the_source_start() {
    let f = fixture();
    let p = f.project.slip_clip(f.shot, frames(3)).expect("slip");
    assert_eq!(range_of(&p, f.shot), range(0, 50));
    assert_eq!(clip_of(&p, f.shot).source_start, frames(3));
    // Source times need not sit on the sequence grid.
    let between = Time::new(1, 1000).expect("t");
    assert!(f.project.slip_clip(f.shot, between).is_ok());
    assert_eq!(
        f.project.slip_clip(f.shot, -between).map(|_| ()),
        Err(EditError::SourceBeforeStart(-between))
    );
}

#[test]
fn numeric_properties_are_clamped_and_non_finite_values_refused() {
    let f = fixture();
    let p = &f.project;
    let gain = |db| {
        p.set_clip_gain(f.sound, db)
            .map(|p| clip_of(&p, f.sound).gain_db)
    };
    assert_eq!(gain(-6.0), Ok(-6.0));
    assert_eq!(gain(100.0), Ok(MAX_GAIN_DB));
    assert_eq!(gain(-1000.0), Ok(MIN_GAIN_DB));
    assert_eq!(gain(f64::NAN), Err(EditError::NotFinite("gain_db")));
    assert_eq!(
        gain(f64::NEG_INFINITY),
        Err(EditError::NotFinite("gain_db"))
    );

    let opacity = |v| {
        p.set_clip_opacity(f.shot, v)
            .map(|p| clip_of(&p, f.shot).opacity)
    };
    assert_eq!(opacity(0.4), Ok(0.4));
    assert_eq!(opacity(1.5), Ok(1.0));
    assert_eq!(opacity(-0.5), Ok(0.0));
    assert_eq!(opacity(f64::INFINITY), Err(EditError::NotFinite("opacity")));

    let transform = |t| {
        p.set_clip_transform(f.shot, t)
            .map(|p| clip_of(&p, f.shot).transform)
    };
    let wild = Transform {
        position: [2e6, -2e6],
        scale: [500.0, -500.0],
        rotation: 1080.0,
        anchor: [-3e6, 12.0],
    };
    assert_eq!(
        transform(wild),
        Ok(Transform {
            position: [MAX_OFFSET_PX, -MAX_OFFSET_PX],
            scale: [MAX_SCALE, -MAX_SCALE],
            rotation: 1080.0,
            anchor: [-MAX_OFFSET_PX, 12.0],
        })
    );
    for (bad, what) in [
        (
            Transform {
                rotation: f64::NAN,
                ..Transform::IDENTITY
            },
            "rotation",
        ),
        (
            Transform {
                scale: [1.0, f64::INFINITY],
                ..Transform::IDENTITY
            },
            "scale",
        ),
        (
            Transform {
                position: [f64::NAN, 0.0],
                ..Transform::IDENTITY
            },
            "position",
        ),
        (
            Transform {
                anchor: [0.0, f64::NAN],
                ..Transform::IDENTITY
            },
            "anchor",
        ),
    ] {
        assert_eq!(transform(bad), Err(EditError::NotFinite(what)));
    }

    let crop = |c| p.set_clip_crop(f.shot, c).map(|p| clip_of(&p, f.shot).crop);
    assert_eq!(
        crop(Crop {
            left: -4.0,
            top: 20.0,
            right: 1e9,
            bottom: 0.0,
            feather: 5000.0,
        }),
        Ok(Crop {
            left: 0.0,
            top: 20.0,
            right: MAX_CROP_PX,
            bottom: 0.0,
            feather: MAX_FEATHER_PX,
        })
    );
    assert_eq!(
        crop(Crop {
            feather: f64::NAN,
            ..Crop::NONE
        }),
        Err(EditError::NotFinite("crop.feather"))
    );
}

#[test]
fn enabled_flag_and_blend_mode() {
    let f = fixture();
    let p = f.project.set_clip_enabled(f.shot, false).expect("off");
    assert!(!clip_of(&p, f.shot).enabled);
    assert_eq!(range_of(&p, f.shot), range(0, 50));
    let p = p.set_clip_enabled(f.shot, true).expect("on");
    assert!(p.content_eq(&f.project));
    let p = f
        .project
        .set_clip_blend(f.shot, BlendMode::ColourDodge)
        .expect("blend");
    assert_eq!(clip_of(&p, f.shot).blend, BlendMode::ColourDodge);
}

#[test]
fn property_edits_refuse_items_that_are_not_clips() {
    let f = fixture();
    let p = &f.project;
    let not = Err(EditError::NotAClip(f.subtitle));
    let s = f.subtitle;
    assert_eq!(p.set_clip_enabled(s, false).map(|_| ()), not);
    assert_eq!(p.set_clip_gain(s, 1.0).map(|_| ()), not);
    assert_eq!(p.set_clip_opacity(s, 0.5).map(|_| ()), not);
    assert_eq!(p.set_clip_blend(s, BlendMode::Add).map(|_| ()), not);
    assert_eq!(
        p.set_clip_transform(s, Transform::IDENTITY).map(|_| ()),
        not
    );
    assert_eq!(p.set_clip_crop(s, Crop::NONE).map(|_| ()), not);
    assert_eq!(p.slip_clip(s, Time::ZERO).map(|_| ()), not);
    assert_eq!(
        p.set_clip_speed(s, speed(2, 1), SpeedKeep::Source)
            .map(|_| ()),
        not
    );
    assert_eq!(p.set_clip_source_range(s, range(0, 5)).map(|_| ()), not);
    assert_eq!(p.add_effect(s, &effects::lut(), None).map(|_| ()), not);
    let unknown = ItemId(999);
    assert_eq!(
        p.set_clip_enabled(unknown, false).map(|_| ()),
        Err(EditError::UnknownItem(unknown))
    );
}

#[test]
fn edits_that_change_nothing_return_the_same_root() {
    let f = fixture();
    let p = &f.project;
    let same = |r: Result<Project, EditError>| untouched(p, &r.expect("edit"), f.shot);
    assert!(same(p.set_clip_enabled(f.shot, true)));
    let gain = p.set_clip_gain(f.sound, 0.0).expect("gain");
    assert!(untouched(p, &gain, f.sound));
    assert!(same(p.set_clip_opacity(f.shot, 1.0)));
    assert!(same(p.set_clip_opacity(f.shot, 7.0)));
    assert!(same(p.set_clip_blend(f.shot, BlendMode::Normal)));
    assert!(same(p.set_clip_transform(f.shot, Transform::IDENTITY)));
    assert!(same(p.set_clip_crop(f.shot, Crop::NONE)));
    assert!(same(p.slip_clip(f.shot, Time::seconds(2))));
    assert!(same(p.set_clip_speed(
        f.shot,
        speed(3, 3),
        SpeedKeep::Source
    )));
    assert!(same(p.set_clip_source_range(
        f.shot,
        TimeRange::new(Time::seconds(2), frames(50))
    )));
}

#[test]
fn property_edits_copy_only_the_clip_and_its_track_spine() {
    let f = fixture();
    let p = f.project.set_clip_opacity(f.shot, 0.5).expect("opacity");
    let (_, old_v1, old_shot) = f.project.find_item(f.shot).expect("old");
    let (_, new_v1, new_shot) = p.find_item(f.shot).expect("new");
    assert!(!Arc::ptr_eq(old_shot, new_shot));
    assert!(!Arc::ptr_eq(old_v1, new_v1));
    assert!(Arc::ptr_eq(
        f.project.find_item(f.next).expect("old").2,
        p.find_item(f.next).expect("new").2
    ));
    assert!(Arc::ptr_eq(
        f.project.find_item(f.sound).expect("old").1,
        p.find_item(f.sound).expect("new").1
    ));
    assert!(f.project.media().ptr_eq(p.media()));
}

#[test]
fn effects_are_added_with_their_defaults_in_stack_order() {
    let f = fixture();
    let lut = effects::lut();
    let (p, a) = f.project.add_effect(f.shot, &lut, None).expect("lut");
    let (p, b) = p.add_effect(f.shot, &glow(), None).expect("glow");
    let (p, c) = p.add_effect(f.shot, &lut, Some(0)).expect("first");
    let (p, d) = p.add_effect(f.shot, &glow(), Some(1)).expect("second");
    let ids: Vec<_> = clip_of(&p, f.shot).effects.iter().map(|e| e.id).collect();
    assert_eq!(ids, [c, d, a, b]);
    // Ids come from the project counter: unique and never reused.
    assert_eq!(a.0 + 1, b.0);
    assert_eq!(p.next_id(), d.0 + 1);
    let effect = &clip_of(&p, f.shot).effects[2];
    assert_eq!(effect.schema, "lut");
    assert!(effect.enabled);
    assert_eq!(effect.params, lut.defaults());
    assert_eq!(
        p.find_effect(b).map(|(i, e)| (i, e.schema.as_str())),
        Some((f.shot, "acme.glow"))
    );
    assert!(p.find_effect(project::EffectId(999)).is_none());
    p.validate().expect("valid");

    let len = 4;
    assert_eq!(
        p.add_effect(f.shot, &lut, Some(len + 1)).map(|_| ()),
        Err(EditError::EffectIndex {
            index: len + 1,
            len
        })
    );
    // The same schema on another clip is a separate instance.
    let (p, e) = p.add_effect(f.next, &lut, None).expect("other clip");
    assert_eq!(p.find_effect(e).expect("e").0, f.next);
}

#[test]
fn invalid_effect_schemas_are_refused() {
    let f = fixture();
    let bad_default = ParamSchema::new(
        "x",
        "x",
        vec![ParamDef::float("a", "a", 5.0, Range::new(0.0, 1.0))],
    );
    let bad_id = ParamSchema::new("Fancy Glow", "x", vec![]);
    let empty_id = ParamSchema::new("", "x", vec![]);
    for schema in [bad_default, bad_id, empty_id] {
        assert!(matches!(
            f.project.add_effect(f.shot, &schema, None),
            Err(EditError::InvalidEffectSchema { .. })
        ));
    }
}

#[test]
fn effect_parameters_are_set_through_the_schema() {
    let f = fixture();
    let lut = effects::lut();
    let (p, fx) = f.project.add_effect(f.shot, &lut, None).expect("lut");
    let value = |p: &Project| {
        p.find_effect(fx)
            .expect("fx")
            .1
            .params
            .float(&lut, "intensity")
    };
    let p = p
        .set_effect_param(fx, &lut, "intensity", ParamValue::Float(0.3))
        .expect("set");
    assert_eq!(value(&p), Some(0.3));
    // Clamped to the parameter's range; wrong types and non-finite numbers fall back to
    // the default, as `ParamValues::set` does.
    let clamped = p
        .set_effect_param(fx, &lut, "intensity", ParamValue::Float(4.0))
        .expect("clamp");
    assert_eq!(value(&clamped), Some(1.0));
    let nan = p
        .set_effect_param(fx, &lut, "intensity", ParamValue::Float(f64::NAN))
        .expect("nan");
    assert_eq!(value(&nan), Some(1.0));
    assert_eq!(
        p.set_effect_param(fx, &lut, "nope", ParamValue::Float(1.0))
            .map(|_| ()),
        Err(EditError::UnknownParam {
            schema: "lut".into(),
            param: "nope".into()
        })
    );
    assert_eq!(
        p.set_effect_param(
            fx,
            &builtin::compositing(),
            "opacity",
            ParamValue::Float(1.0)
        )
        .map(|_| ()),
        Err(EditError::EffectSchemaMismatch {
            effect: fx,
            expected: "lut".into(),
            found: "compositing".into()
        })
    );
    let unknown = project::EffectId(999);
    assert_eq!(
        p.set_effect_param(unknown, &lut, "intensity", ParamValue::Float(1.0))
            .map(|_| ()),
        Err(EditError::UnknownEffect(unknown))
    );
    // Setting the value it already has changes nothing.
    assert!(untouched(
        &p,
        &p.set_effect_param(fx, &lut, "intensity", ParamValue::Float(0.3))
            .expect("same"),
        f.shot
    ));
}

#[test]
fn effects_are_enabled_moved_and_removed() {
    let f = fixture();
    let (p, a) = f
        .project
        .add_effect(f.shot, &effects::lut(), None)
        .expect("a");
    let (p, b) = p.add_effect(f.shot, &glow(), None).expect("b");
    let (p, c) = p.add_effect(f.shot, &glow(), None).expect("c");
    let order =
        |p: &Project| -> Vec<_> { clip_of(p, f.shot).effects.iter().map(|e| e.id).collect() };

    let off = p.set_effect_enabled(b, false).expect("off");
    assert!(!off.find_effect(b).expect("b").1.enabled);
    assert_eq!(order(&off), [a, b, c]);
    assert!(untouched(
        &off,
        &off.set_effect_enabled(b, false).expect("again"),
        f.shot
    ));

    assert_eq!(order(&p.move_effect(a, 2).expect("to end")), [b, c, a]);
    assert_eq!(order(&p.move_effect(c, 0).expect("to top")), [c, a, b]);
    assert!(untouched(
        &p,
        &p.move_effect(b, 1).expect("in place"),
        f.shot
    ));
    assert_eq!(
        p.move_effect(a, 3).map(|_| ()),
        Err(EditError::EffectIndex { index: 3, len: 3 })
    );

    let removed = p.remove_effect(b).expect("remove");
    assert_eq!(order(&removed), [a, c]);
    assert!(removed.find_effect(b).is_none());
    assert_eq!(
        removed.remove_effect(b).map(|_| ()),
        Err(EditError::UnknownEffect(b))
    );
    // Removing never frees the id: the next node gets a new one.
    let (removed, d) = removed.add_effect(f.shot, &glow(), None).expect("d");
    assert!(d.0 > c.0);
    removed.validate().expect("valid");
}

#[test]
fn every_property_edit_is_one_undo_step() {
    let f = fixture();
    let lut = effects::lut();
    let mut h = History::new(f.project.clone());
    let mut roots = vec![f.project.clone()];
    let mut commit = |h: &mut History, label, p: Project| {
        h.commit(label, p.clone());
        roots.push(p);
    };

    let p = h
        .current()
        .set_clip_speed(f.shot, speed(2, 1), SpeedKeep::Source);
    commit(&mut h, EditLabel::Speed, p.expect("speed"));
    let source = TimeRange::new(Time::ZERO, frames(40));
    let p = h.current().set_clip_source_range(f.shot, source);
    commit(&mut h, EditLabel::Trim, p.expect("source range"));
    let p = h.current().slip_clip(f.shot, frames(4));
    commit(&mut h, EditLabel::Slip, p.expect("slip"));
    let p = h.current().trim_item(f.shot, range(2, 10));
    commit(&mut h, EditLabel::Trim, p.expect("trim"));
    let p = h.current().set_clip_enabled(f.next, false);
    commit(&mut h, EditLabel::Disable, p.expect("disable"));
    let p = h.current().set_clip_gain(f.sound, -3.0);
    commit(&mut h, EditLabel::Properties, p.expect("gain"));
    let turned = Transform {
        rotation: 90.0,
        ..Transform::IDENTITY
    };
    let p = h.current().set_clip_transform(f.shot, turned);
    commit(&mut h, EditLabel::Properties, p.expect("transform"));
    let cropped = Crop {
        left: 10.0,
        ..Crop::NONE
    };
    let p = h.current().set_clip_crop(f.shot, cropped);
    commit(&mut h, EditLabel::Properties, p.expect("crop"));
    let p = h.current().set_clip_opacity(f.shot, 0.2);
    commit(&mut h, EditLabel::Properties, p.expect("opacity"));
    let p = h.current().set_clip_blend(f.shot, BlendMode::Multiply);
    commit(&mut h, EditLabel::Properties, p.expect("blend"));
    let (p, a) = h.current().add_effect(f.shot, &lut, None).expect("add");
    commit(&mut h, EditLabel::AddEffect, p);
    let (p, b) = h.current().add_effect(f.shot, &glow(), None).expect("add");
    commit(&mut h, EditLabel::AddEffect, p);
    let p = h
        .current()
        .set_effect_param(a, &lut, "intensity", ParamValue::Float(0.5));
    commit(&mut h, EditLabel::Properties, p.expect("param"));
    let p = h.current().set_effect_enabled(a, false);
    commit(&mut h, EditLabel::Properties, p.expect("effect off"));
    let p = h.current().move_effect(b, 0);
    commit(&mut h, EditLabel::ReorderEffects, p.expect("move"));
    let p = h.current().remove_effect(a);
    commit(&mut h, EditLabel::RemoveEffect, p.expect("remove"));
    let p = h.current().set_clip_enabled(f.next, true);
    commit(&mut h, EditLabel::Enable, p.expect("enable"));

    let steps = roots.len() - 1;
    assert_eq!(steps, 17);
    assert_eq!(h.undo_depth(), steps);
    assert_eq!(h.undo_label(), Some(EditLabel::Enable));
    for i in (0..steps).rev() {
        h.undo().expect("undo");
        assert!(h.current().content_eq(&roots[i]), "undo to step {i}");
        h.current().validate().expect("valid");
    }
    assert!(!h.can_undo());
    for (i, root) in roots.iter().enumerate().skip(1) {
        h.redo().expect("redo");
        assert!(h.current().content_eq(root), "redo to step {i}");
    }
    assert!(!h.can_redo());
}

#[test]
fn clip_properties_survive_flatten_and_rebuild() {
    let f = fixture();
    let lut = effects::lut();
    let (p, fx) = f.project.add_effect(f.shot, &lut, None).expect("fx");
    let p = p
        .set_effect_param(fx, &lut, "input", ParamValue::Choice("srgb".into()))
        .expect("param")
        .set_clip_speed(f.sound, speed(1, 4), SpeedKeep::Timeline)
        .expect("speed")
        .set_clip_transform(
            f.shot,
            Transform {
                position: [1.5, -2.25],
                scale: [0.5, 0.5],
                rotation: -30.0,
                anchor: [4.0, 4.0],
            },
        )
        .expect("transform");
    let rebuilt = rebuild(&flatten(&p)).expect("rebuilds");
    assert!(rebuilt.content_eq(&p));
    let json = serde_json::to_string(&p).expect("ser");
    let back: Project = serde_json::from_str(&json).expect("de");
    assert!(back.content_eq(&p));
    assert_eq!(serde_json::to_string(&back).expect("ser"), json);
}
