//! `Window / Keyboard shortcuts` (Paper, page "03 Timeline", `Sheet / Keyboard`): the
//! keyboard editor, opened from Settings › Shortcuts or ⌥⌘K.
//!
//! Left, the keyboard map for the modifier layer being looked at (hold a modifier, or
//! pick one under Keys held), what the selected key does in every layer, the user's
//! changes and the keys the system keeps. Right, every command with its keys, grouped,
//! searchable by name or by pressing keys, filtered to changed or clashing commands.
//!
//! Recording a key: click a command's keys (or double-click the row), press the chord,
//! Return keeps it, Escape cancels. A key another command already has is flagged
//! before it takes effect, with Swap, Take it and Cancel. Every change is saved as it
//! happens ([`ShortcutsResponse::changed`]) and undoes with ⌘Z, one step per change.

mod glyphs;
mod keyboard;
mod list;

use std::collections::{HashMap, HashSet};

use egui::{
    Align2, CornerRadius, Event, Id, Key, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2, pos2, vec2,
};

use self::glyphs::{CapEdge, Token, cap_width, chord_tokens, draw_tokens, keycap, keycap_faded};
use crate::panels::widgets::{Icon, Paint, Type, icon, size};
use crate::shortcuts::dispatch::is_modifier;
use crate::shortcuts::keymap::{Chord, Keymap, Mods, Platform, reserved};
use crate::shortcuts::{COMMANDS, PresetId, command};
use crate::t;
use crate::theme::Role;

/// The window's width on the board; the editor takes any width from [`MIN_WIDTH`].
pub const WIDTH: f32 = 1262.0;
pub const MIN_WIDTH: f32 = 1100.0;
const LEFT_W: f32 = 736.0;
const PAD: f32 = 20.0;
const BAR_H: f32 = 48.0;
const SEARCH_W: f32 = 360.0;
const PRESET_W: f32 = 200.0;
const CONTROL_H: f32 = 28.0;
const SEGMENT_H: f32 = 20.0;
const ICON: f32 = 16.0;
const SMALL_ICON: f32 = 14.0;
/// The board's icons: a 2.25 stroke on the 24-unit grid.
const ICON_STROKE: f32 = 2.25;
const ROW: f32 = size::ROW;
const DETAIL_CARD_H: f32 = 56.0;
const MENU_W: f32 = 256.0;
const UNDO_DEPTH: usize = 100;

/// Which commands the list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Changed,
    Clashes,
}

/// A key being recorded for a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recording {
    pub command: &'static str,
    /// Adds a key instead of replacing the command's keys.
    pub add: bool,
    /// The chord pressed so far; Return keeps it.
    pub captured: Option<Chord>,
}

/// A recorded key that another command already has, waiting for Swap, Take it or
/// Cancel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingClash {
    pub command: &'static str,
    pub chord: Chord,
    pub add: bool,
    pub others: Vec<&'static str>,
}

/// What the window asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutsAction {
    /// Read a keymap file chosen by the user ([`Keymap::load`]).
    Import,
    /// Write the keymap to a file chosen by the user ([`Keymap::save`]).
    Export,
    /// Done, the close button, or Escape with nothing in progress.
    Close,
}

/// One frame's outcome.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShortcutsResponse {
    /// The keymap changed this frame; the app saves it and rebuilds its dispatcher.
    pub changed: bool,
    pub action: Option<ShortcutsAction>,
}

/// The keymap looked up by chord, with its clashes and changes, built each frame.
pub struct Index {
    by_chord: HashMap<Chord, Vec<&'static str>>,
    clashes: HashSet<(&'static str, Chord)>,
    clash_commands: HashSet<&'static str>,
    changed: HashSet<&'static str>,
    conflicts: usize,
}

impl Index {
    pub fn build(keymap: &Keymap, platform: Platform) -> Self {
        let mut by_chord: HashMap<Chord, Vec<&'static str>> = HashMap::new();
        for c in COMMANDS {
            for chord in keymap.chords(c.id) {
                let ids = by_chord.entry(chord.on(platform)).or_default();
                if !ids.contains(&c.id) {
                    ids.push(c.id);
                }
            }
        }
        let conflicts = keymap.conflicts(platform);
        let mut clashes = HashSet::new();
        let mut clash_commands = HashSet::new();
        for conflict in &conflicts {
            for id in &conflict.commands {
                clashes.insert((*id, conflict.chord));
                clash_commands.insert(*id);
            }
        }
        Self {
            by_chord,
            clashes,
            clash_commands,
            changed: keymap.changed().into_iter().collect(),
            conflicts: conflicts.len(),
        }
    }

    /// The commands on `chord` (already in the platform's form).
    pub fn on(&self, chord: Chord) -> &[&'static str] {
        self.by_chord.get(&chord).map_or(&[], Vec::as_slice)
    }

    pub fn is_clash(&self, id: &str, chord: Chord) -> bool {
        self.clashes.iter().any(|(c, k)| *c == id && *k == chord)
    }

    pub fn has_clash(&self, id: &str) -> bool {
        self.clash_commands.contains(id)
    }

    pub fn is_changed(&self, id: &str) -> bool {
        self.changed.contains(id)
    }
}

/// The editor's state between frames.
#[derive(Clone, Debug)]
pub struct ShortcutsEditor {
    platform: Platform,
    search: String,
    by_key: bool,
    search_chord: Option<Chord>,
    filter: Filter,
    layer: Mods,
    selected: &'static str,
    selected_key: Option<Key>,
    recording: Option<Recording>,
    clash: Option<PendingClash>,
    preset_menu: bool,
    /// A free key from a key search, waiting for the command picked next.
    offer: Option<Chord>,
    scroll: f32,
    undo: Vec<Keymap>,
    redo: Vec<Keymap>,
}

impl ShortcutsEditor {
    pub fn new(platform: Platform) -> Self {
        let first = COMMANDS[0].id;
        Self {
            platform,
            search: String::new(),
            by_key: false,
            search_chord: None,
            filter: Filter::All,
            layer: Mods::NONE,
            selected: first,
            selected_key: None,
            recording: None,
            clash: None,
            preset_menu: false,
            offer: None,
            scroll: 0.0,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    pub fn selected(&self) -> &'static str {
        self.selected
    }

    pub fn recording(&self) -> Option<Recording> {
        self.recording
    }

    pub fn pending_clash(&self) -> Option<&PendingClash> {
        self.clash.as_ref()
    }

    pub fn filter(&self) -> Filter {
        self.filter
    }

    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.scroll = 0.0;
    }

    pub fn set_search(&mut self, text: &str) {
        self.by_key = false;
        self.search = text.to_owned();
        self.scroll = 0.0;
    }

    /// Searches by key: lists what `chord` runs.
    pub fn set_search_chord(&mut self, chord: Option<Chord>) {
        self.by_key = true;
        self.search_chord = chord;
        self.scroll = 0.0;
    }

    /// The modifier layer the keyboard map shows when no modifier is held.
    pub fn set_layer(&mut self, layer: Mods) {
        self.layer = layer;
    }

