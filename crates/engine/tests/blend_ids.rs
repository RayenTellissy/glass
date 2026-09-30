//! The compositing section's blend choices (`project::params::builtin`) and the
//! compositor's blend modes (`render::blend`) must name the same modes with the same
//! stable ids, since the engine passes the stored choice straight to the blend node.

use project::params::builtin::BLEND_MODES;
use render::blend::BlendMode;

#[test]
fn blend_choice_ids_are_the_compositors_ids() {
    for id in BLEND_MODES {
        assert!(
            BlendMode::from_id(id).is_some(),
            "no blend mode for choice {id:?}"
        );
    }
    assert_eq!(BLEND_MODES.len(), BlendMode::ALL.len());
}
