//! The timeline canvas's view: which stretch of the sequence the track area shows, how
//! far the tracks are scrolled, and the conversions every other timeline part draws and
//! hit-tests with (time to x, x to frame, track to y, y to track).
//!
//! [`TimelineView`] keeps the left edge as a position in frames and the zoom as points
//! per frame, both floats, because pinch and scroll zoom move continuously; everything
//! that goes back into the project is exact: [`TimelineView::x_to_time`] gives the
//! [`Time`] of the nearest frame boundary, [`TimelineView::frame_at_x`] the frame under
//! a point. Time runs left to right in every locale (only the header column mirrors).
//!
//! Zoom goes from [`MAX_FRAME_WIDTH`] points per frame down to the whole sequence with
//! its margins ([`TimelineView::fit`]); the left edge can go a margin before zero and
//! until the end of the sequence (at least [`MIN_EXTENT_SECONDS`]) sits mid-view, so
//! there is room to append. Zoom to fit, zoom to a range and dragging the minimap put
//! the left edge on a frame boundary; pinch, scroll and keyboard zoom keep the time
//! under their anchor fixed instead.
//!
//! Input: a pinch, or scrolling with Ctrl, ⌘ or Alt held, zooms about the pointer;
//! scrolling pans (a trackpad on both axes, a wheel through the tracks), and Shift turns
//! a wheel into a horizontal pan ([`TimelineView::handle_input`]). The `view.zoom_in`,
//! `view.zoom_out` and `view.zoom_to_fit` commands arrive through
//! [`TimelineView::run_command`]; they, zoom to fit and zoom to a range animate briefly
//! ([`ANIMATION_SECONDS`]) unless [`TimelineView::reduced_motion`] is set, and any
//! gesture stops an animation where it is.
//!
//! Edit snapping (clip edges, playhead, markers, beats) fills a [`SnapTargets`] and asks
//! [`TimelineView::snap`], which measures the snap distance on screen.

use egui::{
    CornerRadius, Event, Id, MouseWheelUnit, Rangef, Rect, Response, Sense, Stroke, Ui, Vec2,
    WidgetInfo, pos2, vec2,
};
use project::{Rational, Sequence, Time, TimeRange, TrackId};

use super::track_header::TrackRow;
use crate::panels::widgets::Paint;
use crate::t;
use crate::theme::Role;

/// The widest a frame may get, in points (the closest zoom).
pub const MAX_FRAME_WIDTH: f64 = 96.0;
/// Zoom to fit and zoom to a range leave this many points either side.
pub const FIT_MARGIN: f32 = 24.0;
/// An empty or short sequence zooms and scrolls as if it were this long.
pub const MIN_EXTENT_SECONDS: i64 = 10;
/// One keyboard zoom step's factor.
pub const ZOOM_STEP: f64 = 2.0;
/// Zoom factor per point scrolled with a zoom modifier: `exp(points / 200)`, egui's
/// own scroll zoom speed.
pub const ZOOM_PER_POINT: f64 = 1.0 / 200.0;
/// How long keyboard zoom, zoom to fit and zoom to a range take to settle.
pub const ANIMATION_SECONDS: f64 = 0.15;
/// How close (in points) an edit must come to a snap target to snap to it.
pub const SNAP_DISTANCE: f32 = 8.0;
/// The zoom slider's width (a thumb travels 120 points).
pub const ZOOM_SLIDER_WIDTH: f32 = 120.0;
/// The track scrollbar's width.
pub const SCROLLBAR_WIDTH: f32 = 8.0;

/// Where the track area's left edge is and how far it is zoomed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewPos {
    /// The frame position at the left edge (may fall between frames).
    pub start: f64,
    /// Points per frame.
    pub frame_px: f64,
}

/// What the view is laid over, given every frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Extent {
    /// The track area's size in points (without headers, ruler or minimap).
    pub width: f32,
    pub height: f32,
    /// The sequence's length ([`sequence_duration`]).
    pub duration: Time,
    /// The tracks' total height ([`TrackLayout::content_height`]).
    pub content_height: f32,
}

