//! The registered cases, one explicit list. A panel, dialog or parameter widget adds a
//! line to [`all`] pointing at a draw function with realistic sample content (its own
//! `snapshot_cases()` or a function here that calls its public API).

use egui::{Align2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use engine::media_mgmt::{DropSummary, Unreadable};
use project::media_pool::{Facts, Gps, Metadata};

use super::{SnapshotCase, pool_cases};
use crate::dialogs::recovery::{self, RecoveryDialog, RecoveryPrompt, RecoveryState};
use crate::panels::format;
use crate::panels::import::{
    self, DropArea, FOLDER_DIALOG_WIDTH, FolderImport, FolderRow, ImportProgress,
    PROGRESS_STRIP_HEIGHT,
};
use crate::panels::metadata::{self, METADATA_PANEL_HEIGHT, MetadataView};
use crate::panels::widgets::{Icon, Paint, Type, icon, size};
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
            name: "widgets_controls",
            module: "panels/widgets",
            size: vec2(480.0, 200.0),
            draw: widgets_controls,
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
    ]
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
