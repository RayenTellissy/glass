//! `Dialog / Recovery` (Paper, page "01 First run"): shown at launch when the app quit
//! unexpectedly and an autosave holds edits the saved file does not.
//!
//! Restore is the default (Enter). Discard asks once more before anything is removed.
//! While restoring, the footer shows progress and a Cancel button. Closing the dialog
//! (the close button or Escape) decides nothing: the autosave stays for next time.

use std::f32::consts::{FRAC_PI_2, TAU};

use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Key, Order, Painter, Pos2, Rect, Sense, Shape,
    Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::panels::widgets::{Icon, Paint, Type, icon, size};
use crate::t;
use crate::theme::{Role, TEXTURE_TINT};

/// The dialog's width from the board.
pub const WIDTH: f32 = 520.0;
const POSTER: Vec2 = vec2(128.0, 72.0);
const ICON: f32 = 16.0;
/// The board's icons: a 2.25 stroke on the 24-unit grid.
const ICON_STROKE: f32 = 2.25;
const PERCENT_W: f32 = 32.0;
/// Gap between the lines of the autosave card's text column.
const CARD_LINE_GAP: f32 = 6.0;

/// What the dialog shows; the app fills it from `project::autosave::RecoveryInfo` and
/// what the autosave does not know (the sequence, a poster frame).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecoveryPrompt {
    pub project_name: String,
    /// The sequence that was open, and its length as the timeline shows it (`7:48`).
    pub sequence_name: String,
    pub sequence_duration: String,
    /// When the autosave was written, as the user reads times.
    pub autosave_time: String,
    /// When the file was last saved (`13:58`); `None` when never saved or unknown.
    pub saved_time: Option<String>,
    /// How long before the app was last alive the autosave was written.
    pub before_crash_ms: u64,
    /// Undo-history changes since the last save.
    pub edits: u64,
    /// A frame of the sequence at the playhead, and its timecode.
    pub poster: Option<egui::TextureId>,
    pub timecode: Option<String>,
}

impl RecoveryPrompt {
    /// The prompt for an autosave the launch scan rebuilt, at wall-clock `now_ms`. Times
    /// read as ages (`12 min ago`) until the app formats local clock times; the saved
    /// time is left out for the same reason. The poster frame is added by the caller once
    /// playback can render one.
    pub fn from_recovered(recovered: &project::autosave::Recovered, now_ms: u64) -> Self {
        let info = &recovered.info;
        let sequence = recovered.project.sequences().front();
        let duration = sequence
            .and_then(|s| {
                s.tracks
                    .iter()
                    .filter_map(|t| t.end().ok())
                    .max()
                    .map(|end| end.as_secs_f64())
            })
            .unwrap_or(0.0);
        Self {
            project_name: info.project_name.clone(),
            sequence_name: sequence
                .map_or_else(|| t!("recovery.untitled_sequence"), |s| s.name.clone()),
            sequence_duration: crate::panels::format::short_clock(duration),
            autosave_time: age(now_ms.saturating_sub(info.autosaved_at_ms)),
            saved_time: None,
            before_crash_ms: info.alive_ms.saturating_sub(info.autosaved_at_ms),
            edits: info.edits_since_save,
            poster: None,
            timecode: None,
        }
    }
}

/// `Moments ago`, `12 min ago`, `3 h ago`, `2 days ago`.
pub fn age(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => t!("recovery.ago.moments"),
        1..=59 => t!("recovery.ago.minutes", count = minutes),
        60..=1439 => t!("recovery.ago.hours", count = minutes / 60),
        _ => t!("recovery.ago.days", count = minutes / 1440),
    }
}

/// Where the dialog is in its flow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RecoveryState {
    Prompt,
    ConfirmDiscard,
    /// Restoring, `progress` from 0 to 1.
    Restoring {
        progress: f32,
    },
}

/// What the user chose. The caller acts on it and closes the dialog, except for
/// [`RecoveryAction::Restore`], after which the dialog shows progress until the caller
/// closes it (feed progress through [`RecoveryDialog::set_progress`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    Restore,
    /// Confirmed: delete the autosave and open the saved file.
    Discard,
    ShowFolder,
    /// Closed without deciding: open the saved file and keep the autosave.
    Later,
    CancelRestore,
}

