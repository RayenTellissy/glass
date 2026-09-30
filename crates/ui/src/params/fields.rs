//! The interactive fields of one parameter row: scrub-and-type numbers, the slider,
//! stepper, switch, dropdown, colour and text fields, the keyframe lane and the reset
//! button. Transient state (a scrub in progress, the text being typed, a range error)
//! lives in egui's temporary memory under the row's id; values only leave as events.

use egui::{Align, CursorIcon, Id, Key, LayerId, Order, Painter, Rect, Sense, Ui, pos2, vec2};
use project::params::{ChoiceOption, ParamDef, ParamKind, ParamValue, Range};

use super::paint::{self, FieldState, KeyframeInfo, KeyframeMark, SliderState, StepperHover};
use super::{LABEL_WIDTH, ParamEvent, evaluate};
use crate::panels::widgets::{Icon, Paint, Type, icon};
use crate::t;
use crate::theme::Role;

/// Pointer travel, in points, that scrubs across a parameter's soft range.
const SCRUB_SPAN_POINTS: f64 = 400.0;
/// Scrub speed with Shift held.
const FINE_FACTOR: f64 = 0.1;

pub(super) fn icon_colour(paint: &Paint, hovered: bool) -> egui::Color32 {
    paint.c(if hovered {
        Role::TextPrimary
    } else {
        Role::TextMuted
    })
}

/// Which number of a parameter a field edits.
#[derive(Clone, Copy, Debug)]
enum Component {
    Single,
    X([f64; 2]),
    Y([f64; 2]),
}

#[derive(Clone, Debug)]
struct Scrub {
    start: f64,
    moved: f64,
}

#[derive(Clone, Debug)]
struct Editing {
    text: String,
    focused: bool,
}

/// A typed value outside the hard range, kept on screen until the next edit.
#[derive(Clone, Debug)]
pub(super) struct FieldError {
    pub min: String,
    pub max: String,
    typed: String,
    component: u8,
}

pub(super) struct FieldCx<'a> {
    pub paint: &'a Paint,
    /// The row's painter, at the disabled opacity when the row is disabled.
    pub painter: &'a Painter,
    pub id: Id,
    pub def: &'a ParamDef,
    pub enabled: bool,
}

