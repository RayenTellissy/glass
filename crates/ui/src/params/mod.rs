//! Inspector sections generated from an effect's [`ParamSchema`]: one 28 px row per
//! parameter in the lanes of `Sheet / Inspector video` (label 64, fields fill, keyframe
//! toggle 40, reset 16, 8 px gaps and padding), fields as on `Sheet / Controls`.
//!
//! Numbers scrub: the whole field is the handle (drag sideways, Shift for fine steps),
//! a click types, maths works (`+10`, `*2`, `112+10`), Return or leaving the field
//! commits, Esc cancels, and a typed value outside the hard range shows an error and
//! keeps the old value. Integers are steppers, booleans switches, choices drop down,
//! colours open a picker, and parameters marked `slider` get one over their soft range
//! beside a 64 px field. A schema's header switch sits in the section header and
//! disables the other rows while off.
//!
//! The widgets never change values themselves: [`Section::show`] returns
//! [`ParamEvent`]s for the caller to apply as edits (so undo and keyframing stay in the
//! project model); [`apply`] does it directly for simple cases and tests.

mod expr;
mod fields;
pub mod paint;

use egui::{Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use project::params::{ParamDef, ParamSchema, ParamValue, ParamValues};

use crate::panels::widgets::{Paint, Type};
use crate::t;
use crate::theme::Role;

pub use expr::evaluate;
pub use paint::{KeyframeInfo, KeyframeMark};

/// What the user did to a parameter.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamEvent {
    /// A new value, already made valid for the parameter. `commit` is false while a
    /// scrub or slider drag is still moving, so the caller can merge the drag into one
    /// undo step.
    Set {
        id: String,
        value: ParamValue,
        commit: bool,
    },
    Reset {
        id: String,
    },
    /// Add a keyframe at the playhead, or remove the one there.
    ToggleKeyframe {
        id: String,
    },
    PreviousKeyframe {
        id: String,
    },
    NextKeyframe {
        id: String,
    },
    /// The section header's reset: every parameter back to its default.
    ResetSection,
}

/// Applies `events` to `values` (keyframe events are left to the caller).
pub fn apply(schema: &ParamSchema, values: &mut ParamValues, events: &[ParamEvent]) {
    for event in events {
        match event {
            ParamEvent::Set { id, value, .. } => {
                values.set(schema, id, value.clone());
            }
            ParamEvent::Reset { id } => values.reset(id),
            ParamEvent::ResetSection => {
                for p in &schema.params {
                    values.reset(&p.id);
                }
            }
            ParamEvent::ToggleKeyframe { .. }
            | ParamEvent::PreviousKeyframe { .. }
            | ParamEvent::NextKeyframe { .. } => {}
        }
    }
}

/// Label lane width, also the width of the field beside a slider.
pub const LABEL_WIDTH: f32 = 64.0;
/// Previous (12), diamond (16), next (12).
pub const KEYFRAME_LANE: f32 = 40.0;
/// The section's height for `schema` without error helpers: vertical padding, the
/// header and one row per parameter (the header switch has no row).
pub fn section_height(schema: &ParamSchema, row: f32, pad: f32) -> f32 {
    let rows = schema.params.len() - usize::from(schema.header_switch.is_some());
    2.0 * pad + row * (rows + 1) as f32 + 1.0
}

type KeyframeLookup<'a> = &'a dyn Fn(&str) -> KeyframeInfo;

/// One inspector section built from a schema.
pub struct Section<'a> {
    schema: &'a ParamSchema,
    values: &'a ParamValues,
    keyframes: Option<KeyframeLookup<'a>>,
    enabled: bool,
    id_salt: Id,
}

impl<'a> Section<'a> {
    pub fn new(schema: &'a ParamSchema, values: &'a ParamValues) -> Self {
        Self {
            schema,
            values,
            keyframes: None,
            enabled: true,
            id_salt: Id::new(("params", schema.id.as_str())),
        }
    }

    /// Where keyframe toggles read their state; without it they show as off.
    pub fn keyframes(mut self, lookup: KeyframeLookup<'a>) -> Self {
        self.keyframes = Some(lookup);
        self
    }

