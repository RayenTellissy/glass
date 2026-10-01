//! The offline media slate: the frame shown in place of a clip whose source file is
//! missing or unreadable, in the preview and in exports, so an offline clip keeps its
//! place, length and effects and says what is wrong.
//!
//! The design is the viewer's offline state from the error and warning states sheet,
//! drawn at the clip's size: the viewer ground, three soft colour glows seen through a
//! dot screen, and a centred glass card with the offline icon, the title ("Media
//! offline"), the file name in the mono face and a muted detail line. Sizes are design
//! pixels of a 640 x 360 frame, scaled by the smaller of `width / 640` and
//! `height / 360`; the glows and dot screen fill the whole frame. The design's Relink
//! button is interactive, so it is the preview's to draw over the slate, not part of it.
//!
//! Colours come from theme roles ([`SlatePalette::from_theme`]), converted to linear light
//! once. The slate is picture content, so it uses the dark theme whatever the UI theme is
//! and exports look the same everywhere.
//!
//! A slate is laid out and rendered once per canvas size and label into a texture that
//! [`SlateCache`] keeps; every frame of every offline clip with that size and label
//! imports the same texture ([`SlateCache::add_to_graph`]), so nothing is laid out or
//! drawn again per frame. Rendering one is a small graph: `slate background` (the
//! `slate.wgsl` pass, whose CPU version is [`reference`]), the three texts through
//! [`crate::text`], and a [`crate::nodes::CompositeNode`] exported into the cached
//! texture.

use std::collections::HashMap;

use assets::theme::{Rgba8, Role, ThemeColors, ThemeKind};
use swash::zeno::{self, Cap, Join, Stroke, Transform};

use crate::graph::{Graph, Node, NodeContext, POOL_USAGE, Port, TextureDesc, TexturePool};
use crate::nodes::{Layer, composite_stack};
use crate::renderer::{Effect, Filter, Renderer, Uniform};
use crate::text::{
    self, FontFamily, GlyphAtlas, Mask, TextAlign, TextEngine, TextFormat, TextPaint,
};

/// The design frame the slate's sizes are given in.
pub const DESIGN_SIZE: (f32, f32) = (640.0, 360.0);

/// Card width, corner radius, edge width, padding (inside the edge) and the gaps.
const CARD_WIDTH: f32 = 248.0;
const CARD_RADIUS: f32 = 8.0;
const CARD_EDGE: f32 = 1.0;
const CARD_PADDING: f32 = 16.0;
const ROW_GAP: f32 = 2.0;
const ICON_SIZE: f32 = 16.0;
const ICON_GAP: f32 = 8.0;
/// Title: label type (12 px on a 16 px line), medium; file name and detail: caption type
/// (11 px on 14 px), the file name in the mono face.
const TITLE_SIZE: f32 = 12.0;
const TITLE_LINE: f32 = 16.0;
const TITLE_WEIGHT: u16 = 500;
const CAPTION_SIZE: f32 = 11.0;
const CAPTION_LINE: f32 = 14.0;
/// The card's drop shadow: 16 px down, 48 px blur (a Gaussian of half that deviation).
const SHADOW_OFFSET: f32 = 16.0;
const SHADOW_SIGMA: f32 = 24.0;
/// The dot screen: 5 px cells, each a hole of radius 1.7 px in the ground colour.
const DOT_CELL: f32 = 5.0;
const DOT_RADIUS: f32 = 1.7;
/// The card's backdrop is the dot screen blurred: the ground covers this share of a cell.
const DOT_COVER: f32 = 1.0 - std::f32::consts::PI * DOT_RADIUS * DOT_RADIUS / (DOT_CELL * DOT_CELL);
/// The card tints its backdrop with bg/1 at 62%.
const CARD_TINT: f32 = 0.62;
/// The glass gradient runs from its top colour to its bottom colour over the top 35%.
const GLASS_STOP: f32 = 0.35;
/// The glows, bottom to top: centre and radii as fractions of the frame (the design's
/// ellipses on a box 40 px larger than the frame on every side).
const GLOWS: [[f32; 4]; 3] = [
    [0.8375, 0.133_333, 0.3375, 0.488_889],
    [0.725, 0.683_333, 0.506_25, 0.672_222],
    [0.275, 0.377_778, 0.45, 0.611_111],
];
/// The offline icon (Lucide `wifi-off`) on a 24 unit grid, stroked 2.25 units wide.
const ICON_PATH: &str = "M12 20h.01 M8.5 16.43a5 5 0 0 1 7 0 M5 12.86a10 10 0 0 1 5.17-2.69 \
    M19 12.86a10 10 0 0 0-2-1.52 M2 8.82a15 15 0 0 1 4.18-2.64 \
    M22 8.82a15 15 0 0 0-11.29-3.76 M2 2l20 20";
