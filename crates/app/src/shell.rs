//! The window: an eframe app that draws the menu bar, the docked workspace
//! (`ui::workspace`) and the dialogs over it, and feeds the [`AppState`] what the user
//! does.
//!
//! Each frame: the preview takes its due frames and the one on screen is handed to the
//! egui renderer as a texture; the keymap's dispatcher turns key presses into commands
//! (none while a dialog is up); the menu and the panels draw and return what the user
//! did, which is applied after drawing; a recovery prompt, the About dialog or the
//! keyboard editor draw on top. The window repaints continuously while playing, every
//! frame for a moment after anything that will present a new frame, and otherwise only
//! on input.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Id, Order, Rect, Sense, ViewportCommand, vec2};
use engine::autosave::AutosaveConfig;
use engine::project::{History, Sequence};
use engine::render::wgpu;
use ui::dialogs::about::{AboutAction, AboutDialog, AboutInfo};
use ui::dialogs::recovery::RecoveryAction;
use ui::dialogs::shortcuts::{self, ShortcutsAction, ShortcutsEditor};
use ui::panels::preview::{PreviewAction, PreviewView};
use ui::panels::timeline::clip_media::ClipMedia;
use ui::panels::timeline::clips::{self as timeline_clips, ClipsView};
use ui::panels::timeline::ruler::Timecode;
use ui::panels::timeline::view::{TimelineView, TrackLayout};
use ui::workspace::Workspaces;
use ui::workspace::dock;
use ui::workspace::menu::{self, MenuCommand, MenuView};
use ui::workspace::panels::{PanelEvent, Panels, TimelineContent};

use crate::locations::Locations;
use crate::picker::{ProjectPicker, Purpose};
use crate::pool::Pool;
use crate::preview::{AudioOut, Gpu};
use crate::recent::{Recent, RecentProject};
use crate::recovery::{LaunchRecovery, Resolution};
use crate::state::{AppState, Engines, Keys, ShellRequest, new_project};

/// How long after a seek, an edit or an open the window keeps repainting every frame
/// so the new picture shows as soon as playback presents it.
const WAKE_SECONDS: f64 = 1.5;

/// The window's state.
pub struct Shell {
    locations: Locations,
    about_info: AboutInfo,
    state: AppState,
    workspaces: Workspaces,
    menu: MenuView,
    pool: Pool,
    /// Thumbnails and waveforms for the clips; filled by the analysis feed once it is
    /// wired (until then clips draw their placeholders).
    clip_media: ClipMedia,
    about: Option<AboutDialog>,
    recoveries: VecDeque<LaunchRecovery>,
    shortcuts: Option<ShortcutsEditor>,
    picker: ProjectPicker,
    recent: Recent,
    gpu: Option<Gpu>,
    render_state: Option<egui_wgpu::RenderState>,
    texture: Option<(egui::TextureId, [u32; 2])>,
    title: String,
    wake_until: f64,
    /// The layout changed since it was last written.
    layout_dirty: bool,
    /// Autosave for the project just opened, held back while its unclean autosave waits
    /// for the recovery prompt (starting one would overwrite it).
    held_autosave: Option<AutosaveConfig>,
}

/// The device features the preview needs, asked of eframe's adapter when it has them.
pub fn wgpu_options() -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    let base = setup.device_descriptor.clone();
    setup.device_descriptor = Arc::new(move |adapter: &wgpu::Adapter| {
        let mut descriptor = base(adapter);
        descriptor.required_features |= adapter.features() & Gpu::FEATURES;
        descriptor
    });
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

