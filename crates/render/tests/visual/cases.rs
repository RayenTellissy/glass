//! The reference sequences. Each case is a scene as a function of the frame index; the
//! harness renders every listed frame and compares it with
//! `tests/visual/goldens/<name>/frame_<index>.png`. Keep scenes at [`CANVAS`] and to a
//! few frames, so the goldens stay small.

use render::blend::BlendMode;
use render::crop::Crop;
use render::mask::{Mask, MaskMode};
use render::nodes::Layer;
use render::shape::{
    Affine, Cap, Dash, FillRule, Geometry, Join, Paint, Path, Point, Rect, Shape, Stop, Stroke,
};
use render::text::{GlyphTransform, Outline, Shadow, TextPaint};
use render::transform::{Resample, Transform2D};
use render::transition::{Direction, ParamValue, TransitionNode};

use super::{CANVAS, Case, Scene, Source, TextSpec};

/// Every registered case. New compositor features add theirs here.
pub fn all() -> Vec<Case> {
    vec![
        Case {
            name: "blend_modes",
            frames: &[0, 24],
            scene: blend_modes,
        },
        Case {
            name: "transform_crop_opacity",
            frames: &[0, 12, 24],
            scene: transform_crop_opacity,
        },
        Case {
            name: "title_over_video",
            frames: &[0, 12, 24],
            scene: title_over_video,
        },
        Case {
            name: "shapes",
            frames: &[0, 12, 24],
            scene: shapes,
        },
        Case {
            name: "subtitle_burn_in",
            frames: &[0, 1],
            scene: subtitle_burn_in,
        },
        Case {
            name: "mask_hard",
            frames: &[0],
            scene: mask_hard,
        },
        Case {
            name: "mask_feathered",
            frames: &[0],
            scene: mask_feathered,
        },
        Case {
            name: "mask_inverted",
            frames: &[0],
            scene: mask_inverted,
        },
        Case {
            name: "mask_union",
            frames: &[12],
            scene: mask_union,
        },
        Case {
            name: "mask_intersection",
            frames: &[12],
            scene: mask_intersection,
        },
        Case {
            name: "cross_dissolve",
            frames: &[0, 12, 24],
            scene: cross_dissolve,
        },
        Case {
            name: "dip_to_black",
            frames: &[6, 12, 18],
            scene: dip_to_black,
        },
        Case {
            name: "dip_to_white",
            frames: &[6, 12, 18],
            scene: dip_to_white,
        },
        Case {
            name: "dip_to_colour",
            frames: &[6, 12, 18],
            scene: dip_to_colour,
        },
        Case {
            name: "film_dissolve",
            frames: &[6, 12, 18],
            scene: film_dissolve,
        },
        Case {
            name: "additive_dissolve",
            frames: &[6, 12, 18],
            scene: additive_dissolve,
        },
        Case {
            name: "non_additive_dissolve",
            frames: &[6, 12, 18],
            scene: non_additive_dissolve,
        },
        Case {
            name: "linear_wipe",
            frames: &[6, 12, 18],
            scene: linear_wipe,
        },
        Case {
            name: "linear_wipe_soft_border",
            frames: &[6, 12, 18],
            scene: linear_wipe_soft_border,
        },
        Case {
            name: "radial_wipe",
            frames: &[6, 12, 18],
            scene: radial_wipe,
        },
        Case {
            name: "clock_wipe",
            frames: &[6, 12, 18],
            scene: clock_wipe,
        },
        Case {
            name: "iris",
            frames: &[6, 12, 18],
            scene: iris,
        },
        Case {
            name: "iris_star_close",
            frames: &[6, 12, 18],
            scene: iris_star_close,
        },
        Case {
            name: "iris_diamond",
            frames: &[12],
            scene: iris_diamond,
        },
        Case {
            name: "iris_rectangle",
            frames: &[12],
            scene: iris_rectangle,
        },
        Case {
            name: "barn_door",
            frames: &[6, 12, 18],
            scene: barn_door,
        },
        Case {
            name: "venetian_blinds",
            frames: &[6, 12, 18],
            scene: venetian_blinds,
        },
        Case {
            name: "checker_wipe",
            frames: &[6, 12, 18],
            scene: checker_wipe,
        },
        Case {
            name: "push",
            frames: &[6, 12, 18],
            scene: push,
        },
        Case {
            name: "slide",
            frames: &[6, 12, 18],
            scene: slide,
        },
        Case {
            name: "slide_out",
            frames: &[12],
            scene: slide_out,
        },
        Case {
            name: "whip_pan",
            frames: &[6, 12, 18],
            scene: whip_pan,
        },
        Case {
            name: "zoom",
            frames: &[6, 12, 18],
            scene: zoom,
        },
        Case {
            name: "zoom_out",
            frames: &[12],
            scene: zoom_out,
        },
        Case {
            name: "spin",
            frames: &[6, 12, 18],
            scene: spin,
        },
        Case {
            name: "swirl",
            frames: &[6, 12, 18],
            scene: swirl,
        },
        Case {
            name: "roll",
            frames: &[6, 12, 18],
            scene: roll,
        },
        Case {
            name: "glitch",
            frames: &[6, 12, 18],
            scene: glitch,
        },
        Case {
            name: "rgb_split",
            frames: &[6, 12, 18],
            scene: rgb_split,
        },
        Case {
            name: "pixelate",
            frames: &[6, 12, 18],
            scene: pixelate,
        },
        Case {
            name: "vhs",
            frames: &[6, 12, 18],
            scene: vhs,
        },
        Case {
            name: "light_leak",
            frames: &[6, 12, 18],
            scene: light_leak,
        },
        Case {
            name: "film_burn",
            frames: &[6, 12, 18],
            scene: film_burn,
        },
        Case {
            name: "ink_bleed",
            frames: &[6, 12, 18],
            scene: ink_bleed,
        },
        Case {
            name: "paper_tear",
            frames: &[6, 12, 18],
            scene: paper_tear,
        },
        Case {
            name: "liquid",
            frames: &[6, 12, 18],
            scene: liquid,
        },
    ]
}

