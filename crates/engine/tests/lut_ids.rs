//! The LUT effect's input space choices (`project::params::effects`) and the LUT node's
//! input spaces (`render::lut`) must use the same stable ids, since the engine passes the
//! stored choice straight to the node.

use project::params::effects::LUT_INPUT_SPACES;
use render::lut::LutSpace;

#[test]
fn lut_input_choice_ids_are_the_nodes_ids() {
    for id in LUT_INPUT_SPACES {
        assert!(
            LutSpace::from_id(id).is_some(),
            "no LUT input space for choice {id:?}"
        );
    }
    assert_eq!(LUT_INPUT_SPACES.len(), LutSpace::ALL.len());
    assert_eq!(LUT_INPUT_SPACES[0], LutSpace::default().id());
}
