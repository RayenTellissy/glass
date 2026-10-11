//! `Dialog / About` (Paper, page "10 System & states"), with the app tile and name as the
//! Brand page's About window draws them: the version and build, what this copy is, the
//! licences (MIT source, GPLv3 build, third-party list) and the release's source
//! archives.
//!
//! Three views in one dialog: the main view, a licence notice one level in ("Read"),
//! and the third-party list one level in ("View list"). Escape goes back a level, then
//! closes; Enter closes the main view. Links are returned to the caller to open.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use assets::licences::{self, Component, Kind};
use egui::epaint::text::{LayoutJob, TextFormat, TextWrapping};
use egui::mutex::Mutex;
use egui::{
    Align2, Color32, CornerRadius, Galley, Id, Key, Order, Painter, Pos2, Rect, Sense, Stroke, Ui,
    UiBuilder, Vec2, pos2, vec2,
};

use crate::panels::widgets::{Icon, Paint, Type, icon, size};
use crate::t;
use crate::theme::Role;

/// The dialog's width from the board.
pub const WIDTH: f32 = 480.0;
const HEADER_BUTTON: f32 = 24.0;
const ICON: f32 = 16.0;
const LINK_ICON: f32 = 12.0;
/// The board's icons: a 2.25 stroke on the 24-unit grid (2.5 for the 12 pt link arrow).
const ICON_STROKE: f32 = 2.25;
const LINK_STROKE: f32 = 2.5;
const APP_TILE: f32 = 48.0;
/// Width of the label column in the info and licence cards.
const LABEL_W: f32 = 88.0;
/// Rows of the third-party list in view at once, and the list's footer height.
const LIST_ROWS: f32 = 9.0;
const SUB_FOOTER: f32 = 40.0;
/// Height of the licence notice's scrolling text.
const NOTICE_H: f32 = 320.0;
/// How long "Copied" shows after Copy details.
const COPIED_SECS: f64 = 2.0;

/// What the dialog shows about this copy of the app. The app fills it from its package
/// metadata and build (`app::about`); `None` rows are left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AboutInfo {
    /// The Cargo package version (`0.1.0`).
    pub version: String,
    /// Short git commit and build date (`2026.09.30`) when known at build time.
    pub commit: Option<String>,
    pub build_date: Option<String>,
    /// The package's `repository` URL; the source archives link is derived from it.
    pub repository: String,
    /// Computer, chip and graphics API (`MacBook Pro · Apple M2 · Metal`).
    pub machine: Option<String>,
    /// How this copy is installed (`Standard · in Applications`).
    pub installed: Option<String>,
    /// Where its settings live.
    pub settings_folder: Option<String>,
}

impl AboutInfo {
    pub fn new(version: &str, repository: &str) -> Self {
        Self {
            version: version.to_owned(),
            repository: repository.to_owned(),
            ..Self::default()
        }
    }

    /// `0.1.0 · build 2026.09.30 · a7f3c1e`, leaving out what is unknown.
    pub fn version_line(&self) -> String {
        let version = &self.version;
        match (&self.build_date, &self.commit) {
            (Some(date), Some(commit)) => {
                t!(
                    "about.version.build_commit",
                    version = version,
                    date = date,
                    commit = commit
                )
            }
            (Some(date), None) => t!("about.version.build", version = version, date = date),
            (None, Some(commit)) => t!("about.version.commit", version = version, commit = commit),
            (None, None) => version.clone(),
        }
    }

    /// The release page that carries this version's source archives:
    /// `<repository>/releases/tag/v<version>`.
    pub fn source_archives_url(&self) -> String {
        let repo = self.repository.trim_end_matches('/');
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        format!("{repo}/releases/tag/v{}", self.version)
    }

    /// What Copy details puts on the clipboard, ready to paste into an issue.
    pub fn details(&self) -> String {
        let mut lines = vec![t!("about.details.title", version = self.version_line())];
        lines.extend(self.installed.clone());
        lines.extend(self.machine.clone());
        lines.join("\n")
    }
}

/// Which licence a Read link opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Licence {
    /// The source code's MIT licence.
    Mit,
    /// The GPLv3 terms of the build, with the FFmpeg, x264, x265 and dav1d notices.
    Gpl,
}

/// The dialog's views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Main,
    Licence(Licence),
    ThirdParty,
}

/// What the user asked for. The dialog handles going between views and copying itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AboutAction {
    Close,
    /// Open this URL in the browser.
    OpenUrl(String),
}

/// The dialog's clickable parts, for tests and accessibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Control {
    Close,
    Back,
    Read(Licence),
    ViewList,
    SourceArchives,
    CopyDetails,
    Done,
    /// A row of the third-party list, by index into the dialog's list.
    Component(usize),
    ComponentSource,
    FullLicence,
}

/// What release builds ship, native libraries first (`assets::licences`).
pub fn components() -> &'static [&'static Component] {
    static SHIPPED: OnceLock<Vec<&'static Component>> = OnceLock::new();
    SHIPPED.get_or_init(|| licences::shipped().collect())
}

/// The dialog's state between frames.
#[derive(Clone, Debug)]
pub struct AboutDialog {
    view: View,
    filter: String,
    selected: Option<usize>,
    copied_at: Option<f64>,
    controls: Vec<(Control, Rect)>,
    /// The third-party list: [`components`], or a fixed sample in snapshots.
    list: &'static [&'static Component],
}

