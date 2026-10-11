//! The registered cases, one explicit list. A panel, dialog or parameter widget adds a
//! line to [`all`] pointing at a draw function with realistic sample content (its own
//! `snapshot_cases()` or a function here that calls its public API).

use std::sync::OnceLock;

use assets::licences::{Component, Kind};
use egui::{Align2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use engine::media_mgmt::{DropSummary, Unreadable};
use project::media_pool::{Facts, Gps, Metadata};
use project::params::{ParamDef, ParamSchema, ParamValue, ParamValues, Range, Unit, builtin};

use super::{
    SnapshotCase, clip_cases as cl, pool_cases, relink_cases, timeline_cases as tl,
    track_header_cases as th, workspace_cases as ws,
};
use crate::dialogs::about::{self, AboutDialog, AboutInfo, Licence, View};
use crate::dialogs::recovery::{self, RecoveryDialog, RecoveryPrompt, RecoveryState};
use crate::dialogs::shortcuts::{self, ShortcutsEditor};
use crate::panels::format;
use crate::panels::import::{
    self, DropArea, FOLDER_DIALOG_WIDTH, FolderImport, FolderRow, ImportProgress,
    PROGRESS_STRIP_HEIGHT,
};
use crate::panels::metadata::{self, METADATA_PANEL_HEIGHT, MetadataView};
use crate::panels::widgets::{Icon, Paint, Type, icon, size};
use crate::params::paint::{self as param_paint, FieldState, SliderState, StepperHover};
use crate::params::{KeyframeInfo, KeyframeMark, Section};
use crate::shortcuts::{Chord, Keymap, Platform};
use crate::t;
use crate::theme::{self, Role};

pub(super) fn all() -> Vec<SnapshotCase> {
    vec![
        SnapshotCase {
            name: "theme_showcase",
            module: "theme",
            size: vec2(480.0, 320.0),
            draw: theme_showcase,
        },
        SnapshotCase {
            name: "sample_timeline",
            module: "theme",
            size: vec2(560.0, 200.0),
            draw: sample_timeline,
        },
        SnapshotCase {
            name: "import_drop_empty",
            module: "panels/import",
            size: vec2(320.0, 240.0),
            draw: import_drop_empty,
        },
        SnapshotCase {
            name: "import_drop_over",
            module: "panels/import",
            size: vec2(640.0, 160.0),
            draw: import_drop_over,
        },
        SnapshotCase {
            name: "import_folder_dialog",
            module: "panels/import",
            size: vec2(FOLDER_DIALOG_WIDTH + 32.0, 574.0),
            draw: import_folder_dialog,
        },
        SnapshotCase {
            name: "import_progress_strip",
            module: "panels/import",
            size: vec2(360.0, PROGRESS_STRIP_HEIGHT + 16.0),
            draw: import_progress_strip,
        },
        SnapshotCase {
            name: "import_drag_ghost",
            module: "panels/import",
            size: vec2(320.0, 64.0),
            draw: import_drag_ghost,
        },
        SnapshotCase {
            name: "metadata_panel",
            module: "panels/metadata",
            size: vec2(880.0, METADATA_PANEL_HEIGHT),
            draw: metadata_panel,
        },
        SnapshotCase {
            name: "metadata_panel_empty",
            module: "panels/metadata",
            size: vec2(880.0, METADATA_PANEL_HEIGHT),
            draw: metadata_panel_empty,
        },
        SnapshotCase {
            name: "file_picker_entry",
            module: "panels/file_dialog",
            size: vec2(220.0, 200.0),
            draw: file_picker_entry,
        },
        SnapshotCase {
            name: "format_list_columns",
            module: "panels/format",
            size: vec2(520.0, 168.0),
            draw: format_list_columns,
        },
        SnapshotCase {
            name: "media_pool_grid",
            module: "panels/media_pool",
            size: pool_cases::GRID_SIZE,
            draw: pool_cases::grid,
        },
        SnapshotCase {
            name: "media_pool_list",
            module: "panels/media_pool",
            size: pool_cases::LIST_SIZE,
            draw: pool_cases::list,
        },
        SnapshotCase {
            name: "media_pool_label_menu",
            module: "panels/media_pool",
            size: pool_cases::MENU_SIZE,
            draw: pool_cases::label_menu,
        },
        SnapshotCase {
            name: "track_headers_default",
            module: "panels/timeline/track_header",
            size: th::DEFAULT_SIZE,
            draw: th::default,
        },
        SnapshotCase {
            name: "track_headers_states",
            module: "panels/timeline/track_header",
            size: th::STATES_SIZE,
            draw: th::states,
        },
        SnapshotCase {
            name: "track_headers_mix",
            module: "panels/timeline/track_header",
            size: th::MIX_SIZE,
            draw: th::mix,
        },
        SnapshotCase {
            name: "track_headers_heights",
            module: "panels/timeline/track_header",
            size: th::HEIGHTS_SIZE,
            draw: th::heights,
        },
        SnapshotCase {
            name: "track_headers_renaming",
            module: "panels/timeline/track_header",
            size: th::RENAME_SIZE,
            draw: th::renaming,
        },
        SnapshotCase {
            name: "track_headers_rtl",
            module: "panels/timeline/track_header",
            size: th::DEFAULT_SIZE,
            draw: th::rtl,
        },
        SnapshotCase {
            name: "timeline_ruler_frames",
            module: "panels/timeline/ruler",
            size: tl::RULER_SIZE,
            draw: tl::ruler_frames,
        },
        SnapshotCase {
            name: "timeline_ruler_seconds",
            module: "panels/timeline/ruler",
            size: tl::RULER_SIZE,
            draw: tl::ruler_seconds,
        },
        SnapshotCase {
            name: "timeline_ruler_minutes",
            module: "panels/timeline/ruler",
            size: tl::RULER_SIZE,
            draw: tl::ruler_minutes,
        },
        SnapshotCase {
            name: "timeline_ruler_drop_frame",
            module: "panels/timeline/ruler",
            size: tl::RULER_SIZE,
            draw: tl::ruler_drop_frame,
        },
        SnapshotCase {
            name: "timeline_minimap",
            module: "panels/timeline/minimap",
            size: tl::MINIMAP_SIZE,
            draw: tl::minimap_sample,
        },
        SnapshotCase {
            name: "timeline_minimap_empty",
            module: "panels/timeline/minimap",
            size: tl::MINIMAP_SIZE,
            draw: tl::minimap_empty,
        },
        SnapshotCase {
            name: "timeline_minimap_long",
            module: "panels/timeline/minimap",
            size: tl::MINIMAP_SIZE,
            draw: tl::minimap_long,
        },
        SnapshotCase {
            name: "timeline_view",
            module: "panels/timeline/view",
            size: tl::VIEW_SIZE,
            draw: tl::view,
        },
        SnapshotCase {
            name: "workspace_edit",
            module: "workspace",
            size: ws::WORKSPACE_SIZE,
            draw: ws::edit,
        },
        SnapshotCase {
            name: "workspace_colour",
            module: "workspace",
            size: ws::WORKSPACE_SIZE,
            draw: ws::colour,
        },
        SnapshotCase {
            name: "workspace_audio",
            module: "workspace",
            size: ws::WORKSPACE_SIZE,
            draw: ws::audio,
        },
        SnapshotCase {
            name: "workspace_effects",
            module: "workspace",
            size: ws::WORKSPACE_SIZE,
            draw: ws::effects,
        },
        SnapshotCase {
            name: "preview_paused",
            module: "panels/preview",
            size: ws::PANEL_SIZE,
            draw: ws::preview,
        },
        SnapshotCase {
            name: "preview_empty",
            module: "panels/preview",
            size: ws::PANEL_SIZE,
            draw: ws::preview_empty,
        },
        SnapshotCase {
            name: "inspector_empty",
            module: "panels/inspector",
            size: ws::PANEL_SIZE,
            draw: ws::inspector,
        },
        SnapshotCase {
            name: "placeholder_panel",
            module: "panels/placeholder",
            size: ws::PANEL_SIZE,
            draw: ws::placeholder,
        },
        SnapshotCase {
            name: "timeline_clips_video",
            module: "panels/timeline/clips",
            size: cl::CLIPS_SIZE,
            draw: cl::video,
        },
        SnapshotCase {
            name: "timeline_clips_video_close",
            module: "panels/timeline/clips",
            size: cl::CLIPS_SIZE,
            draw: cl::video_close,
        },
        SnapshotCase {
            name: "timeline_clips_rtl",
            module: "panels/timeline/clips",
            size: cl::CLIPS_SIZE,
            draw: cl::video_rtl,
        },
        SnapshotCase {
            name: "timeline_clips_audio",
            module: "panels/timeline/clips",
            size: cl::CLIPS_SIZE,
            draw: cl::audio,
        },
        SnapshotCase {
            name: "timeline_clips_nested_transitions",
            module: "panels/timeline/clips",
            size: cl::CLIPS_SIZE,
            draw: cl::nested_and_transitions,
        },
        SnapshotCase {
            name: "timeline_clips_blocks",
            module: "panels/timeline/clips",
            size: cl::BLOCKS_SIZE,
            draw: cl::blocks,
        },
        SnapshotCase {
            name: "timeline_clips_loading",
            module: "panels/timeline/clip_media",
            size: cl::CLIPS_SIZE,
            draw: cl::loading,
        },
        SnapshotCase {
            name: "widgets_controls",
            module: "panels/widgets",
            size: vec2(480.0, 200.0),
            draw: widgets_controls,
        },
        SnapshotCase {
            name: "params_inspector_video",
            module: "params/fields",
            size: vec2(320.0, 512.0),
            draw: params_inspector_video,
        },
        SnapshotCase {
            name: "params_every_kind",
            module: "params/fields",
            size: vec2(320.0, 326.0),
            draw: params_every_kind,
        },
        SnapshotCase {
            name: "params_row_states",
            module: "params/paint",
            size: vec2(480.0, 216.0),
            draw: params_row_states,
        },
        SnapshotCase {
            name: "params_typed_maths",
            module: "params/expr",
            size: vec2(320.0, 208.0),
            draw: params_typed_maths,
        },
        SnapshotCase {
            name: "recovery_prompt",
            module: "dialogs/recovery",
            size: vec2(recovery::WIDTH + 32.0, RECOVERY_CASE_HEIGHT),
            draw: recovery_prompt,
        },
        SnapshotCase {
            name: "recovery_confirm_discard",
            module: "dialogs/recovery",
            size: vec2(recovery::WIDTH + 32.0, RECOVERY_CASE_HEIGHT),
            draw: recovery_confirm_discard,
        },
        SnapshotCase {
            name: "recovery_restoring",
            module: "dialogs/recovery",
            size: vec2(recovery::WIDTH + 32.0, RECOVERY_CASE_HEIGHT),
            draw: recovery_restoring,
        },
        SnapshotCase {
            name: "about_main",
            module: "dialogs/about",
            size: vec2(about::WIDTH + 32.0, 528.0),
            draw: about_main,
        },
        SnapshotCase {
            name: "about_third_party",
            module: "dialogs/about",
            size: vec2(about::WIDTH + 32.0, 448.0),
            draw: about_third_party,
        },
        SnapshotCase {
            name: "about_gpl_notice",
            module: "dialogs/about",
            size: vec2(about::WIDTH + 32.0, 440.0),
            draw: about_gpl_notice,
        },
        SnapshotCase {
            name: "relink_found",
            module: "dialogs/relink",
            size: relink_cases::FOUND_SIZE,
            draw: relink_cases::found,
        },
        SnapshotCase {
            name: "relink_searching_no_match",
            module: "dialogs/relink",
            size: relink_cases::STATES_SIZE,
            draw: relink_cases::searching_and_no_match,
        },
        SnapshotCase {
            name: "relink_relinking",
            module: "dialogs/relink",
            size: relink_cases::FOUND_SIZE,
            draw: relink_cases::relinking,
        },
        SnapshotCase {
            name: "shortcuts_editor",
            module: "dialogs/shortcuts",
            size: vec2(shortcuts::WIDTH + 32.0, SHORTCUTS_CASE_HEIGHT),
            draw: shortcuts_editor,
        },
        SnapshotCase {
            name: "shortcuts_editor_recording",
            module: "dialogs/shortcuts",
            size: SHORTCUTS_CROP,
            draw: shortcuts_editor_recording,
        },
    ]
}

/// The editor on the board: one change (Freeze frame on F) and a clash waiting.
const SHORTCUTS_CASE_HEIGHT: f32 = 770.0;
/// The right end of the editor: the preset menu and a key being recorded.
const SHORTCUTS_CROP: egui::Vec2 = vec2(600.0, 400.0);

fn shortcuts_sample() -> (ShortcutsEditor, Keymap) {
    let mut keymap = Keymap::default();
    keymap.set("edit.freeze_frame", vec![Chord::key(egui::Key::F)]);
    (ShortcutsEditor::new(Platform::Mac), keymap)
}

/// `Window / Keyboard shortcuts` as drawn on `Sheet / Keyboard`.
fn shortcuts_editor(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let (mut editor, mut keymap) = shortcuts_sample();
        editor.start_recording(&keymap, "view.snapping", false);
        editor.capture(Chord::key(egui::Key::S));
        editor.keep(&mut keymap);
        editor.select(&keymap, "tool.razor");
        let height = editor.height(&keymap);
        let at = Rect::from_min_size(rect.min + vec2(16.0, 16.0), vec2(shortcuts::WIDTH, height));
        let _ = editor.show(ui, at, &mut keymap);
    });
}

