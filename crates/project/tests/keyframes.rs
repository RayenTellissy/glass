//! Keyframe curves (schema v7): evaluation, every edit with undo and redo, refusals,
//! validation of loaded trees, and keys staying on their source frames through clip edits.

use std::sync::Arc;

use project::keyframe::{EASE_INFLUENCE, cubic_bezier};
use project::params::{ParamDef, ParamSchema, ParamValue, Range, builtin, effects};
use project::schema::{self, LoadError};
use project::{
    ClipParam, ClipSpec, Curve, EditError, EditLabel, EffectId, Handle, HandleSide, History,
    Interpolation, ItemId, KeyTarget, Project, Rational, SpeedKeep, Time, TimeRange, TrackKind,
    Transform, ValidationError,
};
use serde_json::Value;

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn secs(num: i64, den: i64) -> Time {
    Time::new(num, den).expect("time")
}

struct Fixture {
    project: Project,
    /// V1: frames 0..50, showing source from 2 s.
    shot: ItemId,
    /// A1: 0..2 s from source 0.
    sound: ItemId,
    subtitle: ItemId,
    /// A LUT on `shot`.
    lut: EffectId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-keys", "Keys");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, s1) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let (p, shot) = p
        .insert_clip(
            v1,
            ClipSpec {
                media,
                range: TimeRange::new(Time::ZERO, frames(50)),
                source_start: Time::seconds(2),
            },
        )
        .expect("shot");
    let (p, sound) = p
        .insert_clip(
            a1,
            ClipSpec {
                media,
                range: TimeRange::new(Time::ZERO, Time::seconds(2)),
                source_start: Time::ZERO,
            },
        )
        .expect("sound");
    let (p, subtitle) = p
        .insert_subtitle(s1, TimeRange::new(Time::ZERO, frames(10)), "Hi")
        .expect("cue");
    let (p, lut) = p.add_effect(shot, &effects::lut(), None).expect("lut");
    Fixture {
        project: p,
        shot,
        sound,
        subtitle,
        lut,
    }
}

fn clip(item: ItemId, param: ClipParam) -> KeyTarget<'static> {
    KeyTarget::Clip { item, param }
}

fn float(v: f64) -> ParamValue {
    ParamValue::Float(v)
}

fn value(p: &Project, target: KeyTarget<'_>, time: Time) -> ParamValue {
    p.param_value_at(target, time).expect("value")
}

fn number(p: &Project, target: KeyTarget<'_>, time: Time) -> f64 {
    value(p, target, time).as_f64().expect("number")
}

fn curve(p: &Project, target: KeyTarget<'_>) -> Arc<Curve> {
    p.param_curve(target).expect("target").expect("animated")
}

fn times(p: &Project, target: KeyTarget<'_>) -> Vec<Time> {
    curve(p, target).keys.iter().map(|k| k.time).collect()
}

/// Opacity on `shot` keyed 0 at 2 s and 1 at 3 s (source time).
fn faded(f: &Fixture) -> Project {
    let target = clip(f.shot, ClipParam::Opacity);
    let p = f
        .project
        .animate_param(target, Time::seconds(2))
        .expect("animate");
    let p = p
        .set_key(target, Time::seconds(2), float(0.0))
        .expect("key");
    p.set_key(target, Time::seconds(3), float(1.0))
        .expect("key")
}

#[test]
fn a_constant_becomes_a_curve_and_back() {
    let f = fixture();
    let target = clip(f.shot, ClipParam::Opacity);
    let p = f.project.set_clip_opacity(f.shot, 0.4).expect("opacity");
    assert_eq!(p.param_curve(target), Ok(None));
    let animated = p.animate_param(target, Time::seconds(3)).expect("animate");
    let keys = &curve(&animated, target).keys;
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].time, Time::seconds(3));
    assert_eq!(keys[0].value, float(0.4));
    assert_eq!(keys[0].interpolation, Interpolation::Linear);
    assert_eq!(
        animated.animate_param(target, Time::ZERO),
        Err(EditError::AlreadyAnimated("opacity".into()))
    );
    let animated = animated
        .set_key(target, Time::seconds(4), float(0.8))
        .expect("key");
    // Back to a constant: the value at the given time.
    let constant = animated
        .make_param_constant(target, secs(7, 2))
        .expect("constant");
    assert_eq!(constant.param_curve(target), Ok(None));
    let opacity = number(&constant, target, Time::ZERO);
    assert!((opacity - 0.6).abs() < 1e-12, "{opacity}");
    assert_eq!(
        constant.make_param_constant(target, Time::ZERO),
        Err(EditError::NotAnimated("opacity".into()))
    );
    assert!(constant.validate().is_ok());
}