impl Default for AboutDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl AboutDialog {
    pub fn new() -> Self {
        Self {
            view: View::Main,
            filter: String::new(),
            selected: None,
            copied_at: None,
            controls: Vec::new(),
            list: components(),
        }
    }

    /// Shows `list` instead of what this build ships, so snapshots stay the same when
    /// dependencies change.
    pub fn with_components(mut self, list: &'static [&'static Component]) -> Self {
        self.list = list;
        self
    }

    /// The third-party list the dialog shows.
    pub fn list(&self) -> &'static [&'static Component] {
        self.list
    }

    pub fn view(&self) -> View {
        self.view
    }

    pub fn set_view(&mut self, view: View) {
        self.view = view;
    }

    /// The expanded row of the third-party list (index into [`AboutDialog::list`]).
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index.filter(|&i| i < self.list.len());
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_owned();
    }

    /// Whether Copy details shows its "Copied" state at time `now` (seconds).
    pub fn shows_copied(&self, now: f64) -> bool {
        self.copied_at.is_some_and(|t| now - t < COPIED_SECS)
    }

    /// Where a control was drawn in the last frame.
    pub fn control_rect(&self, control: Control) -> Option<Rect> {
        self.controls
            .iter()
            .find(|(c, _)| *c == control)
            .map(|(_, r)| *r)
    }

    /// Draws the dialog centred over a dimmed screen that swallows clicks, handles Enter
    /// and Escape, and returns what the user asked for.
    pub fn show(&mut self, ctx: &egui::Context, info: &AboutInfo) -> Option<AboutAction> {
        let paint = Paint::new(ctx);
        let screen = ctx.content_rect();
        let scrim = paint.mix(Role::Bg0, paint.theme.tokens().opacity.glass);
        egui::Area::new(Id::new("glass.about.scrim"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(screen, CornerRadius::ZERO, scrim);
                ui.allocate_rect(screen, Sense::click());
            });
        let height = self.height(ctx, info);
        let origin = (screen.center() - vec2(WIDTH, height) / 2.0).round();
        let mut action = None;
        egui::Area::new(Id::new("glass.about"))
            .order(Order::Foreground)
            .fixed_pos(origin)
            .show(ctx, |ui| {
                action = self.draw(ui, origin, info).0;
            });
        let (enter, escape) =
            ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if action.is_none() {
            match self.view {
                View::Main if enter || escape => action = Some(AboutAction::Close),
                View::Licence(_) | View::ThirdParty if escape => self.view = View::Main,
                _ => {}
            }
        }
        action
    }

    /// The dialog's height in its current view (the main view's text wraps).
    pub fn height(&self, ctx: &egui::Context, info: &AboutInfo) -> f32 {
        let paint = Paint::new(ctx);
        let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
        match self.view {
            View::Main => MainLayout::measure(&paint, &painter, info).total,
            View::Licence(_) => size::DIALOG_HEADER + NOTICE_H + SUB_FOOTER,
            View::ThirdParty => {
                let s = paint.s();
                size::DIALOG_HEADER + size::ROW + s.s8 + s.s4 + list_height() + s.s8 + SUB_FOOTER
            }
        }
    }

    /// Draws the dialog with its top-left at `origin` without scrim or keys (the modal
    /// wrapper is [`AboutDialog::show`]); returns the action and the dialog's rect.
    pub fn draw(
        &mut self,
        ui: &mut Ui,
        origin: Pos2,
        info: &AboutInfo,
    ) -> (Option<AboutAction>, Rect) {
        self.controls.clear();
        let paint = Paint::new(ui.ctx());
        let height = self.height(ui.ctx(), info);
        let rect = Rect::from_min_size(origin, vec2(WIDTH, height));
        // Swallows clicks that land on the dialog but not on a control.
        ui.allocate_rect(rect, Sense::click());
        paint.floating(ui.painter(), rect);
        let action = match self.view {
            View::Main => self.draw_main(ui, &paint, rect, info),
            View::Licence(licence) => self.draw_licence(ui, &paint, rect, licence),
            View::ThirdParty => self.draw_third_party(ui, &paint, rect),
        };
        (action, rect)
    }

    fn control(&mut self, ui: &mut Ui, control: Control, rect: Rect) -> egui::Response {
        self.controls.push((control, rect));
        ui.interact(
            rect,
            Id::new(("glass.about.control", control)),
            Sense::click(),
        )
    }

    /// A 24 pt icon button with a `bg/3` fill on hover.
    fn icon_button(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        control: Control,
        rect: Rect,
        which: Icon,
    ) -> bool {
        let response = self.control(ui, control, rect);
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, paint.control_radius(), paint.c(Role::Bg3));
        }
        icon(
            ui.painter(),
            Rect::from_center_size(rect.center(), Vec2::splat(ICON)),
            which,
            paint.c(Role::TextSecondary),
            ICON_STROKE,
        );
        response.clicked()
    }

    /// The close button at the header's right edge.
    fn close_button(&mut self, ui: &mut Ui, paint: &Paint, header: Rect) -> bool {
        let r = Rect::from_min_size(
            pos2(
                header.max.x - paint.s().s12 - HEADER_BUTTON,
                header.center().y - HEADER_BUTTON / 2.0,
            ),
            Vec2::splat(HEADER_BUTTON),
        );
        self.icon_button(ui, paint, Control::Close, r, Icon::Close)
    }

    /// A text link (`Read`, `View list`): 20 pt high, 8 pt padding, `bg/3` on hover;
    /// its right edge at `right`.
    fn ghost_link(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        control: Control,
        label: &str,
        right: f32,
        mid: f32,
    ) -> bool {
        let painter = ui.painter().clone();
        let s8 = paint.s().s8;
        let w = paint.kept_width(&painter, label, Type::LabelMedium) + 2.0 * s8;
        let r = Rect::from_min_size(
            pos2(right - w, mid - size::SMALL_CONTROL / 2.0),
            vec2(w, size::SMALL_CONTROL),
        );
        let response = self.control(ui, control, r);
        let hovered = response.hovered();
        if hovered {
            painter.rect_filled(r, paint.control_radius(), paint.c(Role::Bg3));
        }
        paint.kept_text(
            &painter,
            r.center(),
            Align2::CENTER_CENTER,
            label,
            Type::LabelMedium,
            if hovered {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            },
            w,
        );
        response.clicked()
    }

    fn draw_main(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        rect: Rect,
        info: &AboutInfo,
    ) -> Option<AboutAction> {
        let painter = ui.painter().clone();
        let s = *paint.s();
        let layout = MainLayout::measure(paint, &painter, info);
        let mut action = None;

        // Header.
        let header = Rect::from_min_size(rect.min, vec2(WIDTH, size::DIALOG_HEADER));
        paint.kept_text(
            &painter,
            pos2(header.min.x + s.s16, header.center().y),
            Align2::LEFT_CENTER,
            &t!("about.title"),
            Type::BodyMedium,
            Role::TextPrimary,
            WIDTH - 2.0 * s.s16 - HEADER_BUTTON,
        );
        if self.close_button(ui, paint, header) {
            action = Some(AboutAction::Close);
        }

        // Identity: the app tile, the name and the version line.
        let left = rect.min.x + s.s16;
        let inner_w = layout.inner_w;
        let mut y = header.max.y;
        let tile = Rect::from_min_size(pos2(left, y), Vec2::splat(APP_TILE));
        crate::brand::app_tile(&painter, tile);
        let col_x = tile.max.x + s.s12;
        let title_h = paint.line_height(Type::Title);
        let col_h = title_h + s.s2 + paint.line_height(Type::MonoCaption);
        let col_y = tile.center().y - col_h / 2.0;
        paint.kept_text(
            &painter,
            pos2(col_x, col_y + title_h / 2.0),
            Align2::LEFT_CENTER,
            &t!("app.name"),
            Type::Title,
            Role::TextPrimary,
            rect.max.x - s.s16 - col_x,
        );
        paint.kept_text(
            &painter,
            pos2(col_x, col_y + title_h + s.s2),
            Align2::LEFT_TOP,
            &info.version_line(),
            Type::MonoCaption,
            Role::TextMuted,
            rect.max.x - s.s16 - col_x,
        );
        y = tile.max.y + s.s16;

        // The promise.
        painter.galley(
            pos2(left, y),
            layout.body.clone(),
            paint.c(Role::TextSecondary),
        );
        y += layout.body.size().y + s.s16;

        // This copy.
        let rows = copy_rows(info);
        if !rows.is_empty() {
            let card = Rect::from_min_size(
                pos2(left, y),
                vec2(inner_w, 2.0 * s.s4 + rows.len() as f32 * size::ROW),
            );
            card_frame(paint, &painter, card);
            let mut row_y = card.min.y + s.s4;
            for (label, value, mono) in &rows {
                let mid = row_y + size::ROW / 2.0;
                row_label(paint, &painter, card.min.x + s.s12, mid, label);
                let x = card.min.x + s.s12 + LABEL_W + s.s8;
                paint.kept_text(
                    &painter,
                    pos2(x, mid),
                    Align2::LEFT_CENTER,
                    value,
                    if *mono {
                        Type::MonoCaption
                    } else {
                        Type::Label
                    },
                    Role::TextSecondary,
                    card.max.x - s.s12 - x,
                );
                row_y += size::ROW;
            }
            y = card.max.y + s.s16;
        }

        // Licences.
        paint.kept_text(
            &painter,
            pos2(left, y + s.s16 / 2.0),
            Align2::LEFT_CENTER,
            &t!("about.licences"),
            Type::Caps,
            Role::TextMuted,
            inner_w,
        );
        y += s.s16 + s.s8;
        let card = Rect::from_min_size(pos2(left, y), vec2(inner_w, 2.0 * s.s4 + 3.0 * size::ROW));
        card_frame(paint, &painter, card);
        let value_x = card.min.x + s.s12 + LABEL_W + s.s8;
        let link_right = card.max.x - s.s4;
        let mut mid = card.min.y + s.s4 + size::ROW / 2.0;

        row_label(
            paint,
            &painter,
            card.min.x + s.s12,
            mid,
            &t!("about.licence.source"),
        );
        paint.kept_text(
            &painter,
            pos2(value_x, mid),
            Align2::LEFT_CENTER,
            &t!("about.licence.mit"),
            Type::Label,
            Role::TextPrimary,
            120.0,
        );
        if self.ghost_link(
            ui,
            paint,
            Control::Read(Licence::Mit),
            &t!("about.read"),
            link_right,
            mid,
        ) {
            self.view = View::Licence(Licence::Mit);
        }
        mid += size::ROW;

        row_label(
            paint,
            &painter,
            card.min.x + s.s12,
            mid,
            &t!("about.licence.build"),
        );
        let gpl = paint.kept_text(
            &painter,
            pos2(value_x, mid),
            Align2::LEFT_CENTER,
            &t!("about.licence.gpl"),
            Type::Label,
            Role::TextPrimary,
            120.0,
        );
        let read = t!("about.read");
        let read_w = paint.kept_width(&painter, &read, Type::LabelMedium) + 2.0 * s.s8;
        paint.kept_text(
            &painter,
            pos2(gpl.max.x + s.s8, mid),
            Align2::LEFT_CENTER,
            &t!("about.licence.gpl_why"),
            Type::Caption,
            Role::TextMuted,
            (link_right - read_w - s.s8 - gpl.max.x - s.s8).max(1.0),
        );
        if self.ghost_link(
            ui,
            paint,
            Control::Read(Licence::Gpl),
            &read,
            link_right,
            mid,
        ) {
            self.view = View::Licence(Licence::Gpl);
        }
        mid += size::ROW;

        row_label(
            paint,
            &painter,
            card.min.x + s.s12,
            mid,
            &t!("about.licence.third_party"),
        );
        let count = self.list.len().to_string();
        let n = paint.kept_text(
            &painter,
            pos2(value_x, mid),
            Align2::LEFT_CENTER,
            &count,
            Type::Mono,
            Role::TextPrimary,
            60.0,
        );
        paint.kept_text(
            &painter,
            pos2(n.max.x + s.s4, mid),
            Align2::LEFT_CENTER,
            &t!("about.components"),
            Type::Label,
            Role::TextPrimary,
            160.0,
        );
        if self.ghost_link(
            ui,
            paint,
            Control::ViewList,
            &t!("about.view_list"),
            link_right,
            mid,
        ) {
            self.view = View::ThirdParty;
        }
        y = card.max.y + s.s8;

        // The release's source archives.
        let link_mid = y + s.s4 + paint.line_height(Type::Label) / 2.0;
        let label = t!("about.source_archives", version = info.version);
        let label_w = paint.kept_width(&painter, &label, Type::LabelMedium);
        let link = Rect::from_min_max(
            pos2(left, y),
            pos2(
                left + ICON + s.s4 + label_w + s.s4 + LINK_ICON,
                y + size::SMALL_CONTROL,
            ),
        );
        let response = self.control(ui, Control::SourceArchives, link);
        let role = if response.hovered() {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        };
        icon(
            &painter,
            Rect::from_min_size(pos2(left, link_mid - ICON / 2.0), Vec2::splat(ICON)),
            Icon::Import,
            paint.c(role),
            ICON_STROKE,
        );
        paint.kept_text(
            &painter,
            pos2(left + ICON + s.s4, link_mid),
            Align2::LEFT_CENTER,
            &label,
            Type::LabelMedium,
            role,
            label_w + 1.0,
        );
        icon(
            &painter,
            Rect::from_min_size(
                pos2(
                    left + ICON + s.s4 + label_w + s.s4,
                    link_mid - LINK_ICON / 2.0,
                ),
                Vec2::splat(LINK_ICON),
            ),
            Icon::ArrowUpRight,
            paint.c(role),
            LINK_STROKE,
        );
        paint.kept_text(
            &painter,
            pos2(link.max.x + s.s4, link_mid),
            Align2::LEFT_CENTER,
            &t!("about.source_archives.contents"),
            Type::MonoCaption,
            Role::TextMuted,
            (rect.max.x - s.s16 - link.max.x - s.s4).max(1.0),
        );
        if response.clicked() {
            action = Some(AboutAction::OpenUrl(info.source_archives_url()));
        }

        // Footer.
        let footer =
            Rect::from_min_max(pos2(rect.min.x, rect.max.y - size::DIALOG_FOOTER), rect.max);
        footer_rule(paint, &painter, footer);
        let fmid = footer.center().y;
        paint.kept_text(
            &painter,
            pos2(footer.min.x + s.s16, fmid),
            Align2::LEFT_CENTER,
            &t!("about.copyright"),
            Type::MonoCaption,
            Role::TextMuted,
            200.0,
        );
        let done = t!("about.done");
        let done_w = paint.kept_width(&painter, &done, Type::LabelMedium) + 2.0 * s.s12;
        let done_r = Rect::from_min_size(
            pos2(
                footer.max.x - s.s16 - done_w,
                fmid - size::DIALOG_BUTTON / 2.0,
            ),
            vec2(done_w, size::DIALOG_BUTTON),
        );
        self.controls.push((Control::Done, done_r));
        if paint.button(ui, done_r, &done, true, true).clicked() {
            action = Some(AboutAction::Close);
        }
        let now = ui.ctx().input(|i| i.time);
        let copied = self.shows_copied(now);
        let copy = if copied {
            t!("about.copied")
        } else {
            t!("about.copy_details")
        };
        let copy_w = paint.kept_width(&painter, &copy, Type::LabelMedium)
            + 2.0 * s.s12
            + if copied { ICON + s.s8 } else { 0.0 };
        let copy_r = Rect::from_min_size(
            pos2(
                done_r.min.x - s.s8 - copy_w,
                fmid - size::DIALOG_BUTTON / 2.0,
            ),
            vec2(copy_w, size::DIALOG_BUTTON),
        );
        let response = self.control(ui, Control::CopyDetails, copy_r);
        let corner = paint.control_radius();
        if copied {
            paint.glass_fill(&painter, copy_r, corner);
            icon(
                &painter,
                Rect::from_min_size(
                    pos2(copy_r.min.x + s.s12, fmid - ICON / 2.0),
                    Vec2::splat(ICON),
                ),
                Icon::Check,
                paint.c(Role::TextPrimary),
                ICON_STROKE,
            );
            let remaining = COPIED_SECS - (now - self.copied_at.unwrap_or(now));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(remaining.max(0.0)));
        } else {
            if response.hovered() {
                painter.rect_filled(copy_r, corner, paint.c(Role::Bg3));
            }
            paint.glass_outline(&painter, copy_r, corner);
        }
        paint.kept_text(
            &painter,
            pos2(copy_r.max.x - s.s12, fmid),
            Align2::RIGHT_CENTER,
            &copy,
            Type::LabelMedium,
            Role::TextPrimary,
            copy_w,
        );
        if response.clicked() {
            ui.ctx().copy_text(info.details());
            self.copied_at = Some(now);
        }
        action
    }

    /// The header of a view one level in: back, title, close. Returns (back, close).
    fn sub_header(&mut self, ui: &mut Ui, paint: &Paint, rect: Rect, title: &str) -> (bool, bool) {
        let s = *paint.s();
        let header = Rect::from_min_size(rect.min, vec2(WIDTH, size::DIALOG_HEADER));
        let back_r = Rect::from_min_size(
            pos2(header.min.x + s.s8, header.center().y - HEADER_BUTTON / 2.0),
            Vec2::splat(HEADER_BUTTON),
        );
        let back = self.icon_button(ui, paint, Control::Back, back_r, Icon::ChevronLeft);
        paint.kept_text(
            ui.painter(),
            pos2(back_r.max.x + s.s4, header.center().y),
            Align2::LEFT_CENTER,
            title,
            Type::BodyMedium,
            Role::TextPrimary,
            WIDTH - 2.0 * HEADER_BUTTON - 3.0 * s.s12,
        );
        let close = self.close_button(ui, paint, header);
        (back, close)
    }

    fn draw_licence(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        rect: Rect,
        licence: Licence,
    ) -> Option<AboutAction> {
        let s = *paint.s();
        let (title, text) = match licence {
            Licence::Mit => (t!("about.licence.mit_title"), t!("about.licence.mit_text")),
            Licence::Gpl => (t!("about.licence.gpl_title"), t!("about.licence.gpl_text")),
        };
        let (back, close) = self.sub_header(ui, paint, rect, &title);
        let mut action = close.then_some(AboutAction::Close);
        if back {
            self.view = View::Main;
        }
        let body = Rect::from_min_size(
            pos2(rect.min.x, rect.min.y + size::DIALOG_HEADER),
            vec2(WIDTH, NOTICE_H),
        );
        let painter = ui.painter().clone();
        let galley = paint.kept_wrapped(&painter, &text, Type::Label, WIDTH - 2.0 * s.s16);
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("glass.about.licence", licence))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (r, _) = ui
                        .allocate_exact_size(vec2(WIDTH, galley.size().y + s.s16), Sense::hover());
                    ui.painter().galley(
                        pos2(r.min.x + s.s16, r.min.y),
                        galley.clone(),
                        paint.c(Role::TextSecondary),
                    );
                });
        });
        let footer = Rect::from_min_max(pos2(rect.min.x, rect.max.y - SUB_FOOTER), rect.max);
        footer_rule(paint, &painter, footer);
        let mid = footer.center().y;
        paint.kept_text(
            &painter,
            pos2(footer.min.x + s.s16, mid),
            Align2::LEFT_CENTER,
            &t!("about.full_texts_ship"),
            Type::MonoCaption,
            Role::TextMuted,
            WIDTH / 2.0,
        );
        if licence == Licence::Gpl {
            let label = t!("about.licence.full_text");
            let label_w = paint.kept_width(&painter, &label, Type::LabelMedium);
            let w = label_w + s.s4 + LINK_ICON + 2.0 * s.s8;
            let r = Rect::from_min_size(
                pos2(footer.max.x - s.s8 - w, mid - size::SMALL_CONTROL / 2.0),
                vec2(w, size::SMALL_CONTROL),
            );
            let response = self.control(ui, Control::FullLicence, r);
            external_link(paint, &painter, r, &label, response.hovered());
            if response.clicked() {
                action = Some(AboutAction::OpenUrl(GPL_URL.to_owned()));
            }
        }
        action
    }

    fn draw_third_party(&mut self, ui: &mut Ui, paint: &Paint, rect: Rect) -> Option<AboutAction> {
        let s = *paint.s();
        let all = self.list;
        let (back, close) = self.sub_header(ui, paint, rect, &t!("about.third_party.title"));
        let mut action = close.then_some(AboutAction::Close);
        if back {
            self.view = View::Main;
        }
        let painter = ui.painter().clone();

        // Filter field.
        let field = Rect::from_min_size(
            pos2(rect.min.x + s.s16, rect.min.y + size::DIALOG_HEADER),
            vec2(WIDTH - 2.0 * s.s16, size::ROW),
        );
        card_frame(paint, &painter, field);
        icon(
            &painter,
            Rect::from_min_size(
                pos2(field.min.x + s.s8, field.center().y - ICON / 2.0),
                Vec2::splat(ICON),
            ),
            Icon::Search,
            paint.c(Role::TextMuted),
            ICON_STROKE,
        );
        let text_rect = Rect::from_min_max(
            pos2(field.min.x + s.s8 + ICON + s.s8, field.min.y),
            pos2(field.max.x - s.s8, field.max.y),
        );
        let hint = egui::RichText::new(t!("about.third_party.filter", count = all.len()))
            .color(paint.c(Role::TextMuted));
        let edit = egui::TextEdit::singleline(&mut self.filter)
            .id(Id::new("glass.about.filter"))
            .frame(egui::Frame::NONE)
            .font(paint.font(Type::Label))
            .text_color(paint.c(Role::TextPrimary))
            .hint_text(hint)
            .vertical_align(egui::Align::Center)
            .desired_width(text_rect.width());
        ui.put(text_rect, edit);

        // The list.
        let needle = self.filter.to_lowercase();
        let shown: Vec<usize> = (0..all.len())
            .filter(|&i| {
                needle.is_empty()
                    || all[i].name.to_lowercase().contains(&needle)
                    || all[i].licence.to_lowercase().contains(&needle)
            })
            .collect();
        let list = Rect::from_min_size(
            pos2(rect.min.x + s.s8, field.max.y + s.s8 + s.s4),
            vec2(WIDTH - 2.0 * s.s8, list_height()),
        );
        let expanded = size::ROW + size::SMALL_CONTROL + s.s8;
        let row_h = |i: usize| {
            if Some(i) == self.selected {
                expanded
            } else {
                size::ROW
            }
        };
        let total: f32 = shown.iter().map(|&i| row_h(i)).sum();
        let mut clicked_row = None;
        let mut open = None;
        let mut rects = Vec::new();
        ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("glass.about.third_party")
                .auto_shrink([false, false])
                .show_viewport(ui, |ui, viewport| {
                    ui.set_height(total);
                    let top = ui.min_rect().min.y;
                    let painter = ui.painter().clone();
                    let mut y = 0.0;
                    for &i in &shown {
                        let h = row_h(i);
                        if y + h >= viewport.min.y && y <= viewport.max.y {
                            let row = Rect::from_min_size(
                                pos2(list.min.x, top + y),
                                vec2(list.width(), h),
                            );
                            let hit =
                                ui.interact(row, Id::new(("glass.about.row", i)), Sense::click());
                            rects.push((Control::Component(i), row));
                            if hit.clicked() {
                                clicked_row = Some(i);
                            }
                            if let Some(source) = draw_component(
                                ui,
                                paint,
                                &painter,
                                row,
                                all[i],
                                Some(i) == self.selected,
                                hit.hovered(),
                            ) {
                                rects.push((Control::ComponentSource, source.0));
                                if source.1 {
                                    open = Some(all[i].repository.to_owned());
                                }
                            }
                        }
                        y += h;
                    }
                });
        });
        self.controls.extend(rects);
        if let Some(url) = open {
            action = Some(AboutAction::OpenUrl(url));
        } else if let Some(i) = clicked_row {
            self.selected = if self.selected == Some(i) {
                None
            } else {
                Some(i)
            };
        }

        let footer = Rect::from_min_max(pos2(rect.min.x, rect.max.y - SUB_FOOTER), rect.max);
        footer_rule(paint, &painter, footer);
        paint.kept_text(
            &painter,
            pos2(footer.min.x + s.s16, footer.center().y),
            Align2::LEFT_CENTER,
            &t!("about.third_party.footer", count = all.len()),
            Type::MonoCaption,
            Role::TextMuted,
            WIDTH - 2.0 * s.s16,
        );
        action
    }
}

