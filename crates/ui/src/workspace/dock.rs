//! Drawing a [`Workspace`] with `egui_dock` in the theme's colours.
//!
//! Tabs drag to another tab bar to join it, or onto a panel's edge to split it; a tab's
//! close button closes the panel (the Window menu restores it); the bars between
//! splits drag to resize. Panels stay in the main window: undocking into floating
//! windows and the tab context menus (whose text is not translated) are off.

use egui::{CornerRadius, Id, Margin, Rect, Sense, Stroke, Ui, WidgetText};
use egui_dock::{AllowedSplits, DockArea, OverlayType, Style, TabViewer};

use super::{PanelKind, Workspace};
use crate::panels::widgets::{Paint, Type, size};
use crate::theme::Role;

/// Draws the panels; the app implements it by calling the panel functions.
pub trait PanelHost {
    /// Draws `panel` into `rect`, the body of its tab.
    fn show(&mut self, ui: &mut Ui, rect: Rect, panel: PanelKind);
}

/// The dock style from the current theme: tab bars on `bg/0`, the active tab and panel
/// bodies on `bg/1`, `border/subtle` splits that turn `border/strong` on hover and the
/// accent while dragged, and the accent for the drop overlay.
pub fn style(ui: &Ui) -> Style {
    let paint = Paint::new(ui.ctx());
    let c = |role| paint.c(role);
    let corner = paint.control_radius();
    let mut style = Style::from_egui(ui.style());
    style.dock_area_padding = None;
    style.main_surface_border_stroke = Stroke::NONE;
    style.main_surface_border_rounding = CornerRadius::ZERO;

    style.separator.width = 1.0;
    style.separator.extra_interact_width = paint.s().s4;
    style.separator.color_idle = c(Role::BorderSubtle);
    style.separator.color_hovered = c(Role::BorderStrong);
    style.separator.color_dragged = c(Role::Accent);

    style.tab_bar.bg_fill = c(Role::Bg0);
    style.tab_bar.height = size::ROW;
    style.tab_bar.hline_color = c(Role::BorderSubtle);
    style.tab_bar.corner_radius = CornerRadius::ZERO;
    style.tab_bar.fill_tab_bar = false;

    let tab = |bg: Role, text: Role, outline: Role| egui_dock::TabInteractionStyle {
        outline_color: c(outline),
        corner_radius: CornerRadius {
            nw: corner.nw,
            ne: corner.ne,
            sw: 0,
            se: 0,
        },
        bg_fill: c(bg),
        text_color: c(text),
    };
    style.tab.active = tab(Role::Bg1, Role::TextPrimary, Role::BorderSubtle);
    style.tab.focused = tab(Role::Bg1, Role::TextPrimary, Role::BorderSubtle);
    style.tab.inactive = tab(Role::Bg0, Role::TextSecondary, Role::Bg0);
    style.tab.hovered = tab(Role::Bg2, Role::TextPrimary, Role::Bg2);
    style.tab.active_with_kb_focus = tab(Role::Bg1, Role::TextPrimary, Role::Accent);
    style.tab.focused_with_kb_focus = tab(Role::Bg1, Role::TextPrimary, Role::Accent);
    style.tab.inactive_with_kb_focus = tab(Role::Bg0, Role::TextPrimary, Role::Accent);
    style.tab.hline_below_active_tab_name = false;
    style.tab.tab_body.bg_fill = c(Role::Bg1);
    style.tab.tab_body.inner_margin = Margin::ZERO;
    style.tab.tab_body.stroke = Stroke::NONE;
    style.tab.tab_body.corner_radius = CornerRadius::ZERO;

    style.buttons.close_tab_color = c(Role::TextMuted);
    style.buttons.close_tab_active_color = c(Role::TextPrimary);
    style.buttons.close_tab_bg_fill = c(Role::Bg3);

    style.overlay.selection_color = c(Role::AccentSoft);
    style.overlay.button_color = c(Role::Bg3);
    style.overlay.button_border_stroke = Stroke::new(1.0, c(Role::BorderStrong));
    style.overlay.overlay_type = OverlayType::Widgets;
    style.overlay.hovered_leaf_highlight.color = c(Role::AccentSoft);
    style.overlay.hovered_leaf_highlight.stroke = Stroke::new(1.0, c(Role::Accent));
    style.overlay.hovered_leaf_highlight.corner_radius = corner;
    style
}

struct Viewer<'a, H> {
    host: &'a mut H,
}

impl<H: PanelHost> TabViewer for Viewer<'_, H> {
    type Tab = PanelKind;

    fn id(&mut self, tab: &mut PanelKind) -> Id {
        Id::new(("glass.workspace.panel", *tab))
    }

    fn title(&mut self, tab: &mut PanelKind) -> WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut PanelKind) {
        let rect = ui.available_rect_before_wrap();
        self.host.show(ui, rect, *tab);
        // The panels paint and sense their own parts; this only claims the space.
        ui.allocate_rect(rect, Sense::hover());
    }

    fn allowed_in_windows(&self, _tab: &mut PanelKind) -> bool {
        false
    }

    fn scroll_bars(&self, _tab: &PanelKind) -> [bool; 2] {
        [false, false]
    }
}

/// Draws `workspace` filling `ui`, each panel through `host`. `id` keeps the dock's
/// drag state apart when more than one is drawn (snapshots).
pub fn show(ui: &mut Ui, id: Id, workspace: &mut Workspace, host: &mut impl PanelHost) {
    let style = style(ui);
    DockArea::new(&mut workspace.dock)
        .id(id)
        .style(style)
        .show_add_buttons(false)
        .show_close_buttons(true)
        .tab_context_menus(false)
        .draggable_tabs(true)
        .show_leaf_close_all_buttons(false)
        .show_leaf_collapse_buttons(false)
        .allowed_splits(AllowedSplits::All)
        .show_inside(ui, &mut Viewer { host });
}

/// The empty window when every panel is closed: a line pointing at the Window menu.
pub fn empty(ui: &mut Ui, rect: Rect) {
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, paint.c(Role::Bg0));
    paint.text(
        &painter,
        rect.center(),
        egui::Align2::CENTER_CENTER,
        &crate::t!("workspace.empty"),
        Type::Body,
        Role::TextMuted,
        rect.width(),
    );
}
