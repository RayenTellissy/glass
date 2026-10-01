//! Painting helpers shared by the panels: the design's glass controls, pills, progress
//! bars, checkboxes and the small line icons, drawn straight with the painter so long
//! lists stay cheap. Colours always come from the theme ([`Paint::c`]).

use std::sync::Arc;

use egui::epaint::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Galley, Painter, Pos2, Rect, Response,
    Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::theme::{Role, Theme};

/// Component sizes from the boards that have no token of their own.
pub mod size {
    /// Panel header and list rows (the `row/pro` token's value).
    pub const ROW: f32 = 28.0;
    /// Small controls in headers and toolbars.
    pub const SMALL_CONTROL: f32 = 20.0;
    /// Pills and badges on thumbnails.
    pub const BADGE: f32 = 16.0;
    /// Dialog header and footer heights.
    pub const DIALOG_HEADER: f32 = 48.0;
    pub const DIALOG_FOOTER: f32 = 56.0;
    /// Dialog buttons.
    pub const DIALOG_BUTTON: f32 = 32.0;
    /// Checkbox and radio size.
    pub const CHECK: f32 = 16.0;
}

/// A type-scale step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Body,
    BodyMedium,
    Label,
    LabelMedium,
    Caption,
    /// Caption size in capitals with the caps tracking (section labels).
    Caps,
    Mono,
    MonoCaption,
    Title,
    Display,
}

/// Theme colours and type styles for one frame of painting.
#[derive(Clone)]
pub struct Paint {
    pub theme: Arc<Theme>,
}

