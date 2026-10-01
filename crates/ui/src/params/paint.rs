//! How a parameter row looks, state by state, drawn straight with the painter: the
//! numeric field (`Sheet / Inspector video`, numeric field states), slider, switch,
//! dropdown and stepper (`Sheet / Controls`), the keyframe lane (previous, diamond, next)
//! and the reset arrow. No interaction here; `fields` decides the state and calls these.

use egui::epaint::PathShape;
use egui::{Align2, Color32, CornerRadius, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};

use crate::panels::widgets::{Icon, Paint, Type, icon};
use crate::theme::Role;

/// A numeric or text field's visual state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldState {
    Idle,
    /// Pointer over it (the scrub cursor shows).
    Hover,
    /// Being dragged: hover fill with a `border/strong` ring.
    Scrub,
    /// Typing: `accent` ring inside, a 3 px `accent/soft` halo outside.
    Editing,
    /// A typed value was out of range: `state/error` ring.
    Error,
}

/// The rounded field body for `state` (everything but the text).
pub fn field_frame(paint: &Paint, painter: &Painter, rect: Rect, state: FieldState) {
    let corner = paint.control_radius();
    if state == FieldState::Editing {
        let halo = crate::panels::widgets::radius(paint.theme.tokens().radius.control + 3.0);
        painter.rect_filled(rect.expand(3.0), halo, paint.c(Role::AccentSoft));
    }
    let fill = match state {
        FieldState::Hover | FieldState::Scrub => Role::Bg3,
        _ => Role::Bg2,
    };
    painter.rect_filled(rect, corner, paint.c(fill));
    let ring = match state {
        FieldState::Scrub => Some(Role::BorderStrong),
        FieldState::Editing => Some(Role::Accent),
        FieldState::Error => Some(Role::StateError),
        FieldState::Idle | FieldState::Hover => None,
    };
    if let Some(role) = ring {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, paint.c(role)),
            StrokeKind::Inside,
        );
    }
}

/// The unit (or axis letter) at the left in muted mono caption, the value at the right
/// in mono.
pub fn field_text(paint: &Paint, painter: &Painter, rect: Rect, unit: &str, value: &str) {
    let pad = paint.s().s8;
    let unit_rect = paint.text(
        painter,
        pos2(rect.left() + pad, rect.center().y),
        Align2::LEFT_CENTER,
        unit,
        Type::MonoCaption,
        Role::TextMuted,
        rect.width() / 2.0,
    );
    let room = rect.right() - pad - unit_rect.right().max(rect.left() + pad) - paint.s().s4;
    paint.text(
        painter,
        pos2(rect.right() - pad, rect.center().y),
        Align2::RIGHT_CENTER,
        value,
        Type::Mono,
        Role::TextPrimary,
        room.max(0.0),
    );
}

/// The glass chip 8 px above a scrubbing field, centred on the pointer, with the change
/// so far.
pub fn delta_chip(paint: &Paint, painter: &Painter, field: Rect, pointer_x: f32, text: &str) {
    let pad = paint.s().s4;
    let width = paint.width(painter, text, Type::MonoCaption) + 2.0 * pad;
    let chip = Rect::from_min_size(
        pos2(
            pointer_x - width / 2.0,
            field.top() - paint.s().s8 - CHIP_HEIGHT,
        ),
        vec2(width, CHIP_HEIGHT),
    );
    let corner = paint.control_radius();
    painter.rect_filled(chip, corner, paint.c(Role::Bg2));
    paint.glass_sheen(painter, chip, corner);
    painter.rect_stroke(
        chip,
        corner,
        Stroke::new(1.0, paint.c(Role::GlassEdge)),
        StrokeKind::Inside,
    );
    paint.text(
        painter,
        chip.left_center() + vec2(pad, 0.0),
        Align2::LEFT_CENTER,
        text,
        Type::MonoCaption,
        Role::TextPrimary,
        width,
    );
}

/// The chip's height.
pub const CHIP_HEIGHT: f32 = 18.0;