#[test]
fn linear_hold_and_boundaries_are_exact() {
    let f = fixture();
    let target = clip(f.shot, ClipParam::Opacity);
    let p = faded(&f);
    assert_eq!(number(&p, target, Time::ZERO), 0.0, "before the first key");
    assert_eq!(number(&p, target, Time::seconds(2)), 0.0);
    assert_eq!(number(&p, target, secs(5, 2)), 0.5);
    assert_eq!(number(&p, target, secs(11, 4)), 0.75);
    assert_eq!(number(&p, target, Time::seconds(3)), 1.0);
    assert_eq!(number(&p, target, Time::seconds(60)), 1.0, "after the last");
    let held = p
        .set_key_interpolation(target, Time::seconds(2), Interpolation::Hold)
        .expect("hold");
    assert_eq!(number(&held, target, secs(2999, 1000)), 0.0);
    assert_eq!(number(&held, target, Time::seconds(3)), 1.0);
    // The clip's readers see the curve, not the constant.
    let c = held
        .find_item(f.shot)
        .expect("shot")
        .2
        .clip()
        .expect("clip");
    assert_eq!(c.opacity, 1.0);
    assert_eq!(c.opacity_at(secs(5, 2)), 0.0);
    assert_eq!(c.opacity_at(Time::seconds(3)), 1.0);
}

#[test]
fn beziers_ease_overshoot_within_the_range_and_stay_functions_of_time() {
    let f = fixture();
    let target = clip(f.shot, ClipParam::CropLeft);
    let p = f
        .project
        .animate_param(target, Time::ZERO)
        .expect("animate");
    let p = p
        .set_key(target, Time::seconds(1), float(100.0))
        .expect("key");
    let eased = p
        .set_key_interpolation(target, Time::ZERO, Interpolation::ease(1))
        .expect("ease");
    let mid = number(&eased, target, secs(1, 2));
    assert!((mid - 50.0).abs() < 1e-9, "{mid}");
    assert!(
        number(&eased, target, secs(1, 10)) < 10.0,
        "eases out slowly"
    );
    assert!(
        number(&eased, target, secs(9, 10)) > 90.0,
        "eases in slowly"
    );
    // Full influence on both sides: the time coordinate still never goes backwards, so
    // with flat handles the value only rises.
    let full = Interpolation::Bezier {
        out: Handle::new(vec![0.0], 1.0),
        into: Handle::new(vec![0.0], 1.0),
    };
    let full = p
        .set_key_interpolation(target, Time::ZERO, full)
        .expect("full");
    let mut last = -1.0;
    for i in 0..=200 {
        let v = number(&full, target, secs(i, 200));
        assert!(v >= last, "{i}: {v} < {last}");
        last = v;
    }
    assert_eq!(last, 100.0);
    // Influences beyond 1 are clamped when set.
    let wide = Interpolation::Bezier {
        out: Handle::new(vec![0.0], 7.0),
        into: Handle::new(vec![0.0], -2.0),
    };
    let clamped = p
        .set_key_interpolation(target, Time::ZERO, wide)
        .expect("clamped");
    match &curve(&clamped, target).keys[0].interpolation {
        Interpolation::Bezier { out, into } => {
            assert_eq!(out.influence, 1.0);
            assert_eq!(into.influence, 0.0);
        }
        other => panic!("{other:?}"),
    }
    // A steep handle overshoots, and the reader clamps to the crop's range (0 or more).
    let steep = Interpolation::Bezier {
        out: Handle::new(vec![-2000.0], 1.0),
        into: Handle::new(vec![0.0], EASE_INFLUENCE),
    };
    let steep = p
        .set_key_interpolation(target, Time::ZERO, steep)
        .expect("steep");
    let raw = curve(&steep, target)
        .value_at(secs(1, 10))
        .and_then(|v| v.as_f64())
        .expect("raw");
    assert!(raw < 0.0, "{raw}");
    assert_eq!(number(&steep, target, secs(1, 10)), 0.0);
}

#[test]
fn the_bezier_matches_a_css_cubic_bezier_when_normalised() {
    // Keys 0 → 10 over 2 s with handles (speed, influence) map to the CSS control points
    // (influence_out, speed_out × influence_out × D / Δv) and (1 − influence_in, ...).
    let f = fixture();
    let target = clip(f.shot, ClipParam::CropTop);
    let p = f
        .project
        .animate_param(target, Time::ZERO)
        .expect("animate");
    let p = p
        .set_key(target, Time::seconds(2), float(10.0))
        .expect("key");
    let (out, into) = (Handle::new(vec![3.0], 0.25), Handle::new(vec![12.0], 0.5));
    let p = p
        .set_key_interpolation(
            target,
            Time::ZERO,
            Interpolation::Bezier {
                out: out.clone(),
                into: into.clone(),
            },
        )
        .expect("bezier");
    let (x1, y1) = (0.25, 3.0 * 0.25 * 2.0 / 10.0);
    let (x2, y2) = (1.0 - 0.5, 1.0 - 12.0 * 0.5 * 2.0 / 10.0);
    for i in 1..20 {
        let x = f64::from(i) / 20.0;
        let expected = 10.0 * cubic_bezier(x1, y1, x2, y2, x);
        let got = number(&p, target, secs(i64::from(i), 10));
        assert!((got - expected).abs() < 1e-9, "{x}: {got} vs {expected}");
    }
}

