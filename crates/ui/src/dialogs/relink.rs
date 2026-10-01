//! `Dialog / Relink media` (Paper, page "10 System & states"): finds moved files for every
//! offline clip and relinks them in one step.
//!
//! The engine searches on its own thread (`engine::media_mgmt::relink`); the app turns
//! each offline source and what was found for it into a [`ClipView`], and the dialog
//! draws the list: one card per clip with its candidates (name, duration and content
//! hash checks and a match percentage), or the search in progress, or "no match". The
//! best match is picked for the user; a radio picks another. Relink (Enter) applies
//! every picked match as one undoable edit; while it runs the footer shows progress and
//! a Stop button. Close or Escape changes nothing.

use egui::{
    Align2, CornerRadius, Id, Key, Order, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
    pos2, vec2,
};
use engine::media_mgmt::relink::{Candidate, Check, RelinkTarget, TargetMatches};
use project::MediaId;

use crate::panels::format;
use crate::panels::widgets::{Icon, Paint, Type, icon, size};
use crate::t;
use crate::theme::{Role, TEXTURE_TINT};

/// The dialog's width from the board.
pub const WIDTH: f32 = 760.0;
const ICON: f32 = 16.0;
/// The board's icons: a 2.25 stroke on the 24-unit grid.
const ICON_STROKE: f32 = 2.25;
const THUMB: Vec2 = vec2(64.0, 36.0);
/// Candidate table columns: name, duration, hash, match.
const COL_NAME: f32 = 56.0;
const COL_DURATION: f32 = 72.0;
const COL_HASH: f32 = 56.0;
const COL_MATCH: f32 = 48.0;
const COLUMN_HEADER: f32 = 24.0;
const SMALL_BUTTON: f32 = 24.0;
const FOOTER_BAR: f32 = 120.0;
/// The space kept around the dialog when the screen is too short for every clip.
const SCREEN_MARGIN: f32 = 32.0;

/// One candidate row.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateRow {
    pub path: String,
    pub name: Check,
    pub duration: Check,
    pub hash: Check,
    /// `None` while the file is still being checked.
    pub percent: Option<u8>,
    /// The hash is still being checked (a spinner in the hash column).
    pub checking: bool,
    /// False for a file that cannot be used (shorter than the clips need, or on a drive
    /// that is not connected): drawn faded, not selectable.
    pub enabled: bool,
}

impl CandidateRow {
    pub fn from_candidate(c: &Candidate) -> Self {
        Self {
            path: c.path.to_string_lossy().into_owned(),
            name: c.name,
            duration: c.duration,
            hash: c.hash,
            percent: Some(c.percent()),
            checking: false,
            enabled: c.relinkable(),
        }
    }
}

/// What a clip's card shows below its header.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipState {
    /// The search is running: where, how far, and the folder being read.
    Searching {
        place: String,
        files_done: usize,
        files_total: usize,
        folder: String,
    },
    /// Candidates, best first, and the one picked.
    Found {
        candidates: Vec<CandidateRow>,
        selected: Option<usize>,
    },
    /// Nothing matched in `place`.
    NoMatch { place: String },
}

/// One offline clip in the dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipView {
    pub media: MediaId,
    /// `A014_C011 · Rope detail`.
    pub name: String,
    /// `Last seen /Volumes/Card A · 4K · 23.976 fps · 12 s`.
    pub detail: String,
    /// Clips on the timeline that use it.
    pub edits: usize,
    pub thumbnail: Option<egui::TextureId>,
    pub state: ClipState,
}

impl ClipView {
    /// A clip from the engine's target and what the search found for it in `place`
    /// (`None`: still searching there).
    pub fn new(target: &RelinkTarget, matches: Option<&TargetMatches>, place: &str) -> Self {
        let state = match matches {
            None => ClipState::Searching {
                place: place.to_owned(),
                files_done: 0,
                files_total: 0,
                folder: place.to_owned(),
            },
            Some(m) if m.candidates.is_empty() => ClipState::NoMatch {
                place: place.to_owned(),
            },
            Some(m) => ClipState::Found {
                candidates: m
                    .candidates
                    .iter()
                    .map(CandidateRow::from_candidate)
                    .collect(),
                selected: m.best(),
            },
        };
        Self {
            media: target.media,
            name: target.name.clone(),
            detail: detail_text(target),
            edits: target.clips,
            thumbnail: None,
            state,
        }
    }

    pub fn selected(&self) -> Option<usize> {
        match &self.state {
            ClipState::Found { selected, .. } => *selected,
            _ => None,
        }
    }
}

/// `Last seen /Volumes/Card A · 4K · 23.976 fps · 12 s`, leaving out what is unknown.
pub fn detail_text(target: &RelinkTarget) -> String {
    let folder = target
        .path
        .parent()
        .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
    let mut parts = vec![t!("relink.clip.last_seen", folder = folder)];
    if let Some(f) = &target.facts {
        if f.has_video() {
            parts.push(format::resolution_class(f.width, f.height));
        }
        if let Some(fps) = f.fps() {
            parts.push(t!("relink.clip.fps", fps = format::fps(fps)));
        }
        if let Some(d) = f.duration_s {
            parts.push(if d < 60.0 {
                t!("relink.clip.seconds", count = d.round() as u64)
            } else {
                format::short_clock(d)
            });
        }
    }
    parts.join(" · ")
}

