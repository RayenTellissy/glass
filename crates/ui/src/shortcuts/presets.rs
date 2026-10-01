//! The four keyboard presets as data: Glass's own (from `Sheet / Keyboard`) and the
//! default keys of Premiere Pro, Final Cut Pro and DaVinci Resolve, from each editor's
//! published default shortcut list (macOS keys; `Cmd` is Ctrl on Windows and Linux).
//!
//! A command an editor has no default key for is left out of that preset and starts
//! unbound. Final Cut Pro and DaVinci Resolve have one trim tool for ripple and roll
//! (which it does depends on where the cut is grabbed), so both commands get its key.

use egui::Key;
use serde::{Deserialize, Serialize};

use super::keymap::Chord;
use crate::t;

/// A preset's stored name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetId {
    Glass,
    Premiere,
    FinalCut,
    Resolve,
}

impl PresetId {
    pub const ALL: [PresetId; 4] = [
        PresetId::Glass,
        PresetId::Premiere,
        PresetId::FinalCut,
        PresetId::Resolve,
    ];

    pub fn label(self) -> String {
        match self {
            PresetId::Glass => t!("shortcuts.preset.glass"),
            PresetId::Premiere => t!("shortcuts.preset.premiere"),
            PresetId::FinalCut => t!("shortcuts.preset.final_cut"),
            PresetId::Resolve => t!("shortcuts.preset.resolve"),
        }
    }

    pub fn bindings(self) -> &'static [(&'static str, &'static [Chord])] {
        match self {
            PresetId::Glass => GLASS,
            PresetId::Premiere => PREMIERE,
            PresetId::FinalCut => FINAL_CUT,
            PresetId::Resolve => RESOLVE,
        }
    }
}

/// The chords `preset` gives `id` (none when it leaves the command unbound).
pub fn chords(preset: PresetId, id: &str) -> &'static [Chord] {
    preset
        .bindings()
        .iter()
        .find(|(c, _)| *c == id)
        .map_or(&[], |(_, chords)| chords)
}

const fn k(key: Key) -> Chord {
    Chord::key(key)
}