#[test]
fn points_colours_and_angles_interpolate_per_component() {
    let f = fixture();
    let position = clip(f.shot, ClipParam::Position);
    let p = f
        .project
        .animate_param(position, Time::ZERO)
        .expect("animate");
    let p = p
        .set_key(
            position,
            Time::seconds(2),
            ParamValue::Point([100.0, -50.0]),
        )
        .expect("key");
    assert_eq!(
        value(&p, position, Time::seconds(1)),
        ParamValue::Point([50.0, -25.0])
    );
    // Rotation 0 → 720 spins twice: no wrapping.
    let rotation = clip(f.shot, ClipParam::Rotation);
    let p = p.animate_param(rotation, Time::ZERO).expect("animate");
    let p = p
        .set_key(rotation, Time::seconds(2), ParamValue::Angle(720.0))
        .expect("key");
    assert_eq!(
        value(&p, rotation, Time::seconds(1)),
        ParamValue::Angle(360.0)
    );
    let shot = p.find_item(f.shot).expect("shot").2.clip().expect("clip");
    assert_eq!(
        shot.transform_at(secs(1, 2)),
        Transform {
            position: [25.0, -12.5],
            rotation: 180.0,
            ..Transform::IDENTITY
        }
    );
    // Colours per channel, in the stored linear light, straight alpha.
    let tint = ParamSchema::new(
        "acme.tint",
        "acme.tint",
        vec![
            ParamDef::colour("colour", "acme.tint.colour", [0.0, 0.0, 0.0, 1.0], true),
            ParamDef::int("steps", "acme.tint.steps", 0, Range::new(0.0, 10.0)),
        ],
    );
    let (p, tinted) = p.add_effect(f.shot, &tint, None).expect("tint");
    let colour = KeyTarget::Effect {
        effect: tinted,
        schema: &tint,
        param: "colour",
    };
    let p = p.animate_param(colour, Time::ZERO).expect("animate");
    let p = p
        .set_key(
            colour,
            Time::seconds(2),
            ParamValue::Colour([1.0, 0.5, 0.0, 0.0]),
        )
        .expect("key");
    assert_eq!(
        value(&p, colour, Time::seconds(1)),
        ParamValue::Colour([0.5, 0.25, 0.0, 0.5])
    );
    // Ints interpolate and round.
    let steps = KeyTarget::Effect {
        effect: tinted,
        schema: &tint,
        param: "steps",
    };
    let p = p.animate_param(steps, Time::ZERO).expect("animate");
    let p = p
        .set_key(steps, Time::seconds(4), ParamValue::Int(3))
        .expect("key");
    assert_eq!(value(&p, steps, Time::seconds(1)), ParamValue::Int(1));
    assert_eq!(value(&p, steps, Time::seconds(2)), ParamValue::Int(2));
}

#[test]
fn effect_parameters_animate_and_clamp_through_their_schema() {
    let f = fixture();
    let lut = effects::lut();
    let intensity = KeyTarget::Effect {
        effect: f.lut,
        schema: &lut,
        param: "intensity",
    };
    let p = f
        .project
        .set_effect_param(f.lut, &lut, "intensity", float(0.5))
        .expect("intensity");
    let p = p
        .animate_param(intensity, Time::seconds(2))
        .expect("animate");
    // Values are clamped to the schema's range when set.
    let p = p
        .set_key(intensity, Time::seconds(3), float(4.0))
        .expect("key");
    assert_eq!(curve(&p, intensity).keys[1].value, float(1.0));
    assert_eq!(number(&p, intensity, secs(5, 2)), 0.75);
    let (_, effect) = p.find_effect(f.lut).expect("lut");
    assert_eq!(
        effect.param_at(&lut, "intensity", Time::seconds(3)),
        Some(float(1.0))
    );
    assert_eq!(effect.param_at(&lut, "nope", Time::ZERO), None);
    // The stored constant is untouched while animated.
    assert_eq!(effect.params.float(&lut, "intensity"), Some(0.5));
    // Non-animatable parameters are refused.
    for param in ["file", "input", "enabled"] {
        let target = KeyTarget::Effect {
            effect: f.lut,
            schema: &lut,
            param,
        };
        assert_eq!(
            p.animate_param(target, Time::ZERO),
            Err(EditError::NotAnimatable(format!("lut/{param}")))
        );
    }
    let motion = builtin::motion_blur();
    let wrong = KeyTarget::Effect {
        effect: f.lut,
        schema: &motion,
        param: "shutter",
    };
    assert!(matches!(
        p.animate_param(wrong, Time::ZERO),
        Err(EditError::EffectSchemaMismatch { .. })
    ));
    let missing = KeyTarget::Effect {
        effect: f.lut,
        schema: &lut,
        param: "radius",
    };
    assert!(matches!(
        p.animate_param(missing, Time::ZERO),
        Err(EditError::UnknownParam { .. })
    ));
    let gone = KeyTarget::Effect {
        effect: EffectId(9999),
        schema: &lut,
        param: "intensity",
    };
    assert_eq!(
        p.animate_param(gone, Time::ZERO),
        Err(EditError::UnknownEffect(EffectId(9999)))
    );
    // A numeric parameter its schema marks not keyframeable is refused too.
    let fixed = ParamSchema::new(
        "acme.fixed",
        "acme.fixed",
        vec![
            ParamDef::int("samples", "acme.fixed.samples", 8, Range::new(2.0, 16.0))
                .keyframeable(false),
        ],
    );
    let (p, fixed_fx) = p.add_effect(f.shot, &fixed, None).expect("fixed");
    let samples = KeyTarget::Effect {
        effect: fixed_fx,
        schema: &fixed,
        param: "samples",
    };
    assert_eq!(
        p.animate_param(samples, Time::ZERO),
        Err(EditError::NotAnimatable("acme.fixed/samples".into()))
    );
    assert!(p.validate().is_ok());
}

