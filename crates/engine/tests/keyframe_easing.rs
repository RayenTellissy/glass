//! Keyframe beziers (`project::keyframe`) and transition easing (`render::transition`)
//! solve the same CSS cubic-bezier, each in its own crate since `project` may not depend
//! on `render`. The two copies must give the same numbers, bit for bit.

use project::keyframe::cubic_bezier;
use render::transition::Easing;

#[test]
fn keyframe_beziers_solve_like_transition_easing() {
    let curves = [
        (0.42, 0.0, 1.0, 1.0),
        (0.0, 0.0, 0.58, 1.0),
        (0.42, 0.0, 0.58, 1.0),
        (0.25, 0.1, 0.25, 1.0),
        (1.0, 0.0, 0.0, 1.0),
        (0.0, 1.0, 1.0, 0.0),
        (0.68, -0.55, 0.265, 1.55),
        (1.4, 0.3, -0.2, 0.9),
        (1.0 / 3.0, 0.0, 2.0 / 3.0, 1.0),
    ];
    for (x1, y1, x2, y2) in curves {
        let easing = Easing::CubicBezier { x1, y1, x2, y2 };
        for i in -10..=1010 {
            let x = f64::from(i) / 1000.0;
            assert_eq!(
                cubic_bezier(x1, y1, x2, y2, x).to_bits(),
                easing.apply(x).to_bits(),
                "cubic-bezier({x1}, {y1}, {x2}, {y2}) at {x}"
            );
        }
        assert_eq!(
            cubic_bezier(x1, y1, x2, y2, f64::NAN),
            easing.apply(f64::NAN)
        );
    }
}
