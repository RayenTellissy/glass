//! Undo and redo as a list of project roots.
//!
//! Every committed edit appends the new root; undo and redo only move an index, so they
//! cost nothing and never recompute anything. Depth is unlimited: the roots share every
//! node an edit did not touch (see [`crate::model`]), so a step costs roughly the path
//! from the root to the changed nodes. [`History::memory_stats`] reports how much is
//! shared.
//!
//! A gesture (a drag, a trim handle pulled across many frames) produces many roots but is
//! one undo step: [`History::begin_gesture`], any number of [`History::update`], then
//! [`History::end_gesture`]. Repeated discrete edits such as nudging with the arrow keys
//! can be merged into one step with [`History::commit_coalescing`].

use std::collections::HashSet;
use std::mem::size_of;
use std::sync::Arc;

use crate::model::{Item, MediaRef, Project, Sequence, Track};
use crate::transition::Transition;

/// What an undo step did, for "Undo Trim" in the Edit menu. [`EditLabel::i18n_key`] is
/// the key of its name in `assets/i18n/en.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditLabel {
    AddMedia,
    AddSequence,
    AddTrack,
    RemoveTrack,
    Insert,
    Delete,
    Move,
    Trim,
    Nudge,
    Properties,
    Slip,
    Speed,
    Enable,
    Disable,
    AddEffect,
    RemoveEffect,
    ReorderEffects,
    Animate,
    StopAnimating,
    AddKeyframe,
    ChangeKeyframe,
    DeleteKeyframe,
    MoveKeyframe,
    KeyframeInterpolation,
    KeyframeHandles,
    StretchKeyframes,
    ScaleKeyframeValues,
    AddMarker,
    ChangeMarker,
    DeleteMarker,
    ChangeTrackKind,
    AddTransition,
    RemoveTransition,
    TransitionDuration,
    TransitionAlignment,
    Transition,
    ProjectSettings,
    SequenceSettings,
    Recover,
    Relink,
}

impl EditLabel {
    pub const ALL: [EditLabel; 40] = [
        EditLabel::AddMedia,
        EditLabel::AddSequence,
        EditLabel::AddTrack,
        EditLabel::RemoveTrack,
        EditLabel::Insert,
        EditLabel::Delete,
        EditLabel::Move,
        EditLabel::Trim,
        EditLabel::Nudge,
        EditLabel::Properties,
        EditLabel::Slip,
        EditLabel::Speed,
        EditLabel::Enable,
        EditLabel::Disable,
        EditLabel::AddEffect,
        EditLabel::RemoveEffect,
        EditLabel::ReorderEffects,
        EditLabel::Animate,
        EditLabel::StopAnimating,
        EditLabel::AddKeyframe,
        EditLabel::ChangeKeyframe,
        EditLabel::DeleteKeyframe,
        EditLabel::MoveKeyframe,
        EditLabel::KeyframeInterpolation,
        EditLabel::KeyframeHandles,
        EditLabel::StretchKeyframes,
        EditLabel::ScaleKeyframeValues,
        EditLabel::AddMarker,
        EditLabel::ChangeMarker,
        EditLabel::DeleteMarker,
        EditLabel::ChangeTrackKind,
        EditLabel::AddTransition,
        EditLabel::RemoveTransition,
        EditLabel::TransitionDuration,
        EditLabel::TransitionAlignment,
        EditLabel::Transition,
        EditLabel::ProjectSettings,
        EditLabel::SequenceSettings,
        EditLabel::Recover,
        EditLabel::Relink,
    ];