#[test]
fn every_edit_is_one_undo_step() {
    let f = fixture();
    let target = clip(f.shot, ClipParam::Scale);
    let mut h = History::new(f.project.clone());
    let mut roots = vec![f.project.clone()];
    let mut commit = |h: &mut History, label, p: Project| {
        h.commit(label, p.clone());
        roots.push(p);
    };
    let p = f
        .project
        .animate_param(target, Time::seconds(2))
        .expect("a");
    commit(&mut h, EditLabel::Animate, p.clone());
    let p = p
        .set_key(target, Time::seconds(4), ParamValue::Point([2.0, 2.0]))
        .expect("add");
    commit(&mut h, EditLabel::AddKeyframe, p.clone());
    let p = p
        .set_key(target, Time::seconds(4), ParamValue::Point([3.0, 1.5]))
        .expect("change");
    commit(&mut h, EditLabel::ChangeKeyframe, p.clone());
    let p = p
        .set_key(target, Time::seconds(3), ParamValue::Point([1.0, 1.0]))
        .expect("add");
    commit(&mut h, EditLabel::AddKeyframe, p.clone());
    let p = p
        .move_key(target, Time::seconds(3), secs(7, 2))
        .expect("move");
    commit(&mut h, EditLabel::MoveKeyframe, p.clone());
    let p = p
        .set_key_interpolation(target, Time::seconds(2), Interpolation::ease(2))
        .expect("ease");
    commit(&mut h, EditLabel::KeyframeInterpolation, p.clone());
    let p = p
        .set_key_handle(
            target,
            secs(7, 2),
            HandleSide::In,
            Handle::new(vec![1.0, -1.0], 0.5),
        )
        .expect("handle");
    commit(&mut h, EditLabel::KeyframeHandles, p.clone());
    let p = p
        .scale_key_times(
            target,
            &[secs(7, 2), Time::seconds(4)],
            Time::seconds(2),
            Rational::new(2, 1),
        )
        .expect("stretch");
    commit(&mut h, EditLabel::StretchKeyframes, p.clone());
    let p = p
        .scale_key_values(
            target,
            &[Time::seconds(6)],
            ParamValue::Point([1.0, 1.0]),
            0.5,
        )
        .expect("scale");
    commit(&mut h, EditLabel::ScaleKeyframeValues, p.clone());
    let p = p.remove_key(target, Time::seconds(5)).expect("remove");
    commit(&mut h, EditLabel::DeleteKeyframe, p.clone());
    let p = p
        .make_param_constant(target, Time::seconds(6))
        .expect("stop");
    commit(&mut h, EditLabel::StopAnimating, p.clone());
    assert_eq!(h.undo_depth(), roots.len() - 1);
    for root in roots.iter().rev().skip(1) {
        assert!(h.undo().expect("undo").content_eq(root));
    }
    assert!(!h.can_undo());
    for root in roots.iter().skip(1) {
        assert!(h.redo().expect("redo").content_eq(root));
    }
    for root in &roots {
        root.validate().expect("every step is valid");
    }
    // The last keys were [1, 1] at 2 s and [3, 1.5] stretched to 6 s and halved about
    // [1, 1]; the constant is the value at 6 s.
    let last = roots.last().expect("last");
    assert_eq!(last.param_curve(target), Ok(None));
    let scale = last
        .find_item(f.shot)
        .expect("shot")
        .2
        .clip()
        .expect("clip")
        .transform
        .scale;
    assert_eq!(scale, [2.0, 1.25]);
}

