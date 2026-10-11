//! The minimap: the whole sequence in one strip, one thin lane per track with its items
//! in their kind's colour, and the track area's window over it. Dragging the window
//! scrolls the timeline (its left edge on whole frames); clicking outside it centres the
//! window there.
//!
//! [`MinimapContent`] is built from the sequence (cheap: one span per item, merged where
//! they touch at the minimap's scale) and [`minimap`] draws it under the tracks.

use egui::{Id, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, pos2, vec2};
use project::{ItemKind, Sequence, TrackKind};

use super::view::{TimelineView, WindowDrag, frames_of};
use crate::panels::widgets::Paint;
use crate::t;
use crate::theme::Role;

/// The minimap's height (the `control/pro` token).
pub const MINIMAP_HEIGHT: f32 = 24.0;
/// Room left and right of the lanes, and above and below them.
const INSET: f32 = 4.0;
/// The window is never drawn narrower than this.
const MIN_WINDOW: f32 = 6.0;
/// The tallest a lane gets.
const MAX_LANE: f32 = 4.0;

/// One item's span on a lane, in frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
    pub role: Role,
}

/// What the minimap shows: each track's item spans, top track first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MinimapContent {
    pub lanes: Vec<Vec<Span>>,
}

impl MinimapContent {
    pub fn from_sequence(sequence: &Sequence) -> Self {
        let rate = sequence.frame_rate;
        let lanes = sequence
            .tracks
            .iter()
            .map(|track| {
                track
                    .items
                    .iter()
                    .filter_map(|item| {
                        let end = item.range.end().ok()?;
                        Some(Span {
                            start: frames_of(item.range.start, rate),
                            end: frames_of(end, rate),
                            role: item_role(track.kind, &item.kind),
                        })
                    })
                    .collect()
            })
            .collect();
        Self { lanes }
    }

    /// The end of the last span, in frames.
    pub fn end(&self) -> f64 {
        self.lanes
            .iter()
            .filter_map(|l| l.last())
            .map(|s| s.end)
            .fold(0.0, f64::max)
    }
}

fn item_role(track: TrackKind, item: &ItemKind) -> Role {
    match (track, item) {
        (TrackKind::Audio, _) => Role::ClipAudio,
        (_, ItemKind::Subtitle(_)) => Role::ClipSubtitle,
        (_, ItemKind::Adjustment(_)) => Role::ClipAdjustment,
        _ => Role::ClipVideo,
    }
}

/// How the minimap maps frames to points: frame `origin` sits at the lanes' left edge
/// and each frame takes `scale` points. The range covers the scrollable extent, the
/// content and the window, so the window always fits.
pub fn scale(rect: Rect, view: &TimelineView, content: &MinimapContent) -> (f64, f64) {
    let lo = view.start().min(0.0);
    let hi = (view.start() + view.visible_frames())
        .max(view.extent_frames())
        .max(content.end());
    let width = f64::from((rect.width() - 2.0 * INSET).max(1.0));
    (lo, width / (hi - lo).max(1.0))
}

/// The window's rectangle over the minimap in `rect`.
pub fn window_rect(rect: Rect, view: &TimelineView, content: &MinimapContent) -> Rect {
    let (origin, s) = scale(rect, view, content);
    let left = rect.left() + INSET + ((view.start() - origin) * s) as f32;
    let width = ((view.visible_frames() * s) as f32).max(MIN_WINDOW);
    Rect::from_min_size(
        pos2(left, rect.top() + 1.0),
        vec2(width, rect.height() - 2.0),
    )
}

/// The egui id of the minimap.
pub fn minimap_id() -> Id {
    Id::new("glass.timeline.minimap")
}

/// Draws the minimap in `rect` and scrolls `view` when its window is dragged or the
/// strip clicked.
pub fn minimap(
    ui: &mut Ui,
    rect: Rect,
    view: &mut TimelineView,
    content: &MinimapContent,
) -> Response {
    let response = ui.interact(rect, minimap_id(), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, t!("timeline.overview")));

    let window = window_rect(rect, view, content);
    let (origin, s) = scale(rect, view, content);
    let left = rect.left() + INSET;
    // The press, not where the drag was recognised, sets the grab, so the window does
    // not lag the pointer by the drag threshold.
    if response.drag_started()
        && let Some(p) = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos())
    {
        let grab = if window.x_range().contains(p.x) {
            p.x - window.left()
        } else {
            window.width() / 2.0
        };
        view.window_drag = Some(WindowDrag {
            grab,
            origin,
            scale: s,
        });
    }
    if let (Some(drag), Some(p)) = (view.window_drag, response.interact_pointer_pos())
        && response.dragged()
    {
        let frames = drag.origin + f64::from(p.x - drag.grab - left) / drag.scale;
        view.scroll_to_frame(frames.round() as i64);
    }
    if response.drag_stopped() {
        view.window_drag = None;
    }
    if response.clicked()
        && let Some(p) = response.interact_pointer_pos()
        && !window.x_range().contains(p.x)
    {
        let centre = origin + f64::from(p.x - left) / s;
        view.scroll_to_frame((centre - view.visible_frames() / 2.0).round() as i64);
    }

    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, paint.c(Role::Bg1));
    let (origin, s) = scale(rect, view, content);
    let lanes = rect.shrink2(vec2(INSET, INSET));
    let count = content.lanes.len().max(1) as f32;
    let pitch = (lanes.height() / count).min(MAX_LANE);
    let top = lanes.center().y - pitch * count / 2.0;
    let bar = (pitch - 1.0).max(1.0);
    for (i, lane) in content.lanes.iter().enumerate() {
        let y = top + i as f32 * pitch;
        // Spans closer than half a point merge into one rectangle.
        let mut run: Option<(f32, f32, Role)> = None;
        let flush = |run: Option<(f32, f32, Role)>| {
            if let Some((a, b, role)) = run {
                let r = Rect::from_min_max(pos2(a, y), pos2(b.max(a + 1.0), y + bar));
                painter.rect_filled(r, 0.0, paint.mix(role, 0.8));
            }
        };
        for span in lane {
            let a = left + ((span.start - origin) * s) as f32;
            let b = left + ((span.end - origin) * s) as f32;
            run = match run {
                Some((ra, rb, role)) if role == span.role && a - rb < 0.5 => Some((ra, b, role)),
                other => {
                    flush(other);
                    Some((a, b, span.role))
                }
            };
        }
        flush(run);
    }

    let window = window_rect(rect, view, content);
    let corner = paint.control_radius();
    painter.rect_filled(window, corner, paint.c(Role::AccentSoft));
    let edge = if response.hovered() || response.dragged() {
        Role::Accent
    } else {
        Role::BorderStrong
    };
    painter.rect_stroke(
        window,
        corner,
        Stroke::new(1.0, paint.c(edge)),
        StrokeKind::Inside,
    );
    painter.hline(
        rect.x_range(),
        rect.top() + 0.5,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
    response.on_hover_text(t!("timeline.overview_hint"))
}
