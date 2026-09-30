//! Glyph rasterisation with swash: a glyph's fill coverage, or the coverage of a round-join
//! stroke along its outline, as an 8-bit mask placed relative to the glyph origin.

use cosmic_text::{CacheKey, CacheKeyFlags, FontSystem};
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Join, Stroke, Transform, Vector};

/// An 8-bit coverage mask. The mask's top-left pixel sits `left` pixels right of and
/// `top` pixels above the glyph origin (the pen position on the baseline).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mask {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// Row-major, `width * height` bytes.
    pub data: Vec<u8>,
}

impl Mask {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Rasterises `key`'s glyph: its fill when `stroke` is 0, else a stroke `stroke` pixels
/// wide centred on its outline. Colour glyphs (emoji) keep only their alpha, so they take
/// the text's fill colour. `None` when the font is missing or the glyph has no image.
pub fn rasterize(
    fonts: &mut FontSystem,
    context: &mut ScaleContext,
    key: CacheKey,
    stroke: f32,
) -> Option<Mask> {
    let font = fonts.get_font(key.font_id, key.font_weight)?;
    let swash_font = font.as_swash();
    let wght = swash::Tag::from_be_bytes(*b"wght");
    let mut builder = context
        .builder(swash_font)
        .size(f32::from_bits(key.font_size_bits))
        .hint(!key.flags.contains(CacheKeyFlags::DISABLE_HINTING));
    if let Some(axis) = swash_font.variations().find_by_tag(wght) {
        let weight = f32::from(key.font_weight.0).clamp(axis.min_value(), axis.max_value());
        builder =
            builder.normalized_coords(swash_font.variations().normalized_coords([(wght, weight)]));
    }
    let mut scaler = builder.build();

    let fill_sources = [
        Source::ColorOutline(0),
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Outline,
    ];
    let stroke_sources = [Source::Outline];
    let mut render = Render::new(if stroke > 0.0 {
        &stroke_sources
    } else {
        &fill_sources
    });
    render
        .format(Format::Alpha)
        .offset(Vector::new(key.x_bin.as_float(), key.y_bin.as_float()))
        .transform(
            key.flags
                .contains(CacheKeyFlags::FAKE_ITALIC)
                .then(|| Transform::skew(Angle::from_degrees(14.0), Angle::from_degrees(0.0))),
        );
    if stroke > 0.0 {
        let mut style = Stroke::new(stroke);
        style.join(Join::Round);
        render.style(style);
    }
    let image = render.render(&mut scaler, key.glyph_id)?;
    let (width, height) = (image.placement.width, image.placement.height);
    let data = match image.content {
        Content::Mask => image.data,
        Content::Color => image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[3])
            .collect(),
        // Not requested (`Format::Alpha`), but average the channels if it ever appears.
        Content::SubpixelMask => image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| ((u16::from(px[0]) + u16::from(px[1]) + u16::from(px[2])) / 3) as u8)
            .collect(),
    };
    if data.len() != (width * height) as usize {
        return None;
    }
    Some(Mask {
        left: image.placement.left,
        top: image.placement.top,
        width,
        height,
        data,
    })
}
