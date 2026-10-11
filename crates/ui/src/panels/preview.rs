//! The preview panel: the frame playback presents, fitted to the panel at the
//! sequence's aspect over `bg/0`, with a position strip and the transport under it
//! (go to start, frame back, play or pause, frame forward, go to end) between the
//! playhead's timecode and the sequence's length.
//!
//! The picture is a texture the app registers with the egui renderer from
//! `engine::timeline::PresentedFrame`; the panel never waits for one and draws the
//! last frame it was given. Clicking or dragging the position strip seeks.

use egui::{Align2, Id, Pos2, Rect, Sense, Shape, Stroke, TextureId, Ui, pos2, vec2};

use super::widgets::{Paint, Type, size};
use crate::t;
use crate::theme::{Role, TEXTURE_TINT};

/// The transport bar's height under the picture.
pub const TRANSPORT_HEIGHT: f32 = size::ROW + 12.0;
/// The position strip's height on top of the transport.
const STRIP: f32 = 4.0;
const BUTTON: f32 = size::SMALL_CONTROL + 4.0;
const GLYPH: f32 = 10.0;

/// What the panel shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreviewView {
    /// The presented frame and its size in pixels.
    pub picture: Option<(TextureId, [u32; 2])>,
    /// The sequence's canvas, which sets the picture's aspect while no frame is shown.
    pub canvas: (u32, u32),
    /// The playhead and the sequence's length in frames.
    pub frame: i64,
    pub end: i64,
    /// The playhead's and the length's timecode, formatted by the caller.
    pub timecode: String,
    pub duration: String,
    pub playing: bool,
    /// No audio device: playback runs on the system clock without sound.
    pub silent: bool,
    /// No sequence is open.
    pub empty: bool,
}

/// What the user did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewAction {
    None,
    TogglePlay,
    GoToStart,
    GoToEnd,
    FrameBack,
    FrameForward,
    /// Seek to this frame (the position strip).
    Seek(i64),
}