/// The preset menu open over the list while Roll trim records ⌃⌥J.
fn shortcuts_editor_recording(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let (mut editor, mut keymap) = shortcuts_sample();
        editor.start_recording(&keymap, "tool.roll", false);
        editor.capture(Chord::key(egui::Key::J).ctrl().alt());
        editor.set_preset_menu(true);
        let height = editor.height(&keymap);
        let left = rect.max.x - 16.0 - shortcuts::WIDTH;
        let at = Rect::from_min_size(
            pos2(left, rect.min.y + 16.0),
            vec2(shortcuts::WIDTH, height),
        );
        let _ = editor.show(ui, at, &mut keymap);
    });
}

const CLIP_ROLES: [Role; 6] = [
    Role::ClipVideo,
    Role::ClipAudio,
    Role::ClipTitle,
    Role::ClipAdjustment,
    Role::ClipSubtitle,
    Role::ClipOffline,
];

/// The type scale, the egui widgets under the theme's style, state colours and the clip
/// palette: one image per theme that shows most tokens.
fn theme_showcase(ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        let theme = theme::current(ui.ctx());
        let styled = |key: &str| egui::TextStyle::Name(key.into());
        ui.label(RichText::new(t!("app.name")).text_style(styled(theme::DISPLAY)));
        ui.label(RichText::new(t!("sample.showcase.title")).text_style(styled(theme::TITLE)));
        ui.label(t!("sample.showcase.body"));
        ui.label(
            RichText::new(t!("sample.showcase.caption"))
                .text_style(styled(theme::CAPTION))
                .color(theme.color(Role::TextMuted)),
        );
        ui.label(RichText::new(t!("sample.timecode")).monospace());
        ui.separator();
        ui.horizontal(|ui| {
            let _ = ui.button(t!("sample.action.import"));
            let _ = ui.button(t!("sample.action.export"));
            let _ = ui.add_enabled(false, egui::Button::new(t!("sample.action.render")));
            let _ = ui.selectable_label(true, t!("sample.tool.blade"));
            let _ = ui.selectable_label(false, t!("sample.tool.select"));
        });
        ui.horizontal(|ui| {
            let mut snap = true;
            let mut ripple = false;
            ui.checkbox(&mut snap, t!("sample.toggle.snap"));
            ui.checkbox(&mut ripple, t!("sample.toggle.ripple"));
            let mut opacity = 72.0f32;
            ui.add(egui::Slider::new(&mut opacity, 0.0..=100.0).text(t!("sample.param.opacity")));
        });
        ui.horizontal(|ui| {
            let mut name = t!("sample.clip.interview");
            ui.add(egui::TextEdit::singleline(&mut name).desired_width(200.0));
            let _ = ui.link(t!("sample.action.relink"));
        });
        ui.separator();
        ui.horizontal(|ui| {
            for (role, key) in [
                (Role::StateSuccess, t!("sample.status.ready")),
                (Role::StateInfo, t!("sample.status.proxy")),
                (Role::StateWarning, t!("sample.status.rendering")),
                (Role::StateError, t!("sample.status.offline")),
            ] {
                ui.label(RichText::new(key).color(theme.color(role)));
            }
        });
        ui.horizontal(|ui| {
            let swatch = vec2(
                theme.tokens().sizes.control_pro,
                theme.tokens().sizes.control_pro,
            );
            for role in CLIP_ROLES.iter().chain(&[
                Role::Accent,
                Role::AccentSoft,
                Role::Bg0,
                Role::Bg2,
                Role::Bg3,
            ]) {
                let (rect, _) = ui.allocate_exact_size(swatch, Sense::hover());
                ui.painter().rect(
                    rect,
                    theme.tokens().radius.control,
                    theme.color(*role),
                    Stroke::new(1.0, theme.color(Role::BorderSubtle)),
                    StrokeKind::Inside,
                );
            }
        });
    });
}

