//! The reference sequences. Each case is a scene as a function of the frame index; the
//! harness renders every listed frame and compares it with
//! `tests/visual/goldens/<name>/frame_<index>.png`. Keep scenes at [`CANVAS`] and to a
//! few frames, so the goldens stay small.

use render::blend::BlendMode;
use render::crop::Crop;
use render::nodes::Layer;
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
