//! Import surfaces from `Sheet / Import` (page `02 Edit workspace`): the drop target a
//! bin shows (empty, files dragged over, some unreadable), the folder import dialog and
//! the progress strip the media pool shows while an import runs. Each is a view model
//! plus a function that draws it; nothing here touches the disk. The scan and the probe
//! run on `engine::media_mgmt::Importer`'s thread.

use std::path::PathBuf;

use egui::{Align2, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use engine::media_mgmt::{DropSummary, ImportPlan, Unreadable};

use super::format;
use super::widgets::{Icon, Paint, Type, icon, size};
use crate::t;
use crate::theme::Role;

/// Files being dragged over the window and files dropped this frame, from egui's input
/// (paths only; nothing is opened on the UI thread).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDrag {
    pub hovered: Vec<PathBuf>,
    pub dropped: Vec<PathBuf>,
}

impl FileDrag {
    pub fn read(ctx: &egui::Context) -> Self {
        ctx.input(|i| Self {
            hovered: i
                .raw
                .hovered_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect(),
            dropped: i
                .raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect(),
        })
    }

    pub fn is_dragging(&self) -> bool {
        !self.hovered.is_empty()
    }
}

/// What a bin's drop area shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropArea {
    /// Nothing in the bin and nothing dragged over it.
    Empty,
    /// Files are over the bin.
    Over(DropSummary),
}

impl DropArea {
    pub fn from_drag(drag: &FileDrag) -> Self {
        if drag.is_dragging() {
            Self::Over(DropSummary::of(&drag.hovered))
        } else {
            Self::Empty
        }
    }
}

/// `8 video · 3 audio · 1 still`.
pub fn kinds_line(summary: &DropSummary) -> String {
    let mut parts = Vec::new();
    if summary.video > 0 {
        parts.push(t!("import.kind.video", count = summary.video));
    }
    if summary.audio > 0 {
        parts.push(t!("import.kind.audio", count = summary.audio));
    }
    match summary.still {
        0 => {}
        1 => parts.push(t!("import.kind.still_one")),
        n => parts.push(t!("import.kind.still", count = n)),
    }
    match summary.folders {
        0 => {}
        1 => parts.push(t!("import.kind.folder_one")),
        n => parts.push(t!("import.kind.folder", count = n)),
    }
    parts.join(" · ")
}

/// Why some dropped files will not import, in the design's voice.
pub fn unreadable_message(kind: Unreadable, count: usize) -> String {
    match (kind, count) {
        (Unreadable::Photoshop, 1) => t!("import.unreadable.photoshop_one"),
        (Unreadable::Photoshop, n) => t!("import.unreadable.photoshop", count = n),
        (Unreadable::CameraRaw, 1) => t!("import.unreadable.raw_one"),
        (Unreadable::CameraRaw, n) => t!("import.unreadable.raw", count = n),
        (Unreadable::Document, 1) => t!("import.unreadable.document_one"),
        (Unreadable::Document, n) => t!("import.unreadable.document", count = n),
    }
}

/// What the user did in the drop area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropAreaAction {
    None,
    OpenFilePicker,
}

