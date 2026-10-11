//! Burned-in subtitles: the cue showing at a time, laid out with [`crate::text`] and
//! drawn over a frame.
//!
//! [`SubtitleStyle`] sizes everything from the frame: the font size is a fraction of the
//! frame height, lines wrap at the width of the title-safe area and are centred, and the
//! last line's bottom sits on the safe area's bottom edge. The default is white with a
//! black outline and a soft drop shadow, legible on any picture. A frame burns its cue
//! in four steps:
//!
//! ```ignore
//! if let Some(cue) = subtitle::cue_at(&cues, time_ms) {
//!     let burn = style.lay_out(&mut engine, &cue.text, (1920, 1080));
//!     let prepared = atlas.prepare(&mut engine, &burn.layout, &burn.paint, None);
//!     frame = subtitle::add_burn_in(&mut graph, frame, &atlas, &prepared, (1920, 1080));
//! }
//! ```
//!
//! Cue times are whole milliseconds ([`SubtitleCue`]); `time_ms` is the frame's time
//! rounded to the nearest millisecond, as the cues were, so a cue that starts and ends
//! on frame boundaries covers exactly its frames.

use media::SubtitleCue;

use crate::graph::{Graph, Port};
use crate::nodes::{Layer, composite_stack};
use crate::text::{
    self, FontFamily, GlyphAtlas, Outline, PreparedText, Shadow, TextAlign, TextEngine, TextFormat,
    TextLayout, TextPaint,
};

/// How burned-in subtitles look, relative to the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleStyle {
    pub family: FontFamily,
    /// Font size as a fraction of the frame height.
    pub size: f32,
    /// Distance between baselines as a multiple of the font size.
    pub line_height: f32,
    /// The title-safe margin on every side, as a fraction of the frame's width (left and
    /// right) and height (top and bottom).
    pub safe_margin: f32,
    /// Linear light, straight alpha.
    pub fill: [f32; 4],
    /// Outline width as a fraction of the font size; 0 for none.
    pub outline: f32,
    pub outline_color: [f32; 4],
    /// Shadow offset (right and down) and blur as fractions of the font size; `None`
    /// for no shadow.
    pub shadow: Option<(f32, f32)>,
    pub shadow_color: [f32; 4],
}

impl Default for SubtitleStyle {
    /// Sans-serif at 5% of the frame height (54 px at 1080p), white with a black outline
    /// of 8% of the font size and a 60% black shadow, inside a 5% title-safe margin
    /// (EBU R 95 for 16:9).
    fn default() -> Self {
        Self {
            family: FontFamily::SansSerif,
            size: 0.05,
            line_height: 1.2,
            safe_margin: 0.05,
            fill: [1.0, 1.0, 1.0, 1.0],
            outline: 0.08,
            outline_color: [0.0, 0.0, 0.0, 1.0],
            shadow: Some((0.05, 0.08)),
            shadow_color: [0.0, 0.0, 0.0, 0.6],
        }
    }
}

/// A cue laid out for a frame: hand both to [`GlyphAtlas::prepare`].
#[derive(Clone, Debug)]
pub struct BurnIn {
    pub layout: TextLayout,
    pub paint: TextPaint,
}

impl SubtitleStyle {
    /// The text format for a `frame` of that size in pixels.
    pub fn format(&self, frame: (u32, u32)) -> TextFormat {
        let (w, h) = (frame.0 as f32, frame.1 as f32);
        TextFormat {
            family: self.family.clone(),
            size: (h * self.size).max(1.0),
            line_height: self.line_height,
            weight: 400,
            italic: false,
            align: TextAlign::Center,
            max_width: Some((w * (1.0 - 2.0 * self.safe_margin)).max(1.0)),
        }
    }

    /// Lays `text` out for a `frame` of that size: centred in the title-safe area's
    /// width, the last line's bottom on its bottom edge.
    pub fn lay_out(&self, engine: &mut TextEngine, text: &str, frame: (u32, u32)) -> BurnIn {
        let format = self.format(frame);
        let layout = engine.layout(text, &format);
        let (w, h) = (frame.0 as f32, frame.1 as f32);
        let font = format.size;
        let left = w * self.safe_margin;
        let bottom = h * (1.0 - self.safe_margin);
        let paint = TextPaint {
            origin: [left, bottom - layout.height],
            fill: self.fill,
            outline: (self.outline > 0.0).then_some(Outline {
                width: self.outline * font,
                color: self.outline_color,
            }),
            shadow: self.shadow.map(|(offset, blur)| Shadow {
                offset: [offset * font, offset * font],
                blur: blur * font,
                color: self.shadow_color,
            }),
        };
        BurnIn { layout, paint }
    }
}

/// The cue showing at `time_ms` (its start at or before, its end after) in `cues`,
/// which are in time order and do not overlap.
pub fn cue_at(cues: &[SubtitleCue], time_ms: i64) -> Option<&SubtitleCue> {
    let after = cues.partition_point(|c| c.start_ms <= time_ms);
    let cue = cues.get(after.checked_sub(1)?)?;
    (time_ms < cue.end_ms).then_some(cue)
}

/// Draws a prepared cue over `frame` (premultiplied linear light, `size` pixels) and
/// returns the port of the result.
pub fn add_burn_in<'a>(
    graph: &mut Graph<'a>,
    frame: Port,
    atlas: &'a GlyphAtlas,
    prepared: &'a PreparedText,
    size: (u32, u32),
) -> Port {
    let text = text::add_to_graph(graph, atlas, prepared, size);
    composite_stack(graph, size, Some(frame), vec![(text, Layer::default())])
        .expect("a background and one layer composite")
}