/// Four tracks of named clips painted from theme roles, with a ruler strip, a selected
/// clip and the playhead: the headless timeline the frame budget tests draw.
fn sample_timeline(ui: &mut egui::Ui) {
    let theme = theme::current(ui.ctx());
    let tokens = theme.tokens();
    egui::CentralPanel::default().show(ui, |ui| {
        let full = ui.available_rect_before_wrap();
        let header_w = 64.0;
        let area = Rect::from_min_max(full.min + vec2(header_w, 0.0), full.max);
        let painter = ui.painter_at(full);
        let track_h = tokens.sizes.row_pro * 1.5;
        let headers = [
            t!("sample.track.v2"),
            t!("sample.track.v1"),
            t!("sample.track.a1"),
            t!("sample.track.a2"),
        ];
        for (track, label) in headers.into_iter().enumerate() {
            let y = full.top() + tokens.sizes.control_pro + (track as f32 + 0.5) * track_h;
            painter.text(
                egui::pos2(full.left() + tokens.spacing.s8, y),
                Align2::LEFT_CENTER,
                label,
                theme.font(&tokens.type_scale.label),
                theme.color(Role::TextSecondary),
            );
        }
        painter.vline(
            area.left() - 1.0,
            full.y_range(),
            Stroke::new(1.0, theme.color(Role::BorderSubtle)),
        );
        let ruler = Rect::from_min_size(area.min, vec2(area.width(), tokens.sizes.control_pro));
        painter.rect_filled(ruler, 0.0, theme.color(Role::Bg2));
        let seconds = 10;
        for s in 0..=seconds {
            let x = ruler.left() + s as f32 / seconds as f32 * ruler.width();
            painter.vline(
                x,
                ruler.bottom() - tokens.spacing.s8..=ruler.bottom(),
                Stroke::new(1.0, theme.color(Role::BorderStrong)),
            );
        }
        let caption = theme.font(&tokens.type_scale.caption);
        painter.text(
            ruler.left_center() + vec2(tokens.spacing.s4, -tokens.spacing.s2),
            Align2::LEFT_CENTER,
            t!("sample.timecode"),
            caption.clone(),
            theme.color(Role::TextMuted),
        );

        let clips: [&[(f32, f32, Role, String)]; 4] = [
            &[(0.35, 0.55, Role::ClipTitle, t!("sample.clip.lower_third"))],
            &[
                (0.0, 0.42, Role::ClipVideo, t!("sample.clip.interview")),
                (0.42, 0.78, Role::ClipVideo, t!("sample.clip.broll")),
                (0.78, 1.0, Role::ClipOffline, t!("sample.clip.missing")),
            ],
            &[(0.0, 0.78, Role::ClipAudio, t!("sample.clip.interview"))],
            &[(0.1, 0.95, Role::ClipAudio, t!("sample.clip.music"))],
        ];
        let selected = (1, 1);
        for (track, row) in clips.iter().enumerate() {
            let top = ruler.bottom() + track as f32 * track_h;
            for (i, (start, end, role, label)) in row.iter().enumerate() {
                let rect = Rect::from_min_max(
                    egui::pos2(area.left() + start * area.width() + 1.0, top + 2.0),
                    egui::pos2(area.left() + end * area.width() - 1.0, top + track_h - 2.0),
                );
                let stroke = if (track, i) == selected {
                    Stroke::new(
                        tokens.stroke.selected_clip_outline,
                        theme.color(Role::Accent),
                    )
                } else {
                    Stroke::new(1.0, theme.color(Role::BorderSubtle))
                };
                painter.rect(
                    rect,
                    tokens.radius.control,
                    theme.color(*role),
                    stroke,
                    StrokeKind::Inside,
                );
                painter.with_clip_rect(rect.shrink(tokens.spacing.s4)).text(
                    rect.left_top() + vec2(tokens.spacing.s4, tokens.spacing.s4),
                    Align2::LEFT_TOP,
                    label,
                    caption.clone(),
                    theme.color(Role::TextInverse),
                );
            }
        }
        let x = area.left() + 0.6 * area.width();
        painter.vline(
            x,
            area.y_range(),
            Stroke::new(2.0, theme.color(Role::Accent)),
        );
    });
}

