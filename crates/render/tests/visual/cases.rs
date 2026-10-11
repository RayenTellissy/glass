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

/// A rotated ellipse feathered by 8 px (16 on the canvas, the layer being doubled).
fn mask_feathered(frame: u32) -> Scene {
    masked_bars(frame, vec![Mask::new(vec![mask_ellipse()]).feathered(8.0)])
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