    pub fn set_preset_menu(&mut self, open: bool) {
        self.preset_menu = open;
    }

    /// Selects a command and its first key on the map.
    pub fn select(&mut self, keymap: &Keymap, id: &'static str) {
        self.selected = id;
        self.selected_key = keymap.chords(id).first().map(|c| c.key);
    }

    /// Starts recording a key for `id` (replacing its keys, or adding one).
    pub fn start_recording(&mut self, keymap: &Keymap, id: &'static str, add: bool) {
        self.select(keymap, id);
        self.clash = None;
        self.recording = Some(Recording {
            command: id,
            add,
            captured: None,
        });
    }

    /// A chord pressed while recording.
    pub fn capture(&mut self, chord: Chord) {
        if let Some(rec) = &mut self.recording
            && !crate::shortcuts::keymap::is_reserved(chord, self.platform)
        {
            rec.captured = Some(chord);
        }
    }

    /// Return while recording: applies the captured chord, or holds it as a pending
    /// clash when another command has it. Returns whether the keymap changed.
    pub fn keep(&mut self, keymap: &mut Keymap) -> bool {
        let Some(Recording {
            command,
            add,
            captured: Some(chord),
        }) = self.recording
        else {
            return false;
        };
        self.recording = None;
        let others: Vec<&'static str> = keymap
            .commands_on(chord, self.platform)
            .into_iter()
            .filter(|id| *id != command)
            .collect();
        if !others.is_empty() {
            self.clash = Some(PendingClash {
                command,
                chord,
                add,
                others,
            });
            return false;
        }
        self.edit(keymap, |k, _| {
            if add {
                let mut chords = k.chords(command).to_vec();
                if !chords.contains(&chord) {
                    chords.push(chord);
                }
                k.set(command, chords);
            } else {
                k.set(command, vec![chord]);
            }
        });
        self.selected_key = Some(chord.key);
        true
    }

    /// Resolves the pending clash: `Some(true)` swaps, `Some(false)` takes the key,
    /// `None` cancels. Returns whether the keymap changed.
    pub fn resolve_clash(&mut self, keymap: &mut Keymap, swap: Option<bool>) -> bool {
        let Some(clash) = self.clash.take() else {
            return false;
        };
        let Some(swap) = swap else {
            return false;
        };
        let platform = self.platform;
        self.edit(keymap, |k, _| {
            let previous: Vec<Chord> = k.chords(clash.command).to_vec();
            if swap {
                k.swap(clash.command, clash.chord, platform);
            } else {
                k.take(clash.command, clash.chord, platform);
            }
            if clash.add {
                let mut chords = previous;
                if !chords.contains(&clash.chord) {
                    chords.push(clash.chord);
                }
                k.set(clash.command, chords);
            }
        });
        self.selected_key = Some(clash.chord.key);
        true
    }

    /// One undoable change.
    fn edit(&mut self, keymap: &mut Keymap, f: impl FnOnce(&mut Keymap, Platform)) {
        let before = keymap.clone();
        f(keymap, self.platform);
        if *keymap != before {
            self.undo.push(before);
            if self.undo.len() > UNDO_DEPTH {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    pub fn undo(&mut self, keymap: &mut Keymap) -> bool {
        match self.undo.pop() {
            Some(previous) => {
                self.redo.push(std::mem::replace(keymap, previous));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self, keymap: &mut Keymap) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(keymap, next));
                true
            }
            None => false,
        }
    }

    /// The editor's height for the board's layout with `keymap`'s changes listed.
    pub fn height(&self, keymap: &Keymap) -> f32 {
        2.0 * BAR_H + size::DIALOG_FOOTER + left_column_height(keymap.changed().len())
    }

    /// Draws the editor into `rect` (from [`MIN_WIDTH`] wide), handles its keys, and
    /// returns what changed.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, keymap: &mut Keymap) -> ShortcutsResponse {
        let mut out = ShortcutsResponse::default();
        out.changed |= self.handle_keys(ui.ctx(), keymap, &mut out.action);

        let paint = Paint::new(ui.ctx());
        let painter = ui.painter().clone();
        ui.allocate_rect(rect, Sense::click());
        window_surface(&paint, &painter, rect);
        let index = Index::build(keymap, self.platform);

        let title = Rect::from_min_size(rect.min, vec2(rect.width(), BAR_H));
        let toolbar = Rect::from_min_size(title.left_bottom(), vec2(rect.width(), BAR_H));
        let footer =
            Rect::from_min_max(pos2(rect.min.x, rect.max.y - size::DIALOG_FOOTER), rect.max);
        let body = Rect::from_min_max(toolbar.left_bottom(), footer.right_top());
        let left = Rect::from_min_size(body.min, vec2(LEFT_W, body.height()));
        let right = Rect::from_min_max(left.right_top(), body.max);
        let hairline = Stroke::new(1.0, paint.c(Role::BorderSubtle));
        painter.hline(title.x_range().shrink(1.0), title.max.y - 0.5, hairline);
        painter.hline(toolbar.x_range().shrink(1.0), toolbar.max.y - 0.5, hairline);
        painter.hline(footer.x_range().shrink(1.0), footer.min.y + 0.5, hairline);
        painter.vline(left.max.x - 0.5, body.y_range(), hairline);

        let preset_rect = self.title_row(ui, &paint, title, keymap, &mut out);
        self.toolbar(ui, &paint, toolbar, keymap, &index);
        self.left_column(ui, &paint, left, keymap, &index, &mut out);
        out.changed |= list::draw(self, ui, &paint, right, keymap, &index);
        self.footer(ui, &paint, footer, keymap, &index, &mut out);
        if self.preset_menu {
            out.changed |= self.preset_menu(ui, &paint, preset_rect, keymap);
        }
        out
    }

    /// Recording, key search, Escape, Return, ⌘F and ⌘Z.
    fn handle_keys(
        &mut self,
        ctx: &egui::Context,
        keymap: &mut Keymap,
        action: &mut Option<ShortcutsAction>,
    ) -> bool {
        let platform = self.platform;
        let capturing = self.recording.is_some() || self.by_key;
        let mut presses: Vec<(Key, egui::Modifiers)> = Vec::new();
        ctx.input_mut(|input| {
            input.events.retain(|event| match event {
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    repeat: false,
                    ..
                } => {
                    let ours = capturing
                        || matches!(key, Key::Escape | Key::Enter)
                        || (modifiers.command && matches!(key, Key::F | Key::Z));
                    if ours {
                        presses.push((*key, *modifiers));
                    }
                    !ours
                }
                Event::Key { .. } if capturing => false,
                Event::Text(_) if capturing => false,
                _ => true,
            });
        });
        let search_focused = ctx.memory(|m| m.has_focus(search_id()));
        let mut changed = false;
        for (key, modifiers) in presses {
            if key == Key::Escape {
                let cancelled = self.recording.take().is_some()
                    | self.clash.take().is_some()
                    | self.offer.take().is_some();
                if cancelled {
                    continue;
                }
                if self.preset_menu {
                    self.preset_menu = false;
                } else if self.by_key {
                    self.by_key = false;
                    self.search_chord = None;
                } else if !self.search.is_empty() {
                    self.search.clear();
                } else {
                    *action = Some(ShortcutsAction::Close);
                }
                continue;
            }
            if self.recording.is_some() {
                if key == Key::Enter
                    && modifiers.is_none()
                    && self.recording.is_some_and(|r| r.captured.is_some())
                {
                    changed |= self.keep(keymap);
                } else if !is_modifier(key) {
                    self.capture(Chord::from_egui(key, modifiers, platform));
                }
                continue;
            }
            if self.by_key {
                if !is_modifier(key) {
                    self.search_chord = Some(Chord::from_egui(key, modifiers, platform));
                    self.scroll = 0.0;
                }
                continue;
            }
            match key {
                Key::Enter => {
                    if self.clash.is_none() && !search_focused {
                        let id = self.selected;
                        self.start_recording(keymap, id, false);
                    }
                }
                Key::F => ctx.memory_mut(|m| m.request_focus(search_id())),
                Key::Z if !search_focused => {
                    changed |= if modifiers.shift {
                        self.redo(keymap)
                    } else {
                        self.undo(keymap)
                    };
                }
                _ => {}
            }
        }
        changed
    }

