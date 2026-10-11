//! Contract tests for keyframe curves (schema v7, Decision 63), written against the
//! contract rather than the implementation: values at and around keys, beziers against an
//! independent de Casteljau reference, per-component values, readers clamping to the
//! range, keys staying in source time through chains of clip edits, the order of refusals,
//! the constant kept by `make_param_constant` and `remove_key`, exact time scaling, value
//! scaling, no-op sharing, undo and redo of effect parameter edits, hand-built curves
//! refused on load, and the v6 migration.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use project::params::{ParamDef, ParamSchema, ParamValue, Range};
use project::schema::{self, LoadError, SCHEMA_VERSION};
use project::{
    Clip, ClipParam, ClipSpec, Curve, EditError, EditLabel, Effect, EffectId, Handle, HandleSide,
    History, Interpolation, ItemId, Key, KeyTarget, Project, Rational, SpeedKeep, Time, TimeRange,
    TrackId, TrackKind, ValidationError,
};
use serde_json::{Value, json};

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn secs(num: i64, den: i64) -> Time {
    Time::new(num, den).expect("time")
}

fn plus(a: Time, b: Time) -> Time {
    a.checked_add(b).expect("add")
}

fn minus(a: Time, b: Time) -> Time {
    a.checked_sub(b).expect("sub")
}

fn float(v: f64) -> ParamValue {
    ParamValue::Float(v)
}

/// An effect with a parameter of every animatable kind but the angle, and a boolean.
fn tint() -> ParamSchema {
    ParamSchema::new(
        "acme.tint",
        "acme.tint",
        vec![
            ParamDef::colour("colour", "acme.tint.colour", [0.0, 0.0, 0.0, 1.0], true),
            ParamDef::int("steps", "acme.tint.steps", 0, Range::new(-10.0, 10.0)),
            ParamDef::float("strength", "acme.tint.strength", 0.5, Range::new(0.0, 1.0)),
            ParamDef::boolean("invert", "acme.tint.invert", false),
        ],
    )
}

struct Fixture {
    project: Project,
    v1: TrackId,
    /// V1: frames 0..50, showing source from 2 s; nothing else on V1.
    shot: ItemId,
    subtitle: ItemId,
    /// A [`tint`] on `shot`.
    tint: EffectId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-keys-contract", "Keys contract");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
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
    let (p, subtitle) = p
        .insert_subtitle(s1, TimeRange::new(Time::ZERO, frames(10)), "Hi")
        .expect("cue");
    let (p, tint) = p.add_effect(shot, &tint(), None).expect("tint");
    Fixture {
        project: p,
        v1,
        shot,
        subtitle,
        tint,
    }
}

fn on_clip(item: ItemId, param: ClipParam) -> KeyTarget<'static> {
    KeyTarget::Clip { item, param }
}

fn on_effect<'a>(effect: EffectId, schema: &'a ParamSchema, param: &'a str) -> KeyTarget<'a> {
    KeyTarget::Effect {
        effect,
        schema,
        param,
    }
}

fn clip_of(p: &Project, item: ItemId) -> &Clip {
    p.find_item(item).expect("item").2.clip().expect("clip")
}

fn effect_of(p: &Project, effect: EffectId) -> &Effect {
    p.find_effect(effect).expect("effect").1
}

fn curve_of(p: &Project, target: KeyTarget<'_>) -> Arc<Curve> {
    p.param_curve(target).expect("target").expect("animated")
}

fn key_times(p: &Project, target: KeyTarget<'_>) -> Vec<Time> {
    curve_of(p, target).keys.iter().map(|k| k.time).collect()
}

fn number(p: &Project, target: KeyTarget<'_>, time: Time) -> f64 {
    p.param_value_at(target, time)
        .expect("value")
        .as_f64()
        .expect("number")
}

fn key(time: Time, value: ParamValue, interpolation: Interpolation) -> Key {
    Key {
        time,
        value,
        interpolation,
    }
}

/// A bezier from (speeds, influence) of the out handle and of the in handle.
fn bezier(out: (Vec<f64>, f64), into: (Vec<f64>, f64)) -> Interpolation {
    Interpolation::Bezier {
        out: Handle::new(out.0, out.1),
        into: Handle::new(into.0, into.1),
    }
}

fn valid(keys: Vec<Key>) -> Curve {
    let curve = Curve { keys };
    curve.check().expect("a valid curve");
    curve
}

fn at(curve: &Curve, time: Time) -> f64 {
    curve
        .value_at(time)
        .and_then(|v| v.as_f64())
        .expect("number")
}

/// One bezier segment as the contract states it, for an independent reference.
#[derive(Clone, Copy, Debug)]
struct Segment {
    t0: f64,
    v0: f64,
    t1: f64,
    v1: f64,
    /// (speed, influence) of the first key's out handle.
    out: (f64, f64),
    /// (speed, influence) of the second key's in handle.
    into: (f64, f64),
}

fn casteljau(p: [f64; 4], s: f64) -> f64 {
    let lerp = |a: f64, b: f64| a + (b - a) * s;
    let q = [lerp(p[0], p[1]), lerp(p[1], p[2]), lerp(p[2], p[3])];
    lerp(lerp(q[0], q[1]), lerp(q[1], q[2]))
}

impl Segment {
    fn controls(&self) -> ([f64; 4], [f64; 4]) {
        let d = self.t1 - self.t0;
        let (so, io) = self.out;
        let (si, ii) = self.into;
        (
            [self.t0, self.t0 + io * d, self.t1 - ii * d, self.t1],
            [
                self.v0,
                self.v0 + so * io * d,
                self.v1 - si * ii * d,
                self.v1,
            ],
        )
    }

    /// The value at time `t`: de Casteljau at the curve parameter whose time is `t`,
    /// found by bisection (time only moves forward along the curve while influences stay
    /// in 0..1).
    /// The curve parameter whose time is `t`, by bisection (time only moves forward
    /// along the curve while influences stay in 0..1).
    fn parameter(&self, t: f64) -> f64 {
        let (xs, _) = self.controls();
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..200 {
            let mid = 0.5 * (lo + hi);
            if casteljau(xs, mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }

    /// The value at time `t`, by de Casteljau's construction.
    fn at(&self, t: f64) -> f64 {
        casteljau(self.controls().1, self.parameter(t))
    }

    /// Whether `got` is a value the reference curve takes within a billionth of the
    /// segment of time `t`, give or take a billionth of the value scale. (Where the curve
    /// is nearly vertical in time, any solver's tolerance in time moves the value a long
    /// way.)
    fn admits(&self, t: f64, got: f64) -> bool {
        let d = self.t1 - self.t0;
        let (from, to) = (self.parameter(t - 1e-9 * d), self.parameter(t + 1e-9 * d));
        let ys = self.controls().1;
        let scale = ys.iter().fold(1.0_f64, |m, v| m.max(v.abs()));
        let slack = 1e-9 * scale;
        let (lo, hi) = (0..=1000)
            .map(|i| casteljau(ys, from + (to - from) * f64::from(i) / 1000.0))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            });
        (lo - slack..=hi + slack).contains(&got)
    }
}