/// The name people know a folder by: the drive under `/Volumes`, else its last part.
pub fn place_name(path: &std::path::Path) -> String {
    let mut parts = path.components().filter_map(|c| match c {
        std::path::Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
        _ => None,
    });
    match parts.next() {
        Some(first) if first == "Volumes" => parts.next().unwrap_or(first),
        _ => path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

/// While the relink is applied: `done` of `total`, now `name`.
#[derive(Clone, Debug, PartialEq)]
pub struct Relinking {
    pub done: usize,
    pub total: usize,
    pub name: String,
}

/// What the dialog shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RelinkView {
    pub project_name: String,
    pub clips: Vec<ClipView>,
    /// Where the last search looked (`Studio SSD`); empty before the first search.
    pub place: String,
    pub folders_searched: usize,
    pub elapsed_s: f32,
    /// "Look in the same folders for other offline clips".
    pub same_folders: bool,
    pub relinking: Option<Relinking>,
}

impl RelinkView {
    /// Clips with a match picked.
    pub fn matched(&self) -> usize {
        self.clips.iter().filter(|c| c.selected().is_some()).count()
    }

    fn searching(&self) -> bool {
        self.clips
            .iter()
            .any(|c| matches!(c.state, ClipState::Searching { .. }))
    }

    /// Applies the choices the dialog handles itself (picking a row, leaving a clip
    /// offline, the same-folders option); the rest are for the caller.
    pub fn apply(&mut self, action: &RelinkAction) {
        match *action {
            RelinkAction::Select { media, index } => {
                if let Some(clip) = self.clips.iter_mut().find(|c| c.media == media)
                    && let ClipState::Found {
                        candidates,
                        selected,
                    } = &mut clip.state
                    && candidates.get(index).is_some_and(|r| r.enabled)
                {
                    *selected = Some(index);
                }
            }
            RelinkAction::LeaveOffline(media) => self.clips.retain(|c| c.media != media),
            RelinkAction::SameFolders(on) => self.same_folders = on,
            _ => {}
        }
    }
}

/// What the user did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelinkAction {
    /// Close, Cancel or Escape: nothing changes.
    Close,
    /// Pick a folder or drive to search for every clip still offline.
    SearchFolder,
    /// Pick a folder for one clip that had no match.
    SearchFolderFor(MediaId),
    Select {
        media: MediaId,
        index: usize,
    },
    LeaveOffline(MediaId),
    SameFolders(bool),
    /// Relink every picked match (Enter).
    Relink,
    /// Stop a relink in progress.
    Stop,
}

/// The dialog's clickable parts, for tests and accessibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Control {
    Close,
    SearchFolder,
    /// A clip's own Search folder… (no match), by clip index.
    ClipSearchFolder(usize),
    LeaveOffline(usize),
    /// Clip index, candidate index.
    Candidate(usize, usize),
    SameFolders,
    Cancel,
    Relink,
    Stop,
}

/// The dialog's state between frames.
#[derive(Clone, Debug, Default)]
pub struct RelinkDialog {
    controls: Vec<(Control, Rect)>,
}