/// The panel's whole area inside a `CentralPanel` without margins, so painter-drawn
/// panels start at the case's top-left.
/// The recovery dialog with 16 pt of margin, as on the board's stage.
const RECOVERY_CASE_HEIGHT: f32 = 360.0;

/// What the board shows: 38 edits two minutes before the crash, saved at 13:58.
fn recovery_sample() -> RecoveryPrompt {
    RecoveryPrompt {
        project_name: t!("sample.recovery.project"),
        sequence_name: t!("sample.recovery.sequence"),
        sequence_duration: t!("sample.recovery.duration"),
        autosave_time: t!("sample.recovery.autosave_time"),
        saved_time: Some(t!("sample.recovery.saved_time")),
        before_crash_ms: 2 * 60_000 + 5_000,
        edits: 38,
        poster: None,
        timecode: Some(t!("sample.recovery.timecode")),
    }
}

fn recovery_in(ui: &mut egui::Ui, state: RecoveryState) {
    bare_panel(ui, |ui, rect| {
        let mut dialog = RecoveryDialog::new();
        dialog.set_state(state);
        let _ = dialog.draw(ui, rect.min + vec2(16.0, 16.0), &recovery_sample());
    });
}

fn recovery_prompt(ui: &mut egui::Ui) {
    recovery_in(ui, RecoveryState::Prompt);
}

/// After Discard: the footer asks once more.
fn recovery_confirm_discard(ui: &mut egui::Ui) {
    recovery_in(ui, RecoveryState::ConfirmDiscard);
}

/// After Restore: progress in the footer.
fn recovery_restoring(ui: &mut egui::Ui) {
    recovery_in(ui, RecoveryState::Restoring { progress: 0.62 });
}

/// A release build with everything the dialog can show about this copy.
fn about_sample() -> AboutInfo {
    AboutInfo {
        commit: Some("a7f3c1e".into()),
        build_date: Some("2026.09.30".into()),
        machine: Some(t!("sample.about.machine")),
        installed: Some(t!("sample.about.installed")),
        settings_folder: Some(t!("sample.about.settings")),
        ..AboutInfo::new("0.1.0", "https://github.com/OWNER/glass")
    }
}