#[test]
fn values_are_exact_at_keys_flat_outside_them_and_hold_switches_at_the_next_key() {
    let eps = secs(1, 1_000_000_000_000);
    let (a, b, c, d) = (secs(1, 3), secs(2, 3), secs(7, 6), secs(5, 2));
    let curve = valid(vec![
        key(a, float(10.0), Interpolation::Linear),
        key(b, float(40.0), Interpolation::Hold),
        key(c, float(-20.0), Interpolation::Linear),
        key(d, float(0.0), Interpolation::Linear),
    ]);
    for (time, v) in [(a, 10.0), (b, 40.0), (c, -20.0), (d, 0.0)] {
        assert_eq!(curve.value_at(time), Some(float(v)), "at {time}");
    }
    // Before the first key and after the last, however close or far.
    for time in [Time::ZERO, minus(a, eps)] {
        assert_eq!(curve.value_at(time), Some(float(10.0)), "before, {time}");
    }
    for time in [plus(d, eps), Time::seconds(1_000_000_000)] {
        assert_eq!(curve.value_at(time), Some(float(0.0)), "after, {time}");
    }
    // Just inside a linear segment the value has started to move, by slope × ε.
    let after_a = at(&curve, plus(a, eps));
    assert!(after_a > 10.0 && after_a - 10.0 < 1e-9, "{after_a}");
    let before_b = at(&curve, minus(b, eps));
    assert!(before_b < 40.0 && 40.0 - before_b < 1e-9, "{before_b}");
    // Hold: the key's value up to the next key, which takes over exactly at its time.
    assert_eq!(curve.value_at(plus(b, eps)), Some(float(40.0)));
    assert_eq!(curve.value_at(secs(11, 12)), Some(float(40.0)));
    assert_eq!(curve.value_at(minus(c, eps)), Some(float(40.0)));
    assert_eq!(curve.value_at(c), Some(float(-20.0)));
    let after_c = at(&curve, plus(c, eps));
    assert!(after_c > -20.0 && after_c + 20.0 < 1e-9, "{after_c}");
    // A single key is its value everywhere.
    let single = valid(vec![key(a, float(3.0), Interpolation::Hold)]);
    for time in [Time::ZERO, a, Time::seconds(9)] {
        assert_eq!(single.value_at(time), Some(float(3.0)));
    }
    // Keys are exact under a bezier too, for every component.
    let eased = valid(vec![
        key(
            a,
            ParamValue::Colour([0.25, 1.0, 0.0, 0.5]),
            bezier(
                (vec![7.0, -3.0, 2.0, 9.0], 0.9),
                (vec![-5.0, 4.0, 1.0, 0.0], 0.6),
            ),
        ),
        key(
            d,
            ParamValue::Colour([0.75, 0.0, 2.0, 1.0]),
            Interpolation::Linear,
        ),
    ]);
    assert_eq!(
        eased.value_at(a),
        Some(ParamValue::Colour([0.25, 1.0, 0.0, 0.5]))
    );
    assert_eq!(
        eased.value_at(d),
        Some(ParamValue::Colour([0.75, 0.0, 2.0, 1.0]))
    );
}

/// (out speed, out influence, in speed, in influence) for keys -4 at 1/3 s and 20 at
/// 13/6 s (11/6 s apart).
const HANDLES: [(f64, f64, f64, f64); 11] = [
    (0.0, 0.0, 0.0, 0.0),
    (50.0, 0.0, -50.0, 0.0),
    (0.0, 1.0, 0.0, 1.0),
    (30.0, 1.0, -30.0, 1.0),
    (5.0, 0.25, 40.0, 0.75),
    (0.0, 0.0, 0.0, 1.0),
    (0.0, 1.0, 0.0, 0.0),
    (144.0 / 11.0, 1.0 / 3.0, 144.0 / 11.0, 1.0 / 3.0),
    (1e6, 1.0, -1e6, 1.0),
    (-1e6, 0.5, 1e6, 0.1),
    (1e6, 0.0, 1e6, 1.0),
];

#[test]
fn bezier_segments_match_an_independent_cubic_for_every_handle_setting() {
    let (t0, t1) = (secs(1, 3), secs(13, 6));
    let mut samples: Vec<Time> = (40..=260).map(|k| secs(k, 120)).collect();
    // Around the middle, where full influences make the curve vertical in time.
    for k in [-1, 1, -1000, 1000] {
        samples.push(secs(5_000_000_000_000 + k, 4_000_000_000_000));
    }
    for (so, io, si, ii) in HANDLES {
        let curve = valid(vec![
            key(t0, float(-4.0), bezier((vec![so], io), (vec![si], ii))),
            key(t1, float(20.0), Interpolation::Linear),
        ]);
        let segment = Segment {
            t0: t0.as_secs_f64(),
            v0: -4.0,
            t1: t1.as_secs_f64(),
            v1: 20.0,
            out: (so, io),
            into: (si, ii),
        };
        assert_eq!(curve.value_at(t0), Some(float(-4.0)), "{segment:?}");
        assert_eq!(curve.value_at(t1), Some(float(20.0)), "{segment:?}");
        for &time in &samples {
            let got = at(&curve, time);
            let t = time.as_secs_f64();
            assert!(
                segment.admits(t, got),
                "{segment:?} at {time}: {got} vs {}",
                segment.at(t)
            );
            // Zero influence puts the control points on the keys: a straight line,
            // whatever the speeds.
            if io == 0.0 && ii == 0.0 {
                let line = -4.0 + 24.0 * (t - segment.t0) / (segment.t1 - segment.t0);
                assert!((got - line).abs() < 1e-9 * 24.0, "{segment:?} at {time}");
            }
        }
    }
}

#[test]
fn flat_handles_only_rise_whatever_influences_are_asked_for() {
    // Influences outside 0..1 are clamped when set, so time never runs backwards and a
    // flat-handled rise never dips or overshoots.
    let f = fixture();
    let target = on_clip(f.shot, ClipParam::CropLeft);
    let p = f
        .project
        .animate_param(target, Time::ZERO)
        .expect("animate");
    let p = p.set_key(target, secs(3, 2), float(100.0)).expect("key");
    let influences = [-1.0, 0.0, 0.25, 0.5, 1.0, 4.0];
    for io in influences {
        for ii in influences {
            let shaped = p
                .set_key_interpolation(target, Time::ZERO, bezier((vec![0.0], io), (vec![0.0], ii)))
                .expect("bezier");
            let mut last = 0.0;
            for k in 0..=300 {
                let v = at(&curve_of(&shaped, target), secs(k, 200));
                assert!(
                    (last..=100.0).contains(&v),
                    "influences {io}, {ii} at {k}/200: {v} after {last}"
                );
                last = v;
            }
            assert_eq!(last, 100.0);
        }
    }
}

#[test]
fn points_and_colours_follow_one_curve_per_component() {
    let (t0, t1) = (Time::ZERO, Time::seconds(2));
    let point = valid(vec![
        key(
            t0,
            ParamValue::Point([0.0, 10.0]),
            bezier((vec![12.0, -40.0], 0.6), (vec![-5.0, 25.0], 0.2)),
        ),
        key(t1, ParamValue::Point([8.0, -6.0]), Interpolation::Linear),
    ]);
    let axes = [
        Segment {
            t0: 0.0,
            v0: 0.0,
            t1: 2.0,
            v1: 8.0,
            out: (12.0, 0.6),
            into: (-5.0, 0.2),
        },
        Segment {
            t0: 0.0,
            v0: 10.0,
            t1: 2.0,
            v1: -6.0,
            out: (-40.0, 0.6),
            into: (25.0, 0.2),
        },
    ];
    for k in 1..40 {
        let time = secs(k, 20);
        let Some(ParamValue::Point(got)) = point.value_at(time) else {
            panic!("a point");
        };
        for (axis, segment) in axes.iter().enumerate() {
            assert!(
                segment.admits(time.as_secs_f64(), got[axis]),
                "axis {axis} at {time}: {} vs {}",
                got[axis],
                segment.at(time.as_secs_f64())
            );
        }
    }

    // Linear colours move each channel, alpha included, on its own.
    let linear = valid(vec![
        key(
            t0,
            ParamValue::Colour([0.0, 1.0, 0.25, 1.0]),
            Interpolation::Linear,
        ),
        key(
            Time::seconds(1),
            ParamValue::Colour([1.0, 0.0, 0.75, 0.5]),
            Interpolation::Linear,
        ),
    ]);
    assert_eq!(
        linear.value_at(secs(1, 4)),
        Some(ParamValue::Colour([0.25, 0.75, 0.375, 0.875]))
    );
    // A bezier colour: each channel its own speed through the shared influences.
    let (from, to) = ([0.0, 1.0, 0.25, 1.0], [1.0, 0.0, 0.75, 0.5]);
    let (out, into) = ([2.0, -3.0, 0.5, 0.0], [0.0, 6.0, -1.0, 4.0]);
    let colour = valid(vec![
        key(
            t0,
            ParamValue::Colour(from),
            bezier((out.to_vec(), 0.3), (into.to_vec(), 0.7)),
        ),
        key(t1, ParamValue::Colour(to), Interpolation::Linear),
    ]);
    for k in 1..40 {
        let time = secs(k, 20);
        let Some(ParamValue::Colour(got)) = colour.value_at(time) else {
            panic!("a colour");
        };
        for channel in 0..4 {
            let segment = Segment {
                t0: 0.0,
                v0: f64::from(from[channel]),
                t1: 2.0,
                v1: f64::from(to[channel]),
                out: (out[channel], 0.3),
                into: (into[channel], 0.7),
            };
            let want = segment.at(time.as_secs_f64());
            let got = f64::from(got[channel]);
            assert!(
                (got - want).abs() < 1e-6,
                "channel {channel} at {time}: {got} vs {want}"
            );
        }
    }
}