const ICON_GRID: f32 = 24.0;
const ICON_STROKE: f32 = 2.25;

/// Linear light with straight alpha from a token's sRGB bytes.
pub fn linear(c: Rgba8) -> [f32; 4] {
    let eotf = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [eotf(c.r), eotf(c.g), eotf(c.b), f32::from(c.a) / 255.0]
}

/// The slate's colours, linear light with straight alpha, each from a theme role.
#[derive(Clone, Debug, PartialEq)]
pub struct SlatePalette {
    /// `bg/0`: the viewer ground and the dot screen.
    pub ground: [f32; 4],
    /// `bg/1`: the card's tint.
    pub card: [f32; 4],
    /// `clip/title`, `clip/video`, `clip/offline`: the glows, bottom to top.
    pub glows: [[f32; 4]; 3],
    /// `glass/top`, `glass/bottom`, `glass/edge`, `glass/highlight`, `glass/shadow`.
    pub glass_top: [f32; 4],
    pub glass_bottom: [f32; 4],
    pub glass_edge: [f32; 4],
    pub glass_highlight: [f32; 4],
    pub glass_shadow: [f32; 4],
    /// `state/error`: the icon.
    pub icon: [f32; 4],
    /// `text/primary`, `text/secondary`, `text/muted`: title, file name, detail.
    pub title: [f32; 4],
    pub file_name: [f32; 4],
    pub detail: [f32; 4],
}

impl SlatePalette {
    pub fn from_theme(theme: &ThemeColors) -> Self {
        let c = |role| linear(theme.get(role));
        Self {
            ground: c(Role::Bg0),
            card: c(Role::Bg1),
            glows: [c(Role::ClipTitle), c(Role::ClipVideo), c(Role::ClipOffline)],
            glass_top: c(Role::GlassTop),
            glass_bottom: c(Role::GlassBottom),
            glass_edge: c(Role::GlassEdge),
            glass_highlight: c(Role::GlassHighlight),
            glass_shadow: c(Role::GlassShadow),
            icon: c(Role::StateError),
            title: c(Role::TextPrimary),
            file_name: c(Role::TextSecondary),
            detail: c(Role::TextMuted),
        }
    }
}

impl Default for SlatePalette {
    /// The dark theme's roles (see the module docs).
    fn default() -> Self {
        match ThemeColors::load(ThemeKind::Dark) {
            Ok(theme) => Self::from_theme(&theme),
            Err(e) => {
                tracing::error!("the dark theme does not load ({e}); the slate is black");
                Self {
                    ground: [0.0, 0.0, 0.0, 1.0],
                    card: [0.0; 4],
                    glows: [[0.0; 4]; 3],
                    glass_top: [0.0; 4],
                    glass_bottom: [0.0; 4],
                    glass_edge: [0.0; 4],
                    glass_highlight: [0.0; 4],
                    glass_shadow: [0.0; 4],
                    icon: [1.0; 4],
                    title: [1.0; 4],
                    file_name: [1.0; 4],
                    detail: [1.0; 4],
                }
            }
        }
    }
}

/// Palette and faces of every slate a [`SlateCache`] draws.
#[derive(Clone, Debug, PartialEq)]
pub struct SlateStyle {
    pub palette: SlatePalette,
    /// The UI face (title and detail) and the mono face (file name). Missing characters
    /// and missing families fall back to other system fonts.
    pub ui_font: FontFamily,
    pub mono_font: FontFamily,
}

