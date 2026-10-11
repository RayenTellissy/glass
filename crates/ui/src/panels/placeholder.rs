//! The panel a workspace shows for a panel that is not built yet (scopes, colour
//! wheels, the mixer, the effects browser), so the Colour, Audio and Effects presets
//! have their final shape: the panel's name and a line saying it comes later.

use egui::{Align2, Rect, Ui, pos2};

use super::widgets::{Paint, Type};
use crate::t;
use crate::theme::Role;

/// Draws the placeholder for the panel called `name` into `rect`.
pub fn placeholder_panel(ui: &mut Ui, rect: Rect, name: &str) {
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, paint.c(Role::Bg1));
    let gap = paint.s().s4;
    let centre = rect.center();
    paint.text(
        &painter,
        pos2(centre.x, centre.y - gap / 2.0),
        Align2::CENTER_BOTTOM,
        name,
        Type::BodyMedium,
        Role::TextSecondary,
        rect.width() - 2.0 * paint.s().s16,
    );
    paint.text(
        &painter,
        pos2(centre.x, centre.y + gap / 2.0),
        Align2::CENTER_TOP,
        &t!("workspace.placeholder"),
        Type::Label,
        Role::TextMuted,
        rect.width() - 2.0 * paint.s().s16,
    );
}