impl RelinkDialog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Where a control was drawn in the last frame.
    pub fn control_rect(&self, control: Control) -> Option<Rect> {
        self.controls
            .iter()
            .find(|(c, _)| *c == control)
            .map(|(_, r)| *r)
    }

    /// Draws the dialog centred over a dimmed screen that swallows clicks, handles Enter
    /// and Escape, applies the choices it can ([`RelinkView::apply`]) and returns the
    /// user's action.
    pub fn show(&mut self, ctx: &egui::Context, view: &mut RelinkView) -> Option<RelinkAction> {
        let paint = Paint::new(ctx);
        let screen = ctx.content_rect();
        let scrim = paint.mix(Role::Bg0, paint.theme.tokens().opacity.glass);
        egui::Area::new(Id::new("glass.relink.scrim"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(screen, CornerRadius::ZERO, scrim);
                ui.allocate_rect(screen, Sense::click());
            });
        let max_height = (screen.height() - 2.0 * SCREEN_MARGIN).max(240.0);
        let height = self.height(ctx, view).min(max_height);
        let origin = (screen.center() - vec2(WIDTH, height) / 2.0).round();
        let mut action = None;
        egui::Area::new(Id::new("glass.relink"))
            .order(Order::Foreground)
            .fixed_pos(origin)
            .show(ctx, |ui| {
                action = self.draw(ui, origin, view, max_height).0;
            });
        let (enter, escape) =
            ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if action.is_none() && view.relinking.is_none() {
            if enter && view.matched() > 0 {
                action = Some(RelinkAction::Relink);
            } else if escape {
                action = Some(RelinkAction::Close);
            }
        }
        if let Some(a) = &action {
            view.apply(a);
        }
        action
    }

    /// The dialog's full height for `view` (before limiting it to the screen).
    pub fn height(&self, ctx: &egui::Context, view: &RelinkView) -> f32 {
        let paint = Paint::new(ctx);
        size::DIALOG_HEADER + body_height(&paint, view) + size::DIALOG_FOOTER
    }

    /// Draws the dialog with its top-left at `origin`, at most `max_height` tall (the
    /// clips scroll beyond that), without scrim or keys ([`RelinkDialog::show`] adds
    /// them); returns the action and the dialog's rect.
    pub fn draw(
        &mut self,
        ui: &mut Ui,
        origin: Pos2,
        view: &RelinkView,
        max_height: f32,
    ) -> (Option<RelinkAction>, Rect) {
        self.controls.clear();
        let paint = Paint::new(ui.ctx());
        let painter = ui.painter().clone();
        let s = *paint.s();
        let content = body_height(&paint, view);
        let height = (size::DIALOG_HEADER + content + size::DIALOG_FOOTER).min(max_height);
        let rect = Rect::from_min_size(origin, vec2(WIDTH, height));
        ui.allocate_rect(rect, Sense::click());
        paint.floating(&painter, rect);
        let relinking = view.relinking.is_some();
        let mut action = None;

        // Header: title, project and offline count, close button.
        let header = Rect::from_min_size(rect.min, vec2(WIDTH, size::DIALOG_HEADER));
        painter.hline(
            header.x_range().shrink(1.0),
            header.max.y - 0.5,
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        );
        // Title and subtitle share a line, bottoms aligned like the board's baselines.
        let mut job = egui::text::LayoutJob::default();
        job.append(
            &t!("relink.title"),
            0.0,
            egui::TextFormat::simple(paint.font(Type::BodyMedium), paint.c(Role::TextPrimary)),
        );
        job.append(
            &t!(
                "relink.subtitle",
                project = view.project_name,
                count = view.clips.len()
            ),
            s.s8,
            egui::TextFormat::simple(paint.font(Type::MonoCaption), paint.c(Role::TextMuted)),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(WIDTH - 2.0 * s.s16 - SMALL_BUTTON);
        let title = painter.layout_job(job);
        painter.galley(
            pos2(
                header.min.x + s.s16,
                header.center().y - title.size().y / 2.0,
            ),
            title,
            paint.c(Role::TextPrimary),
        );
        let close = Rect::from_min_size(
            pos2(
                header.max.x - s.s12 - SMALL_BUTTON,
                header.center().y - SMALL_BUTTON / 2.0,
            ),
            Vec2::splat(SMALL_BUTTON),
        );
        let response = self.control(ui, Control::Close, close);
        if response.hovered() && !relinking {
            painter.rect_filled(close, paint.control_radius(), paint.c(Role::Bg3));
        }
        icon(
            &painter,
            Rect::from_center_size(close.center(), Vec2::splat(ICON)),
            Icon::Close,
            paint.c(Role::TextSecondary),
            ICON_STROKE,
        );
        if response.clicked() && !relinking {
            action = Some(RelinkAction::Close);
        }

        // Body: scrolls when the screen is too short for every clip.
        let body = Rect::from_min_max(
            pos2(rect.min.x, header.max.y),
            pos2(rect.max.x, rect.max.y - size::DIALOG_FOOTER),
        );
        let mut body_action = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("glass.relink.body")
                .max_height(body.height())
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let (inner, _) = ui.allocate_exact_size(vec2(WIDTH, content), Sense::hover());
                    body_action = self.draw_body(ui, &paint, inner, view);
                });
        });

        // Footer.
        let footer =
            Rect::from_min_max(pos2(rect.min.x, rect.max.y - size::DIALOG_FOOTER), rect.max);
        painter.hline(
            footer.x_range().shrink(1.0),
            footer.min.y + 0.5,
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        );
        let footer_action = self.draw_footer(ui, &paint, footer, view);
        (action.or(body_action).or(footer_action), rect)
    }

    fn control(&mut self, ui: &mut Ui, control: Control, rect: Rect) -> egui::Response {
        self.controls.push((control, rect));
        ui.interact(
            rect,
            Id::new(("glass.relink.control", control)),
            Sense::click(),
        )
    }

    fn draw_body(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        rect: Rect,
        view: &RelinkView,
    ) -> Option<RelinkAction> {
        let s = *paint.s();
        let painter = ui.painter().clone();
        let relinking = view.relinking.is_some();
        let mut action = None;
        let left = rect.min.x + s.s16;
        let right = rect.max.x - s.s16;
        let mut y = rect.min.y + s.s16;

        // Summary line and Search folder… button.
        let mid = y + SMALL_BUTTON / 2.0;
        let label = t!("relink.search_folder");
        let w = s.s8 + ICON + s.s8 + paint.width(&painter, &label, Type::LabelMedium) + s.s8;
        let search = Rect::from_min_size(pos2(right - w, y), vec2(w, SMALL_BUTTON));
        if self.search_button(ui, paint, Control::SearchFolder, search, &label, !relinking) {
            action = Some(RelinkAction::SearchFolder);
        }
        paint.text(
            &painter,
            pos2(left, mid),
            Align2::LEFT_CENTER,
            &summary_text(view),
            Type::Label,
            Role::TextSecondary,
            search.min.x - s.s16 - left,
        );
        y += SMALL_BUTTON + s.s16;

        // One card per clip.
        // Only the cards in view are drawn; a long batch scrolls.
        let visible = ui.clip_rect();
        for (index, clip) in view.clips.iter().enumerate() {
            let h = card_height(paint, clip);
            let card = Rect::from_min_size(pos2(left, y), vec2(right - left, h));
            if card.intersects(visible)
                && let Some(a) = self.draw_card(ui, paint, card, index, clip, relinking)
            {
                action = Some(a);
            }
            y += h + s.s16;
        }

        // The same-folders option.
        let check = Rect::from_min_size(pos2(left, y), Vec2::splat(size::CHECK));
        let label_x = check.max.x + s.s8;
        let title_h = paint.line_height(Type::Label);
        let option = Rect::from_min_max(
            check.min,
            pos2(right, y + title_h + 2.0 + paint.line_height(Type::Caption)),
        );
        let response = self.control(ui, Control::SameFolders, option);
        paint.checkbox(&painter, check, view.same_folders);
        if response.hovered() && !relinking {
            painter.rect_stroke(
                check,
                paint.control_radius(),
                Stroke::new(1.0, paint.c(Role::TextSecondary)),
                StrokeKind::Outside,
            );
        }
        paint.text(
            &painter,
            pos2(label_x, y),
            Align2::LEFT_TOP,
            &t!("relink.same_folders"),
            Type::LabelMedium,
            Role::TextPrimary,
            right - label_x,
        );
        paint.text(
            &painter,
            pos2(label_x, y + title_h + 2.0),
            Align2::LEFT_TOP,
            &t!("relink.same_folders_note"),
            Type::Caption,
            Role::TextMuted,
            right - label_x,
        );
        if response.clicked() && !relinking {
            action = Some(RelinkAction::SameFolders(!view.same_folders));
        }
        action
    }

    /// A small glass button with the folder icon; returns true when clicked.
    fn search_button(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        control: Control,
        rect: Rect,
        label: &str,
        enabled: bool,
    ) -> bool {
        let painter = ui.painter().clone();
        let s = *paint.s();
        let response = self.control(ui, control, rect);
        let opacity = if enabled {
            1.0
        } else {
            paint.theme.tokens().opacity.disabled
        };
        if response.hovered() && enabled {
            painter.rect_filled(rect, paint.control_radius(), paint.c(Role::Bg3));
        }
        paint.glass_outline(&painter, rect, paint.control_radius());
        let color = paint.c(Role::TextPrimary).gamma_multiply(opacity);
        icon(
            &painter,
            Rect::from_min_size(
                pos2(rect.min.x + s.s8, rect.center().y - ICON / 2.0),
                Vec2::splat(ICON),
            ),
            Icon::Folder,
            color,
            ICON_STROKE,
        );
        paint.text_c(
            &painter,
            pos2(rect.min.x + s.s8 + ICON + s.s8, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            Type::LabelMedium,
            color,
            rect.width(),
        );
        response.clicked() && enabled
    }

    fn draw_card(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        card: Rect,
        index: usize,
        clip: &ClipView,
        relinking: bool,
    ) -> Option<RelinkAction> {
        let s = *paint.s();
        let painter = ui.painter().clone();
        let corner = paint.control_radius();
        painter.rect_filled(card, corner, paint.c(Role::Bg1));

        // Header: thumbnail, name and last-seen line, offline pill.
        let header_h = card_header_height(paint);
        let header = Rect::from_min_size(card.min, vec2(card.width(), header_h));
        let thumb = Rect::from_min_size(
            pos2(card.min.x + s.s12, header.center().y - 0.5 - THUMB.y / 2.0),
            THUMB,
        );
        draw_thumbnail(paint, &painter, thumb, clip.thumbnail, index);
        let pill_text = if clip.edits == 1 {
            t!("relink.clip.offline_one")
        } else {
            t!("relink.clip.offline", count = clip.edits)
        };
        let pill_w = 8.0 + 6.0 + 8.0 + paint.width(&painter, &pill_text, Type::Caption) + 8.0;
        let pill_x = card.max.x - s.s12 - pill_w;
        paint.status_pill(
            &painter,
            pos2(pill_x, thumb.center().y),
            &pill_text,
            Role::StateError,
        );
        let text_x = thumb.max.x + s.s12;
        let text_w = pill_x - s.s12 - text_x;
        let lines_h = paint.line_height(Type::Label) + 2.0 + paint.line_height(Type::Caption);
        let top = thumb.center().y - lines_h / 2.0;
        paint.text(
            &painter,
            pos2(text_x, top),
            Align2::LEFT_TOP,
            &clip.name,
            Type::LabelMedium,
            Role::TextPrimary,
            text_w,
        );
        paint.text(
            &painter,
            pos2(text_x, top + paint.line_height(Type::Label) + 2.0),
            Align2::LEFT_TOP,
            &clip.detail,
            Type::MonoCaption,
            Role::TextMuted,
            text_w,
        );
        painter.hline(
            card.x_range(),
            header.max.y - 0.5,
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        );

        let below = Rect::from_min_max(pos2(card.min.x, header.max.y), card.max);
        let action = match &clip.state {
            ClipState::Found {
                candidates,
                selected,
            } => self.draw_candidates(
                ui, paint, below, index, clip.media, candidates, *selected, relinking,
            ),
            ClipState::Searching {
                place,
                files_done,
                files_total,
                folder,
            } => {
                draw_searching(
                    paint,
                    &painter,
                    below,
                    place,
                    *files_done,
                    *files_total,
                    folder,
                );
                None
            }
            ClipState::NoMatch { place } => {
                self.draw_no_match(ui, paint, below, index, clip.media, place, relinking)
            }
        };
        painter.rect_stroke(
            card,
            corner,
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
            StrokeKind::Inside,
        );
        action
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_candidates(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        area: Rect,
        clip_index: usize,
        media: MediaId,
        rows: &[CandidateRow],
        selected: Option<usize>,
        relinking: bool,
    ) -> Option<RelinkAction> {
        let s = *paint.s();
        let painter = ui.painter().clone();
        let columns = Columns::new(area, s.s12, s.s8);

        // Column headers.
        let mid = area.min.y + COLUMN_HEADER / 2.0;
        let caps = |x: f32, align: Align2, label: String, w: f32| {
            paint.text(
                &painter,
                pos2(x, mid),
                align,
                &label,
                Type::Caps,
                Role::TextMuted,
                w,
            );
        };
        caps(
            columns.path.min,
            Align2::LEFT_CENTER,
            t!("relink.column.found_at"),
            columns.path.span(),
        );
        caps(
            columns.name.center(),
            Align2::CENTER_CENTER,
            t!("relink.column.name"),
            COL_NAME + 16.0,
        );
        caps(
            columns.duration.center(),
            Align2::CENTER_CENTER,
            t!("relink.column.duration"),
            COL_DURATION + 16.0,
        );
        caps(
            columns.hash.center(),
            Align2::CENTER_CENTER,
            t!("relink.column.hash"),
            COL_HASH + 16.0,
        );
        caps(
            columns.matched.max,
            Align2::RIGHT_CENTER,
            t!("relink.column.match"),
            COL_MATCH + 16.0,
        );

        let mut action = None;
        let mut y = area.min.y + COLUMN_HEADER;
        for (i, row) in rows.iter().enumerate() {
            let r = Rect::from_min_size(pos2(area.min.x, y), vec2(area.width(), size::ROW));
            let on = selected == Some(i);
            let response = self.control(ui, Control::Candidate(clip_index, i), r);
            let response = if row.enabled {
                response
            } else {
                response.on_hover_text(t!("relink.row.unavailable"))
            };
            if response.hovered() && row.enabled && !relinking {
                painter.rect_filled(
                    r.shrink2(vec2(1.0, 0.0)),
                    CornerRadius::ZERO,
                    paint.c(Role::Bg3),
                );
            }
            let opacity = if row.enabled {
                1.0
            } else {
                paint.theme.tokens().opacity.disabled
            };
            draw_row(paint, &painter, &columns, r, row, on, opacity);
            if response.clicked() && row.enabled && !relinking && !on {
                action = Some(RelinkAction::Select { media, index: i });
            }
            y += size::ROW;
        }
        action
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_no_match(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        area: Rect,
        clip_index: usize,
        media: MediaId,
        place: &str,
        relinking: bool,
    ) -> Option<RelinkAction> {
        let s = *paint.s();
        let painter = ui.painter().clone();
        let mut action = None;
        let top = area.min.y + s.s12;
        let left = area.min.x + s.s12;
        let right = area.max.x - s.s12;
        icon(
            &painter,
            Rect::from_min_size(pos2(left, top), Vec2::splat(ICON)),
            Icon::Alert,
            paint.c(Role::StateWarning),
            ICON_STROKE,
        );
        // Buttons on the right, centred on the text block.
        let text_h = paint.line_height(Type::Label) * 2.0 + 2.0;
        let mid = top + text_h / 2.0;
        let label = t!("relink.search_folder");
        let w = s.s8 + ICON + s.s8 + paint.width(&painter, &label, Type::LabelMedium) + s.s8;
        let search = Rect::from_min_size(
            pos2(right - w, mid - SMALL_BUTTON / 2.0),
            vec2(w, SMALL_BUTTON),
        );
        if self.search_button(
            ui,
            paint,
            Control::ClipSearchFolder(clip_index),
            search,
            &label,
            !relinking,
        ) {
            action = Some(RelinkAction::SearchFolderFor(media));
        }
        let leave = t!("relink.leave_offline");
        let leave_w = paint.width(&painter, &leave, Type::LabelMedium) + 2.0 * s.s8;
        let leave_rect = Rect::from_min_size(
            pos2(search.min.x - s.s8 - leave_w, mid - SMALL_BUTTON / 2.0),
            vec2(leave_w, SMALL_BUTTON),
        );
        let response = self.control(ui, Control::LeaveOffline(clip_index), leave_rect);
        if response.hovered() && !relinking {
            painter.rect_filled(leave_rect, paint.control_radius(), paint.c(Role::Bg3));
        }
        paint.text(
            &painter,
            leave_rect.center(),
            Align2::CENTER_CENTER,
            &leave,
            Type::LabelMedium,
            Role::TextSecondary,
            leave_w,
        );
        if response.clicked() && !relinking {
            action = Some(RelinkAction::LeaveOffline(media));
        }
        let text_x = left + ICON + s.s8;
        let text_w = leave_rect.min.x - s.s8 - text_x;
        paint.text(
            &painter,
            pos2(text_x, top),
            Align2::LEFT_TOP,
            &t!("relink.no_match.title", place = place),
            Type::LabelMedium,
            Role::TextPrimary,
            text_w,
        );
        paint.text(
            &painter,
            pos2(text_x, top + paint.line_height(Type::Label) + 2.0),
            Align2::LEFT_TOP,
            &t!("relink.no_match.body"),
            Type::Label,
            Role::TextMuted,
            text_w,
        );
        action
    }

    fn draw_footer(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        footer: Rect,
        view: &RelinkView,
    ) -> Option<RelinkAction> {
        let s = *paint.s();
        let painter = ui.painter().clone();
        let mid = footer.center().y;
        let right = footer.max.x - s.s16;
        let left = footer.min.x + s.s16;
        let mut action = None;
        let count = match &view.relinking {
            Some(r) => r.total,
            None => view.matched(),
        };
        let primary_label = if count == 0 {
            t!("relink.relink")
        } else if count == 1 {
            t!("relink.relink_one")
        } else {
            t!("relink.relink_many", count = count)
        };
        let primary_w = paint.width(&painter, &primary_label, Type::LabelMedium) + 2.0 * s.s12;
        let primary = Rect::from_min_size(
            pos2(right - primary_w, mid - size::DIALOG_BUTTON / 2.0),
            vec2(primary_w, size::DIALOG_BUTTON),
        );
        let enabled = view.relinking.is_none() && count > 0 && !view.searching();
        let response = self.control(ui, Control::Relink, primary);
        let opacity = if enabled {
            1.0
        } else {
            paint.theme.tokens().opacity.disabled
        };
        let corner = paint.control_radius();
        painter.rect_filled(
            primary,
            corner,
            paint.c(Role::Accent).gamma_multiply(opacity),
        );
        if response.hovered() && enabled {
            painter.rect_filled(primary, corner, paint.c(Role::GlassHighlight));
        }
        paint.text_c(
            &painter,
            primary.center(),
            Align2::CENTER_CENTER,
            &primary_label,
            Type::LabelMedium,
            paint.c(Role::TextInverse).gamma_multiply(opacity),
            primary_w,
        );
        if response.clicked() && enabled {
            action = Some(RelinkAction::Relink);
        }
        let (secondary_control, secondary_label) = match view.relinking {
            Some(_) => (Control::Stop, t!("relink.stop")),
            None => (Control::Cancel, t!("common.cancel")),
        };
        let secondary_w = paint.width(&painter, &secondary_label, Type::LabelMedium) + 2.0 * s.s12;
        let secondary = Rect::from_min_size(
            pos2(
                primary.min.x - s.s8 - secondary_w,
                mid - size::DIALOG_BUTTON / 2.0,
            ),
            vec2(secondary_w, size::DIALOG_BUTTON),
        );
        let response = self.control(ui, secondary_control, secondary);
        if response.hovered() {
            painter.rect_filled(secondary, corner, paint.c(Role::Bg3));
        }
        paint.glass_outline(&painter, secondary, corner);
        paint.text(
            &painter,
            secondary.center(),
            Align2::CENTER_CENTER,
            &secondary_label,
            Type::LabelMedium,
            Role::TextPrimary,
            secondary_w,
        );
        if response.clicked() {
            action = Some(match secondary_control {
                Control::Stop => RelinkAction::Stop,
                _ => RelinkAction::Close,
            });
        }

        let text_right = secondary.min.x - s.s16;
        match &view.relinking {
            Some(r) => {
                paint.spinner(
                    &painter,
                    Rect::from_min_size(pos2(left, mid - ICON / 2.0), Vec2::splat(ICON)),
                );
                let label_x = left + ICON + s.s12;
                let label = paint.text(
                    &painter,
                    pos2(label_x, mid),
                    Align2::LEFT_CENTER,
                    &t!(
                        "relink.footer.relinking",
                        done = r.done.min(r.total) + 1,
                        total = r.total,
                        name = r.name
                    ),
                    Type::MonoCaption,
                    Role::TextSecondary,
                    (text_right - label_x - FOOTER_BAR - s.s12).max(1.0),
                );
                let bar = Rect::from_min_size(
                    pos2(label.max.x + s.s12, mid - 2.0),
                    vec2(FOOTER_BAR, 4.0),
                );
                // The clip in progress counts, as on the board (1 of 2 is half way).
                let fraction = if r.total == 0 {
                    0.0
                } else {
                    (r.done + 1).min(r.total) as f32 / r.total as f32
                };
                paint.progress(&painter, bar, fraction, Role::Bg3);
            }
            None => {
                paint.text(
                    &painter,
                    pos2(left, mid),
                    Align2::LEFT_CENTER,
                    &footer_text(view),
                    Type::MonoCaption,
                    Role::TextMuted,
                    (text_right - left).max(1.0),
                );
            }
        }
        action
    }
}

/// The x ranges of the candidate table, shared by the header and every row.
struct Columns {
    radio: egui::Rangef,
    path: egui::Rangef,
    name: egui::Rangef,
    duration: egui::Rangef,
    hash: egui::Rangef,
    matched: egui::Rangef,
}

impl Columns {
    fn new(area: Rect, pad: f32, gap: f32) -> Self {
        let right = area.max.x - pad;
        let matched = egui::Rangef::new(right - COL_MATCH, right);
        let hash = egui::Rangef::new(matched.min - gap - COL_HASH, matched.min - gap);
        let duration = egui::Rangef::new(hash.min - gap - COL_DURATION, hash.min - gap);
        let name = egui::Rangef::new(duration.min - gap - COL_NAME, duration.min - gap);
        let radio = egui::Rangef::new(area.min.x + pad, area.min.x + pad + size::CHECK);
        let path = egui::Rangef::new(radio.max + gap, name.min - gap);
        Self {
            radio,
            path,
            name,
            duration,
            hash,
            matched,
        }
    }
}

fn draw_row(
    paint: &Paint,
    painter: &Painter,
    columns: &Columns,
    r: Rect,
    row: &CandidateRow,
    on: bool,
    opacity: f32,
) {
    let mid = r.center().y;
    let faded = |role: Role| paint.c(role).gamma_multiply(opacity);
    let radio = Rect::from_center_size(pos2(columns.radio.center(), mid), Vec2::splat(size::CHECK));
    if opacity < 1.0 {
        // A faded radio: draw it into its own layer's colours at the row's opacity.
        painter.circle_filled(radio.center(), radio.width() / 2.0, faded(Role::Bg2));
        painter.circle_stroke(
            radio.center(),
            radio.width() / 2.0 - 0.5,
            Stroke::new(1.0, faded(Role::BorderStrong)),
        );
    } else {
        paint.radio(painter, radio, on);
    }
    let text_role = if on {
        Role::TextPrimary
    } else {
        Role::TextSecondary
    };
    paint.text_c(
        painter,
        pos2(columns.path.min, mid),
        Align2::LEFT_CENTER,
        &row.path,
        Type::Mono,
        faded(text_role),
        columns.path.span(),
    );
    let check = |range: egui::Rangef, value: Check, checking: bool| {
        let at = Rect::from_center_size(pos2(range.center(), mid), Vec2::splat(ICON));
        if checking {
            paint.spinner(painter, at);
            return;
        }
        match value {
            Check::Match => icon(
                painter,
                at,
                Icon::Check,
                faded(Role::StateSuccess),
                ICON_STROKE,
            ),
            Check::Mismatch => icon(
                painter,
                at,
                Icon::Close,
                faded(Role::TextMuted),
                ICON_STROKE,
            ),
            Check::Unknown => {
                paint.text_c(
                    painter,
                    at.center(),
                    Align2::CENTER_CENTER,
                    &t!("relink.unknown"),
                    Type::Mono,
                    faded(Role::TextMuted),
                    range.span(),
                );
            }
        }
    };
    check(columns.name, row.name, false);
    check(columns.duration, row.duration, false);
    check(columns.hash, row.hash, row.checking);
    let (percent, role) = match row.percent {
        Some(p) if row.enabled => (
            t!("relink.percent", percent = p),
            if row.checking {
                Role::TextMuted
            } else {
                text_role
            },
        ),
        Some(_) => (t!("relink.unknown"), text_role),
        None => (t!("relink.unknown"), Role::TextMuted),
    };
    paint.text_c(
        painter,
        pos2(columns.matched.max, mid),
        Align2::RIGHT_CENTER,
        &percent,
        Type::Mono,
        faded(role),
        COL_MATCH + 16.0,
    );
}

fn draw_searching(
    paint: &Paint,
    painter: &Painter,
    area: Rect,
    place: &str,
    done: usize,
    total: usize,
    folder: &str,
) {
    let s = paint.s();
    let left = area.min.x + s.s12;
    let right = area.max.x - s.s12;
    let mut y = area.min.y + s.s12;
    let line = paint.line_height(Type::Label);
    let mid = y + line / 2.0;
    paint.spinner(
        painter,
        Rect::from_min_size(pos2(left, mid - ICON / 2.0), Vec2::splat(ICON)),
    );
    let count = if total > 0 {
        t!(
            "relink.searching.files",
            done = thousands(done),
            total = thousands(total)
        )
    } else {
        String::new()
    };
    let count_r = paint.text(
        painter,
        pos2(right, mid),
        Align2::RIGHT_CENTER,
        &count,
        Type::MonoCaption,
        Role::TextMuted,
        (right - left) / 2.0,
    );
    paint.text(
        painter,
        pos2(left + ICON + s.s8, mid),
        Align2::LEFT_CENTER,
        &t!("relink.searching.title", place = place),
        Type::LabelMedium,
        Role::TextPrimary,
        count_r.min.x - s.s8 - (left + ICON + s.s8),
    );
    y += line + s.s8;
    let bar = Rect::from_min_size(pos2(left, y), vec2(right - left, 4.0));
    let fraction = if total == 0 {
        0.0
    } else {
        done as f32 / total as f32
    };
    paint.progress(painter, bar, fraction, Role::Bg3);
    y += 4.0 + s.s8;
    paint.text(
        painter,
        pos2(left, y),
        Align2::LEFT_TOP,
        &t!("relink.searching.folder", folder = folder),
        Type::MonoCaption,
        Role::TextMuted,
        right - left,
    );
}

/// A clip's thumbnail, or the board's offline stand-in: two soft colour glows behind
/// the halftone screen.
fn draw_thumbnail(
    paint: &Paint,
    painter: &Painter,
    rect: Rect,
    texture: Option<egui::TextureId>,
    index: usize,
) {
    let corner = paint.control_radius();
    painter.rect_filled(rect, corner, paint.c(Role::Bg2));
    if let Some(texture) = texture {
        painter.image(
            texture,
            rect,
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            TEXTURE_TINT,
        );
        return;
    }
    let clipped = painter.with_clip_rect(rect);
    // The board's glows sit in an 80 x 52 box 8 pt outside the 64 x 36 thumbnail.
    let at = |fx: f32, fy: f32| rect.min + vec2(-8.0 + 80.0 * fx, -8.0 + 52.0 * fy);
    let (first, second) = if index.is_multiple_of(2) {
        (
            (
                at(0.30, 0.40),
                vec2(40.0, 31.2),
                paint.mix(Role::ClipOffline, 0.7),
            ),
            (
                at(0.75, 0.65),
                vec2(36.0, 28.6),
                paint.mix(Role::ClipVideo, 0.45),
            ),
        )
    } else {
        (
            (
                at(0.65, 0.35),
                vec2(44.0, 26.0),
                paint.mix(Role::ClipOffline, 0.7),
            ),
            (
                at(0.25, 0.70),
                vec2(36.0, 28.6),
                paint.mix(Role::ClipTitle, 0.4),
            ),
        )
    };
    paint.glow(&clipped, first.0, first.1, first.2);
    paint.glow(&clipped, second.0, second.1, second.2);
    paint.halftone(&clipped, rect, 5.0, paint.c(Role::Bg0));
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(&t!("relink.thousands_separator"));
        }
        out.push(c);
    }
    out
}

