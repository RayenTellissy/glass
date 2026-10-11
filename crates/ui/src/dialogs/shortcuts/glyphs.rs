//! How chords read on screen: the macOS modifier and key symbols drawn as line icons
//! (the UI fonts may lack them), words on Windows and Linux, and the keycap chip that
//! holds a chord.

use egui::{Align2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

use crate::panels::widgets::{Paint, Type};
use crate::shortcuts::{Chord, Mods, Platform};
use crate::t;
use crate::theme::Role;

/// A symbol drawn as a line icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Command,
    Shift,
    Option,
    Control,
    Backspace,
    ForwardDelete,
    Tab,
    CapsLock,
    Return,
    Left,
    Right,
    Up,
    Down,
}

/// One piece of a chord's label.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Glyph(Glyph),
    Text(String),
}

/// Keycap chip height and the icon box inside one.
pub const CAP_H: f32 = 18.0;
const GLYPH: f32 = 10.0;
const GLYPH_GAP: f32 = 1.0;

/// The modifier tokens of `mods` in the platform's order (⌃⌥⇧⌘ on macOS, `Ctrl+Alt+
/// Shift+` elsewhere).
pub fn mod_tokens(mods: Mods, platform: Platform) -> Vec<Token> {
    let mut out = Vec::new();
    if platform.is_mac() {
        for (on, glyph) in [
            (mods.ctrl, Glyph::Control),
            (mods.alt, Glyph::Option),
            (mods.shift, Glyph::Shift),
            (mods.cmd, Glyph::Command),
        ] {
            if on {
                out.push(Token::Glyph(glyph));
            }
        }
    } else {
        let mut text = String::new();
        for (on, name) in [
            (mods.cmd || mods.ctrl, t!("shortcuts.key.ctrl")),
            (mods.alt, t!("shortcuts.key.alt")),
            (mods.shift, t!("shortcuts.key.shift")),
        ] {
            if on {
                text.push_str(&name);
                text.push('+');
            }
        }
        if !text.is_empty() {
            out.push(Token::Text(text));
        }
    }
    out
}

/// The token for a key on its own.
pub fn key_token(key: egui::Key, platform: Platform) -> Token {
    use egui::Key as K;
    let mac = platform.is_mac();
    match key {
        K::ArrowLeft => Token::Glyph(Glyph::Left),
        K::ArrowRight => Token::Glyph(Glyph::Right),
        K::ArrowUp => Token::Glyph(Glyph::Up),
        K::ArrowDown => Token::Glyph(Glyph::Down),
        K::Backspace if mac => Token::Glyph(Glyph::Backspace),
        K::Delete if mac => Token::Glyph(Glyph::ForwardDelete),
        K::Enter if mac => Token::Glyph(Glyph::Return),
        K::Tab if mac => Token::Glyph(Glyph::Tab),
        K::Backspace => Token::Text(t!("shortcuts.key.backspace")),
        K::Delete => Token::Text(t!("shortcuts.key.delete")),
        K::Enter => Token::Text(t!("shortcuts.key.enter")),
        K::Tab => Token::Text(t!("shortcuts.key.tab")),
        K::Space => Token::Text(t!("shortcuts.key.space")),
        K::Escape => Token::Text(t!("shortcuts.key.esc")),
        K::Home => Token::Text(t!("shortcuts.key.home")),
        K::End => Token::Text(t!("shortcuts.key.end")),
        K::PageUp => Token::Text(t!("shortcuts.key.page_up")),
        K::PageDown => Token::Text(t!("shortcuts.key.page_down")),
        K::Insert => Token::Text(t!("shortcuts.key.insert")),
        // The fonts' minus sign may be missing; the hyphen reads the same on a key.
        K::Minus => Token::Text("-".to_owned()),
        K::Quote => Token::Text("'".to_owned()),
        other => Token::Text(other.symbol_or_name().to_owned()),
    }
}

/// Every token of `chord`.
pub fn chord_tokens(chord: Chord, platform: Platform) -> Vec<Token> {
    let chord = chord.on(platform);
    let mut out = mod_tokens(chord.mods, platform);
    let key = key_token(chord.key, platform);
    match (out.last_mut(), key) {
        (Some(Token::Text(text)), Token::Text(k)) => text.push_str(&k),
        (_, key) => out.push(key),
    }
    out
}

/// Width of `tokens` in mono caption type.
pub fn tokens_width(paint: &Paint, painter: &Painter, tokens: &[Token]) -> f32 {
    tokens
        .iter()
        .map(|t| match t {
            Token::Glyph(_) => GLYPH + GLYPH_GAP,
            Token::Text(text) => paint.width(painter, text, Type::MonoCaption),
        })
        .sum()
}