/// The dialog's clickable parts, for tests and accessibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Control {
    Close,
    ShowFolder,
    Discard,
    Restore,
    Keep,
    ConfirmDiscard,
    Cancel,
}

/// The dialog's state between frames.
#[derive(Clone, Debug)]
pub struct RecoveryDialog {
    state: RecoveryState,
    controls: Vec<(Control, Rect)>,
}

impl Default for RecoveryDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryDialog {
    pub fn new() -> Self {
        Self {
            state: RecoveryState::Prompt,
            controls: Vec::new(),
        }
    }

    pub fn state(&self) -> RecoveryState {
        self.state
    }

    /// Puts the dialog in a state directly (the caller's restore finished or failed).
    pub fn set_state(&mut self, state: RecoveryState) {
        self.state = state;
    }

    pub fn set_progress(&mut self, progress: f32) {
        self.state = RecoveryState::Restoring {
            progress: progress.clamp(0.0, 1.0),
        };
    }

    /// Where a control was drawn in the last frame.
    pub fn control_rect(&self, control: Control) -> Option<Rect> {
        self.controls
            .iter()
            .find(|(c, _)| *c == control)
            .map(|(_, r)| *r)
    }

    /// Draws the dialog centred over a dimmed screen that swallows clicks, handles Enter
    /// and Escape, and returns the user's choice.
    pub fn show(&mut self, ctx: &egui::Context, prompt: &RecoveryPrompt) -> Option<RecoveryAction> {
        let paint = Paint::new(ctx);
        let screen = ctx.content_rect();
        let scrim = paint.mix(Role::Bg0, paint.theme.tokens().opacity.glass);
        egui::Area::new(Id::new("glass.recovery.scrim"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(screen, CornerRadius::ZERO, scrim);
                ui.allocate_rect(screen, Sense::click());
            });
        let height = self.height(ctx, prompt);
        let origin = screen.center() - vec2(WIDTH, height) / 2.0;
        let mut action = None;
        egui::Area::new(Id::new("glass.recovery"))
            .order(Order::Foreground)
            .fixed_pos(origin.round())
            .show(ctx, |ui| {
                action = self.draw(ui, origin.round(), prompt).0;
            });
        let (enter, escape) =
            ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if action.is_none() {
            action = match self.state {
                RecoveryState::Prompt if enter => Some(RecoveryAction::Restore),
                RecoveryState::Prompt if escape => Some(RecoveryAction::Later),
                RecoveryState::ConfirmDiscard if escape => {
                    self.state = RecoveryState::Prompt;
                    None
                }
                _ => None,
            };
        }
        if action == Some(RecoveryAction::Restore) {
            self.state = RecoveryState::Restoring { progress: 0.0 };
        }
        action
    }

    /// The dialog's height for `prompt` (the body text wraps).
    pub fn height(&self, ctx: &egui::Context, prompt: &RecoveryPrompt) -> f32 {
        let paint = Paint::new(ctx);
        let layer = egui::LayerId::background();
        let painter = Painter::new(ctx.clone(), layer, Rect::EVERYTHING);
        Layout::measure(&paint, &painter, prompt).total
    }