/// A fixed third-party list, so the goldens do not change with every dependency update.
fn about_components() -> &'static [&'static Component] {
    const fn line(
        kind: Kind,
        name: &'static str,
        version: &'static str,
        licence: &'static str,
        repository: &'static str,
        description: &'static str,
    ) -> Component {
        Component {
            kind,
            name,
            version,
            licence,
            repository,
            description,
        }
    }
    static LINES: [Component; 12] = [
        line(
            Kind::Native,
            "FFmpeg",
            "9.0.2",
            "GPL-3.0-or-later",
            "https://ffmpeg.org",
            "",
        ),
        line(
            Kind::Native,
            "x264",
            "b35605a",
            "GPL-2.0-or-later",
            "https://x264.example",
            "",
        ),
        line(
            Kind::Native,
            "x265",
            "4.2",
            "GPL-2.0-or-later",
            "https://x265.example",
            "",
        ),
        line(
            Kind::Native,
            "dav1d",
            "1.5.4",
            "BSD-2-Clause",
            "https://dav1d.example",
            "",
        ),
        line(
            Kind::Native,
            "nv-codec-headers",
            "13.0.19.1",
            "MIT",
            "https://nv.example",
            "",
        ),
        line(
            Kind::Native,
            "zlib",
            "1.3.2",
            "Zlib",
            "https://zlib.net",
            "",
        ),
        line(
            Kind::Crate,
            "accesskit",
            "0.24.1",
            "MIT OR Apache-2.0",
            "https://a.example",
            "",
        ),
        line(
            Kind::Crate,
            "adler2",
            "2.0.1",
            "0BSD OR MIT OR Apache-2.0",
            "https://b.example",
            "",
        ),
        line(
            Kind::Crate,
            "ahash",
            "0.8.12",
            "MIT OR Apache-2.0",
            "https://c.example",
            "",
        ),
        line(
            Kind::Crate,
            "aho-corasick",
            "1.1.5",
            "Unlicense OR MIT",
            "https://d.example",
            "",
        ),
        line(
            Kind::Crate,
            "allocator-api2",
            "0.2.21",
            "MIT OR Apache-2.0",
            "https://e.example",
            "",
        ),
        line(
            Kind::Crate,
            "android_system_properties",
            "0.1.6",
            "MIT OR Apache-2.0",
            "https://f.example",
            "",
        ),
    ];
    static LIST: OnceLock<Vec<&'static Component>> = OnceLock::new();
    LIST.get_or_init(|| LINES.iter().collect())
}

fn about_dialog() -> AboutDialog {
    AboutDialog::new().with_components(about_components())
}

fn about_in(ui: &mut egui::Ui, dialog: &mut AboutDialog) {
    bare_panel(ui, |ui, rect| {
        let _ = dialog.draw(ui, rect.min + vec2(16.0, 16.0), &about_sample());
    });
}

fn about_main(ui: &mut egui::Ui) {
    about_in(ui, &mut about_dialog());
}

/// View list: the third-party list one level in, FFmpeg selected.
fn about_third_party(ui: &mut egui::Ui) {
    let mut dialog = about_dialog();
    dialog.set_view(View::ThirdParty);
    dialog.select(Some(0));
    about_in(ui, &mut dialog);
}

/// Read on "This build": the GPLv3 notice one level in.
fn about_gpl_notice(ui: &mut egui::Ui) {
    let mut dialog = about_dialog();
    dialog.set_view(View::Licence(Licence::Gpl));
    about_in(ui, &mut dialog);
}

fn bare_panel(ui: &mut egui::Ui, draw: impl FnOnce(&mut egui::Ui, Rect)) {
    let fill = theme::current(ui.ctx()).color(Role::Bg1);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(fill))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            draw(ui, rect);
        });
}

/// An empty bin: the halftone drop target with its Import button.
fn import_drop_empty(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let bin = t!("sample.bin.interviews");
        let _ = import::drop_area(ui, rect.shrink(8.0), &bin, &DropArea::Empty);
    });
}

/// Files over a bin: all readable (left) and some that will not import (right).
fn import_drop_over(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let bin = t!("sample.bin.interviews");
        let half = rect.width() / 2.0;
        let left = Rect::from_min_size(rect.min, vec2(half, rect.height())).shrink(8.0);
        let right =
            Rect::from_min_size(rect.min + vec2(half, 0.0), vec2(half, rect.height())).shrink(8.0);
        let readable = DropSummary {
            video: 8,
            audio: 3,
            still: 1,
            ..DropSummary::default()
        };
        let partial = DropSummary {
            video: 6,
            audio: 2,
            unreadable: vec![(Unreadable::Photoshop, 2), (Unreadable::CameraRaw, 1)],
            ..DropSummary::default()
        };
        let _ = import::drop_area(ui, left, &bin, &DropArea::Over(readable));
        let _ = import::drop_area(ui, right, &bin, &DropArea::Over(partial));
    });
}

/// The folder dialog for a camera card with nested bins, one folder left out and files
/// already in the project.
fn import_folder_dialog(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let row = |depth, name: String, clips, has_children, left_out: bool| FolderRow {
            depth,
            name,
            clips,
            has_children,
            included: !left_out,
            left_out,
        };
        let mut view = FolderImport {
            folder_name: t!("sample.folder.card"),
            from: t!("sample.folder.path"),
            rows: vec![
                row(0, t!("sample.folder.card"), 0, true, false),
                row(1, t!("sample.folder.a_cam"), 14, true, false),
                row(2, t!("sample.folder.a_cam_day1"), 8, false, false),
                row(2, t!("sample.folder.a_cam_day2"), 6, false, false),
                row(1, t!("sample.folder.b_cam"), 9, false, false),
                row(1, t!("sample.folder.sound"), 12, false, false),
                row(1, t!("sample.folder.stills"), 23, false, false),
                row(1, t!("sample.folder.misc"), 0, false, true),
            ],
            keep_folders_as_bins: true,
            skip_existing: true,
            already_in_project: 4,
            split_into_scenes: false,
            new_files: 58,
            bins: 7,
            new_bytes: 171_300_000_000,
            hovered_row: None,
            first_row: 0,
        };
        let _ = import::folder_dialog(ui, rect.min + vec2(16.0, 16.0), &mut view);
    });
}

/// An import part-way through, with a rate to estimate time left from.
fn import_progress_strip(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let progress = ImportProgress {
            source: t!("sample.folder.card"),
            bin: t!("sample.bin.interviews"),
            done: 23,
            total: 58,
            current_file: t!("sample.file.a_cam"),
            current_bytes: 2_100_000_000,
            bytes_done: 64_000_000_000,
            bytes_total: 171_300_000_000,
            elapsed_s: 48.0,
        };
        let strip = Rect::from_min_size(
            rect.min + vec2(0.0, 8.0),
            vec2(rect.width(), PROGRESS_STRIP_HEIGHT),
        );
        let _ = import::progress_strip(ui, strip, &progress);
    });
}

/// The chip that follows the pointer while twelve files are dragged in.
fn import_drag_ghost(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        import::drag_ghost(ui, rect.min, &t!("sample.file.a_cam"), 12);
    });
}