/// The end of the last item or marker of `sequence`.
pub fn sequence_duration(sequence: &Sequence) -> Time {
    let tracks = sequence.tracks.iter().filter_map(|t| t.end().ok());
    let markers = sequence.markers.iter().filter_map(|m| m.range().end().ok());
    tracks.chain(markers).max().unwrap_or(Time::ZERO)
}

/// `t` in frames at `rate`, as a float (exact for frame boundaries).
pub fn frames_of(t: Time, rate: Rational) -> f64 {
    let n = i128::from(t.num()) * i128::from(rate.num);
    let d = i128::from(t.den()) * i128::from(rate.den);
    if d == 0 {
        return 0.0;
    }
    let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
    let (q, r) = (n.div_euclid(d), n.rem_euclid(d));
    q as f64 + r as f64 / d as f64
}

/// The tracks' rows top to bottom with their heights, for track to y and back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackLayout {
    /// Each track and the top of its row, in content points from the first row's top.
    rows: Vec<(TrackId, f32, f32)>,
    total: f32,
}

impl TrackLayout {
    /// Rows of the given heights, top first.
    pub fn new(rows: impl IntoIterator<Item = (TrackId, f32)>) -> Self {
        let mut total = 0.0;
        let rows = rows
            .into_iter()
            .map(|(id, h)| {
                let row = (id, total, h);
                total += h;
                row
            })
            .collect();
        Self { rows, total }
    }

    /// The header rows' tracks at their height presets.
    pub fn from_rows(rows: &[TrackRow]) -> Self {
        Self::new(rows.iter().map(|r| (r.id, r.flags.height.height())))
    }

    pub fn content_height(&self) -> f32 {
        self.total
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Each track and its row's span in content points.
    pub fn iter(&self) -> impl Iterator<Item = (TrackId, Rangef)> + '_ {
        self.rows
            .iter()
            .map(|&(id, top, h)| (id, Rangef::new(top, top + h)))
    }

    /// `track`'s row in content points.
    pub fn span(&self, track: TrackId) -> Option<Rangef> {
        self.iter().find(|(id, _)| *id == track).map(|(_, r)| r)
    }

    /// The track whose row holds content point `y` (a row's top belongs to it, its
    /// bottom to the next).
    pub fn track_at(&self, y: f32) -> Option<TrackId> {
        if !(0.0..self.total).contains(&y) {
            return None;
        }
        let i = self.rows.partition_point(|&(_, top, _)| top <= y);
        self.rows.get(i.checked_sub(1)?).map(|r| r.0)
    }
}

/// What a snap target is, so the snap can say what it caught.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapKind {
    ClipEdge,
    Playhead,
    Marker,
    InOut,
    Beat,
}

/// The times an edit may snap to, kept in time order. The snapping task fills it from
/// the sequence (clip edges, playhead, markers, in and out, beats); the view only
/// measures distances.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapTargets {
    targets: Vec<(Time, SnapKind)>,
}

impl SnapTargets {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, time: Time, kind: SnapKind) {
        let at = self.targets.partition_point(|(t, _)| *t <= time);
        self.targets.insert(at, (time, kind));
    }

    pub fn len(&self) -> usize {
        self.targets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Time, SnapKind)> + '_ {
        self.targets.iter().copied()
    }

    /// The target nearest to `time` (the earlier one on a tie).
    pub fn nearest(&self, time: Time) -> Option<(Time, SnapKind)> {
        let at = self.targets.partition_point(|(t, _)| *t < time);
        let after = self.targets.get(at).copied();
        let before = at.checked_sub(1).and_then(|i| self.targets.get(i)).copied();
        match (before, after) {
            (Some(b), Some(a)) => {
                let db = time.checked_sub(b.0).ok();
                let da = a.0.checked_sub(time).ok();
                match (db, da) {
                    (Some(db), Some(da)) if da < db => Some(a),
                    _ => Some(b),
                }
            }
            (b, a) => b.or(a),
        }
    }
}