    /// Draws the dialog with its top-left at `origin` without scrim or keys (the modal
    /// wrapper is [`RecoveryDialog::show`]); returns the action and the dialog's rect.
    pub fn draw(
        &mut self,
        ui: &mut Ui,
        origin: Pos2,
        prompt: &RecoveryPrompt,
    ) -> (Option<RecoveryAction>, Rect) {
        self.controls.clear();
        let paint = Paint::new(ui.ctx());
        let painter = ui.painter().clone();
        let s = *paint.s();
        let layout = Layout::measure(&paint, &painter, prompt);
        let rect = Rect::from_min_size(origin, vec2(WIDTH, layout.total));
        // Swallows clicks that land on the dialog but not on a control.
        ui.allocate_rect(rect, Sense::click());
        paint.floating(&painter, rect);
        let mut action = None;

        // Header: icon, title, close button.
        let header = Rect::from_min_size(rect.min, vec2(WIDTH, size::DIALOG_HEADER));
        let icon_rect = Rect::from_min_size(
            pos2(header.min.x + s.s16, header.center().y - ICON / 2.0),
            Vec2::splat(ICON),
        );
        icon_reset(&painter, icon_rect, paint.c(Role::TextSecondary));
        let title_font = FontId::new(
            paint.theme.tokens().type_scale.title.size_px,
            crate::fonts::medium(),
        );
        let title =
            painter.layout_no_wrap(t!("recovery.title"), title_font, paint.c(Role::TextPrimary));
        painter.galley(
            pos2(
                icon_rect.max.x + s.s8,
                header.center().y - title.size().y / 2.0,
            ),
            title,
            paint.c(Role::TextPrimary),
        );
        let close = Rect::from_min_size(
            pos2(
                header.max.x - s.s8 - size::DIALOG_BUTTON,
                header.center().y - size::DIALOG_BUTTON / 2.0,
            ),
            Vec2::splat(size::DIALOG_BUTTON),
        );
        let restoring = matches!(self.state, RecoveryState::Restoring { .. });
        let response = self.control(ui, Control::Close, close);
        if response.hovered() && !restoring {
            painter.rect_filled(close, paint.control_radius(), paint.c(Role::Bg3));
        }
        icon(
            &painter,
            Rect::from_center_size(close.center(), Vec2::splat(ICON)),
            Icon::Close,
            paint.c(Role::TextSecondary),
            ICON_STROKE,
        );
        if response.clicked() && !restoring {
            action = Some(RecoveryAction::Later);
        }

        // Body: the message, the autosave card, the note.
        let left = rect.min.x + s.s16;
        let mut y = header.max.y + s.s4;
        painter.galley(
            pos2(left, y),
            layout.body.clone(),
            paint.c(Role::TextPrimary),
        );
        y += layout.body.size().y + s.s16;
        let card = Rect::from_min_size(pos2(left, y), vec2(layout.inner_w, layout.card_h));
        self.draw_card(&paint, &painter, card, prompt, &layout);
        y = card.max.y + s.s16;
        let note_mid = y + layout.note_h / 2.0;
        icon_shield(
            &painter,
            Rect::from_min_size(pos2(left, note_mid - ICON / 2.0), Vec2::splat(ICON)),
            paint.c(Role::TextMuted),
        );
        painter.galley(
            pos2(left + ICON + s.s8, note_mid - layout.note.size().y / 2.0),
            layout.note.clone(),
            paint.c(Role::TextMuted),
        );

        // Footer.
        let footer =
            Rect::from_min_max(pos2(rect.min.x, rect.max.y - size::DIALOG_FOOTER), rect.max);
        painter.hline(
            footer.x_range().shrink(1.0),
            footer.min.y + 0.5,
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        );
        let footer_action = match self.state {
            RecoveryState::Prompt => self.footer_prompt(ui, &paint, footer),
            RecoveryState::ConfirmDiscard => self.footer_confirm(ui, &paint, footer, prompt),
            RecoveryState::Restoring { progress } => {
                self.footer_restoring(ui, &paint, footer, prompt, progress)
            }
        };
        (action.or(footer_action), rect)
    }

    fn control(&mut self, ui: &mut Ui, control: Control, rect: Rect) -> egui::Response {
        self.controls.push((control, rect));
        ui.interact(
            rect,
            Id::new(("glass.recovery.control", control)),
            Sense::click(),
        )
    }

