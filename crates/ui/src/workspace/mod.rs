//! The dockable workspace (Decision 79): the editor's panels as tabs in a tree of splits
//! that the user can move, tab together, split, close and restore, saved as named
//! workspaces, with presets for Edit, Colour, Audio and Effects.
//!
//! A [`Workspace`] is an `egui_dock` tree of [`PanelKind`]s; [`dock::show`] draws it with
//! theme roles and hands each visible panel's rectangle to a [`dock::PanelHost`], which
//! the app implements by calling the panel functions in [`crate::panels`]. Panels that
//! do not exist yet (scopes, colour wheels, the mixer, the effects browser) draw
//! [`crate::panels::placeholder`] so the Colour, Audio and Effects presets already have
//! their shape. [`Workspaces`] holds the current layout and the saved ones and reads and
//! writes them as one JSON file; where that file lives is the app's to decide
//! (`workspaces.json` in the config folder).

pub mod dock;
pub mod menu;
pub mod panels;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use egui_dock::{DockState, NodeIndex};
use serde::{Deserialize, Serialize};

use crate::t;

/// Every panel the workspace can hold. The serialised names are stored in saved layouts
/// and never change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelKind {
    MediaPool,
    Preview,
    Timeline,
    Inspector,
    Metadata,
    /// Not built yet: drawn as a placeholder.
    Scopes,
    /// Not built yet: drawn as a placeholder.
    ColourWheels,
    /// Not built yet: drawn as a placeholder.
    Mixer,
    /// Not built yet: drawn as a placeholder.
    EffectsBrowser,
}

impl PanelKind {
    pub const ALL: [PanelKind; 9] = [
        PanelKind::MediaPool,
        PanelKind::Preview,
        PanelKind::Timeline,
        PanelKind::Inspector,
        PanelKind::Metadata,
        PanelKind::Scopes,
        PanelKind::ColourWheels,
        PanelKind::Mixer,
        PanelKind::EffectsBrowser,
    ];

    /// The tab title and the panel's name in the Window menu.
    pub fn title(self) -> String {
        match self {
            PanelKind::MediaPool => t!("workspace.panel.media_pool"),
            PanelKind::Preview => t!("workspace.panel.preview"),
            PanelKind::Timeline => t!("workspace.panel.timeline"),
            PanelKind::Inspector => t!("workspace.panel.inspector"),
            PanelKind::Metadata => t!("workspace.panel.metadata"),
            PanelKind::Scopes => t!("workspace.panel.scopes"),
            PanelKind::ColourWheels => t!("workspace.panel.colour_wheels"),
            PanelKind::Mixer => t!("workspace.panel.mixer"),
            PanelKind::EffectsBrowser => t!("workspace.panel.effects"),
        }
    }

    /// Whether the panel is built yet, or a placeholder.
    pub fn is_placeholder(self) -> bool {
        matches!(
            self,
            PanelKind::Scopes
                | PanelKind::ColourWheels
                | PanelKind::Mixer
                | PanelKind::EffectsBrowser
        )
    }
}

/// The built-in layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Edit,
    Colour,
    Audio,
    Effects,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::Edit, Preset::Colour, Preset::Audio, Preset::Effects];

    pub fn label(self) -> String {
        match self {
            Preset::Edit => t!("workspace.preset.edit"),
            Preset::Colour => t!("workspace.preset.colour"),
            Preset::Audio => t!("workspace.preset.audio"),
            Preset::Effects => t!("workspace.preset.effects"),
        }
    }
}

/// One layout: which panels are open, how they are split and tabbed, and the split
/// fractions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Workspace {
    pub dock: DockState<PanelKind>,
}

impl PartialEq for Workspace {
    /// Two layouts are equal when they hold the same panels in the same tree and tabs;
    /// rectangles the last frame measured do not count.
    fn eq(&self, other: &Self) -> bool {
        self.shape() == other.shape()
    }
}

impl Workspace {
    /// The layout of a preset.
    ///
    /// - Edit: media pool over metadata on the left, preview in the middle, inspector on
    ///   the right, the timeline along the bottom.
    /// - Colour: preview beside the scopes, colour wheels tabbed with the timeline below.
    /// - Audio: media pool and preview over the timeline, the mixer right of the
    ///   timeline.
    /// - Effects: effects browser, preview and inspector over the timeline.
    pub fn preset(preset: Preset) -> Self {
        use PanelKind as P;
        let mut dock = DockState::new(vec![P::Preview]);
        let tree = dock.main_surface_mut();
        let root = NodeIndex::root();
        match preset {
            Preset::Edit => {
                let [top, _] = tree.split_below(root, 0.58, vec![P::Timeline]);
                let [middle, _] = tree.split_left(top, 0.26, vec![P::MediaPool]);
                // `split_left` keeps the old node on the right, so `middle` is the preview.
                tree.split_right(middle, 0.68, vec![P::Inspector]);
                if let Some((node, _)) = tree.find_tab(&P::MediaPool) {
                    tree.split_below(node, 0.62, vec![P::Metadata]);
                }
            }
            Preset::Colour => {
                let [top, _] = tree.split_below(root, 0.6, vec![P::ColourWheels, P::Timeline]);
                tree.split_right(top, 0.62, vec![P::Scopes]);
            }
            Preset::Audio => {
                let [top, bottom] = tree.split_below(root, 0.42, vec![P::Timeline]);
                tree.split_left(top, 0.4, vec![P::MediaPool, P::Metadata]);
                tree.split_right(bottom, 0.7, vec![P::Mixer]);
            }
            Preset::Effects => {
                let [top, _] = tree.split_below(root, 0.58, vec![P::Timeline]);
                let [middle, _] = tree.split_left(top, 0.26, vec![P::EffectsBrowser]);
                tree.split_right(middle, 0.68, vec![P::Inspector]);
            }
        }
        Self { dock }
    }