    pub fn i18n_key(self) -> &'static str {
        match self {
            EditLabel::AddMedia => "edit.label.add_media",
            EditLabel::AddSequence => "edit.label.add_sequence",
            EditLabel::AddTrack => "edit.label.add_track",
            EditLabel::RemoveTrack => "edit.label.remove_track",
            EditLabel::Insert => "edit.label.insert",
            EditLabel::Delete => "edit.label.delete",
            EditLabel::Move => "edit.label.move",
            EditLabel::Trim => "edit.label.trim",
            EditLabel::Nudge => "edit.label.nudge",
            EditLabel::Properties => "edit.label.properties",
            EditLabel::Slip => "edit.label.slip",
            EditLabel::Speed => "edit.label.speed",
            EditLabel::Enable => "edit.label.enable",
            EditLabel::Disable => "edit.label.disable",
            EditLabel::AddEffect => "edit.label.add_effect",
            EditLabel::RemoveEffect => "edit.label.remove_effect",
            EditLabel::ReorderEffects => "edit.label.reorder_effects",
            EditLabel::Animate => "edit.label.animate",
            EditLabel::StopAnimating => "edit.label.stop_animating",
            EditLabel::AddKeyframe => "edit.label.add_keyframe",
            EditLabel::ChangeKeyframe => "edit.label.change_keyframe",
            EditLabel::DeleteKeyframe => "edit.label.delete_keyframe",
            EditLabel::MoveKeyframe => "edit.label.move_keyframe",
            EditLabel::KeyframeInterpolation => "edit.label.keyframe_interpolation",
            EditLabel::KeyframeHandles => "edit.label.keyframe_handles",
            EditLabel::StretchKeyframes => "edit.label.stretch_keyframes",
            EditLabel::ScaleKeyframeValues => "edit.label.scale_keyframe_values",
            EditLabel::AddMarker => "edit.label.add_marker",
            EditLabel::ChangeMarker => "edit.label.change_marker",
            EditLabel::DeleteMarker => "edit.label.delete_marker",
            EditLabel::ChangeTrackKind => "edit.label.change_track_kind",
            EditLabel::AddTransition => "edit.label.add_transition",
            EditLabel::RemoveTransition => "edit.label.remove_transition",
            EditLabel::TransitionDuration => "edit.label.transition_duration",
            EditLabel::TransitionAlignment => "edit.label.transition_alignment",
            EditLabel::Transition => "edit.label.transition",
            EditLabel::ProjectSettings => "edit.label.project_settings",
            EditLabel::SequenceSettings => "edit.label.sequence_settings",
            EditLabel::Recover => "edit.label.recover",
            EditLabel::Relink => "edit.label.relink",
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    root: Project,
    /// The edit that produced this root; `None` for the initial root.
    label: Option<EditLabel>,
    /// Set by [`History::commit_coalescing`]; a later commit with the same key merges.
    coalesce: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct Gesture {
    label: EditLabel,
    /// Whether this gesture has pushed its entry yet.
    pushed: bool,
}

/// The undo history of one project.
#[derive(Clone, Debug)]
pub struct History {
    entries: Vec<Entry>,
    /// The entry whose root is current.
    index: usize,
    gesture: Option<Gesture>,
    /// Grows by one every time the current root changes; see [`History::changes`].
    changes: u64,
}

impl History {
    pub fn new(root: Project) -> Self {
        Self {
            entries: vec![Entry {
                root,
                label: None,
                coalesce: None,
            }],
            index: 0,
            gesture: None,
            changes: 0,
        }
    }

    /// The current root.
    pub fn current(&self) -> &Project {
        &self.entries[self.index].root
    }

    /// Commits one edit as its own undo step and drops any redo steps. A root equal to
    /// the current one (a no-op edit) adds nothing. Inside a gesture this is
    /// [`History::update`].
    pub fn commit(&mut self, label: EditLabel, root: Project) {
        if self.gesture.is_some() {
            self.update(root);
            return;
        }
        self.push(label, root, None);
    }

    /// Like [`History::commit`], but merges into the previous step when that step was
    /// committed with the same `key` and nothing was undone since. Callers pick a key
    /// per run of repeated edits (for example the selection plus the nudge direction).
    pub fn commit_coalescing(&mut self, label: EditLabel, key: u64, root: Project) {
        if self.gesture.is_some() {
            self.update(root);
            return;
        }
        let at_top = self.index + 1 == self.entries.len() && self.index > 0;
        let top = &mut self.entries[self.index];
        if at_top && top.coalesce == Some(key) {
            top.root = root;
            top.label = Some(label);
            return;
        }
        self.push(label, root, Some(key));
    }

    /// Starts a gesture: every root until [`History::end_gesture`] becomes one step.
    /// A gesture already in progress is ended first.
    pub fn begin_gesture(&mut self, label: EditLabel) {
        self.end_gesture();
        self.gesture = Some(Gesture {
            label,
            pushed: false,
        });
    }

    /// The latest root of the gesture in progress; outside a gesture, a plain commit
    /// labelled [`EditLabel::Properties`].
    pub fn update(&mut self, root: Project) {
        match self.gesture {
            None => self.push(EditLabel::Properties, root, None),
            Some(Gesture { pushed: true, .. }) => self.entries[self.index].root = root,
            Some(Gesture {
                label,
                pushed: false,
            }) => {
                if !root.content_eq(self.current()) {
                    self.push(label, root, None);
                    self.gesture = Some(Gesture {
                        label,
                        pushed: true,
                    });
                }
            }
        }
    }

    /// Ends the gesture in progress. Returns true when it left an undo step. A gesture
    /// that ends on the root it started from leaves none.
    pub fn end_gesture(&mut self) -> bool {
        let Some(gesture) = self.gesture.take() else {
            return false;
        };
        if !gesture.pushed {
            return false;
        }
        if self.entries[self.index]
            .root
            .content_eq(&self.entries[self.index - 1].root)
        {
            self.entries.pop();
            self.index -= 1;
            return false;
        }
        true
    }

    /// Abandons the gesture in progress and returns to the root it started from.
    pub fn cancel_gesture(&mut self) {
        if let Some(Gesture { pushed: true, .. }) = self.gesture.take() {
            self.entries.pop();
            self.index -= 1;
            self.changes += 1;
        }
    }

    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    pub fn can_undo(&self) -> bool {
        self.index > 0
    }

    pub fn can_redo(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    /// The label of the step [`History::undo`] would undo.
    pub fn undo_label(&self) -> Option<EditLabel> {
        self.entries[self.index].label
    }

    /// The label of the step [`History::redo`] would redo.
    pub fn redo_label(&self) -> Option<EditLabel> {
        self.entries.get(self.index + 1).and_then(|e| e.label)
    }

    /// Steps back one undo step and returns the new current root. Ends a gesture in
    /// progress first.
    pub fn undo(&mut self) -> Option<&Project> {
        self.end_gesture();
        if !self.can_undo() {
            return None;
        }
        self.index -= 1;
        self.changes += 1;
        Some(self.current())
    }

    pub fn redo(&mut self) -> Option<&Project> {
        self.end_gesture();
        if !self.can_redo() {
            return None;
        }
        self.index += 1;
        self.changes += 1;
        Some(self.current())
    }

    /// A counter that grows by one whenever a commit, a gesture's first change, an undo,
    /// a redo or a cancelled gesture changes the current root. Autosave counts the edits
    /// between two ticks with it. Updates inside a gesture and coalesced commits replace
    /// the current root without counting again.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// The number of undo steps available.
    pub fn undo_depth(&self) -> usize {
        self.index
    }

    /// The number of roots held, the initial one included.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Every root held, oldest first.
    pub fn roots(&self) -> impl Iterator<Item = &Project> {
        self.entries.iter().map(|e| &e.root)
    }

    /// Nodes held across all roots, counting each shared node once. `approx_bytes` covers
    /// the nodes themselves and their strings, not the internal chunks of the persistent
    /// collections, which are shared the same way.
    pub fn memory_stats(&self) -> MemoryStats {
        let mut seen = HashSet::new();
        let mut stats = MemoryStats {
            roots: self.entries.len(),
            ..MemoryStats::default()
        };
        let mut visit = |ptr: usize, bytes: usize, stats: &mut MemoryStats| -> bool {
            stats.node_refs += 1;
            if seen.insert(ptr) {
                stats.unique_nodes += 1;
                stats.approx_bytes += bytes;
                true
            } else {
                false
            }
        };
        for entry in &self.entries {
            for media in entry.root.media.iter() {
                let bytes = size_of::<MediaRef>() + media.path.len() + media.content_hash.len();
                visit(arc_addr(media), bytes, &mut stats);
            }
            for seq in &entry.root.sequences {
                if !visit(
                    arc_addr(seq),
                    size_of::<Sequence>() + seq.name.len(),
                    &mut stats,
                ) {
                    continue;
                }
                for track in &seq.tracks {
                    let bytes = size_of::<Track>() + track.name.len();
                    if !visit(arc_addr(track), bytes, &mut stats) {
                        continue;
                    }
                    for item in &track.items {
                        visit(arc_addr(item), size_of::<Item>(), &mut stats);
                    }
                    for transition in &track.transitions {
                        let bytes = size_of::<Transition>()
                            + transition.kind.len()
                            + transition.easing.len();
                        visit(arc_addr(transition), bytes, &mut stats);
                    }
                }
            }
        }
        stats
    }

    fn push(&mut self, label: EditLabel, root: Project, coalesce: Option<u64>) {
        if root.content_eq(self.current()) {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(Entry {
            root,
            label: Some(label),
            coalesce,
        });
        self.index += 1;
        self.changes += 1;
    }
}

fn arc_addr<T>(arc: &Arc<T>) -> usize {
    Arc::as_ptr(arc) as usize
}

/// See [`History::memory_stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryStats {
    pub roots: usize,
    /// Distinct node allocations across all roots.
    pub unique_nodes: usize,
    /// Node references across all roots, as if nothing were shared (shared subtrees are
    /// counted once per root at their top node only).
    pub node_refs: usize,
    pub approx_bytes: usize,
}