#[test]
fn angles_interpolate_as_plain_numbers_without_wrapping() {
    let line = |a: f64, b: f64| {
        valid(vec![
            key(Time::ZERO, ParamValue::Angle(a), Interpolation::Linear),
            key(
                Time::seconds(4),
                ParamValue::Angle(b),
                Interpolation::Linear,
            ),
        ])
    };
    let angle = |c: &Curve, time: Time| c.value_at(time).expect("value");
    // 350° to 10° turns back through 180°, not forward through 0°.
    assert_eq!(
        angle(&line(350.0, 10.0), Time::seconds(2)),
        ParamValue::Angle(180.0)
    );
    let twice = line(0.0, 720.0);
    assert_eq!(angle(&twice, Time::seconds(1)), ParamValue::Angle(180.0));
    assert_eq!(angle(&twice, Time::seconds(3)), ParamValue::Angle(540.0));
    assert_eq!(angle(&twice, Time::seconds(9)), ParamValue::Angle(720.0));
    assert_eq!(
        angle(&line(-360.0, 360.0), Time::seconds(2)),
        ParamValue::Angle(0.0)
    );
}

#[test]
fn interpolated_ints_round_and_halves_round_the_same_way_for_either_sign() {
    let line = |a: i64, b: i64| {
        valid(vec![
            key(Time::ZERO, ParamValue::Int(a), Interpolation::Linear),
            key(Time::seconds(1), ParamValue::Int(b), Interpolation::Linear),
        ])
    };
    let int = |c: &Curve, time: Time| c.value_at(time).expect("value");
    assert_eq!(int(&line(0, 1), secs(49, 100)), ParamValue::Int(0));
    assert_eq!(int(&line(0, 1), secs(1, 2)), ParamValue::Int(1));
    assert_eq!(int(&line(0, 1), secs(51, 100)), ParamValue::Int(1));
    // Not to even: 2.5 is 3.
    assert_eq!(int(&line(2, 3), secs(1, 2)), ParamValue::Int(3));
    // Negating the keys negates the value: -0.5 is -1, -2.5 is -3.
    assert_eq!(int(&line(0, -1), secs(1, 2)), ParamValue::Int(-1));
    assert_eq!(int(&line(-2, -3), secs(1, 2)), ParamValue::Int(-3));
    assert_eq!(int(&line(0, -1), secs(49, 100)), ParamValue::Int(0));

    // Through an effect parameter's reader as well.
    let f = fixture();
    let schema = tint();
    let steps = on_effect(f.tint, &schema, "steps");
    let p = f.project.animate_param(steps, Time::ZERO).expect("animate");
    let p = p
        .set_key(steps, Time::seconds(2), ParamValue::Int(-3))
        .expect("key");
    // -2.25 and -1.5 (at 3/2 s and 1 s).
    assert_eq!(p.param_value_at(steps, secs(3, 2)), Ok(ParamValue::Int(-2)));
    assert_eq!(
        p.param_value_at(steps, Time::seconds(1)),
        Ok(ParamValue::Int(-2))
    );
}

#[test]
fn readers_clamp_each_component_of_an_overshoot_to_the_range() {
    let f = fixture();
    let (k0, k1, mid) = (Time::seconds(2), Time::seconds(3), secs(5, 2));
    // Scale's x is held at the top of its range by keys there, and steep handles push it
    // past; y stays inside.
    let scale = on_clip(f.shot, ClipParam::Scale);
    let p = f.project.animate_param(scale, k0).expect("animate");
    let p = p
        .set_key(scale, k0, ParamValue::Point([100.0, 0.0]))
        .expect("key");
    let p = p
        .set_key(scale, k1, ParamValue::Point([100.0, 50.0]))
        .expect("key");
    let p = p
        .set_key_interpolation(
            scale,
            k0,
            bezier((vec![500.0, 0.0], 1.0), (vec![-500.0, 0.0], 1.0)),
        )
        .expect("bezier");
    let Some(ParamValue::Point(raw)) = curve_of(&p, scale).value_at(mid) else {
        panic!("a point");
    };
    assert!(raw[0] > 100.0 && (0.0..50.0).contains(&raw[1]), "{raw:?}");
    let shot = clip_of(&p, f.shot);
    assert_eq!(
        shot.param_at(ClipParam::Scale, mid),
        ParamValue::Point([100.0, raw[1]])
    );
    assert_eq!(shot.transform_at(mid).scale, [100.0, raw[1]]);
    assert_eq!(
        p.param_value_at(scale, mid),
        Ok(ParamValue::Point([100.0, raw[1]]))
    );

    // Opacity undershoots after its first key and overshoots before its last.
    let opacity = on_clip(f.shot, ClipParam::Opacity);
    let p = p.animate_param(opacity, k0).expect("animate");
    let p = p.set_key(opacity, k0, float(0.0)).expect("key");
    let p = p.set_key(opacity, k1, float(1.0)).expect("key");
    let p = p
        .set_key_interpolation(opacity, k0, bezier((vec![-10.0], 0.5), (vec![-10.0], 0.5)))
        .expect("bezier");
    let (early, late) = (secs(21, 10), secs(29, 10));
    let raw = curve_of(&p, opacity);
    assert!(at(&raw, early) < 0.0 && at(&raw, late) > 1.0);
    let shot = clip_of(&p, f.shot);
    assert_eq!(shot.opacity_at(early), 0.0);
    assert_eq!(shot.opacity_at(late), 1.0);

    // An effect colour: channels at 0 or more, alpha within 0..1, each on its own.
    let schema = tint();
    let colour = on_effect(f.tint, &schema, "colour");
    let p = p.animate_param(colour, k0).expect("animate");
    let p = p
        .set_key(colour, k0, ParamValue::Colour([0.0, 0.5, 0.5, 0.0]))
        .expect("key");
    let p = p
        .set_key(colour, k1, ParamValue::Colour([1.0, 0.5, 0.5, 1.0]))
        .expect("key");
    let p = p
        .set_key_interpolation(
            colour,
            k0,
            bezier(
                (vec![-20.0, 0.0, 0.0, 0.0], 0.5),
                (vec![0.0, 0.0, 0.0, -20.0], 0.5),
            ),
        )
        .expect("bezier");
    let raw = curve_of(&p, colour);
    let effect = effect_of(&p, f.tint);
    let (Some(ParamValue::Colour(raw_early)), Some(ParamValue::Colour(raw_late))) =
        (raw.value_at(early), raw.value_at(late))
    else {
        panic!("colours");
    };
    assert!(raw_early[0] < 0.0 && raw_late[3] > 1.0);
    assert_eq!(
        effect.param_at(&schema, "colour", early),
        Some(ParamValue::Colour([
            0.0,
            raw_early[1],
            raw_early[2],
            raw_early[3]
        ]))
    );
    assert_eq!(
        effect.param_at(&schema, "colour", late),
        Some(ParamValue::Colour([
            raw_late[0],
            raw_late[1],
            raw_late[2],
            1.0
        ]))
    );
}