const GPL_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";

fn list_height() -> f32 {
    // Nine rows plus room for the expanded row's second line.
    LIST_ROWS * size::ROW + size::ROW
}

/// One row of the third-party list; the selected row adds its description and a Source
/// link. Returns the link's rect and whether it was clicked.
fn draw_component(
    ui: &mut Ui,
    paint: &Paint,
    painter: &Painter,
    row: Rect,
    c: &Component,
    selected: bool,
    hovered: bool,
) -> Option<(Rect, bool)> {
    let s = *paint.s();
    let corner = paint.control_radius();
    if selected {
        paint.glass_fill(painter, row, corner);
    } else if hovered {
        painter.rect_filled(row, corner, paint.c(Role::Bg3));
    }
    let lit = selected || hovered;
    let mid = row.min.y + size::ROW / 2.0;
    let licence = paint.kept_text(
        painter,
        pos2(row.max.x - s.s8, mid),
        Align2::RIGHT_CENTER,
        c.licence,
        Type::MonoCaption,
        if lit {
            Role::TextSecondary
        } else {
            Role::TextMuted
        },
        row.width() / 2.0,
    );
    let name = if selected {
        t!(
            "about.third_party.name_version",
            name = c.name,
            version = c.version
        )
    } else {
        c.name.to_owned()
    };
    paint.kept_text(
        painter,
        pos2(row.min.x + s.s8, mid),
        Align2::LEFT_CENTER,
        &name,
        if selected {
            Type::LabelMedium
        } else {
            Type::Label
        },
        if lit {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        },
        (licence.min.x - s.s8 - row.min.x - s.s8).max(1.0),
    );
    if !selected {
        return None;
    }
    let mid = row.min.y + size::ROW + size::SMALL_CONTROL / 2.0;
    let label = t!("about.third_party.source");
    let label_w = paint.kept_width(painter, &label, Type::LabelMedium);
    let link_w = label_w + s.s4 + LINK_ICON;
    let link = Rect::from_min_size(
        pos2(row.max.x - s.s8 - link_w, mid - size::SMALL_CONTROL / 2.0),
        vec2(link_w, size::SMALL_CONTROL),
    );
    let response = ui.interact(
        link,
        Id::new(("glass.about.source", c.name, c.version)),
        Sense::click(),
    );
    let role = if response.hovered() {
        Role::TextPrimary
    } else {
        Role::TextSecondary
    };
    paint.kept_text(
        painter,
        pos2(link.min.x, mid),
        Align2::LEFT_CENTER,
        &label,
        Type::LabelMedium,
        role,
        label_w + 1.0,
    );
    icon(
        painter,
        Rect::from_min_size(
            pos2(link.max.x - LINK_ICON, mid - LINK_ICON / 2.0),
            Vec2::splat(LINK_ICON),
        ),
        Icon::ArrowUpRight,
        paint.c(role),
        LINK_STROKE,
    );
    paint.kept_text(
        painter,
        pos2(row.min.x + s.s8, mid),
        Align2::LEFT_CENTER,
        &description(c),
        Type::Caption,
        Role::TextSecondary,
        (link.min.x - s.s16 - row.min.x - s.s8).max(1.0),
    );
    Some((link, response.clicked()))
}