    /// The open panels, in tree order.
    pub fn panels(&self) -> Vec<PanelKind> {
        self.dock.iter_all_tabs().map(|(_, tab)| *tab).collect()
    }

    pub fn is_open(&self, panel: PanelKind) -> bool {
        self.dock.find_tab(&panel).is_some()
    }

    /// Closes `panel`; returns whether it was open. An emptied split closes with it.
    pub fn close(&mut self, panel: PanelKind) -> bool {
        match self.dock.find_tab(&panel) {
            Some(path) => self.dock.remove_tab(path).is_some(),
            None => false,
        }
    }

    /// Opens `panel` as a tab of the focused split (or the first one), or brings it to
    /// the front when it is already open. With every panel closed it fills the window.
    pub fn restore(&mut self, panel: PanelKind) {
        if let Some(path) = self.dock.find_tab(&panel) {
            let _ = self.dock.set_active_tab(path);
            return;
        }
        if self.dock.main_surface().num_tabs() == 0 {
            self.dock = DockState::new(vec![panel]);
        } else if self.dock.focused_leaf().is_some() {
            self.dock.push_to_focused_leaf(panel);
        } else {
            self.dock.push_to_first_leaf(panel);
        }
    }

    /// Closes `panel` when open, else restores it (the Window menu's check items).
    pub fn toggle(&mut self, panel: PanelKind) {
        if !self.close(panel) {
            self.restore(panel);
        }
    }

    /// The tree without measured rectangles: splits with their fractions, leaves with
    /// their tabs and the active one.
    fn shape(&self) -> String {
        let mut out = String::new();
        for (_, surface) in self.dock.iter_surfaces_indexed() {
            let Some(tree) = surface.node_tree() else {
                out.push('-');
                continue;
            };
            for node in tree.iter() {
                use egui_dock::Node;
                match node {
                    Node::Empty => out.push('.'),
                    Node::Leaf(leaf) => {
                        out.push_str(&format!("L{:?}@{};", leaf.tabs, leaf.active.0));
                    }
                    Node::Vertical(split) => out.push_str(&format!("V{:.4};", split.fraction)),
                    Node::Horizontal(split) => {
                        out.push_str(&format!("H{:.4};", split.fraction));
                    }
                }
            }
            out.push('|');
        }
        out
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::preset(Preset::Edit)
    }
}

/// The version of the workspaces file this build writes.
pub const FORMAT_VERSION: u32 = 1;

/// Which layout is showing: a preset, or a saved workspace by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Active {
    Preset(Preset),
    Saved(String),
}

/// The current layout and the user's saved ones: the workspaces file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspaces {
    pub version: u32,
    /// What the current layout started from (the Window menu ticks it).
    pub active: Active,
    /// The layout on screen, with the user's changes since.
    pub current: Workspace,
    /// Saved workspaces by name.
    pub saved: BTreeMap<String, Workspace>,
}

impl Default for Workspaces {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            active: Active::Preset(Preset::Edit),
            current: Workspace::preset(Preset::Edit),
            saved: BTreeMap::new(),
        }
    }
}