/// Where an edit landed after snapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snap {
    pub time: Time,
    /// The target it caught, if any; otherwise the time is on the nearest frame.
    pub target: Option<SnapKind>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Animation {
    from: ViewPos,
    to: ViewPos,
    start: f64,
}

/// A minimap drag in progress: the pointer's offset into the window and the minimap's
/// scale when it began, so the mapping holds still while the view moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct WindowDrag {
    pub grab: f32,
    pub origin: f64,
    pub scale: f64,
}

/// The track area's view of a sequence; see the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineView {
    rate: Rational,
    pos: ViewPos,
    scroll_y: f32,
    extent: Extent,
    /// Zoomed to fit once the first real width arrives.
    placed: bool,
    animation: Option<Animation>,
    /// Jump instead of animating (the system's reduce-motion setting).
    pub reduced_motion: bool,
    pub(super) window_drag: Option<WindowDrag>,
}

impl TimelineView {
    /// A view of a sequence at `rate`; it zooms to fit when [`Self::set_extent`] first
    /// gives it a width.
    pub fn new(rate: Rational) -> Self {
        let rate = if rate.is_positive_rate() {
            rate
        } else {
            Rational::new(25, 1)
        };
        Self {
            rate,
            pos: ViewPos {
                start: 0.0,
                frame_px: 1.0,
            },
            scroll_y: 0.0,
            extent: Extent::default(),
            placed: false,
            animation: None,
            reduced_motion: false,
            window_drag: None,
        }
    }

    pub fn for_sequence(sequence: &Sequence) -> Self {
        Self::new(sequence.frame_rate)
    }

    pub fn rate(&self) -> Rational {
        self.rate
    }

    /// Frames per second.
    pub fn fps(&self) -> f64 {
        self.rate.to_f64().unwrap_or(25.0)
    }

    pub fn extent(&self) -> Extent {
        self.extent
    }

    /// Lays the view over a track area and sequence; call every frame before drawing.
    /// The first call with a width zooms to fit; later ones keep the position, held
    /// inside the new limits.
    pub fn set_extent(&mut self, extent: Extent) {
        self.extent = extent;
        if !self.placed && extent.width > 0.0 {
            self.placed = true;
            self.pos = self.fit();
            self.animation = None;
        }
        self.pos = self.clamp(self.pos);
        let limits = self.limits();
        if let Some(a) = &mut self.animation {
            a.to = clamp_pos(a.to, limits);
        }
        self.scroll_y = self.clamp_scroll(self.scroll_y);
    }

    /// The current position (mid-animation while one runs).
    pub fn pos(&self) -> ViewPos {
        self.pos
    }

    /// Where the view is heading: the animation's end, or the current position.
    pub fn target(&self) -> ViewPos {
        self.animation.map_or(self.pos, |a| a.to)
    }

    pub fn is_animating(&self) -> bool {
        self.animation.is_some()
    }

    /// Points per frame.
    pub fn frame_px(&self) -> f64 {
        self.pos.frame_px
    }

    /// Points per second.
    pub fn px_per_second(&self) -> f64 {
        self.pos.frame_px * self.fps()
    }

    /// The frame position at the left edge.
    pub fn start(&self) -> f64 {
        self.pos.start
    }

    /// Points the tracks are scrolled down (the headers' `scroll`).
    pub fn scroll_y(&self) -> f32 {
        self.scroll_y
    }

    /// The scrollable length in frames: the sequence, at least [`MIN_EXTENT_SECONDS`].
    pub fn extent_frames(&self) -> f64 {
        let min = frames_of(Time::seconds(MIN_EXTENT_SECONDS), self.rate);
        frames_of(self.extent.duration, self.rate).max(min)
    }

    /// The closest zoom: [`MAX_FRAME_WIDTH`] points per frame.
    pub fn max_frame_px(&self) -> f64 {
        MAX_FRAME_WIDTH
    }

