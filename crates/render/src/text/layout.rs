//! Shaping and layout with cosmic-text: font selection and fallback from the system font
//! database, bidirectional text, line breaking and alignment. The result is a list of
//! positioned glyphs and per-line metrics, independent of any GPU state.

use std::ops::Range;
use std::path::Path;

use cosmic_text::fontdb::Database;
use cosmic_text::{
    Align, Attrs, Buffer, CacheKey, Family, FontSystem, LayoutGlyph, Metrics, Shaping, Style,
    Weight,
};
use swash::scale::ScaleContext;

use super::raster::{self, Mask};

/// The font a text is set in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontFamily {
    /// A family name, such as "Arial"; missing characters fall back to other fonts.
    Named(String),
    SansSerif,
    Serif,
    Monospace,
}

/// Horizontal alignment of each line within the text box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    /// The paragraph's natural side: left for left-to-right text, right for right-to-left.
    #[default]
    Start,
    Center,
    /// The side opposite `Start`.
    End,
}

/// How a text is set: font, size and line layout.
#[derive(Clone, Debug, PartialEq)]
pub struct TextFormat {
    pub family: FontFamily,
    /// Font size in pixels.
    pub size: f32,
    /// Distance between baselines as a multiple of `size`.
    pub line_height: f32,
    /// CSS weight, 100 to 900 (400 regular, 700 bold).
    pub weight: u16,
    pub italic: bool,
    pub align: TextAlign,
    /// Lines wrap at word boundaries past this width in pixels; `None` breaks only at
    /// newlines.
    pub max_width: Option<f32>,
}

impl Default for TextFormat {
    fn default() -> Self {
        Self {
            family: FontFamily::SansSerif,
            size: 48.0,
            line_height: 1.2,
            weight: 400,
            italic: false,
            align: TextAlign::Start,
            max_width: None,
        }
    }
}

/// One shaped glyph and the line it sits on.
#[derive(Clone, Debug)]
pub struct LaidGlyph {
    pub glyph: LayoutGlyph,
    pub line: usize,
}

impl LaidGlyph {
    /// The glyph's byte range in the source text.
    pub fn cluster(&self) -> Range<usize> {
        self.glyph.start..self.glyph.end
    }

    pub fn rtl(&self) -> bool {
        self.glyph.level.is_rtl()
    }
}

/// One laid-out line, in pixels from the top-left of the text box.
#[derive(Clone, Debug, PartialEq)]
pub struct LineMetrics {
    pub top: f32,
    pub baseline: f32,
    pub height: f32,
    pub width: f32,
    /// The paragraph's direction.
    pub rtl: bool,
    /// Indices into [`TextLayout::glyphs`].
    pub glyphs: Range<usize>,
}

/// A shaped and laid-out text, line by line. Within a right-to-left run glyphs may be in
/// logical order; their `x` gives the position.
#[derive(Clone, Debug, Default)]
pub struct TextLayout {
    pub glyphs: Vec<LaidGlyph>,
    pub lines: Vec<LineMetrics>,
    /// The widest line.
    pub width: f32,
    /// From the first line's top to the last line's bottom.
    pub height: f32,
}

/// Fonts and the shaping and rasterisation state that goes with them. One per render
/// thread; loading the system fonts takes a moment, so keep it alive.
pub struct TextEngine {
    fonts: FontSystem,
    scale: ScaleContext,
}

impl std::fmt::Debug for TextEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextEngine")
            .field("faces", &self.fonts.db().len())
            .finish_non_exhaustive()
    }
}

impl TextEngine {
    /// Every font installed on the system, with the system locale for fallback.
    pub fn system() -> Self {
        Self::from_font_system(FontSystem::new())
    }

    /// Only the given font files (tests pin a font this way). Fallback stays within them.
    pub fn with_font_files(paths: &[&Path]) -> std::io::Result<Self> {
        let mut db = Database::new();
        for path in paths {
            db.load_font_file(path)?;
        }
        Ok(Self::from_font_system(FontSystem::new_with_locale_and_db(
            "en-US".to_owned(),
            db,
        )))
    }

    pub fn from_font_system(fonts: FontSystem) -> Self {
        Self {
            fonts,
            scale: ScaleContext::new(),
        }
    }

    pub fn font_system(&mut self) -> &mut FontSystem {
        &mut self.fonts
    }

    /// Shapes and lays out `text`.
    pub fn layout(&mut self, text: &str, format: &TextFormat) -> TextLayout {
        let size = format.size.max(0.1);
        let metrics = Metrics::new(size, size * format.line_height.max(0.1));
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        let mut buffer = buffer.borrow_with(&mut self.fonts);
        buffer.set_size(format.max_width, None);
        let family = match &format.family {
            FontFamily::Named(name) => Family::Name(name),
            FontFamily::SansSerif => Family::SansSerif,
            FontFamily::Serif => Family::Serif,
            FontFamily::Monospace => Family::Monospace,
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(format.weight))
            .style(if format.italic {
                Style::Italic
            } else {
                Style::Normal
            });
        let align = match format.align {
            TextAlign::Start => None,
            TextAlign::Center => Some(Align::Center),
            TextAlign::End => Some(Align::End),
        };
        buffer.set_text(text, &attrs, Shaping::Advanced, align);
        buffer.shape_until_scroll(false);

        let mut layout = TextLayout::default();
        for run in buffer.layout_runs() {
            let first = layout.glyphs.len();
            let line = layout.lines.len();
            layout
                .glyphs
                .extend(run.glyphs.iter().map(|glyph| LaidGlyph {
                    glyph: glyph.clone(),
                    line,
                }));
            layout.lines.push(LineMetrics {
                top: run.line_top,
                baseline: run.line_y,
                height: run.line_height,
                width: run.line_w,
                rtl: run.rtl,
                glyphs: first..layout.glyphs.len(),
            });
            layout.width = layout.width.max(run.line_w);
            layout.height = run.line_top + run.line_height;
        }
        layout
    }

    /// Rasterises one glyph: its fill when `stroke` is 0, else a stroke of that width
    /// centred on its outline.
    pub fn rasterize(&mut self, key: CacheKey, stroke: f32) -> Option<Mask> {
        raster::rasterize(&mut self.fonts, &mut self.scale, key, stroke)
    }
}