fn placed(position: [f64; 2], scale: [f64; 2], rotation: f64, anchor: [f64; 2]) -> Transform2D {
    Transform2D {
        position,
        scale,
        rotation,
        anchor,
    }
}

/// Every blend mode: one disc each over the gradient, in two rows (Normal, Multiply,
/// Screen, Overlay, Add, Subtract on top), drifting right by half a pixel a frame so the
/// bicubic filter samples between texels.
fn blend_modes(frame: u32) -> Scene {
    let mut layers = vec![(Source::Gradient, Layer::default())];
    let drift = f64::from(frame) * 0.5;
    for (i, mode) in BlendMode::ALL.into_iter().enumerate() {
        let (row, column) = (i / 6, i % 6);
        let x = 30.0 + 52.0 * column as f64 + 26.0 * row as f64 + drift;
        let y = 50.0 + 80.0 * row as f64;
        layers.push((
            Source::Disc,
            Layer::blended(mode).with_transform(placed([x, y], [0.75, 0.75], 0.0, [32.0, 32.0])),
        ));
    }
    Scene { layers }
}

/// Decoded video rotated, scaled and cropped with a feather, a second copy fading in at
/// half size, and a stretched disc turning the other way.
fn transform_crop_opacity(frame: u32) -> Scene {
    let t = f64::from(frame);
    Scene {
        layers: vec![
            (
                Source::Fill {
                    colour: [0.02, 0.02, 0.025, 1.0],
                    size: CANVAS,
                },
                Layer::default(),
            ),
            (
                Source::Bars,
                Layer::default()
                    .with_transform(placed([130.0, 85.0], [1.2, 1.2], 2.5 * t, [80.0, 45.0]))
                    .with_crop(Crop {
                        left: 10.0,
                        top: 5.0,
                        right: 20.0,
                        bottom: 0.0,
                        feather: 6.0,
                    }),
            ),
            (
                Source::Bars,
                Layer::blended(BlendMode::Screen)
                    .with_transform(placed([200.0, 110.0], [0.5, 0.5], 0.0, [0.0, 0.0]))
                    .with_opacity(0.3 + t as f32 / 40.0),
            ),
            (
                Source::Bars,
                Layer::default()
                    .with_transform(placed([8.0, 130.0], [0.5, 0.5], 0.0, [0.0, 0.0]))
                    .with_resample(Resample::Bilinear)
                    .with_crop(Crop {
                        right: 80.0,
                        ..Crop::default()
                    }),
            ),
            (
                Source::Disc,
                Layer::default()
                    .with_transform(placed([265.0, 50.0], [1.5, 0.75], -3.0 * t, [32.0, 32.0]))
                    .with_opacity(0.85),
            ),
        ],
    }
}

