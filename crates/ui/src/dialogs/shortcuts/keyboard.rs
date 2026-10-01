//! The keyboard map on `Sheet / Keyboard`: an ANSI keyboard drawn at 44 pt per key,
//! each key showing what it runs in the modifier layer being looked at, coloured by the
//! command's group, with dots for changed and clashing keys.

use egui::{Align2, Id, Key, Painter, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::Index;
use super::glyphs::{Glyph, Token, draw_tokens, glyph, key_token};
use crate::panels::widgets::{Paint, Type};
use crate::shortcuts::{Chord, Mods, Platform, command};
use crate::t;
use crate::theme::Role;

/// Key size and gap from the board.
pub const KEY: f32 = 44.0;
pub const GAP: f32 = 4.0;
/// The map's height: five rows.
pub const HEIGHT: f32 = 5.0 * KEY + 4.0 * GAP;
/// The stacked up and down arrows are half height.
const HALF: f32 = (KEY - GAP) / 2.0;
const PAD_X: f32 = 5.0;
const PAD_Y: f32 = 4.0;

/// Which modifier a modifier key is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Modifier {
    Cmd,
    Ctrl,
    Alt,
    Shift,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A key commands can take.
    Bindable(Key),
    /// The up and down arrows, stacked in one slot.
    Stacked(Key, Key),
    Modifier(Modifier),
    /// Keys that never take a command here (fn, Caps Lock, Tab moves focus).
    Inert,
}

#[derive(Clone, Copy, Debug)]
struct Cap {
    kind: Kind,
    width: f32,
    label: Label,
}

#[derive(Clone, Copy, Debug)]
enum Label {
    /// The key's own token.
    Key,
    Glyph(Glyph),
    Fn,
    Space,
    Ctrl,
    Alt,
    Win,
}

const fn bind(key: Key) -> Cap {
    Cap {
        kind: Kind::Bindable(key),
        width: KEY,
        label: Label::Key,
    }
}

const fn wide(key: Key, width: f32) -> Cap {
    Cap {
        kind: Kind::Bindable(key),
        width,
        label: Label::Key,
    }
}

const fn modifier(m: Modifier, width: f32, label: Label) -> Cap {
    Cap {
        kind: Kind::Modifier(m),
        width,
        label,
    }
}

const fn inert(width: f32, label: Label) -> Cap {
    Cap {
        kind: Kind::Inert,
        width,
        label,
    }
}

const ROW_DIGITS: &[Cap] = &[
    bind(Key::Backtick),
    bind(Key::Num1),
    bind(Key::Num2),
    bind(Key::Num3),
    bind(Key::Num4),
    bind(Key::Num5),
    bind(Key::Num6),
    bind(Key::Num7),
    bind(Key::Num8),
    bind(Key::Num9),
    bind(Key::Num0),
    bind(Key::Minus),
    bind(Key::Equals),
    wide(Key::Backspace, 72.0),
];

const ROW_TOP: &[Cap] = &[
    inert(62.0, Label::Glyph(Glyph::Tab)),
    bind(Key::Q),
    bind(Key::W),
    bind(Key::E),
    bind(Key::R),
    bind(Key::T),
    bind(Key::Y),
    bind(Key::U),
    bind(Key::I),
    bind(Key::O),
    bind(Key::P),
    bind(Key::OpenBracket),
    bind(Key::CloseBracket),
    wide(Key::Backslash, 54.0),
];

const ROW_HOME: &[Cap] = &[
    inert(74.0, Label::Glyph(Glyph::CapsLock)),
    bind(Key::A),
    bind(Key::S),
    bind(Key::D),
    bind(Key::F),
    bind(Key::G),
    bind(Key::H),
    bind(Key::J),
    bind(Key::K),
    bind(Key::L),
    bind(Key::Semicolon),
    bind(Key::Quote),
    wide(Key::Enter, 90.0),
];

const ROW_BOTTOM: &[Cap] = &[
    modifier(Modifier::Shift, 96.0, Label::Glyph(Glyph::Shift)),
    bind(Key::Z),
    bind(Key::X),
    bind(Key::C),
    bind(Key::V),
    bind(Key::B),
    bind(Key::N),
    bind(Key::M),
    bind(Key::Comma),
    bind(Key::Period),
    bind(Key::Slash),
    modifier(Modifier::Shift, 116.0, Label::Glyph(Glyph::Shift)),
];

const ARROWS: [Cap; 3] = [
    bind(Key::ArrowLeft),
    Cap {
        kind: Kind::Stacked(Key::ArrowUp, Key::ArrowDown),
        width: KEY,
        label: Label::Key,
    },
    bind(Key::ArrowRight),
];