/// A crate's own description, or ours for the native libraries.
fn description(c: &Component) -> String {
    if c.kind != Kind::Native {
        return c.description.to_owned();
    }
    match c.name {
        "FFmpeg" => t!("about.third_party.ffmpeg"),
        "x264" => t!("about.third_party.x264"),
        "x265" => t!("about.third_party.x265"),
        "dav1d" => t!("about.third_party.dav1d"),
        "nv-codec-headers" => t!("about.third_party.nv_codec_headers"),
        "zlib" => t!("about.third_party.zlib"),
        _ => String::new(),
    }
}

/// A text link with the outbound arrow, `bg/3` on hover.
fn external_link(paint: &Paint, painter: &Painter, r: Rect, label: &str, hovered: bool) {
    let s = *paint.s();
    if hovered {
        painter.rect_filled(r, paint.control_radius(), paint.c(Role::Bg3));
    }
    let role = if hovered {
        Role::TextPrimary
    } else {
        Role::TextSecondary
    };
    let label_w = paint.kept_width(painter, label, Type::LabelMedium);
    paint.kept_text(
        painter,
        pos2(r.min.x + s.s8, r.center().y),
        Align2::LEFT_CENTER,
        label,
        Type::LabelMedium,
        role,
        label_w + 1.0,
    );
    icon(
        painter,
        Rect::from_min_size(
            pos2(
                r.min.x + s.s8 + label_w + s.s4,
                r.center().y - LINK_ICON / 2.0,
            ),
            Vec2::splat(LINK_ICON),
        ),
        Icon::ArrowUpRight,
        paint.c(role),
        LINK_STROKE,
    );
}