/// `Sheet / Keyboard`: single keys for tools, marks and playback, ⌘ for whole-edit
/// commands, ⇧ for the variant of a nearby key.
const GLASS: &[(&str, &[Chord])] = &[
    ("tool.select", &[k(Key::V)]),
    ("tool.razor", &[k(Key::B)]),
    ("tool.ripple", &[k(Key::R)]),
    ("tool.roll", &[k(Key::N)]),
    ("tool.slip", &[k(Key::Y)]),
    ("tool.slide", &[k(Key::U)]),
    ("tool.rate_stretch", &[k(Key::S)]),
    ("tool.hand", &[k(Key::H)]),
    ("tool.zoom", &[k(Key::Z)]),
    ("playback.play_pause", &[k(Key::Space)]),
    ("playback.shuttle_reverse", &[k(Key::J)]),
    ("playback.stop", &[k(Key::K)]),
    ("playback.shuttle_forward", &[k(Key::L)]),
    ("playback.frame_back", &[k(Key::ArrowLeft)]),
    ("playback.frame_forward", &[k(Key::ArrowRight)]),
    ("playback.prev_edit", &[k(Key::ArrowUp)]),
    ("playback.next_edit", &[k(Key::ArrowDown)]),
    ("playback.go_to_start", &[k(Key::Home)]),
    ("playback.go_to_end", &[k(Key::End)]),
    ("playback.prev_keyframe", &[k(Key::OpenBracket)]),
    ("playback.next_keyframe", &[k(Key::CloseBracket)]),
    ("playback.play_around", &[k(Key::Slash)]),
    ("playback.loop", &[k(Key::L).cmd()]),
    ("mark.in", &[k(Key::I)]),
    ("mark.out", &[k(Key::O)]),
    ("mark.clear_in_out", &[k(Key::X).alt()]),
    ("mark.go_to_in", &[k(Key::I).shift()]),
    ("mark.go_to_out", &[k(Key::O).shift()]),
    ("mark.clip", &[k(Key::X).shift()]),
    ("mark.add_marker", &[k(Key::M)]),
    ("mark.prev_marker", &[k(Key::Semicolon)]),
    ("mark.next_marker", &[k(Key::Quote)]),
    ("edit.undo", &[k(Key::Z).cmd()]),
    ("edit.redo", &[k(Key::Z).shift().cmd()]),
    ("edit.cut", &[k(Key::X).cmd()]),
    ("edit.copy", &[k(Key::C).cmd()]),
    ("edit.paste", &[k(Key::V).cmd()]),
    ("edit.paste_attributes", &[k(Key::V).alt().cmd()]),
    ("edit.delete", &[k(Key::Backspace)]),
    ("edit.ripple_delete", &[k(Key::Backspace).shift()]),
    ("edit.split", &[k(Key::B).cmd()]),
    ("edit.split_all_tracks", &[k(Key::B).shift().cmd()]),
    ("edit.trim_start", &[k(Key::Q)]),
    ("edit.trim_end", &[k(Key::W)]),
    ("edit.nudge_left", &[k(Key::Comma)]),
    ("edit.nudge_right", &[k(Key::Period)]),
    ("edit.freeze_frame", &[k(Key::F).alt()]),
    ("edit.speed_duration", &[k(Key::R).cmd()]),
    ("edit.select_all", &[k(Key::A).cmd()]),
    ("edit.deselect_all", &[k(Key::A).shift().cmd()]),
    ("edit.insert", &[k(Key::F9)]),
    ("edit.overwrite", &[k(Key::F10)]),
    ("edit.append", &[k(Key::F12)]),
    ("edit.link", &[k(Key::L).alt().cmd()]),
    ("edit.group", &[k(Key::G).cmd()]),
    ("edit.enable", &[k(Key::E).shift()]),
    ("edit.default_transition", &[k(Key::T).cmd()]),
    ("edit.save", &[k(Key::S).cmd()]),
    ("edit.import", &[k(Key::I).cmd()]),
    ("view.zoom_in", &[k(Key::Equals)]),
    ("view.zoom_out", &[k(Key::Minus)]),
    ("view.zoom_to_fit", &[k(Key::Z).shift()]),
    ("view.snapping", &[k(Key::S).shift()]),
    ("view.compare", &[k(Key::Backslash)]),
    ("view.full_screen", &[k(Key::F).shift().cmd()]),
    ("view.command_palette", &[k(Key::K).cmd()]),
    ("view.keyboard_shortcuts", &[k(Key::K).alt().cmd()]),
    ("view.help", &[k(Key::Slash).shift().cmd()]),
];