impl Default for SlateStyle {
    fn default() -> Self {
        Self {
            palette: SlatePalette::default(),
            ui_font: FontFamily::Named("Geist".to_owned()),
            mono_font: FontFamily::Named("Geist Mono".to_owned()),
        }
    }
}

/// What a slate says, already translated: the title (`media.offline.slate.title`), the
/// file name, and a detail line (empty for none).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SlateLabel {
    pub title: String,
    pub file_name: String,
    pub detail: String,
}

/// Where a slate's parts landed on its canvas, in pixels (x0, y0, x1, y1).
#[derive(Clone, Debug, PartialEq)]
pub struct SlateGeometry {
    pub size: (u32, u32),
    /// Canvas pixels per design pixel.
    pub scale: f32,
    pub card: [f32; 4],
    /// The icon's 16 x 16 design box.
    pub icon: [f32; 4],
    /// Each text's line boxes (top of the first line to the bottom of the last); the
    /// detail's is empty when there is no detail.
    pub title: [f32; 4],
    pub file_name: [f32; 4],
    pub detail: [f32; 4],
    /// The file name as drawn: shortened in the middle with `…` when it is wider than
    /// the card.
    pub file_name_shown: String,
}

/// The background pass's inputs: the geometry and palette as the shader reads them.
#[derive(Clone, Debug, PartialEq)]
pub struct SlateParams {
    pub size: (u32, u32),
    pub scale: f32,
    pub card: [f32; 4],
    pub palette: SlatePalette,
    /// The icon's coverage mask, placed at `left`, `top` canvas pixels.
    pub icon: Mask,
}

impl SlateParams {
    fn uniform(&self) -> Uniform {
        let s = self.scale;
        let p = &self.palette;
        let mut u = Uniform::new()
            .vec4([self.size.0 as f32, self.size.1 as f32, s, DOT_CELL * s])
            .vec4([DOT_RADIUS * s, DOT_COVER, CARD_TINT, GLASS_STOP])
            .vec4(self.card)
            .vec4([
                CARD_RADIUS * s,
                CARD_EDGE * s,
                SHADOW_OFFSET * s,
                SHADOW_SIGMA * s,
            ])
            .vec4([
                self.icon.left as f32,
                self.icon.top as f32,
                self.icon.width as f32,
                self.icon.height as f32,
            ])
            .vec4(p.ground)
            .vec4(p.card)
            .vec4(p.glass_top)
            .vec4(p.glass_bottom)
            .vec4(p.glass_edge)
            .vec4(p.glass_highlight)
            .vec4(p.glass_shadow)
            .vec4(p.icon);
        for (color, glow) in p.glows.iter().zip(GLOWS) {
            u = u.vec4(*color).vec4(glow);
        }
        u
    }
}

fn erf(x: f64) -> f64 {
    let a = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * a);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-a * a).exp();
    y.copysign(x)
}