    fn title_row(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        bar: Rect,
        keymap: &Keymap,
        out: &mut ShortcutsResponse,
    ) -> Rect {
        let painter = ui.painter().clone();
        let mid = bar.center().y;
        let mut x = bar.min.x + 16.0;
        icon_keyboard(
            &painter,
            square(pos2(x, mid), ICON),
            paint.c(Role::TextSecondary),
        );
        x += ICON + 8.0;
        paint.text(
            &painter,
            pos2(x, mid),
            Align2::LEFT_CENTER,
            &t!("shortcuts.title"),
            Type::LabelMedium,
            Role::TextPrimary,
            320.0,
        );

        // Right to left: close, Export…, Import…, divider, preset dropdown, "Preset".
        let mut right = bar.max.x - 12.0;
        let close = Rect::from_min_max(pos2(right - 24.0, mid - 12.0), pos2(right, mid + 12.0));
        if self.ghost_icon(ui, paint, close, "close").clicked() {
            out.action = Some(ShortcutsAction::Close);
        }
        icon(
            &painter,
            Rect::from_center_size(close.center(), Vec2::splat(ICON)),
            Icon::Close,
            paint.c(Role::TextSecondary),
            ICON_STROKE,
        );
        right = close.min.x - 8.0;
        for (label, which, act) in [
            (
                t!("shortcuts.export"),
                BarIcon::Export,
                ShortcutsAction::Export,
            ),
            (
                t!("shortcuts.import"),
                BarIcon::Import,
                ShortcutsAction::Import,
            ),
        ] {
            let w = 8.0 + ICON + 8.0 + paint.width(&painter, &label, Type::LabelMedium) + 8.0;
            let r = Rect::from_min_max(
                pos2(right - w, mid - CONTROL_H / 2.0),
                pos2(right, mid + CONTROL_H / 2.0),
            );
            if self.ghost_icon(ui, paint, r, act_id(act)).clicked() {
                out.action = Some(act);
            }
            let icon_rect = square(pos2(r.min.x + 8.0, mid), ICON);
            match which {
                BarIcon::Import => icon(
                    &painter,
                    icon_rect,
                    Icon::Import,
                    paint.c(Role::TextSecondary),
                    ICON_STROKE,
                ),
                BarIcon::Export => icon_upload(&painter, icon_rect, paint.c(Role::TextSecondary)),
            }
            paint.text(
                &painter,
                pos2(r.min.x + 8.0 + ICON + 8.0, mid),
                Align2::LEFT_CENTER,
                &label,
                Type::LabelMedium,
                Role::TextSecondary,
                f32::INFINITY,
            );
            right = r.min.x - 8.0;
        }
        painter.vline(
            right - 0.5,
            (mid - 10.0)..=(mid + 10.0),
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        );
        right -= 8.0;
        let dropdown = Rect::from_min_max(
            pos2(right - PRESET_W, mid - CONTROL_H / 2.0),
            pos2(right, mid + CONTROL_H / 2.0),
        );
        let response = ui.interact(dropdown, Id::new("glass.shortcuts.preset"), Sense::click());
        painter.rect_filled(
            dropdown,
            paint.control_radius(),
            paint.c(if response.hovered() {
                Role::Bg3
            } else {
                Role::Bg2
            }),
        );
        if response.clicked() {
            self.preset_menu = !self.preset_menu;
        }
        paint.text(
            &painter,
            pos2(dropdown.min.x + 8.0, mid),
            Align2::LEFT_CENTER,
            &keymap.preset.label(),
            Type::Label,
            Role::TextPrimary,
            PRESET_W - 80.0,
        );
        let chevron = square(pos2(dropdown.max.x - 8.0 - 12.0, mid), 12.0);
        icon(
            &painter,
            chevron,
            Icon::ChevronDown,
            paint.c(Role::TextMuted),
            2.5,
        );
        if !keymap.changed().is_empty() {
            paint.text(
                &painter,
                pos2(chevron.min.x - 6.0, mid),
                Align2::RIGHT_CENTER,
                &t!("shortcuts.preset.edited"),
                Type::MonoCaption,
                Role::TextMuted,
                f32::INFINITY,
            );
        }
        paint.text(
            &painter,
            pos2(dropdown.min.x - 8.0 - 4.0, mid),
            Align2::RIGHT_CENTER,
            &t!("shortcuts.preset"),
            Type::Label,
            Role::TextMuted,
            f32::INFINITY,
        );
        dropdown
    }