#[test]
fn keys_stay_on_their_source_frames_through_a_chain_of_clip_edits() {
    let f = fixture();
    let target = on_clip(f.shot, ClipParam::Opacity);
    // Opacity 0 at source 2 s (the clip's first frame) to 1 at source 3 s.
    let p = f
        .project
        .animate_param(target, Time::seconds(2))
        .expect("animate");
    let p = p
        .set_key(target, Time::seconds(2), float(0.0))
        .expect("key");
    let p = p
        .set_key(target, Time::seconds(3), float(1.0))
        .expect("key");
    let keys = [Time::seconds(2), Time::seconds(3)];
    let start = |p: &Project| p.find_item(f.shot).expect("shot").2.range.start;
    let plays_at = |p: &Project, source: Time| {
        clip_of(p, f.shot)
            .timeline_time_of(start(p), source)
            .expect("timeline time")
    };
    let opacity_at = |p: &Project, time: Time| {
        let clip = clip_of(p, f.shot);
        clip.opacity_at(clip.source_time_at(start(p), time).expect("source"))
    };

    // Two thirds speed over the same timeline range: source 3 s plays (3 − 2) × 3/2 in.
    let p = p
        .set_clip_speed(f.shot, Rational::new(2, 3), SpeedKeep::Timeline)
        .expect("speed");
    assert_eq!(key_times(&p, target), keys);
    assert_eq!(plays_at(&p, Time::seconds(3)), secs(3, 2));
    assert_eq!(opacity_at(&p, secs(3, 2)), 1.0);
    assert_eq!(opacity_at(&p, secs(3, 4)), 0.5);

    // Trimming 10 frames (2/5 s) off the head moves the source start 4/15 s on: the key at
    // 2 s is now before the visible range and kept, and still shapes the first frame.
    let p = p
        .trim_item(f.shot, TimeRange::new(frames(10), frames(40)))
        .expect("trim");
    assert_eq!(clip_of(&p, f.shot).source_start, secs(34, 15));
    assert_eq!(key_times(&p, target), keys);
    assert_eq!(plays_at(&p, Time::seconds(3)), secs(3, 2));
    assert!((opacity_at(&p, frames(10)) - 4.0 / 15.0).abs() < 1e-12);

    // Slipping to source 1 s: the key at 3 s now plays past the clip's end, kept.
    let p = p.slip_clip(f.shot, Time::seconds(1)).expect("slip");
    assert_eq!(key_times(&p, target), keys);
    assert_eq!(plays_at(&p, Time::seconds(3)), secs(17, 5));
    assert_eq!(opacity_at(&p, secs(2, 5)), 0.0);
    // The last frame (1.96 s) shows source 1 + 1.56 × 2/3 = 2.04 s.
    let last_frame = opacity_at(&p, frames(49));
    assert!((last_frame - 0.04).abs() < 1e-12, "{last_frame}");

    // Moving the clip to 4 s carries everything with it.
    let p = p.move_item(f.shot, f.v1, frames(100)).expect("move");
    assert_eq!(key_times(&p, target), keys);
    assert_eq!(plays_at(&p, Time::seconds(3)), Time::seconds(7));
    assert_eq!(opacity_at(&p, frames(139)), last_frame);
    assert!(p.validate().is_ok());
}

#[test]
fn arguments_come_before_the_target_and_the_parameter_before_its_curve() {
    let f = fixture();
    let p = &f.project;
    let schema = tint();
    let early = -frames(1);
    let zero = Rational::new(0, 1);
    // Arguments that are wrong on their own win over a target that does not resolve.
    let targets = [
        on_clip(ItemId(9999), ClipParam::Opacity),
        on_clip(f.subtitle, ClipParam::Opacity),
        on_effect(EffectId(9999), &schema, "strength"),
        on_effect(f.tint, &schema, "nope"),
        on_effect(f.tint, &schema, "invert"),
    ];
    for target in targets {
        assert_eq!(
            p.animate_param(target, early),
            Err(EditError::KeyBeforeZero(early))
        );
        assert_eq!(
            p.set_key(target, early, float(f64::NAN)),
            Err(EditError::KeyBeforeZero(early))
        );
        assert_eq!(
            p.move_key(target, Time::seconds(9), early),
            Err(EditError::KeyBeforeZero(early))
        );
        assert_eq!(
            p.scale_key_times(target, &[Time::seconds(9)], Time::ZERO, zero),
            Err(EditError::TimeScale(zero))
        );
        assert_eq!(
            p.scale_key_values(target, &[Time::seconds(9)], float(0.0), f64::INFINITY),
            Err(EditError::NotFinite("value scale"))
        );
    }
    // The target before anything about the value.
    assert_eq!(
        p.set_key(
            on_clip(f.subtitle, ClipParam::Opacity),
            Time::ZERO,
            float(f64::NAN)
        ),
        Err(EditError::NotAClip(f.subtitle))
    );
    // Whether the parameter can animate before its value or its curve.
    let invert = on_effect(f.tint, &schema, "invert");
    let not_animatable = Err(EditError::NotAnimatable("acme.tint/invert".into()));
    assert_eq!(
        p.set_key(invert, Time::ZERO, float(f64::NAN)),
        not_animatable
    );
    assert_eq!(
        p.scale_key_values(invert, &[Time::seconds(9)], float(0.0), 2.0),
        not_animatable
    );
    assert_eq!(
        p.set_key_interpolation(invert, Time::seconds(9), Interpolation::ease(3)),
        not_animatable
    );
    // A value of the parameter's kind and finite before whether it is animated.
    let rotation = on_clip(f.shot, ClipParam::Rotation);
    assert_eq!(
        p.set_key(rotation, Time::ZERO, float(1.0)),
        Err(EditError::KeyValueKind {
            expected: "angle",
            found: "float"
        })
    );
    assert_eq!(
        p.set_key(rotation, Time::ZERO, ParamValue::Angle(f64::NAN)),
        Err(EditError::NotFinite("key value"))
    );
    assert_eq!(
        p.scale_key_values(rotation, &[Time::seconds(9)], float(0.0), 2.0),
        Err(EditError::KeyValueKind {
            expected: "angle",
            found: "float"
        })
    );
    // Whether it is animated before which keys it has.
    let not_animated = Err(EditError::NotAnimated("rotation".into()));
    let nine = Time::seconds(9);
    assert_eq!(p.remove_key(rotation, nine), not_animated);
    assert_eq!(p.move_key(rotation, nine, Time::ZERO), not_animated);
    assert_eq!(
        p.set_key_interpolation(rotation, nine, Interpolation::Hold),
        not_animated
    );
    assert_eq!(
        p.set_key_handle(rotation, nine, HandleSide::Out, Handle::ease(1)),
        not_animated
    );
    assert_eq!(
        p.scale_key_times(rotation, &[nine], Time::ZERO, Rational::new(2, 1)),
        not_animated
    );
    assert_eq!(
        p.scale_key_values(rotation, &[nine], ParamValue::Angle(0.0), 2.0),
        not_animated
    );
    assert_eq!(p.make_param_constant(rotation, nine), not_animated);
    // Which keys there are before where they would go.
    let opacity = on_clip(f.shot, ClipParam::Opacity);
    let keyed = [1, 2, 4].into_iter().fold(
        p.animate_param(opacity, Time::seconds(1)).expect("animate"),
        |p, s| {
            p.set_key(opacity, Time::seconds(s), float(0.5))
                .expect("key")
        },
    );
    assert_eq!(
        keyed.move_key(opacity, nine, Time::seconds(2)),
        Err(EditError::NoKeyAt(nine))
    );
    assert_eq!(
        keyed.set_key_handle(opacity, nine, HandleSide::In, Handle::ease(1)),
        Err(EditError::NoKeyAt(nine))
    );
    // 1 s would land before zero, but 9 s is no key.
    assert_eq!(
        keyed.scale_key_times(
            opacity,
            &[Time::seconds(1), nine],
            Time::seconds(4),
            Rational::new(2, 1)
        ),
        Err(EditError::NoKeyAt(nine))
    );
    // About 7/2 s by 3/5, 1 s lands on the key at 2 s and 4 s at 19/5 s.
    assert_eq!(
        keyed.scale_key_times(
            opacity,
            &[Time::seconds(1), Time::seconds(4)],
            secs(7, 2),
            Rational::new(3, 5)
        ),
        Err(EditError::KeyTimeTaken(Time::seconds(2)))
    );
    // Both at once: keys at 1 and 3 s about 4 s by 2 land at -2 s and on the key at 2 s.
    let keyed = keyed
        .set_key(opacity, Time::seconds(3), float(0.5))
        .expect("key");
    assert_eq!(
        keyed.scale_key_times(
            opacity,
            &[Time::seconds(3), Time::seconds(1)],
            Time::seconds(4),
            Rational::new(2, 1)
        ),
        Err(EditError::KeyBeforeZero(-Time::seconds(2)))
    );
}

