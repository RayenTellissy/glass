//! The inspector panel. Until clip selection exists it shows its empty state; then it
//! will list the selected clip's sections, each drawn by [`crate::params::Section`]
//! from the effect's parameter schema.

use egui::{Align2, Rect, Ui, pos2};

use super::widgets::{Paint, Type, size};
use crate::t;
use crate::theme::Role;

/// Draws the inspector into `rect`: a section header and the empty state.
pub fn inspector_panel(ui: &mut Ui, rect: Rect) {
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, paint.c(Role::Bg1));
    let pad = paint.s().s12;
    let header = Rect::from_min_size(rect.min, egui::vec2(rect.width(), size::ROW));
    paint.text(
        &painter,
        pos2(header.left() + pad, header.center().y),
        Align2::LEFT_CENTER,
        &t!("inspector.title"),
        Type::Caps,
        Role::TextMuted,
        header.width() - 2.0 * pad,
    );
    painter.hline(
        header.x_range(),
        header.bottom() - 0.5,
        egui::Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
    let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
    let galley = paint.wrapped(
        &painter,
        &t!("inspector.empty"),
        Type::Label,
        paint.c(Role::TextMuted),
        body.width() - 4.0 * pad,
        true,
    );
    let at = pos2(
        body.center().x - galley.size().x / 2.0,
        body.center().y - galley.size().y / 2.0,
    );
    painter.galley(at, galley, paint.c(Role::TextMuted));
}