#[test]
fn moving_removing_and_scaling_keys() {
    let f = fixture();
    let target = clip(f.shot, ClipParam::GainDb);
    let p = f
        .project
        .animate_param(target, Time::seconds(1))
        .expect("animate");
    let p = p
        .set_key(target, Time::seconds(1), float(-6.0))
        .expect("key");
    let p = p
        .set_key(target, Time::seconds(2), float(0.0))
        .expect("key");
    let p = p
        .set_key(target, Time::seconds(4), float(6.0))
        .expect("key");
    // A move past a neighbour re-sorts, carrying value and interpolation.
    let held = p
        .set_key_interpolation(target, Time::seconds(1), Interpolation::Hold)
        .expect("hold");
    let moved = held
        .move_key(target, Time::seconds(1), Time::seconds(3))
        .expect("move");
    assert_eq!(
        times(&moved, target),
        [Time::seconds(2), Time::seconds(3), Time::seconds(4)]
    );
    let key = &curve(&moved, target).keys[1];
    assert_eq!(
        (key.value.clone(), key.interpolation.clone()),
        (float(-6.0), Interpolation::Hold)
    );
    assert_eq!(
        held.move_key(target, Time::seconds(1), Time::seconds(2)),
        Err(EditError::KeyTimeTaken(Time::seconds(2)))
    );
    assert_eq!(
        held.move_key(target, Time::seconds(9), Time::seconds(8)),
        Err(EditError::NoKeyAt(Time::seconds(9)))
    );
    assert_eq!(
        held.move_key(target, Time::seconds(1), -Time::seconds(1)),
        Err(EditError::KeyBeforeZero(-Time::seconds(1)))
    );
    // Moving a key onto itself changes nothing.
    let same = held
        .move_key(target, Time::seconds(1), Time::seconds(1))
        .expect("same");
    assert!(untouched(&held, &same, f.shot));
    // Time scaling about an anchor, exact.
    let half = p
        .scale_key_times(
            target,
            &[Time::seconds(2), Time::seconds(4)],
            Time::seconds(1),
            Rational::new(1, 2),
        )
        .expect("half");
    assert_eq!(
        times(&half, target),
        [Time::seconds(1), secs(3, 2), secs(5, 2)]
    );
    // A selected key landing on an unselected one is refused.
    assert_eq!(
        p.scale_key_times(target, &[Time::seconds(2)], Time::ZERO, Rational::new(1, 2)),
        Err(EditError::KeyTimeTaken(Time::seconds(1)))
    );
    assert_eq!(
        p.scale_key_times(
            target,
            &[Time::seconds(1)],
            Time::seconds(2),
            Rational::new(3, 1)
        ),
        Err(EditError::KeyBeforeZero(-Time::seconds(1)))
    );
    for bad in [
        Rational::new(0, 1),
        Rational::new(-1, 2),
        Rational::new(1, 0),
    ] {
        assert_eq!(
            p.scale_key_times(target, &[Time::seconds(1)], Time::ZERO, bad),
            Err(EditError::TimeScale(bad))
        );
    }
    assert_eq!(
        p.scale_key_times(target, &[secs(3, 2)], Time::ZERO, Rational::new(2, 1)),
        Err(EditError::NoKeyAt(secs(3, 2)))
    );
    // Value scaling about an anchor, clamped to the gain's range.
    let louder = p
        .scale_key_values(
            target,
            &[Time::seconds(1), Time::seconds(4)],
            float(0.0),
            10.0,
        )
        .expect("louder");
    let values: Vec<_> = curve(&louder, target)
        .keys
        .iter()
        .map(|k| k.value.clone())
        .collect();
    assert_eq!(values, [float(-60.0), float(0.0), float(24.0)]);
    assert_eq!(
        p.scale_key_values(target, &[Time::seconds(1)], float(0.0), f64::NAN),
        Err(EditError::NotFinite("value scale"))
    );
    assert!(matches!(
        p.scale_key_values(target, &[Time::seconds(1)], ParamValue::Angle(0.0), 2.0),
        Err(EditError::KeyValueKind { .. })
    ));
    // An empty selection changes nothing.
    let none = p
        .scale_key_values(target, &[], float(0.0), 2.0)
        .expect("none");
    assert!(untouched(&p, &none, f.shot));
    // Removing keys; the last one leaves a constant at its value.
    let p = p.remove_key(target, Time::seconds(1)).expect("remove");
    let p = p.remove_key(target, Time::seconds(4)).expect("remove");
    assert_eq!(
        p.remove_key(target, Time::seconds(4)),
        Err(EditError::NoKeyAt(Time::seconds(4)))
    );
    let p = p
        .set_key(target, Time::seconds(2), float(-3.0))
        .expect("key");
    let p = p.remove_key(target, Time::seconds(2)).expect("last");
    assert_eq!(p.param_curve(target), Ok(None));
    let shot = p.find_item(f.shot).expect("shot").2.clip().expect("clip");
    assert_eq!(shot.gain_db, -3.0);
}

