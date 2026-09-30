//! Text: shaping and layout with cosmic-text ([`TextEngine`]), a GPU glyph atlas
//! ([`GlyphAtlas`]) and render-graph nodes that draw a text into linear light with a fill
//! colour, an outline and a drop shadow.
//!
//! A frame draws text in three steps: lay the text out (CPU, cached by the caller while
//! the text and format are unchanged), prepare it against the atlas (rasterises and
//! uploads glyphs the atlas does not hold yet, and asks the optional per-glyph transform
//! hook for each glyph's offset, scale, rotation and opacity), then add it to the frame's
//! graph with [`add_to_graph`]:
//!
//! ```ignore
//! let layout = engine.layout("Title", &format);
//! atlas.begin_frame();
//! let prepared = atlas.prepare(&mut engine, &layout, &paint, Some(&|g: &GlyphInfo| animator.at(g)));
//! let mut g = Graph::new();
//! let text = text::add_to_graph(&mut g, &atlas, &prepared, (3840, 2160));
//! g.export(text, &target);
//! ```
//!
//! The graph passes are: `text glyphs` (glyph quads into a coverage texture: fill in red,
//! outline in green, their union in blue), `text shadow h` and `text shadow v` (the union
//! offset and Gaussian-blurred, only with a shadow) and `text fill` (colours composited:
//! fill over outline over shadow, premultiplied, on transparent black). Colours are linear
//! light in the working primaries with straight alpha.

mod atlas;
mod layout;
mod nodes;
mod raster;

pub use atlas::{AtlasEntry, AtlasStats, GlyphAtlas, GlyphKey, PreparedGlyph, PreparedText};
pub use layout::{
    FontFamily, LaidGlyph, LineMetrics, TextAlign, TextEngine, TextFormat, TextLayout,
};
pub use nodes::{GlyphCoverageNode, ShadowBlurNode, TextCompositeNode};
pub use raster::Mask;

use std::ops::Range;

use crate::graph::{Graph, Port};

/// A glyph's placement handed to the per-glyph transform hook.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphInfo {
    /// Index into [`TextLayout::glyphs`].
    pub index: usize,
    pub line: usize,
    /// The glyph's byte range in the source text.
    pub cluster: Range<usize>,
    pub rtl: bool,
    /// Horizontal advance in pixels.
    pub advance: f32,
    /// Centre of the glyph's cell (its advance by its line's height) in canvas pixels:
    /// the point the transform scales and rotates about.
    pub pivot: [f32; 2],
}

/// A per-glyph transform about the glyph's pivot, applied in this order: scale, rotate,
/// offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphTransform {
    /// Pixels on the canvas, y down.
    pub offset: [f32; 2],
    pub scale: [f32; 2],
    /// Radians; positive turns clockwise on screen.
    pub rotation: f32,
    /// Multiplies the glyph's coverage (fill, outline and shadow); 0 hides it.
    pub opacity: f32,
}

impl GlyphTransform {
    pub const IDENTITY: Self = Self {
        offset: [0.0, 0.0],
        scale: [1.0, 1.0],
        rotation: 0.0,
        opacity: 1.0,
    };
}

impl Default for GlyphTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// A stroke around each glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outline {
    /// Pixels outside the glyph's edge (the stroke also covers as much inside).
    pub width: f32,
    /// Linear light, straight alpha.
    pub color: [f32; 4],
}

/// A blurred copy of the text (fill and outline) behind it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    /// Pixels on the canvas, y down.
    pub offset: [f32; 2],
    /// Gaussian standard deviation in pixels; 0 is a hard shadow. Kernels stop at
    /// [`ShadowBlurNode::MAX_RADIUS`] pixels (3 standard deviations up to 32).
    pub blur: f32,
    /// Linear light, straight alpha.
    pub color: [f32; 4],
}

/// How a text is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct TextPaint {
    /// Where the layout's top-left lands on the canvas, in pixels.
    pub origin: [f32; 2],
    /// Linear light, straight alpha.
    pub fill: [f32; 4],
    pub outline: Option<Outline>,
    pub shadow: Option<Shadow>,
}

impl Default for TextPaint {
    fn default() -> Self {
        Self {
            origin: [0.0, 0.0],
            fill: [1.0; 4],
            outline: None,
            shadow: None,
        }
    }
}

/// Adds the passes that draw `text` onto a transparent `size` canvas and returns the
/// port of the premultiplied linear result.
pub fn add_to_graph<'a>(
    graph: &mut Graph<'a>,
    atlas: &'a GlyphAtlas,
    text: &'a PreparedText,
    size: (u32, u32),
) -> Port {
    let coverage = graph.add(GlyphCoverageNode::new(atlas, text, size), &[]);
    match text.paint.shadow {
        Some(shadow) => {
            let bounds = text.bounds(atlas);
            let h = graph.add(
                ShadowBlurNode::horizontal(shadow).with_bounds(bounds),
                &[coverage.out(0)],
            );
            let v = graph.add(
                ShadowBlurNode::vertical(shadow).with_bounds(bounds),
                &[h.out(0)],
            );
            graph
                .add(
                    TextCompositeNode::new(&text.paint),
                    &[coverage.out(0), v.out(0)],
                )
                .out(0)
        }
        None => graph
            .add(TextCompositeNode::new(&text.paint), &[coverage.out(0)])
            .out(0),
    }
}