    fn toolbar(&mut self, ui: &mut Ui, paint: &Paint, bar: Rect, keymap: &Keymap, index: &Index) {
        let painter = ui.painter().clone();
        let mid = bar.center().y;
        let field = Rect::from_min_size(
            pos2(bar.min.x + 16.0, mid - CONTROL_H / 2.0),
            vec2(SEARCH_W, CONTROL_H),
        );
        let focused = self.by_key || ui.ctx().memory(|m| m.has_focus(search_id()));
        search_field(paint, &painter, field, focused);
        icon(
            &painter,
            square(pos2(field.min.x + 8.0, mid), ICON),
            Icon::Search,
            paint.c(Role::TextMuted),
            ICON_STROKE,
        );
        let text_left = field.min.x + 8.0 + ICON + 8.0;
        if self.by_key {
            match self.search_chord {
                Some(chord) => {
                    let tokens = chord_tokens(chord, self.platform);
                    keycap(
                        paint,
                        &painter,
                        pos2(text_left, mid),
                        &tokens,
                        Role::TextPrimary,
                        CapEdge::Glass,
                    );
                    let count = index.on(chord.on(self.platform)).len();
                    let found = match count {
                        1 => t!("shortcuts.search.one_command"),
                        n => t!("shortcuts.search.commands", count = n),
                    };
                    paint.text(
                        &painter,
                        pos2(field.max.x - 3.0 - 22.0 - 8.0, mid),
                        Align2::RIGHT_CENTER,
                        &found,
                        Type::MonoCaption,
                        Role::TextMuted,
                        f32::INFINITY,
                    );
                }
                None => {
                    paint.text(
                        &painter,
                        pos2(text_left, mid),
                        Align2::LEFT_CENTER,
                        &t!("shortcuts.search.press_keys"),
                        Type::Label,
                        Role::TextMuted,
                        SEARCH_W - 80.0,
                    );
                }
            }
        } else {
            let hint_tokens = chord_tokens(Chord::key(Key::F).cmd(), self.platform);
            let hint_w = cap_width(paint, &painter, &hint_tokens);
            let hint_left = field.max.x - 4.0 - hint_w;
            keycap(
                paint,
                &painter,
                pos2(hint_left, mid),
                &hint_tokens,
                Role::TextSecondary,
                CapEdge::Glass,
            );
            let text_rect = Rect::from_min_max(
                pos2(text_left, field.min.y),
                pos2(hint_left - 8.0, field.max.y),
            );
            let hint =
                egui::RichText::new(t!("shortcuts.search.hint")).color(paint.c(Role::TextMuted));
            let edit = egui::TextEdit::singleline(&mut self.search)
                .id(search_id())
                .frame(egui::Frame::NONE)
                .font(paint.font(Type::Label))
                .text_color(paint.c(Role::TextPrimary))
                .hint_text(hint)
                .vertical_align(egui::Align::Center)
                .desired_width(text_rect.width());
            if ui.put(text_rect, edit).changed() {
                self.scroll = 0.0;
            }
        }

        // The by-key toggle.
        let toggle = Rect::from_min_size(
            pos2(field.max.x + 8.0, mid - CONTROL_H / 2.0),
            Vec2::splat(CONTROL_H),
        );
        let response = ui.interact(toggle, Id::new("glass.shortcuts.by_key"), Sense::click());
        if self.by_key {
            paint.glass_fill(&painter, toggle, paint.control_radius());
        } else if response.hovered() {
            painter.rect_filled(toggle, paint.control_radius(), paint.c(Role::Bg3));
        }
        icon_keyboard(
            &painter,
            square(toggle.center() - vec2(ICON / 2.0, 0.0), ICON),
            paint.c(Role::TextSecondary),
        );
        if response.clicked() {
            self.by_key = !self.by_key;
            self.search_chord = None;
            self.scroll = 0.0;
            if self.by_key {
                ui.ctx().memory_mut(|m| m.surrender_focus(search_id()));
            }
        }

        // Filters, right-aligned.
        let changed = keymap.changed().len();
        let clashing = COMMANDS.iter().filter(|c| index.has_clash(c.id)).count();
        let items = [
            Segment {
                text: t!("shortcuts.filter.all"),
                tokens: Vec::new(),
                dot: None,
                count: Some(COMMANDS.len()),
                on: self.filter == Filter::All,
            },
            Segment {
                text: t!("shortcuts.filter.changed"),
                tokens: Vec::new(),
                dot: Some(Role::Accent),
                count: Some(changed),
                on: self.filter == Filter::Changed,
            },
            Segment {
                text: t!("shortcuts.filter.clashes"),
                tokens: Vec::new(),
                dot: Some(Role::StateWarning),
                count: Some(clashing),
                on: self.filter == Filter::Clashes,
            },
        ];
        let w = segmented_width(paint, &painter, &items);
        if let Some(i) = segmented(ui, paint, pos2(bar.max.x - 16.0 - w, mid), &items, "filter") {
            self.set_filter([Filter::All, Filter::Changed, Filter::Clashes][i]);
        }
    }