impl Paint {
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            theme: crate::theme::current(ctx),
        }
    }

    pub fn c(&self, role: Role) -> Color32 {
        self.theme.color(role)
    }

    /// `color-mix(role N%, transparent)`.
    pub fn mix(&self, role: Role, amount: f32) -> Color32 {
        self.c(role).gamma_multiply(amount)
    }

    pub fn font(&self, ty: Type) -> FontId {
        let ts = &self.theme.tokens().type_scale;
        match ty {
            Type::Body => self.theme.font(&ts.body),
            Type::BodyMedium => FontId::new(ts.body.size_px, crate::fonts::medium()),
            Type::Label => self.theme.font(&ts.label),
            Type::LabelMedium => FontId::new(ts.label.size_px, crate::fonts::medium()),
            Type::Caption | Type::Caps => self.theme.font(&ts.caption),
            Type::Mono => self.theme.font(&ts.mono),
            Type::MonoCaption => FontId::new(ts.caption.size_px, FontFamily::Monospace),
            Type::Title => self.theme.font(&ts.title),
            Type::Display => self.theme.font(&ts.display),
        }
    }

    pub fn line_height(&self, ty: Type) -> f32 {
        let ts = &self.theme.tokens().type_scale;
        match ty {
            Type::Body | Type::BodyMedium => ts.body.line_height_px,
            Type::Label | Type::LabelMedium => ts.label.line_height_px,
            Type::Caption | Type::Caps | Type::MonoCaption => ts.caption.line_height_px,
            Type::Mono => ts.mono.line_height_px,
            Type::Title => ts.title.line_height_px,
            Type::Display => ts.display.line_height_px,
        }
    }

    pub fn s(&self) -> &assets::theme::Spacing {
        &self.theme.tokens().spacing
    }

    pub fn control_radius(&self) -> CornerRadius {
        radius(self.theme.tokens().radius.control)
    }

    pub fn floating_radius(&self) -> CornerRadius {
        radius(self.theme.tokens().radius.floating)
    }

    /// Lays out one line, cut with an ellipsis at `max_width`.
    pub fn galley(
        &self,
        painter: &Painter,
        text: &str,
        ty: Type,
        color: Color32,
        max_width: f32,
    ) -> Arc<Galley> {
        let (text, spacing) = if ty == Type::Caps {
            let ts = &self.theme.tokens().type_scale.caption;
            let em = ts.caps_letter_spacing_em.unwrap_or(0.06);
            (text.to_uppercase(), em * ts.size_px)
        } else {
            (text.to_owned(), 0.0)
        };
        let mut format = TextFormat::simple(self.font(ty), color);
        format.extra_letter_spacing = spacing;
        let mut job = LayoutJob::single_section(text, format);
        job.wrap = TextWrapping::truncate_at_width(max_width.max(1.0));
        job.break_on_newline = false;
        painter.layout_job(job)
    }

    /// Lays out wrapped text up to `max_width`, centred when `centred`.
    pub fn wrapped(
        &self,
        painter: &Painter,
        text: &str,
        ty: Type,
        color: Color32,
        max_width: f32,
        centred: bool,
    ) -> Arc<Galley> {
        let mut job =
            LayoutJob::single_section(text.to_owned(), TextFormat::simple(self.font(ty), color));
        job.wrap = TextWrapping::wrap_at_width(max_width.max(1.0));
        if centred {
            job.halign = egui::Align::Center;
        }
        painter.layout_job(job)
    }

    /// Draws one line of text with `align` at `pos`, cut at `max_width`; returns its
    /// rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &self,
        painter: &Painter,
        pos: Pos2,
        align: Align2,
        text: &str,
        ty: Type,
        role: Role,
        max_width: f32,
    ) -> Rect {
        self.text_c(painter, pos, align, text, ty, self.c(role), max_width)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text_c(
        &self,
        painter: &Painter,
        pos: Pos2,
        align: Align2,
        text: &str,
        ty: Type,
        color: Color32,
        max_width: f32,
    ) -> Rect {
        let galley = self.galley(painter, text, ty, color, max_width);
        let rect = align.anchor_size(pos, galley.size());
        painter.galley(rect.min, galley, color);
        rect
    }

    /// Width of one line of text.
    pub fn width(&self, painter: &Painter, text: &str, ty: Type) -> f32 {
        self.galley(painter, text, ty, Color32::TRANSPARENT, f32::INFINITY)
            .size()
            .x
    }

    /// The selected-row fill: `bg/3` with the glass sheen and inset edge.
    pub fn glass_fill(&self, painter: &Painter, rect: Rect, corner: CornerRadius) {
        painter.rect_filled(rect, corner, self.c(Role::Bg3));
        self.glass_sheen(painter, rect, corner);
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, self.c(Role::GlassEdge)),
            StrokeKind::Inside,
        );
    }

    /// The glass gradient (`glass/top` to `glass/bottom`) and its 1 px top highlight.
    pub fn glass_sheen(&self, painter: &Painter, rect: Rect, corner: CornerRadius) {
        let top = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.center().y));
        painter.rect_filled(
            top,
            CornerRadius {
                nw: corner.nw,
                ne: corner.ne,
                sw: 0,
                se: 0,
            },
            self.c(Role::GlassTop),
        );
        let bottom = Rect::from_min_max(pos2(rect.min.x, rect.center().y), rect.max);
        painter.rect_filled(
            bottom,
            CornerRadius {
                nw: 0,
                ne: 0,
                sw: corner.sw,
                se: corner.se,
            },
            self.c(Role::GlassBottom),
        );
        let inset = f32::from(corner.nw.max(corner.ne)).min(rect.width() / 2.0);
        painter.line_segment(
            [
                pos2(rect.min.x + inset, rect.min.y + 0.5),
                pos2(rect.max.x - inset, rect.min.y + 0.5),
            ],
            Stroke::new(1.0, self.c(Role::GlassHighlight)),
        );
    }

    /// A secondary glass button or pill outline: sheen plus a `glass/edge` border.
    pub fn glass_outline(&self, painter: &Painter, rect: Rect, corner: CornerRadius) {
        self.glass_sheen(painter, rect, corner);
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, self.c(Role::GlassEdge)),
            StrokeKind::Inside,
        );
    }

    /// A floating surface (dialogs, menus, popovers): `bg/2`, sheen, edge, shadow.
    pub fn floating(&self, painter: &Painter, rect: Rect) {
        let corner = self.floating_radius();
        let shadow = &self.theme.tokens().rules.floating_shadow;
        painter.add(
            egui::epaint::Shadow {
                offset: [0, shadow.offset_y_px.round().clamp(-128.0, 127.0) as i8],
                blur: shadow.blur_px.round().clamp(0.0, 255.0) as u8,
                spread: 0,
                color: self.c(Role::GlassShadow),
            }
            .as_shape(rect, corner),
        );
        painter.rect_filled(rect, corner, self.c(Role::Bg2));
        self.glass_outline(painter, rect, corner);
    }

    /// A 4 px (or `rect`-high) progress bar.
    pub fn progress(&self, painter: &Painter, rect: Rect, fraction: f32, track: Role) {
        let pill = CornerRadius::same(255);
        painter.rect_filled(rect, pill, self.c(track));
        let fraction = fraction.clamp(0.0, 1.0);
        if fraction > 0.0 {
            let fill = Rect::from_min_size(
                rect.min,
                vec2(
                    (rect.width() * fraction).round().max(rect.height()),
                    rect.height(),
                ),
            );
            painter.rect_filled(fill, pill, self.c(Role::Accent));
        }
    }

    pub fn dot(&self, painter: &Painter, center: Pos2, diameter: f32, color: Color32) {
        painter.circle_filled(center, diameter / 2.0, color);
    }

    /// A checkbox: accent square with a check when on, `bg/2` with a `border/strong` ring
    /// when off.
    pub fn checkbox(&self, painter: &Painter, rect: Rect, on: bool) {
        let corner = self.control_radius();
        if on {
            painter.rect_filled(rect, corner, self.c(Role::Accent));
            icon(
                painter,
                rect.shrink(3.0),
                Icon::Check,
                self.c(Role::TextInverse),
                2.8,
            );
        } else {
            painter.rect_filled(rect, corner, self.c(Role::Bg2));
            painter.rect_stroke(
                rect,
                corner,
                Stroke::new(1.0, self.c(Role::BorderStrong)),
                StrokeKind::Inside,
            );
        }
    }

    /// A radio button: accent disc with a 6 px inverse centre when on.
    pub fn radio(&self, painter: &Painter, rect: Rect, on: bool) {
        let r = rect.width() / 2.0;
        if on {
            painter.circle_filled(rect.center(), r, self.c(Role::Accent));
            painter.circle_filled(rect.center(), 3.0, self.c(Role::TextInverse));
        } else {
            painter.circle_filled(rect.center(), r, self.c(Role::Bg2));
            painter.circle_stroke(
                rect.center(),
                r - 0.5,
                Stroke::new(1.0, self.c(Role::BorderStrong)),
            );
        }
    }

    /// The 24 x 14 switch.
    pub fn switch(&self, painter: &Painter, rect: Rect, on: bool) {
        let pill = CornerRadius::same(255);
        if on {
            painter.rect_filled(rect, pill, self.c(Role::Accent));
            let knob = pos2(rect.max.x - 7.0, rect.center().y);
            painter.circle_filled(knob, 5.0, self.c(Role::TextInverse));
        } else {
            painter.rect_filled(rect, pill, self.c(Role::Bg3));
            painter.rect_stroke(
                rect,
                pill,
                Stroke::new(1.0, self.c(Role::BorderStrong)),
                StrokeKind::Inside,
            );
            let knob = pos2(rect.min.x + 7.0, rect.center().y);
            painter.circle_filled(knob, 5.0, self.c(Role::TextSecondary));
        }
    }

    /// A pill with a leading dot (status pills: `Offline · in 3 edits`).
    pub fn status_pill(&self, painter: &Painter, left_center: Pos2, text: &str, dot: Role) -> Rect {
        let width = self.width(painter, text, Type::Caption);
        let rect = Rect::from_min_size(
            left_center - vec2(0.0, size::SMALL_CONTROL / 2.0),
            vec2(8.0 + 6.0 + 8.0 + width + 8.0, size::SMALL_CONTROL),
        );
        let pill = CornerRadius::same(255);
        self.glass_outline(painter, rect, pill);
        self.dot(
            painter,
            pos2(rect.min.x + 11.0, rect.center().y),
            6.0,
            self.c(dot),
        );
        self.text(
            painter,
            pos2(rect.min.x + 22.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            Type::Caption,
            Role::TextSecondary,
            width + 1.0,
        );
        rect
    }

    /// A badge on a thumbnail: `bg/0` at 70% behind caption text.
    pub fn badge(
        &self,
        painter: &Painter,
        anchor: Pos2,
        align: Align2,
        text: &str,
        ty: Type,
    ) -> Rect {
        let galley = self.galley(painter, text, ty, self.c(Role::TextPrimary), f32::INFINITY);
        let rect = align.anchor_size(anchor, vec2(galley.size().x + 8.0, size::BADGE));
        painter.rect_filled(rect, self.control_radius(), self.mix(Role::Bg0, 0.7));
        painter.galley(
            pos2(rect.min.x + 4.0, rect.center().y - galley.size().y / 2.0),
            galley,
            self.c(Role::TextPrimary),
        );
        rect
    }

    /// A tag chip (glass outline pill with caption text); returns its rectangle.
    pub fn chip(&self, painter: &Painter, left_center: Pos2, text: &str, max_width: f32) -> Rect {
        let galley = self.galley(
            painter,
            text,
            Type::Caption,
            self.c(Role::TextPrimary),
            (max_width - 12.0).max(1.0),
        );
        let rect = Rect::from_min_size(
            left_center - vec2(0.0, size::BADGE / 2.0),
            vec2(galley.size().x + 12.0, size::BADGE),
        );
        self.glass_outline(painter, rect, CornerRadius::same(255));
        painter.galley(
            pos2(rect.min.x + 6.0, rect.center().y - galley.size().y / 2.0),
            galley,
            self.c(Role::TextPrimary),
        );
        rect
    }

    /// The design's halftone screen: `fill` everywhere except round holes on a 5 pt
    /// grid (`cell`) through which whatever was painted underneath shows. One textured
    /// quad, the tile cached per colour.
    pub fn halftone(&self, painter: &Painter, rect: Rect, cell: f32, fill: Color32) {
        let ctx = painter.ctx();
        let id = egui::Id::new(("glass.halftone", fill.to_array()));
        let texture = ctx
            .data_mut(|d| d.get_temp::<egui::TextureHandle>(id))
            .unwrap_or_else(|| {
                let texture = ctx.load_texture(
                    "halftone",
                    halftone_tile(fill),
                    egui::TextureOptions::LINEAR_REPEAT,
                );
                ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
                texture
            });
        let mut mesh = egui::Mesh::with_texture(texture.id());
        let uv = Rect::from_min_max(
            pos2(0.0, 0.0),
            pos2(rect.width() / cell, rect.height() / cell),
        );
        mesh.add_rect_with_uv(rect, uv, crate::theme::TEXTURE_TINT);
        painter.add(Shape::mesh(mesh));
    }

    /// A soft round glow (the blurred colour blobs behind offline slates): `color` at
    /// the centre fading to nothing at `radii`.
    pub fn glow(&self, painter: &Painter, center: Pos2, radii: egui::Vec2, color: Color32) {
        const RIM: usize = 32;
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(center, color);
        for i in 0..RIM {
            let a = i as f32 / RIM as f32 * std::f32::consts::TAU;
            let mid = center + vec2(a.cos() * radii.x, a.sin() * radii.y) * 0.5;
            mesh.colored_vertex(mid, color.gamma_multiply(0.5));
        }
        for i in 0..RIM {
            let a = i as f32 / RIM as f32 * std::f32::consts::TAU;
            mesh.colored_vertex(
                center + vec2(a.cos() * radii.x, a.sin() * radii.y),
                Color32::TRANSPARENT,
            );
        }
        let r = RIM as u32;
        for i in 0..r {
            let (a, b) = (1 + i, 1 + (i + 1) % r);
            mesh.add_triangle(0, a, b);
            mesh.add_triangle(a, b, a + r);
            mesh.add_triangle(b, b + r, a + r);
        }
        painter.add(Shape::mesh(mesh));
    }

    /// A button: primary (accent fill, inverse label) or secondary (glass outline).
    pub fn button(
        &self,
        ui: &mut Ui,
        rect: Rect,
        label: &str,
        primary: bool,
        enabled: bool,
    ) -> Response {
        let response = ui.allocate_rect(
            rect,
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let painter = ui.painter();
        let corner = self.control_radius();
        let opacity = if enabled {
            1.0
        } else {
            self.theme.tokens().opacity.disabled
        };
        let (text_role, ty) = if primary {
            painter.rect_filled(rect, corner, self.c(Role::Accent).gamma_multiply(opacity));
            (Role::TextInverse, Type::LabelMedium)
        } else {
            if response.hovered() && enabled {
                painter.rect_filled(rect, corner, self.c(Role::Bg3));
            }
            self.glass_outline(painter, rect, corner);
            (Role::TextPrimary, Type::LabelMedium)
        };
        self.text_c(
            painter,
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            ty,
            self.c(text_role).gamma_multiply(opacity),
            rect.width(),
        );
        response
    }

    /// The boards' spinner in `rect`: a `bg/3` ring with a quarter arc in the accent that
    /// turns once a second (asks for the next frame).
    pub fn spinner(&self, painter: &Painter, rect: Rect) {
        const STEPS: usize = 24;
        let k = rect.width() / 24.0;
        let width = 3.0 * k;
        painter.circle_stroke(
            rect.center(),
            9.0 * k,
            Stroke::new(width, self.c(Role::Bg3)),
        );
        let turn = (painter.ctx().input(|i| i.time) as f32 * std::f32::consts::TAU)
            % std::f32::consts::TAU;
        let (a0, a1) = (turn - std::f32::consts::FRAC_PI_2, turn);
        let points = (0..=STEPS)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / STEPS as f32;
                pos2(
                    rect.min.x + (12.0 + 9.0 * a.cos()) * k,
                    rect.min.y + (12.0 + 9.0 * a.sin()) * k,
                )
            })
            .collect();
        painter.add(Shape::line(
            points,
            Stroke::new(width, self.c(Role::Accent)),
        ));
        painter.ctx().request_repaint();
    }

    /// A text-only button (`Change…`, `Stop`, `Leave offline`).
    pub fn ghost_button(&self, ui: &mut Ui, rect: Rect, label: &str) -> Response {
        let response = ui.allocate_rect(rect, Sense::click());
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, self.control_radius(), self.c(Role::Bg3));
        }
        self.text(
            ui.painter(),
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            Type::LabelMedium,
            Role::TextSecondary,
            rect.width(),
        );
        response
    }
}