    fn draw_card(
        &self,
        paint: &Paint,
        painter: &Painter,
        card: Rect,
        prompt: &RecoveryPrompt,
        layout: &Layout,
    ) {
        let s = paint.s();
        let corner = paint.control_radius();
        painter.rect_filled(card, corner, paint.c(Role::Bg1));
        painter.rect_stroke(
            card,
            corner,
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
            StrokeKind::Inside,
        );
        let poster = Rect::from_min_size(
            pos2(card.min.x + s.s12, card.center().y - POSTER.y / 2.0),
            POSTER,
        );
        painter.rect_filled(poster, corner, paint.c(Role::Bg0));
        match prompt.poster {
            Some(texture) => {
                painter.image(
                    texture,
                    poster,
                    Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                    TEXTURE_TINT,
                );
            }
            None => icon(
                painter,
                Rect::from_center_size(poster.center(), Vec2::splat(20.0)),
                Icon::Film,
                paint.c(Role::TextMuted),
                ICON_STROKE,
            ),
        }
        if let Some(timecode) = &prompt.timecode {
            paint.badge(
                painter,
                pos2(poster.min.x + s.s4, poster.max.y - s.s4),
                Align2::LEFT_BOTTOM,
                timecode,
                Type::MonoCaption,
            );
        }

        let x = poster.max.x + s.s12;
        let col_w = card.max.x - s.s12 - x;
        let mut y = card.center().y - layout.text_col_h / 2.0;
        let line_h = paint.line_height(Type::Body);
        let title_r = paint.text(
            painter,
            pos2(x, y + line_h / 2.0),
            Align2::LEFT_CENTER,
            &t!("recovery.card.title"),
            Type::BodyMedium,
            Role::TextPrimary,
            col_w,
        );
        paint.text(
            painter,
            pos2(title_r.max.x + s.s8, y + line_h / 2.0),
            Align2::LEFT_CENTER,
            &prompt.autosave_time,
            Type::Mono,
            Role::TextPrimary,
            (col_w - title_r.width() - s.s8).max(1.0),
        );
        y += line_h + CARD_LINE_GAP;
        painter.galley(pos2(x, y), layout.detail.clone(), paint.c(Role::TextMuted));
        y += layout.detail.size().y + CARD_LINE_GAP;
        paint.status_pill(
            painter,
            pos2(x, y + size::SMALL_CONTROL / 2.0),
            &edits_text(prompt),
            Role::StateSuccess,
        );
    }

    fn footer_prompt(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        footer: Rect,
    ) -> Option<RecoveryAction> {
        let s = *paint.s();
        let painter = ui.painter().clone();
        let mid = footer.center().y;
        let mut action = None;

        // Show autosaves folder: ghost button with an icon.
        let label = t!("recovery.show_folder");
        let label_w = paint.width(&painter, &label, Type::Body);
        let folder = Rect::from_min_size(
            pos2(footer.min.x + s.s8, mid - size::DIALOG_BUTTON / 2.0),
            vec2(s.s8 + ICON + s.s8 + label_w + s.s8, size::DIALOG_BUTTON),
        );
        let response = self.control(ui, Control::ShowFolder, folder);
        if response.hovered() {
            painter.rect_filled(folder, paint.control_radius(), paint.c(Role::Bg3));
        }
        icon(
            &painter,
            Rect::from_min_size(
                pos2(folder.min.x + s.s8, mid - ICON / 2.0),
                Vec2::splat(ICON),
            ),
            Icon::Folder,
            paint.c(Role::TextSecondary),
            ICON_STROKE,
        );
        paint.text(
            &painter,
            pos2(folder.min.x + s.s8 + ICON + s.s8, mid),
            Align2::LEFT_CENTER,
            &label,
            Type::Body,
            Role::TextSecondary,
            label_w + 1.0,
        );
        if response.clicked() {
            action = Some(RecoveryAction::ShowFolder);
        }

        // Restore (primary, with the Return key hint), then Discard to its left.
        let restore = t!("recovery.restore");
        let restore_w = paint.width(&painter, &restore, Type::BodyMedium);
        let hint_w = paint.line_height(Type::Caption);
        let w = s.s12 + restore_w + s.s8 + hint_w + s.s12;
        let right = footer.max.x - s.s16;
        let r = Rect::from_min_size(
            pos2(right - w, mid - size::DIALOG_BUTTON / 2.0),
            vec2(w, size::DIALOG_BUTTON),
        );
        let response = self.control(ui, Control::Restore, r);
        primary_fill(paint, &painter, r, Role::Accent, response.hovered());
        paint.text(
            &painter,
            pos2(r.min.x + s.s12, mid),
            Align2::LEFT_CENTER,
            &restore,
            Type::BodyMedium,
            Role::TextInverse,
            restore_w + 1.0,
        );
        icon_return(
            &painter,
            Rect::from_min_size(
                pos2(r.max.x - s.s12 - hint_w, mid - hint_w / 2.0),
                Vec2::splat(hint_w),
            ),
            paint.c(Role::TextInverse).gamma_multiply(0.6),
        );
        if response.clicked() {
            action = Some(RecoveryAction::Restore);
        }
        let discard = self.secondary(
            ui,
            paint,
            Control::Discard,
            &t!("recovery.discard"),
            r.min.x - s.s8,
            mid,
        );
        if discard.clicked() {
            self.state = RecoveryState::ConfirmDiscard;
        }
        action
    }