    fn left_column(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        column: Rect,
        keymap: &mut Keymap,
        index: &Index,
        out: &mut ShortcutsResponse,
    ) {
        let painter = ui.painter().clone();
        let x0 = column.min.x + PAD;
        let inner_w = column.width() - 2.0 * PAD;
        let mut y = column.min.y + PAD;
        let platform = self.platform;

        // Keys held.
        let mid = y + SEGMENT_H / 2.0;
        let caps = paint.text(
            &painter,
            pos2(x0, mid),
            Align2::LEFT_CENTER,
            &t!("shortcuts.held"),
            Type::Caps,
            Role::TextMuted,
            f32::INFINITY,
        );
        let held = held_mods(ui.ctx(), platform);
        let layer = if held.is_none() { self.layer } else { held };
        let layers = layer_choices(platform);
        let items: Vec<Segment> = layers
            .iter()
            .map(|m| Segment {
                text: if m.is_none() {
                    t!("shortcuts.held.none")
                } else {
                    String::new()
                },
                tokens: if m.is_none() {
                    Vec::new()
                } else {
                    glyphs::mod_tokens(*m, platform)
                },
                dot: None,
                count: None,
                on: *m == layer,
            })
            .collect();
        if let Some(i) = segmented(ui, paint, pos2(caps.max.x + 12.0, mid), &items, "layer") {
            self.layer = layers[i];
        }
        paint.text(
            &painter,
            pos2(x0 + inner_w, mid),
            Align2::RIGHT_CENTER,
            &t!("shortcuts.held.hint"),
            Type::Caption,
            Role::TextMuted,
            f32::INFINITY,
        );
        y += SEGMENT_H + PAD;

        // The keyboard.
        let state = keyboard::MapState {
            platform,
            layer,
            selected_key: self.selected_key,
            index,
        };
        if let Some(key) = keyboard::draw(ui, paint, pos2(x0, y), &state) {
            self.selected_key = Some(key);
            let chord = Chord { mods: layer, key }.on(platform);
            if let Some(id) = index.on(chord).first() {
                self.selected = id;
            }
        }
        y += keyboard::HEIGHT + PAD;

        // Legend.
        let cap_h = paint.line_height(Type::Caption);
        let lmid = y + cap_h / 2.0;
        let mut x = x0;
        for group in crate::shortcuts::Group::ALL {
            x = legend(paint, &painter, x, lmid, group.role(), &group.label()) + 16.0;
        }
        let mut rx = x0 + inner_w;
        for (role, label) in [
            (Role::StateWarning, t!("shortcuts.legend.clash")),
            (Role::Accent, t!("shortcuts.legend.changed")),
        ] {
            let w = 6.0 + 6.0 + paint.width(&painter, &label, Type::Caption);
            legend(paint, &painter, rx - w, lmid, role, &label);
            rx -= w + 16.0;
        }
        y += cap_h + PAD;

        // What the selected key does in every layer.
        if let Some(key) = self.selected_key {
            let key_tokens = vec![glyphs::key_token(key, platform)];
            let name = token_text(&key_tokens);
            let header_h = paint.line_height(Type::Label);
            let hmid = y + header_h / 2.0;
            let caps = paint.text(
                &painter,
                pos2(x0, hmid),
                Align2::LEFT_CENTER,
                &t!("shortcuts.key_detail.title", key = name),
                Type::Caps,
                Role::TextMuted,
                f32::INFINITY,
            );
            paint.text(
                &painter,
                pos2(caps.max.x + 8.0, hmid),
                Align2::LEFT_CENTER,
                &t!("shortcuts.key_detail.body", key = name),
                Type::Label,
                Role::TextMuted,
                f32::INFINITY,
            );
            y += header_h + 10.0;
            let n = layers.len() as f32;
            let card_w = (inner_w - 8.0 * (n - 1.0)) / n;
            for (i, m) in layers.iter().enumerate() {
                let card = Rect::from_min_size(
                    pos2(x0 + i as f32 * (card_w + 8.0), y),
                    vec2(card_w, DETAIL_CARD_H),
                );
                painter.rect_filled(card, paint.control_radius(), paint.c(Role::Bg1));
                painter.rect_stroke(
                    card,
                    paint.control_radius(),
                    Stroke::new(1.0, paint.c(Role::BorderSubtle)),
                    StrokeKind::Inside,
                );
                let caps_mid = card.min.y + 8.0 + glyphs::CAP_H / 2.0;
                let mut cx = card.min.x + 10.0;
                for token in glyphs::mod_tokens(*m, platform)
                    .into_iter()
                    .chain(std::iter::once(glyphs::key_token(key, platform)))
                {
                    let r = keycap(
                        paint,
                        &painter,
                        pos2(cx, caps_mid),
                        &[token],
                        Role::TextSecondary,
                        CapEdge::Glass,
                    );
                    cx = r.max.x + 4.0;
                }
                let chord = Chord { mods: *m, key }.on(platform);
                let label_mid = card.max.y - 8.0 - paint.line_height(Type::Label) / 2.0;
                match index.on(chord).first().and_then(|id| command(id)) {
                    Some(c) => paint.text(
                        &painter,
                        pos2(card.min.x + 10.0, label_mid),
                        Align2::LEFT_CENTER,
                        &c.label(),
                        Type::Label,
                        Role::TextPrimary,
                        card_w - 20.0,
                    ),
                    None => paint.text(
                        &painter,
                        pos2(card.min.x + 10.0, label_mid),
                        Align2::LEFT_CENTER,
                        &t!("shortcuts.free"),
                        Type::Label,
                        Role::TextMuted,
                        card_w - 20.0,
                    ),
                };
            }
            y += DETAIL_CARD_H + PAD;
        }

        // Your changes.
        let changed = keymap.changed();
        if !changed.is_empty() {
            let header_h = paint.line_height(Type::Label);
            let hmid = y + header_h / 2.0;
            let caps = paint.text(
                &painter,
                pos2(x0, hmid),
                Align2::LEFT_CENTER,
                &t!("shortcuts.changes.title"),
                Type::Caps,
                Role::TextMuted,
                f32::INFINITY,
            );
            paint.text(
                &painter,
                pos2(caps.max.x + 8.0, hmid),
                Align2::LEFT_CENTER,
                &t!(
                    "shortcuts.changes.body",
                    count = changed.len(),
                    preset = keymap.preset.label()
                ),
                Type::Label,
                Role::TextMuted,
                f32::INFINITY,
            );
            y += header_h + 8.0;
            let mut reset = None;
            for id in &changed {
                let Some(c) = command(id) else { continue };
                let row = Rect::from_min_size(pos2(x0, y), vec2(inner_w, ROW));
                let rmid = row.center().y;
                paint.dot(&painter, pos2(x0 + 3.0, rmid), 6.0, paint.c(Role::Accent));
                paint.text(
                    &painter,
                    pos2(x0 + 6.0 + 8.0, rmid),
                    Align2::LEFT_CENTER,
                    &c.label(),
                    Type::Label,
                    Role::TextPrimary,
                    inner_w / 2.0,
                );
                let button =
                    Rect::from_min_size(pos2(row.max.x - 24.0, rmid - 12.0), Vec2::splat(24.0));
                if self.ghost_icon(ui, paint, button, id).clicked() {
                    reset = Some(*id);
                }
                icon_reset(
                    &painter,
                    square(button.center() - vec2(SMALL_ICON / 2.0, 0.0), SMALL_ICON),
                    paint.c(Role::TextSecondary),
                );
                // Old keys → new keys, right-aligned before the reset button.
                let new = chords_tokens_or_none(keymap.chords(id), platform);
                let old = chords_tokens_or_none(keymap.preset_chords(id), platform);
                let mut rx = button.min.x - 8.0;
                for (i, tokens) in new.iter().enumerate().rev() {
                    let w = cap_width(paint, &painter, tokens);
                    keycap(
                        paint,
                        &painter,
                        pos2(rx - w, rmid),
                        tokens,
                        Role::TextPrimary,
                        CapEdge::Glass,
                    );
                    rx -= w + if i == 0 { 6.0 } else { 4.0 };
                }
                icon_arrow_right(
                    &painter,
                    square(pos2(rx - 12.0, rmid), 12.0),
                    paint.c(Role::TextMuted),
                );
                rx -= 12.0 + 6.0;
                for tokens in old.iter().rev() {
                    let w = cap_width(paint, &painter, tokens);
                    keycap(
                        paint,
                        &painter,
                        pos2(rx - w, rmid),
                        tokens,
                        Role::TextMuted,
                        CapEdge::Glass,
                    );
                    rx -= w + 4.0;
                }
                y += ROW;
            }
            if let Some(id) = reset {
                self.edit(keymap, |k, _| k.reset(id));
                out.changed = true;
            }
            y += PAD;
        }

        // Kept by the system.
        let header_h = paint.line_height(Type::Label);
        let hmid = y + header_h / 2.0;
        let title = match platform {
            Platform::Mac => t!("shortcuts.reserved.mac"),
            Platform::Windows => t!("shortcuts.reserved.windows"),
            Platform::Linux => t!("shortcuts.reserved.linux"),
        };
        let caps = paint.text(
            &painter,
            pos2(x0, hmid),
            Align2::LEFT_CENTER,
            &title,
            Type::Caps,
            Role::TextMuted,
            f32::INFINITY,
        );
        paint.text(
            &painter,
            pos2(caps.max.x + 8.0, hmid),
            Align2::LEFT_CENTER,
            &t!("shortcuts.reserved.body"),
            Type::Label,
            Role::TextMuted,
            f32::INFINITY,
        );
        y += header_h + 8.0;
        let rmid = y + glyphs::CAP_H / 2.0;
        icon_lock(
            &painter,
            square(pos2(x0, rmid), SMALL_ICON),
            paint.c(Role::TextMuted),
        );
        let mut cx = x0 + SMALL_ICON + 6.0;
        for chord in reserved(platform) {
            let r = keycap_faded(
                paint,
                &painter,
                pos2(cx, rmid),
                &chord_tokens(*chord, platform),
            );
            cx = r.max.x + 6.0;
        }
    }