    /// The zoom that shows the whole sequence with [`FIT_MARGIN`] either side, which is
    /// also the farthest zoom.
    pub fn fit_frame_px(&self) -> f64 {
        let room = f64::from((self.extent.width - 2.0 * FIT_MARGIN).max(1.0));
        (room / self.extent_frames()).min(MAX_FRAME_WIDTH)
    }

    pub fn min_frame_px(&self) -> f64 {
        self.fit_frame_px()
    }

    /// Frames across the track area at `frame_px`.
    fn span_at(&self, frame_px: f64) -> f64 {
        f64::from(self.extent.width.max(0.0)) / frame_px
    }

    /// Frames across the track area.
    pub fn visible_frames(&self) -> f64 {
        self.span_at(self.pos.frame_px)
    }

    fn limits(&self) -> Limits {
        Limits {
            min_px: self.min_frame_px(),
            max_px: self.max_frame_px(),
            extent: self.extent_frames(),
            width: f64::from(self.extent.width.max(0.0)),
        }
    }

    /// `pos` held to the zoom limits and the left edge's range, which runs on whole
    /// frames from a margin before zero to where the end sits mid-view.
    pub fn clamp(&self, pos: ViewPos) -> ViewPos {
        clamp_pos(pos, self.limits())
    }

    /// The left edge's range at `frame_px`, in whole frames.
    pub fn start_bounds(&self, frame_px: f64) -> (f64, f64) {
        self.limits().start_bounds(frame_px)
    }

    // Conversions. x is in points from the track area's left edge, y in points from the
    // top of its first visible row.

    /// The x of a time.
    pub fn time_to_x(&self, t: Time) -> f32 {
        ((frames_of(t, self.rate) - self.pos.start) * self.pos.frame_px) as f32
    }

    /// The x of a frame boundary.
    pub fn frame_to_x(&self, frame: i64) -> f32 {
        ((frame as f64 - self.pos.start) * self.pos.frame_px) as f32
    }

    /// The frame position (fractional) at `x`.
    pub fn x_to_frames(&self, x: f32) -> f64 {
        self.pos.start + f64::from(x) / self.pos.frame_px
    }

    /// The frame boundary nearest to `x` (halves round up).
    pub fn x_to_frame(&self, x: f32) -> i64 {
        (self.x_to_frames(x) + 0.5).floor() as i64
    }

    /// The frame whose cell holds `x`.
    pub fn frame_at_x(&self, x: f32) -> i64 {
        self.x_to_frames(x).floor() as i64
    }

    /// The time of the frame boundary nearest to `x`, exact.
    pub fn x_to_time(&self, x: f32) -> Time {
        self.frame_time(self.x_to_frame(x))
    }

    /// The exact time of frame `frame`'s start.
    pub fn frame_time(&self, frame: i64) -> Time {
        Time::from_frames(frame, self.rate).unwrap_or(Time::ZERO)
    }

    /// The whole frames the track area shows (cut ones included), as a time range.
    pub fn visible_range(&self) -> TimeRange {
        let first = self.frame_at_x(0.0);
        let last = self.x_to_frames(self.extent.width).ceil() as i64;
        let (a, b) = (self.frame_time(first), self.frame_time(last.max(first)));
        TimeRange::between(a, b).unwrap_or(TimeRange::new(a, Time::ZERO))
    }

    /// The length of `points` on screen, in seconds (for tolerances).
    pub fn seconds_of(&self, points: f32) -> f64 {
        f64::from(points) / self.px_per_second()
    }

    /// `track`'s row on screen, from the top of the track area.
    pub fn track_to_y(&self, layout: &TrackLayout, track: TrackId) -> Option<Rangef> {
        layout
            .span(track)
            .map(|r| Rangef::new(r.min - self.scroll_y, r.max - self.scroll_y))
    }

    /// The track under `y` (from the top of the track area).
    pub fn y_to_track(&self, layout: &TrackLayout, y: f32) -> Option<TrackId> {
        layout.track_at(y + self.scroll_y)
    }