#[test]
fn setting_a_key_replaces_only_its_value_and_new_keys_are_linear() {
    let f = fixture();
    let target = on_clip(f.shot, ClipParam::CropBottom);
    let p = f
        .project
        .animate_param(target, Time::seconds(2))
        .expect("animate");
    let p = p
        .set_key(target, Time::seconds(4), float(80.0))
        .expect("key");
    let p = p
        .set_key_interpolation(target, Time::seconds(2), Interpolation::ease(1))
        .expect("ease");
    let p = p
        .set_key_interpolation(target, Time::seconds(4), Interpolation::Hold)
        .expect("hold");
    let p = p
        .set_key(target, Time::seconds(2), float(20.0))
        .expect("replace");
    let p = p
        .set_key(target, Time::seconds(4), float(60.0))
        .expect("replace");
    let p = p
        .set_key(target, Time::seconds(3), float(10.0))
        .expect("insert");
    let curve = curve_of(&p, target);
    assert_eq!(
        curve.keys,
        vec![
            key(Time::seconds(2), float(20.0), Interpolation::ease(1)),
            key(Time::seconds(3), float(10.0), Interpolation::Linear),
            key(Time::seconds(4), float(60.0), Interpolation::Hold),
        ]
    );
}

#[test]
fn removing_the_last_key_leaves_its_value_as_the_constant() {
    let f = fixture();
    let schema = tint();
    let colour = on_effect(f.tint, &schema, "colour");
    let value = [0.5, 0.25, 2.0, 0.75];
    let p = f
        .project
        .animate_param(colour, Time::ZERO)
        .expect("animate");
    let p = p
        .set_key(colour, Time::seconds(1), ParamValue::Colour(value))
        .expect("key");
    let p = p.remove_key(colour, Time::ZERO).expect("remove");
    assert_eq!(curve_of(&p, colour).keys.len(), 1);
    let p = p.remove_key(colour, Time::seconds(1)).expect("remove last");
    assert_eq!(p.param_curve(colour), Ok(None));
    let effect = effect_of(&p, f.tint);
    assert_eq!(effect.params.colour(&schema, "colour"), Some(value));
    assert_eq!(
        p.param_value_at(colour, Time::seconds(7)),
        Ok(ParamValue::Colour(value))
    );
    assert_eq!(
        p.remove_key(colour, Time::seconds(1)),
        Err(EditError::NotAnimated("acme.tint/colour".into()))
    );

    // A clip point: the constant the key replaced is gone.
    let position = on_clip(f.shot, ClipParam::Position);
    let p = p
        .animate_param(position, Time::seconds(5))
        .expect("animate");
    let p = p
        .set_key(position, Time::seconds(5), ParamValue::Point([-30.0, 12.5]))
        .expect("key");
    let p = p.remove_key(position, Time::seconds(5)).expect("remove");
    assert_eq!(p.param_curve(position), Ok(None));
    assert_eq!(clip_of(&p, f.shot).transform.position, [-30.0, 12.5]);
}

#[test]
fn making_a_parameter_constant_keeps_its_value_at_the_given_time() {
    let f = fixture();
    let target = on_clip(f.shot, ClipParam::CropLeft);
    let p = f
        .project
        .animate_param(target, Time::seconds(1))
        .expect("animate");
    let p = p
        .set_key(target, Time::seconds(1), float(40.0))
        .expect("key");
    let p = p
        .set_key(target, Time::seconds(2), float(100.0))
        .expect("key");
    let p = p
        .set_key(target, Time::seconds(3), float(70.0))
        .expect("key");
    let p = p
        .set_key_interpolation(
            target,
            Time::seconds(1),
            bezier((vec![300.0], 0.5), (vec![0.0], 1.0 / 3.0)),
        )
        .expect("bezier");
    let p = p
        .set_key_interpolation(target, Time::seconds(2), Interpolation::Hold)
        .expect("hold");
    let on_bezier = Segment {
        t0: 1.0,
        v0: 40.0,
        t1: 2.0,
        v1: 100.0,
        out: (300.0, 0.5),
        into: (0.0, 1.0 / 3.0),
    };
    let constant_at = |at: Time| {
        let q = p.make_param_constant(target, at).expect("constant");
        assert_eq!(q.param_curve(target), Ok(None));
        clip_of(&q, f.shot).crop.left
    };
    assert_eq!(constant_at(Time::ZERO), 40.0, "before the first key");
    assert_eq!(constant_at(Time::seconds(2)), 100.0, "at a key");
    assert_eq!(constant_at(secs(2999, 1000)), 100.0, "held");
    assert_eq!(constant_at(Time::seconds(3)), 70.0, "at the last key");
    assert_eq!(constant_at(Time::seconds(60)), 70.0, "after the last key");
    let t = secs(13, 10);
    let kept = constant_at(t);
    assert_eq!(kept, number(&p, target, t));
    assert!(
        on_bezier.admits(1.3, kept),
        "{kept} vs {}",
        on_bezier.at(1.3)
    );
    assert!(kept > 70.0, "{kept}: a bezier value, not a line's");

    // A value the curve overshoots to is clamped into the range.
    let steep = p
        .set_key_interpolation(
            target,
            Time::seconds(1),
            bezier((vec![-2000.0], 1.0), (vec![0.0], 1.0 / 3.0)),
        )
        .expect("steep");
    assert!(at(&curve_of(&steep, target), secs(11, 10)) < 0.0);
    let q = steep
        .make_param_constant(target, secs(11, 10))
        .expect("constant");
    assert_eq!(clip_of(&q, f.shot).crop.left, 0.0);

    // An effect parameter's constant is written into its values.
    let schema = tint();
    let strength = on_effect(f.tint, &schema, "strength");
    let p = p.animate_param(strength, Time::ZERO).expect("animate");
    let p = p.set_key(strength, Time::ZERO, float(0.2)).expect("key");
    let p = p
        .set_key(strength, Time::seconds(2), float(0.8))
        .expect("key");
    let q = p
        .make_param_constant(strength, secs(1, 2))
        .expect("constant");
    let kept = effect_of(&q, f.tint)
        .params
        .float(&schema, "strength")
        .expect("strength");
    assert!((kept - 0.35).abs() < 1e-12, "{kept}");
}