    fn footer(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        footer: Rect,
        keymap: &mut Keymap,
        index: &Index,
        out: &mut ShortcutsResponse,
    ) {
        let painter = ui.painter().clone();
        let mid = footer.center().y;
        let clashes = match index.conflicts {
            1 => t!("shortcuts.footer.one_clash"),
            n => t!("shortcuts.footer.clashes", count = n),
        };
        paint.text(
            &painter,
            pos2(footer.min.x + 16.0, mid),
            Align2::LEFT_CENTER,
            &t!(
                "shortcuts.footer.summary",
                count = COMMANDS.len(),
                changed = keymap.changed().len(),
                preset = keymap.preset.label(),
                clashes = clashes
            ),
            Type::MonoCaption,
            Role::TextMuted,
            footer.width() / 2.0,
        );
        let done = t!("shortcuts.done");
        let done_w = paint.width(&painter, &done, Type::LabelMedium) + 24.0;
        let done_rect = Rect::from_min_size(
            pos2(
                footer.max.x - 16.0 - done_w,
                mid - size::DIALOG_BUTTON / 2.0,
            ),
            vec2(done_w, size::DIALOG_BUTTON),
        );
        if paint.button(ui, done_rect, &done, true, true).clicked() {
            out.action = Some(ShortcutsAction::Close);
        }
        let reset = t!("shortcuts.reset_all", preset = keymap.preset.label());
        let reset_w = paint.width(&painter, &reset, Type::LabelMedium) + 24.0;
        let reset_rect = Rect::from_min_size(
            pos2(done_rect.min.x - 8.0 - reset_w, done_rect.min.y),
            vec2(reset_w, size::DIALOG_BUTTON),
        );
        if paint.ghost_button(ui, reset_rect, &reset).clicked() && !keymap.overrides.is_empty() {
            self.edit(keymap, |k, _| k.reset_all());
            out.changed = true;
        }
    }

    /// The preset menu under the dropdown; returns whether the keymap changed.
    fn preset_menu(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        dropdown: Rect,
        keymap: &mut Keymap,
    ) -> bool {
        let painter = ui.painter().clone();
        let header_h = 24.0;
        let h = 4.0 + header_h + PresetId::ALL.len() as f32 * ROW + 4.0;
        let menu = Rect::from_min_size(
            pos2(dropdown.max.x - MENU_W, dropdown.max.y + 4.0),
            vec2(MENU_W, h),
        );
        let shield = ui.interact(menu, Id::new("glass.shortcuts.preset_menu"), Sense::click());
        paint.floating(&painter, menu);
        paint.text(
            &painter,
            pos2(menu.min.x + 4.0 + 8.0, menu.min.y + 4.0 + header_h / 2.0),
            Align2::LEFT_CENTER,
            &t!("shortcuts.preset.start_from"),
            Type::Caps,
            Role::TextMuted,
            f32::INFINITY,
        );
        let mut changed = false;
        let mut y = menu.min.y + 4.0 + header_h;
        for preset in PresetId::ALL {
            let row = Rect::from_min_size(pos2(menu.min.x + 4.0, y), vec2(MENU_W - 8.0, ROW));
            let response = ui.interact(
                row,
                Id::new(("glass.shortcuts.preset_item", preset)),
                Sense::click(),
            );
            if response.hovered() {
                paint.glass_fill(&painter, row, paint.control_radius());
            }
            let mid = row.center().y;
            if preset == keymap.preset {
                icon(
                    &painter,
                    square(pos2(row.min.x + 8.0, mid), ICON),
                    Icon::Check,
                    paint.c(Role::TextPrimary),
                    ICON_STROKE,
                );
                if !keymap.changed().is_empty() {
                    paint.text(
                        &painter,
                        pos2(row.max.x - 8.0, mid),
                        Align2::RIGHT_CENTER,
                        &t!("shortcuts.preset.edited"),
                        Type::MonoCaption,
                        Role::TextMuted,
                        f32::INFINITY,
                    );
                }
            }
            paint.text(
                &painter,
                pos2(row.min.x + 8.0 + ICON + 8.0, mid),
                Align2::LEFT_CENTER,
                &preset.label(),
                Type::Label,
                Role::TextPrimary,
                MENU_W - 100.0,
            );
            if response.clicked() {
                if preset != keymap.preset {
                    self.edit(keymap, |k, _| k.switch_preset(preset, true));
                    changed = true;
                }
                self.preset_menu = false;
            }
            y += ROW;
        }
        // A click anywhere else closes the menu.
        let clicked_outside = ui.input(|i| i.pointer.any_click())
            && !shield.hovered()
            && !ui.input(|i| {
                i.pointer
                    .interact_pos()
                    .is_some_and(|p| dropdown.contains(p))
            });
        if clicked_outside {
            self.preset_menu = false;
        }
        changed
    }

    /// A ghost hit area that fills on hover.
    fn ghost_icon(&self, ui: &mut Ui, paint: &Paint, rect: Rect, name: &str) -> egui::Response {
        let response = ui.interact(
            rect,
            Id::new(("glass.shortcuts.button", name)),
            Sense::click(),
        );
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, paint.control_radius(), paint.c(Role::Bg3));
        }
        response
    }
}

#[derive(Clone, Copy)]
enum BarIcon {
    Import,
    Export,
}

fn act_id(action: ShortcutsAction) -> &'static str {
    match action {
        ShortcutsAction::Import => "import",
        ShortcutsAction::Export => "export",
        ShortcutsAction::Close => "close",
    }
}

fn search_id() -> Id {
    Id::new("glass.shortcuts.search")
}

/// The left column's height: keys held, keyboard, legend, key detail, `changes` rows of
/// changes (the section is left out at 0) and the system's keys.
fn left_column_height(changes: usize) -> f32 {
    let label = 16.0;
    let caption = 14.0;
    let mut h = PAD + SEGMENT_H + PAD + keyboard::HEIGHT + PAD + caption + PAD;
    h += label + 10.0 + DETAIL_CARD_H + PAD;
    if changes > 0 {
        h += label + 8.0 + changes as f32 * ROW + PAD;
    }
    h + label + 8.0 + glyphs::CAP_H + PAD
}

/// The modifiers held right now as a chord's modifiers.
fn held_mods(ctx: &egui::Context, platform: Platform) -> Mods {
    let m = ctx.input(|i| i.modifiers);
    Chord::from_egui(Key::A, m, platform).mods
}

/// The layers the editor offers: none, ⇧, ⌥, ⌘ and on macOS ⌃.
fn layer_choices(platform: Platform) -> Vec<Mods> {
    let mut out = vec![
        Mods::NONE,
        Mods {
            shift: true,
            ..Mods::NONE
        },
        Mods {
            alt: true,
            ..Mods::NONE
        },
        Mods {
            cmd: true,
            ..Mods::NONE
        },
    ];
    if platform.is_mac() {
        out.push(Mods {
            ctrl: true,
            ..Mods::NONE
        });
    }
    out
}

fn token_text(tokens: &[Token]) -> String {
    tokens
        .iter()
        .filter_map(|t| match t {
            Token::Text(s) => Some(s.as_str()),
            Token::Glyph(_) => None,
        })
        .collect()
}