    fn footer_confirm(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        footer: Rect,
        prompt: &RecoveryPrompt,
    ) -> Option<RecoveryAction> {
        let s = *paint.s();
        let painter = ui.painter().clone();
        let mid = footer.center().y;
        let right = footer.max.x - s.s16;
        let mut action = None;
        let confirm = t!("recovery.discard_edits");
        let confirm_w = paint.width(&painter, &confirm, Type::BodyMedium);
        let r = Rect::from_min_size(
            pos2(
                right - confirm_w - 2.0 * s.s12,
                mid - size::DIALOG_BUTTON / 2.0,
            ),
            vec2(confirm_w + 2.0 * s.s12, size::DIALOG_BUTTON),
        );
        let response = self.control(ui, Control::ConfirmDiscard, r);
        primary_fill(paint, &painter, r, Role::StateError, response.hovered());
        paint.text(
            &painter,
            r.center(),
            Align2::CENTER_CENTER,
            &confirm,
            Type::BodyMedium,
            Role::TextInverse,
            r.width(),
        );
        if response.clicked() {
            action = Some(RecoveryAction::Discard);
        }
        let keep = self.secondary(
            ui,
            paint,
            Control::Keep,
            &t!("recovery.keep"),
            r.min.x - s.s8,
            mid,
        );
        if keep.clicked() {
            self.state = RecoveryState::Prompt;
        }
        let left = footer.min.x + s.s16;
        icon(
            &painter,
            Rect::from_min_size(pos2(left, mid - ICON / 2.0), Vec2::splat(ICON)),
            Icon::Alert,
            paint.c(Role::StateWarning),
            ICON_STROKE,
        );
        let message = match prompt.edits {
            1 => t!("recovery.confirm_discard_one"),
            n => t!("recovery.confirm_discard", count = n),
        };
        paint.text(
            &painter,
            pos2(left + ICON + s.s8, mid),
            Align2::LEFT_CENTER,
            &message,
            Type::Body,
            Role::TextPrimary,
            (keep.rect.min.x - s.s12 - left - ICON - s.s8).max(1.0),
        );
        action
    }

    fn footer_restoring(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        footer: Rect,
        prompt: &RecoveryPrompt,
        progress: f32,
    ) -> Option<RecoveryAction> {
        let s = *paint.s();
        let painter = ui.painter().clone();
        let mid = footer.center().y;
        let cancel = self.secondary(
            ui,
            paint,
            Control::Cancel,
            &t!("common.cancel"),
            footer.max.x - s.s16,
            mid,
        );
        let left = footer.min.x + s.s16;
        icon_spinner(
            paint,
            &painter,
            Rect::from_min_size(pos2(left, mid - ICON / 2.0), Vec2::splat(ICON)),
        );
        let label_x = left + ICON + s.s12;
        let percent_right = cancel.rect.min.x - s.s12;
        let bar_right = percent_right - PERCENT_W - s.s12;
        let label = paint.text(
            &painter,
            pos2(label_x, mid),
            Align2::LEFT_CENTER,
            &t!("recovery.restoring", project = prompt.project_name),
            Type::Body,
            Role::TextPrimary,
            (bar_right - label_x - s.s12 - 48.0).max(1.0),
        );
        let bar = Rect::from_min_max(
            pos2(label.max.x + s.s12, mid - 2.0),
            pos2(bar_right.max(label.max.x + s.s12), mid + 2.0),
        );
        paint.progress(&painter, bar, progress, Role::Bg3);
        paint.text(
            &painter,
            pos2(percent_right, mid),
            Align2::RIGHT_CENTER,
            &t!(
                "recovery.percent",
                percent = (progress * 100.0).round() as u32
            ),
            Type::MonoCaption,
            Role::TextSecondary,
            PERCENT_W,
        );
        cancel.clicked().then_some(RecoveryAction::CancelRestore)
    }