/// The metadata strip for an A-cam clip with every field the board shows.
fn metadata_panel(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let facts = Facts {
            width: 3840,
            height: 2160,
            bit_depth: 10,
            chroma: Some("4:2:2".into()),
            primaries: Some("bt709".into()),
            audio_channels: 2,
            sample_rate: 48_000,
            audio_bit_depth: Some(24),
            ..Facts::default()
        };
        let data = Metadata {
            camera_make: Some("Sony".into()),
            camera_model: Some("ILME-FX6V".into()),
            focal_length_mm: Some(35.0),
            f_number: Some(2.8),
            iso: Some(800),
            reel: Some("A014".into()),
            timecode: Some("19:48:07:11".into()),
            created: Some("2026-08-12T19:48:07+02:00".into()),
            gps: Some(Gps {
                latitude: 60.391234,
                longitude: 5.322345,
                altitude_m: Some(12.0),
            }),
            ..Metadata::default()
        };
        let view = MetadataView::new(
            &t!("sample.clip.harbour"),
            Some(&t!("sample.file.harbour")),
            Some(2_100_000_000),
            &facts,
            &data,
        );
        let _ = metadata::metadata_panel(ui, rect, &view);
    });
}

/// A voice memo: no camera, lens, reel, timecode or position, so those rows show the
/// empty mark.
fn metadata_panel_empty(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let facts = Facts {
            audio_channels: 1,
            sample_rate: 48_000,
            ..Facts::default()
        };
        let data = Metadata {
            created: Some("2026-08-14T08:02:51Z".into()),
            ..Metadata::default()
        };
        let view = MetadataView::new(
            &t!("sample.clip.voice_memo"),
            Some(&t!("sample.file.voice_memo")),
            Some(1_800_000),
            &facts,
            &data,
        );
        let _ = metadata::metadata_panel(ui, rect, &view);
    });
}

/// The system picker has no pixels of its own; this is the narrow-bin drop target whose
/// Import button opens it.
fn file_picker_entry(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let bin = t!("sample.bin.selects");
        let _ = import::drop_area(ui, rect.shrink(8.0), &bin, &DropArea::Empty);
    });
}

/// The media pool's list columns as `panels::format` writes them, and the time-left
/// strings the progress strip uses.
fn format_list_columns(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let paint = Paint::new(ui.ctx());
        let painter = ui.painter_at(rect);
        let columns = [
            (t!("sample.column.name"), 8.0, 160.0),
            (t!("sample.column.duration"), 176.0, 72.0),
            (t!("sample.column.timecode"), 256.0, 88.0),
            (t!("sample.column.fps"), 352.0, 56.0),
            (t!("sample.column.resolution"), 408.0, 48.0),
            (t!("sample.column.size"), 456.0, 60.0),
        ];
        let header_y = rect.min.y + size::ROW / 2.0;
        for (label, x, w) in &columns {
            paint.text(
                &painter,
                pos2(rect.min.x + x, header_y),
                Align2::LEFT_CENTER,
                label,
                Type::Caps,
                Role::TextMuted,
                *w,
            );
        }
        painter.hline(
            rect.x_range(),
            rect.min.y + size::ROW - 0.5,
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        );
        let rows = [
            (
                t!("sample.file.a_cam"),
                754.0 / 23.976,
                24000.0 / 1001.0,
                (3840, 2160),
                2_100_000_000,
            ),
            (
                t!("sample.file.b_cam"),
                3862.0,
                25.0,
                (1920, 1080),
                38_400_000_000,
            ),
            (
                t!("sample.file.drone"),
                31.4,
                60000.0 / 1001.0,
                (1280, 720),
                840_000_000,
            ),
            (
                t!("sample.file.phone"),
                18.0 * 60.0 + 42.0,
                30.0,
                (1080, 1920),
                12_000,
            ),
        ];
        for (i, (name, seconds, fps, (w, h), bytes)) in rows.into_iter().enumerate() {
            let y = rect.min.y + size::ROW * (i as f32 + 1.5);
            let cells = [
                (name, Type::Label, Role::TextPrimary),
                (
                    format::clock(seconds),
                    Type::MonoCaption,
                    Role::TextSecondary,
                ),
                (
                    format::timecode(seconds, Some(fps)),
                    Type::MonoCaption,
                    Role::TextSecondary,
                ),
                (format::fps(fps), Type::MonoCaption, Role::TextSecondary),
                (
                    format::resolution_class(w, h),
                    Type::Caption,
                    Role::TextSecondary,
                ),
                (format::bytes(bytes), Type::MonoCaption, Role::TextSecondary),
            ];
            for ((text, ty, role), (_, x, w)) in cells.into_iter().zip(&columns) {
                paint.text(
                    &painter,
                    pos2(rect.min.x + x, y),
                    Align2::LEFT_CENTER,
                    &text,
                    ty,
                    role,
                    *w,
                );
            }
        }
        let footer = [
            format::time_left(18.0),
            format::time_left(60.0),
            format::time_left(600.0),
        ]
        .join(" · ");
        paint.text(
            &painter,
            pos2(rect.min.x + 8.0, rect.min.y + size::ROW * 5.5),
            Align2::LEFT_CENTER,
            &footer,
            Type::Caption,
            Role::TextMuted,
            rect.width() - 16.0,
        );
    });
}