/// The picture's rectangle in `area` for a frame of `size`: as large as fits, centred.
pub fn fit(area: Rect, size: (u32, u32)) -> Rect {
    let (w, h) = (size.0.max(1) as f32, size.1.max(1) as f32);
    let scale = (area.width() / w).min(area.height() / h).max(0.0);
    Rect::from_center_size(area.center(), vec2(w * scale, h * scale))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Glyph {
    Start,
    Back,
    Play,
    Pause,
    Forward,
    End,
}

fn triangle(painter: &egui::Painter, c: Pos2, w: f32, right: bool, color: egui::Color32) {
    let s = if right { 1.0 } else { -1.0 };
    painter.add(Shape::convex_polygon(
        vec![
            pos2(c.x - s * w / 2.0, c.y - w / 2.0),
            pos2(c.x + s * w / 2.0, c.y),
            pos2(c.x - s * w / 2.0, c.y + w / 2.0),
        ],
        color,
        Stroke::NONE,
    ));
}

fn glyph(painter: &egui::Painter, rect: Rect, which: Glyph, color: egui::Color32) {
    let c = rect.center();
    let g = GLYPH;
    let bar = |x: f32| {
        painter.rect_filled(
            Rect::from_center_size(pos2(x, c.y), vec2(2.0, g)),
            0.0,
            color,
        )
    };
    match which {
        Glyph::Play => triangle(painter, c, g, true, color),
        Glyph::Pause => {
            bar(c.x - 2.5);
            bar(c.x + 2.5);
        }
        Glyph::Back => triangle(painter, c, g * 0.8, false, color),
        Glyph::Forward => triangle(painter, c, g * 0.8, true, color),
        Glyph::Start => {
            bar(c.x - g / 2.0 + 1.0);
            triangle(painter, c + vec2(1.5, 0.0), g * 0.8, false, color);
        }
        Glyph::End => {
            bar(c.x + g / 2.0 - 1.0);
            triangle(painter, c - vec2(1.5, 0.0), g * 0.8, true, color);
        }
    }
}

fn button(ui: &mut Ui, paint: &Paint, rect: Rect, which: Glyph, label: String, on: bool) -> bool {
    let id = Id::new(("glass.preview.transport", which));
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    let painter = ui.painter();
    if on {
        painter.rect_filled(rect, paint.control_radius(), paint.c(Role::AccentSoft));
    } else if response.hovered() {
        painter.rect_filled(rect, paint.control_radius(), paint.c(Role::Bg3));
    }
    let role = if on { Role::Accent } else { Role::TextPrimary };
    glyph(painter, rect, which, paint.c(role));
    response.on_hover_text(label).clicked()
}

/// Draws the panel into `rect` and returns what the user did.
pub fn preview_panel(ui: &mut Ui, rect: Rect, view: &PreviewView) -> PreviewAction {
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    let area = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - TRANSPORT_HEIGHT));
    painter.rect_filled(area, 0.0, paint.c(Role::Bg0));
    let mut action = PreviewAction::None;

    if view.empty {
        paint.text(
            &painter,
            area.center(),
            Align2::CENTER_CENTER,
            &t!("preview.no_sequence"),
            Type::Body,
            Role::TextMuted,
            area.width(),
        );
    } else {
        match view.picture {
            Some((texture, size)) => {
                let picture = fit(area.shrink(paint.s().s8), (size[0], size[1]));
                painter.image(
                    texture,
                    picture,
                    Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                    TEXTURE_TINT,
                );
            }
            None => {
                let picture = fit(area.shrink(paint.s().s8), view.canvas);
                painter.rect_stroke(
                    picture,
                    0.0,
                    Stroke::new(1.0, paint.c(Role::BorderSubtle)),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    // The position strip: the played part in the accent, the playhead as a knob.
    let bar = Rect::from_min_max(pos2(rect.min.x, area.max.y), rect.max);
    painter.rect_filled(bar, 0.0, paint.c(Role::Bg1));
    let strip = Rect::from_min_size(bar.min, vec2(bar.width(), STRIP));
    let strip_hit = strip.expand2(vec2(0.0, 4.0));
    let response = ui.interact(
        strip_hit,
        Id::new("glass.preview.position"),
        Sense::click_and_drag(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, t!("preview.position"))
    });
    let end = view.end.max(1);
    if !view.empty
        && (response.clicked() || response.dragged())
        && let Some(p) = response.interact_pointer_pos()
    {
        let u = ((p.x - strip.left()) / strip.width().max(1.0)).clamp(0.0, 1.0);
        action = PreviewAction::Seek(((u * end as f32).round() as i64).min(end - 1).max(0));
    }
    let u = (view.frame as f32 / end as f32).clamp(0.0, 1.0);
    paint.progress(&painter, strip, u, Role::Bg3);
    if response.hovered() || response.dragged() {
        paint.dot(
            &painter,
            pos2(strip.left() + u * strip.width(), strip.center().y),
            10.0,
            paint.c(Role::TextPrimary),
        );
    }

    let row = Rect::from_min_max(pos2(bar.min.x, strip.max.y), bar.max);
    let pad = paint.s().s12;
    paint.text(
        &painter,
        pos2(row.left() + pad, row.center().y),
        Align2::LEFT_CENTER,
        &view.timecode,
        Type::Mono,
        Role::TextPrimary,
        row.width() / 3.0,
    );
    let right = paint.text(
        &painter,
        pos2(row.right() - pad, row.center().y),
        Align2::RIGHT_CENTER,
        &view.duration,
        Type::Mono,
        Role::TextMuted,
        row.width() / 3.0,
    );

    let buttons = [
        (
            Glyph::Start,
            t!("shortcuts.cmd.playback.go_to_start"),
            PreviewAction::GoToStart,
        ),
        (
            Glyph::Back,
            t!("shortcuts.cmd.playback.frame_back"),
            PreviewAction::FrameBack,
        ),
        if view.playing {
            (Glyph::Pause, t!("preview.pause"), PreviewAction::TogglePlay)
        } else {
            (Glyph::Play, t!("preview.play"), PreviewAction::TogglePlay)
        },
        (
            Glyph::Forward,
            t!("shortcuts.cmd.playback.frame_forward"),
            PreviewAction::FrameForward,
        ),
        (
            Glyph::End,
            t!("shortcuts.cmd.playback.go_to_end"),
            PreviewAction::GoToEnd,
        ),
    ];
    let gap = paint.s().s4;
    let total = buttons.len() as f32 * BUTTON + (buttons.len() - 1) as f32 * gap;
    let mut x = row.center().x - total / 2.0;
    if view.silent {
        // Between the transport and the duration, left out when it does not fit.
        let caption = t!("preview.no_audio");
        let end = right.left() - paint.s().s12;
        let room = end - (row.center().x + total / 2.0 + paint.s().s12);
        if paint.width(&painter, &caption, Type::Caption) <= room {
            paint.text(
                &painter,
                pos2(end, row.center().y),
                Align2::RIGHT_CENTER,
                &caption,
                Type::Caption,
                Role::TextMuted,
                room,
            );
        }
    }
    for (which, label, act) in buttons {
        let r = Rect::from_min_size(pos2(x, row.center().y - BUTTON / 2.0), vec2(BUTTON, BUTTON));
        let on = which == Glyph::Pause;
        if !view.empty && button(ui, &paint, r, which, label, on) {
            action = act;
        } else if view.empty {
            glyph(
                ui.painter(),
                r,
                which,
                paint.mix(Role::TextPrimary, paint.theme.tokens().opacity.disabled),
            );
        }
        x += BUTTON + gap;
    }
    action
}
