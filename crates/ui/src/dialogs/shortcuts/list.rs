//! The command list on the right of the keyboard editor: group headers with counts,
//! one 28 pt row per command with its keys, the recording and clash states, and the
//! "nothing uses this key" offer when a key search finds nothing.

use egui::{Align2, Id, Painter, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use super::glyphs::{CapEdge, Glyph, Token, cap_width, chord_tokens, glyph, keycap};
use super::{
    Filter, ICON_STROKE, Index, PendingClash, ROW, ShortcutsEditor, focus_ring, icon_reset, square,
};
use crate::panels::widgets::{Icon, Paint, Type, icon};
use crate::shortcuts::keymap::{Chord, Keymap, Platform};
use crate::shortcuts::{COMMANDS, Command, Group, command};
use crate::t;
use crate::theme::Role;

/// Space for the column header's right label over the trailing button.
const HEADER_PAD_RIGHT: f32 = 40.0;
const FIELD_H: f32 = 22.0;
const TRAIL: f32 = 24.0;
const SMALL_BUTTON_H: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Entry {
    Group(Group, usize),
    Row(&'static str),
    Clash(&'static str),
    Nothing(Chord),
}

/// The commands the list shows, in order, under their group headers.
fn entries(editor: &ShortcutsEditor, keymap: &Keymap, index: &Index) -> Vec<Entry> {
    let platform = editor.platform;
    let needle = editor.search.trim().to_lowercase();
    let shown = |c: &Command| -> bool {
        let filter = match editor.filter {
            Filter::All => true,
            Filter::Changed => index.is_changed(c.id),
            Filter::Clashes => index.has_clash(c.id),
        };
        let search = if editor.by_key {
            match editor.search_chord {
                Some(chord) => keymap
                    .chords(c.id)
                    .iter()
                    .any(|k| k.on(platform) == chord.on(platform)),
                None => true,
            }
        } else {
            needle.is_empty() || c.label().to_lowercase().contains(&needle)
        };
        filter && search
    };
    let mut out = Vec::new();
    for group in Group::ALL {
        let rows: Vec<&'static str> = COMMANDS
            .iter()
            .filter(|c| c.group == group && shown(c))
            .map(|c| c.id)
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push(Entry::Group(group, rows.len()));
        for id in rows {
            out.push(Entry::Row(id));
            if editor.clash.as_ref().is_some_and(|c| c.command == id) {
                out.push(Entry::Clash(id));
            }
        }
    }
    if out.is_empty()
        && editor.by_key
        && let Some(chord) = editor.search_chord
    {
        out.push(Entry::Nothing(chord));
    }
    out
}

/// Draws the list into `column`; returns whether the keymap changed.
pub(super) fn draw(
    editor: &mut ShortcutsEditor,
    ui: &mut Ui,
    paint: &Paint,
    column: Rect,
    keymap: &mut Keymap,
    index: &Index,
) -> bool {
    let full = ui.painter().clone();
    let header = Rect::from_min_size(
        pos2(column.min.x, column.min.y + 8.0),
        vec2(column.width(), ROW),
    );
    full.hline(
        header.x_range(),
        header.max.y - 0.5,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
    let mid = header.center().y;
    paint.text(
        &full,
        pos2(header.min.x + 16.0, mid),
        Align2::LEFT_CENTER,
        &t!("shortcuts.column.command"),
        Type::Caps,
        Role::TextMuted,
        f32::INFINITY,
    );
    paint.text(
        &full,
        pos2(header.max.x - HEADER_PAD_RIGHT, mid),
        Align2::RIGHT_CENTER,
        &t!("shortcuts.column.key"),
        Type::Caps,
        Role::TextMuted,
        f32::INFINITY,
    );

    let area = Rect::from_min_max(
        pos2(column.min.x, header.max.y),
        pos2(column.max.x, column.max.y - 8.0),
    );
    let painter = full.with_clip_rect(area);
    let list = entries(editor, keymap, index);
    let heights: Vec<f32> = list
        .iter()
        .map(|e| entry_height(editor, paint, &painter, e, area.width(), keymap))
        .collect();
    let content: f32 = heights.iter().sum();
    if ui.rect_contains_pointer(area) {
        editor.scroll -= ui.input(|i| i.smooth_scroll_delta.y);
    }
    editor.scroll = editor.scroll.clamp(0.0, (content - area.height()).max(0.0));

    let mut changed = false;
    let mut y = area.min.y - editor.scroll;
    for (entry, h) in list.iter().zip(heights) {
        let rect = Rect::from_min_size(pos2(area.min.x, y), vec2(area.width(), h));
        y += h;
        if rect.max.y < area.min.y || rect.min.y > area.max.y {
            continue;
        }
        match *entry {
            Entry::Group(group, count) => {
                let mid = rect.center().y;
                let caps = paint.text(
                    &painter,
                    pos2(rect.min.x + 16.0, mid),
                    Align2::LEFT_CENTER,
                    &group.label(),
                    Type::Caps,
                    Role::TextMuted,
                    f32::INFINITY,
                );
                paint.text(
                    &painter,
                    pos2(caps.max.x + 8.0, mid),
                    Align2::LEFT_CENTER,
                    &count.to_string(),
                    Type::MonoCaption,
                    Role::TextMuted,
                    f32::INFINITY,
                );
            }
            Entry::Row(id) => {
                changed |= row(editor, ui, paint, &painter, rect, area, id, keymap, index);
            }
            Entry::Clash(id) => {
                if let Some(clash) = editor.clash.clone()
                    && clash.command == id
                {
                    changed |= clash_panel(editor, ui, paint, &painter, rect, area, &clash, keymap);
                }
            }
            Entry::Nothing(chord) => nothing_found(editor, ui, paint, &painter, rect, area, chord),
        }
    }
    changed
}

fn entry_height(
    editor: &ShortcutsEditor,
    paint: &Paint,
    painter: &Painter,
    entry: &Entry,
    width: f32,
    keymap: &Keymap,
) -> f32 {
    match entry {
        Entry::Group(..) | Entry::Row(_) => ROW,
        Entry::Clash(_) => {
            let Some(clash) = &editor.clash else {
                return 0.0;
            };
            let body = paint.wrapped(
                painter,
                &clash_body_for(clash, keymap, editor.platform),
                Type::Label,
                paint.c(Role::TextSecondary),
                width - 32.0 - 24.0,
                false,
            );
            2.0 + 10.0
                + paint.line_height(Type::Label)
                + 2.0
                + body.size().y
                + 8.0
                + SMALL_BUTTON_H
                + 10.0
                + 10.0
        }
        Entry::Nothing(_) => {
            8.0 + 8.0 + 2.0 * paint.line_height(Type::Label) + 16.0 + SMALL_BUTTON_H + 10.0
        }
    }
}

/// How a chord reads inside a sentence (symbols are drawn, not typed, so sentences
/// spell modifiers out).
fn chord_words(chord: Chord, platform: Platform) -> String {
    let chord = chord.on(platform);
    let m = chord.mods;
    let names: [(bool, String); 4] = if platform.is_mac() {
        [
            (m.ctrl, t!("shortcuts.key.control")),
            (m.alt, t!("shortcuts.key.option")),
            (m.shift, t!("shortcuts.key.shift")),
            (m.cmd, t!("shortcuts.key.command")),
        ]
    } else {
        [
            (m.cmd, t!("shortcuts.key.ctrl")),
            (m.alt, t!("shortcuts.key.alt")),
            (m.shift, t!("shortcuts.key.shift")),
            (false, String::new()),
        ]
    };
    let mut out = String::new();
    for (on, name) in names {
        if on {
            out.push_str(&name);
            out.push('-');
        }
    }
    let key = match super::glyphs::key_token(chord.key, platform) {
        Token::Text(s) => s,
        Token::Glyph(g) => match g {
            Glyph::Backspace => t!("shortcuts.key.backspace"),
            Glyph::ForwardDelete => t!("shortcuts.key.delete"),
            Glyph::Return => t!("shortcuts.key.enter"),
            Glyph::Tab => t!("shortcuts.key.tab"),
            Glyph::Left => t!("shortcuts.key.left"),
            Glyph::Right => t!("shortcuts.key.right"),
            Glyph::Up => t!("shortcuts.key.up"),
            Glyph::Down => t!("shortcuts.key.down"),
            _ => chord.key.name().to_owned(),
        },
    };
    out.push_str(&key);
    out
}

fn clash_title(clash: &PendingClash, platform: Platform) -> String {
    let other = clash
        .others
        .first()
        .and_then(|id| command(id))
        .map(Command::label)
        .unwrap_or_default();
    t!(
        "shortcuts.clash.title",
        key = chord_words(clash.chord, platform),
        command = other
    )
}

/// The swap target text for the pending clash: who gets what.
fn clash_body_for(clash: &PendingClash, keymap: &Keymap, platform: Platform) -> String {
    let other = clash
        .others
        .first()
        .and_then(|id| command(id))
        .map(Command::label)
        .unwrap_or_default();
    match keymap.chords(clash.command).first() {
        Some(old) if !clash.add => t!(
            "shortcuts.clash.body",
            command = other,
            key = chord_words(*old, platform)
        ),
        _ => t!("shortcuts.clash.body_take", command = other),
    }
}

#[allow(clippy::too_many_arguments)]
fn row(
    editor: &mut ShortcutsEditor,
    ui: &mut Ui,
    paint: &Paint,
    painter: &Painter,
    rect: Rect,
    area: Rect,
    id: &'static str,
    keymap: &mut Keymap,
    index: &Index,
) -> bool {
    let Some(c) = command(id) else {
        return false;
    };
    let platform = editor.platform;
    let visible = rect.intersect(area);
    let response = ui.interact(
        visible,
        Id::new(("glass.shortcuts.row", id)),
        Sense::click(),
    );
    let selected = editor.selected == id;
    let clash = editor.clash.as_ref().filter(|c| c.command == id).cloned();
    if clash.is_some() {
        painter.rect_filled(rect, 0.0, paint.mix(Role::StateWarning, 0.06));
    } else if selected {
        paint.glass_fill(painter, rect, egui::CornerRadius::ZERO);
        painter.rect_filled(
            Rect::from_min_size(pos2(rect.min.x, rect.min.y + 6.0), vec2(2.0, 16.0)),
            egui::CornerRadius::same(255),
            paint.c(Role::Accent),
        );
    } else if response.hovered() {
        painter.rect_filled(rect, 0.0, paint.c(Role::Bg3));
    }
    let mid = rect.center().y;
    let name_ty = if selected && clash.is_none() {
        Type::LabelMedium
    } else {
        Type::Label
    };
    let trail = Rect::from_min_size(
        pos2(rect.max.x - 8.0 - TRAIL, mid - TRAIL / 2.0),
        Vec2::splat(TRAIL),
    );
    let keys_right = trail.min.x - 8.0;

    let mut changed = false;
    let changed_here = index.is_changed(id);
    let chords: Vec<Chord> = keymap.chords(id).to_vec();
    let recording = editor.recording.filter(|r| r.command == id);
    let keys_left;
    if let Some(rec) = recording {
        // Recording: the focused key field.
        let (content_w, tokens) = match rec.captured {
            Some(chord) => {
                let tokens = chord_tokens(chord, platform);
                let w = cap_width(paint, painter, &tokens)
                    + 6.0
                    + 10.0
                    + 2.0
                    + paint.width(painter, &t!("shortcuts.record.keeps"), Type::MonoCaption);
                (w, Some(tokens))
            }
            None => (
                paint.width(painter, &t!("shortcuts.record.press"), Type::Label) + 6.0 + 1.0,
                None,
            ),
        };
        let field = Rect::from_min_max(
            pos2(keys_right - content_w - 12.0, mid - FIELD_H / 2.0),
            pos2(keys_right, mid + FIELD_H / 2.0),
        );
        painter.rect_filled(field, paint.control_radius(), paint.c(Role::Bg2));
        focus_ring(
            paint,
            painter,
            field,
            Role::Accent,
            paint.c(Role::AccentSoft),
        );
        let x = field.min.x + 6.0;
        match tokens {
            Some(tokens) => {
                let cap = keycap(
                    paint,
                    painter,
                    pos2(x, mid),
                    &tokens,
                    Role::TextPrimary,
                    CapEdge::Glass,
                );
                let r = Rect::from_min_size(pos2(cap.max.x + 6.0, mid - 5.0), vec2(10.0, 10.0));
                glyph(painter, r, Glyph::Return, paint.c(Role::TextMuted));
                paint.text(
                    painter,
                    pos2(r.max.x + 2.0, mid),
                    Align2::LEFT_CENTER,
                    &t!("shortcuts.record.keeps"),
                    Type::MonoCaption,
                    Role::TextMuted,
                    f32::INFINITY,
                );
            }
            None => {
                let text = paint.text(
                    painter,
                    pos2(x, mid),
                    Align2::LEFT_CENTER,
                    &t!("shortcuts.record.press"),
                    Type::Label,
                    Role::TextMuted,
                    f32::INFINITY,
                );
                painter.vline(
                    text.max.x + 6.5,
                    (mid - 6.0)..=(mid + 6.0),
                    Stroke::new(1.0, paint.c(Role::Accent)),
                );
            }
        }
        keys_left = field.min.x;
    } else if let Some(clash) = &clash {
        // The key that clashes, flagged.
        let tokens = chord_tokens(clash.chord, platform);
        let w = 12.0 + 6.0 + cap_width(paint, painter, &tokens);
        let field = Rect::from_min_max(
            pos2(keys_right - w - 12.0, mid - FIELD_H / 2.0),
            pos2(keys_right, mid + FIELD_H / 2.0),
        );
        painter.rect_filled(field, paint.control_radius(), paint.c(Role::Bg2));
        focus_ring(
            paint,
            painter,
            field,
            Role::StateWarning,
            paint.mix(Role::StateWarning, 0.16),
        );
        icon(
            painter,
            square(pos2(field.min.x + 6.0, mid), 12.0),
            Icon::Alert,
            paint.c(Role::StateWarning),
            2.5,
        );
        keycap(
            paint,
            painter,
            pos2(field.min.x + 6.0 + 12.0 + 6.0, mid),
            &tokens,
            Role::TextPrimary,
            CapEdge::Glass,
        );
        keys_left = field.min.x;
    } else if chords.is_empty() {
        // No key: the Add key chip.
        let label = t!("shortcuts.add_key");
        let w = 6.0 + 12.0 + 4.0 + paint.width(painter, &label, Type::Caption) + 6.0;
        let chip = Rect::from_min_max(
            pos2(keys_right - w, mid - FIELD_H / 2.0),
            pos2(keys_right, mid + FIELD_H / 2.0),
        );
        let add = ui.interact(
            chip.intersect(area),
            Id::new(("glass.shortcuts.add", id)),
            Sense::click(),
        );
        painter.rect_stroke(
            chip,
            paint.control_radius(),
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
            StrokeKind::Inside,
        );
        icon(
            painter,
            square(pos2(chip.min.x + 6.0, mid), 12.0),
            Icon::Plus,
            paint.c(Role::TextMuted),
            ICON_STROKE,
        );
        paint.text(
            painter,
            pos2(chip.min.x + 6.0 + 12.0 + 4.0, mid),
            Align2::LEFT_CENTER,
            &label,
            Type::Caption,
            Role::TextMuted,
            f32::INFINITY,
        );
        if add.clicked() {
            editor.start_recording(keymap, id, false);
        }
        keys_left = chip.min.x;
    } else {
        // The keys, right-aligned; hovering one offers to remove it.
        let text_role = if changed_here {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        };
        let mut x = keys_right;
        let mut remove = None;
        let mut record = false;
        for (i, chord) in chords.iter().enumerate().rev() {
            let tokens = chord_tokens(*chord, platform);
            let cap_w = cap_width(paint, painter, &tokens);
            let edge = if index.is_clash(id, chord.on(platform)) {
                CapEdge::Warning
            } else {
                CapEdge::Glass
            };
            let hover_id = Id::new(("glass.shortcuts.cap", id, i));
            let hovered_before = ui
                .ctx()
                .data(|d| d.get_temp::<bool>(hover_id))
                .unwrap_or(false);
            let group_w = if hovered_before {
                4.0 + cap_w + 4.0 + 12.0 + 4.0
            } else {
                cap_w
            };
            let group = Rect::from_min_max(
                pos2(x - group_w, mid - FIELD_H / 2.0),
                pos2(x, mid + FIELD_H / 2.0),
            );
            let response = ui.interact(group.intersect(area), hover_id, Sense::click());
            let hovered = response.hovered();
            ui.ctx().data_mut(|d| d.insert_temp(hover_id, hovered));
            if hovered_before {
                painter.rect_filled(group, paint.control_radius(), paint.c(Role::Bg3));
                let cap = keycap(
                    paint,
                    painter,
                    pos2(group.min.x + 4.0, mid),
                    &tokens,
                    text_role,
                    edge,
                );
                let close = square(pos2(cap.max.x + 4.0, mid), 12.0);
                icon(
                    painter,
                    close,
                    Icon::Close,
                    paint.c(Role::TextSecondary),
                    ICON_STROKE,
                );
                if response.clicked() {
                    let pointer = ui.input(|i| i.pointer.interact_pos());
                    if pointer.is_some_and(|p| p.x >= close.min.x - 2.0) {
                        remove = Some(i);
                    } else {
                        record = true;
                    }
                }
            } else {
                keycap(
                    paint,
                    painter,
                    pos2(group.min.x, mid),
                    &tokens,
                    text_role,
                    edge,
                );
                if response.clicked() {
                    record = true;
                }
            }
            x = group.min.x - 6.0;
        }
        if changed_here {
            // `was ⌥F`, with a changed dot.
            let old = keymap.preset_chords(id);
            let mut tokens: Vec<Token> = vec![Token::Text(t!("shortcuts.was"))];
            match old.first() {
                Some(chord) => {
                    tokens.push(Token::Text(" ".to_owned()));
                    tokens.extend(chord_tokens(*chord, platform));
                }
                None => tokens.push(Token::Text(format!(" {}", t!("shortcuts.none")))),
            }
            let w = super::glyphs::tokens_width(paint, painter, &tokens);
            super::glyphs::draw_tokens(
                paint,
                painter,
                x - w,
                mid,
                &tokens,
                paint.c(Role::TextMuted),
            );
            x -= w + 6.0;
            paint.dot(painter, pos2(x - 3.0, mid), 6.0, paint.c(Role::Accent));
            x -= 6.0 + 6.0;
        }
        if let Some(i) = remove {
            let mut left = chords.clone();
            left.remove(i);
            editor.edit(keymap, |k, _| k.set(id, left));
            changed = true;
        } else if record {
            editor.start_recording(keymap, id, false);
        }
        keys_left = x;
    }

    paint.text(
        painter,
        pos2(rect.min.x + 16.0, mid),
        Align2::LEFT_CENTER,
        &c.label(),
        name_ty,
        Role::TextPrimary,
        (keys_left - 8.0 - rect.min.x - 16.0).max(1.0),
    );

    // The trailing slot: reset a changed command, or add a key on hover.
    if recording.is_none() && clash.is_none() {
        if changed_here {
            let reset = ui.interact(
                trail.intersect(area),
                Id::new(("glass.shortcuts.reset", id)),
                Sense::click(),
            );
            if reset.hovered() {
                painter.rect_filled(trail, paint.control_radius(), paint.c(Role::Bg3));
            }
            icon_reset(
                painter,
                square(trail.center() - vec2(7.0, 0.0), 14.0),
                paint.c(Role::TextSecondary),
            );
            if reset.clicked() {
                editor.edit(keymap, |k, _| k.reset(id));
                changed = true;
            }
        } else if response.hovered() || ui.rect_contains_pointer(trail.intersect(area)) {
            let add = ui.interact(
                trail.intersect(area),
                Id::new(("glass.shortcuts.add_more", id)),
                Sense::click(),
            );
            icon(
                painter,
                square(trail.center() - vec2(7.0, 0.0), 14.0),
                Icon::Plus,
                paint.c(Role::TextSecondary),
                ICON_STROKE,
            );
            if add.clicked() {
                editor.start_recording(keymap, id, true);
            }
        }
    }

    if response.clicked() {
        editor.select(keymap, id);
        if let Some(chord) = editor.offer.take() {
            editor.start_recording(keymap, id, false);
            editor.capture(chord);
        }
    }
    if response.double_clicked() {
        editor.start_recording(keymap, id, false);
    }
    changed
}

#[allow(clippy::too_many_arguments)]
fn clash_panel(
    editor: &mut ShortcutsEditor,
    ui: &mut Ui,
    paint: &Paint,
    painter: &Painter,
    rect: Rect,
    area: Rect,
    clash: &PendingClash,
    keymap: &mut Keymap,
) -> bool {
    let platform = editor.platform;
    painter.rect_filled(rect, 0.0, paint.mix(Role::StateWarning, 0.06));
    let panel = Rect::from_min_max(
        pos2(rect.min.x + 16.0, rect.min.y + 2.0),
        pos2(rect.max.x - 16.0, rect.max.y - 10.0),
    );
    let corner = paint.control_radius();
    painter.rect_filled(panel, corner, paint.mix(Role::StateWarning, 0.10));
    painter.rect_stroke(
        panel,
        corner,
        Stroke::new(1.0, paint.mix(Role::StateWarning, 0.32)),
        StrokeKind::Inside,
    );
    let x = panel.min.x + 12.0;
    let mut y = panel.min.y + 10.0;
    let line = paint.line_height(Type::Label);
    paint.text(
        painter,
        pos2(x, y + line / 2.0),
        Align2::LEFT_CENTER,
        &clash_title(clash, platform),
        Type::Label,
        Role::TextPrimary,
        panel.width() - 24.0,
    );
    y += line + 2.0;
    let body = paint.wrapped(
        painter,
        &clash_body_for(clash, keymap, platform),
        Type::Label,
        paint.c(Role::TextSecondary),
        panel.width() - 24.0,
        false,
    );
    let body_h = body.size().y;
    painter.galley(pos2(x, y), body, paint.c(Role::TextSecondary));
    y += body_h + 8.0;
    let mid = y + SMALL_BUTTON_H / 2.0;

    let mut bx = x;
    let mut choice = None;
    let can_swap = !clash.add && !keymap.chords(clash.command).is_empty();
    let buttons: Vec<(String, Option<bool>, bool)> = if can_swap {
        vec![
            (t!("shortcuts.clash.swap"), Some(true), true),
            (t!("shortcuts.clash.take"), Some(false), false),
            (t!("common.cancel"), None, false),
        ]
    } else {
        vec![
            (t!("shortcuts.clash.take"), Some(false), true),
            (t!("common.cancel"), None, false),
        ]
    };
    for (i, (label, value, primary)) in buttons.into_iter().enumerate() {
        let w = paint.width(painter, &label, Type::LabelMedium) + 16.0;
        let r = Rect::from_min_size(
            pos2(bx, mid - SMALL_BUTTON_H / 2.0),
            vec2(w, SMALL_BUTTON_H),
        );
        let response = ui.interact(
            r.intersect(area),
            Id::new(("glass.shortcuts.clash", i)),
            Sense::click(),
        );
        if primary {
            if response.hovered() {
                painter.rect_filled(r, corner, paint.c(Role::Bg3));
            }
            paint.glass_outline(painter, r, corner);
        } else if response.hovered() {
            painter.rect_filled(r, corner, paint.c(Role::Bg3));
        }
        paint.text(
            painter,
            r.center(),
            Align2::CENTER_CENTER,
            &label,
            Type::LabelMedium,
            if primary {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            },
            f32::INFINITY,
        );
        if response.clicked() {
            choice = Some(value);
        }
        bx = r.max.x + 4.0;
    }
    paint.text(
        painter,
        pos2(panel.max.x - 12.0, mid),
        Align2::RIGHT_CENTER,
        &t!("shortcuts.clash.esc"),
        Type::MonoCaption,
        Role::TextMuted,
        f32::INFINITY,
    );
    match choice {
        Some(value) => editor.resolve_clash(keymap, value),
        None => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn nothing_found(
    editor: &mut ShortcutsEditor,
    ui: &mut Ui,
    paint: &Paint,
    painter: &Painter,
    rect: Rect,
    area: Rect,
    chord: Chord,
) {
    let x = rect.min.x + 16.0 + 12.0;
    let line = paint.line_height(Type::Label);
    let mut y = rect.min.y + 8.0 + 8.0;
    paint.text(
        painter,
        pos2(x, y + line / 2.0),
        Align2::LEFT_CENTER,
        &t!(
            "shortcuts.nothing.title",
            key = chord_words(chord, editor.platform)
        ),
        Type::Label,
        Role::TextPrimary,
        rect.width() - 56.0,
    );
    y += line;
    paint.text(
        painter,
        pos2(x, y + line / 2.0),
        Align2::LEFT_CENTER,
        &t!("shortcuts.nothing.body"),
        Type::Label,
        Role::TextSecondary,
        rect.width() - 56.0,
    );
    y += line + 8.0;
    let label = t!("shortcuts.nothing.give");
    let w = paint.width(painter, &label, Type::LabelMedium) + 16.0;
    let r = Rect::from_min_size(pos2(x, y), vec2(w, SMALL_BUTTON_H));
    let response = ui.interact(
        r.intersect(area),
        Id::new("glass.shortcuts.give"),
        Sense::click(),
    );
    if response.hovered() {
        painter.rect_filled(r, paint.control_radius(), paint.c(Role::Bg3));
    }
    paint.glass_outline(painter, r, paint.control_radius());
    paint.text(
        painter,
        r.center(),
        Align2::CENTER_CENTER,
        &label,
        Type::LabelMedium,
        Role::TextPrimary,
        f32::INFINITY,
    );
    if response.clicked() {
        // Back to the list by name; the next command picked gets this key.
        editor.by_key = false;
        editor.search_chord = None;
        editor.offer = Some(chord);
    }
}