#[test]
fn scaling_keeps_handle_shapes() {
    let f = fixture();
    let target = clip(f.shot, ClipParam::CropRight);
    let p = f
        .project
        .animate_param(target, Time::ZERO)
        .expect("animate");
    let p = p
        .set_key(target, Time::seconds(1), float(10.0))
        .expect("key");
    let bezier = Interpolation::Bezier {
        out: Handle::new(vec![4.0], 0.5),
        into: Handle::new(vec![8.0], 0.25),
    };
    let p = p
        .set_key_interpolation(target, Time::ZERO, bezier)
        .expect("bezier");
    let both = [Time::ZERO, Time::seconds(1)];
    let slow = p
        .scale_key_times(target, &both, Time::ZERO, Rational::new(2, 1))
        .expect("stretch");
    // The same curve at half speed: speeds halve, influences stay.
    for i in 0..=10 {
        let a = number(&p, target, secs(i, 10));
        let b = number(&slow, target, secs(i, 5));
        assert!((a - b).abs() < 1e-9, "{i}: {a} vs {b}");
    }
    match &curve(&slow, target).keys[0].interpolation {
        Interpolation::Bezier { out, into } => {
            assert_eq!((out.speed.clone(), out.influence), (vec![2.0], 0.5));
            assert_eq!((into.speed.clone(), into.influence), (vec![4.0], 0.25));
        }
        other => panic!("{other:?}"),
    }
    let tall = p
        .scale_key_values(target, &both, float(0.0), 3.0)
        .expect("scale");
    for i in 0..=10 {
        let a = number(&p, target, secs(i, 10));
        let b = number(&tall, target, secs(i, 10));
        assert!((3.0 * a - b).abs() < 1e-9, "{i}: {a} vs {b}");
    }
}

#[test]
fn handles_belong_to_beziers() {
    let f = fixture();
    let target = clip(f.shot, ClipParam::Anchor);
    let p = f
        .project
        .animate_param(target, Time::ZERO)
        .expect("animate");
    let p = p
        .set_key(target, Time::seconds(1), ParamValue::Point([5.0, 5.0]))
        .expect("key");
    let handle = Handle::new(vec![1.0, 2.0], 0.4);
    assert_eq!(
        p.set_key_handle(target, Time::ZERO, HandleSide::Out, handle.clone()),
        Err(EditError::NotBezier {
            time: Time::ZERO,
            side: HandleSide::Out
        })
    );
    // The first key has no in handle.
    let eased = p
        .set_key_interpolation(target, Time::ZERO, Interpolation::ease(2))
        .expect("ease");
    assert_eq!(
        eased.set_key_handle(target, Time::ZERO, HandleSide::In, handle.clone()),
        Err(EditError::NotBezier {
            time: Time::ZERO,
            side: HandleSide::In
        })
    );
    let set = eased
        .set_key_handle(target, Time::seconds(1), HandleSide::In, handle.clone())
        .expect("in");
    let set = set
        .set_key_handle(
            target,
            Time::ZERO,
            HandleSide::Out,
            Handle::new(vec![0.0, 0.0], 9.0),
        )
        .expect("out");
    assert_eq!(
        curve(&set, target).keys[0].interpolation,
        Interpolation::Bezier {
            out: Handle::new(vec![0.0, 0.0], 1.0),
            into: handle,
        }
    );
    assert_eq!(
        eased.set_key_handle(
            target,
            Time::ZERO,
            HandleSide::Out,
            Handle::new(vec![1.0], 0.5)
        ),
        Err(EditError::HandleComponents {
            expected: 2,
            found: 1
        })
    );
    assert_eq!(
        eased.set_key_handle(
            target,
            Time::ZERO,
            HandleSide::Out,
            Handle::new(vec![f64::INFINITY, 0.0], 0.5)
        ),
        Err(EditError::NotFinite("handle speed"))
    );
    assert_eq!(
        eased.set_key_handle(
            target,
            Time::ZERO,
            HandleSide::Out,
            Handle::new(vec![0.0, 0.0], f64::NAN)
        ),
        Err(EditError::NotFinite("handle influence"))
    );
    assert_eq!(
        eased.set_key_interpolation(target, Time::ZERO, Interpolation::ease(1)),
        Err(EditError::HandleComponents {
            expected: 2,
            found: 1
        })
    );
    assert_eq!(
        eased.set_key_handle(target, Time::seconds(5), HandleSide::Out, Handle::ease(2)),
        Err(EditError::NoKeyAt(Time::seconds(5)))
    );
}