impl Shell {
    /// Builds the window state: the theme, the GPU device eframe created, the keymap and
    /// workspaces from the config folder, the launch recovery scan, and the project
    /// named on the command line (or a new one).
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        locations: Locations,
        about_info: AboutInfo,
        open: Option<PathBuf>,
    ) -> Self {
        ui::theme::set(&cc.egui_ctx, ui::theme::ThemeKind::default());
        let render_state = cc.wgpu_render_state.clone();
        let gpu = render_state.as_ref().and_then(|rs| {
            if rs.device.features().contains(Gpu::FEATURES) {
                Some(Gpu {
                    device: rs.device.clone(),
                    queue: rs.queue.clone(),
                })
            } else {
                tracing::warn!("the GPU has no 16-bit normalised textures; the preview is off");
                None
            }
        });
        let keys = Keys::load(Some(locations.keymap_file()));
        let workspaces = match Workspaces::load(&locations.workspaces_file()) {
            Ok(Some(w)) => w,
            Ok(None) => Workspaces::default(),
            Err(error) => {
                tracing::warn!("workspaces: {error}; using the Edit preset");
                Workspaces::default()
            }
        };
        let recent = Recent::load(&locations.recent_file());
        let recoveries: VecDeque<LaunchRecovery> =
            LaunchRecovery::scan_all(&locations.cache, &recent, engine::autosave::now_ms()).into();
        let project = new_project(&ui::t!("app.untitled"));
        let engines = Engines {
            gpu: gpu.clone(),
            audio: AudioOut::Device,
            autosave: Some(AutosaveConfig::new(locations.autosave_dir(&project, None))),
        };
        let state = AppState::new(History::new(project), None, keys, engines);
        let mut shell = Self {
            locations,
            about_info,
            state,
            workspaces,
            menu: MenuView::default(),
            pool: Pool::default(),
            clip_media: ClipMedia::new(),
            about: None,
            recoveries,
            shortcuts: None,
            picker: ProjectPicker::default(),
            recent,
            gpu,
            render_state,
            texture: None,
            title: String::new(),
            wake_until: 0.0,
            layout_dirty: false,
            held_autosave: None,
        };
        if let Some(path) = open {
            shell.open_path(&path);
        }
        shell
    }

    fn engines(&self, autosave: Option<AutosaveConfig>) -> Engines {
        Engines {
            gpu: self.gpu.clone(),
            audio: AudioOut::Device,
            autosave,
        }
    }

    /// Opens the project at `path` (see `project_io::open`), offering its autosave when
    /// the last session with it ended uncleanly.
    pub fn open_path(&mut self, path: &Path) {
        let opened = match crate::project_io::open(path) {
            Ok(opened) => opened,
            Err(error) => {
                self.state.messages.push(ui::t!(
                    "app.open_failed",
                    path = path.display(),
                    error = error
                ));
                return;
            }
        };
        let project = opened.project;
        let autosave_dir = self.locations.autosave_dir(&project, Some(path));
        let mut autosave = Some(AutosaveConfig::new(autosave_dir.clone()));
        self.held_autosave = None;
        if let Some(found) =
            LaunchRecovery::for_project(&autosave_dir, path, engine::autosave::now_ms())
        {
            self.recoveries.push_front(found);
            self.held_autosave = autosave.take();
        }
        let cache = self.locations.project_cache(&project, Some(path));
        self.recent.opened(RecentProject {
            path: path.to_path_buf(),
            cache,
        });
        self.save_recent();
        let engines = self.engines(autosave);
        self.state
            .replace(History::new(project), Some(path.to_path_buf()), engines);
        if !opened.missing.is_empty() {
            self.state
                .messages
                .push(ui::t!("app.media_missing", count = opened.missing.len()));
        }
    }

    fn save_recent(&self) {
        if let Err(error) = self.recent.save(&self.locations.recent_file()) {
            tracing::warn!("recent projects: {error}");
        }
    }

    fn save_workspaces(&self) {
        if let Err(error) = self.workspaces.save(&self.locations.workspaces_file()) {
            tracing::warn!("workspaces: {error}");
        }
    }

    /// Saves to the project's file, or asks for one.
    fn save(&mut self, ask: bool) {
        match self.state.file().map(Path::to_path_buf) {
            Some(path) if !ask => self.save_to(&path),
            _ => {
                let name = self.state.project().name().to_owned();
                self.picker.open(Purpose::Save, &name);
            }
        }
    }

    fn save_to(&mut self, path: &Path) {
        let dir = self
            .locations
            .autosave_dir(self.state.project(), Some(path));
        match self.state.save_to(path, Some(dir)) {
            Ok(()) => {
                let cache = self
                    .locations
                    .project_cache(self.state.project(), Some(path));
                self.recent.opened(RecentProject {
                    path: path.to_path_buf(),
                    cache,
                });
                self.save_recent();
            }
            Err(error) => self.state.messages.push(ui::t!(
                "app.save_failed",
                path = path.display(),
                error = error
            )),
        }
    }

    fn menu_command(&mut self, ctx: &egui::Context, command: MenuCommand) {
        match command {
            MenuCommand::Open => self.picker.open(Purpose::Open, ""),
            MenuCommand::Save => self.save(false),
            MenuCommand::SaveAs => self.save(true),
            MenuCommand::Undo => {
                self.state.undo();
            }
            MenuCommand::Redo => {
                self.state.redo();
            }
            MenuCommand::Preset(preset) => {
                self.workspaces.apply_preset(preset);
                self.save_workspaces();
            }
            MenuCommand::Workspace(name) => {
                self.workspaces.switch_to(&name);
                self.save_workspaces();
            }
            MenuCommand::SaveWorkspace(name) => {
                self.workspaces.save_as(&name);
                self.save_workspaces();
            }
            MenuCommand::DeleteWorkspace(name) => {
                self.workspaces.delete(&name);
                self.save_workspaces();
            }
            MenuCommand::ResetWorkspace => {
                self.workspaces.reset();
                self.save_workspaces();
            }
            MenuCommand::TogglePanel(panel) => {
                self.workspaces.current.toggle(panel);
                self.save_workspaces();
            }
            MenuCommand::KeyboardShortcuts => self.shell_request(ShellRequest::KeyboardShortcuts),
            MenuCommand::About => self.about = Some(AboutDialog::new()),
        }
        self.wake(ctx);
    }

    fn shell_request(&mut self, request: ShellRequest) {
        match request {
            ShellRequest::Save => self.save(false),
            ShellRequest::KeyboardShortcuts => {
                self.shortcuts = Some(ShortcutsEditor::new(self.state.keys.platform()));
            }
        }
    }

    fn wake(&mut self, ctx: &egui::Context) {
        self.wake_until = ctx.input(|i| i.time) + WAKE_SECONDS;
    }

    /// Hands the frame on screen to the egui renderer.
    fn update_texture(&mut self) {
        let (Some(rs), Some(preview)) = (&self.render_state, self.state.preview()) else {
            self.texture = None;
            return;
        };
        let Some(shown) = preview.shown() else {
            return;
        };
        let view = shown
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = [shown.texture.width(), shown.texture.height()];
        let mut renderer = rs.renderer.write();
        match self.texture {
            Some((id, _)) => renderer.update_egui_texture_from_wgpu_texture(
                &rs.device,
                &view,
                wgpu::FilterMode::Linear,
                id,
            ),
            None => {
                let id =
                    renderer.register_native_texture(&rs.device, &view, wgpu::FilterMode::Linear);
                self.texture = Some((id, size));
            }
        }
        if let Some((id, _)) = self.texture {
            self.texture = Some((id, size));
        }
    }

    fn preview_view(&self) -> PreviewView {
        let state = &self.state;
        let Some(sequence) = state.sequence().and_then(|s| state.project().sequence(s)) else {
            return PreviewView {
                empty: true,
                ..PreviewView::default()
            };
        };
        let tc = Timecode::for_sequence(sequence);
        PreviewView {
            picture: self
                .texture
                .filter(|_| state.preview().is_some_and(|p| p.shown().is_some())),
            canvas: (sequence.resolution.width, sequence.resolution.height),
            frame: state.playhead(),
            end: state.end(),
            timecode: tc.format(state.playhead()),
            duration: tc.format(state.end()),
            playing: state.is_playing(),
            silent: state.preview().is_none_or(|p| p.silent()),
            empty: false,
        }
    }

    fn panel_events(&mut self, events: Vec<PanelEvent>) {
        for event in events {
            match event {
                PanelEvent::Preview(action) => match action {
                    PreviewAction::None => {}
                    PreviewAction::TogglePlay => self.state.toggle_play(),
                    PreviewAction::GoToStart => {
                        self.state.run_command("playback.go_to_start", 0.0);
                    }
                    PreviewAction::GoToEnd => {
                        self.state.run_command("playback.go_to_end", 0.0);
                    }
                    PreviewAction::FrameBack => {
                        self.state.run_command("playback.frame_back", 0.0);
                    }
                    PreviewAction::FrameForward => {
                        self.state.run_command("playback.frame_forward", 0.0);
                    }
                    PreviewAction::Seek(frame) => {
                        self.state.pause();
                        self.state.seek(frame);
                    }
                },
                PanelEvent::Timeline(action) => self.state.timeline_action(action),
                PanelEvent::MediaPool(action) => {
                    let project = self.state.project().clone();
                    self.pool.action(&action, &project);
                }
                PanelEvent::Metadata(_) => {}
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(front) = self.recoveries.front_mut()
            && let Some(action) = front.dialog.show(ctx, &front.prompt)
        {
            let file = front.file.clone();
            match front.resolve(action) {
                Ok(Some(Resolution::Restored { history, autosave })) => {
                    self.recoveries.pop_front();
                    self.held_autosave = None;
                    let engines = self.engines(Some(autosave));
                    self.state.replace(history, file, engines);
                    self.wake(ctx);
                }
                Ok(Some(Resolution::Discarded)) => {
                    self.recoveries.pop_front();
                    if let Some(config) = self.held_autosave.take() {
                        self.state.start_autosave(config);
                    }
                }
                Ok(Some(Resolution::Kept)) => {
                    self.recoveries.pop_front();
                    if self.held_autosave.take().is_some() {
                        tracing::warn!(
                            "the project's earlier autosave was kept, so this session is not autosaved"
                        );
                    }
                }
                Ok(Some(Resolution::ShowFolder(dir))) => {
                    if let Err(error) = engine::media::export::verify::reveal_in_file_manager(&dir)
                    {
                        tracing::warn!("{}: {error}", dir.display());
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.state.messages.push(error.to_string());
                    self.recoveries.pop_front();
                }
            }
            if action != RecoveryAction::Restore {
                ctx.request_repaint();
            }
            return;
        }
        if let Some(about) = &mut self.about {
            match about.show(ctx, &self.about_info) {
                Some(AboutAction::Close) => self.about = None,
                Some(AboutAction::OpenUrl(url)) => ctx.open_url(egui::OpenUrl::new_tab(url)),
                None => {}
            }
            return;
        }
        if let Some(editor) = &mut self.shortcuts {
            let screen = ctx.content_rect();
            let width = (screen.width() - 32.0).clamp(shortcuts::MIN_WIDTH, shortcuts::WIDTH);
            let mut keymap = self.state.keys.keymap().clone();
            let height = editor.height(&keymap).min(screen.height() - 32.0);
            let rect = Rect::from_center_size(screen.center(), vec2(width, height));
            let paint = ui::panels::widgets::Paint::new(ctx);
            let scrim = paint.mix(ui::theme::Role::Bg0, paint.theme.tokens().opacity.glass);
            egui::Area::new(Id::new("glass.shortcuts.scrim"))
                .order(Order::Middle)
                .fixed_pos(screen.min)
                .show(ctx, |ui| {
                    ui.painter().rect_filled(screen, 0.0, scrim);
                    ui.allocate_rect(screen, Sense::click());
                });
            let mut response = None;
            egui::Area::new(Id::new("glass.shortcuts"))
                .order(Order::Foreground)
                .fixed_pos(rect.min)
                .show(ctx, |ui| {
                    response = Some(editor.show(ui, rect, &mut keymap))
                });
            if let Some(response) = response {
                if response.changed
                    && let Err(error) = self.state.keys.set(keymap)
                {
                    self.state.messages.push(error.to_string());
                }
                match response.action {
                    Some(ShortcutsAction::Close) => self.shortcuts = None,
                    // Import and export from the preset menu are the shortcuts task's
                    // remaining part (PLAN.md Phase 6).
                    Some(ShortcutsAction::Import | ShortcutsAction::Export) | None => {}
                }
            }
        }
    }

    fn modal_open(&self) -> bool {
        !self.recoveries.is_empty() || self.about.is_some() || self.shortcuts.is_some()
    }

    fn window_title(&mut self, ctx: &egui::Context) {
        let project = self.state.project().name().to_owned();
        let title = if self.state.is_edited() {
            ui::t!("app.window_title_edited", project = project)
        } else {
            ui::t!("app.window_title", project = project)
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn menu_view(&mut self, ctx: &egui::Context) {
        let history = self.state.history();
        self.menu.undo = history
            .can_undo()
            .then(|| history.undo_label().map(menu::undo_text))
            .map(|l| l.unwrap_or_else(|| ui::t!("shortcuts.cmd.edit.undo")));
        self.menu.redo = history
            .can_redo()
            .then(|| history.redo_label().map(menu::redo_text))
            .map(|l| l.unwrap_or_else(|| ui::t!("shortcuts.cmd.edit.redo")));
        self.menu.can_save = true;
        let keys = &self.state.keys;
        self.menu.shortcuts = [
            "edit.undo",
            "edit.redo",
            "edit.save",
            "view.keyboard_shortcuts",
        ]
        .into_iter()
        .filter_map(|id| keys.shortcut_text(ctx, id).map(|s| (id, s)))
        .collect();
    }

    /// One frame of the whole window into `ui`.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.state.tick() {
            self.update_texture();
        }
        if let Some((purpose, path)) = self.picker.poll() {
            match (purpose, path) {
                (Purpose::Open, Some(path)) => self.open_path(&path),
                (Purpose::Save, Some(path)) => self.save_to(&path),
                (_, None) => {}
            }
            self.wake(&ctx);
        }
        if !self.modal_open() {
            for request in self.state.dispatch_keys(&ctx) {
                self.shell_request(request);
                self.wake(&ctx);
            }
        }
        self.pool.refresh(self.state.project());
        self.menu_view(&ctx);

        let mut command = None;
        egui::Panel::top("glass.menu").show(ui, |ui| {
            command = menu::menu_bar(ui, &mut self.menu, &self.workspaces);
        });
        if let Some(text) = self.state.messages.last().cloned() {
            egui::Panel::bottom("glass.status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(text);
                    if ui.small_button(ui::t!("about.done")).clicked() {
                        self.state.messages.clear();
                    }
                });
            });
        }

        let preview = self.preview_view();
        let sequence: Option<Arc<Sequence>> = self
            .state
            .sequence()
            .and_then(|s| self.state.project().sequence(s))
            .cloned();
        let playhead = self.state.playhead();
        let metadata = self.pool.metadata.clone();
        let project = self.state.project().clone();
        let clip_media = &mut self.clip_media;
        let mut clips =
            |ui: &mut egui::Ui, rect: Rect, view: &TimelineView, layout: &TrackLayout| {
                if let Some(sequence) = &sequence {
                    let _ = timeline_clips::clips(
                        ui,
                        rect,
                        view,
                        layout,
                        &ClipsView::new(&project, sequence),
                        clip_media,
                        &(),
                    );
                }
            };
        let layout_before = self.workspaces.current.clone();
        let events = {
            let mut panels = Panels {
                pool: &mut self.pool.view,
                metadata: &metadata,
                preview: &preview,
                timeline: None,
                events: Vec::new(),
            };
            if let (Some(sequence), Some(timeline)) = (&sequence, self.state.timeline_mut()) {
                panels.timeline = Some(TimelineContent {
                    panel: timeline,
                    sequence,
                    playhead,
                    layer: Some(&mut clips),
                });
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    if self.workspaces.current.panels().is_empty() {
                        dock::empty(ui, ui.max_rect());
                    } else {
                        dock::show(
                            ui,
                            Id::new("glass.workspace"),
                            &mut self.workspaces.current,
                            &mut panels,
                        );
                    }
                });
            panels.events
        };
        if !events.is_empty() {
            self.wake(&ctx);
        }
        self.panel_events(events);
        self.layout_dirty |= self.workspaces.current != layout_before;
        if self.layout_dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.layout_dirty = false;
            self.save_workspaces();
        }
        if let Some(command) = command {
            self.menu_command(&ctx, command);
        }
        self.dialogs(&ctx);
        self.window_title(&ctx);

        let now = ctx.input(|i| i.time);
        if self.state.is_playing() || now < self.wake_until {
            ctx.request_repaint();
        }
    }

    /// The app is quitting: stop playback and close autosave cleanly.
    pub fn quit(&mut self) {
        self.save_workspaces();
        self.state.close();
    }
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self) {
        self.quit();
    }
}
