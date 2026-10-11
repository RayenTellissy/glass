//! Every bundled transition's parameters (`render::transition`, read from
//! `assets/transitions/<id>/transition.json`) must be valid `project::params` definitions,
//! since the engine builds the inspector's schema from them with the transition's id and
//! name; and render's packing must agree with the schema's defaults.

use project::params::{ParamDef, ParamKind, ParamSchema};
use render::transition::{self, TransitionRegistry};

#[test]
fn bundled_transition_params_are_valid_schemas() {
    let registry = TransitionRegistry::bundled();
    assert!(registry.problems().is_empty(), "{:?}", registry.problems());
    assert!(!registry.is_empty());
    for t in registry.iter() {
        let params: Vec<ParamDef> = t
            .schema_params()
            .iter()
            .map(|json| {
                serde_json::from_value(json.clone())
                    .unwrap_or_else(|e| panic!("{}: {json}: {e}", t.id()))
            })
            .collect();
        let schema = ParamSchema::new(t.id(), t.name(), params);
        schema
            .validate()
            .unwrap_or_else(|e| panic!("{}: {e}", t.id()));
        assert_eq!(schema.params.len(), t.params().len());
        for (def, param) in schema.params.iter().zip(t.params()) {
            assert_eq!(def.id, param.id);
            let same_kind = matches!(
                (&def.kind, &param.kind),
                (
                    ParamKind::Float { .. } | ParamKind::Int { .. },
                    transition::ParamKind::Number
                ) | (ParamKind::Angle { .. }, transition::ParamKind::Angle)
                    | (ParamKind::Bool { .. }, transition::ParamKind::Bool)
                    | (ParamKind::Choice { .. }, transition::ParamKind::Choice(_))
                    | (ParamKind::Colour { .. }, transition::ParamKind::Colour)
                    | (ParamKind::Point { .. }, transition::ParamKind::Point)
            );
            assert!(same_kind, "{}/{}: kinds differ", t.id(), def.id);
        }
    }
}