    /// `time` moved to the nearest snap target within [`SNAP_DISTANCE`] points, or else
    /// to the nearest frame boundary.
    pub fn snap(&self, time: Time, targets: &SnapTargets) -> Snap {
        let x = self.time_to_x(time);
        if let Some((t, kind)) = targets.nearest(time)
            && (self.time_to_x(t) - x).abs() <= SNAP_DISTANCE
        {
            return Snap {
                time: t,
                target: Some(kind),
            };
        }
        let frame = (frames_of(time, self.rate) + 0.5).floor() as i64;
        Snap {
            time: self.frame_time(frame),
            target: None,
        }
    }

    // Gestures: immediate, and they stop an animation where it is.

    /// Moves to `pos` (held to the limits) at once.
    pub fn set_pos(&mut self, pos: ViewPos) {
        self.animation = None;
        self.pos = self.clamp(pos);
    }

    /// Multiplies the zoom by `factor` about `anchor_x`: the time under the anchor stays
    /// put unless a limit stops it.
    pub fn zoom_at(&mut self, factor: f64, anchor_x: f32) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let pos = zoom_about(self.pos, factor, anchor_x, self.limits());
        self.set_pos(pos);
    }

    /// Moves the content by `dx` and `dy` points (positive: right and down, as egui's
    /// scroll deltas).
    pub fn pan(&mut self, dx: f32, dy: f32) {
        if dx != 0.0 {
            let pos = ViewPos {
                start: self.pos.start - f64::from(dx) / self.pos.frame_px,
                ..self.pos
            };
            self.set_pos(pos);
        }
        if dy != 0.0 {
            self.set_scroll_y(self.scroll_y - dy);
        }
    }

    pub fn set_scroll_y(&mut self, y: f32) {
        self.scroll_y = self.clamp_scroll(y);
    }

    fn clamp_scroll(&self, y: f32) -> f32 {
        let max = (self.extent.content_height - self.extent.height).max(0.0);
        y.clamp(0.0, max)
    }

    /// Puts the left edge on frame `frame` (held to the limits) at once.
    pub fn scroll_to_frame(&mut self, frame: i64) {
        self.set_pos(ViewPos {
            start: frame as f64,
            frame_px: self.pos.frame_px,
        });
    }

    /// Sets the zoom at once, about the middle of the track area.
    pub fn set_frame_px(&mut self, frame_px: f64) {
        let factor = frame_px / self.pos.frame_px;
        self.zoom_at(factor, self.extent.width / 2.0);
    }

    // Animated moves.

    /// The position that shows the whole sequence with [`FIT_MARGIN`] either side, its
    /// left edge on a frame boundary.
    pub fn fit(&self) -> ViewPos {
        let frame_px = self.fit_frame_px();
        let start = (-f64::from(FIT_MARGIN) / frame_px).floor();
        self.clamp(ViewPos { start, frame_px })
    }

    /// The position that shows `range` with [`FIT_MARGIN`] either side (centred when
    /// that would zoom past the closest zoom), its left edge on a frame boundary.
    pub fn range_pos(&self, range: TimeRange) -> ViewPos {
        let a = frames_of(range.start, self.rate);
        let b = range
            .end()
            .map_or(a, |end| frames_of(end, self.rate))
            .max(a);
        let room = f64::from((self.extent.width - 2.0 * FIT_MARGIN).max(1.0));
        let wanted = room / (b - a).max(1.0);
        let frame_px = wanted.clamp(self.min_frame_px(), self.max_frame_px());
        let start = if wanted > frame_px {
            (a + b) / 2.0 - self.span_at(frame_px) / 2.0
        } else {
            a - f64::from(FIT_MARGIN) / frame_px
        };
        self.clamp(ViewPos {
            start: start.floor(),
            frame_px,
        })
    }

    /// Animates to the whole sequence.
    pub fn zoom_to_fit(&mut self, now: f64) {
        self.animate_to(self.fit(), now);
    }

    /// Animates to `range` (zoom to selection; the caller gives the selection's span).
    pub fn zoom_to_range(&mut self, range: TimeRange, now: f64) {
        self.animate_to(self.range_pos(range), now);
    }

    /// Zooms in (`steps` > 0) or out by [`ZOOM_STEP`] per step about `anchor` when it is
    /// on screen (the playhead), else about the middle; repeated steps build on the
    /// running animation's end.
    pub fn zoom_step(&mut self, steps: i32, anchor: Option<Time>, now: f64) {
        let from = self.target();
        let width = self.extent.width;
        let anchor_x = anchor
            .map(|t| ((frames_of(t, self.rate) - from.start) * from.frame_px) as f32)
            .filter(|x| (0.0..=width).contains(x))
            .unwrap_or(width / 2.0);
        let to = zoom_about(from, ZOOM_STEP.powi(steps), anchor_x, self.limits());
        self.animate_to(to, now);
    }

    /// Runs a view command from the keymap; returns whether it was one. `anchor` is the
    /// playhead, for keyboard zoom.
    pub fn run_command(&mut self, id: &str, anchor: Option<Time>, now: f64) -> bool {
        match id {
            "view.zoom_in" => self.zoom_step(1, anchor, now),
            "view.zoom_out" => self.zoom_step(-1, anchor, now),
            "view.zoom_to_fit" => self.zoom_to_fit(now),
            _ => return false,
        }
        true
    }

    /// Starts an animation from the current position to `to` (held to the limits), or
    /// jumps there under reduced motion.
    pub fn animate_to(&mut self, to: ViewPos, now: f64) {
        let to = self.clamp(to);
        if self.reduced_motion || to == self.pos {
            self.animation = None;
            self.pos = to;
            return;
        }
        self.animation = Some(Animation {
            from: self.pos,
            to,
            start: now,
        });
    }

    /// Moves a running animation to `now`; returns whether it is still running.
    pub fn advance(&mut self, now: f64) -> bool {
        let Some(a) = self.animation else {
            return false;
        };
        let u = ((now - a.start) / ANIMATION_SECONDS).clamp(0.0, 1.0);
        if u >= 1.0 {
            self.pos = a.to;
            self.animation = None;
            return false;
        }
        let eased = 1.0 - (1.0 - u).powi(3);
        self.pos = interpolate(a.from, a.to, eased);
        true
    }

    /// Advances the animation and applies this frame's zoom and scroll input while the
    /// pointer is over `hover` (the headers and tracks); zoom anchors at the pointer's x
    /// from `tracks_left`. Returns whether the view moved.
    pub fn handle_input(&mut self, ui: &Ui, hover: Rect, tracks_left: f32) -> bool {
        let before = (self.pos, self.scroll_y);
        let now = ui.input(|i| i.time);
        if self.advance(now) {
            ui.ctx().request_repaint();
        }
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if let Some(p) = pointer.filter(|p| hover.contains(*p))
            && ui.rect_contains_pointer(hover)
        {
            let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
            let gesture = ui.input(|i| {
                let mut g = gesture(&i.events, line, hover.height());
                if let Some(touch) = i.multi_touch() {
                    g.zoom *= f64::from(touch.zoom_delta);
                }
                g
            });
            if gesture.zoom != 1.0 {
                self.zoom_at(gesture.zoom, p.x - tracks_left);
            }
            if gesture.pan != Vec2::ZERO {
                self.pan(gesture.pan.x, gesture.pan.y);
            }
            if gesture.zoom != 1.0 || gesture.pan != Vec2::ZERO {
                ui.ctx().input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
            }
        }
        (self.pos, self.scroll_y) != before
    }
}