/// The helper under a field whose typed value was out of range: the alert circle and the
/// caption in `state/error`.
pub fn error_helper(paint: &Paint, painter: &Painter, left_center: Pos2, text: &str) {
    let glyph = Rect::from_center_size(left_center + vec2(6.0, 0.0), vec2(12.0, 12.0));
    // `Icon::Info` is the circle with the mark in its lower half: the alert circle.
    icon(painter, glyph, Icon::Info, paint.c(Role::StateError), 2.5);
    paint.text(
        painter,
        pos2(glyph.right() + paint.s().s4, left_center.y),
        Align2::LEFT_CENTER,
        text,
        Type::Caption,
        Role::StateError,
        f32::INFINITY,
    );
}

/// A slider's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderState {
    Idle,
    Hover,
    Drag,
}

/// The slider: a 4 px `bg/3` track, the filled part up to `fraction` and a 12 px accent
/// knob ringed in `bg/1` (14 px with an `accent/soft` halo on hover).
pub fn slider(paint: &Paint, painter: &Painter, rect: Rect, fraction: f32, state: SliderState) {
    let pill = CornerRadius::same(u8::MAX);
    let track = Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
    painter.rect_filled(track, pill, paint.c(Role::Bg3));
    let f = fraction.clamp(0.0, 1.0);
    let filled = Rect::from_min_size(track.min, vec2((track.width() * f).round(), 4.0));
    let fill = if state == SliderState::Idle {
        Role::TextSecondary
    } else {
        Role::TextPrimary
    };
    painter.rect_filled(filled, pill, paint.c(fill));
    let r = if state == SliderState::Hover {
        7.0
    } else {
        6.0
    };
    // The knob sits inside the track's ends.
    let x = track.left() + r + (track.width() - 2.0 * r) * f;
    let knob = pos2(x, track.center().y);
    if state == SliderState::Hover {
        painter.circle_filled(knob, r + 5.0, paint.c(Role::AccentSoft));
    }
    painter.circle_filled(knob, r + 2.0, paint.c(Role::Bg1));
    painter.circle_filled(knob, r, paint.c(Role::Accent));
}

/// The 24 x 14 switch, with the off-hover ring and knob of `Sheet / Controls`.
pub fn switch(paint: &Paint, painter: &Painter, rect: Rect, on: bool, hover: bool) {
    if on || !hover {
        paint.switch(painter, rect, on);
        return;
    }
    let pill = CornerRadius::same(u8::MAX);
    painter.rect_filled(rect, pill, paint.c(Role::Bg3));
    painter.rect_stroke(
        rect,
        pill,
        Stroke::new(1.0, paint.c(Role::TextSecondary)),
        StrokeKind::Inside,
    );
    painter.circle_filled(
        pos2(rect.min.x + 7.0, rect.center().y),
        5.0,
        paint.c(Role::TextPrimary),
    );
}

/// A closed dropdown: the choice in label type and a chevron at the right (hover lifts
/// the fill to `bg/3`; open adds the glass sheen and flips the chevron).
pub fn dropdown(paint: &Paint, painter: &Painter, rect: Rect, text: &str, hover: bool, open: bool) {
    let corner = paint.control_radius();
    let fill = if hover || open { Role::Bg3 } else { Role::Bg2 };
    painter.rect_filled(rect, corner, paint.c(fill));
    if open {
        paint.glass_sheen(painter, rect, corner);
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, paint.c(Role::GlassEdge)),
            StrokeKind::Inside,
        );
    }
    let s = paint.s();
    let chevron = Rect::from_center_size(
        pos2(rect.right() - s.s4 - 6.0, rect.center().y),
        vec2(12.0, 12.0),
    );
    let chevron_role = match (open, hover) {
        (true, _) => Role::TextPrimary,
        (false, true) => Role::TextSecondary,
        (false, false) => Role::TextMuted,
    };
    let (which, colour) = (Icon::ChevronDown, paint.c(chevron_role));
    if open {
        // Pointing up: the down chevron mirrored.
        let p = |x: f32, y: f32| chevron.min + vec2(x, y) * (12.0 / 24.0);
        painter.add(PathShape::line(
            vec![p(6.0, 15.0), p(12.0, 9.0), p(18.0, 15.0)],
            Stroke::new(1.25, colour),
        ));
    } else {
        icon(painter, chevron, which, colour, 2.5);
    }
    paint.text(
        painter,
        pos2(rect.left() + s.s8, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        Type::Label,
        Role::TextPrimary,
        (chevron.left() - s.s4 - rect.left() - s.s8).max(0.0),
    );
}