#[test]
fn edits_refuse_bad_values_and_targets() {
    let f = fixture();
    let opacity = clip(f.shot, ClipParam::Opacity);
    let p = faded(&f);
    assert_eq!(
        p.set_key(opacity, Time::seconds(1), float(f64::NAN)),
        Err(EditError::NotFinite("key value"))
    );
    assert_eq!(
        p.set_key(opacity, Time::seconds(1), ParamValue::Point([0.0, 0.0])),
        Err(EditError::KeyValueKind {
            expected: "float",
            found: "point"
        })
    );
    assert_eq!(
        p.set_key(opacity, -Time::seconds(1), float(0.5)),
        Err(EditError::KeyBeforeZero(-Time::seconds(1)))
    );
    assert_eq!(
        f.project.animate_param(opacity, -frames(1)),
        Err(EditError::KeyBeforeZero(-frames(1)))
    );
    // Out-of-range values are clamped.
    let p2 = p
        .set_key(opacity, Time::seconds(1), float(1.5))
        .expect("key");
    assert_eq!(curve(&p2, opacity).keys[0].value, float(1.0));
    // Key edits on a constant parameter.
    let rotation = clip(f.shot, ClipParam::Rotation);
    assert_eq!(
        p.set_key(rotation, Time::ZERO, ParamValue::Angle(1.0)),
        Err(EditError::NotAnimated("rotation".into()))
    );
    assert_eq!(
        p.remove_key(rotation, Time::ZERO),
        Err(EditError::NotAnimated("rotation".into()))
    );
    assert_eq!(
        p.set_key_interpolation(rotation, Time::ZERO, Interpolation::Hold),
        Err(EditError::NotAnimated("rotation".into()))
    );
    // Targets that are not clips.
    let cue = clip(f.subtitle, ClipParam::Opacity);
    assert_eq!(
        p.animate_param(cue, Time::ZERO),
        Err(EditError::NotAClip(f.subtitle))
    );
    assert_eq!(
        p.animate_param(clip(ItemId(9999), ClipParam::Opacity), Time::ZERO),
        Err(EditError::UnknownItem(ItemId(9999)))
    );
    // Setting a key to the value it has returns the same root.
    let same = p
        .set_key(opacity, Time::seconds(3), float(1.0))
        .expect("same");
    assert!(untouched(&p, &same, f.shot));
    // Every refusal left the project as it was; the edits copy only the clip's track.
    let other = clip(f.sound, ClipParam::GainDb);
    let q = p.animate_param(other, Time::ZERO).expect("gain");
    let v1 = p.find_item(f.shot).expect("shot").1;
    assert!(Arc::ptr_eq(v1, q.find_item(f.shot).expect("shot").1));
}