/// Adobe Premiere Pro, default keyboard shortcuts (macOS).
const PREMIERE: &[(&str, &[Chord])] = &[
    ("tool.select", &[k(Key::V)]),
    ("tool.razor", &[k(Key::C)]),
    ("tool.ripple", &[k(Key::B)]),
    ("tool.roll", &[k(Key::N)]),
    ("tool.slip", &[k(Key::Y)]),
    ("tool.slide", &[k(Key::U)]),
    ("tool.rate_stretch", &[k(Key::R)]),
    ("tool.hand", &[k(Key::H)]),
    ("tool.zoom", &[k(Key::Z)]),
    ("playback.play_pause", &[k(Key::Space)]),
    ("playback.shuttle_reverse", &[k(Key::J)]),
    ("playback.stop", &[k(Key::K)]),
    ("playback.shuttle_forward", &[k(Key::L)]),
    ("playback.frame_back", &[k(Key::ArrowLeft)]),
    ("playback.frame_forward", &[k(Key::ArrowRight)]),
    ("playback.prev_edit", &[k(Key::ArrowUp)]),
    ("playback.next_edit", &[k(Key::ArrowDown)]),
    ("playback.go_to_start", &[k(Key::Home)]),
    ("playback.go_to_end", &[k(Key::End)]),
    ("playback.play_around", &[k(Key::K).shift()]),
    ("mark.in", &[k(Key::I)]),
    ("mark.out", &[k(Key::O)]),
    ("mark.clear_in_out", &[k(Key::X).alt()]),
    ("mark.go_to_in", &[k(Key::I).shift()]),
    ("mark.go_to_out", &[k(Key::O).shift()]),
    ("mark.clip", &[k(Key::X)]),
    ("mark.add_marker", &[k(Key::M)]),
    ("mark.prev_marker", &[k(Key::M).shift().cmd()]),
    ("mark.next_marker", &[k(Key::M).shift()]),
    ("edit.undo", &[k(Key::Z).cmd()]),
    ("edit.redo", &[k(Key::Z).shift().cmd()]),
    ("edit.cut", &[k(Key::X).cmd()]),
    ("edit.copy", &[k(Key::C).cmd()]),
    ("edit.paste", &[k(Key::V).cmd()]),
    ("edit.paste_attributes", &[k(Key::V).alt().cmd()]),
    ("edit.delete", &[k(Key::Backspace)]),
    ("edit.ripple_delete", &[k(Key::Backspace).alt()]),
    ("edit.split", &[k(Key::K).cmd()]),
    ("edit.split_all_tracks", &[k(Key::K).shift().cmd()]),
    ("edit.trim_start", &[k(Key::Q)]),
    ("edit.trim_end", &[k(Key::W)]),
    ("edit.nudge_left", &[k(Key::ArrowLeft).alt()]),
    ("edit.nudge_right", &[k(Key::ArrowRight).alt()]),
    ("edit.speed_duration", &[k(Key::R).cmd()]),
    ("edit.select_all", &[k(Key::A).cmd()]),
    ("edit.deselect_all", &[k(Key::A).shift().cmd()]),
    ("edit.insert", &[k(Key::Comma)]),
    ("edit.overwrite", &[k(Key::Period)]),
    ("edit.link", &[k(Key::L).cmd()]),
    ("edit.group", &[k(Key::G).cmd()]),
    ("edit.enable", &[k(Key::E).shift().cmd()]),
    ("edit.default_transition", &[k(Key::D).cmd()]),
    ("edit.save", &[k(Key::S).cmd()]),
    ("edit.import", &[k(Key::I).cmd()]),
    ("view.zoom_in", &[k(Key::Equals)]),
    ("view.zoom_out", &[k(Key::Minus)]),
    ("view.zoom_to_fit", &[k(Key::Backslash)]),
    ("view.snapping", &[k(Key::S)]),
    ("view.keyboard_shortcuts", &[k(Key::K).alt().cmd()]),
    ("view.help", &[k(Key::F1)]),
];

