//! The app's state between frames: the open project and its undo history, the
//! sequence on screen and its playback plan, the panels' state, the preview, autosave,
//! and the keymap with its dispatcher.
//!
//! Every change to the project goes through [`AppState::edit`], [`AppState::undo`] or
//! [`AppState::redo`], which commit to the [`History`] and then bring everything that
//! reads the project in step: the sequence is compiled into a new `TimelinePlan`, the
//! frames the edit dirtied (`DirtyRanges::between`) go to the preview with it
//! (`TimelinePlayback::set_plan`, Decision 67; a new canvas or rate restarts the
//! preview), the live mix gets the new snapshot, and autosave gets the new root.
//!
//! Keys reach commands through the keymap's dispatcher (Decision 39):
//! [`AppState::dispatch_keys`] turns this frame's key presses into command ids and
//! [`AppState::run_command`] runs each; commands the window owns (save, the keyboard
//! editor) come back as a [`ShellRequest`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine::autosave::{Autosave, AutosaveConfig, AutosaveEvent};
use engine::mix::MixSettings;
use engine::project::{
    EditError, EditLabel, History, Project, Rational, SequenceFormat, SequenceId, Time, TrackKind,
};
use engine::timeline::{DirtyFrames, DirtyRanges, TimelinePlan};
use ui::panels::timeline::panel::{TimelineAction, TimelinePanel};
use ui::panels::timeline::track_header;
use ui::shortcuts::{Chord, Dispatcher, Keymap, KeymapError, Platform, PresetId};

use crate::preview::{AudioOut, Gpu, Preview};

/// The keymap the app dispatches through, and where it is kept (`keymap.json` in the
/// config folder).
pub struct Keys {
    keymap: Keymap,
    dispatcher: Dispatcher,
    file: Option<PathBuf>,
    platform: Platform,
}

impl Keys {
    /// Reads the keymap file. A missing file means the Glass preset; an unreadable one
    /// is logged and the preset used, and the file is left alone until the user changes
    /// a key.
    pub fn load(file: Option<PathBuf>) -> Self {
        let keymap = match file.as_deref().map(Keymap::load) {
            Some(Ok(Some(keymap))) => keymap,
            Some(Ok(None)) | None => Keymap::new(PresetId::Glass),
            Some(Err(error)) => {
                tracing::warn!("keymap: {error}; using the Glass preset");
                Keymap::new(PresetId::Glass)
            }
        };
        Self::with(keymap, file)
    }

    pub fn with(keymap: Keymap, file: Option<PathBuf>) -> Self {
        let platform = Platform::current();
        Self {
            dispatcher: Dispatcher::new(&keymap, platform),
            keymap,
            file,
            platform,
        }
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub fn dispatcher(&self) -> &Dispatcher {
        &self.dispatcher
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// Takes the keymap the keyboard editor changed: rebuilds the dispatcher and saves
    /// the file.
    pub fn set(&mut self, keymap: Keymap) -> Result<(), KeymapError> {
        self.dispatcher = Dispatcher::new(&keymap, self.platform);
        self.keymap = keymap;
        match &self.file {
            Some(file) => self.keymap.save(file),
            None => Ok(()),
        }
    }

    /// The first key of command `id`, for menus (`⌘Z`).
    pub fn shortcut_text(&self, ctx: &egui::Context, id: &str) -> Option<String> {
        let chord = self.keymap.chords(id).first()?.on(self.platform);
        Some(ctx.format_shortcut(&egui::KeyboardShortcut::new(modifiers(chord), chord.key)))
    }
}

fn modifiers(chord: Chord) -> egui::Modifiers {
    let m = chord.mods;
    let mut out = egui::Modifiers::NONE;
    if m.cmd {
        out |= egui::Modifiers::COMMAND;
    }
    if m.ctrl {
        out |= egui::Modifiers::CTRL;
    }
    if m.alt {
        out |= egui::Modifiers::ALT;
    }
    if m.shift {
        out |= egui::Modifiers::SHIFT;
    }
    out
}

/// Commands the window carries out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellRequest {
    Save,
    KeyboardShortcuts,
}

/// What running a command led to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Shell(ShellRequest),
    /// No such command, or not one the app runs yet.
    Unhandled,
}

/// How the state reaches the GPU and the audio device.
#[derive(Clone, Debug)]
pub struct Engines {
    /// `None`: no preview (tests without a GPU).
    pub gpu: Option<Gpu>,
    pub audio: AudioOut,
    /// The project's autosave folder; `None` turns autosave off (tests).
    pub autosave: Option<AutosaveConfig>,
}

