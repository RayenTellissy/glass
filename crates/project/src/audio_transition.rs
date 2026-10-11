//! Built-in audio transitions: what a transition on an audio track plays.
//!
//! Every transition on an audio track plays as a crossfade between its clips (a fade in
//! from nothing or out to nothing when one side is a gap or the track's start). Its
//! [`CROSSFADE`] kind carries one parameter, [`CURVE`], choosing the shape:
//! [`EQUAL_POWER`] (constant power, the default), [`LINEAR`] (constant gain) or
//! [`S_CURVE`] (constant gain, no slope at the ends). A transition of any other kind on an
//! audio track (a picture transition's id, such as the default `cross-dissolve`, or a kind
//! this build does not know) plays the default equal-power crossfade, so moving or
//! retyping a track never leaves a cut silent. The transition's easing bends the progress
//! the curve follows, as it does for pictures; its direction is ignored.
//!
//! The kind and its parameter live here, not in the render-side registry
//! (`assets/transitions/`), because audio transitions have no shader. They add nothing to
//! the file format: a transition already stores any kind id and parameter values.

use crate::id::ItemId;
use crate::params::{ParamDef, ParamSchema};
use crate::time::Time;
use crate::transition::{Transition, TransitionSpec};

/// The crossfade's kind id.
pub const CROSSFADE: &str = "crossfade";
/// The crossfade's curve parameter: one of [`CURVES`].
pub const CURVE: &str = "curve";
/// Constant power: `cos` out, `sin` in. Level for unrelated material.
pub const EQUAL_POWER: &str = "equal-power";
/// Constant gain: `1 − p` out, `p` in. Level for the same material on both sides.
pub const LINEAR: &str = "linear";
/// Constant gain on a raised cosine: `cos²` out, `sin²` in.
pub const S_CURVE: &str = "s-curve";
/// The curve ids in menu order; the first is the default.
pub const CURVES: [&str; 3] = [EQUAL_POWER, LINEAR, S_CURVE];

/// The crossfade's parameter schema.
pub fn crossfade() -> ParamSchema {
    ParamSchema::new(
        CROSSFADE,
        "transition.name.crossfade",
        vec![ParamDef::choice(
            CURVE,
            "transition.param.curve",
            &[
                (EQUAL_POWER, "transition.option.equal_power"),
                (LINEAR, "transition.option.linear"),
                (S_CURVE, "transition.option.s_curve"),
            ],
        )],
    )
}

/// Every built-in audio transition's schema.
pub fn all() -> Vec<ParamSchema> {
    vec![crossfade()]
}

/// A [`CROSSFADE`] transition with `curve` (one of [`CURVES`]; another id leaves the
/// default), centred, linear easing.
pub fn crossfade_spec(
    outgoing: Option<ItemId>,
    incoming: Option<ItemId>,
    duration: Time,
    curve: &str,
) -> TransitionSpec {
    let schema = crossfade();
    let mut spec = TransitionSpec::new(outgoing, incoming, duration);
    spec.kind = CROSSFADE.to_owned();
    if curve != EQUAL_POWER {
        spec.params.set(
            &schema,
            CURVE,
            crate::params::ParamValue::Choice(curve.to_owned()),
        );
    }
    spec
}

/// The curve id (one of [`CURVES`]) `transition` plays on an audio track: its [`CURVE`]
/// for a [`CROSSFADE`], else [`EQUAL_POWER`].
pub fn curve_of(transition: &Transition) -> &'static str {
    if transition.kind != CROSSFADE {
        return EQUAL_POWER;
    }
    let curve = transition.params.choice(&crossfade(), CURVE);
    CURVES
        .into_iter()
        .find(|c| curve.as_deref() == Some(*c))
        .unwrap_or(EQUAL_POWER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ParamValue;
    use crate::transition::Alignment;

    fn transition(spec: TransitionSpec) -> Transition {
        Transition {
            id: crate::id::TransitionId(1),
            outgoing: spec.outgoing,
            incoming: spec.incoming,
            kind: spec.kind,
            duration: spec.duration,
            alignment: Alignment::Centre,
            direction: None,
            easing: spec.easing,
            params: spec.params,
        }
    }

    #[test]
    fn the_schema_is_valid_and_curves_read_back() {
        crossfade().validate().unwrap();
        let d = Time::seconds(1);
        for curve in CURVES {
            let t = transition(crossfade_spec(Some(ItemId(1)), None, d, curve));
            assert_eq!(t.kind, CROSSFADE);
            assert_eq!(curve_of(&t), curve);
        }
        // Equal power is stored as the default (no value).
        let t = transition(crossfade_spec(None, Some(ItemId(1)), d, EQUAL_POWER));
        assert!(t.params.is_default(&crossfade(), CURVE));
        // Unknown curve ids and other kinds play equal power.
        let mut t = transition(crossfade_spec(None, Some(ItemId(1)), d, LINEAR));
        t.kind = "cross-dissolve".to_owned();
        assert_eq!(curve_of(&t), EQUAL_POWER);
        let mut t = transition(crossfade_spec(None, Some(ItemId(1)), d, "wobble"));
        assert_eq!(curve_of(&t), EQUAL_POWER);
        t.params =
            serde_json::from_value(serde_json::json!({ "curve": { "choice": "wobble" } })).unwrap();
        assert_eq!(
            t.params.get(&crossfade(), CURVE),
            Some(ParamValue::Choice(EQUAL_POWER.to_owned()))
        );
        assert_eq!(curve_of(&t), EQUAL_POWER);
    }
}