impl FieldCx<'_> {
    fn sense(&self, sense: Sense) -> Sense {
        if self.enabled { sense } else { Sense::hover() }
    }

    fn field_height(&self) -> f32 {
        self.paint.theme.tokens().sizes.control_pro
    }

    fn field_rect(&self, lane: Rect) -> Rect {
        Rect::from_center_size(lane.center(), vec2(lane.width(), self.field_height()))
    }

    pub fn error(&self, ui: &Ui) -> Option<FieldError> {
        ui.data(|d| d.get_temp::<FieldError>(self.id))
    }

    /// The fields for the parameter's kind in the `fields` lane.
    pub fn fields(
        &self,
        ui: &mut Ui,
        fields: Rect,
        value: &ParamValue,
        events: &mut Vec<ParamEvent>,
    ) {
        let gap = self.paint.s().s8;
        match (&self.def.kind, value) {
            (ParamKind::Float { .. } | ParamKind::Angle { .. }, _) => {
                let current = value.as_f64().unwrap_or_default();
                let soft = self.def.kind.soft_range().filter(Range::is_bounded);
                let field = match soft {
                    Some(soft) if self.def.slider => {
                        let slider = Rect::from_min_max(
                            fields.min,
                            pos2(fields.right() - LABEL_WIDTH - gap, fields.bottom()),
                        );
                        self.slider(ui, slider, soft, current, events);
                        Rect::from_min_max(
                            pos2(fields.right() - LABEL_WIDTH, fields.top()),
                            fields.max,
                        )
                    }
                    _ => fields,
                };
                self.number(
                    ui,
                    self.field_rect(field),
                    Component::Single,
                    current,
                    events,
                );
            }
            (ParamKind::Int { .. }, ParamValue::Int(v)) => {
                let width = paint::STEPPER_WIDTH.min(fields.width());
                let rect = Rect::from_min_size(
                    pos2(fields.left(), fields.center().y - self.field_height() / 2.0),
                    vec2(width, self.field_height()),
                );
                self.stepper(ui, rect, *v, events);
            }
            (ParamKind::Point { .. }, ParamValue::Point(p)) => {
                let half = (fields.width() - gap) / 2.0;
                let x = Rect::from_min_size(fields.min, vec2(half, fields.height()));
                let y = Rect::from_min_size(
                    pos2(fields.right() - half, fields.top()),
                    vec2(half, fields.height()),
                );
                self.number(ui, self.field_rect(x), Component::X(*p), p[0], events);
                self.number(ui, self.field_rect(y), Component::Y(*p), p[1], events);
            }
            (ParamKind::Bool { .. }, ParamValue::Bool(on)) => {
                let rect = Rect::from_min_size(
                    pos2(fields.left(), fields.center().y - 7.0),
                    vec2(24.0, 14.0),
                );
                let response = ui.interact(rect, self.id, self.sense(Sense::click()));
                paint::switch(
                    self.paint,
                    self.painter,
                    rect,
                    *on,
                    self.enabled && response.hovered(),
                );
                if self.enabled && response.clicked() {
                    self.set(events, ParamValue::Bool(!on), true);
                }
            }
            (ParamKind::Choice { options, .. }, ParamValue::Choice(current)) => {
                self.choice(ui, self.field_rect(fields), options, current, events);
            }
            (ParamKind::Colour { alpha, .. }, ParamValue::Colour(c)) => {
                self.colour(ui, self.field_rect(fields), *c, *alpha, events);
            }
            (ParamKind::Text { multiline, .. }, ParamValue::Text(s)) => {
                self.text(ui, self.field_rect(fields), s, *multiline, events);
            }
            _ => {}
        }
    }

    /// Stored units per point of pointer travel.
    fn scrub_step(&self) -> f64 {
        match self.def.kind.soft_range().filter(Range::is_bounded) {
            Some(r) if r.max > r.min => (r.max - r.min) / SCRUB_SPAN_POINTS,
            _ if matches!(self.def.kind, ParamKind::Int { .. }) => 0.1,
            _ => 1.0,
        }
    }

    fn value_for(&self, component: Component, v: f64) -> ParamValue {
        match (component, &self.def.kind) {
            (Component::X(p), _) => ParamValue::Point([v, p[1]]),
            (Component::Y(p), _) => ParamValue::Point([p[0], v]),
            (_, ParamKind::Int { .. }) => ParamValue::Int(v.round() as i64),
            (_, ParamKind::Angle { .. }) => ParamValue::Angle(v),
            _ => ParamValue::Float(v),
        }
    }

    fn set(&self, events: &mut Vec<ParamEvent>, value: ParamValue, commit: bool) {
        events.push(ParamEvent::Set {
            id: self.def.id.clone(),
            value: self.def.sanitise(value),
            commit,
        });
    }

    fn component_id(&self, component: Component) -> (Id, u8) {
        match component {
            Component::Single => (self.id, 0),
            Component::X(_) => (self.id.with("x"), 1),
            Component::Y(_) => (self.id.with("y"), 2),
        }
    }

    fn unit_label(&self, component: Component) -> String {
        match component {
            Component::Single => self.def.unit.symbol().to_owned(),
            Component::X(_) => t!("params.axis.x"),
            Component::Y(_) => t!("params.axis.y"),
        }
    }

    /// Commits typed text: in range sets the value, outside the hard range records an
    /// error and keeps the old value, unparsable text is dropped.
    fn commit_typed(
        &self,
        ui: &Ui,
        component: Component,
        code: u8,
        text: &str,
        current: f64,
        events: &mut Vec<ParamEvent>,
    ) {
        let unit = self.def.unit;
        let Some(v) = evaluate(text, unit.to_display(current)).map(|v| unit.from_display(v)) else {
            return;
        };
        let range = self.def.kind.range().unwrap_or(Range::UNBOUNDED);
        if range.contains(v) {
            ui.data_mut(|d| d.remove::<FieldError>(self.id));
            self.set(events, self.value_for(component, v), true);
        } else {
            let show = |v: f64| {
                let v = unit.to_display(v);
                if v.fract() == 0.0 {
                    format!("{v:.0}")
                } else {
                    format!("{v}")
                }
            };
            let error = FieldError {
                min: show(range.min),
                max: show(range.max),
                typed: text.trim().to_owned(),
                component: code,
            };
            ui.data_mut(|d| d.insert_temp(self.id, error));
        }
    }

    /// A number field: drag to scrub, click to type.
    fn number(
        &self,
        ui: &mut Ui,
        rect: Rect,
        component: Component,
        current: f64,
        events: &mut Vec<ParamEvent>,
    ) {
        let (id, code) = self.component_id(component);
        if self.editing(ui, id, rect, component, code, current, events) {
            return;
        }
        let response = ui.interact(rect, id, self.sense(Sense::click_and_drag()));
        let scrub = ui.data(|d| d.get_temp::<Scrub>(id));
        let error = self.error(ui).filter(|e| e.component == code);
        let hot = self.enabled && (response.hovered() || scrub.is_some());
        if hot {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        let state = match (&error, &scrub, hot) {
            (Some(_), _, _) => FieldState::Error,
            (None, Some(_), _) => FieldState::Scrub,
            (None, None, true) => FieldState::Hover,
            (None, None, false) => FieldState::Idle,
        };
        paint::field_frame(self.paint, self.painter, rect, state);
        let text = error.map_or_else(|| self.def.format(current), |e| e.typed);
        paint::field_text(
            self.paint,
            self.painter,
            rect,
            &self.unit_label(component),
            &text,
        );
        if self.enabled {
            self.scrub_or_click(ui, &response, id, rect, component, current, scrub, events);
        }
    }

    /// The typing state of a number field; true while it is being edited (it then drew
    /// itself).
    #[allow(clippy::too_many_arguments)]
    fn editing(
        &self,
        ui: &mut Ui,
        id: Id,
        rect: Rect,
        component: Component,
        code: u8,
        current: f64,
        events: &mut Vec<ParamEvent>,
    ) -> bool {
        let Some(mut editing) = ui.data(|d| d.get_temp::<Editing>(id)) else {
            return false;
        };
        paint::field_frame(self.paint, self.painter, rect, FieldState::Editing);
        let pad = self.paint.s().s8;
        let unit = self.unit_label(component);
        let unit_w = self.paint.width(self.painter, &unit, Type::MonoCaption);
        paint::field_text(self.paint, self.painter, rect, &unit, "");
        let edit_rect = Rect::from_min_max(
            pos2(rect.left() + pad + unit_w + self.paint.s().s4, rect.top()),
            pos2(rect.right() - pad, rect.bottom()),
        );
        let edit = egui::TextEdit::singleline(&mut editing.text)
            .font(self.paint.font(Type::Mono))
            .frame(egui::Frame::NONE)
            .horizontal_align(Align::RIGHT)
            .vertical_align(Align::Center)
            .text_color(self.paint.c(Role::TextPrimary))
            .margin(egui::Margin::ZERO)
            .desired_width(edit_rect.width());
        let response = ui.put(edit_rect, edit);
        if !editing.focused {
            response.request_focus();
            editing.focused = true;
        }
        let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if escape {
            ui.data_mut(|d| d.remove::<Editing>(id));
        } else if enter || response.lost_focus() {
            ui.data_mut(|d| d.remove::<Editing>(id));
            self.commit_typed(ui, component, code, &editing.text, current, events);
        } else {
            ui.data_mut(|d| d.insert_temp(id, editing));
        }
        true
    }

    /// Starts, moves and ends a scrub, or starts typing on a click.
    #[allow(clippy::too_many_arguments)]
    fn scrub_or_click(
        &self,
        ui: &Ui,
        response: &egui::Response,
        id: Id,
        rect: Rect,
        component: Component,
        current: f64,
        scrub: Option<Scrub>,
        events: &mut Vec<ParamEvent>,
    ) {
        let scrub = if response.drag_started() {
            let started = Scrub {
                start: current,
                moved: 0.0,
            };
            ui.data_mut(|d| {
                d.insert_temp(id, started.clone());
                d.remove::<FieldError>(self.id);
            });
            Some(started)
        } else {
            scrub
        };
        if let Some(mut s) = scrub {
            if response.dragged() {
                let factor = if ui.input(|i| i.modifiers.shift) {
                    FINE_FACTOR
                } else {
                    1.0
                };
                let dx = f64::from(response.drag_delta().x);
                if dx != 0.0 {
                    // Kept inside the hard range so dragging back responds at once.
                    let range = self.def.kind.range().unwrap_or(Range::UNBOUNDED);
                    s.moved =
                        range.clamp(s.start + s.moved + dx * self.scrub_step() * factor) - s.start;
                    self.set(events, self.value_for(component, s.start + s.moved), false);
                }
                self.delta_chip(ui, rect, s.moved);
                ui.data_mut(|d| d.insert_temp(id, s.clone()));
            }
            if response.drag_stopped() {
                ui.data_mut(|d| d.remove::<Scrub>(id));
                self.set(events, self.value_for(component, s.start + s.moved), true);
            }
        } else if response.clicked() {
            ui.data_mut(|d| {
                d.insert_temp(
                    id,
                    Editing {
                        text: self.def.format(current),
                        focused: false,
                    },
                );
                d.remove::<FieldError>(self.id);
            });
        }
    }

    /// The glass chip above a scrubbing field with the change so far, on the tooltip
    /// layer so neighbouring rows do not cover it.
    fn delta_chip(&self, ui: &Ui, field: Rect, moved: f64) {
        let shown = self.def.unit.to_display(moved);
        let sign = if shown < 0.0 { '\u{2212}' } else { '+' };
        let text = format!("{sign}{:.*}", self.def.kind.decimals(), shown.abs());
        let painter = ui
            .ctx()
            .layer_painter(LayerId::new(Order::Tooltip, self.id));
        let x = ui
            .ctx()
            .pointer_latest_pos()
            .map_or(field.center().x, |p| p.x);
        paint::delta_chip(self.paint, &painter, field, x, &text);
    }

    fn slider(
        &self,
        ui: &mut Ui,
        rect: Rect,
        soft: Range,
        current: f64,
        events: &mut Vec<ParamEvent>,
    ) {
        let response = ui.interact(
            rect,
            self.id.with("slider"),
            self.sense(Sense::click_and_drag()),
        );
        let span = soft.max - soft.min;
        let fraction = ((current - soft.min) / span) as f32;
        let state = match (self.enabled, response.dragged(), response.hovered()) {
            (true, true, _) => SliderState::Drag,
            (true, false, true) => SliderState::Hover,
            _ => SliderState::Idle,
        };
        paint::slider(self.paint, self.painter, rect, fraction, state);
        if !self.enabled {
            return;
        }
        let at = |x: f32| {
            let f = f64::from(((x - rect.left() - 6.0) / (rect.width() - 12.0)).clamp(0.0, 1.0));
            soft.min + f * span
        };
        if let Some(p) = response.interact_pointer_pos() {
            if response.dragged() {
                self.set(events, ParamValue::Float(at(p.x)), false);
            } else if response.clicked() {
                self.set(events, ParamValue::Float(at(p.x)), true);
            }
        }
        if response.drag_stopped() {
            let v = response.interact_pointer_pos().map_or(current, |p| at(p.x));
            self.set(events, ParamValue::Float(v), true);
        }
    }

    fn stepper(&self, ui: &mut Ui, rect: Rect, current: i64, events: &mut Vec<ParamEvent>) {
        let [minus, value, plus] = paint::stepper_parts(rect);
        let m = ui.interact(minus, self.id.with("minus"), self.sense(Sense::click()));
        let p = ui.interact(plus, self.id.with("plus"), self.sense(Sense::click()));
        let current_f = current as f64;
        let component = Component::Single;
        let (id, code) = self.component_id(component);
        if self.editing(ui, id, rect, component, code, current_f, events) {
            return;
        }
        let v = ui.interact(value, id, self.sense(Sense::click_and_drag()));
        let scrub = ui.data(|d| d.get_temp::<Scrub>(id));
        let error = self.error(ui);
        let hover = match (
            self.enabled,
            m.hovered(),
            v.hovered() || scrub.is_some(),
            p.hovered(),
        ) {
            (false, ..) => StepperHover::None,
            (true, true, _, _) => StepperHover::Minus,
            (true, _, true, _) => StepperHover::Value,
            (true, _, _, true) => StepperHover::Plus,
            _ => StepperHover::None,
        };
        if hover == StepperHover::Value {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        let state = match (&error, &scrub) {
            (Some(_), _) => FieldState::Error,
            (None, Some(_)) => FieldState::Scrub,
            (None, None) => FieldState::Idle,
        };
        let text = error.map_or_else(|| self.def.format(current_f), |e| e.typed);
        paint::stepper(self.paint, self.painter, rect, &text, hover, state);
        if !self.enabled {
            return;
        }
        if m.clicked() {
            self.set(events, ParamValue::Int(current.saturating_sub(1)), true);
        }
        if p.clicked() {
            self.set(events, ParamValue::Int(current.saturating_add(1)), true);
        }
        self.scrub_or_click(ui, &v, id, rect, component, current_f, scrub, events);
    }

    fn choice(
        &self,
        ui: &mut Ui,
        rect: Rect,
        options: &[ChoiceOption],
        current: &str,
        events: &mut Vec<ParamEvent>,
    ) {
        let selected = options
            .iter()
            .find(|o| o.id == current)
            .map(|o| assets::i18n::text(&o.label))
            .unwrap_or_default();
        let response = ui.interact(rect, self.id, self.sense(Sense::click()));
        let popup_id = egui::Popup::default_response_id(&response);
        let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        paint::dropdown(
            self.paint,
            self.painter,
            rect,
            &selected,
            self.enabled && response.hovered(),
            open,
        );
        if !self.enabled {
            return;
        }
        let mut picked = None;
        egui::Popup::menu(&response).width(rect.width()).show(|ui| {
            let row_h = self.paint.theme.tokens().sizes.row_pro;
            for option in options {
                let (row, r) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let is_current = option.id == current;
                let painter = ui.painter();
                if r.hovered() {
                    let corner = self.paint.control_radius();
                    painter.rect_filled(row, corner, self.paint.c(Role::Bg3));
                    self.paint.glass_sheen(painter, row, corner);
                }
                let s = self.paint.s();
                let check = Rect::from_min_size(
                    pos2(row.left() + s.s8, row.center().y - 8.0),
                    vec2(16.0, 16.0),
                );
                if is_current {
                    icon(
                        painter,
                        check,
                        Icon::Check,
                        self.paint.c(Role::TextPrimary),
                        2.25,
                    );
                }
                let role = if is_current || r.hovered() {
                    Role::TextPrimary
                } else {
                    Role::TextSecondary
                };
                self.paint.text(
                    painter,
                    pos2(check.right() + s.s8, row.center().y),
                    egui::Align2::LEFT_CENTER,
                    &assets::i18n::text(&option.label),
                    Type::Label,
                    role,
                    row.right() - check.right() - 2.0 * s.s8,
                );
                if r.clicked() {
                    picked = Some(option.id.clone());
                    egui::Popup::close_id(ui.ctx(), popup_id);
                }
            }
        });
        if let Some(id) = picked.filter(|id| id != current) {
            self.set(events, ParamValue::Choice(id), true);
        }
    }

    fn colour(
        &self,
        ui: &mut Ui,
        rect: Rect,
        c: [f32; 4],
        with_alpha: bool,
        events: &mut Vec<ParamEvent>,
    ) {
        // The parameter's value, not a UI colour: egui keeps it premultiplied.
        let mut rgba = egui::Rgba::TRANSPARENT;
        for i in 0..3 {
            rgba[i] = c[i] * c[3];
        }
        rgba[3] = c[3];
        let alpha = if with_alpha {
            egui::color_picker::Alpha::OnlyBlend
        } else {
            egui::color_picker::Alpha::Opaque
        };
        let mut changed = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            if !self.enabled {
                ui.multiply_opacity(self.paint.theme.tokens().opacity.disabled);
            }
            ui.add_enabled_ui(self.enabled, |ui| {
                ui.spacing_mut().interact_size = rect.size();
                changed =
                    egui::color_picker::color_edit_button_rgba(ui, &mut rgba, alpha).changed();
            });
        });
        if changed {
            let [r, g, b, a] = rgba.to_rgba_unmultiplied();
            self.set(events, ParamValue::Colour([r, g, b, a]), true);
        }
    }

    fn text(
        &self,
        ui: &mut Ui,
        rect: Rect,
        current: &str,
        multiline: bool,
        events: &mut Vec<ParamEvent>,
    ) {
        let id = self.id.with("text");
        let focused = ui.memory(|m| m.has_focus(id));
        let hovered = self.enabled && ui.rect_contains_pointer(rect);
        let state = match (focused, hovered) {
            (true, _) => FieldState::Editing,
            (false, true) => FieldState::Hover,
            (false, false) => FieldState::Idle,
        };
        paint::field_frame(self.paint, self.painter, rect, state);
        let mut text = current.to_owned();
        let pad = self.paint.s().s8;
        let changed = ui
            .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                if !self.enabled {
                    ui.multiply_opacity(self.paint.theme.tokens().opacity.disabled);
                }
                ui.add_enabled_ui(self.enabled, |ui| {
                    let edit = if multiline {
                        egui::TextEdit::multiline(&mut text)
                    } else {
                        egui::TextEdit::singleline(&mut text)
                    };
                    let edit = edit
                        .id(id)
                        .font(self.paint.font(Type::Label))
                        .text_color(self.paint.c(Role::TextPrimary))
                        .frame(egui::Frame::NONE)
                        .vertical_align(Align::Center)
                        .margin(egui::Margin::ZERO)
                        .desired_width(rect.width() - 2.0 * pad);
                    ui.put(rect.shrink2(vec2(pad, 0.0)), edit).changed()
                })
                .inner
            })
            .inner;
        if changed {
            self.set(events, ParamValue::Text(text), true);
        }
    }

    pub fn keyframe_lane(
        &self,
        ui: &mut Ui,
        lane: Rect,
        info: KeyframeInfo,
        events: &mut Vec<ParamEvent>,
    ) {
        let [prev, diamond, next] = paint::keyframe_slots(lane);
        let sense = self.sense(Sense::click());
        let id = &self.def.id;
        let r = ui
            .interact(diamond, self.id.with("key"), sense)
            .on_hover_text(if info.mark == KeyframeMark::AtPlayhead {
                t!("params.keyframe.remove")
            } else {
                t!("params.keyframe.add")
            });
        paint::keyframe_lane(
            self.paint,
            self.painter,
            lane,
            info,
            self.enabled && r.hovered(),
        );
        if r.clicked() {
            events.push(ParamEvent::ToggleKeyframe { id: id.clone() });
        }
        if info.mark == KeyframeMark::Off {
            return;
        }
        let r = ui
            .interact(prev, self.id.with("prev"), sense)
            .on_hover_text(t!("params.keyframe.previous"));
        if r.clicked() && info.previous {
            events.push(ParamEvent::PreviousKeyframe { id: id.clone() });
        }
        let r = ui
            .interact(next, self.id.with("next"), sense)
            .on_hover_text(t!("params.keyframe.next"));
        if r.clicked() && info.next {
            events.push(ParamEvent::NextKeyframe { id: id.clone() });
        }
    }

    pub fn reset(&self, ui: &mut Ui, slot: Rect, name: &str, events: &mut Vec<ParamEvent>) {
        let size = self.paint.theme.tokens().icon.sm;
        let icon_rect = Rect::from_center_size(slot.center(), vec2(size, size));
        let response = ui
            .interact(icon_rect, self.id.with("reset"), Sense::click())
            .on_hover_text(t!("params.reset", name = name));
        paint::reset(
            self.painter,
            icon_rect,
            icon_colour(self.paint, response.hovered()),
            12.0,
            2.5,
        );
        if response.clicked() {
            events.push(ParamEvent::Reset {
                id: self.def.id.clone(),
            });
        }
    }
}