fn summary_text(view: &RelinkView) -> String {
    let total = view.clips.len();
    let found = view.matched();
    let place = &view.place;
    if view.searching() {
        t!("relink.summary.searching", place = place, count = total)
    } else if place.is_empty() {
        t!("relink.summary.pick_folder")
    } else if found == 0 {
        t!("relink.summary.none", place = place)
    } else if found < total {
        t!(
            "relink.summary.some",
            place = place,
            found = found,
            total = total
        )
    } else if total == 1 {
        t!("relink.summary.one", place = place)
    } else if total == 2 {
        t!("relink.summary.both", place = place)
    } else {
        t!("relink.summary.all", place = place, count = total)
    }
}

fn footer_text(view: &RelinkView) -> String {
    let matched = view.matched();
    let total = view.clips.len();
    let unmatched: Vec<&ClipView> = view
        .clips
        .iter()
        .filter(|c| c.selected().is_none())
        .collect();
    if view.place.is_empty() || view.searching() {
        return String::new();
    }
    if unmatched.is_empty() {
        let folders = if view.folders_searched == 1 {
            t!("relink.footer.one_folder")
        } else {
            t!("relink.footer.folders", count = view.folders_searched)
        };
        t!(
            "relink.footer.matched",
            count = matched,
            folders = folders,
            seconds = format!("{:.1}", view.elapsed_s)
        )
    } else if unmatched.len() == 1 {
        t!(
            "relink.footer.partial_one",
            matched = matched,
            total = total,
            name = unmatched[0].name
        )
    } else {
        t!(
            "relink.footer.partial",
            matched = matched,
            total = total,
            count = unmatched.len()
        )
    }
}