#[test]
fn time_scaling_is_exact_and_divides_the_scaled_keys_handle_speeds() {
    let f = fixture();
    let target = on_clip(f.shot, ClipParam::CropTop);
    let times = [secs(1, 3), Time::seconds(1), secs(5, 2), Time::seconds(4)];
    let values = [10.0, 20.0, 40.0, 0.0];
    // (out speed, out influence, in speed, in influence) of each key's interpolation.
    let handles = [
        (3.0, 0.2, 6.0, 0.3),
        (-9.0, 0.4, 12.0, 0.5),
        (15.0, 0.6, -18.0, 0.7),
        (21.0, 0.8, 24.0, 0.9),
    ];
    let mut p = f.project.animate_param(target, times[0]).expect("animate");
    for ((&time, &v), &(so, io, si, ii)) in times.iter().zip(&values).zip(&handles) {
        p = p.set_key(target, time, float(v)).expect("key");
        p = p
            .set_key_interpolation(target, time, bezier((vec![so], io), (vec![si], ii)))
            .expect("bezier");
    }
    // The keys at 1 s and 5/2 s about 1/7 s by 2/3: 1/7 + 6/7 × 2/3 = 5/7 and
    // 1/7 + 33/14 × 2/3 = 12/7. Their out handles and the in handles arriving at them
    // (held by the key before) speed up by 3/2; the others stay.
    let expected_times = [secs(1, 3), secs(5, 7), secs(12, 7), Time::seconds(4)];
    let expected_handles = [
        (3.0, 0.2, 9.0, 0.3),
        (-13.5, 0.4, 18.0, 0.5),
        (22.5, 0.6, -18.0, 0.7),
        (21.0, 0.8, 24.0, 0.9),
    ];
    for factor in [
        Rational::new(2, 3),
        Rational::new(-2, -3),
        Rational::new(4, 6),
    ] {
        let q = p
            .scale_key_times(target, &[times[1], times[2]], secs(1, 7), factor)
            .expect("scale");
        let curve = curve_of(&q, target);
        assert_eq!(key_times(&q, target), expected_times, "{factor}");
        for (i, k) in curve.keys.iter().enumerate() {
            assert_eq!(k.value, float(values[i]));
            let (so, io, si, ii) = expected_handles[i];
            let Interpolation::Bezier { out, into } = &k.interpolation else {
                panic!("{factor}: key {i} is no longer a bezier");
            };
            assert!((out.speed[0] - so).abs() < 1e-12, "{factor} {i}: {out:?}");
            assert!((into.speed[0] - si).abs() < 1e-12, "{factor} {i}: {into:?}");
            assert_eq!((out.influence, into.influence), (io, ii), "{factor} {i}");
        }
        assert!(q.validate().is_ok());
    }
    // A key scaled past others is re-sorted with its value.
    let q = p
        .scale_key_times(target, &[times[3]], Time::ZERO, Rational::new(1, 24))
        .expect("scale");
    assert_eq!(
        key_times(&q, target),
        [secs(1, 6), secs(1, 3), Time::seconds(1), secs(5, 2)]
    );
    let moved: Vec<_> = curve_of(&q, target)
        .keys
        .iter()
        .map(|k| k.value.clone())
        .collect();
    assert_eq!(moved, [float(0.0), float(10.0), float(20.0), float(40.0)]);
}

#[test]
fn value_scaling_works_per_component_clamps_and_multiplies_speeds() {
    let f = fixture();
    let scale = on_clip(f.shot, ClipParam::Scale);
    let mut p = f.project.animate_param(scale, Time::ZERO).expect("animate");
    for (s, v) in [(0, [10.0, 50.0]), (1, [20.0, -60.0]), (2, [30.0, 30.0])] {
        p = p
            .set_key(scale, Time::seconds(s), ParamValue::Point(v))
            .expect("key");
        p = p
            .set_key_interpolation(
                scale,
                Time::seconds(s),
                bezier((vec![1.0, 2.0], 0.5), (vec![3.0, 4.0], 0.5)),
            )
            .expect("bezier");
    }
    let summary = |p: &Project| -> Vec<(ParamValue, Interpolation)> {
        curve_of(p, scale)
            .keys
            .iter()
            .map(|k| (k.value.clone(), k.interpolation.clone()))
            .collect()
    };
    // About (0, 10) by 3: y leaves the ±100 range both ways and is clamped.
    let q = p
        .scale_key_values(
            scale,
            &[Time::ZERO, Time::seconds(1)],
            ParamValue::Point([0.0, 10.0]),
            3.0,
        )
        .expect("scale");
    assert_eq!(
        summary(&q),
        [
            (
                ParamValue::Point([30.0, 100.0]),
                bezier((vec![3.0, 6.0], 0.5), (vec![9.0, 12.0], 0.5))
            ),
            (
                ParamValue::Point([60.0, -100.0]),
                bezier((vec![3.0, 6.0], 0.5), (vec![3.0, 4.0], 0.5))
            ),
            (
                ParamValue::Point([30.0, 30.0]),
                bezier((vec![1.0, 2.0], 0.5), (vec![3.0, 4.0], 0.5))
            ),
        ]
    );
    // A negative factor mirrors about the anchor, speeds included.
    let q = p
        .scale_key_values(
            scale,
            &[Time::seconds(1)],
            ParamValue::Point([20.0, 0.0]),
            -1.0,
        )
        .expect("mirror");
    assert_eq!(
        summary(&q),
        [
            (
                ParamValue::Point([10.0, 50.0]),
                bezier((vec![1.0, 2.0], 0.5), (vec![-3.0, -4.0], 0.5))
            ),
            (
                ParamValue::Point([20.0, 60.0]),
                bezier((vec![-1.0, -2.0], 0.5), (vec![3.0, 4.0], 0.5))
            ),
            (
                ParamValue::Point([30.0, 30.0]),
                bezier((vec![1.0, 2.0], 0.5), (vec![3.0, 4.0], 0.5))
            ),
        ]
    );

    // Colours per channel: negative channels clamp to 0, alpha to 1.
    let schema = tint();
    let colour = on_effect(f.tint, &schema, "colour");
    let p = f
        .project
        .animate_param(colour, Time::ZERO)
        .expect("animate");
    let p = p
        .set_key(colour, Time::ZERO, ParamValue::Colour([0.2, 0.4, 0.6, 0.9]))
        .expect("key");
    let q = p
        .scale_key_values(colour, &[Time::ZERO], ParamValue::Colour([0.5; 4]), 4.0)
        .expect("scale");
    let ParamValue::Colour(c) = curve_of(&q, colour).keys[0].value else {
        panic!("a colour");
    };
    assert_eq!((c[0], c[3]), (0.0, 1.0), "{c:?}");
    assert!(
        (c[1] - 0.1).abs() < 1e-6 && (c[2] - 0.9).abs() < 1e-6,
        "{c:?}"
    );

    // Ints: 1 + 2 × 4 = 9 and 1 − 3 × 4 = −11, clamped to −10.
    let steps = on_effect(f.tint, &schema, "steps");
    let p = q.animate_param(steps, Time::ZERO).expect("animate");
    let p = p
        .set_key(steps, Time::ZERO, ParamValue::Int(3))
        .expect("key");
    let p = p
        .set_key(steps, Time::seconds(1), ParamValue::Int(-2))
        .expect("key");
    let q = p
        .scale_key_values(
            steps,
            &[Time::ZERO, Time::seconds(1)],
            ParamValue::Int(1),
            4.0,
        )
        .expect("scale");
    let ints: Vec<_> = curve_of(&q, steps)
        .keys
        .iter()
        .map(|k| k.value.clone())
        .collect();
    assert_eq!(ints, [ParamValue::Int(9), ParamValue::Int(-10)]);
}