    /// A secondary (glass) button with body-size medium text whose right edge is at
    /// `right`, centred on `mid`.
    fn secondary(
        &mut self,
        ui: &mut Ui,
        paint: &Paint,
        control: Control,
        label: &str,
        right: f32,
        mid: f32,
    ) -> egui::Response {
        let painter = ui.painter().clone();
        let s12 = paint.s().s12;
        let w = paint.width(&painter, label, Type::BodyMedium) + 2.0 * s12;
        let r = Rect::from_min_size(
            pos2(right - w, mid - size::DIALOG_BUTTON / 2.0),
            vec2(w, size::DIALOG_BUTTON),
        );
        let response = self.control(ui, control, r);
        if response.hovered() {
            painter.rect_filled(r, paint.control_radius(), paint.c(Role::Bg3));
        }
        paint.glass_outline(&painter, r, paint.control_radius());
        paint.text(
            &painter,
            r.center(),
            Align2::CENTER_CENTER,
            label,
            Type::BodyMedium,
            Role::TextPrimary,
            r.width(),
        );
        response
    }
}

/// The measured parts of the dialog that depend on its text.
struct Layout {
    inner_w: f32,
    body: std::sync::Arc<egui::Galley>,
    detail: std::sync::Arc<egui::Galley>,
    note: std::sync::Arc<egui::Galley>,
    text_col_h: f32,
    card_h: f32,
    note_h: f32,
    total: f32,
}

impl Layout {
    fn measure(paint: &Paint, painter: &Painter, prompt: &RecoveryPrompt) -> Self {
        let s = paint.s();
        let inner_w = WIDTH - 2.0 * s.s16;
        let body = paint.wrapped(
            painter,
            &body_text(prompt),
            Type::Body,
            paint.c(Role::TextPrimary),
            inner_w,
            false,
        );
        let col_w = inner_w - 2.0 * s.s12 - POSTER.x - s.s12;
        let detail = paint.wrapped(
            painter,
            &detail_text(prompt),
            Type::MonoCaption,
            paint.c(Role::TextMuted),
            col_w,
            false,
        );
        let note = paint.wrapped(
            painter,
            &t!("recovery.note"),
            Type::Label,
            paint.c(Role::TextMuted),
            inner_w - ICON - s.s8,
            false,
        );
        let text_col_h = paint.line_height(Type::Body)
            + CARD_LINE_GAP
            + detail.size().y
            + CARD_LINE_GAP
            + size::SMALL_CONTROL;
        let card_h = 2.0 * s.s12 + POSTER.y.max(text_col_h);
        let note_h = note.size().y.max(ICON);
        let body_h = s.s4 + body.size().y + s.s16 + card_h + s.s16 + note_h + s.s16;
        Self {
            inner_w,
            total: (size::DIALOG_HEADER + body_h + size::DIALOG_FOOTER).round(),
            body,
            detail,
            note,
            text_col_h,
            card_h,
            note_h,
        }
    }
}

fn body_text(p: &RecoveryPrompt) -> String {
    match p.before_crash_ms / 60_000 {
        0 => t!("recovery.body.moments", project = p.project_name),
        1 => t!("recovery.body.one_minute", project = p.project_name),
        m => t!(
            "recovery.body.minutes",
            project = p.project_name,
            minutes = m
        ),
    }
}

fn detail_text(p: &RecoveryPrompt) -> String {
    match p.before_crash_ms / 60_000 {
        0 => t!(
            "recovery.card.detail_moments",
            sequence = p.sequence_name,
            duration = p.sequence_duration
        ),
        m => t!(
            "recovery.card.detail",
            sequence = p.sequence_name,
            duration = p.sequence_duration,
            minutes = m
        ),
    }
}