/// Apple Final Cut Pro, default command set.
const FINAL_CUT: &[(&str, &[Chord])] = &[
    ("tool.select", &[k(Key::A)]),
    ("tool.razor", &[k(Key::B)]),
    ("tool.ripple", &[k(Key::T)]),
    ("tool.roll", &[k(Key::T)]),
    ("tool.hand", &[k(Key::H)]),
    ("tool.zoom", &[k(Key::Z)]),
    ("playback.play_pause", &[k(Key::Space)]),
    ("playback.shuttle_reverse", &[k(Key::J)]),
    ("playback.stop", &[k(Key::K)]),
    ("playback.shuttle_forward", &[k(Key::L)]),
    ("playback.frame_back", &[k(Key::ArrowLeft)]),
    ("playback.frame_forward", &[k(Key::ArrowRight)]),
    ("playback.prev_edit", &[k(Key::ArrowUp)]),
    ("playback.next_edit", &[k(Key::ArrowDown)]),
    ("playback.go_to_start", &[k(Key::Home)]),
    ("playback.go_to_end", &[k(Key::End)]),
    ("playback.prev_keyframe", &[k(Key::Semicolon).alt()]),
    ("playback.next_keyframe", &[k(Key::Quote).alt()]),
    ("playback.play_around", &[k(Key::Slash).shift()]),
    ("playback.loop", &[k(Key::L).cmd()]),
    ("mark.in", &[k(Key::I)]),
    ("mark.out", &[k(Key::O)]),
    ("mark.clear_in_out", &[k(Key::X).alt()]),
    ("mark.go_to_in", &[k(Key::I).shift()]),
    ("mark.go_to_out", &[k(Key::O).shift()]),
    ("mark.clip", &[k(Key::X)]),
    ("mark.add_marker", &[k(Key::M)]),
    ("mark.prev_marker", &[k(Key::Semicolon).ctrl()]),
    ("mark.next_marker", &[k(Key::Quote).ctrl()]),
    ("edit.undo", &[k(Key::Z).cmd()]),
    ("edit.redo", &[k(Key::Z).shift().cmd()]),
    ("edit.cut", &[k(Key::X).cmd()]),
    ("edit.copy", &[k(Key::C).cmd()]),
    ("edit.paste", &[k(Key::V).cmd()]),
    ("edit.paste_attributes", &[k(Key::V).shift().cmd()]),
    ("edit.delete", &[k(Key::Backspace).shift()]),
    ("edit.ripple_delete", &[k(Key::Backspace)]),
    ("edit.split", &[k(Key::B).cmd()]),
    ("edit.split_all_tracks", &[k(Key::B).shift().cmd()]),
    ("edit.trim_start", &[k(Key::OpenBracket).alt()]),
    ("edit.trim_end", &[k(Key::CloseBracket).alt()]),
    ("edit.nudge_left", &[k(Key::Comma)]),
    ("edit.nudge_right", &[k(Key::Period)]),
    ("edit.freeze_frame", &[k(Key::F).alt()]),
    ("edit.speed_duration", &[k(Key::R).cmd()]),
    ("edit.select_all", &[k(Key::A).cmd()]),
    ("edit.deselect_all", &[k(Key::A).shift().cmd()]),
    ("edit.insert", &[k(Key::W)]),
    ("edit.overwrite", &[k(Key::D)]),
    ("edit.append", &[k(Key::E)]),
    ("edit.group", &[k(Key::G).alt()]),
    ("edit.enable", &[k(Key::V)]),
    ("edit.default_transition", &[k(Key::T).cmd()]),
    ("edit.import", &[k(Key::I).cmd()]),
    ("view.zoom_in", &[k(Key::Equals).cmd()]),
    ("view.zoom_out", &[k(Key::Minus).cmd()]),
    ("view.zoom_to_fit", &[k(Key::Z).shift()]),
    ("view.snapping", &[k(Key::N)]),
    ("view.full_screen", &[k(Key::F).shift().cmd()]),
    ("view.keyboard_shortcuts", &[k(Key::K).alt().cmd()]),
];

/// Blackmagic DaVinci Resolve, default keyboard (macOS).
const RESOLVE: &[(&str, &[Chord])] = &[
    ("tool.select", &[k(Key::A)]),
    ("tool.razor", &[k(Key::B)]),
    ("tool.ripple", &[k(Key::T)]),
    ("tool.roll", &[k(Key::T)]),
    ("playback.play_pause", &[k(Key::Space)]),
    ("playback.shuttle_reverse", &[k(Key::J)]),
    ("playback.stop", &[k(Key::K)]),
    ("playback.shuttle_forward", &[k(Key::L)]),
    ("playback.frame_back", &[k(Key::ArrowLeft)]),
    ("playback.frame_forward", &[k(Key::ArrowRight)]),
    ("playback.prev_edit", &[k(Key::ArrowUp)]),
    ("playback.next_edit", &[k(Key::ArrowDown)]),
    ("playback.go_to_start", &[k(Key::Home)]),
    ("playback.go_to_end", &[k(Key::End)]),
    ("playback.prev_keyframe", &[k(Key::OpenBracket)]),
    ("playback.next_keyframe", &[k(Key::CloseBracket)]),
    ("playback.play_around", &[k(Key::Slash)]),
    ("playback.loop", &[k(Key::Slash).cmd()]),
    ("mark.in", &[k(Key::I)]),
    ("mark.out", &[k(Key::O)]),
    ("mark.clear_in_out", &[k(Key::X).alt()]),
    ("mark.go_to_in", &[k(Key::I).shift()]),
    ("mark.go_to_out", &[k(Key::O).shift()]),
    ("mark.clip", &[k(Key::X)]),
    ("mark.add_marker", &[k(Key::M)]),
    ("mark.prev_marker", &[k(Key::ArrowUp).shift()]),
    ("mark.next_marker", &[k(Key::ArrowDown).shift()]),
    ("edit.undo", &[k(Key::Z).cmd()]),
    ("edit.redo", &[k(Key::Z).shift().cmd()]),
    ("edit.cut", &[k(Key::X).cmd()]),
    ("edit.copy", &[k(Key::C).cmd()]),
    ("edit.paste", &[k(Key::V).cmd()]),
    ("edit.paste_attributes", &[k(Key::V).alt()]),
    ("edit.delete", &[k(Key::Delete)]),
    ("edit.ripple_delete", &[k(Key::Backspace)]),
    ("edit.split", &[k(Key::B).cmd()]),
    ("edit.trim_start", &[k(Key::OpenBracket).shift()]),
    ("edit.trim_end", &[k(Key::CloseBracket).shift()]),
    ("edit.nudge_left", &[k(Key::Comma)]),
    ("edit.nudge_right", &[k(Key::Period)]),
    ("edit.freeze_frame", &[k(Key::R).shift()]),
    ("edit.speed_duration", &[k(Key::R)]),
    ("edit.select_all", &[k(Key::A).cmd()]),
    ("edit.deselect_all", &[k(Key::A).shift().cmd()]),
    ("edit.insert", &[k(Key::F9)]),
    ("edit.overwrite", &[k(Key::F10)]),
    ("edit.append", &[k(Key::F12).shift()]),
    ("edit.link", &[k(Key::L).alt().cmd()]),
    ("edit.enable", &[k(Key::D)]),
    ("edit.default_transition", &[k(Key::T).cmd()]),
    ("edit.save", &[k(Key::S).cmd()]),
    ("edit.import", &[k(Key::I).cmd()]),
    ("view.zoom_in", &[k(Key::Equals).cmd()]),
    ("view.zoom_out", &[k(Key::Minus).cmd()]),
    ("view.zoom_to_fit", &[k(Key::Z).shift()]),
    ("view.snapping", &[k(Key::N)]),
    ("view.full_screen", &[k(Key::F).cmd()]),
    ("view.keyboard_shortcuts", &[k(Key::K).alt().cmd()]),
];