/// The open project and everything that follows it.
pub struct AppState {
    history: History,
    file: Option<PathBuf>,
    /// [`History::changes`] when the project was last saved or opened.
    saved_at: u64,
    sequence: Option<SequenceId>,
    plan: Option<Arc<TimelinePlan>>,
    plan_builds: u64,
    last_dirty: Option<DirtyFrames>,
    timeline: Option<TimelinePanel>,
    playhead: i64,
    preview: Option<Preview>,
    engines: Engines,
    autosave: Option<(Autosave, std::sync::mpsc::Receiver<AutosaveEvent>)>,
    pub keys: Keys,
    /// Problems to show in the status line, oldest first.
    pub messages: Vec<String>,
}

/// A new project: one 1080p sequence at 30 fps with a video and an audio track.
pub fn new_project(name: &str) -> Project {
    let uid = format!("{:x}-{:x}", engine::autosave::now_ms(), std::process::id());
    let project = Project::new(uid, name);
    let format = SequenceFormat {
        frame_rate: Rational::new(30, 1),
        ..project.settings().sequence
    };
    let built = project
        .add_sequence_with(ui::t!("app.untitled"), format)
        .and_then(|(p, seq)| {
            let (p, _) = p.add_track(seq, TrackKind::Video, "V1", None)?;
            let (p, _) = p.add_track(seq, TrackKind::Audio, "A1", None)?;
            Ok(p)
        });
    built.unwrap_or(project)
}

impl AppState {
    /// Opens `history`'s project (saved at `file`, if it has one) on its first
    /// sequence, paused at frame 0.
    pub fn new(history: History, file: Option<PathBuf>, keys: Keys, engines: Engines) -> Self {
        let mut state = Self {
            saved_at: history.changes(),
            history,
            file,
            sequence: None,
            plan: None,
            plan_builds: 0,
            last_dirty: None,
            timeline: None,
            playhead: 0,
            preview: None,
            engines,
            autosave: None,
            keys,
            messages: Vec::new(),
        };
        state.sequence = state
            .history
            .current()
            .browser_sequences()
            .next()
            .map(|s| s.id);
        state.start_autosave_from_engines();
        state.show_sequence();
        state
    }

    /// Replaces the open project: closes the old one's preview and autosave (cleanly,
    /// so it leaves nothing to recover) and opens this one.
    pub fn replace(&mut self, history: History, file: Option<PathBuf>, engines: Engines) {
        self.close();
        let keys = std::mem::replace(&mut self.keys, Keys::with(Keymap::default(), None));
        *self = Self::new(history, file, keys, engines);
    }

    fn start_autosave_from_engines(&mut self) {
        if let Some(config) = self.engines.autosave.clone() {
            self.start_autosave(config);
        }
    }

    /// Starts autosaving into `config`'s folder (when the project opened without it,
    /// while an earlier autosave there waited for the recovery prompt).
    pub fn start_autosave(&mut self, config: AutosaveConfig) {
        if self.autosave.is_some() {
            return;
        }
        self.engines.autosave = Some(config.clone());
        match Autosave::start(config, self.project().clone(), self.history.changes()) {
            Ok(handle) => self.autosave = Some(handle),
            Err(error) => self.message(format!("autosave: {error}")),
        }
    }

    fn message(&mut self, text: String) {
        tracing::warn!("{text}");
        self.messages.push(text);
    }