/// A lower-third title over full-frame video: the bars scaled up to fill the canvas, a
/// translucent band, and a title with an outline and a soft shadow fading in, its glyphs
/// rising one after another.
fn title_over_video(frame: u32) -> Scene {
    let t = frame as f32;
    let rise = move |index: usize| (6.0 - (t - index as f32)).clamp(0.0, 6.0);
    Scene {
        layers: vec![
            (
                Source::Bars,
                Layer::default().with_transform(placed([0.0, 0.0], [2.0, 2.0], 0.0, [0.0, 0.0])),
            ),
            (
                Source::Fill {
                    colour: [0.0, 0.0, 0.0, 0.55],
                    size: (CANVAS.0, 46),
                },
                Layer::default().with_transform(placed([0.0, 124.0], [1.0, 1.0], 0.0, [0.0, 0.0])),
            ),
            (
                Source::Text(TextSpec {
                    text: "Glass title 24",
                    size: 28.0,
                    paint: TextPaint {
                        origin: [16.0, 128.0],
                        fill: [1.0, 0.95, 0.85, 1.0],
                        outline: Some(Outline {
                            width: 1.5,
                            color: [0.02, 0.02, 0.1, 1.0],
                        }),
                        shadow: Some(Shadow {
                            offset: [2.0, 3.0],
                            blur: 2.0,
                            color: [0.0, 0.0, 0.0, 0.8],
                        }),
                    },
                    glyphs: Some(Box::new(move |glyph| GlyphTransform {
                        offset: [0.0, rise(glyph.index)],
                        opacity: 1.0 - rise(glyph.index) / 6.0,
                        ..GlyphTransform::IDENTITY
                    })),
                }),
                Layer::default().with_opacity((0.4 + t / 40.0).min(1.0)),
            ),
        ],
    }
}

/// A burned-in subtitle in the default style over the colour bars: one line on frame 0;
/// on frame 1 a cue that wraps at the title-safe width, then a forced second line.
fn subtitle_burn_in(frame: u32) -> Scene {
    let cue = if frame == 0 {
        "Burned-in subtitle"
    } else {
        "A longer subtitle that wraps inside the title-safe area of the frame\nand a second line"
    };
    Scene {
        layers: vec![
            (
                Source::Bars,
                Layer::default().with_transform(placed([0.0, 0.0], [2.0, 2.0], 0.0, [0.0, 0.0])),
            ),
            (Source::Subtitle(cue), Layer::default()),
        ],
    }
}