const SPACE: Cap = Cap {
    kind: Kind::Bindable(Key::Space),
    width: 240.0,
    label: Label::Space,
};

const ROW_MODS_MAC: &[Cap] = &[
    inert(KEY, Label::Fn),
    modifier(Modifier::Ctrl, KEY, Label::Glyph(Glyph::Control)),
    modifier(Modifier::Alt, KEY, Label::Glyph(Glyph::Option)),
    modifier(Modifier::Cmd, 56.0, Label::Glyph(Glyph::Command)),
    SPACE,
    modifier(Modifier::Cmd, 56.0, Label::Glyph(Glyph::Command)),
    modifier(Modifier::Alt, KEY, Label::Glyph(Glyph::Option)),
    ARROWS[0],
    ARROWS[1],
    ARROWS[2],
];

const ROW_MODS_PC: &[Cap] = &[
    modifier(Modifier::Cmd, KEY, Label::Ctrl),
    inert(KEY, Label::Fn),
    inert(KEY, Label::Win),
    modifier(Modifier::Alt, 56.0, Label::Alt),
    SPACE,
    modifier(Modifier::Alt, 56.0, Label::Alt),
    modifier(Modifier::Cmd, KEY, Label::Ctrl),
    ARROWS[0],
    ARROWS[1],
    ARROWS[2],
];

fn rows(platform: Platform) -> [&'static [Cap]; 5] {
    let mods = if platform.is_mac() {
        ROW_MODS_MAC
    } else {
        ROW_MODS_PC
    };
    [ROW_DIGITS, ROW_TOP, ROW_HOME, ROW_BOTTOM, mods]
}

/// The map's width (every row is the same width).
#[cfg(test)]
pub fn width(platform: Platform) -> f32 {
    let row = rows(platform)[0];
    row.iter().map(|c| c.width).sum::<f32>() + GAP * (row.len() - 1) as f32
}

fn held(m: Modifier, layer: Mods) -> bool {
    match m {
        Modifier::Cmd => layer.cmd,
        Modifier::Ctrl => layer.ctrl,
        Modifier::Alt => layer.alt,
        Modifier::Shift => layer.shift,
    }
}

/// What the map needs to know each frame.
pub struct MapState<'a> {
    pub platform: Platform,
    pub layer: Mods,
    pub selected_key: Option<Key>,
    pub index: &'a Index,
}

/// Draws the map with its top-left at `origin`; returns the key clicked, if any.
pub fn draw(ui: &mut Ui, paint: &Paint, origin: egui::Pos2, state: &MapState<'_>) -> Option<Key> {
    let painter = ui.painter().clone();
    let mut clicked = None;
    let mut y = origin.y;
    for row in rows(state.platform) {
        let mut x = origin.x;
        for cap in row {
            let rect = Rect::from_min_size(pos2(x, y), vec2(cap.width, KEY));
            match cap.kind {
                Kind::Stacked(up, down) => {
                    let top = Rect::from_min_size(rect.min, vec2(cap.width, HALF));
                    let bottom = Rect::from_min_max(pos2(rect.min.x, rect.max.y - HALF), rect.max);
                    for (r, key) in [(top, up), (bottom, down)] {
                        if draw_bindable(ui, paint, &painter, r, key, cap, state, true) {
                            clicked = Some(key);
                        }
                    }
                }
                Kind::Bindable(key) => {
                    if draw_bindable(ui, paint, &painter, rect, key, cap, state, false) {
                        clicked = Some(key);
                    }
                }
                Kind::Modifier(m) if held(m, state.layer) => {
                    let corner = paint.control_radius();
                    painter.rect_filled(rect, corner, paint.c(Role::Bg3));
                    paint.glass_sheen(&painter, rect, corner);
                    ring(paint, &painter, rect, Role::GlassEdge, 1.0);
                    label(
                        paint,
                        &painter,
                        rect,
                        cap,
                        state.platform,
                        Role::TextPrimary,
                    );
                }
                Kind::Modifier(_) | Kind::Inert => {
                    painter.rect_filled(rect, paint.control_radius(), paint.c(Role::Bg0));
                    ring(paint, &painter, rect, Role::BorderSubtle, 1.0);
                    label(paint, &painter, rect, cap, state.platform, Role::TextMuted);
                }
            }
            x += cap.width + GAP;
        }
        y += KEY + GAP;
    }
    clicked
}

fn ring(paint: &Paint, painter: &Painter, rect: Rect, role: Role, width: f32) {
    painter.rect_stroke(
        rect,
        paint.control_radius(),
        Stroke::new(width, paint.c(role)),
        StrokeKind::Inside,
    );
}

