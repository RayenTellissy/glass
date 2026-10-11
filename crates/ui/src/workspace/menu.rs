//! The window's menu bar: File (open, save), Edit (undo, redo), Window (workspace
//! presets, saved workspaces, panels) and Help (keyboard shortcuts, About). It draws a
//! [`MenuView`] and returns the [`MenuCommand`] chosen; the app carries it out.

use egui::{Button, Ui};

use super::{Active, PanelKind, Preset, Workspaces};
use crate::t;

/// What a menu item asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuCommand {
    Open,
    Save,
    SaveAs,
    Undo,
    Redo,
    Preset(Preset),
    /// Show the saved workspace of this name.
    Workspace(String),
    /// Save the current layout under this name.
    SaveWorkspace(String),
    DeleteWorkspace(String),
    ResetWorkspace,
    /// Close the panel when open, else restore it.
    TogglePanel(PanelKind),
    KeyboardShortcuts,
    About,
}

/// What the menus show, filled by the app each frame.
#[derive(Clone, Debug, Default)]
pub struct MenuView {
    /// `Undo Trim`, or `None` when there is nothing to undo.
    pub undo: Option<String>,
    pub redo: Option<String>,
    pub can_save: bool,
    /// Shortcut text for a command id, from the keymap (`⌘Z`).
    pub shortcuts: Vec<(&'static str, String)>,
    /// The name typed under Window › Save workspace as, kept between frames.
    pub workspace_name: String,
}

impl MenuView {
    fn shortcut(&self, id: &str) -> Option<&str> {
        self.shortcuts
            .iter()
            .find(|(c, _)| *c == id)
            .map(|(_, s)| s.as_str())
    }
}

/// `Undo Trim`: the Edit menu's undo item for the step `label` names.
pub fn undo_text(label: project::EditLabel) -> String {
    t!(
        "edit.undo_named",
        action = assets::i18n::text(label.i18n_key())
    )
}

/// `Redo Trim`.
pub fn redo_text(label: project::EditLabel) -> String {
    t!(
        "edit.redo_named",
        action = assets::i18n::text(label.i18n_key())
    )
}

fn item(ui: &mut Ui, label: String, shortcut: Option<&str>, enabled: bool) -> bool {
    let mut button = Button::new(label);
    if let Some(s) = shortcut {
        button = button.shortcut_text(s);
    }
    let clicked = ui.add_enabled(enabled, button).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn check(ui: &mut Ui, label: String, on: bool) -> bool {
    let clicked = ui.add(Button::new(label).selected(on)).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// Draws the menu bar into `ui` (a top strip) and returns the command chosen.
pub fn menu_bar(ui: &mut Ui, view: &mut MenuView, workspaces: &Workspaces) -> Option<MenuCommand> {
    let mut out = None;
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button(t!("menu.file"), |ui| {
            if item(ui, t!("menu.file.open"), None, true) {
                out = Some(MenuCommand::Open);
            }
            if item(
                ui,
                t!("menu.file.save"),
                view.shortcut("edit.save"),
                view.can_save,
            ) {
                out = Some(MenuCommand::Save);
            }
            if item(ui, t!("menu.file.save_as"), None, view.can_save) {
                out = Some(MenuCommand::SaveAs);
            }
        });
        ui.menu_button(t!("menu.edit"), |ui| {
            let undo = view
                .undo
                .clone()
                .unwrap_or_else(|| t!("shortcuts.cmd.edit.undo"));
            if item(ui, undo, view.shortcut("edit.undo"), view.undo.is_some()) {
                out = Some(MenuCommand::Undo);
            }
            let redo = view
                .redo
                .clone()
                .unwrap_or_else(|| t!("shortcuts.cmd.edit.redo"));
            if item(ui, redo, view.shortcut("edit.redo"), view.redo.is_some()) {
                out = Some(MenuCommand::Redo);
            }
        });
        ui.menu_button(t!("menu.window"), |ui| {
            for preset in Preset::ALL {
                if check(
                    ui,
                    preset.label(),
                    workspaces.active == Active::Preset(preset),
                ) {
                    out = Some(MenuCommand::Preset(preset));
                }
            }
            if !workspaces.saved.is_empty() {
                ui.separator();
                for name in workspaces.saved.keys() {
                    let on = workspaces.active == Active::Saved(name.clone());
                    if check(ui, name.clone(), on) {
                        out = Some(MenuCommand::Workspace(name.clone()));
                    }
                }
            }
            ui.separator();
            ui.menu_button(t!("menu.window.save_workspace"), |ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut view.workspace_name)
                        .hint_text(t!("menu.window.workspace_name")),
                );
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let name = view.workspace_name.trim().to_owned();
                if (ui
                    .add_enabled(!name.is_empty(), Button::new(t!("menu.window.save")))
                    .clicked()
                    || enter)
                    && !name.is_empty()
                {
                    out = Some(MenuCommand::SaveWorkspace(name));
                    view.workspace_name.clear();
                    ui.close();
                }
            });
            if let Active::Saved(name) = &workspaces.active
                && item(
                    ui,
                    t!("menu.window.delete_workspace", name = name),
                    None,
                    true,
                )
            {
                out = Some(MenuCommand::DeleteWorkspace(name.clone()));
            }
            if item(ui, t!("menu.window.reset_workspace"), None, true) {
                out = Some(MenuCommand::ResetWorkspace);
            }
            ui.separator();
            for panel in PanelKind::ALL {
                if check(ui, panel.title(), workspaces.current.is_open(panel)) {
                    out = Some(MenuCommand::TogglePanel(panel));
                }
            }
        });
        ui.menu_button(t!("menu.help"), |ui| {
            if item(
                ui,
                t!("shortcuts.cmd.view.keyboard_shortcuts"),
                view.shortcut("view.keyboard_shortcuts"),
                true,
            ) {
                out = Some(MenuCommand::KeyboardShortcuts);
            }
            if item(ui, t!("menu.help.about"), None, true) {
                out = Some(MenuCommand::About);
            }
        });
    });
    out
}