/// Draws a bin's drop area into `rect` (the area under the bin header).
pub fn drop_area(ui: &mut Ui, rect: Rect, bin: &str, state: &DropArea) -> DropAreaAction {
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    let corner = paint.control_radius();
    let mut action = DropAreaAction::None;
    match state {
        DropArea::Empty => {
            painter.rect_filled(rect, corner, paint.c(Role::Bg2));
            paint.halftone(&painter, rect, 5.0, paint.c(Role::Bg0));
            // Icon, two lines of text and the button, 12 apart, centred.
            let title = t!("import.drop.title");
            let detail = t!("import.drop.empty_bin");
            let block_h = 20.0 + 12.0 + 16.0 + 2.0 + 14.0 + 12.0 + 24.0;
            let mut y = rect.center().y - block_h / 2.0;
            icon(
                &painter,
                Rect::from_center_size(pos2(rect.center().x, y + 10.0), vec2(20.0, 20.0)),
                Icon::Import,
                paint.c(Role::TextSecondary),
                1.8,
            );
            y += 32.0;
            paint.text(
                &painter,
                pos2(rect.center().x, y),
                Align2::CENTER_TOP,
                &title,
                Type::Label,
                Role::TextPrimary,
                rect.width() - 16.0,
            );
            y += 18.0;
            paint.text(
                &painter,
                pos2(rect.center().x, y),
                Align2::CENTER_TOP,
                &detail,
                Type::Caption,
                Role::TextMuted,
                rect.width() - 16.0,
            );
            y += 26.0;
            let label = t!("import.drop.button");
            let key = t!("shortcut.import");
            let label_w = paint.width(&painter, &label, Type::LabelMedium);
            let key_w = paint.width(&painter, &key, Type::MonoCaption) + 8.0;
            let button = Rect::from_center_size(
                pos2(rect.center().x, y + 12.0),
                vec2(8.0 + label_w + 8.0 + key_w + 4.0, 24.0),
            );
            let response = ui.interact(
                button,
                ui.id().with(("import.drop.button", bin)),
                Sense::click(),
            );
            painter.rect_filled(
                button,
                corner,
                paint.c(if response.hovered() {
                    Role::Bg3
                } else {
                    Role::Bg2
                }),
            );
            paint.glass_outline(&painter, button, corner);
            paint.text(
                &painter,
                pos2(button.min.x + 8.0, button.center().y),
                Align2::LEFT_CENTER,
                &label,
                Type::LabelMedium,
                Role::TextPrimary,
                label_w + 1.0,
            );
            let key_rect = Rect::from_min_size(
                pos2(button.max.x - 4.0 - key_w, button.center().y - 8.0),
                vec2(key_w, 16.0),
            );
            painter.rect_filled(key_rect, corner, paint.c(Role::Bg3));
            paint.text(
                &painter,
                key_rect.center(),
                Align2::CENTER_CENTER,
                &key,
                Type::MonoCaption,
                Role::TextSecondary,
                key_w,
            );
            if response.clicked() {
                action = DropAreaAction::OpenFilePicker;
            }
        }
        DropArea::Over(summary) => {
            let unreadable = summary.unreadable_total();
            let border = if unreadable > 0 {
                Role::StateWarning
            } else {
                Role::Accent
            };
            painter.rect_filled(rect, corner, paint.c(Role::Bg3));
            painter.rect_stroke(
                rect,
                corner,
                Stroke::new(1.5, paint.c(border)),
                StrokeKind::Inside,
            );
            let (title, detail, detail_ty) = if unreadable > 0 {
                let title = t!(
                    "import.drop.partial",
                    count = summary.importable(),
                    total = summary.total()
                );
                let detail = summary
                    .unreadable
                    .iter()
                    .map(|(kind, n)| unreadable_message(*kind, *n))
                    .collect::<Vec<_>>()
                    .join(" ");
                (title, detail, Type::Caption)
            } else {
                let title = match summary.importable() {
                    1 => t!("import.drop.over_one", bin = bin),
                    n => t!("import.drop.over", count = n, bin = bin),
                };
                (title, kinds_line(summary), Type::MonoCaption)
            };
            let width = rect.width() - 64.0;
            let detail_galley = paint.wrapped(
                &painter,
                &detail,
                detail_ty,
                paint.c(Role::TextSecondary),
                width,
                true,
            );
            let block_h = 16.0 + 2.0 + detail_galley.size().y;
            let top = rect.center().y - block_h / 2.0;
            paint.text(
                &painter,
                pos2(rect.center().x, top),
                Align2::CENTER_TOP,
                &title,
                Type::LabelMedium,
                Role::TextPrimary,
                width,
            );
            painter.galley(
                pos2(rect.center().x - width / 2.0, top + 18.0),
                detail_galley,
                paint.c(Role::TextSecondary),
            );
        }
    }
    action
}

