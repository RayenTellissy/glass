//! The command registry: every editing command a key can run, with a stable id (what
//! keymaps and settings store), its group, and its names as i18n keys.
//!
//! Ids never change once released; a renamed command keeps its id. The list covers the
//! commands PLAN.md Phases 3, 6 and 7 name; later features add theirs here.

use crate::t;
use crate::theme::Role;

/// How the keyboard editor groups commands, in display order. Each group has a clip
/// colour on the keyboard map (`Sheet / Keyboard`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Tools,
    Playback,
    Marks,
    Editing,
    View,
}

impl Group {
    pub const ALL: [Group; 5] = [
        Group::Tools,
        Group::Playback,
        Group::Marks,
        Group::Editing,
        Group::View,
    ];

    pub fn label(self) -> String {
        match self {
            Group::Tools => t!("shortcuts.group.tools"),
            Group::Playback => t!("shortcuts.group.playback"),
            Group::Marks => t!("shortcuts.group.marks"),
            Group::Editing => t!("shortcuts.group.editing"),
            Group::View => t!("shortcuts.group.view"),
        }
    }

    /// The colour of the group's keys on the keyboard map.
    pub fn role(self) -> Role {
        match self {
            Group::Tools => Role::ClipVideo,
            Group::Playback => Role::ClipAudio,
            Group::Marks => Role::ClipTitle,
            Group::Editing => Role::ClipAdjustment,
            Group::View => Role::ClipSubtitle,
        }
    }
}

/// One command a key can run.
#[derive(Clone, Copy, Debug)]
pub struct Command {
    /// Stable id, `group.name` (`tool.razor`, `edit.ripple_delete`).
    pub id: &'static str,
    pub group: Group,
    /// Whether holding the key repeats the command (frame steps, nudges, zoom). Other
    /// commands run once per press.
    pub repeats: bool,
    label: fn() -> String,
    cap: fn() -> String,
}

impl Command {
    /// The command's full name (`Ripple delete`).
    pub fn label(&self) -> String {
        (self.label)()
    }

    /// The short name printed on its key in the keyboard map (`Ripple`).
    pub fn cap(&self) -> String {
        (self.cap)()
    }
}

macro_rules! commands {
    ($($id:literal, $group:ident, $repeats:literal, $label:expr, $cap:expr;)+) => {
        /// Every command, grouped and in display order.
        pub static COMMANDS: &[Command] = &[
            $(Command {
                id: $id,
                group: Group::$group,
                repeats: $repeats,
                label: || $label,
                cap: || $cap,
            },)+
        ];
    };
}