    /// A bypassed section shows its values at the disabled opacity and takes no input.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Distinguishes two sections of the same effect (for example two instances).
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Id::new(("params", self.schema.id.as_str(), salt));
        self
    }

    /// Draws the section at the top of `ui`'s remaining space, full width, and returns
    /// what the user did this frame.
    pub fn show(self, ui: &mut Ui) -> Vec<ParamEvent> {
        let paint = Paint::new(ui.ctx());
        let pad = paint.s().s4;
        let mut events = Vec::new();
        ui.add_space(pad);
        let switch_on = self.header(ui, &paint, &mut events);
        let rows_enabled = self.enabled && switch_on;
        for def in &self.schema.params {
            if self.schema.header_switch.as_deref() == Some(def.id.as_str()) {
                continue;
            }
            self.row(ui, &paint, def, rows_enabled, &mut events);
        }
        ui.add_space(pad);
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(
            line.x_range(),
            line.center().y,
            Stroke::new(1.0, paint.c(Role::BorderSubtle)),
        );
        events
    }

    /// The caps title, the header switch and the section reset; returns whether the
    /// header switch (if any) is on.
    fn header(&self, ui: &mut Ui, paint: &Paint, events: &mut Vec<ParamEvent>) -> bool {
        let s = paint.s();
        let (rect, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), paint.theme.tokens().sizes.row_pro),
            Sense::hover(),
        );
        let inner = rect.shrink2(vec2(s.s8, 0.0));
        let mut painter = ui.painter().clone();
        if !self.enabled {
            painter.multiply_opacity(paint.theme.tokens().opacity.disabled);
        }
        let reset_w = paint.theme.tokens().icon.sm;
        let reset = Rect::from_center_size(
            pos2(inner.right() - reset_w / 2.0, inner.center().y),
            vec2(reset_w, reset_w),
        );
        let mut title_right = reset.left() - s.s8;
        let mut switch_on = true;
        if let Some(id) = &self.schema.header_switch {
            switch_on = self.values.boolean(self.schema, id).unwrap_or(true);
            let switch = Rect::from_min_size(
                pos2(reset.left() - s.s8 - 24.0, inner.center().y - 7.0),
                vec2(24.0, 14.0),
            );
            title_right = switch.left() - s.s8;
            let sense = if self.enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let response = ui.interact(switch, self.id_salt.with(("switch", id)), sense);
            paint::switch(
                paint,
                &painter,
                switch,
                switch_on,
                self.enabled && response.hovered(),
            );
            if self.enabled && response.clicked() {
                events.push(ParamEvent::Set {
                    id: id.clone(),
                    value: ParamValue::Bool(!switch_on),
                    commit: true,
                });
            }
        }
        let title = assets::i18n::text(&self.schema.label).to_uppercase();
        paint.text(
            &painter,
            inner.left_center(),
            egui::Align2::LEFT_CENTER,
            &title,
            Type::Caps,
            Role::TextMuted,
            (title_right - inner.left()).max(0.0),
        );
        let changed = self
            .schema
            .params
            .iter()
            .any(|p| !self.values.is_default(self.schema, &p.id));
        if changed && self.enabled {
            let response = ui
                .interact(reset, self.id_salt.with("reset"), Sense::click())
                .on_hover_text(t!("params.reset_section"));
            let colour = fields::icon_colour(paint, response.hovered());
            paint::reset(&painter, reset, colour, 16.0, 2.25);
            if response.clicked() {
                events.push(ParamEvent::ResetSection);
            }
        }
        switch_on
    }

    fn row(
        &self,
        ui: &mut Ui,
        paint: &Paint,
        def: &ParamDef,
        enabled: bool,
        events: &mut Vec<ParamEvent>,
    ) {
        let tokens = paint.theme.tokens();
        let s = paint.s();
        let (row, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), tokens.sizes.row_pro),
            Sense::hover(),
        );
        let gap = s.s8;
        let inner = row.shrink2(vec2(s.s8, 0.0));
        let reset_w = tokens.icon.sm;
        let label = Rect::from_min_size(inner.min, vec2(LABEL_WIDTH, inner.height()));
        let reset = Rect::from_min_size(
            pos2(inner.right() - reset_w, inner.top()),
            vec2(reset_w, inner.height()),
        );
        let lane = Rect::from_min_size(
            pos2(reset.left() - gap - KEYFRAME_LANE, inner.top()),
            vec2(KEYFRAME_LANE, inner.height()),
        );
        let fields = Rect::from_min_max(
            pos2(label.right() + gap, inner.top()),
            pos2(lane.left() - gap, inner.bottom()),
        );
        let mut painter = ui.painter().clone();
        if !enabled {
            painter.multiply_opacity(tokens.opacity.disabled);
        }
        let name = assets::i18n::text(&def.label);
        paint.text(
            &painter,
            label.left_center(),
            egui::Align2::LEFT_CENTER,
            &name,
            Type::Label,
            Role::TextSecondary,
            LABEL_WIDTH,
        );

        let value = self
            .values
            .get(self.schema, &def.id)
            .unwrap_or_else(|| def.kind.default_value());
        let cx = fields::FieldCx {
            paint,
            painter: &painter,
            id: self.id_salt.with(def.id.as_str()),
            def,
            enabled,
        };
        cx.fields(ui, fields, &value, events);
        if def.keyframeable {
            let info = self.keyframes.map(|f| f(&def.id)).unwrap_or_default();
            cx.keyframe_lane(ui, lane, info, events);
        }
        if enabled && !self.values.is_default(self.schema, &def.id) {
            cx.reset(ui, reset, &name, events);
        }
        if let Some(error) = cx.error(ui) {
            let (helper, _) = ui.allocate_exact_size(
                vec2(
                    ui.available_width(),
                    tokens.type_scale.caption.line_height_px + s.s4,
                ),
                Sense::hover(),
            );
            let text = t!("params.error.range", min = error.min, max = error.max);
            paint::error_helper(
                paint,
                &painter,
                pos2(
                    fields.left(),
                    helper.top() + tokens.type_scale.caption.line_height_px / 2.0,
                ),
                &text,
            );
        }
    }
}