/// The floating chip that follows the pointer while files are dragged in: the first
/// file's name and how many there are.
pub fn drag_ghost(ui: &Ui, at: Pos2, first_name: &str, count: usize) {
    let paint = Paint::new(ui.ctx());
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        ui.id().with("import.ghost"),
    ));
    let name_w = paint.width(&painter, first_name, Type::Label).min(200.0);
    let count_text = count.to_string();
    let badge_w = paint.width(&painter, &count_text, Type::MonoCaption) + 10.0;
    let rect = Rect::from_min_size(
        at + vec2(12.0, 12.0),
        vec2(4.0 + 40.0 + 8.0 + name_w + 8.0 + badge_w + 8.0, 32.0),
    );
    paint.floating(&painter, rect);
    let thumb = Rect::from_min_size(
        pos2(rect.min.x + 4.0, rect.center().y - 11.0),
        vec2(40.0, 22.0),
    );
    painter.rect_filled(
        thumb,
        paint.control_radius(),
        paint.mix(Role::ClipVideo, 0.55),
    );
    paint.text(
        &painter,
        pos2(thumb.max.x + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        first_name,
        Type::Label,
        Role::TextPrimary,
        name_w,
    );
    let badge = Rect::from_min_size(
        pos2(rect.max.x - 8.0 - badge_w, rect.center().y - 8.0),
        vec2(badge_w, 16.0),
    );
    painter.rect_filled(badge, egui::CornerRadius::same(255), paint.c(Role::Accent));
    paint.text(
        &painter,
        badge.center(),
        Align2::CENTER_CENTER,
        &count_text,
        Type::MonoCaption,
        Role::TextInverse,
        badge_w,
    );
}

/// One row of the folder dialog's "Bins to create" tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderRow {
    pub depth: usize,
    pub name: String,
    pub clips: usize,
    pub has_children: bool,
    pub included: bool,
    /// No video, audio or pictures: shown dimmed and never imported.
    pub left_out: bool,
}

/// The folder import dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct FolderImport {
    pub folder_name: String,
    pub from: String,
    pub rows: Vec<FolderRow>,
    pub keep_folders_as_bins: bool,
    pub skip_existing: bool,
    pub already_in_project: usize,
    pub split_into_scenes: bool,
    pub new_files: usize,
    pub bins: usize,
    pub new_bytes: u64,
    pub hovered_row: Option<usize>,
    /// First row shown when the tree is taller than [`FOLDER_TREE_ROWS`].
    pub first_row: usize,
}

impl FolderImport {
    /// The dialog for a scanned folder (the plan's first root).
    pub fn from_plan(plan: &ImportPlan) -> Self {
        let root = plan.roots.first().cloned().unwrap_or_default();
        let folder_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rows: Vec<FolderRow> = plan
            .folders
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                // A left-out folder is listed once; its (equally empty) subfolders are not.
                let parent = &f.bin[..f.bin.len().saturating_sub(1)];
                !plan.folders.iter().any(|p| p.bin == parent && p.left_out())
            })
            .map(|(i, f)| FolderRow {
                depth: f.depth(),
                name: f.bin.last().cloned().unwrap_or_default(),
                clips: f.new_clips,
                has_children: plan.folders.get(i + 1).is_some_and(|next| {
                    next.bin.len() > f.bin.len() && next.bin.starts_with(&f.bin)
                }),
                included: !f.left_out(),
                left_out: f.left_out(),
            })
            .collect();
        Self {
            folder_name,
            from: root.display().to_string(),
            rows,
            keep_folders_as_bins: true,
            skip_existing: true,
            already_in_project: plan.already_in_project(),
            split_into_scenes: false,
            new_files: plan.new_count(),
            bins: plan.bins(),
            new_bytes: plan.new_bytes(),
            hovered_row: None,
            first_row: 0,
        }
    }
}

/// What the user did in the folder dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderImportAction {
    None,
    ChangeFolder,
    RenameBin(usize),
    Cancel,
    Import,
}