/// This frame's zoom factor and pan from raw wheel and pinch events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gesture {
    pub zoom: f64,
    pub pan: Vec2,
}

/// Reads wheel and pinch events: Ctrl, ⌘ or Alt with a wheel, or a pinch, zooms (up or
/// right zooms in); Shift makes a wheel pan horizontally; otherwise a wheel pans as it
/// scrolls. Line and page steps become points.
pub fn gesture(events: &[Event], line_points: f32, page_points: f32) -> Gesture {
    let mut g = Gesture {
        zoom: 1.0,
        pan: Vec2::ZERO,
    };
    for event in events {
        match event {
            Event::Zoom(factor) => g.zoom *= f64::from(*factor),
            Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } => {
                let delta = match unit {
                    MouseWheelUnit::Point => *delta,
                    MouseWheelUnit::Line => *delta * line_points,
                    MouseWheelUnit::Page => *delta * page_points,
                };
                if modifiers.ctrl || modifiers.command || modifiers.mac_cmd || modifiers.alt {
                    g.zoom *= (ZOOM_PER_POINT * f64::from(delta.x + delta.y)).exp();
                } else if modifiers.shift {
                    g.pan.x += delta.x + delta.y;
                } else {
                    g.pan += delta;
                }
            }
            _ => {}
        }
    }
    g
}