/// One halftone cell at 4 px per point: `fill` with an anti-aliased round hole of
/// radius 1.7 in a 5-unit cell, as in the design's pattern.
fn halftone_tile(fill: Color32) -> egui::ColorImage {
    const N: usize = 20;
    let scale = N as f32 / 5.0;
    let mut pixels = Vec::with_capacity(N * N);
    for y in 0..N {
        for x in 0..N {
            let dx = (x as f32 + 0.5) / scale - 2.5;
            let dy = (y as f32 + 0.5) / scale - 2.5;
            let d = (dx * dx + dy * dy).sqrt();
            let coverage = ((d - 1.7) * scale + 0.5).clamp(0.0, 1.0);
            pixels.push(fill.gamma_multiply(coverage));
        }
    }
    egui::ColorImage::new([N, N], pixels)
}

pub fn radius(px: f32) -> CornerRadius {
    CornerRadius::same(px.round().clamp(0.0, 255.0) as u8)
}

/// The line icons the panels use, drawn on a 24-unit grid like the design's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Search,
    Close,
    Check,
    Folder,
    List,
    Grid,
    ChevronDown,
    ChevronRight,
    Plus,
    Filter,
    WifiOff,
    Alert,
    Info,
    Film,
    Music,
    Image,
    Import,
    More,
}