fn rounded_cover(p: [f64; 2], r: [f64; 4], radius: f64) -> f64 {
    let (cx, cy) = ((r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5);
    let (hx, hy) = ((r[2] - r[0]) * 0.5, (r[3] - r[1]) * 0.5);
    let qx = (p[0] - cx).abs() - hx + radius;
    let qy = (p[1] - cy).abs() - hy + radius;
    let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
    (0.5 - d).clamp(0.0, 1.0)
}

fn mix3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn rgb(c: [f32; 4]) -> [f64; 3] {
    [c[0], c[1], c[2]].map(f64::from)
}

/// The background pass's linear colour at canvas point `p` (a pixel centre): the CPU
/// reference for `slate.wgsl`. The background is opaque.
pub fn reference(params: &SlateParams, p: [f64; 2]) -> [f64; 3] {
    let pal = &params.palette;
    let s = f64::from(params.scale);
    let (w, h) = (f64::from(params.size.0), f64::from(params.size.1));
    let ground = rgb(pal.ground);

    let mut glows = ground;
    for (color, g) in pal.glows.iter().zip(GLOWS) {
        let g = g.map(f64::from);
        let t = ((p[0] / w - g[0]) / g[2]).hypot((p[1] / h - g[1]) / g[3]);
        let a = (1.0 - t).clamp(0.0, 1.0) * f64::from(color[3]);
        glows = mix3(glows, rgb(*color), a);
    }

    let cell = f64::from(DOT_CELL) * s;
    let q = [0, 1].map(|i| p[i] - ((p[i] / cell).floor() + 0.5) * cell);
    let hole = (f64::from(DOT_RADIUS) * s - q[0].hypot(q[1]) + 0.5).clamp(0.0, 1.0);
    let mut c = mix3(glows, ground, 1.0 - hole);

    let card = params.card.map(f64::from);
    let radius = f64::from(CARD_RADIUS) * s;
    let edge = f64::from(CARD_EDGE) * s;
    let cover = rounded_cover(p, card, radius);

    let dy = f64::from(SHADOW_OFFSET) * s;
    let k = 1.0 / (f64::from(SHADOW_SIGMA) * s * std::f64::consts::SQRT_2);
    let blur = 0.25
        * (erf((p[0] - card[0]) * k) - erf((p[0] - card[2]) * k))
        * (erf((p[1] - card[1] - dy) * k) - erf((p[1] - card[3] - dy) * k));
    let shadow = f64::from(pal.glass_shadow[3]) * blur;
    c = mix3(c, rgb(pal.glass_shadow), shadow * (1.0 - cover));

    let backdrop = mix3(glows, ground, f64::from(DOT_COVER));
    let mut fill = mix3(backdrop, rgb(pal.card), f64::from(CARD_TINT));
    let v = ((p[1] - card[1]) / (card[3] - card[1]) / f64::from(GLASS_STOP)).clamp(0.0, 1.0);
    let (ta, ba) = (f64::from(pal.glass_top[3]), f64::from(pal.glass_bottom[3]));
    let alpha = ta + (ba - ta) * v;
    let premul = mix3(
        rgb(pal.glass_top).map(|x| x * ta),
        rgb(pal.glass_bottom).map(|x| x * ba),
        v,
    );
    fill = [0, 1, 2].map(|i| fill[i] * (1.0 - alpha) + premul[i]);
    c = mix3(c, fill, cover);

    let inner = [
        card[0] + edge,
        card[1] + edge,
        card[2] - edge,
        card[3] - edge,
    ];
    let inner_radius = (radius - edge).max(0.0);
    let inner_cover = rounded_cover(p, inner, inner_radius);
    let shifted = [inner[0], inner[1] + s, inner[2], inner[3] + s];
    let highlight = inner_cover * (1.0 - rounded_cover(p, shifted, inner_radius));
    c = mix3(
        c,
        rgb(pal.glass_highlight),
        f64::from(pal.glass_highlight[3]) * highlight,
    );
    c = mix3(
        c,
        rgb(pal.glass_edge),
        f64::from(pal.glass_edge[3]) * (cover - inner_cover).clamp(0.0, 1.0),
    );

    let icon = &params.icon;
    let ix = (p[0] - f64::from(icon.left)).floor();
    let iy = (p[1] - f64::from(icon.top)).floor();
    if ix >= 0.0 && iy >= 0.0 && ix < f64::from(icon.width) && iy < f64::from(icon.height) {
        let m = f64::from(icon.data[iy as usize * icon.width as usize + ix as usize]) / 255.0;
        c = mix3(c, rgb(pal.icon), f64::from(pal.icon[3]) * m);
    }
    c
}

/// The icon's coverage in a `size` pixel box whose top-left is at `origin` (whole
/// pixels).
fn icon_mask(origin: [f32; 2], size: f32) -> Mask {
    let k = size / ICON_GRID;
    let mut stroke = Stroke::new(ICON_STROKE);
    stroke.cap(Cap::Round).join(Join::Round);
    let (data, placement) = zeno::Mask::new(ICON_PATH)
        .style(stroke)
        .transform(Some(
            Transform::scale(k, k).then_translate(origin[0], origin[1]),
        ))
        .render();
    Mask {
        left: placement.left,
        top: placement.top,
        width: placement.width,
        height: placement.height,
        data,
    }
}

/// The background pass: see `slate.wgsl`.
pub struct SlateBackgroundNode<'a> {
    params: &'a SlateParams,
    icon: &'a wgpu::TextureView,
}