/// Vector shapes over the gradient: a rounded rectangle with a linear gradient and a
/// round-joined stroke, a radial-gradient ellipse, an even-odd star with a dashed stroke
/// turning with the frame, and an open cubic path with round caps; then a second shape
/// layer screened over the first.
fn shapes(frame: u32) -> Scene {
    let t = frame as f32;
    let star: Vec<Point> = (0..10)
        .map(|i| {
            let r = if i % 2 == 0 { 34.0 } else { 14.0 };
            let a = std::f32::consts::PI * (i as f32 / 5.0 - 0.5);
            Point::new(r * a.cos(), r * a.sin())
        })
        .collect();
    // A pentagram drawn as one self-crossing contour: even-odd leaves its centre empty.
    let pentagram: Vec<Point> = (0..5).map(|i| star[(i * 4) % 10]).collect();
    let base = vec![
        Shape::new(Geometry::Rect {
            rect: Rect::new(14.0, 14.0, 120.0, 70.0),
            radius: 14.0,
        })
        .filled(Paint::Linear {
            start: Point::new(14.0, 14.0),
            end: Point::new(134.0, 84.0),
            stops: vec![
                Stop::new(0.0, [0.05, 0.3, 0.9, 1.0]),
                Stop::new(1.0, [0.9, 0.15, 0.4, 1.0]),
            ],
        })
        .stroked(Stroke {
            join: Join::Round,
            ..Stroke::new(Paint::Solid([1.0, 1.0, 1.0, 1.0]), 3.0)
        }),
        Shape::new(Geometry::Ellipse {
            center: Point::new(0.0, 0.0),
            radii: [50.0, 32.0],
        })
        .filled(Paint::Radial {
            center: Point::new(0.0, 0.0),
            radius: 50.0,
            stops: vec![
                Stop::new(0.0, [1.0, 0.9, 0.3, 1.0]),
                Stop::new(1.0, [1.0, 0.3, 0.0, 0.0]),
            ],
        })
        .transformed(Affine::rotate(0.4).then(Affine::translate(230.0, 52.0))),
        Shape::new(Geometry::Polygon(pentagram))
            .filled_with(Paint::Solid([0.2, 0.9, 0.5, 1.0]), FillRule::EvenOdd)
            .stroked(Stroke {
                dash: Some(Dash {
                    array: vec![6.0, 4.0],
                    offset: t,
                }),
                ..Stroke::new(Paint::Solid([0.02, 0.05, 0.02, 1.0]), 1.5)
            })
            .transformed(Affine::rotate(t * 0.05).then(Affine::translate(70.0, 130.0))),
    ];
    let overlay = vec![
        Shape::new(Geometry::Path(Path::new().move_to(150.0, 160.0).cubic_to(
            180.0,
            90.0 - t,
            240.0,
            190.0 + t,
            300.0,
            120.0,
        )))
        .stroked(Stroke {
            cap: Cap::Round,
            ..Stroke::new(Paint::Solid([0.3, 0.6, 1.0, 1.0]), 8.0)
        }),
    ];
    Scene {
        layers: vec![
            (Source::Gradient, Layer::default()),
            (Source::Shapes(base), Layer::default()),
            (
                Source::Shapes(overlay),
                Layer::blended(BlendMode::Screen).with_opacity(0.8),
            ),
        ],
    }
}

/// The bars (160 x 90, masked in their own pixels) scaled to fill the canvas over the
/// gradient, with `masks`; turned a little with the frame so the masks follow the layer.
fn masked_bars(frame: u32, masks: Vec<Mask>) -> Scene {
    let turn = f64::from(frame) * 0.25;
    Scene {
        layers: vec![
            (Source::Gradient, Layer::default()),
            (
                Source::Masked(Box::new(Source::Bars), masks),
                Layer::default().with_transform(placed(
                    [160.0, 90.0],
                    [2.0, 2.0],
                    turn,
                    [80.0, 45.0],
                )),
            ),
        ],
    }
}

fn mask_rect() -> Shape {
    Shape::new(Geometry::Rect {
        rect: Rect::new(18.5, 14.25, 70.0, 50.0),
        radius: 8.0,
    })
}

fn mask_ellipse() -> Shape {
    Shape::new(Geometry::Ellipse {
        center: Point::new(0.0, 0.0),
        radii: [38.0, 24.0],
    })
    .transformed(Affine::rotate(-0.35).then(Affine::translate(100.0, 50.0)))
}

/// A rounded rectangle cut out of the bars with a hard edge.
fn mask_hard(frame: u32) -> Scene {
    masked_bars(frame, vec![Mask::new(vec![mask_rect()])])
}

/// A rotated ellipse feathered by 16 px (32 on the canvas, the layer being doubled),
/// which blurs at half resolution.
fn mask_feathered(frame: u32) -> Scene {
    masked_bars(frame, vec![Mask::new(vec![mask_ellipse()]).feathered(16.0)])
}

/// The ellipse inverted and slightly feathered: a soft hole showing the gradient.
fn mask_inverted(frame: u32) -> Scene {
    masked_bars(
        frame,
        vec![Mask::new(vec![mask_ellipse()]).feathered(2.0).inverted()],
    )
}

/// The rectangle and the ellipse as two masks, unioned.
fn mask_union(frame: u32) -> Scene {
    masked_bars(
        frame,
        vec![
            Mask::new(vec![mask_rect()]),
            Mask::new(vec![mask_ellipse()]).feathered(3.0),
        ],
    )
}