commands! {
    "tool.select", Tools, false, t!("shortcuts.cmd.tool.select"), t!("shortcuts.cap.tool.select");
    "tool.razor", Tools, false, t!("shortcuts.cmd.tool.razor"), t!("shortcuts.cap.tool.razor");
    "tool.ripple", Tools, false, t!("shortcuts.cmd.tool.ripple"), t!("shortcuts.cap.tool.ripple");
    "tool.roll", Tools, false, t!("shortcuts.cmd.tool.roll"), t!("shortcuts.cap.tool.roll");
    "tool.slip", Tools, false, t!("shortcuts.cmd.tool.slip"), t!("shortcuts.cap.tool.slip");
    "tool.slide", Tools, false, t!("shortcuts.cmd.tool.slide"), t!("shortcuts.cap.tool.slide");
    "tool.rate_stretch", Tools, false, t!("shortcuts.cmd.tool.rate_stretch"), t!("shortcuts.cap.tool.rate_stretch");
    "tool.hand", Tools, false, t!("shortcuts.cmd.tool.hand"), t!("shortcuts.cap.tool.hand");
    "tool.zoom", Tools, false, t!("shortcuts.cmd.tool.zoom"), t!("shortcuts.cap.tool.zoom");

    "playback.play_pause", Playback, false, t!("shortcuts.cmd.playback.play_pause"), t!("shortcuts.cap.playback.play_pause");
    "playback.shuttle_reverse", Playback, false, t!("shortcuts.cmd.playback.shuttle_reverse"), t!("shortcuts.cap.playback.shuttle_reverse");
    "playback.stop", Playback, false, t!("shortcuts.cmd.playback.stop"), t!("shortcuts.cap.playback.stop");
    "playback.shuttle_forward", Playback, false, t!("shortcuts.cmd.playback.shuttle_forward"), t!("shortcuts.cap.playback.shuttle_forward");
    "playback.frame_back", Playback, true, t!("shortcuts.cmd.playback.frame_back"), t!("shortcuts.cap.playback.frame_back");
    "playback.frame_forward", Playback, true, t!("shortcuts.cmd.playback.frame_forward"), t!("shortcuts.cap.playback.frame_forward");
    "playback.prev_edit", Playback, true, t!("shortcuts.cmd.playback.prev_edit"), t!("shortcuts.cap.playback.prev_edit");
    "playback.next_edit", Playback, true, t!("shortcuts.cmd.playback.next_edit"), t!("shortcuts.cap.playback.next_edit");
    "playback.go_to_start", Playback, false, t!("shortcuts.cmd.playback.go_to_start"), t!("shortcuts.cap.playback.go_to_start");
    "playback.go_to_end", Playback, false, t!("shortcuts.cmd.playback.go_to_end"), t!("shortcuts.cap.playback.go_to_end");
    "playback.prev_keyframe", Playback, true, t!("shortcuts.cmd.playback.prev_keyframe"), t!("shortcuts.cap.playback.prev_keyframe");
    "playback.next_keyframe", Playback, true, t!("shortcuts.cmd.playback.next_keyframe"), t!("shortcuts.cap.playback.next_keyframe");
    "playback.play_around", Playback, false, t!("shortcuts.cmd.playback.play_around"), t!("shortcuts.cap.playback.play_around");
    "playback.loop", Playback, false, t!("shortcuts.cmd.playback.loop"), t!("shortcuts.cap.playback.loop");

    "mark.in", Marks, false, t!("shortcuts.cmd.mark.in"), t!("shortcuts.cap.mark.in");
    "mark.out", Marks, false, t!("shortcuts.cmd.mark.out"), t!("shortcuts.cap.mark.out");
    "mark.clear_in_out", Marks, false, t!("shortcuts.cmd.mark.clear_in_out"), t!("shortcuts.cap.mark.clear_in_out");
    "mark.go_to_in", Marks, false, t!("shortcuts.cmd.mark.go_to_in"), t!("shortcuts.cap.mark.go_to_in");
    "mark.go_to_out", Marks, false, t!("shortcuts.cmd.mark.go_to_out"), t!("shortcuts.cap.mark.go_to_out");
    "mark.clip", Marks, false, t!("shortcuts.cmd.mark.clip"), t!("shortcuts.cap.mark.clip");
    "mark.add_marker", Marks, false, t!("shortcuts.cmd.mark.add_marker"), t!("shortcuts.cap.mark.add_marker");
    "mark.prev_marker", Marks, true, t!("shortcuts.cmd.mark.prev_marker"), t!("shortcuts.cap.mark.prev_marker");
    "mark.next_marker", Marks, true, t!("shortcuts.cmd.mark.next_marker"), t!("shortcuts.cap.mark.next_marker");

    "edit.undo", Editing, true, t!("shortcuts.cmd.edit.undo"), t!("shortcuts.cap.edit.undo");
    "edit.redo", Editing, true, t!("shortcuts.cmd.edit.redo"), t!("shortcuts.cap.edit.redo");
    "edit.cut", Editing, false, t!("shortcuts.cmd.edit.cut"), t!("shortcuts.cap.edit.cut");
    "edit.copy", Editing, false, t!("shortcuts.cmd.edit.copy"), t!("shortcuts.cap.edit.copy");
    "edit.paste", Editing, false, t!("shortcuts.cmd.edit.paste"), t!("shortcuts.cap.edit.paste");
    "edit.paste_attributes", Editing, false, t!("shortcuts.cmd.edit.paste_attributes"), t!("shortcuts.cap.edit.paste_attributes");
    "edit.delete", Editing, false, t!("shortcuts.cmd.edit.delete"), t!("shortcuts.cap.edit.delete");
    "edit.ripple_delete", Editing, false, t!("shortcuts.cmd.edit.ripple_delete"), t!("shortcuts.cap.edit.ripple_delete");
    "edit.split", Editing, false, t!("shortcuts.cmd.edit.split"), t!("shortcuts.cap.edit.split");
    "edit.split_all_tracks", Editing, false, t!("shortcuts.cmd.edit.split_all_tracks"), t!("shortcuts.cap.edit.split_all_tracks");
    "edit.trim_start", Editing, false, t!("shortcuts.cmd.edit.trim_start"), t!("shortcuts.cap.edit.trim_start");
    "edit.trim_end", Editing, false, t!("shortcuts.cmd.edit.trim_end"), t!("shortcuts.cap.edit.trim_end");
    "edit.nudge_left", Editing, true, t!("shortcuts.cmd.edit.nudge_left"), t!("shortcuts.cap.edit.nudge_left");
    "edit.nudge_right", Editing, true, t!("shortcuts.cmd.edit.nudge_right"), t!("shortcuts.cap.edit.nudge_right");
    "edit.freeze_frame", Editing, false, t!("shortcuts.cmd.edit.freeze_frame"), t!("shortcuts.cap.edit.freeze_frame");
    "edit.speed_duration", Editing, false, t!("shortcuts.cmd.edit.speed_duration"), t!("shortcuts.cap.edit.speed_duration");
    "edit.select_all", Editing, false, t!("shortcuts.cmd.edit.select_all"), t!("shortcuts.cap.edit.select_all");
    "edit.deselect_all", Editing, false, t!("shortcuts.cmd.edit.deselect_all"), t!("shortcuts.cap.edit.deselect_all");
    "edit.insert", Editing, false, t!("shortcuts.cmd.edit.insert"), t!("shortcuts.cap.edit.insert");
    "edit.overwrite", Editing, false, t!("shortcuts.cmd.edit.overwrite"), t!("shortcuts.cap.edit.overwrite");
    "edit.append", Editing, false, t!("shortcuts.cmd.edit.append"), t!("shortcuts.cap.edit.append");
    "edit.link", Editing, false, t!("shortcuts.cmd.edit.link"), t!("shortcuts.cap.edit.link");
    "edit.group", Editing, false, t!("shortcuts.cmd.edit.group"), t!("shortcuts.cap.edit.group");
    "edit.enable", Editing, false, t!("shortcuts.cmd.edit.enable"), t!("shortcuts.cap.edit.enable");
    "edit.default_transition", Editing, false, t!("shortcuts.cmd.edit.default_transition"), t!("shortcuts.cap.edit.default_transition");
    "edit.save", Editing, false, t!("shortcuts.cmd.edit.save"), t!("shortcuts.cap.edit.save");
    "edit.import", Editing, false, t!("shortcuts.cmd.edit.import"), t!("shortcuts.cap.edit.import");

    "view.zoom_in", View, true, t!("shortcuts.cmd.view.zoom_in"), t!("shortcuts.cap.view.zoom_in");
    "view.zoom_out", View, true, t!("shortcuts.cmd.view.zoom_out"), t!("shortcuts.cap.view.zoom_out");
    "view.zoom_to_fit", View, false, t!("shortcuts.cmd.view.zoom_to_fit"), t!("shortcuts.cap.view.zoom_to_fit");
    "view.snapping", View, false, t!("shortcuts.cmd.view.snapping"), t!("shortcuts.cap.view.snapping");
    "view.compare", View, false, t!("shortcuts.cmd.view.compare"), t!("shortcuts.cap.view.compare");
    "view.full_screen", View, false, t!("shortcuts.cmd.view.full_screen"), t!("shortcuts.cap.view.full_screen");
    "view.command_palette", View, false, t!("shortcuts.cmd.view.command_palette"), t!("shortcuts.cap.view.command_palette");
    "view.keyboard_shortcuts", View, false, t!("shortcuts.cmd.view.keyboard_shortcuts"), t!("shortcuts.cap.view.keyboard_shortcuts");
    "view.help", View, false, t!("shortcuts.cmd.view.help"), t!("shortcuts.cap.view.help");
}

/// The command with `id`, if it exists in this version.
pub fn command(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// Where `id` sits in [`COMMANDS`] (lower first), for stable ordering.
pub fn index(id: &str) -> Option<usize> {
    COMMANDS.iter().position(|c| c.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_named_after_their_group() {
        let mut seen = std::collections::BTreeSet::new();
        for c in COMMANDS {
            assert!(seen.insert(c.id), "duplicate command id {}", c.id);
            let prefix = match c.group {
                Group::Tools => "tool.",
                Group::Playback => "playback.",
                Group::Marks => "mark.",
                Group::Editing => "edit.",
                Group::View => "view.",
            };
            assert!(c.id.starts_with(prefix), "{} is not in {:?}", c.id, c.group);
        }
    }

    #[test]
    fn commands_are_listed_group_by_group() {
        let groups: Vec<Group> = COMMANDS.iter().map(|c| c.group).collect();
        let mut sorted = groups.clone();
        sorted.sort();
        assert_eq!(groups, sorted);
    }

    #[test]
    fn every_command_has_both_names() {
        for c in COMMANDS {
            assert!(!c.label().contains('⟦'), "{} has no label string", c.id);
            assert!(!c.cap().contains('⟦'), "{} has no keycap string", c.id);
        }
    }
}