/// The panels' shared controls: buttons, check, radio and switch in both states, pills,
/// badges, chips, a progress bar and the icon set.
fn widgets_controls(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let paint = Paint::new(ui.ctx());
        let painter = ui.painter_at(rect);
        let x0 = rect.min.x + 16.0;
        let mut y = rect.min.y + 16.0;
        let button = |x: f32, w: f32| Rect::from_min_size(pos2(x, y), vec2(w, size::DIALOG_BUTTON));
        let _ = paint.button(
            ui,
            button(x0, 96.0),
            &t!("sample.action.import"),
            true,
            true,
        );
        let _ = paint.button(
            ui,
            button(x0 + 104.0, 96.0),
            &t!("common.cancel"),
            false,
            true,
        );
        let _ = paint.button(
            ui,
            button(x0 + 208.0, 96.0),
            &t!("sample.action.render"),
            true,
            false,
        );
        let _ = paint.ghost_button(ui, button(x0 + 312.0, 120.0), &t!("sample.action.relink"));
        y += size::DIALOG_BUTTON + 16.0;
        let check = |x: f32| Rect::from_min_size(pos2(x, y), vec2(size::CHECK, size::CHECK));
        paint.checkbox(&painter, check(x0), true);
        paint.checkbox(&painter, check(x0 + 24.0), false);
        paint.radio(&painter, check(x0 + 56.0), true);
        paint.radio(&painter, check(x0 + 80.0), false);
        paint.switch(
            &painter,
            Rect::from_min_size(pos2(x0 + 112.0, y + 1.0), vec2(24.0, 14.0)),
            true,
        );
        paint.switch(
            &painter,
            Rect::from_min_size(pos2(x0 + 144.0, y + 1.0), vec2(24.0, 14.0)),
            false,
        );
        paint.progress(
            &painter,
            Rect::from_min_size(pos2(x0 + 184.0, y + 6.0), vec2(160.0, 4.0)),
            0.37,
            Role::Bg3,
        );
        y += size::CHECK + 20.0;
        let center_y = y + size::SMALL_CONTROL / 2.0;
        let pill = paint.status_pill(
            &painter,
            pos2(x0, center_y),
            &t!("sample.status.offline"),
            Role::StateError,
        );
        let pill = paint.status_pill(
            &painter,
            pos2(pill.max.x + 8.0, center_y),
            &t!("sample.status.proxy"),
            Role::StateInfo,
        );
        let badge = paint.badge(
            &painter,
            pos2(pill.max.x + 8.0, center_y),
            Align2::LEFT_CENTER,
            &t!("format.resolution.uhd"),
            Type::MonoCaption,
        );
        let badge = paint.badge(
            &painter,
            pos2(badge.max.x + 8.0, center_y),
            Align2::LEFT_CENTER,
            &format::clock(754.0 / 23.976),
            Type::MonoCaption,
        );
        let _ = paint.chip(
            &painter,
            pos2(badge.max.x + 8.0, center_y),
            &t!("sample.clip.interview"),
            120.0,
        );
        y += size::SMALL_CONTROL + 20.0;
        let icons = [
            Icon::Search,
            Icon::Close,
            Icon::Check,
            Icon::Folder,
            Icon::List,
            Icon::Grid,
            Icon::ChevronDown,
            Icon::ChevronRight,
            Icon::Plus,
            Icon::Filter,
            Icon::WifiOff,
            Icon::Alert,
            Icon::Info,
            Icon::Film,
            Icon::Music,
            Icon::Image,
            Icon::Import,
            Icon::More,
        ];
        for (i, which) in icons.into_iter().enumerate() {
            let cell = Rect::from_min_size(pos2(x0 + i as f32 * 24.0, y), vec2(16.0, 16.0));
            icon(&painter, cell, which, paint.c(Role::TextSecondary), 1.8);
        }
    });
}

/// Sections stacked top to bottom at the case's width, like the inspector column.
fn param_sections(ui: &mut egui::Ui, draw: impl FnOnce(&mut egui::Ui)) {
    let fill = theme::current(ui.ctx()).color(Role::Bg1);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(fill))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            draw(ui);
        });
}

/// `Sheet / Inspector video`, default column: Position between keyframes, Scale with a
/// keyframe at the playhead, Opacity animated, crop and motion blur switched on.
fn params_inspector_video(ui: &mut egui::Ui) {
    param_sections(ui, |ui| {
        let transform = builtin::transform();
        let mut t_values = ParamValues::new();
        t_values.set(&transform, "position", ParamValue::Point([1012.4, 548.0]));
        t_values.set(&transform, "scale", ParamValue::Float(1.12));
        let keys = |id: &str| match id {
            "position" => KeyframeInfo {
                mark: KeyframeMark::On,
                previous: true,
                next: true,
            },
            "scale" => KeyframeInfo {
                mark: KeyframeMark::AtPlayhead,
                previous: true,
                next: true,
            },
            "opacity" => KeyframeInfo {
                mark: KeyframeMark::On,
                previous: true,
                next: false,
            },
            _ => KeyframeInfo::default(),
        };
        let _ = Section::new(&transform, &t_values)
            .keyframes(&keys)
            .show(ui);

        let compositing = builtin::compositing();
        let mut c_values = ParamValues::new();
        c_values.set(&compositing, "opacity", ParamValue::Float(0.86));
        let _ = Section::new(&compositing, &c_values)
            .keyframes(&keys)
            .show(ui);

        let crop = builtin::crop();
        let mut crop_values = ParamValues::new();
        crop_values.set(&crop, "top", ParamValue::Float(140.0));
        crop_values.set(&crop, "bottom", ParamValue::Float(140.0));
        crop_values.set(&crop, "feather", ParamValue::Float(12.0));
        let _ = Section::new(&crop, &crop_values).show(ui);

        let blur = builtin::motion_blur();
        let mut b_values = ParamValues::new();
        b_values.set(&blur, "enabled", ParamValue::Bool(true));
        b_values.set(&blur, "samples", ParamValue::Int(16));
        let _ = Section::new(&blur, &b_values).show(ui);
    });
}

/// A made-up effect with one parameter of every kind the schema has, then the same
/// effect bypassed.
fn params_every_kind(ui: &mut egui::Ui) {
    param_sections(ui, |ui| {
        let caption = t!("sample.clip.lower_third");
        let glow = ParamSchema::new(
            "glow",
            "sample.effect.glow",
            vec![
                ParamDef::colour("colour", "sample.param.colour", [1.0, 0.62, 0.2, 1.0], true),
                ParamDef::text("caption", "sample.param.caption", &caption, false),
                ParamDef::boolean("invert", "sample.param.invert", false),
                ParamDef::choice(
                    "falloff",
                    "sample.param.falloff",
                    &[
                        ("smooth", "sample.param.falloff_smooth"),
                        ("linear", "sample.param.falloff_linear"),
                    ],
                ),
                ParamDef::int("copies", "sample.param.copies", 3, Range::new(1.0, 12.0)),
                ParamDef::angle("angle", "sample.param.angle", 0.0),
                ParamDef::float(
                    "radius",
                    "sample.param.radius",
                    24.0,
                    Range::new(0.0, 500.0),
                )
                .soft(Range::new(0.0, 100.0))
                .unit(Unit::Pixels)
                .decimals(0)
                .slider(),
            ],
        );
        let mut values = ParamValues::new();
        values.set(&glow, "invert", ParamValue::Bool(true));
        values.set(&glow, "angle", ParamValue::Angle(-12.5));
        values.set(&glow, "radius", ParamValue::Float(36.0));
        let _ = Section::new(&glow, &values).show(ui);

        let vignette = ParamSchema::new(
            "vignette",
            "sample.effect.vignette",
            vec![
                ParamDef::point("centre", "sample.param.centre", [960.0, 540.0]),
                ParamDef::float("amount", "sample.param.amount", 0.4, Range::new(0.0, 1.0))
                    .unit(Unit::Percent)
                    .decimals(0)
                    .slider(),
            ],
        );
        let _ = Section::new(&vignette, &vignette.defaults())
            .enabled(false)
            .show(ui);
    });
}