/// Why a workspaces file could not be read or written.
#[derive(Debug)]
pub enum WorkspaceError {
    Io(std::io::Error),
    Format(serde_json::Error),
    /// Written by a newer Glass with this format version.
    Newer(u32),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Format(e) => write!(f, "not a workspaces file: {e}"),
            Self::Newer(v) => write!(f, "written by a newer Glass (format {v})"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

impl Workspaces {
    /// Shows a preset's layout, dropping unsaved changes to the current one.
    pub fn apply_preset(&mut self, preset: Preset) {
        self.current = Workspace::preset(preset);
        self.active = Active::Preset(preset);
    }

    /// Saves the current layout under `name` (trimmed; replacing one of that name) and
    /// makes it the active one. Returns false for an empty name.
    pub fn save_as(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        self.saved.insert(name.to_owned(), self.current.clone());
        self.active = Active::Saved(name.to_owned());
        true
    }

    /// Shows the saved workspace `name`; returns whether it exists.
    pub fn switch_to(&mut self, name: &str) -> bool {
        let Some(layout) = self.saved.get(name) else {
            return false;
        };
        self.current = layout.clone();
        self.active = Active::Saved(name.to_owned());
        true
    }

    /// Deletes the saved workspace `name`; the layout on screen stays.
    pub fn delete(&mut self, name: &str) -> bool {
        let removed = self.saved.remove(name).is_some();
        if removed && self.active == Active::Saved(name.to_owned()) {
            self.active = Active::Preset(Preset::Edit);
        }
        removed
    }

    /// Puts the active layout back as it was saved (or as its preset defines it).
    pub fn reset(&mut self) {
        self.current = match &self.active {
            Active::Preset(p) => Workspace::preset(*p),
            Active::Saved(name) => self
                .saved
                .get(name)
                .cloned()
                .unwrap_or_else(|| Workspace::preset(Preset::Edit)),
        };
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a workspace always serialises")
    }

    pub fn from_json(text: &str) -> Result<Self, WorkspaceError> {
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        let version: Version = serde_json::from_str(text).map_err(WorkspaceError::Format)?;
        if version.version > FORMAT_VERSION {
            return Err(WorkspaceError::Newer(version.version));
        }
        let mut workspaces: Self = serde_json::from_str(text).map_err(WorkspaceError::Format)?;
        // A file with every panel closed would open to an empty window.
        if workspaces.current.panels().is_empty() {
            workspaces.reset();
        }
        Ok(workspaces)
    }

    /// Reads a workspaces file. A missing file is `Ok(None)`.
    pub fn load(path: &Path) -> Result<Option<Self>, WorkspaceError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(WorkspaceError::Io(e)),
        }
    }

    /// Writes the file through a temporary file in the same folder, so a crash mid-write
    /// leaves the old one.
    pub fn save(&self, path: &Path) -> Result<(), WorkspaceError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(WorkspaceError::Io)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        std::fs::write(&tmp, self.to_json()).map_err(WorkspaceError::Io)?;
        std::fs::rename(&tmp, path).map_err(WorkspaceError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_hold_their_panels() {
        use PanelKind as P;
        let edit = Workspace::preset(Preset::Edit);
        let mut panels = edit.panels();
        panels.sort();
        assert_eq!(
            panels,
            [
                P::MediaPool,
                P::Preview,
                P::Timeline,
                P::Inspector,
                P::Metadata
            ]
        );
        for preset in Preset::ALL {
            let ws = Workspace::preset(preset);
            assert!(ws.is_open(P::Preview), "{preset:?}");
            assert!(ws.is_open(P::Timeline), "{preset:?}");
        }
        assert!(Workspace::preset(Preset::Colour).is_open(P::Scopes));
        assert!(Workspace::preset(Preset::Audio).is_open(P::Mixer));
        assert!(Workspace::preset(Preset::Effects).is_open(P::EffectsBrowser));
    }

    #[test]
    fn close_and_restore() {
        let mut ws = Workspace::preset(Preset::Edit);
        assert!(ws.close(PanelKind::Metadata));
        assert!(!ws.is_open(PanelKind::Metadata));
        assert!(!ws.close(PanelKind::Metadata));
        ws.restore(PanelKind::Metadata);
        assert!(ws.is_open(PanelKind::Metadata));
        // Restoring an open panel only brings it forward.
        let count = ws.panels().len();
        ws.restore(PanelKind::Metadata);
        assert_eq!(ws.panels().len(), count);
        for panel in ws.panels() {
            ws.close(panel);
        }
        assert!(ws.panels().is_empty());
        ws.restore(PanelKind::Timeline);
        assert_eq!(ws.panels(), [PanelKind::Timeline]);
        ws.toggle(PanelKind::Timeline);
        assert!(ws.panels().is_empty());
    }

    #[test]
    fn named_workspaces() {
        let mut all = Workspaces::default();
        all.current.close(PanelKind::Inspector);
        assert!(!all.save_as("  "));
        assert!(all.save_as(" Mine "));
        assert_eq!(all.active, Active::Saved("Mine".into()));
        all.apply_preset(Preset::Audio);
        assert!(all.current.is_open(PanelKind::Mixer));
        assert!(all.switch_to("Mine"));
        assert!(!all.current.is_open(PanelKind::Inspector));
        all.current.restore(PanelKind::Inspector);
        all.reset();
        assert!(!all.current.is_open(PanelKind::Inspector));
        assert!(all.delete("Mine"));
        assert_eq!(all.active, Active::Preset(Preset::Edit));
        assert!(!all.switch_to("Mine"));
    }

    #[test]
    fn newer_files_are_refused() {
        let mut all = Workspaces::default();
        all.version = FORMAT_VERSION + 1;
        assert!(matches!(
            Workspaces::from_json(&all.to_json()),
            Err(WorkspaceError::Newer(_))
        ));
        assert!(matches!(
            Workspaces::from_json("{}"),
            Err(WorkspaceError::Format(_))
        ));
    }
}