/// Dialog width from the board.
pub const FOLDER_DIALOG_WIDTH: f32 = 560.0;

/// Rows the bins tree shows before it scrolls (the board shows eight).
pub const FOLDER_TREE_ROWS: usize = 8;

/// Draws the folder import dialog with its top-left at `origin`; returns the action and
/// the dialog's rectangle.
pub fn folder_dialog(
    ui: &mut Ui,
    origin: Pos2,
    view: &mut FolderImport,
) -> (FolderImportAction, Rect) {
    let paint = Paint::new(ui.ctx());
    let s12 = paint.s().s12;
    let s16 = paint.s().s16;
    let visible_rows = view.rows.len().min(FOLDER_TREE_ROWS);
    let rows_h = visible_rows as f32 * size::ROW + 8.0;
    let body_h =
        s16 + 32.0 + s16 + (14.0 + 8.0 + rows_h) + s16 + (16.0 + s12 + 16.0 + s12 + 32.0) + s16;
    let rect = Rect::from_min_size(
        origin,
        vec2(
            FOLDER_DIALOG_WIDTH,
            size::DIALOG_HEADER + body_h + size::DIALOG_FOOTER,
        ),
    );
    let painter = ui.painter().clone();
    paint.floating(&painter, rect);
    let mut action = FolderImportAction::None;

    // Header.
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), size::DIALOG_HEADER));
    painter.hline(
        header.x_range(),
        header.max.y - 0.5,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
    let title = t!("import.folder.title", name = view.folder_name);
    paint.text(
        &painter,
        pos2(header.min.x + s16, header.center().y),
        Align2::LEFT_CENTER,
        &title,
        Type::BodyMedium,
        Role::TextPrimary,
        header.width() - 64.0,
    );
    let close = Rect::from_center_size(
        pos2(header.max.x - 8.0 - 12.0, header.center().y),
        vec2(24.0, 24.0),
    );
    if super::widgets::icon_button(&paint, ui, close, Icon::Close, false).clicked() {
        action = FolderImportAction::Cancel;
    }

    // From.
    let mut y = header.max.y + s16;
    let x0 = rect.min.x + s16;
    let x1 = rect.max.x - s16;
    paint.text(
        &painter,
        pos2(x0, y + 16.0),
        Align2::LEFT_CENTER,
        &t!("import.folder.from"),
        Type::Label,
        Role::TextSecondary,
        64.0,
    );
    let change = t!("import.folder.change");
    let change_w = paint.width(&painter, &change, Type::Label) + 24.0;
    let field = Rect::from_min_max(
        pos2(x0 + 64.0 + 8.0, y),
        pos2(x1 - change_w - 8.0, y + 32.0),
    );
    painter.rect_filled(field, paint.control_radius(), paint.c(Role::Bg2));
    painter.rect_stroke(
        field,
        paint.control_radius(),
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        StrokeKind::Inside,
    );
    paint.text(
        &painter,
        pos2(field.min.x + 8.0, field.center().y),
        Align2::LEFT_CENTER,
        &view.from,
        Type::Mono,
        Role::TextPrimary,
        field.width() - 16.0,
    );
    if paint
        .ghost_button(
            ui,
            Rect::from_min_size(pos2(x1 - change_w, y), vec2(change_w, 32.0)),
            &change,
        )
        .clicked()
    {
        action = FolderImportAction::ChangeFolder;
    }
    y += 32.0 + s16;

    // Bins to create.
    paint.text(
        &painter,
        pos2(x0, y),
        Align2::LEFT_TOP,
        &t!("import.folder.bins"),
        Type::Caps,
        Role::TextMuted,
        300.0,
    );
    paint.text(
        &painter,
        pos2(x1, y),
        Align2::RIGHT_TOP,
        &t!("import.folder.files"),
        Type::MonoCaption,
        Role::TextMuted,
        100.0,
    );
    y += 14.0 + 8.0;
    let list = Rect::from_min_max(pos2(x0, y), pos2(x1, y + rows_h));
    painter.rect_filled(list, paint.control_radius(), paint.c(Role::Bg1));
    painter.rect_stroke(
        list,
        paint.control_radius(),
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        StrokeKind::Inside,
    );
    let max_first = view.rows.len() - visible_rows;
    if ui.rect_contains_pointer(list) {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 {
            let step = (-scroll / size::ROW).round() as isize;
            view.first_row = (view.first_row as isize + step).clamp(0, max_first as isize) as usize;
        }
    }
    view.first_row = view.first_row.min(max_first);
    let mut row_y = list.min.y + 4.0;
    view.hovered_row = None;
    let disabled = paint.theme.tokens().opacity.disabled;
    let first = view.first_row;
    for (i, row) in view
        .rows
        .iter_mut()
        .enumerate()
        .skip(first)
        .take(visible_rows)
    {
        let row_rect = Rect::from_min_size(pos2(list.min.x, row_y), vec2(list.width(), size::ROW));
        let response = ui.interact(
            row_rect,
            ui.id().with(("import.folder.row", i)),
            Sense::click(),
        );
        if response.hovered() && !row.left_out {
            painter.rect_filled(row_rect, 0.0, paint.c(Role::Bg3));
            view.hovered_row = Some(i);
        }
        if response.clicked() && !row.left_out {
            row.included = !row.included;
        }
        let fade = |c: egui::Color32| {
            if row.left_out {
                c.gamma_multiply(disabled)
            } else {
                c
            }
        };
        // Indent: 8, 28 and then 20 per level (a chevron slot at each level with children).
        let indent = 8.0 + row.depth as f32 * 20.0 + if row.has_children { 0.0 } else { 20.0 };
        let mut x = row_rect.min.x + indent;
        let check = Rect::from_min_size(
            pos2(x, row_rect.center().y - 8.0),
            vec2(size::CHECK, size::CHECK),
        );
        paint.checkbox(&painter, check, row.included && !row.left_out);
        x += 16.0 + 8.0;
        if row.has_children {
            icon(
                &painter,
                Rect::from_min_size(pos2(x, row_rect.center().y - 6.0), vec2(12.0, 12.0)),
                Icon::ChevronDown,
                fade(paint.c(Role::TextMuted)),
                2.4,
            );
            x += 12.0 + 8.0;
        }
        icon(
            &painter,
            Rect::from_min_size(pos2(x, row_rect.center().y - 7.0), vec2(14.0, 14.0)),
            Icon::Folder,
            fade(paint.c(Role::TextSecondary)),
            2.4,
        );
        x += 14.0 + 8.0;
        let name_role = if row.left_out {
            Role::TextSecondary
        } else {
            Role::TextPrimary
        };
        let count_x = row_rect.max.x - 8.0;
        let mut name_max = count_x - 48.0 - 8.0 - x;
        if row.left_out {
            let note = t!("import.folder.left_out");
            let r = paint.text_c(
                &painter,
                pos2(count_x - 48.0 - 8.0, row_rect.center().y),
                Align2::RIGHT_CENTER,
                &note,
                Type::Caption,
                fade(paint.c(Role::TextSecondary)),
                220.0,
            );
            name_max = r.min.x - 8.0 - x;
        } else if view.hovered_row == Some(i) {
            let rename = t!("import.folder.rename_bin");
            let r = paint.text(
                &painter,
                pos2(count_x - 48.0 - 8.0, row_rect.center().y),
                Align2::RIGHT_CENTER,
                &rename,
                Type::Caption,
                Role::TextSecondary,
                160.0,
            );
            name_max = r.min.x - 8.0 - x;
            if response.secondary_clicked() || response.double_clicked() {
                action = FolderImportAction::RenameBin(i);
            }
        }
        paint.text_c(
            &painter,
            pos2(x, row_rect.center().y),
            Align2::LEFT_CENTER,
            &row.name,
            Type::Label,
            fade(paint.c(name_role)),
            name_max,
        );
        paint.text_c(
            &painter,
            pos2(count_x, row_rect.center().y),
            Align2::RIGHT_CENTER,
            &row.clips.to_string(),
            Type::MonoCaption,
            fade(paint.c(Role::TextMuted)),
            48.0,
        );
        row_y += size::ROW;
    }
    y = list.max.y + s16;

    // Options.
    let switch = Rect::from_min_size(pos2(x0, y + 1.0), vec2(24.0, 14.0));
    let keep = ui.interact(
        Rect::from_min_size(pos2(x0, y), vec2(360.0, 16.0)),
        ui.id().with("import.folder.keep"),
        Sense::click(),
    );
    if keep.clicked() {
        view.keep_folders_as_bins = !view.keep_folders_as_bins;
    }
    paint.switch(&painter, switch, view.keep_folders_as_bins);
    paint.text(
        &painter,
        pos2(x0 + 32.0, y + 8.0),
        Align2::LEFT_CENTER,
        &t!("import.folder.keep_structure"),
        Type::Label,
        Role::TextPrimary,
        400.0,
    );
    y += 16.0 + s12;
    let skip_text = match view.already_in_project {
        1 => t!("import.folder.skip_existing_one"),
        n => t!("import.folder.skip_existing", count = n),
    };
    let skip = ui.interact(
        Rect::from_min_size(pos2(x0, y), vec2(400.0, 16.0)),
        ui.id().with("import.folder.skip"),
        Sense::click(),
    );
    if skip.clicked() {
        view.skip_existing = !view.skip_existing;
    }
    paint.checkbox(
        &painter,
        Rect::from_min_size(pos2(x0, y), vec2(16.0, 16.0)),
        view.skip_existing,
    );
    paint.text(
        &painter,
        pos2(x0 + 24.0, y + 8.0),
        Align2::LEFT_CENTER,
        &skip_text,
        Type::Label,
        Role::TextPrimary,
        400.0,
    );
    y += 16.0 + s12;
    let split = ui.interact(
        Rect::from_min_size(pos2(x0, y), vec2(400.0, 32.0)),
        ui.id().with("import.folder.split"),
        Sense::click(),
    );
    if split.clicked() {
        view.split_into_scenes = !view.split_into_scenes;
    }
    paint.checkbox(
        &painter,
        Rect::from_min_size(pos2(x0, y), vec2(16.0, 16.0)),
        view.split_into_scenes,
    );
    paint.text(
        &painter,
        pos2(x0 + 24.0, y),
        Align2::LEFT_TOP,
        &t!("import.folder.split_scenes"),
        Type::Label,
        Role::TextPrimary,
        400.0,
    );
    paint.text(
        &painter,
        pos2(x0 + 24.0, y + 18.0),
        Align2::LEFT_TOP,
        &t!("import.folder.split_scenes_detail"),
        Type::Caption,
        Role::TextMuted,
        440.0,
    );

    // Footer.
    let footer = Rect::from_min_max(pos2(rect.min.x, rect.max.y - size::DIALOG_FOOTER), rect.max);
    painter.hline(
        footer.x_range(),
        footer.min.y + 0.5,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
    let summary = t!(
        "import.folder.summary",
        files = view.new_files,
        bins = if view.keep_folders_as_bins {
            view.bins
        } else {
            0
        },
        size = format::bytes(view.new_bytes)
    );
    paint.text(
        &painter,
        pos2(footer.min.x + s16, footer.center().y),
        Align2::LEFT_CENTER,
        &summary,
        Type::MonoCaption,
        Role::TextMuted,
        280.0,
    );
    let import_label = match view.new_files {
        1 => t!("import.folder.import_one"),
        n => t!("import.folder.import", count = n),
    };
    let import_w = paint.width(&painter, &import_label, Type::LabelMedium) + 24.0;
    let import_rect = Rect::from_min_size(
        pos2(footer.max.x - s16 - import_w, footer.center().y - 16.0),
        vec2(import_w, size::DIALOG_BUTTON),
    );
    let cancel_label = t!("common.cancel");
    let cancel_w = paint.width(&painter, &cancel_label, Type::LabelMedium) + 24.0;
    let cancel_rect = Rect::from_min_size(
        pos2(import_rect.min.x - 8.0 - cancel_w, import_rect.min.y),
        vec2(cancel_w, size::DIALOG_BUTTON),
    );
    if paint
        .button(ui, cancel_rect, &cancel_label, false, true)
        .clicked()
    {
        action = FolderImportAction::Cancel;
    }
    if paint
        .button(ui, import_rect, &import_label, true, view.new_files > 0)
        .clicked()
    {
        action = FolderImportAction::Import;
    }
    (action, rect)
}

/// An import in progress, for the strip at the top of the media pool.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportProgress {
    /// What is being imported (`Card B`, a folder name, or the first file's name).
    pub source: String,
    pub bin: String,
    pub done: usize,
    pub total: usize,
    pub current_file: String,
    pub current_bytes: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub elapsed_s: f64,
}

