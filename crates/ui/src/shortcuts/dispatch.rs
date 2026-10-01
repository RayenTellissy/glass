//! Turning egui key events into command ids through the active keymap.
//!
//! The table is built once per keymap change, so a key press is one hash lookup. When
//! two commands share a chord (a clash the editor flags), the one listed first in
//! [`COMMANDS`] runs. A held key's repeats run only commands that repeat (frame steps,
//! nudges, zoom).

use std::collections::HashMap;

use egui::{Event, Key};

use super::commands::{COMMANDS, Command};
use super::keymap::{Chord, Keymap, Platform};

/// The active keymap as a lookup table.
#[derive(Clone, Debug)]
pub struct Dispatcher {
    platform: Platform,
    table: HashMap<Chord, &'static Command>,
}

impl Dispatcher {
    pub fn new(keymap: &Keymap, platform: Platform) -> Self {
        let mut table = HashMap::new();
        for c in COMMANDS {
            for chord in keymap.chords(c.id) {
                table.entry(chord.on(platform)).or_insert(c);
            }
        }
        Self { platform, table }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The command `chord` runs.
    pub fn lookup(&self, chord: Chord) -> Option<&'static str> {
        self.table.get(&chord.on(self.platform)).map(|c| c.id)
    }

    /// The command a key press runs, if any. Releases, and repeats of a command that
    /// does not repeat, run nothing.
    pub fn translate(&self, event: &Event) -> Option<&'static str> {
        let Event::Key {
            key,
            pressed: true,
            repeat,
            modifiers,
            ..
        } = event
        else {
            return None;
        };
        let chord = Chord::from_egui(*key, *modifiers, self.platform);
        let c = self.table.get(&chord)?;
        (!repeat || c.repeats).then_some(c.id)
    }

    /// Takes this frame's key presses that run a command out of `ctx`'s input and
    /// returns the commands in order. Nothing is taken while a text field has the
    /// keyboard, so typing never runs a command.
    pub fn dispatch(&self, ctx: &egui::Context) -> Vec<&'static str> {
        if ctx.egui_wants_keyboard_input() {
            return Vec::new();
        }
        let mut out = Vec::new();
        ctx.input_mut(|input| {
            input.events.retain(|event| match self.translate(event) {
                Some(id) => {
                    out.push(id);
                    false
                }
                None => {
                    // A repeat of a bound chord whose command does not repeat is still
                    // the app's key, not a text or widget key.
                    !matches!(event, Event::Key { key, modifiers, .. }
                        if self.table.contains_key(&Chord::from_egui(*key, *modifiers, self.platform)))
                }
            });
        });
        out
    }
}

/// Whether `key` is only a modifier (it never makes a chord on its own).
pub fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}

#[cfg(test)]
mod tests {
    use egui::Modifiers;

    use super::super::presets::PresetId;
    use super::*;

    fn press(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    const MAC_CMD: Modifiers = Modifiers {
        alt: false,
        ctrl: false,
        shift: false,
        mac_cmd: true,
        command: true,
    };

    fn run(
        preset: PresetId,
        platform: Platform,
        key: Key,
        modifiers: Modifiers,
    ) -> Option<&'static str> {
        Dispatcher::new(&Keymap::new(preset), platform).translate(&press(key, modifiers))
    }

    #[test]
    fn glass_default_keys() {
        let mac = Platform::Mac;
        let none = Modifiers::NONE;
        assert_eq!(
            run(PresetId::Glass, mac, Key::Space, none),
            Some("playback.play_pause")
        );
        assert_eq!(run(PresetId::Glass, mac, Key::B, none), Some("tool.razor"));
        assert_eq!(
            run(PresetId::Glass, mac, Key::B, MAC_CMD),
            Some("edit.split")
        );
        assert_eq!(run(PresetId::Glass, mac, Key::I, none), Some("mark.in"));
        assert_eq!(
            run(PresetId::Glass, mac, Key::Backspace, Modifiers::SHIFT),
            Some("edit.ripple_delete")
        );
        assert_eq!(
            run(PresetId::Glass, mac, Key::K, MAC_CMD),
            Some("view.command_palette")
        );
        assert_eq!(run(PresetId::Glass, mac, Key::G, none), None);
    }

    #[test]
    fn premiere_pro_keys() {
        let mac = Platform::Mac;
        let none = Modifiers::NONE;
        assert_eq!(
            run(PresetId::Premiere, mac, Key::C, none),
            Some("tool.razor")
        );
        assert_eq!(
            run(PresetId::Premiere, mac, Key::B, none),
            Some("tool.ripple")
        );
        assert_eq!(
            run(PresetId::Premiere, mac, Key::K, MAC_CMD),
            Some("edit.split")
        );
        assert_eq!(
            run(PresetId::Premiere, mac, Key::S, none),
            Some("view.snapping")
        );
        assert_eq!(
            run(PresetId::Premiere, mac, Key::Backspace, Modifiers::ALT),
            Some("edit.ripple_delete")
        );
        assert_eq!(
            run(PresetId::Premiere, mac, Key::Backslash, none),
            Some("view.zoom_to_fit")
        );
        // The same preset on Windows: Ctrl stands in for ⌘.
        assert_eq!(
            run(
                PresetId::Premiere,
                Platform::Windows,
                Key::K,
                Modifiers::CTRL
            ),
            Some("edit.split")
        );
    }