impl<'a> SlateBackgroundNode<'a> {
    /// `icon` holds `params.icon`'s mask (`R8Unorm`, at least 1 x 1).
    pub fn new(params: &'a SlateParams, icon: &'a wgpu::TextureView) -> Self {
        Self { params, icon }
    }
}

impl Node for SlateBackgroundNode<'_> {
    fn label(&self) -> &str {
        "slate background"
    }

    fn input_count(&self) -> usize {
        0
    }

    fn outputs(&self, _inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![TextureDesc::working(self.params.size.0, self.params.size.1)]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![("scale", self.params.scale.to_string())]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        _inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let uniform = self.params.uniform();
        cx.draw(
            &Effect {
                label: "slate background",
                shader: "slate",
                entry: "fs_slate",
                uniform: uniform.bytes(),
                inputs: &[self.icon],
                filter: Filter::Nearest,
            },
            outputs[0],
        );
    }
}

/// A slate's laid-out parts, before anything touches the GPU.
struct Laid {
    params: SlateParams,
    geometry: SlateGeometry,
    texts: [(text::TextLayout, TextPaint); 3],
}

fn caption(family: &FontFamily, s: f32) -> TextFormat {
    TextFormat {
        family: family.clone(),
        size: CAPTION_SIZE * s,
        line_height: CAPTION_LINE / CAPTION_SIZE,
        ..Default::default()
    }
}

/// `name`, shortened in the middle with `…` until it fits `max_width`.
fn fit_middle(
    engine: &mut TextEngine,
    name: &str,
    format: &TextFormat,
    max_width: f32,
) -> (String, text::TextLayout) {
    let layout = engine.layout(name, format);
    if layout.width <= max_width {
        return (name.to_owned(), layout);
    }
    let chars: Vec<char> = name.chars().collect();
    // Keep the extension end a little longer than the start: it tells files apart.
    let mut keep = chars.len().saturating_sub(1);
    loop {
        let head = keep / 2;
        let tail = keep - head;
        let shown: String = chars[..head]
            .iter()
            .chain(std::iter::once(&'…'))
            .chain(&chars[chars.len() - tail..])
            .collect();
        let layout = engine.layout(&shown, format);
        if layout.width <= max_width || keep == 0 {
            return (shown, layout);
        }
        keep -= 1;
    }
}