#[cfg(test)]
mod tests {
    use super::super::commands::{COMMANDS, command};
    use super::super::keymap::{Keymap, Platform, is_reserved};
    use super::*;

    #[test]
    fn presets_name_only_known_commands_once() {
        for preset in PresetId::ALL {
            let mut seen = std::collections::BTreeSet::new();
            for (id, chords) in preset.bindings() {
                assert!(command(id).is_some(), "{preset:?} binds unknown {id}");
                assert!(seen.insert(*id), "{preset:?} binds {id} twice");
                assert!(!chords.is_empty(), "{preset:?} lists {id} without a key");
            }
        }
    }

    #[test]
    fn glass_binds_every_command() {
        for c in COMMANDS {
            assert!(
                !chords(PresetId::Glass, c.id).is_empty(),
                "{} unbound",
                c.id
            );
        }
    }

    #[test]
    fn presets_have_no_clashes_or_reserved_keys() {
        for preset in PresetId::ALL {
            for platform in [Platform::Mac, Platform::Windows, Platform::Linux] {
                let keymap = Keymap::new(preset);
                assert_eq!(
                    keymap.conflicts(platform),
                    vec![],
                    "{preset:?} on {platform:?}"
                );
                for (id, chords) in preset.bindings() {
                    for chord in *chords {
                        assert!(
                            !is_reserved(*chord, platform),
                            "{preset:?} gives {id} the reserved {chord} on {platform:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn only_the_shared_trim_tool_key_runs_two_commands() {
        for preset in PresetId::ALL {
            let mut by_chord = std::collections::BTreeMap::<Chord, Vec<&str>>::new();
            for (id, chords) in preset.bindings() {
                for chord in *chords {
                    by_chord
                        .entry(chord.on(Platform::Mac))
                        .or_default()
                        .push(id);
                }
            }
            let shared: Vec<Vec<&str>> =
                by_chord.into_values().filter(|ids| ids.len() > 1).collect();
            let expected: Vec<Vec<&str>> = match preset {
                PresetId::FinalCut | PresetId::Resolve => vec![vec!["tool.ripple", "tool.roll"]],
                _ => vec![],
            };
            assert_eq!(shared, expected, "{preset:?}");
        }
    }
}