#[derive(Clone, Copy, Debug)]
struct Limits {
    min_px: f64,
    max_px: f64,
    extent: f64,
    width: f64,
}

impl Limits {
    fn start_bounds(self, frame_px: f64) -> (f64, f64) {
        let lo = (-f64::from(FIT_MARGIN) / frame_px).floor();
        let hi = (self.extent - self.width / frame_px / 2.0).ceil().max(lo);
        (lo, hi)
    }
}

fn clamp_pos(pos: ViewPos, limits: Limits) -> ViewPos {
    let frame_px = if pos.frame_px.is_finite() {
        pos.frame_px
            .clamp(limits.min_px, limits.max_px.max(limits.min_px))
    } else {
        limits.max_px
    };
    let (lo, hi) = limits.start_bounds(frame_px);
    let start = if pos.start.is_finite() { pos.start } else { lo };
    ViewPos {
        start: start.clamp(lo, hi),
        frame_px,
    }
}

/// `pos` zoomed by `factor` about `anchor_x`, held to the limits.
fn zoom_about(pos: ViewPos, factor: f64, anchor_x: f32, limits: Limits) -> ViewPos {
    let anchor = pos.start + f64::from(anchor_x) / pos.frame_px;
    let frame_px = (pos.frame_px * factor).clamp(limits.min_px, limits.max_px.max(limits.min_px));
    clamp_pos(
        ViewPos {
            start: anchor - f64::from(anchor_x) / frame_px,
            frame_px,
        },
        limits,
    )
}

/// The view `u` of the way from `a` to `b`: the zoom moves evenly in log scale about the
/// screen point both views agree on, so a zoom about a point keeps that point still; a
/// pure pan moves evenly.
fn interpolate(a: ViewPos, b: ViewPos, u: f64) -> ViewPos {
    if u >= 1.0 {
        return b;
    }
    let frame_px = (a.frame_px.ln() * (1.0 - u) + b.frame_px.ln() * u).exp();
    let inv = 1.0 / a.frame_px - 1.0 / b.frame_px;
    let start = if inv.abs() <= 1e-9 / a.frame_px {
        a.start + (b.start - a.start) * u
    } else {
        // a.start + x / a.px == b.start + x / b.px at this x.
        let x = (b.start - a.start) / inv;
        let frame = a.start + x / a.frame_px;
        frame - x / frame_px
    };
    ViewPos { start, frame_px }
}

/// The zoom slider: farthest zoom on the left, closest on the right, on a log scale.
/// Dragging or clicking zooms about the middle of the track area at once. Returns the
/// slider's response.
pub fn zoom_slider(ui: &mut Ui, rect: Rect, view: &mut TimelineView) -> Response {
    let id = Id::new("glass.timeline.zoom");
    let response = ui.interact(rect, id, Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::slider(true, view.zoom_fraction(), t!("timeline.zoom")));
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect.expand(1.0));
    let thumb = 12.0;
    let track = rect.shrink2(vec2(thumb / 2.0, 0.0));
    if let Some(p) = response.interact_pointer_pos()
        && (response.dragged() || response.clicked())
    {
        let u = f64::from(((p.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0));
        view.set_zoom_fraction(u);
    }
    let bar = Rect::from_center_size(track.center(), vec2(track.width(), 4.0));
    let u = view.zoom_fraction() as f32;
    paint.progress(&painter, bar, u, Role::Bg3);
    let centre = pos2(track.left() + u * track.width(), track.center().y);
    let hovered = response.hovered() || response.dragged();
    paint.dot(
        &painter,
        centre,
        thumb,
        paint.c(if hovered {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        }),
    );
    if response.has_focus() {
        painter.circle_stroke(
            centre,
            thumb / 2.0 + 2.0,
            Stroke::new(
                paint.theme.tokens().rules.focus_ring.width_px,
                paint.c(Role::Accent),
            ),
        );
    }
    response.on_hover_text(t!("timeline.zoom"))
}