/// The info cards: `bg/1` with a `border/subtle` inset ring.
fn card_frame(paint: &Paint, painter: &Painter, rect: Rect) {
    let corner = paint.control_radius();
    painter.rect_filled(rect, corner, paint.c(Role::Bg1));
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        egui::StrokeKind::Inside,
    );
}

fn row_label(paint: &Paint, painter: &Painter, x: f32, mid: f32, label: &str) {
    paint.kept_text(
        painter,
        pos2(x, mid),
        Align2::LEFT_CENTER,
        label,
        Type::Label,
        Role::TextMuted,
        LABEL_W,
    );
}

fn footer_rule(paint: &Paint, painter: &Painter, footer: Rect) {
    painter.hline(
        footer.x_range().shrink(1.0),
        footer.min.y + 0.5,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
}

/// The dialog's text layouts, kept between frames. egui keeps only the layouts used in
/// the previous frame, so without this every switch between views laid out all of the
/// new view's text again (the licence notices alone took over 3 ms in a debug build).
///
/// Layouts are keyed by their job (text, font, wrap width) and the scale, and laid out
/// in the placeholder colour so one layout serves every colour the text is painted in.
/// They are dropped when egui recreates its fonts (new definitions, text options or a
/// full glyph atlas), which a marker layout that egui must keep alive reveals.
#[derive(Default)]
struct KeptLayouts {
    pass: Option<u64>,
    marker: Option<Arc<Galley>>,
    galleys: HashMap<u64, Arc<Galley>>,
}

/// Enough for every view's text and the third-party rows a scroll through the list shows.
const KEPT_LAYOUTS_MAX: usize = 2048;

impl KeptLayouts {
    fn layout(&mut self, painter: &Painter, job: LayoutJob) -> Arc<Galley> {
        let pass = painter.ctx().cumulative_pass_nr();
        if self.pass != Some(pass) {
            self.pass = Some(pass);
            let marker = painter.layout_no_wrap(
                "glass.about".to_owned(),
                egui::FontId::default(),
                Color32::PLACEHOLDER,
            );
            if !self
                .marker
                .as_ref()
                .is_some_and(|m| Arc::ptr_eq(m, &marker))
            {
                self.galleys.clear();
            }
            self.marker = Some(marker);
        }
        if self.galleys.len() >= KEPT_LAYOUTS_MAX {
            self.galleys.clear();
        }
        let key = egui::util::hash((&job, painter.pixels_per_point().to_bits()));
        self.galleys
            .entry(key)
            .or_insert_with(|| painter.layout_job(job))
            .clone()
    }
}

fn kept_layout(painter: &Painter, job: LayoutJob) -> Arc<Galley> {
    let kept = painter.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Arc<Mutex<KeptLayouts>>>(Id::new("glass.about.layouts"))
            .clone()
    });
    kept.lock().layout(painter, job)
}