    pub fn project(&self) -> &Project {
        self.history.current()
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Edits since the last save or open.
    pub fn is_edited(&self) -> bool {
        self.history.changes() != self.saved_at
    }

    pub fn sequence(&self) -> Option<SequenceId> {
        self.sequence
    }

    /// The open sequence's playback plan (rebuilt on every change).
    pub fn plan(&self) -> Option<&Arc<TimelinePlan>> {
        self.plan.as_ref()
    }

    /// How many times the plan was built (once on open, once per change).
    pub fn plan_builds(&self) -> u64 {
        self.plan_builds
    }

    /// The frames the last change dirtied in the open sequence.
    pub fn last_dirty(&self) -> Option<&DirtyFrames> {
        self.last_dirty.as_ref()
    }

    pub fn timeline(&self) -> Option<&TimelinePanel> {
        self.timeline.as_ref()
    }

    pub fn timeline_mut(&mut self) -> Option<&mut TimelinePanel> {
        self.timeline.as_mut()
    }

    pub fn preview(&self) -> Option<&Preview> {
        self.preview.as_ref()
    }

    pub fn preview_mut(&mut self) -> Option<&mut Preview> {
        self.preview.as_mut()
    }

    pub fn playhead(&self) -> i64 {
        self.playhead
    }

    pub fn is_playing(&self) -> bool {
        self.preview.as_ref().is_some_and(Preview::is_playing)
    }

    /// Frames in the open sequence.
    pub fn end(&self) -> i64 {
        self.plan.as_ref().map_or(0, |p| p.end())
    }

    /// The mixer settings the preview plays with. Mute and solo are project track
    /// switches (Decision 77), which reach the mix with every edit; faders, pans, sends
    /// and buses have no place in the project yet, so they are the defaults.
    pub fn mix_settings(&self) -> MixSettings {
        MixSettings::default()
    }

    /// Compiles the open sequence and starts its preview.
    fn show_sequence(&mut self) {
        if let Some(preview) = self.preview.take() {
            preview.shutdown();
        }
        self.plan = None;
        self.timeline = None;
        let Some(seq) = self.sequence else {
            return;
        };
        if let Some(sequence) = self.project().sequence(seq) {
            self.timeline = Some(TimelinePanel::new(sequence));
        }
        self.build_plan();
        self.start_preview();
    }

    fn build_plan(&mut self) {
        let Some(seq) = self.sequence else {
            self.plan = None;
            return;
        };
        match TimelinePlan::build(self.project(), seq) {
            Ok(plan) => {
                self.plan_builds += 1;
                self.playhead = self.playhead.clamp(0, (plan.end() - 1).max(0));
                self.plan = Some(Arc::new(plan));
            }
            Err(error) => {
                self.plan = None;
                self.message(format!("playback: {error}"));
            }
        }
    }

    fn start_preview(&mut self) {
        let (Some(gpu), Some(plan), Some(seq)) =
            (self.engines.gpu.clone(), self.plan.clone(), self.sequence)
        else {
            return;
        };
        let mix = self.mix_settings();
        match Preview::start(
            &gpu,
            plan,
            self.project(),
            seq,
            &mix,
            self.engines.audio,
            self.playhead,
        ) {
            Ok(preview) => self.preview = Some(preview),
            Err(error) => self.message(format!("preview: {error}")),
        }
    }

    /// Commits `edit` of the current project as one undo step labelled `label`.
    pub fn edit(
        &mut self,
        label: EditLabel,
        edit: impl FnOnce(&Project) -> Result<Project, EditError>,
    ) -> Result<(), EditError> {
        let old = self.project().clone();
        let new = edit(&old)?;
        self.history.commit(label, new);
        self.changed(&old);
        Ok(())
    }

    /// Steps back one undo step; returns whether there was one.
    pub fn undo(&mut self) -> bool {
        let old = self.project().clone();
        if self.history.undo().is_none() {
            return false;
        }
        self.changed(&old);
        true
    }

    pub fn redo(&mut self) -> bool {
        let old = self.project().clone();
        if self.history.redo().is_none() {
            return false;
        }
        self.changed(&old);
        true
    }

    /// Brings the plan, the preview, the mix and autosave in step after the project
    /// changed from `old`.
    fn changed(&mut self, old: &Project) {
        let new = self.project().clone();
        if new.ptr_eq(old) {
            return;
        }
        if let Some(seq) = self.sequence
            && new.sequence(seq).is_none()
        {
            self.sequence = new.browser_sequences().next().map(|s| s.id);
            self.show_sequence();
        } else if let Some(seq) = self.sequence {
            let dirty = DirtyRanges::between(old, &new).sequence(seq);
            self.build_plan();
            self.last_dirty = Some(dirty.clone());
            let mix = self.mix_settings();
            let swapped = match (&mut self.preview, &self.plan) {
                (Some(preview), Some(plan)) => {
                    let ok = preview.set_plan(plan.clone(), dirty).is_ok();
                    if ok {
                        preview.update_mix(&new, &mix);
                    }
                    ok
                }
                _ => false,
            };
            if !swapped {
                // No preview yet, or a new canvas or frame rate, which needs a new
                // playback.
                self.start_preview_again();
            }
        }
        if let Some((autosave, _)) = &self.autosave {
            autosave.submit(&new, self.history.changes());
        }
    }

    fn start_preview_again(&mut self) {
        if let Some(preview) = self.preview.take() {
            preview.shutdown();
        }
        self.start_preview();
    }

    /// Moves the playhead to `frame` (held to the sequence).
    pub fn seek(&mut self, frame: i64) {
        let frame = frame.clamp(0, (self.end() - 1).max(0));
        self.playhead = frame;
        if let Some(preview) = &mut self.preview {
            preview.seek(frame);
        }
    }

    pub fn play(&mut self) {
        if self.playhead >= self.end() - 1 {
            self.seek(0);
        }
        if let Some(preview) = &mut self.preview {
            preview.play();
        }
    }

    pub fn pause(&mut self) {
        if let Some(preview) = &mut self.preview {
            preview.pause();
            if let Some(shown) = preview.shown() {
                self.playhead = shown.frame;
            }
        }
    }

    pub fn toggle_play(&mut self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Once per UI frame: takes the frames that are due and moves the playhead with
    /// playback; drains autosave's events. Returns whether the frame on screen changed.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        if let Some(preview) = &mut self.preview {
            changed = preview.poll();
            if preview.is_playing()
                && let Some(shown) = preview.shown()
            {
                self.playhead = shown.frame;
            }
        }
        let mut failed = Vec::new();
        if let Some((_, events)) = &self.autosave {
            while let Ok(event) = events.try_recv() {
                if let AutosaveEvent::Failed(why) = event {
                    failed.push(format!("autosave: {why}"));
                }
            }
        }
        for text in failed {
            self.message(text);
        }
        changed
    }