impl ImportProgress {
    pub fn fraction(&self) -> f32 {
        if self.bytes_total > 0 {
            self.bytes_done as f32 / self.bytes_total as f32
        } else if self.total > 0 {
            self.done as f32 / self.total as f32
        } else {
            0.0
        }
    }

    /// Seconds left at the rate so far, once there is a rate to go on.
    pub fn seconds_left(&self) -> Option<f64> {
        let fraction = self.fraction() as f64;
        (fraction > 0.02 && self.elapsed_s > 0.5)
            .then(|| self.elapsed_s * (1.0 - fraction) / fraction)
    }
}

/// Height of the progress strip.
pub const PROGRESS_STRIP_HEIGHT: f32 = 8.0 + 16.0 + 6.0 + 4.0 + 6.0 + 14.0 + 10.0;

/// Draws the import strip into `rect` (full panel width, [`PROGRESS_STRIP_HEIGHT`]);
/// returns true when Stop was pressed.
pub fn progress_strip(ui: &mut Ui, rect: Rect, progress: &ImportProgress) -> bool {
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    painter.hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
    let x0 = rect.min.x + 8.0;
    let x1 = rect.max.x - 8.0;
    let mut y = rect.min.y + 8.0;
    let count = t!(
        "import.progress.count",
        done = progress.done,
        total = progress.total
    );
    let stop = Rect::from_min_size(pos2(x1 - 20.0, y - 2.0), vec2(20.0, 20.0));
    let count_rect = paint.text(
        &painter,
        pos2(stop.min.x - 8.0, y + 8.0),
        Align2::RIGHT_CENTER,
        &count,
        Type::MonoCaption,
        Role::TextMuted,
        120.0,
    );
    let title = t!(
        "import.progress.title",
        source = progress.source,
        bin = progress.bin
    );
    paint.text(
        &painter,
        pos2(x0, y + 8.0),
        Align2::LEFT_CENTER,
        &title,
        Type::LabelMedium,
        Role::TextPrimary,
        count_rect.min.x - 8.0 - x0,
    );
    let stopped = super::widgets::icon_button(&paint, ui, stop, Icon::Close, false).clicked();
    y += 16.0 + 6.0;
    paint.progress(
        &painter,
        Rect::from_min_max(pos2(x0, y), pos2(x1, y + 4.0)),
        progress.fraction(),
        Role::Bg3,
    );
    y += 4.0 + 6.0;
    let mut detail = format!(
        "{} · {}",
        progress.current_file,
        format::bytes(progress.current_bytes)
    );
    if let Some(left) = progress.seconds_left() {
        detail = format!("{detail} · {}", format::time_left(left));
    }
    paint.text(
        &painter,
        pos2(x0, y),
        Align2::LEFT_TOP,
        &detail,
        Type::MonoCaption,
        Role::TextMuted,
        x1 - x0,
    );
    stopped
}