/// Draws `tokens` left to right from `left` centred on `mid`; returns the right edge.
pub fn draw_tokens(
    paint: &Paint,
    painter: &Painter,
    left: f32,
    mid: f32,
    tokens: &[Token],
    color: Color32,
) -> f32 {
    let mut x = left;
    for token in tokens {
        match token {
            Token::Glyph(g) => {
                let rect = Rect::from_min_size(pos2(x, mid - GLYPH / 2.0), vec2(GLYPH, GLYPH));
                glyph(painter, rect, *g, color);
                x += GLYPH + GLYPH_GAP;
            }
            Token::Text(text) => {
                let r = paint.text_c(
                    painter,
                    pos2(x, mid),
                    Align2::LEFT_CENTER,
                    text,
                    Type::MonoCaption,
                    color,
                    f32::INFINITY,
                );
                x = r.max.x;
            }
        }
    }
    x
}

/// How a keycap chip is outlined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapEdge {
    Glass,
    Warning,
}

/// Width of a keycap chip holding `tokens`.
pub fn cap_width(paint: &Paint, painter: &Painter, tokens: &[Token]) -> f32 {
    (tokens_width(paint, painter, tokens) + 8.0).max(20.0)
}

/// The board's keycap chip: 18 high, at least 20 wide, glass outline, mono caption.
/// Returns its rectangle.
pub fn keycap(
    paint: &Paint,
    painter: &Painter,
    left_center: Pos2,
    tokens: &[Token],
    text: Role,
    edge: CapEdge,
) -> Rect {
    let w = cap_width(paint, painter, tokens);
    let rect = Rect::from_min_size(left_center - vec2(0.0, CAP_H / 2.0), vec2(w, CAP_H));
    let corner = paint.control_radius();
    paint.glass_sheen(painter, rect, corner);
    let edge = match edge {
        CapEdge::Glass => Role::GlassEdge,
        CapEdge::Warning => Role::StateWarning,
    };
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, paint.c(edge)),
        egui::StrokeKind::Inside,
    );
    let inner = tokens_width(paint, painter, tokens);
    draw_tokens(
        paint,
        painter,
        rect.center().x - inner / 2.0,
        rect.center().y,
        tokens,
        paint.c(text),
    );
    rect
}

/// A keycap at reduced opacity (keys the system keeps).
pub fn keycap_faded(paint: &Paint, painter: &Painter, left_center: Pos2, tokens: &[Token]) -> Rect {
    let w = cap_width(paint, painter, tokens);
    let rect = Rect::from_min_size(left_center - vec2(0.0, CAP_H / 2.0), vec2(w, CAP_H));
    let a = paint.theme.tokens().opacity.disabled;
    let corner = paint.control_radius();
    let half = |r: Rect, top: bool| {
        if top {
            Rect::from_min_max(r.min, pos2(r.max.x, r.center().y))
        } else {
            Rect::from_min_max(pos2(r.min.x, r.center().y), r.max)
        }
    };
    painter.rect_filled(
        half(rect, true),
        CornerRadius {
            sw: 0,
            se: 0,
            ..corner
        },
        paint.c(Role::GlassTop).gamma_multiply(a),
    );
    painter.rect_filled(
        half(rect, false),
        CornerRadius {
            nw: 0,
            ne: 0,
            ..corner
        },
        paint.c(Role::GlassBottom).gamma_multiply(a),
    );
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, paint.c(Role::GlassEdge).gamma_multiply(a)),
        egui::StrokeKind::Inside,
    );
    let inner = tokens_width(paint, painter, tokens);
    draw_tokens(
        paint,
        painter,
        rect.center().x - inner / 2.0,
        rect.center().y,
        tokens,
        paint.c(Role::TextSecondary).gamma_multiply(a),
    );
    rect
}