/// The rectangle and the ellipse as two masks, intersected.
fn mask_intersection(frame: u32) -> Scene {
    masked_bars(
        frame,
        vec![
            Mask::new(vec![mask_rect()]),
            Mask::new(vec![mask_ellipse()])
                .feathered(3.0)
                .with_mode(MaskMode::Intersection),
        ],
    )
}

/// Cross dissolve from the gradient to the colour bars over 24 frames: frame 0 is the
/// gradient alone, 12 half way, 24 the bars alone.
fn cross_dissolve(frame: u32) -> Scene {
    transition("cross-dissolve", frame)
}

/// The bundled transition `id` with default parameters from the gradient to the colour
/// bars over 24 frames.
fn transition(id: &'static str, frame: u32) -> Scene {
    transition_with(id, frame, |node| node)
}

/// The bundled transition `id` from the gradient to the colour bars over 24 frames, with
/// the direction and parameters `setup` sets.
fn transition_with(
    id: &'static str,
    frame: u32,
    setup: fn(TransitionNode) -> TransitionNode,
) -> Scene {
    Scene {
        layers: vec![(
            Source::Transition {
                id,
                progress: f64::from(frame) / 24.0,
                setup,
            },
            Layer::default(),
        )],
    }
}

/// Dip to black from the gradient to the colour bars, a quarter, half and three quarters
/// of the way.
fn dip_to_black(frame: u32) -> Scene {
    transition("dip-to-black", frame)
}

/// Dip to white from the gradient to the colour bars, a quarter, half and three quarters
/// of the way.
fn dip_to_white(frame: u32) -> Scene {
    transition("dip-to-white", frame)
}

/// Dip to colour (the default red) from the gradient to the colour bars, a quarter, half and three quarters
/// of the way.
fn dip_to_colour(frame: u32) -> Scene {
    transition("dip-to-colour", frame)
}

/// Film dissolve from the gradient to the colour bars, a quarter, half and three quarters
/// of the way.
fn film_dissolve(frame: u32) -> Scene {
    transition("film-dissolve", frame)
}

/// Additive dissolve from the gradient to the colour bars, a quarter, half and three quarters
/// of the way.
fn additive_dissolve(frame: u32) -> Scene {
    transition("additive-dissolve", frame)
}

/// Non-additive dissolve from the gradient to the colour bars, a quarter, half and three quarters
/// of the way.
fn non_additive_dissolve(frame: u32) -> Scene {
    transition("non-additive-dissolve", frame)
}

/// Linear wipe (default: travelling right) from the gradient to the colour bars, a quarter, half and three quarters of the way.
fn linear_wipe(frame: u32) -> Scene {
    transition("linear-wipe", frame)
}

/// Linear wipe travelling down and left with a 12 px softness and a 3 px orange border, a quarter, half and three quarters of the way.
fn linear_wipe_soft_border(frame: u32) -> Scene {
    transition_with("linear-wipe", frame, |node| {
        node.direction(Direction::DownLeft)
            .value("softness", &ParamValue::Number(12.0))
            .value("border_width", &ParamValue::Number(3.0))
            .value("border_colour", &ParamValue::Colour([1.0, 0.4, 0.0, 1.0]))
    })
}

/// Radial wipe from the centre, a quarter, half and three quarters of the way.
fn radial_wipe(frame: u32) -> Scene {
    transition("radial-wipe", frame)
}

/// Clock wipe from twelve o'clock, clockwise, a quarter, half and three quarters of the way.
fn clock_wipe(frame: u32) -> Scene {
    transition("clock-wipe", frame)
}

/// Iris opening as a circle, a quarter, half and three quarters of the way.
fn iris(frame: u32) -> Scene {
    transition("iris", frame)
}

/// Iris closing as a star with a 2 px white border, a quarter, half and three quarters of the way.
fn iris_star_close(frame: u32) -> Scene {
    transition_with("iris", frame, |node| {
        node.value("shape", &ParamValue::Choice("star".into()))
            .value("mode", &ParamValue::Choice("close".into()))
            .value("border_width", &ParamValue::Number(2.0))
    })
}

/// Iris opening as a diamond, half way.
fn iris_diamond(frame: u32) -> Scene {
    transition_with("iris", frame, |node| {
        node.value("shape", &ParamValue::Choice("diamond".into()))
    })
}