/// [`Paint`]'s text helpers with the dialog's kept layouts; the jobs match `Paint`'s.
trait KeptText {
    #[allow(clippy::too_many_arguments)]
    fn kept_text(
        &self,
        painter: &Painter,
        pos: Pos2,
        align: Align2,
        text: &str,
        ty: Type,
        role: Role,
        max_width: f32,
    ) -> Rect;
    fn kept_width(&self, painter: &Painter, text: &str, ty: Type) -> f32;
    /// Wrapped text, to be painted with a colour of its own.
    fn kept_wrapped(&self, painter: &Painter, text: &str, ty: Type, max_width: f32) -> Arc<Galley>;
}

impl KeptText for Paint {
    fn kept_text(
        &self,
        painter: &Painter,
        pos: Pos2,
        align: Align2,
        text: &str,
        ty: Type,
        role: Role,
        max_width: f32,
    ) -> Rect {
        let galley = kept_line(self, painter, text, ty, max_width);
        let rect = align.anchor_size(pos, galley.size());
        painter.galley(rect.min, galley, self.c(role));
        rect
    }

    fn kept_width(&self, painter: &Painter, text: &str, ty: Type) -> f32 {
        kept_line(self, painter, text, ty, f32::INFINITY).size().x
    }

    fn kept_wrapped(&self, painter: &Painter, text: &str, ty: Type, max_width: f32) -> Arc<Galley> {
        let mut job = LayoutJob::single_section(
            text.to_owned(),
            TextFormat::simple(self.font(ty), Color32::PLACEHOLDER),
        );
        job.wrap = TextWrapping::wrap_at_width(max_width.max(1.0));
        kept_layout(painter, job)
    }
}