/// Draws `glyph` into the square `rect` on a 24-unit grid.
pub fn glyph(painter: &Painter, rect: Rect, glyph: Glyph, color: Color32) {
    let s = rect.width().min(rect.height()) / 24.0;
    let o = rect.center() - vec2(12.0, 12.0) * s;
    let p = |x: f32, y: f32| o + vec2(x, y) * s;
    let stroke = Stroke::new((2.4 * s).max(1.0), color);
    let line =
        |pts: &[(f32, f32)]| Shape::line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke);
    let closed = |pts: &[(f32, f32)]| {
        Shape::closed_line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke)
    };
    let shapes = match glyph {
        Glyph::Command => {
            let mut v = vec![
                line(&[(8.0, 5.0), (8.0, 19.0)]),
                line(&[(16.0, 5.0), (16.0, 19.0)]),
                line(&[(5.0, 8.0), (19.0, 8.0)]),
                line(&[(5.0, 16.0), (19.0, 16.0)]),
            ];
            for (x, y) in [(5.0, 5.0), (19.0, 5.0), (5.0, 19.0), (19.0, 19.0)] {
                v.push(Shape::circle_stroke(p(x, y), 3.0 * s, stroke));
            }
            v
        }
        Glyph::Shift => vec![closed(&[
            (12.0, 3.0),
            (21.0, 12.0),
            (16.0, 12.0),
            (16.0, 20.0),
            (8.0, 20.0),
            (8.0, 12.0),
            (3.0, 12.0),
        ])],
        Glyph::Option => vec![
            line(&[(3.0, 6.0), (9.0, 6.0), (15.0, 18.0), (21.0, 18.0)]),
            line(&[(14.0, 6.0), (21.0, 6.0)]),
        ],
        Glyph::Control => vec![line(&[(5.0, 14.0), (12.0, 7.0), (19.0, 14.0)])],
        Glyph::Backspace => vec![
            closed(&[
                (8.0, 5.0),
                (21.0, 5.0),
                (21.0, 19.0),
                (8.0, 19.0),
                (2.0, 12.0),
            ]),
            line(&[(11.5, 9.0), (17.5, 15.0)]),
            line(&[(17.5, 9.0), (11.5, 15.0)]),
        ],
        Glyph::ForwardDelete => vec![
            closed(&[
                (16.0, 5.0),
                (3.0, 5.0),
                (3.0, 19.0),
                (16.0, 19.0),
                (22.0, 12.0),
            ]),
            line(&[(6.5, 9.0), (12.5, 15.0)]),
            line(&[(12.5, 9.0), (6.5, 15.0)]),
        ],
        Glyph::Tab => vec![
            line(&[(3.0, 12.0), (18.0, 12.0)]),
            line(&[(13.0, 7.0), (18.0, 12.0), (13.0, 17.0)]),
            line(&[(21.0, 6.0), (21.0, 18.0)]),
        ],
        Glyph::CapsLock => vec![
            closed(&[
                (12.0, 3.0),
                (20.0, 11.0),
                (15.5, 11.0),
                (15.5, 16.0),
                (8.5, 16.0),
                (8.5, 11.0),
                (4.0, 11.0),
            ]),
            line(&[(8.5, 20.0), (15.5, 20.0)]),
        ],
        Glyph::Return => vec![
            line(&[(20.0, 5.0), (20.0, 12.0), (18.0, 14.0), (5.0, 14.0)]),
            line(&[(9.0, 10.0), (5.0, 14.0), (9.0, 18.0)]),
        ],
        Glyph::Left => vec![
            line(&[(20.0, 12.0), (4.0, 12.0)]),
            line(&[(10.0, 6.0), (4.0, 12.0), (10.0, 18.0)]),
        ],
        Glyph::Right => vec![
            line(&[(4.0, 12.0), (20.0, 12.0)]),
            line(&[(14.0, 6.0), (20.0, 12.0), (14.0, 18.0)]),
        ],
        Glyph::Up => vec![
            line(&[(12.0, 20.0), (12.0, 4.0)]),
            line(&[(6.0, 10.0), (12.0, 4.0), (18.0, 10.0)]),
        ],
        Glyph::Down => vec![
            line(&[(12.0, 4.0), (12.0, 20.0)]),
            line(&[(6.0, 14.0), (12.0, 20.0), (18.0, 14.0)]),
        ],
    };
    painter.extend(shapes);
}

#[cfg(test)]
mod tests {
    use egui::Key;

    use super::*;

    #[test]
    fn mac_chords_read_as_symbols_in_apple_order() {
        let chord = Chord::key(Key::V).alt().cmd();
        assert_eq!(
            chord_tokens(chord, Platform::Mac),
            vec![
                Token::Glyph(Glyph::Option),
                Token::Glyph(Glyph::Command),
                Token::Text("V".into())
            ]
        );
        let chord = Chord::key(Key::Backspace).shift();
        assert_eq!(
            chord_tokens(chord, Platform::Mac),
            vec![Token::Glyph(Glyph::Shift), Token::Glyph(Glyph::Backspace)]
        );
    }

    #[test]
    fn other_platforms_read_as_words() {
        let chord = Chord::key(Key::V).alt().cmd();
        assert_eq!(
            chord_tokens(chord, Platform::Windows),
            vec![Token::Text("Ctrl+Alt+V".into())]
        );
        let chord = Chord::key(Key::ArrowLeft).alt();
        assert_eq!(
            chord_tokens(chord, Platform::Linux),
            vec![Token::Text("Alt+".into()), Token::Glyph(Glyph::Left)]
        );
    }
}