#[test]
fn edits_that_change_nothing_return_the_same_root() {
    let f = fixture();
    let opacity = on_clip(f.shot, ClipParam::Opacity);
    let schema = tint();
    let strength = on_effect(f.tint, &schema, "strength");
    let (k0, k1) = (Time::seconds(2), Time::seconds(3));
    let shaped = bezier((vec![1.0], 1.0), (vec![2.0], 0.0));
    let p = f.project.animate_param(opacity, k0).expect("animate");
    let p = p.set_key(opacity, k0, float(0.0)).expect("key");
    let p = p.set_key(opacity, k1, float(1.0)).expect("key");
    let p = p
        .set_key_interpolation(opacity, k0, shaped.clone())
        .expect("bezier");
    let base = p.animate_param(strength, Time::ZERO).expect("animate");
    let cases = [
        ("the value it has", base.set_key(opacity, k0, float(0.0))),
        (
            "a value clamped to the one it has",
            base.set_key(opacity, k1, float(7.0)),
        ),
        (
            "the interpolation it has",
            base.set_key_interpolation(opacity, k0, shaped.clone()),
        ),
        (
            "influences clamped to the ones it has",
            base.set_key_interpolation(opacity, k0, bezier((vec![1.0], 5.0), (vec![2.0], -3.0))),
        ),
        (
            "linear again",
            base.set_key_interpolation(opacity, k1, Interpolation::Linear),
        ),
        (
            "the out handle it has",
            base.set_key_handle(opacity, k0, HandleSide::Out, Handle::new(vec![1.0], 1.0)),
        ),
        (
            "an in handle clamped to the one it has",
            base.set_key_handle(opacity, k1, HandleSide::In, Handle::new(vec![2.0], -1.0)),
        ),
        ("a move onto itself", base.move_key(opacity, k0, k0)),
        (
            "times by one",
            base.scale_key_times(opacity, &[k0, k1], secs(5, 2), Rational::new(1, 1)),
        ),
        (
            "times by an unreduced one",
            base.scale_key_times(opacity, &[k0, k1], Time::ZERO, Rational::new(3, 3)),
        ),
        (
            "no keys stretched",
            base.scale_key_times(opacity, &[], Time::ZERO, Rational::new(2, 1)),
        ),
        (
            "values by one",
            base.scale_key_values(opacity, &[k0, k1], float(0.5), 1.0),
        ),
        (
            "an effect key set to its value",
            base.set_key(strength, Time::ZERO, float(0.5)),
        ),
    ];
    for (name, result) in cases {
        let q = result.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        // The same root: equal, and the clip's item still the very node (any edit that
        // went through would have copied it).
        assert!(q.content_eq(&base), "{name}");
        assert!(
            Arc::ptr_eq(
                q.find_item(f.shot).expect("shot").2,
                base.find_item(f.shot).expect("shot").2
            ),
            "{name}"
        );
    }
}

#[test]
fn undo_and_redo_step_through_every_edit_of_an_effect_parameter() {
    let f = fixture();
    let schema = tint();
    let target = on_effect(f.tint, &schema, "strength");
    let probe = Time::seconds(1);
    // Each step: label, root, whether animated, value at 1 s, stored constant.
    let mut steps: Vec<(EditLabel, Project, bool, f64, f64)> = Vec::new();
    let mut step = |label: EditLabel,
                    next: Result<Project, EditError>,
                    animated: bool,
                    value: f64,
                    constant: f64| {
        let root = next.expect("edit");
        steps.push((label, root.clone(), animated, value, constant));
        root
    };
    let q = step(
        EditLabel::Animate,
        f.project.animate_param(target, Time::ZERO),
        true,
        0.5,
        0.5,
    );
    let q = step(
        EditLabel::AddKeyframe,
        q.set_key(target, Time::seconds(2), float(1.0)),
        true,
        0.75,
        0.5,
    );
    let q = step(
        EditLabel::ChangeKeyframe,
        q.set_key(target, Time::seconds(2), float(0.9)),
        true,
        0.7,
        0.5,
    );
    let q = step(
        EditLabel::KeyframeInterpolation,
        q.set_key_interpolation(target, Time::ZERO, Interpolation::Hold),
        true,
        0.5,
        0.5,
    );
    // Handles at the segment's slope (0.2 per second) keep it a straight line.
    let q = step(
        EditLabel::KeyframeInterpolation,
        q.set_key_interpolation(
            target,
            Time::ZERO,
            bezier((vec![0.2], 1.0 / 3.0), (vec![0.2], 1.0 / 3.0)),
        ),
        true,
        0.7,
        0.5,
    );
    let q = step(
        EditLabel::KeyframeHandles,
        q.set_key_handle(
            target,
            Time::seconds(2),
            HandleSide::In,
            Handle::new(vec![0.2], 0.0),
        ),
        true,
        0.7,
        0.5,
    );
    // Twice as long, half the speed: still the line, now 0.1 per second.
    let q = step(
        EditLabel::StretchKeyframes,
        q.scale_key_times(
            target,
            &[Time::ZERO, Time::seconds(2)],
            Time::ZERO,
            Rational::new(2, 1),
        ),
        true,
        0.6,
        0.5,
    );
    // Values halved about 0.5 (0.5 and 0.7), speeds halved: the line at 0.05 per second.
    let q = step(
        EditLabel::ScaleKeyframeValues,
        q.scale_key_values(target, &[Time::ZERO, Time::seconds(4)], float(0.5), 0.5),
        true,
        0.55,
        0.5,
    );
    let moved = Segment {
        t0: 0.0,
        v0: 0.5,
        t1: 2.0,
        v1: 0.7,
        out: (0.05, 1.0 / 3.0),
        into: (0.05, 0.0),
    }
    .at(1.0);
    let q = step(
        EditLabel::MoveKeyframe,
        q.move_key(target, Time::seconds(4), Time::seconds(2)),
        true,
        moved,
        0.5,
    );
    let q = step(
        EditLabel::DeleteKeyframe,
        q.remove_key(target, Time::ZERO),
        true,
        0.7,
        0.5,
    );
    step(
        EditLabel::StopAnimating,
        q.make_param_constant(target, Time::ZERO),
        false,
        0.7,
        0.7,
    );

    let check = |p: &Project, animated: bool, value: f64, constant: f64, what: &str| {
        assert_eq!(
            p.param_curve(target).expect("target").is_some(),
            animated,
            "{what}"
        );
        let got = number(p, target, probe);
        assert!((got - value).abs() < 1e-9, "{what}: {got} vs {value}");
        let stored = effect_of(p, f.tint)
            .params
            .float(&schema, "strength")
            .expect("strength");
        assert!(
            (stored - constant).abs() < 1e-12,
            "{what}: constant {stored}"
        );
    };
    let mut h = History::new(f.project.clone());
    for (label, root, animated, value, constant) in &steps {
        check(root, *animated, *value, *constant, &format!("{label:?}"));
        h.commit(*label, root.clone());
    }
    for i in (0..steps.len()).rev() {
        assert_eq!(h.undo_label(), Some(steps[i].0));
        let p = h.undo().expect("undo").clone();
        match i.checked_sub(1).map(|j| &steps[j]) {
            Some((label, _, animated, value, constant)) => check(
                &p,
                *animated,
                *value,
                *constant,
                &format!("undo to {label:?}"),
            ),
            None => check(&p, false, 0.5, 0.5, "undo to the start"),
        }
    }
    assert!(!h.can_undo());
    for (label, _, animated, value, constant) in &steps {
        assert_eq!(h.redo_label(), Some(*label));
        let p = h.redo().expect("redo").clone();
        check(&p, *animated, *value, *constant, &format!("redo {label:?}"));
    }
    assert!(!h.can_redo());
}

fn linear_key(time: &str, value: Value) -> Value {
    json!({ "time": time, "value": value, "interpolation": "linear" })
}

fn bezier_key(time: &str, value: Value, out: (Value, f64), into: (Value, f64)) -> Value {
    json!({
        "time": time,
        "value": value,
        "interpolation": { "bezier": {
            "out": { "speed": out.0, "influence": out.1 },
            "in": { "speed": into.0, "influence": into.1 }
        } }
    })
}