/// Each chord's tokens, or a dash for none.
fn chords_tokens_or_none(chords: &[Chord], platform: Platform) -> Vec<Vec<Token>> {
    if chords.is_empty() {
        vec![vec![Token::Text(t!("shortcuts.none"))]]
    } else {
        chords.iter().map(|c| chord_tokens(*c, platform)).collect()
    }
}

/// A square of `side` whose left edge is at `left_mid`, vertically centred on it.
fn square(left_mid: Pos2, side: f32) -> Rect {
    Rect::from_min_size(pos2(left_mid.x, left_mid.y - side / 2.0), Vec2::splat(side))
}

/// The window's floating surface: shadow, `bg/2`, and the glass gradient over the top
/// 35% only (the board's `glass/top 0%` to `glass/bottom 35%`), so a tall window shows
/// no step halfway down.
fn window_surface(paint: &Paint, painter: &Painter, rect: Rect) {
    let corner = paint.floating_radius();
    let shadow = &paint.theme.tokens().rules.floating_shadow;
    painter.add(
        egui::epaint::Shadow {
            offset: [0, shadow.offset_y_px.round().clamp(-128.0, 127.0) as i8],
            blur: shadow.blur_px.round().clamp(0.0, 255.0) as u8,
            spread: 0,
            color: paint.c(Role::GlassShadow),
        }
        .as_shape(rect, corner),
    );
    painter.rect_filled(rect, corner, paint.c(Role::Bg2));
    painter.rect_filled(rect, corner, paint.c(Role::GlassBottom));
    let r = f32::from(corner.nw);
    let top = Rect::from_min_size(rect.min, vec2(rect.width(), r));
    painter.rect_filled(
        top,
        CornerRadius {
            sw: 0,
            se: 0,
            ..corner
        },
        paint.c(Role::GlassTop),
    );
    let fade = Rect::from_min_max(
        pos2(rect.min.x, top.max.y),
        pos2(rect.max.x, rect.min.y + 0.35 * rect.height()),
    );
    let mut mesh = egui::Mesh::default();
    let (from, to) = (paint.c(Role::GlassTop), egui::Color32::TRANSPARENT);
    mesh.colored_vertex(fade.left_top(), from);
    mesh.colored_vertex(fade.right_top(), from);
    mesh.colored_vertex(fade.right_bottom(), to);
    mesh.colored_vertex(fade.left_bottom(), to);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
    painter.line_segment(
        [
            pos2(rect.min.x + r, rect.min.y + 0.5),
            pos2(rect.max.x - r, rect.min.y + 0.5),
        ],
        Stroke::new(1.0, paint.c(Role::GlassHighlight)),
    );
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, paint.c(Role::GlassEdge)),
        StrokeKind::Inside,
    );
}

/// The search field's surface: `bg/2`, with the accent ring and soft halo when focused.
fn search_field(paint: &Paint, painter: &Painter, rect: Rect, focused: bool) {
    let corner = paint.control_radius();
    painter.rect_filled(rect, corner, paint.c(Role::Bg2));
    if focused {
        focus_ring(
            paint,
            painter,
            rect,
            Role::Accent,
            paint.c(Role::AccentSoft),
        );
    }
}

/// The board's focus treatment: a 1 px inset ring and a 3 px outer halo.
fn focus_ring(paint: &Paint, painter: &Painter, rect: Rect, ring: Role, halo: egui::Color32) {
    let corner = paint.control_radius();
    painter.rect_stroke(rect, corner, Stroke::new(3.0, halo), StrokeKind::Outside);
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, paint.c(ring)),
        StrokeKind::Inside,
    );
}

/// A legend entry: a 6 pt dot and a caption; returns the right edge.
fn legend(paint: &Paint, painter: &Painter, x: f32, mid: f32, role: Role, label: &str) -> f32 {
    paint.dot(painter, pos2(x + 3.0, mid), 6.0, paint.c(role));
    paint
        .text(
            painter,
            pos2(x + 12.0, mid),
            Align2::LEFT_CENTER,
            label,
            Type::Caption,
            Role::TextSecondary,
            f32::INFINITY,
        )
        .max
        .x
}

/// One segment of a segmented control.
struct Segment {
    text: String,
    tokens: Vec<Token>,
    dot: Option<Role>,
    count: Option<usize>,
    on: bool,
}

fn segment_width(paint: &Paint, painter: &Painter, s: &Segment) -> f32 {
    let mut w = 16.0;
    let mut parts = 0;
    if s.dot.is_some() {
        w += 6.0;
        parts += 1;
    }
    if !s.text.is_empty() {
        let ty = if s.on { Type::LabelMedium } else { Type::Label };
        w += paint.width(painter, &s.text, ty);
        parts += 1;
    }
    if !s.tokens.is_empty() {
        w += glyphs::tokens_width(paint, painter, &s.tokens);
        parts += 1;
    }
    if let Some(n) = s.count {
        w += paint.width(painter, &n.to_string(), Type::MonoCaption);
        parts += 1;
    }
    w + 6.0 * (parts.max(1) - 1) as f32
}

fn segmented_width(paint: &Paint, painter: &Painter, items: &[Segment]) -> f32 {
    4.0 + items
        .iter()
        .map(|s| segment_width(paint, painter, s))
        .sum::<f32>()
        + 2.0 * (items.len() - 1) as f32
}

/// The board's segmented control (`bg/0` track, glass selected segment); returns the
/// segment clicked.
fn segmented(
    ui: &mut Ui,
    paint: &Paint,
    left_mid: Pos2,
    items: &[Segment],
    name: &str,
) -> Option<usize> {
    let painter = ui.painter().clone();
    let w = segmented_width(paint, &painter, items);
    let track = Rect::from_min_size(
        pos2(left_mid.x, left_mid.y - SEGMENT_H / 2.0 - 2.0),
        vec2(w, SEGMENT_H + 4.0),
    );
    let corner = paint.control_radius();
    painter.rect_filled(track, corner, paint.c(Role::Bg0));
    painter.rect_stroke(
        track,
        corner,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        StrokeKind::Inside,
    );
    let mut x = track.min.x + 2.0;
    let mut clicked = None;
    for (i, s) in items.iter().enumerate() {
        let sw = segment_width(paint, &painter, s);
        let r = Rect::from_min_size(pos2(x, left_mid.y - SEGMENT_H / 2.0), vec2(sw, SEGMENT_H));
        let response = ui.interact(
            r,
            Id::new(("glass.shortcuts.segment", name, i)),
            Sense::click(),
        );
        if s.on {
            paint.glass_fill(&painter, r, corner);
        } else if response.hovered() {
            painter.rect_filled(r, corner, paint.c(Role::Bg2));
        }
        let mut cx = r.min.x + 8.0;
        if let Some(role) = s.dot {
            paint.dot(&painter, pos2(cx + 3.0, left_mid.y), 6.0, paint.c(role));
            cx += 6.0 + 6.0;
        }
        let role = if s.on {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        };
        if !s.text.is_empty() {
            let ty = if s.on { Type::LabelMedium } else { Type::Label };
            cx = paint
                .text(
                    &painter,
                    pos2(cx, left_mid.y),
                    Align2::LEFT_CENTER,
                    &s.text,
                    ty,
                    role,
                    f32::INFINITY,
                )
                .max
                .x
                + 6.0;
        }
        if !s.tokens.is_empty() {
            cx = draw_tokens(paint, &painter, cx, left_mid.y, &s.tokens, paint.c(role)) + 6.0;
        }
        if let Some(n) = s.count {
            paint.text(
                &painter,
                pos2(cx, left_mid.y),
                Align2::LEFT_CENTER,
                &n.to_string(),
                Type::MonoCaption,
                Role::TextMuted,
                f32::INFINITY,
            );
        }
        if response.clicked() {
            clicked = Some(i);
        }
        x += sw + 2.0;
    }
    clicked
}