fn lay_out(
    engine: &mut TextEngine,
    style: &SlateStyle,
    size: (u32, u32),
    label: &SlateLabel,
) -> Laid {
    let (w, h) = (size.0 as f32, size.1 as f32);
    let s = (w / DESIGN_SIZE.0).min(h / DESIGN_SIZE.1);
    let pal = &style.palette;
    let inset = (CARD_EDGE + CARD_PADDING) * s;
    let content_width = (CARD_WIDTH - 2.0 * (CARD_EDGE + CARD_PADDING)) * s;

    let title_format = TextFormat {
        family: style.ui_font.clone(),
        size: TITLE_SIZE * s,
        line_height: TITLE_LINE / TITLE_SIZE,
        weight: TITLE_WEIGHT,
        ..Default::default()
    };
    let title = engine.layout(&label.title, &title_format);
    let file_format = caption(&style.mono_font, s);
    let (file_name_shown, file) = fit_middle(engine, &label.file_name, &file_format, content_width);
    let detail_format = TextFormat {
        align: TextAlign::Center,
        max_width: Some(content_width),
        ..caption(&style.ui_font, s)
    };
    let detail = if label.detail.is_empty() {
        text::TextLayout::default()
    } else {
        engine.layout(&label.detail, &detail_format)
    };
    let detail_lines = detail.lines.len() as f32;

    let content_height = TITLE_LINE
        + ROW_GAP
        + CAPTION_LINE
        + if detail_lines > 0.0 {
            ROW_GAP + CAPTION_LINE * detail_lines
        } else {
            0.0
        };
    let card_w = CARD_WIDTH * s;
    let card_h = content_height * s + 2.0 * inset;
    let x0 = ((w - card_w) * 0.5).round();
    let y0 = ((h - card_h) * 0.5).round();
    let card = [x0, y0, x0 + card_w, y0 + card_h];
    let cx = x0 + card_w * 0.5;
    let top = y0 + inset;

    let row_width = (ICON_SIZE + ICON_GAP) * s + title.width;
    let icon_origin = [(cx - row_width * 0.5).round(), top.round()];
    let icon = icon_mask(icon_origin, ICON_SIZE * s);
    let title_origin = [icon_origin[0] + (ICON_SIZE + ICON_GAP) * s, top];
    let file_top = top + (TITLE_LINE + ROW_GAP) * s;
    let file_origin = [cx - file.width * 0.5, file_top];
    let detail_top = file_top + (CAPTION_LINE + ROW_GAP) * s;
    // Centred lines inside a box of the content width.
    let detail_origin = [cx - content_width * 0.5, detail_top];

    let bbox = |origin: [f32; 2], layout: &text::TextLayout, width: f32| {
        [
            origin[0],
            origin[1],
            origin[0] + width,
            origin[1] + layout.height,
        ]
    };
    let detail_box = if detail.lines.is_empty() {
        [cx, detail_top, cx, detail_top]
    } else {
        let widest = detail.width;
        [
            cx - widest * 0.5,
            detail_top,
            cx + widest * 0.5,
            detail_top + detail.height,
        ]
    };
    let geometry = SlateGeometry {
        size,
        scale: s,
        card,
        icon: [
            icon_origin[0],
            icon_origin[1],
            icon_origin[0] + ICON_SIZE * s,
            icon_origin[1] + ICON_SIZE * s,
        ],
        title: bbox(title_origin, &title, title.width),
        file_name: bbox(file_origin, &file, file.width),
        detail: detail_box,
        file_name_shown,
    };
    let paint = |origin, fill| TextPaint {
        origin,
        fill,
        outline: None,
        shadow: None,
    };
    Laid {
        params: SlateParams {
            size,
            scale: s,
            card,
            palette: pal.clone(),
            icon,
        },
        geometry,
        texts: [
            (title, paint(title_origin, pal.title)),
            (file, paint(file_origin, pal.file_name)),
            (detail, paint(detail_origin, pal.detail)),
        ],
    }
}

/// One rendered slate.
pub struct Slate {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    params: SlateParams,
    geometry: SlateGeometry,
}

impl Slate {
    /// Premultiplied linear light, `Rgba16Float`, opaque.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn params(&self) -> &SlateParams {
        &self.params
    }

    pub fn geometry(&self) -> &SlateGeometry {
        &self.geometry
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SlateKey {
    size: (u32, u32),
    label: SlateLabel,
}

/// Rendered slates by canvas size and label (see the module docs). One per render
/// thread, with its own glyph atlas.
pub struct SlateCache {
    style: SlateStyle,
    atlas: Option<GlyphAtlas>,
    slates: HashMap<SlateKey, Slate>,
    renders: u64,
}

impl std::fmt::Debug for SlateCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlateCache")
            .field("slates", &self.slates.len())
            .field("renders", &self.renders)
            .finish_non_exhaustive()
    }
}

impl Default for SlateCache {
    fn default() -> Self {
        Self::new(SlateStyle::default())
    }
}

impl SlateCache {
    pub fn new(style: SlateStyle) -> Self {
        Self {
            style,
            atlas: None,
            slates: HashMap::new(),
            renders: 0,
        }
    }

    pub fn style(&self) -> &SlateStyle {
        &self.style
    }