fn card_header_height(paint: &Paint) -> f32 {
    let s = paint.s();
    let lines = paint.line_height(Type::Label) + 2.0 + paint.line_height(Type::Caption);
    s.s8 + THUMB.y.max(lines) + s.s8 + 1.0
}

fn card_height(paint: &Paint, clip: &ClipView) -> f32 {
    let s = paint.s();
    let below = match &clip.state {
        ClipState::Found { candidates, .. } => COLUMN_HEADER + size::ROW * candidates.len() as f32,
        ClipState::Searching { .. } => {
            s.s12
                + paint.line_height(Type::Label)
                + s.s8
                + 4.0
                + s.s8
                + paint.line_height(Type::Caption)
                + s.s12
        }
        ClipState::NoMatch { .. } => {
            s.s12 + (paint.line_height(Type::Label) * 2.0 + 2.0).max(SMALL_BUTTON) + s.s12
        }
    };
    card_header_height(paint) + below
}

fn body_height(paint: &Paint, view: &RelinkView) -> f32 {
    let s = paint.s();
    let option = paint.line_height(Type::Label) + 2.0 + paint.line_height(Type::Caption);
    let cards: f32 = view
        .clips
        .iter()
        .map(|c| card_height(paint, c) + s.s16)
        .sum();
    s.s16 + SMALL_BUTTON + s.s16 + cards + option + s.s16
}