    #[test]
    fn final_cut_pro_keys() {
        let mac = Platform::Mac;
        let none = Modifiers::NONE;
        assert_eq!(
            run(PresetId::FinalCut, mac, Key::A, none),
            Some("tool.select")
        );
        assert_eq!(
            run(PresetId::FinalCut, mac, Key::T, none),
            Some("tool.ripple")
        );
        assert_eq!(
            run(PresetId::FinalCut, mac, Key::N, none),
            Some("view.snapping")
        );
        assert_eq!(
            run(PresetId::FinalCut, mac, Key::E, none),
            Some("edit.append")
        );
        assert_eq!(
            run(PresetId::FinalCut, mac, Key::V, MAC_CMD | Modifiers::SHIFT),
            Some("edit.paste_attributes")
        );
        assert_eq!(
            run(PresetId::FinalCut, mac, Key::Quote, Modifiers::CTRL),
            Some("mark.next_marker")
        );
        // ⌘= arrives as ⌘+ when the layout makes = a shifted key's base; both zoom.
        assert_eq!(
            run(PresetId::FinalCut, mac, Key::Equals, MAC_CMD),
            Some("view.zoom_in")
        );
    }

    #[test]
    fn davinci_resolve_keys() {
        let mac = Platform::Mac;
        let none = Modifiers::NONE;
        assert_eq!(
            run(PresetId::Resolve, mac, Key::B, MAC_CMD),
            Some("edit.split")
        );
        assert_eq!(
            run(PresetId::Resolve, mac, Key::R, Modifiers::SHIFT),
            Some("edit.freeze_frame")
        );
        assert_eq!(
            run(PresetId::Resolve, mac, Key::F9, none),
            Some("edit.insert")
        );
        assert_eq!(
            run(PresetId::Resolve, mac, Key::Backspace, none),
            Some("edit.ripple_delete")
        );
        assert_eq!(
            run(PresetId::Resolve, mac, Key::Delete, none),
            Some("edit.delete")
        );
        assert_eq!(
            run(PresetId::Resolve, mac, Key::Slash, MAC_CMD),
            Some("playback.loop")
        );
    }

    #[test]
    fn shifted_punctuation_matches_its_base_key() {
        let glass = Dispatcher::new(&Keymap::default(), Platform::Mac);
        // ⇧⌘/ is help; a US layout reports it as ⌘? with Shift held.
        let question = press(Key::Questionmark, MAC_CMD | Modifiers::SHIFT);
        assert_eq!(glass.translate(&question), Some("view.help"));
    }

    #[test]
    fn a_clash_runs_the_first_command_and_overrides_win_over_the_preset() {
        let mut keymap = Keymap::default();
        keymap.set("view.snapping", vec![Chord::key(Key::S)]);
        let d = Dispatcher::new(&keymap, Platform::Mac);
        assert_eq!(d.lookup(Chord::key(Key::S)), Some("tool.rate_stretch"));
        assert_eq!(
            d.lookup(Chord::key(Key::S).shift()),
            None,
            "the old key is free"
        );
        keymap.take("view.snapping", Chord::key(Key::S), Platform::Mac);
        let d = Dispatcher::new(&keymap, Platform::Mac);
        assert_eq!(d.lookup(Chord::key(Key::S)), Some("view.snapping"));
    }

    #[test]
    fn repeats_run_only_repeating_commands_and_releases_run_nothing() {
        let d = Dispatcher::new(&Keymap::default(), Platform::Mac);
        let repeat = |key| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(
            d.translate(&repeat(Key::ArrowRight)),
            Some("playback.frame_forward")
        );
        assert_eq!(d.translate(&repeat(Key::B)), None);
        let release = Event::Key {
            key: Key::B,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(d.translate(&release), None);
    }

    #[test]
    fn dispatch_takes_matched_events_and_leaves_the_rest() {
        let ctx = egui::Context::default();
        let d = Dispatcher::new(&Keymap::default(), Platform::Mac);
        let input = egui::RawInput {
            events: vec![
                press(Key::J, Modifiers::NONE),
                press(Key::G, Modifiers::NONE),
                press(Key::L, Modifiers::NONE),
            ],
            ..Default::default()
        };
        let mut ran = Vec::new();
        let mut output = ctx.run_ui(input, |ui| {
            ran = d.dispatch(ui.ctx());
            let left = ui.input(|i| i.events.len());
            assert_eq!(left, 1, "only G is left for widgets");
        });
        output.textures_delta.clear();
        assert_eq!(
            ran,
            vec!["playback.shuttle_reverse", "playback.shuttle_forward"]
        );
    }
}