/// Iris opening as a rectangle of the frame's aspect, half way.
fn iris_rectangle(frame: u32) -> Scene {
    transition_with("iris", frame, |node| {
        node.value("shape", &ParamValue::Choice("rectangle".into()))
    })
}

/// Barn door opening on a vertical split, a quarter, half and three quarters of the way.
fn barn_door(frame: u32) -> Scene {
    transition("barn-door", frame)
}

/// Venetian blinds (8, travelling down), a quarter, half and three quarters of the way.
fn venetian_blinds(frame: u32) -> Scene {
    transition("venetian-blinds", frame)
}

/// Checker wipe (8 by 5, travelling right), a quarter, half and three quarters of the way.
fn checker_wipe(frame: u32) -> Scene {
    transition("checker-wipe", frame)
}

/// Push (default: travelling left, B from the right), a quarter, half and three quarters
/// of the way.
fn push(frame: u32) -> Scene {
    transition("push", frame)
}

/// Slide in (default: B from the right over A), a quarter, half and three quarters of the
/// way.
fn slide(frame: u32) -> Scene {
    transition("slide", frame)
}

/// Slide out travelling down: A slides off the bottom uncovering B, half way.
fn slide_out(frame: u32) -> Scene {
    transition_with("slide", frame, |node| {
        node.direction(Direction::Down)
            .value("mode", &ParamValue::Choice("out".into()))
    })
}

/// Whip pan (default: travelling left, a quarter of the frame's blur at the middle), a
/// quarter, half and three quarters of the way.
fn whip_pan(frame: u32) -> Scene {
    transition("whip-pan", frame)
}

/// Zoom in (3x, 30% blur), a quarter, half and three quarters of the way.
fn zoom(frame: u32) -> Scene {
    transition("zoom", frame)
}

/// Zoom out (3x, 30% blur), half way.
fn zoom_out(frame: u32) -> Scene {
    transition_with("zoom", frame, |node| {
        node.value("mode", &ParamValue::Choice("out".into()))
    })
}

/// Spin (half a turn clockwise, 125% zoom), a quarter, half and three quarters of the way.
fn spin(frame: u32) -> Scene {
    transition("spin", frame)
}

/// Swirl (540 degrees clockwise over the inscribed circle), a quarter, half and three
/// quarters of the way.
fn swirl(frame: u32) -> Scene {
    transition("swirl", frame)
}

/// Roll (default: travelling right about the bottom centre), a quarter, half and three
/// quarters of the way.
fn roll(frame: u32) -> Scene {
    transition("roll", frame)
}

/// Glitch (seed 1, 60%), a quarter, half and three quarters of the way.
fn glitch(frame: u32) -> Scene {
    transition("glitch", frame)
}

/// RGB split (travelling right, 6% of the width at the middle), a quarter, half and
/// three quarters of the way.
fn rgb_split(frame: u32) -> Scene {
    transition("rgb-split", frame)
}

/// Pixelate (blocks up to 5% of the width), a quarter, half and three quarters of the
/// way.
fn pixelate(frame: u32) -> Scene {
    transition("pixelate", frame)
}

/// VHS (seed 1, full strength), a quarter, half and three quarters of the way.
fn vhs(frame: u32) -> Scene {
    transition("vhs", frame)
}

/// Light leak (seed 1, sweeping right, warm orange at intensity 1), a quarter, half and
/// three quarters of the way.
fn light_leak(frame: u32) -> Scene {
    transition("light-leak", frame)
}

/// Film burn (seed 1, orange glow at 2), a quarter, half and three quarters of the way.
fn film_burn(frame: u32) -> Scene {
    transition("film-burn", frame)
}

/// Ink bleed (seed 1, 6% softness), a quarter, half and three quarters of the way.
fn ink_bleed(frame: u32) -> Scene {
    transition("ink-bleed", frame)
}

/// Paper tear (seed 1, travelling right, 4% roughness), a quarter, half and three
/// quarters of the way.
fn paper_tear(frame: u32) -> Scene {
    transition("paper-tear", frame)
}

/// Liquid (seed 1, 8% push), a quarter, half and three quarters of the way.
fn liquid(frame: u32) -> Scene {
    transition("liquid", frame)
}