/// The key's own label at the top left.
fn label(paint: &Paint, painter: &Painter, rect: Rect, cap: &Cap, platform: Platform, role: Role) {
    let mid = rect.min.y + PAD_Y + paint.line_height(Type::MonoCaption) / 2.0;
    let left = rect.min.x + PAD_X;
    let color = paint.c(role);
    let text = |s: String| {
        paint.text_c(
            painter,
            pos2(left, mid),
            Align2::LEFT_CENTER,
            &s,
            Type::MonoCaption,
            color,
            rect.width() - PAD_X,
        );
    };
    match cap.label {
        Label::Key => {
            if let Kind::Bindable(key) = cap.kind {
                draw_tokens(
                    paint,
                    painter,
                    left,
                    mid,
                    &[key_token(key, platform)],
                    color,
                );
            }
        }
        Label::Glyph(g) => glyph(
            painter,
            Rect::from_min_size(pos2(left, mid - 5.0), vec2(10.0, 10.0)),
            g,
            color,
        ),
        Label::Fn => text(t!("shortcuts.key.fn")),
        Label::Space => text(t!("shortcuts.key.space_cap")),
        Label::Ctrl => text(t!("shortcuts.key.ctrl")),
        Label::Alt => text(t!("shortcuts.key.alt")),
        Label::Win => text(t!("shortcuts.key.win")),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bindable(
    ui: &mut Ui,
    paint: &Paint,
    painter: &Painter,
    rect: Rect,
    key: Key,
    cap: &Cap,
    state: &MapState<'_>,
    half: bool,
) -> bool {
    let response = ui.interact(rect, Id::new(("glass.shortcuts.key", key)), Sense::click());
    let chord = Chord {
        mods: state.layer,
        key,
    }
    .on(state.platform);
    let ids = state.index.on(chord);
    let bound = ids.first().and_then(|id| command(id));
    let selected = state.selected_key == Some(key);
    let corner = paint.control_radius();
    let key_role = if bound.is_some() || selected {
        Role::TextPrimary
    } else {
        Role::TextMuted
    };
    match bound {
        Some(c) => {
            let band = if selected { 3.0 } else { 2.0 };
            if selected {
                let shadow = egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 16,
                    spread: 0,
                    color: paint.c(Role::GlassShadow),
                };
                painter.add(shadow.as_shape(rect, corner));
            }
            painter.rect_filled(rect, corner, paint.c(c.group.role()));
            let face = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - band));
            let fill = if selected || response.hovered() {
                Role::Bg3
            } else {
                Role::Bg2
            };
            painter.rect_filled(face, corner, paint.c(fill));
            paint.glass_sheen(painter, face, corner);
            if selected {
                ring(paint, painter, rect, Role::Accent, 1.5);
            } else if response.hovered() {
                ring(paint, painter, rect, Role::BorderStrong, 1.0);
            } else {
                ring(paint, painter, rect, Role::GlassEdge, 1.0);
            }
        }
        None => {
            let fill = if response.hovered() {
                Role::Bg3
            } else {
                Role::Bg1
            };
            painter.rect_filled(rect, corner, paint.c(fill));
            if selected {
                ring(paint, painter, rect, Role::Accent, 1.5);
            } else {
                let edge = if response.hovered() {
                    Role::BorderStrong
                } else {
                    Role::BorderSubtle
                };
                ring(paint, painter, rect, edge, 1.0);
            }
        }
    }

    let color = paint.c(key_role);
    let token: Token = match cap.label {
        Label::Space => Token::Text(t!("shortcuts.key.space_cap")),
        _ => key_token(key, state.platform),
    };
    if half {
        let w = super::glyphs::tokens_width(paint, painter, std::slice::from_ref(&token));
        draw_tokens(
            paint,
            painter,
            rect.center().x - w / 2.0,
            rect.center().y,
            &[token],
            color,
        );
    } else {
        let mid = rect.min.y + PAD_Y + paint.line_height(Type::MonoCaption) / 2.0;
        draw_tokens(paint, painter, rect.min.x + PAD_X, mid, &[token], color);
        if let Some(c) = bound {
            paint.text(
                painter,
                pos2(rect.min.x + PAD_X, rect.max.y - PAD_Y),
                Align2::LEFT_BOTTOM,
                &c.cap(),
                Type::Caption,
                Role::TextSecondary,
                rect.width() - 4.0,
            );
        }
        // Changed and clash dots at the top right.
        let dot = if ids.iter().any(|id| state.index.is_clash(id, chord)) {
            Some(Role::StateWarning)
        } else if ids.iter().any(|id| state.index.is_changed(id)) {
            Some(Role::Accent)
        } else {
            None
        };
        if let Some(role) = dot {
            paint.dot(
                painter,
                pos2(rect.max.x - PAD_X - 3.0, mid),
                6.0,
                paint.c(role),
            );
        }
    }
    response.clicked()
}