impl TimelineView {
    /// The zoom as a fraction from the farthest (0) to the closest (1), on a log scale.
    pub fn zoom_fraction(&self) -> f64 {
        let (lo, hi) = (self.min_frame_px().ln(), self.max_frame_px().ln());
        if hi - lo <= f64::EPSILON {
            return 1.0;
        }
        ((self.pos.frame_px.ln() - lo) / (hi - lo)).clamp(0.0, 1.0)
    }

    /// Sets the zoom from a [`Self::zoom_fraction`] at once, about the middle.
    pub fn set_zoom_fraction(&mut self, u: f64) {
        let (lo, hi) = (self.min_frame_px().ln(), self.max_frame_px().ln());
        self.set_frame_px((lo + u.clamp(0.0, 1.0) * (hi - lo)).exp());
    }
}

/// The track lanes behind the clips: `bg/0`, with each row's bottom edge, lined up with
/// the header rows.
pub fn lanes(ui: &Ui, rect: Rect, view: &TimelineView, layout: &TrackLayout) {
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, paint.c(Role::Bg0));
    let stroke = Stroke::new(1.0, paint.c(Role::BorderSubtle));
    for (track, _) in layout.iter() {
        let Some(span) = view.track_to_y(layout, track) else {
            continue;
        };
        let bottom = rect.top() + span.max;
        if bottom < rect.top() || span.min + rect.top() > rect.bottom() {
            continue;
        }
        painter.hline(rect.x_range(), bottom - 0.5, stroke);
    }
    // The sequence's end, so empty space after it reads as such.
    let end = rect.left() + view.time_to_x(view.extent().duration);
    if end > rect.left() && end < rect.right() {
        painter.rect_filled(
            Rect::from_min_max(pos2(end, rect.top()), rect.max),
            CornerRadius::ZERO,
            paint.mix(Role::Bg1, 0.5),
        );
    }
}

/// The tracks' vertical scrollbar in `rect` (a [`SCROLLBAR_WIDTH`] strip right of the
/// tracks): a thumb only while the tracks are taller than the area, dragged to scroll.
pub fn track_scrollbar(ui: &mut Ui, rect: Rect, view: &mut TimelineView) -> Response {
    let id = Id::new("glass.timeline.track_scrollbar");
    let extent = view.extent();
    let overflow = extent.content_height > extent.height && extent.content_height > 0.0;
    let response = ui.interact(
        rect,
        id,
        if overflow {
            Sense::drag()
        } else {
            Sense::hover()
        },
    );
    if !overflow {
        return response;
    }
    let ratio = rect.height() / extent.content_height;
    if response.dragged() {
        view.set_scroll_y(view.scroll_y() + response.drag_delta().y / ratio);
    }
    let paint = Paint::new(ui.ctx());
    let thumb_h = (extent.height * ratio).max(16.0).min(rect.height());
    let travel = rect.height() - thumb_h;
    let max_scroll = (extent.content_height - extent.height).max(1.0);
    let top = rect.top() + travel * (view.scroll_y() / max_scroll);
    let thumb = Rect::from_min_size(pos2(rect.center().x - 3.0, top), vec2(6.0, thumb_h))
        .shrink2(vec2(0.0, 2.0));
    let role = if response.hovered() || response.dragged() {
        Role::TextSecondary
    } else {
        Role::TextMuted
    };
    ui.painter_at(rect)
        .rect_filled(thumb, CornerRadius::same(3), paint.mix(role, 0.6));
    response
}