/// Which part of a stepper the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepperHover {
    None,
    Minus,
    Value,
    Plus,
}

/// The integer stepper: minus, the value centred in mono, plus, in one `bg/2` field
/// (`Sheet / Inspector video`, motion blur samples).
pub fn stepper(
    paint: &Paint,
    painter: &Painter,
    rect: Rect,
    value: &str,
    hover: StepperHover,
    state: FieldState,
) {
    field_frame(paint, painter, rect, state);
    let [minus, _, plus] = stepper_parts(rect);
    let corner = paint.control_radius();
    for (part, which) in [(minus, StepperHover::Minus), (plus, StepperHover::Plus)] {
        if hover == which {
            painter.rect_filled(part, corner, paint.c(Role::Bg3));
        }
    }
    let colour = paint.c(Role::TextSecondary);
    let glyph = |r: Rect| Rect::from_center_size(r.center(), vec2(12.0, 12.0));
    let m = glyph(minus);
    painter.line_segment(
        [
            m.left_center() + vec2(12.0 * 5.0 / 24.0, 0.0),
            m.right_center() - vec2(12.0 * 5.0 / 24.0, 0.0),
        ],
        Stroke::new(1.25, colour),
    );
    icon(painter, glyph(plus), Icon::Plus, colour, 2.5);
    paint.text(
        painter,
        rect.center(),
        Align2::CENTER_CENTER,
        value,
        Type::Mono,
        Role::TextPrimary,
        rect.width() - 2.0 * STEPPER_BUTTON,
    );
}

/// The stepper's buttons are square, as tall as the field.
pub const STEPPER_BUTTON: f32 = 24.0;
/// The stepper's width on the board.
pub const STEPPER_WIDTH: f32 = 96.0;

/// Minus, value and plus areas of a stepper.
pub fn stepper_parts(rect: Rect) -> [Rect; 3] {
    let minus = Rect::from_min_size(rect.min, vec2(STEPPER_BUTTON, rect.height()));
    let plus = Rect::from_min_size(
        pos2(rect.right() - STEPPER_BUTTON, rect.top()),
        vec2(STEPPER_BUTTON, rect.height()),
    );
    let value = Rect::from_min_max(
        pos2(minus.right(), rect.top()),
        pos2(plus.left(), rect.bottom()),
    );
    [minus, value, plus]
}

/// A parameter's keyframe toggle state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyframeMark {
    /// Not animated.
    #[default]
    Off,
    /// Animated, no keyframe at the playhead.
    On,
    /// A keyframe sits at the playhead.
    AtPlayhead,
}

/// What the keyframe lane shows for one parameter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyframeInfo {
    pub mark: KeyframeMark,
    /// A keyframe exists before the playhead.
    pub previous: bool,
    /// A keyframe exists after the playhead.
    pub next: bool,
}

/// The lane's slots: previous (12), diamond (16), next (12), 16 tall.
pub fn keyframe_slots(lane: Rect) -> [Rect; 3] {
    let top = lane.center().y - 8.0;
    let prev = Rect::from_min_size(pos2(lane.left(), top), vec2(12.0, 16.0));
    let diamond = Rect::from_min_size(pos2(prev.right(), top), vec2(16.0, 16.0));
    let next = Rect::from_min_size(pos2(diamond.right(), top), vec2(12.0, 16.0));
    [prev, diamond, next]
}