fn edits_text(p: &RecoveryPrompt) -> String {
    match (&p.saved_time, p.edits) {
        (Some(time), 1) => t!("recovery.card.one_edit_since_save", time = time),
        (Some(time), n) => t!("recovery.card.edits_since_save", count = n, time = time),
        (None, 1) => t!("recovery.card.one_edit_unsaved"),
        (None, n) => t!("recovery.card.edits_unsaved", count = n),
    }
}

/// A filled button background in `role`, lightened on hover.
fn primary_fill(paint: &Paint, painter: &Painter, rect: Rect, role: Role, hovered: bool) {
    let corner = paint.control_radius();
    painter.rect_filled(rect, corner, paint.c(role));
    if hovered {
        painter.rect_filled(rect, corner, paint.c(Role::GlassHighlight));
    }
}

/// Maps the board's 24-unit icon grid onto `rect`.
fn grid(rect: Rect) -> impl Fn(f32, f32) -> Pos2 {
    let k = rect.width() / 24.0;
    move |x, y| pos2(rect.min.x + x * k, rect.min.y + y * k)
}

fn stroke(rect: Rect, width_24: f32, color: Color32) -> Stroke {
    Stroke::new(width_24 * rect.width() / 24.0, color)
}

/// Points on a circle of the grid from angle `a0` to `a1` (radians, y down).
fn arc(g: &impl Fn(f32, f32) -> Pos2, c: (f32, f32), r: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    const STEPS: usize = 24;
    (0..=STEPS)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / STEPS as f32;
            g(c.0 + r * a.cos(), c.1 + r * a.sin())
        })
        .collect()
}

/// `Icon / reset`: a counter-clockwise arrow round a circle.
fn icon_reset(painter: &Painter, rect: Rect, color: Color32) {
    let g = grid(rect);
    let st = stroke(rect, ICON_STROKE, color);
    let mut points = arc(&g, (12.0, 12.0), 9.0, std::f32::consts::PI, -FRAC_PI_2);
    points.extend([g(8.5, 3.7), g(5.26, 5.74), g(3.0, 8.0)]);
    painter.add(Shape::line(points, st));
    painter.add(Shape::line(vec![g(3.0, 3.0), g(3.0, 8.0), g(8.0, 8.0)], st));
}

/// `Icon / shield`.
fn icon_shield(painter: &Painter, rect: Rect, color: Color32) {
    let g = grid(rect);
    let points = [
        (20.0, 13.0),
        (18.0, 17.5),
        (12.34, 21.95),
        (11.67, 21.94),
        (6.0, 17.5),
        (4.0, 13.0),
        (4.0, 6.0),
        (5.0, 5.0),
        (11.24, 2.28),
        (12.76, 2.28),
        (19.0, 5.0),
        (20.0, 6.0),
    ];
    painter.add(Shape::closed_line(
        points.iter().map(|&(x, y)| g(x, y)).collect(),
        stroke(rect, ICON_STROKE, color),
    ));
}

/// The Return key hint drawn as a line icon (the mono font may lack the glyph).
fn icon_return(painter: &Painter, rect: Rect, color: Color32) {
    let g = grid(rect);
    let st = stroke(rect, ICON_STROKE, color);
    painter.add(Shape::line(
        vec![g(20.0, 5.0), g(20.0, 12.0), g(18.0, 14.0), g(5.0, 14.0)],
        st,
    ));
    painter.add(Shape::line(
        vec![g(9.0, 10.0), g(5.0, 14.0), g(9.0, 18.0)],
        st,
    ));
}

/// The board's spinner: a `bg/3` ring with a quarter arc in the accent that turns with
/// time while restoring.
fn icon_spinner(paint: &Paint, painter: &Painter, rect: Rect) {
    let g = grid(rect);
    let width = 3.0 * rect.width() / 24.0;
    painter.circle_stroke(
        rect.center(),
        9.0 * rect.width() / 24.0,
        Stroke::new(width, paint.c(Role::Bg3)),
    );
    let turn = (painter.ctx().input(|i| i.time) as f32 * TAU) % TAU;
    painter.add(Shape::line(
        arc(&g, (12.0, 12.0), 9.0, turn - FRAC_PI_2, turn),
        Stroke::new(width, paint.c(Role::Accent)),
    ));
    painter.ctx().request_repaint();
}