fn arc(center: Pos2, r: f32, from_deg: f32, to_deg: f32) -> Vec<Pos2> {
    let steps = 10;
    (0..=steps)
        .map(|i| {
            let a = (from_deg + (to_deg - from_deg) * i as f32 / steps as f32).to_radians();
            center + vec2(a.cos(), a.sin()) * r
        })
        .collect()
}

/// Draws `icon` into `rect` (square) with the design's stroke width on its 24 grid.
pub fn icon(painter: &Painter, rect: Rect, icon: Icon, color: Color32, stroke_24: f32) {
    let s = rect.width().min(rect.height()) / 24.0;
    let o = rect.center() - vec2(12.0, 12.0) * s;
    let p = |x: f32, y: f32| o + vec2(x, y) * s;
    let stroke = Stroke::new(stroke_24 * s, color);
    let line = |pts: Vec<Pos2>| Shape::line(pts, stroke);
    let shapes: Vec<Shape> = match icon {
        Icon::Search => vec![
            Shape::circle_stroke(p(11.0, 11.0), 8.0 * s, stroke),
            line(vec![p(21.0, 21.0), p(16.7, 16.7)]),
        ],
        Icon::Close => vec![
            line(vec![p(18.0, 6.0), p(6.0, 18.0)]),
            line(vec![p(6.0, 6.0), p(18.0, 18.0)]),
        ],
        Icon::Check => vec![line(vec![p(20.0, 6.0), p(9.0, 17.0), p(4.0, 12.0)])],
        Icon::Folder => vec![Shape::closed_line(
            vec![
                p(2.0, 5.0),
                p(4.0, 3.0),
                p(8.0, 3.0),
                p(10.4, 5.1),
                p(20.0, 6.0),
                p(22.0, 8.0),
                p(22.0, 18.0),
                p(20.0, 20.0),
                p(4.0, 20.0),
                p(2.0, 18.0),
            ],
            stroke,
        )],
        Icon::List => {
            let mut v = Vec::new();
            for y in [6.0, 12.0, 18.0] {
                v.push(Shape::circle_filled(p(3.5, y), stroke.width * 0.6, color));
                v.push(line(vec![p(8.0, y), p(21.0, y)]));
            }
            v
        }
        Icon::Grid => [(3.0, 3.0), (14.0, 3.0), (14.0, 14.0), (3.0, 14.0)]
            .iter()
            .map(|(x, y)| {
                Shape::rect_stroke(
                    Rect::from_min_size(p(*x, *y), vec2(7.0, 7.0) * s),
                    CornerRadius::same(1),
                    stroke,
                    StrokeKind::Middle,
                )
            })
            .collect(),
        Icon::ChevronDown => vec![line(vec![p(6.0, 9.0), p(12.0, 15.0), p(18.0, 9.0)])],
        Icon::ChevronRight => vec![line(vec![p(9.0, 6.0), p(15.0, 12.0), p(9.0, 18.0)])],
        Icon::Plus => vec![
            line(vec![p(12.0, 5.0), p(12.0, 19.0)]),
            line(vec![p(5.0, 12.0), p(19.0, 12.0)]),
        ],
        Icon::Filter => vec![Shape::closed_line(
            vec![
                p(3.0, 4.0),
                p(21.0, 4.0),
                p(14.0, 12.5),
                p(14.0, 20.0),
                p(10.0, 18.0),
                p(10.0, 12.5),
            ],
            stroke,
        )],
        Icon::WifiOff => vec![
            Shape::circle_filled(p(12.0, 20.0), stroke.width * 0.6, color),
            line(arc(p(12.0, 20.0), 5.0, 225.0, 315.0)),
            line(arc(p(12.0, 20.0), 10.0, 222.0, 262.0)),
            line(arc(p(12.0, 20.0), 10.0, 290.0, 318.0)),
            line(arc(p(12.0, 22.0), 15.5, 228.0, 256.0)),
            line(arc(p(12.0, 22.0), 15.5, 272.0, 312.0)),
            line(vec![p(2.0, 2.0), p(22.0, 22.0)]),
        ],
        Icon::Alert => vec![
            Shape::closed_line(vec![p(12.0, 3.0), p(22.0, 20.5), p(2.0, 20.5)], stroke),
            line(vec![p(12.0, 9.0), p(12.0, 13.0)]),
            Shape::circle_filled(p(12.0, 17.0), stroke.width * 0.6, color),
        ],
        Icon::Info => vec![
            Shape::circle_stroke(p(12.0, 12.0), 10.0 * s, stroke),
            line(vec![p(12.0, 8.0), p(12.0, 12.0)]),
            Shape::circle_filled(p(12.0, 16.0), stroke.width * 0.6, color),
        ],
        Icon::Film => vec![
            Shape::rect_stroke(
                Rect::from_min_max(p(3.0, 3.0), p(21.0, 21.0)),
                CornerRadius::same((2.0 * s) as u8),
                stroke,
                StrokeKind::Middle,
            ),
            line(vec![p(7.0, 3.0), p(7.0, 21.0)]),
            line(vec![p(17.0, 3.0), p(17.0, 21.0)]),
            line(vec![p(3.0, 12.0), p(21.0, 12.0)]),
        ],
        Icon::Music => vec![
            line(vec![p(9.0, 18.0), p(9.0, 5.0), p(21.0, 3.0), p(21.0, 16.0)]),
            Shape::circle_stroke(p(6.0, 18.0), 3.0 * s, stroke),
            Shape::circle_stroke(p(18.0, 16.0), 3.0 * s, stroke),
        ],
        Icon::Image => vec![
            Shape::rect_stroke(
                Rect::from_min_max(p(3.0, 3.0), p(21.0, 21.0)),
                CornerRadius::same((2.0 * s) as u8),
                stroke,
                StrokeKind::Middle,
            ),
            Shape::circle_stroke(p(9.0, 9.0), 2.0 * s, stroke),
            line(vec![p(21.0, 15.0), p(16.0, 10.0), p(5.0, 21.0)]),
        ],
        Icon::Import => vec![
            line(vec![p(12.0, 3.0), p(12.0, 15.0)]),
            line(vec![p(7.0, 10.0), p(12.0, 15.0), p(17.0, 10.0)]),
            line(vec![
                p(21.0, 15.0),
                p(21.0, 19.0),
                p(19.0, 21.0),
                p(5.0, 21.0),
                p(3.0, 19.0),
                p(3.0, 15.0),
            ]),
        ],
        Icon::More => [4.0, 12.0, 20.0]
            .iter()
            .map(|x| Shape::circle_stroke(p(*x, 12.0), 1.0 * s, stroke))
            .collect(),
    };
    painter.extend(shapes);
}

/// An icon-only button of `size`; the fill appears on hover or when `on`.
pub fn icon_button(paint: &Paint, ui: &mut Ui, rect: Rect, which: Icon, on: bool) -> Response {
    let response = ui.allocate_rect(rect, Sense::click());
    let painter = ui.painter();
    if on {
        paint.glass_fill(painter, rect, paint.control_radius());
    } else if response.hovered() {
        painter.rect_filled(rect, paint.control_radius(), paint.c(Role::Bg3));
    }
    let role = if on {
        Role::TextPrimary
    } else {
        Role::TextSecondary
    };
    icon(
        painter,
        Rect::from_center_size(rect.center(), vec2(14.0, 14.0)),
        which,
        paint.c(role),
        2.4,
    );
    response
}