/// The keyframe lane: chevrons only while animated (dimmed to the disabled opacity when
/// there is no keyframe that way), the diamond outlined in `text/muted` when off,
/// `text/primary` when on, filled with `accent` at the playhead; hover puts a `bg/3`
/// square behind the diamond.
pub fn keyframe_lane(
    paint: &Paint,
    painter: &Painter,
    lane: Rect,
    info: KeyframeInfo,
    diamond_hover: bool,
) {
    let [prev, slot, next] = keyframe_slots(lane);
    if info.mark != KeyframeMark::Off {
        let disabled = paint.theme.tokens().opacity.disabled;
        let colour = |available: bool| {
            let c = paint.c(Role::TextSecondary);
            if available {
                c
            } else {
                c.gamma_multiply(disabled)
            }
        };
        chevron(painter, prev, false, colour(info.previous));
        chevron(painter, next, true, colour(info.next));
    }
    if diamond_hover {
        painter.rect_filled(slot, paint.control_radius(), paint.c(Role::Bg3));
    }
    let (stroke, fill) = match info.mark {
        KeyframeMark::Off if diamond_hover => (paint.c(Role::TextPrimary), None),
        KeyframeMark::Off => (paint.c(Role::TextMuted), None),
        KeyframeMark::On => (paint.c(Role::TextPrimary), None),
        KeyframeMark::AtPlayhead => (paint.c(Role::Accent), Some(paint.c(Role::Accent))),
    };
    diamond(painter, slot, stroke, fill);
}

/// The keyframe diamond (12 px in a 16 px slot), outlined or filled.
pub fn diamond(painter: &Painter, slot: Rect, stroke: Color32, fill: Option<Color32>) {
    let c = slot.center();
    let r = 4.5;
    let points = vec![
        pos2(c.x, c.y - r),
        pos2(c.x + r, c.y),
        pos2(c.x, c.y + r),
        pos2(c.x - r, c.y),
    ];
    let outline = Stroke::new(1.5, stroke);
    match fill {
        Some(fill) => painter.add(PathShape::convex_polygon(points, fill, outline)),
        None => painter.add(PathShape::closed_line(points, outline)),
    };
}

/// Maps a point of a 24-unit icon grid into `rect` scaled to `size` points.
fn grid(rect: Rect, size: f32) -> impl Fn(f32, f32) -> Pos2 {
    let origin = rect.center() - vec2(size, size) / 2.0;
    move |x, y| origin + vec2(x, y) * (size / 24.0)
}

/// A 12 px chevron pointing left or right.
pub fn chevron(painter: &Painter, slot: Rect, right: bool, color: Color32) {
    let p = grid(slot, 12.0);
    let points = if right {
        vec![p(9.0, 18.0), p(15.0, 12.0), p(9.0, 6.0)]
    } else {
        vec![p(15.0, 18.0), p(9.0, 12.0), p(15.0, 6.0)]
    };
    painter.add(PathShape::line(points, Stroke::new(1.25, color)));
}

/// The counter-clockwise reset arrow at `size` points, stroked `stroke_24` on its 24
/// grid (rows: 12 px at 2.5, section headers: 16 px at 2.25).
pub fn reset(painter: &Painter, slot: Rect, color: Color32, size: f32, stroke_24: f32) {
    let p = grid(slot, size);
    let stroke = Stroke::new(size / 24.0 * stroke_24, color);
    let centre = p(12.0, 12.0);
    let radius = size * 9.0 / 24.0;
    // From the left, down around the bottom and right side up to the top, then curling
    // back towards the arrow head at the upper left.
    let arc: Vec<Pos2> = (0..=28)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 - 1.75 * i as f32 / 28.0);
            centre + vec2(a.cos(), a.sin()) * radius
        })
        .chain([p(3.0, 8.0)])
        .collect();
    painter.add(PathShape::line(arc, stroke));
    painter.add(PathShape::line(
        vec![p(3.0, 3.0), p(3.0, 8.0), p(8.0, 8.0)],
        stroke,
    ));
}