/// The fixture saved, with `shot`'s keyframes and its tint's keyframes replaced.
fn with_curves(f: &Fixture, clip: Value, effect: Value) -> Result<Project, LoadError> {
    let mut json: Value = serde_json::from_slice(&schema::save(&f.project)).expect("JSON");
    assert_eq!(json["schema_version"], json!(SCHEMA_VERSION));
    let shot = &mut json["project"]["sequences"][0]["tracks"][0]["items"][0];
    assert_eq!(shot["id"], json!(f.shot.0));
    shot["keyframes"] = clip;
    shot["effects"][0]["keyframes"] = effect;
    schema::load(&serde_json::to_vec(&json).expect("encode"))
}

#[test]
fn hand_built_curves_breaking_the_contract_are_refused_inside_their_track() {
    let f = fixture();
    let none = json!({});
    let f0 = |v: f64| json!({ "float": v });
    let cases = [
        (
            json!({ "opacity": { "keys": [linear_key("3", f0(1.0)), linear_key("2", f0(0.5))] } }),
            none.clone(),
            EditError::KeysOutOfOrder(Time::seconds(2)),
        ),
        (
            none.clone(),
            json!({ "strength": { "keys": [] } }),
            EditError::EmptyCurve,
        ),
        (
            none.clone(),
            json!({ "strength": { "keys": [linear_key("-1/25", f0(0.5))] } }),
            EditError::KeyBeforeZero(-frames(1)),
        ),
        (
            json!({ "opacity": { "keys": [linear_key("0", f0(0.5)), linear_key("1", json!({ "int": 1 }))] } }),
            none.clone(),
            EditError::KeyValueKind {
                expected: "float",
                found: "int",
            },
        ),
        (
            json!({ "opacity": { "keys": [linear_key("0", json!({ "int": 1 }))] } }),
            none.clone(),
            EditError::KeyValueKind {
                expected: "float",
                found: "int",
            },
        ),
        (
            json!({ "rotation": { "keys": [linear_key("0", json!({ "bool": true }))] } }),
            none.clone(),
            EditError::KeyValueKind {
                expected: "a float, int, point, angle or colour",
                found: "bool",
            },
        ),
        (
            json!({ "scale": { "keys": [linear_key("0", json!({ "point": [50.0, 100.5] }))] } }),
            none.clone(),
            EditError::OutOfRange {
                what: "scale",
                value: 100.5,
                min: -100.0,
                max: 100.0,
            },
        ),
        (
            json!({ "crop.feather": { "keys": [linear_key("0", f0(0.0)), linear_key("1", f0(1000.5))] } }),
            none.clone(),
            EditError::OutOfRange {
                what: "crop.feather",
                value: 1000.5,
                min: 0.0,
                max: 1000.0,
            },
        ),
        (
            json!({ "opacity": { "keys": [
                bezier_key("0", f0(0.0), (json!([0.0]), 0.5), (json!([0.0]), -0.25)),
                linear_key("1", f0(1.0)),
            ] } }),
            none.clone(),
            EditError::OutOfRange {
                what: "handle influence",
                value: -0.25,
                min: 0.0,
                max: 1.0,
            },
        ),
        (
            json!({ "scale": { "keys": [
                bezier_key("0", json!({ "point": [1.0, 1.0] }), (json!([1.0]), 0.5), (json!([0.0, 0.0]), 0.5)),
            ] } }),
            none.clone(),
            EditError::HandleComponents {
                expected: 2,
                found: 1,
            },
        ),
        (
            none.clone(),
            json!({ "": { "keys": [linear_key("0", f0(0.5))] } }),
            EditError::InvalidParamId(String::new()),
        ),
        (
            none.clone(),
            json!({ "Strength": { "keys": [linear_key("0", f0(0.5))] } }),
            EditError::InvalidParamId("Strength".into()),
        ),
    ];
    for (clip, effect, expected) in cases {
        match with_curves(&f, clip, effect) {
            Err(LoadError::Invalid(ValidationError::Track { track, error })) => {
                assert_eq!((track, error), (f.v1, expected));
            }
            other => panic!("expected {expected:?}, got {other:?}"),
        }
    }

    // At the limits: influences of exactly 0 and 1, values at the ends of their ranges.
    let loaded = with_curves(
        &f,
        json!({
            "opacity": { "keys": [
                bezier_key("0", f0(0.0), (json!([5.0]), 0.0), (json!([5.0]), 1.0)),
                linear_key("1", f0(1.0)),
            ] },
            "scale": { "keys": [linear_key("0", json!({ "point": [100.0, -100.0] }))] },
            "crop.feather": { "keys": [linear_key("0", f0(1000.0))] },
        }),
        json!({ "strength": { "keys": [
            bezier_key("1/3", f0(0.0), (json!([0.0]), 1.0), (json!([0.0]), 0.0)),
            linear_key("2/3", f0(1.0)),
        ] } }),
    )
    .expect("loads");
    let shot = clip_of(&loaded, f.shot);
    assert_eq!(
        shot.curve(ClipParam::Opacity).expect("opacity").keys[0].interpolation,
        bezier((vec![5.0], 0.0), (vec![5.0], 1.0))
    );
    assert_eq!(
        shot.param_at(ClipParam::Scale, Time::ZERO),
        ParamValue::Point([100.0, -100.0])
    );
    assert_eq!(shot.crop_at(Time::ZERO).feather, 1000.0);
    let schema = tint();
    assert_eq!(
        effect_of(&loaded, f.tint).param_at(&schema, "strength", secs(2, 3)),
        Some(float(1.0))
    );
}

fn fixture_file(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/projects")
        .join(name);
    serde_json::from_slice(&fs::read(&path).expect("fixture")).expect("JSON")
}

#[test]
fn the_v6_migration_gives_clips_and_their_effects_empty_keyframes_and_nothing_else() {
    let v6 = json!({
        "next_id": 12,
        "sequences": [{
            "id": 2,
            "tracks": [
                { "id": 3, "items": [
                    { "id": 4, "type": "clip", "media": 1, "effects": [
                        { "id": 5, "schema": "lut" },
                        { "id": 6, "schema": "acme.tint" }
                    ] },
                    { "id": 7, "type": "clip", "media": 1 },
                    { "id": 8, "type": "subtitle", "text": "Hi" },
                    { "id": 9, "type": "adjustment" }
                ] },
                { "id": 10, "items": [] },
                { "id": 11 }
            ]
        }]
    });
    let mut v7 = v6.clone();
    let items = &mut v7["sequences"][0]["tracks"][0]["items"];
    items[0]["keyframes"] = json!({});
    items[0]["effects"][0]["keyframes"] = json!({});
    items[0]["effects"][1]["keyframes"] = json!({});
    items[1]["keyframes"] = json!({});
    assert_eq!(schema::migrate_v6_to_v7(v6), Ok(v7));
    let bare = json!({ "next_id": 1 });
    assert_eq!(schema::migrate_v6_to_v7(bare.clone()), Ok(bare));
}

#[test]
fn a_v6_clip_or_effect_already_holding_keyframes_is_refused_by_the_migration() {
    let v6 = fixture_file("v6-props.json");
    assert_eq!(v6["schema_version"], json!(6));
    assert!(schema::load(&serde_json::to_vec(&v6).expect("JSON")).is_ok());
    let refused = |pointer: &str| {
        let mut json = v6.clone();
        json.pointer_mut(pointer)
            .and_then(Value::as_object_mut)
            .expect("an object")
            .insert("keyframes".into(), json!({}));
        match schema::load(&serde_json::to_vec(&json).expect("JSON")) {
            Err(LoadError::Migration { from, message }) => (from, message),
            other => panic!("{pointer}: {other:?}"),
        }
    };
    let clip = "/project/sequences/0/tracks/0/items/0";
    assert_eq!(
        refused(clip),
        (6, "a v6 clip already has keyframes".to_string())
    );
    assert_eq!(
        refused(&format!("{clip}/effects/1")),
        (6, "a v6 effect already has keyframes".to_string())
    );
}