    /// Runs a command from the registry (`ui::shortcuts::COMMANDS`). `now` is egui's
    /// time, for the view's animations.
    pub fn run_command(&mut self, id: &str, now: f64) -> Outcome {
        let step = |state: &mut Self, by: i64| {
            state.pause();
            state.seek(state.playhead + by);
        };
        match id {
            "edit.undo" => {
                self.undo();
            }
            "edit.redo" => {
                self.redo();
            }
            "edit.save" => return Outcome::Shell(ShellRequest::Save),
            "view.keyboard_shortcuts" => return Outcome::Shell(ShellRequest::KeyboardShortcuts),
            "playback.play_pause" => self.toggle_play(),
            "playback.stop" => self.pause(),
            "playback.frame_back" => step(self, -1),
            "playback.frame_forward" => step(self, 1),
            "playback.go_to_start" => {
                self.pause();
                self.seek(0);
            }
            "playback.go_to_end" => {
                self.pause();
                self.seek(self.end() - 1);
            }
            "view.zoom_in" | "view.zoom_out" | "view.zoom_to_fit" => {
                let anchor = self.playhead_time();
                let Some(timeline) = &mut self.timeline else {
                    return Outcome::Unhandled;
                };
                timeline.view.run_command(id, anchor, now);
            }
            _ => return Outcome::Unhandled,
        }
        Outcome::Done
    }

    fn playhead_time(&self) -> Option<Time> {
        let seq = self.project().sequence(self.sequence?)?;
        Time::from_frames(self.playhead, seq.frame_rate).ok()
    }

    /// Runs this frame's key presses through the keymap; returns the requests for the
    /// window. Nothing runs while a text field has the keyboard.
    pub fn dispatch_keys(&mut self, ctx: &egui::Context) -> Vec<ShellRequest> {
        let ids = self.keys.dispatcher().dispatch(ctx);
        let now = ctx.input(|i| i.time);
        ids.into_iter()
            .filter_map(|id| match self.run_command(id, now) {
                Outcome::Shell(request) => Some(request),
                Outcome::Done | Outcome::Unhandled => None,
            })
            .collect()
    }

    /// Applies what the timeline panel returned.
    pub fn timeline_action(&mut self, action: TimelineAction) {
        match action {
            TimelineAction::None => {}
            TimelineAction::Seek(frame) => {
                self.pause();
                self.seek(frame);
            }
            TimelineAction::Header(header) => match track_header::edit(self.project(), &header) {
                None => {}
                Some(Ok((new, label))) => {
                    let old = self.project().clone();
                    self.history.commit(label, new);
                    self.changed(&old);
                }
                Some(Err(error)) => self.message(format!("track: {error}")),
            },
        }
    }

    /// Writes the project to `path` and makes it the project's file; autosave moves to
    /// `autosave_dir` (the file's project cache) when that changes.
    pub fn save_to(
        &mut self,
        path: &Path,
        autosave_dir: Option<PathBuf>,
    ) -> Result<(), crate::project_io::IoError> {
        crate::project_io::save(self.project(), path)?;
        self.file = Some(path.to_path_buf());
        self.saved_at = self.history.changes();
        if let Some((autosave, _)) = &self.autosave {
            match autosave_dir {
                Some(dir) => autosave.saved_as(self.project(), self.saved_at, dir),
                None => autosave.saved(self.project(), self.saved_at),
            }
        }
        Ok(())
    }

    /// Stops the preview and closes autosave cleanly (a normal quit leaves nothing to
    /// recover).
    pub fn close(&mut self) {
        if let Some(preview) = self.preview.take() {
            preview.shutdown();
        }
        if let Some((autosave, _)) = self.autosave.take() {
            autosave.close();
        }
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        if let Some(preview) = self.preview.take() {
            preview.shutdown();
        }
    }
}