/// One line cut with an ellipsis at `max_width`, as [`Paint::galley`] lays it out.
fn kept_line(
    paint: &Paint,
    painter: &Painter,
    text: &str,
    ty: Type,
    max_width: f32,
) -> Arc<Galley> {
    let (text, spacing) = if ty == Type::Caps {
        let ts = &paint.theme.tokens().type_scale.caption;
        let em = ts.caps_letter_spacing_em.unwrap_or(0.06);
        (text.to_uppercase(), em * ts.size_px)
    } else {
        (text.to_owned(), 0.0)
    };
    let mut format = TextFormat::simple(paint.font(ty), Color32::PLACEHOLDER);
    format.extra_letter_spacing = spacing;
    let mut job = LayoutJob::single_section(text, format);
    job.wrap = TextWrapping::truncate_at_width(max_width.max(1.0));
    job.break_on_newline = false;
    kept_layout(painter, job)
}

/// The "This copy" rows that are known: label, value, whether the value is a path.
fn copy_rows(info: &AboutInfo) -> Vec<(String, String, bool)> {
    let mut rows = Vec::new();
    if let Some(v) = &info.machine {
        rows.push((t!("about.copy.machine"), v.clone(), false));
    }
    if let Some(v) = &info.installed {
        rows.push((t!("about.copy.installed"), v.clone(), false));
    }
    if let Some(v) = &info.settings_folder {
        rows.push((t!("about.copy.settings"), v.clone(), true));
    }
    rows
}