/// Maps the board's 24-unit icon grid onto `rect`.
fn grid(rect: Rect) -> impl Fn(f32, f32) -> Pos2 {
    let k = rect.width() / 24.0;
    move |x, y| pos2(rect.min.x + x * k, rect.min.y + y * k)
}

fn stroke(rect: Rect, color: egui::Color32) -> Stroke {
    Stroke::new(ICON_STROKE * rect.width() / 24.0, color)
}

fn arc(g: &impl Fn(f32, f32) -> Pos2, c: (f32, f32), r: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    const STEPS: usize = 24;
    (0..=STEPS)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / STEPS as f32;
            g(c.0 + r * a.cos(), c.1 + r * a.sin())
        })
        .collect()
}

/// `Icon / keyboard`: a rounded key frame, two rows of dots and a space bar.
fn icon_keyboard(painter: &Painter, rect: Rect, color: egui::Color32) {
    let g = grid(rect);
    let st = stroke(rect, color);
    painter.rect_stroke(
        Rect::from_min_max(g(2.0, 4.0), g(22.0, 20.0)),
        CornerRadius::same((2.0 * rect.width() / 24.0).round() as u8),
        st,
        StrokeKind::Middle,
    );
    for (x, y) in [
        (6.0, 8.0),
        (10.0, 8.0),
        (14.0, 8.0),
        (18.0, 8.0),
        (8.0, 12.0),
        (12.0, 12.0),
        (16.0, 12.0),
    ] {
        painter.circle_filled(g(x, y), st.width * 0.55, color);
    }
    painter.line_segment([g(7.0, 16.0), g(17.0, 16.0)], st);
}

/// `Icon / upload`: a tray with an arrow out of it.
fn icon_upload(painter: &Painter, rect: Rect, color: egui::Color32) {
    let g = grid(rect);
    let st = stroke(rect, color);
    painter.add(Shape::line(
        vec![
            g(21.0, 15.0),
            g(21.0, 19.0),
            g(19.0, 21.0),
            g(5.0, 21.0),
            g(3.0, 19.0),
            g(3.0, 15.0),
        ],
        st,
    ));
    painter.add(Shape::line(
        vec![g(17.0, 8.0), g(12.0, 3.0), g(7.0, 8.0)],
        st,
    ));
    painter.line_segment([g(12.0, 3.0), g(12.0, 15.0)], st);
}

fn icon_arrow_right(painter: &Painter, rect: Rect, color: egui::Color32) {
    let g = grid(rect);
    let st = Stroke::new(2.5 * rect.width() / 24.0, color);
    painter.line_segment([g(5.0, 12.0), g(19.0, 12.0)], st);
    painter.add(Shape::line(
        vec![g(12.0, 5.0), g(19.0, 12.0), g(12.0, 19.0)],
        st,
    ));
}

/// `Icon / reset`: a counter-clockwise arrow round a circle.
fn icon_reset(painter: &Painter, rect: Rect, color: egui::Color32) {
    use std::f32::consts::{FRAC_PI_2, PI};
    let g = grid(rect);
    let st = stroke(rect, color);
    let mut points = arc(&g, (12.0, 12.0), 9.0, PI, -FRAC_PI_2);
    points.extend([g(8.5, 3.7), g(5.26, 5.74), g(3.0, 8.0)]);
    painter.add(Shape::line(points, st));
    painter.add(Shape::line(vec![g(3.0, 3.0), g(3.0, 8.0), g(8.0, 8.0)], st));
}

/// `Icon / lock`.
fn icon_lock(painter: &Painter, rect: Rect, color: egui::Color32) {
    use std::f32::consts::PI;
    let g = grid(rect);
    let st = stroke(rect, color);
    painter.rect_stroke(
        Rect::from_min_max(g(3.0, 11.0), g(21.0, 22.0)),
        CornerRadius::same((2.0 * rect.width() / 24.0).round() as u8),
        st,
        StrokeKind::Middle,
    );
    let mut shackle = vec![g(7.0, 11.0)];
    shackle.extend(arc(&g, (12.0, 7.0), 5.0, PI, 2.0 * PI));
    shackle.push(g(17.0, 11.0));
    painter.add(Shape::line(shackle, st));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_layout_adds_up() {
        assert_eq!(keyboard::width(Platform::Mac), LEFT_W - 2.0 * PAD);
        assert_eq!(keyboard::width(Platform::Windows), LEFT_W - 2.0 * PAD);
        assert_eq!(left_column_height(1), 586.0);
    }

    #[test]
    fn recording_a_free_key_applies_it_and_undo_restores() {
        let mut keymap = Keymap::default();
        let mut editor = ShortcutsEditor::new(Platform::Mac);
        editor.start_recording(&keymap, "edit.freeze_frame", false);
        editor.capture(Chord::key(Key::F));
        assert!(editor.keep(&mut keymap));
        assert_eq!(keymap.chords("edit.freeze_frame"), &[Chord::key(Key::F)]);
        assert!(editor.undo(&mut keymap));
        assert!(keymap.changed().is_empty());
        assert!(editor.redo(&mut keymap));
        assert_eq!(keymap.changed(), vec!["edit.freeze_frame"]);
    }

    #[test]
    fn a_taken_key_waits_for_a_choice() {
        let mut keymap = Keymap::default();
        let mut editor = ShortcutsEditor::new(Platform::Mac);
        editor.start_recording(&keymap, "view.snapping", false);
        editor.capture(Chord::key(Key::S));
        assert!(
            !editor.keep(&mut keymap),
            "nothing changes before the choice"
        );
        assert_eq!(
            editor.pending_clash().unwrap().others,
            vec!["tool.rate_stretch"]
        );
        assert!(editor.resolve_clash(&mut keymap, Some(true)));
        assert_eq!(keymap.chords("view.snapping"), &[Chord::key(Key::S)]);
        assert_eq!(
            keymap.chords("tool.rate_stretch"),
            &[Chord::key(Key::S).shift()]
        );
        assert!(keymap.conflicts(Platform::Mac).is_empty());
    }

    #[test]
    fn reserved_keys_are_never_captured() {
        let keymap = Keymap::default();
        let mut editor = ShortcutsEditor::new(Platform::Mac);
        editor.start_recording(&keymap, "edit.save", false);
        editor.capture(Chord::key(Key::Q).cmd());
        assert_eq!(editor.recording().unwrap().captured, None);
    }
}