/// Every state of the row parts, painted directly: keyframe lanes (off, hover, on with
/// only an earlier keyframe, at the playhead, between keyframes), numeric fields (idle,
/// hover, scrubbing with its delta chip, editing, error with its helper), sliders,
/// switches, dropdowns and the stepper.
fn params_row_states(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let paint = Paint::new(ui.ctx());
        let painter = ui.painter_at(rect);
        let x0 = rect.min.x + 16.0;
        let col = 92.0;
        let lanes = [
            (KeyframeMark::Off, false, false, false),
            (KeyframeMark::Off, false, false, true),
            (KeyframeMark::On, true, false, false),
            (KeyframeMark::AtPlayhead, true, true, false),
            (KeyframeMark::On, true, true, false),
        ];
        let y = rect.min.y + 12.0;
        for (i, (mark, previous, next, hover)) in lanes.into_iter().enumerate() {
            let lane = Rect::from_min_size(pos2(x0 + i as f32 * col, y), vec2(40.0, 16.0));
            let info = KeyframeInfo {
                mark,
                previous,
                next,
            };
            param_paint::keyframe_lane(&paint, &painter, lane, info, hover);
        }

        let y = rect.min.y + 64.0;
        let states = [
            FieldState::Idle,
            FieldState::Hover,
            FieldState::Scrub,
            FieldState::Editing,
            FieldState::Error,
        ];
        for (i, state) in states.into_iter().enumerate() {
            let field = Rect::from_min_size(pos2(x0 + i as f32 * col, y), vec2(76.0, 24.0));
            param_paint::field_frame(&paint, &painter, field, state);
            let value = match state {
                FieldState::Scrub => "74",
                FieldState::Error => "140",
                _ => "86",
            };
            param_paint::field_text(&paint, &painter, field, "%", value);
            if state == FieldState::Scrub {
                param_paint::delta_chip(&paint, &painter, field, field.center().x, "\u{2212}12");
            }
            if state == FieldState::Error {
                let text = t!("params.error.range", min = 0, max = 100);
                param_paint::error_helper(
                    &paint,
                    &painter,
                    pos2(field.left() - 16.0, field.bottom() + 12.0),
                    &text,
                );
            }
        }

        let y = rect.min.y + 120.0;
        let sliders = [
            (0.6, SliderState::Idle),
            (0.6, SliderState::Hover),
            (0.75, SliderState::Drag),
        ];
        for (i, (f, state)) in sliders.into_iter().enumerate() {
            let slider = Rect::from_min_size(pos2(x0 + i as f32 * col, y), vec2(76.0, 24.0));
            param_paint::slider(&paint, &painter, slider, f, state);
        }
        for (i, (on, hover)) in [(false, false), (false, true), (true, false)]
            .into_iter()
            .enumerate()
        {
            let switch = Rect::from_min_size(
                pos2(x0 + (3 + i) as f32 * col * 0.5 + 3.0 * col * 0.5, y + 5.0),
                vec2(24.0, 14.0),
            );
            param_paint::switch(&paint, &painter, switch, on, hover);
        }

        let y = rect.min.y + 168.0;
        let normal = t!("blend.normal");
        let screen = t!("blend.screen");
        let dropdowns = [
            (&normal, false, false),
            (&screen, true, false),
            (&screen, false, true),
        ];
        for (i, (text, hover, open)) in dropdowns.into_iter().enumerate() {
            let field = Rect::from_min_size(pos2(x0 + i as f32 * col, y), vec2(84.0, 24.0));
            param_paint::dropdown(&paint, &painter, field, text, hover, open);
        }
        let stepper = Rect::from_min_size(
            pos2(x0 + 3.0 * col, y),
            vec2(param_paint::STEPPER_WIDTH, 24.0),
        );
        param_paint::stepper(
            &paint,
            &painter,
            stepper,
            "16",
            StepperHover::Plus,
            FieldState::Idle,
        );
    });
}

/// Maths typed into a field and what it commits to: plain numbers, sums, relative
/// entries applied to the current value, products, parentheses and the typographic
/// minus; division by zero commits nothing.
fn params_typed_maths(ui: &mut egui::Ui) {
    bare_panel(ui, |ui, rect| {
        let paint = Paint::new(ui.ctx());
        let painter = ui.painter_at(rect);
        let current = 112.0;
        let entries = [
            "112",
            "112+10",
            "+10",
            "*2",
            "(1+2)*3 - 1",
            "\u{2212}12",
            "1/0",
        ];
        for (i, typed) in entries.into_iter().enumerate() {
            let y = rect.min.y + 12.0 + i as f32 * 28.0;
            let left = Rect::from_min_size(pos2(rect.min.x + 16.0, y), vec2(136.0, 24.0));
            let right = Rect::from_min_size(pos2(rect.min.x + 168.0, y), vec2(136.0, 24.0));
            param_paint::field_frame(&paint, &painter, left, FieldState::Editing);
            param_paint::field_text(&paint, &painter, left, "%", typed);
            match crate::params::evaluate(typed, current) {
                Some(v) => {
                    param_paint::field_frame(&paint, &painter, right, FieldState::Idle);
                    param_paint::field_text(&paint, &painter, right, "%", &format!("{v:.1}"));
                }
                None => {
                    param_paint::field_frame(&paint, &painter, right, FieldState::Error);
                    param_paint::field_text(&paint, &painter, right, "%", &format!("{current:.1}"));
                }
            }
        }
    });
}