    /// The slate for `size` and `label`, laid out and rendered (submitted on the
    /// renderer's queue) the first time it is asked for. A zero size is taken as 1.
    pub fn prepare(
        &mut self,
        renderer: &Renderer,
        engine: &mut TextEngine,
        pool: &mut TexturePool,
        size: (u32, u32),
        label: &SlateLabel,
    ) -> &Slate {
        let key = SlateKey {
            size: (size.0.max(1), size.1.max(1)),
            label: label.clone(),
        };
        if !self.slates.contains_key(&key) {
            let slate = self.render(renderer, engine, pool, &key);
            self.renders += 1;
            self.slates.insert(key.clone(), slate);
        }
        &self.slates[&key]
    }

    /// An already rendered slate.
    pub fn get(&self, size: (u32, u32), label: &SlateLabel) -> Option<&Slate> {
        self.slates.get(&SlateKey {
            size: (size.0.max(1), size.1.max(1)),
            label: label.clone(),
        })
    }

    /// Imports a rendered slate into a frame's graph, as a source like a decoded frame;
    /// `None` when it has not been prepared.
    pub fn add_to_graph<'a>(
        &'a self,
        graph: &mut Graph<'a>,
        size: (u32, u32),
        label: &SlateLabel,
    ) -> Option<Port> {
        let slate = self.get(size, label)?;
        Some(graph.import("offline slate", slate.view()))
    }

    /// How many slates were laid out and rendered.
    pub fn renders(&self) -> u64 {
        self.renders
    }

    pub fn len(&self) -> usize {
        self.slates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slates.is_empty()
    }

    /// Drops every slate whose label is not wanted any more (a relinked file).
    pub fn retain(&mut self, mut keep: impl FnMut((u32, u32), &SlateLabel) -> bool) {
        self.slates.retain(|k, _| keep(k.size, &k.label));
    }

    fn render(
        &mut self,
        renderer: &Renderer,
        engine: &mut TextEngine,
        pool: &mut TexturePool,
        key: &SlateKey,
    ) -> Slate {
        let device = renderer.device();
        let laid = lay_out(engine, &self.style, key.size, &key.label);
        let atlas = self
            .atlas
            .get_or_insert_with(|| GlyphAtlas::new(device, renderer.queue()));
        atlas.begin_frame();
        let prepared: Vec<_> = laid
            .texts
            .iter()
            .filter(|(layout, _)| !layout.glyphs.is_empty())
            .map(|(layout, paint)| atlas.prepare(engine, layout, paint, None))
            .collect();
        let atlas = &*atlas;

        let icon = &laid.params.icon;
        let icon_size = (icon.width.max(1), icon.height.max(1));
        let icon_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("slate icon"),
            size: wgpu::Extent3d {
                width: icon_size.0,
                height: icon_size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        if !icon.is_empty() {
            renderer.queue().write_texture(
                icon_texture.as_image_copy(),
                &icon.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(icon.width),
                    rows_per_image: Some(icon.height),
                },
                icon_texture.size(),
            );
        }
        let icon_view = icon_texture.create_view(&Default::default());

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offline slate"),
            size: wgpu::Extent3d {
                width: key.size.0,
                height: key.size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TextureDesc::WORKING_FORMAT,
            usage: POOL_USAGE,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        {
            let mut g = Graph::new();
            let background = g
                .add(SlateBackgroundNode::new(&laid.params, &icon_view), &[])
                .out(0);
            let layers = prepared
                .iter()
                .map(|p| {
                    (
                        text::add_to_graph(&mut g, atlas, p, key.size),
                        Layer::default(),
                    )
                })
                .collect();
            let out =
                composite_stack(&mut g, key.size, Some(background), layers).unwrap_or(background);
            g.export(out, &view);
            match g.compile() {
                Ok(compiled) => {
                    compiled.execute(renderer, pool, None);
                }
                Err(e) => tracing::error!("the offline slate graph does not compile: {e}"),
            }
        }
        Slate {
            texture,
            view,
            params: laid.params,
            geometry: laid.geometry,
        }
    }
}