#[test]
fn keys_stay_on_their_source_frames_through_clip_edits() {
    let f = fixture();
    let target = clip(f.shot, ClipParam::Opacity);
    // Keys at source 2 s (the clip's first frame) and 3 s (25 frames in).
    let p = faded(&f);
    let item_start = |p: &Project| p.find_item(f.shot).expect("shot").2.range.start;
    let shot = |p: &Project| {
        p.find_item(f.shot)
            .expect("shot")
            .2
            .clip()
            .expect("c")
            .clone()
    };
    let at_timeline = |p: &Project, t: Time| {
        let c = shot(p);
        c.opacity_at(c.source_time_at(item_start(p), t).expect("source"))
    };
    assert_eq!(
        shot(&p).timeline_time_of(item_start(&p), Time::seconds(3)),
        Ok(frames(25))
    );
    assert_eq!(at_timeline(&p, frames(25)), 1.0);
    // Trimming the head by 10 frames: the keys stay on their source frames, which stay at
    // the same timeline times.
    let trimmed = p
        .trim_item(f.shot, TimeRange::new(frames(10), frames(40)))
        .expect("trim");
    assert_eq!(
        times(&trimmed, target),
        [Time::seconds(2), Time::seconds(3)]
    );
    assert_eq!(
        shot(&trimmed).timeline_time_of(item_start(&trimmed), Time::seconds(3)),
        Ok(frames(25))
    );
    assert_eq!(at_timeline(&trimmed, frames(25)), 1.0);
    assert_eq!(
        at_timeline(&trimmed, frames(20)),
        at_timeline(&p, frames(20))
    );
    // Slipping by a second: the content moves, and the keys with it.
    let slipped = p.slip_clip(f.shot, Time::seconds(1)).expect("slip");
    assert_eq!(
        times(&slipped, target),
        [Time::seconds(2), Time::seconds(3)]
    );
    assert_eq!(
        shot(&slipped).timeline_time_of(item_start(&slipped), Time::seconds(3)),
        Ok(frames(50))
    );
    // Half speed, keeping the source range: the key 1 s into the source plays 2 s in.
    for keep in [SpeedKeep::Source, SpeedKeep::Timeline] {
        let slow = p
            .set_clip_speed(f.shot, Rational::new(1, 2), keep)
            .expect("speed");
        assert_eq!(times(&slow, target), [Time::seconds(2), Time::seconds(3)]);
        assert_eq!(
            shot(&slow).timeline_time_of(item_start(&slow), Time::seconds(3)),
            Ok(Time::seconds(2))
        );
        assert_eq!(at_timeline(&slow, Time::seconds(2)), 1.0);
        assert_eq!(at_timeline(&slow, Time::seconds(1)), 0.5);
    }
    // Moving the clip on the timeline carries its keys.
    let (_, v1, _) = p.find_item(f.shot).expect("shot");
    let moved = p.move_item(f.shot, v1.id, frames(200)).expect("move");
    assert_eq!(
        shot(&moved).timeline_time_of(item_start(&moved), Time::seconds(3)),
        Ok(frames(225))
    );
    assert_eq!(
        at_timeline(&moved, frames(212)),
        at_timeline(&p, frames(12))
    );
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

fn project_json(p: &Project) -> Value {
    serde_json::from_slice(&schema::save(p)).expect("JSON")
}

fn load_json(v: &Value) -> Result<Project, LoadError> {
    schema::load(&serde_json::to_vec(v).expect("encode"))
}

#[test]
fn curves_round_trip_and_broken_curves_are_refused_on_load() {
    let f = fixture();
    let lut = effects::lut();
    let intensity = KeyTarget::Effect {
        effect: f.lut,
        schema: &lut,
        param: "intensity",
    };
    let p = faded(&f);
    let p = p.animate_param(intensity, Time::ZERO).expect("animate");
    let p = p
        .set_key_interpolation(intensity, Time::ZERO, Interpolation::ease(1))
        .expect("ease");
    let loaded = schema::load(&schema::save(&p)).expect("loads");
    assert!(loaded.content_eq(&p));
    let json = project_json(&p);
    let shot = &json["project"]["sequences"][0]["tracks"][0]["items"][0];
    assert_eq!(
        shot["keyframes"]["opacity"]["keys"][1],
        serde_json::json!({ "time": "3", "value": { "float": 1.0 }, "interpolation": "linear" })
    );
    assert!(
        shot["effects"][0]["keyframes"]["intensity"]["keys"][0]["interpolation"]["bezier"]
            .is_object()
    );

    let refused = |edit: &dyn Fn(&mut Value), expected: EditError| {
        let mut broken = json.clone();
        edit(&mut broken["project"]["sequences"][0]["tracks"][0]["items"][0]);
        match load_json(&broken) {
            Err(LoadError::Invalid(ValidationError::Track { error, .. })) => {
                assert_eq!(error, expected)
            }
            other => panic!("expected {expected:?}, got {other:?}"),
        }
    };
    refused(
        &|shot| shot["keyframes"]["opacity"]["keys"][1]["time"] = "2".into(),
        EditError::KeysOutOfOrder(Time::seconds(2)),
    );
    refused(
        &|shot| shot["keyframes"]["opacity"]["keys"] = serde_json::json!([]),
        EditError::EmptyCurve,
    );
    refused(
        &|shot| {
            shot["keyframes"]["opacity"]["keys"][1]["value"] = serde_json::json!({ "float": 2.0 })
        },
        EditError::OutOfRange {
            what: "opacity",
            value: 2.0,
            min: 0.0,
            max: 1.0,
        },
    );
    refused(
        &|shot| {
            shot["keyframes"]["opacity"]["keys"][1]["value"] = serde_json::json!({ "angle": 1.0 })
        },
        EditError::KeyValueKind {
            expected: "float",
            found: "angle",
        },
    );
    refused(
        &|shot| {
            shot["keyframes"]["opacity"]["keys"][0]["value"] =
                serde_json::json!({ "point": [0.0, 0.0] });
            shot["keyframes"]["opacity"]["keys"][1]["value"] =
                serde_json::json!({ "point": [0.0, 0.0] });
        },
        EditError::KeyValueKind {
            expected: "float",
            found: "point",
        },
    );
    refused(
        &|shot| shot["keyframes"]["opacity"]["keys"][0]["time"] = "-1".into(),
        EditError::KeyBeforeZero(-Time::seconds(1)),
    );
    refused(
        &|shot| {
            shot["effects"][0]["keyframes"]["intensity"]["keys"][0]["interpolation"]["bezier"]["out"]
                ["influence"] = 1.5.into()
        },
        EditError::OutOfRange {
            what: "handle influence",
            value: 1.5,
            min: 0.0,
            max: 1.0,
        },
    );
    refused(
        &|shot| {
            shot["effects"][0]["keyframes"]["intensity"]["keys"][0]["interpolation"]["bezier"]["in"]
                ["speed"] = serde_json::json!([0.0, 0.0])
        },
        EditError::HandleComponents {
            expected: 1,
            found: 2,
        },
    );
    refused(
        &|shot| {
            let curve = shot["effects"][0]["keyframes"]["intensity"].clone();
            shot["effects"][0]["keyframes"]["Bad Id"] = curve;
        },
        EditError::InvalidParamId("Bad Id".into()),
    );
    refused(
        &|shot| shot["effects"][0]["keyframes"]["file"] = serde_json::json!({ "keys": [{ "time": "0", "value": { "text": "a" }, "interpolation": "hold" }] }),
        EditError::KeyValueKind {
            expected: "a float, int, point, angle or colour",
            found: "text",
        },
    );
    // An unknown clip property does not parse.
    let mut unknown = json.clone();
    unknown["project"]["sequences"][0]["tracks"][0]["items"][0]["keyframes"]["skew"] =
        unknown["project"]["sequences"][0]["tracks"][0]["items"][0]["keyframes"]["opacity"].clone();
    assert!(matches!(load_json(&unknown), Err(LoadError::Model { .. })));
}