/// The measured parts of the main view that depend on its text.
struct MainLayout {
    inner_w: f32,
    body: Arc<Galley>,
    total: f32,
}

impl MainLayout {
    fn measure(paint: &Paint, painter: &Painter, info: &AboutInfo) -> Self {
        let s = paint.s();
        let inner_w = WIDTH - 2.0 * s.s16;
        let body = paint.kept_wrapped(painter, &t!("about.promise"), Type::Body, inner_w);
        let rows = copy_rows(info).len();
        let copy_card = if rows == 0 {
            0.0
        } else {
            2.0 * s.s4 + rows as f32 * size::ROW + s.s16
        };
        let licences = s.s16 + s.s8 + 2.0 * s.s4 + 3.0 * size::ROW + s.s8 + size::SMALL_CONTROL;
        let body_h = APP_TILE + s.s16 + body.size().y + s.s16 + copy_card + licences + s.s16;
        Self {
            inner_w,
            total: (size::DIALOG_HEADER + body_h + size::DIALOG_FOOTER).round(),
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_archives_link_follows_the_repository_and_version() {
        let mut info = AboutInfo::new("0.1.0", "https://github.com/OWNER/glass");
        assert_eq!(
            info.source_archives_url(),
            "https://github.com/OWNER/glass/releases/tag/v0.1.0"
        );
        info.repository = "https://github.com/someone/glass.git/".into();
        info.version = "1.4.2".into();
        assert_eq!(
            info.source_archives_url(),
            "https://github.com/someone/glass/releases/tag/v1.4.2"
        );
    }

    #[test]
    fn version_line_leaves_out_what_is_unknown() {
        let mut info = AboutInfo::new("0.1.0", "");
        assert_eq!(info.version_line(), "0.1.0");
        info.commit = Some("a7f3c1e".into());
        assert_eq!(info.version_line(), "0.1.0 · a7f3c1e");
        info.build_date = Some("2026.09.30".into());
        assert_eq!(info.version_line(), "0.1.0 · build 2026.09.30 · a7f3c1e");
        info.machine = Some("MacBook Pro".into());
        assert_eq!(
            info.details(),
            "Glass 0.1.0 · build 2026.09.30 · a7f3c1e\nMacBook Pro"
        );
    }

    #[test]
    fn the_list_starts_with_ffmpeg_and_every_native_library_has_a_description() {
        assert_eq!(components()[0].name, "FFmpeg");
        for c in components().iter().filter(|c| c.kind == Kind::Native) {
            assert!(!description(c).is_empty(), "{}", c.name);
        }
    }
}
