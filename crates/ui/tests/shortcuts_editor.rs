//! The keyboard shortcuts editor: recording, clashes and Escape driven by key events on
//! a real egui context, and every state within the frame budget.

mod support;

use egui::{Event, Key, Modifiers, Pos2, RawInput, Rect, pos2, vec2};
use support::FrameBudget;
use ui::dialogs::shortcuts::{Filter, ShortcutsAction, ShortcutsEditor, ShortcutsResponse, WIDTH};
use ui::headless::{HeadlessUi, Viewport};
use ui::shortcuts::{Chord, Keymap, Mods, Platform, PresetId};
use ui::theme::{self, ThemeKind};

struct Driver {
    ctx: egui::Context,
    editor: ShortcutsEditor,
    keymap: Keymap,
}

impl Driver {
    /// `None` when no font is available to lay out text.
    fn new() -> Option<Self> {
        let ctx = egui::Context::default();
        if !ui::fonts::install(&ctx) {
            eprintln!("skipping: no font found");
            return None;
        }
        theme::set(&ctx, ThemeKind::Dark);
        let mut driver = Self {
            ctx,
            editor: ShortcutsEditor::new(Platform::Mac),
            keymap: Keymap::default(),
        };
        driver.frame(Vec::new());
        Some(driver)
    }

    fn frame(&mut self, events: Vec<Event>) -> ShortcutsResponse {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440.0, 900.0))),
            events,
            ..RawInput::default()
        };
        let mut response = ShortcutsResponse::default();
        let mut output = self.ctx.run_ui(input, |ui| {
            let height = self.editor.height(&self.keymap);
            let rect = Rect::from_min_size(pos2(80.0, 40.0), vec2(WIDTH, height));
            response = self.editor.show(ui, rect, &mut self.keymap);
        });
        output.textures_delta.clear();
        response
    }

    fn press(&mut self, key: Key, modifiers: Modifiers) -> ShortcutsResponse {
        let event = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        self.frame(vec![event(true), event(false)])
    }
}

#[test]
fn return_records_and_keeps_a_free_key() {
    let Some(mut d) = Driver::new() else { return };
    d.editor.select(&d.keymap, "edit.freeze_frame");
    assert!(!d.press(Key::Enter, Modifiers::NONE).changed);
    assert!(d.editor.recording().is_some());
    d.press(Key::F, Modifiers::NONE);
    assert_eq!(
        d.editor.recording().unwrap().captured,
        Some(Chord::key(Key::F))
    );
    assert!(d.press(Key::Enter, Modifiers::NONE).changed);
    assert_eq!(d.keymap.chords("edit.freeze_frame"), &[Chord::key(Key::F)]);
    // ⌘Z undoes it as one step.
    assert!(
        d.press(Key::Z, Modifiers::COMMAND | Modifiers::MAC_CMD)
            .changed
    );
    assert!(d.keymap.changed().is_empty());
}

#[test]
fn a_taken_key_is_flagged_and_escape_cancels() {
    let Some(mut d) = Driver::new() else { return };
    d.editor.select(&d.keymap, "view.snapping");
    d.press(Key::Enter, Modifiers::NONE);
    d.press(Key::S, Modifiers::NONE);
    assert!(!d.press(Key::Enter, Modifiers::NONE).changed);
    let clash = d.editor.pending_clash().expect("clash").clone();
    assert_eq!(clash.others, vec!["tool.rate_stretch"]);
    d.frame(Vec::new());
    assert_eq!(d.press(Key::Escape, Modifiers::NONE).action, None);
    assert!(d.editor.pending_clash().is_none());
    assert_eq!(d.keymap, Keymap::default(), "nothing changed");
    assert_eq!(
        d.press(Key::Escape, Modifiers::NONE).action,
        Some(ShortcutsAction::Close)
    );
}

#[test]
fn reserved_and_modifier_keys_are_not_captured() {
    let Some(mut d) = Driver::new() else { return };
    d.editor.select(&d.keymap, "edit.save");
    d.press(Key::Enter, Modifiers::NONE);
    d.press(Key::Q, Modifiers::COMMAND | Modifiers::MAC_CMD);
    d.press(Key::ShiftLeft, Modifiers::SHIFT);
    assert_eq!(d.editor.recording().unwrap().captured, None);
}

const STATES: usize = 8;
const STATE_FRAMES: usize = 20;

#[test]
fn every_state_within_the_frame_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip(
        "shortcuts editor",
        Viewport {
            width_px: 1440,
            height_px: 900,
            pixels_per_point: 1.0,
        },
    ) else {
        return;
    };
    for kind in ThemeKind::ALL {
        theme::set(headless.context(), kind);
        let mut editor = ShortcutsEditor::new(Platform::Mac);
        let mut keymap = Keymap::default();
        keymap.set("edit.freeze_frame", vec![Chord::key(Key::F)]);
        // The warm-up shows every state once, so new glyphs and text layouts are
        // cached before timing (as they are after the window's first second).
        let stats = FrameBudget::run(&mut headless, STATES * STATE_FRAMES, 240, |ui, frame| {
            // A new state every 20 frames: plain, a layer, recording, a clash, the
            // preset menu, a key search, the changed filter, another preset.
            match (frame / STATE_FRAMES) % STATES {
                0 => editor = ShortcutsEditor::new(Platform::Mac),
                1 => editor.set_layer(Mods {
                    cmd: true,
                    ..Mods::NONE
                }),
                2 => {
                    if editor.recording().is_none() {
                        editor.start_recording(&keymap, "tool.roll", false);
                        editor.capture(Chord::key(Key::J).ctrl().alt());
                    }
                }
                3 => {
                    if editor.pending_clash().is_none() {
                        editor.start_recording(&keymap, "view.snapping", false);
                        editor.capture(Chord::key(Key::S));
                        editor.keep(&mut keymap);
                    }
                }
                4 => editor.set_preset_menu(true),
                5 => editor.set_search_chord(Some(Chord::key(Key::Z).shift())),
                6 => editor.set_filter(Filter::Changed),
                _ => keymap.switch_preset(PresetId::Premiere, true),
            }
            egui::CentralPanel::default().show(ui, |_| {});
            let height = editor.height(&keymap);
            let rect = Rect::from_min_size(pos2(80.0, 40.0), vec2(WIDTH, height));
            editor.show(ui, rect, &mut keymap);
        })
        .expect("headless frames");
        eprintln!(
            "shortcuts editor {kind:?} ({}): {stats}",
            headless.adapter()
        );
        stats.assert_within_budget(&headless, "shortcuts editor");
    }
}
