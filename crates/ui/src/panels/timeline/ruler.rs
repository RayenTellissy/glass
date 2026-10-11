//! The timeline's ruler: timecode labels over ticks whose spacing follows the zoom, from
//! single frames through seconds to minutes and hours, counted in the sequence's
//! timecode (drop-frame at 29.97 and 59.94 when the sequence says so).
//!
//! Ticks sit on timecode values, not on whole seconds of real time: at 29.97 fps
//! non-drop a "second" is 30 frames, and with drop-frame a minute tick lands on the
//! minute's first frame (`00:01:00;02`, since `;00` and `;01` do not exist there).
//! [`tick_spacing`] and [`ticks`] are pure, so the choice is tested at many zooms;
//! [`ruler`] draws them and returns the strip's response for the playhead to scrub with.

use egui::{Align2, Id, Rect, Response, Sense, Stroke, Ui, WidgetInfo, WidgetType, pos2};
use project::{Rational, Sequence};

use super::view::TimelineView;
use crate::panels::widgets::{Paint, Type};
use crate::t;
use crate::theme::Role;

/// The ruler's height (the `row/pro` token).
pub const RULER_HEIGHT: f32 = 28.0;
/// The least room between two minor ticks.
pub const MIN_MINOR_PX: f64 = 6.0;
/// Room left between a label and the next major tick.
const LABEL_GAP: f32 = 16.0;
/// A label's distance from its tick.
const LABEL_INSET: f32 = 4.0;
/// Minor ticks rise this far from the bottom edge.
const MINOR_TICK: f32 = 6.0;
/// The major tick a label belongs to rises this far.
const MAJOR_TICK: f32 = 12.0;

/// How a sequence counts timecode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timecode {
    rate: Rational,
    drop_frame: bool,
}

impl Timecode {
    /// Drop-frame only where the rate allows it (29.97 and 59.94), else non-drop.
    pub fn new(rate: Rational, drop_frame: bool) -> Self {
        Self {
            rate,
            drop_frame: drop_frame && project::settings::allows_drop_frame(rate),
        }
    }

    pub fn for_sequence(sequence: &Sequence) -> Self {
        Self::new(sequence.frame_rate, sequence.drop_frame)
    }

    pub fn rate(self) -> Rational {
        self.rate
    }

    pub fn is_drop_frame(self) -> bool {
        self.drop_frame
    }

    /// Frames per timecode second: the rate rounded (30 at 29.97, 24 at 23.976).
    pub fn nominal(self) -> i64 {
        let (num, den) = (i64::from(self.rate.num), i64::from(self.rate.den));
        if num <= 0 || den <= 0 {
            return 1;
        }
        ((num + den / 2) / den).max(1)
    }

    /// Frame numbers skipped at the start of each minute but every tenth (2 at 29.97, 4
    /// at 59.94), zero for non-drop.
    fn dropped(self) -> i64 {
        if self.drop_frame {
            self.nominal() / 15
        } else {
            0
        }
    }

    /// The timecode count of `frame`: the number its label spells out in frames, which
    /// skips the dropped numbers.
    pub fn label_number(self, frame: i64) -> i64 {
        if frame < 0 {
            return -self.label_number(-frame);
        }
        let d = self.dropped();
        if d == 0 {
            return frame;
        }
        let per_minute = 60 * self.nominal() - d;
        let per_ten = 10 * per_minute + d;
        let (tens, rest) = (frame / per_ten, frame % per_ten);
        let mut label = frame + 9 * d * tens;
        if rest >= d {
            label += d * ((rest - d) / per_minute);
        }
        label
    }

    /// The frame showing timecode count `label`; a dropped count gives the first frame
    /// of its minute.
    pub fn frame_of_label(self, label: i64) -> i64 {
        if label < 0 {
            return -self.frame_of_label(-label);
        }
        let d = self.dropped();
        if d == 0 {
            return label;
        }
        let per_minute = 60 * self.nominal();
        let minutes = label / per_minute;
        let mut label = label;
        if minutes % 10 != 0 && label % per_minute < d {
            label += d - label % per_minute;
        }
        label - d * (minutes - minutes / 10)
    }

    /// `HH:MM:SS:FF`, with `;` before the frames in drop-frame; negative frames get a
    /// leading minus. Hours keep counting past 24.
    pub fn format(self, frame: i64) -> String {
        if frame < 0 {
            return format!("-{}", self.format(-frame));
        }
        let n = self.nominal();
        let label = self.label_number(frame);
        let (ff, seconds) = (label % n, label / n);
        let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
        let sep = if self.drop_frame { ';' } else { ':' };
        format!("{h:02}:{m:02}:{s:02}{sep}{ff:02}")
    }

    /// Frames per timecode count, about 0.999 in drop-frame and 1 otherwise.
    pub fn frames_per_label(self) -> f64 {
        if self.drop_frame {
            self.rate.to_f64().unwrap_or(1.0) / self.nominal() as f64
        } else {
            1.0
        }
    }
}

/// Tick steps in timecode counts: labels every `major`, ticks every `minor` (which
/// divides it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickSpacing {
    pub major: i64,
    pub minor: i64,
}

/// The steps a ruler may use at `nominal` frames per second, smallest first: the
/// divisors of a second in frames, then 1, 2, 5, 10, 15 and 30 seconds and minutes,
/// then hours.
pub fn steps(nominal: i64) -> Vec<i64> {
    let n = nominal.max(1);
    let mut steps: Vec<i64> = (1..n).filter(|d| n % d == 0).collect();
    for unit in [n, 60 * n] {
        steps.extend([1, 2, 5, 10, 15, 30].map(|k| k * unit));
    }
    steps.extend([1, 2, 3, 6, 12, 24].map(|k| k * 3600 * n));
    steps
}

/// The spacing at `label_px` points per timecode count: the smallest step whose labels
/// are at least `min_major_px` apart, and inside it the smallest step that divides it
/// into at most ten parts at least `min_minor_px` apart (the major step itself when
/// none does).
pub fn tick_spacing(
    label_px: f64,
    nominal: i64,
    min_major_px: f64,
    min_minor_px: f64,
) -> TickSpacing {
    let steps = steps(nominal);
    let wide = |step: i64, min: f64| step as f64 * label_px >= min;
    let major = match steps.iter().find(|s| wide(**s, min_major_px)) {
        Some(s) => *s,
        None => {
            // Beyond a day per label: keep doubling the largest step.
            let mut s = *steps.last().unwrap_or(&1);
            while !wide(s, min_major_px) && s < i64::MAX / 4 {
                s *= 2;
            }
            s
        }
    };
    let minor = steps
        .iter()
        .copied()
        .filter(|s| *s < major && major % s == 0 && major / s <= 10)
        .find(|s| wide(*s, min_minor_px))
        .unwrap_or(major);
    TickSpacing { major, minor }
}

/// One tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    /// The frame it marks the start of.
    pub frame: i64,
    /// Labelled.
    pub major: bool,
}

/// The ticks from `first` to `last` frame (both included), starting one major step
/// early so a label cut by the left edge still shows; nothing before frame 0. Ticks are
/// in frame order and never share a frame.
pub fn ticks(tc: Timecode, spacing: TickSpacing, first: i64, last: i64) -> Vec<Tick> {
    let mut out = Vec::new();
    let first = first.max(0);
    if last < first {
        return out;
    }
    let minor = spacing.minor.max(1);
    let major = spacing.major.max(minor);
    let mut label = tc.label_number(first) / major * major;
    let mut previous = None;
    loop {
        let frame = tc.frame_of_label(label);
        if frame > last {
            break;
        }
        if previous != Some(frame) {
            out.push(Tick {
                frame,
                major: label % major == 0,
            });
            previous = Some(frame);
        }
        label += minor;
    }
    out
}

/// The egui id of the ruler strip.
pub fn ruler_id() -> Id {
    Id::new("glass.timeline.ruler")
}

/// Draws the ruler over the track area `rect` (as wide as the tracks, aligned with
/// them) and returns its response (click and drag, for the playhead).
pub fn ruler(ui: &mut Ui, rect: Rect, view: &TimelineView, tc: Timecode) -> Response {
    let response = ui.interact(rect, ruler_id(), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, t!("timeline.ruler")));
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, paint.c(Role::Bg1));

    let first = view.frame_at_x(0.0);
    let last = view.frame_at_x(rect.width()) + 1;
    // The widest label on screen sets the spacing, so labels never overlap.
    let widest = paint.width(&painter, &tc.format(last.max(0)), Type::MonoCaption);
    let spacing = tick_spacing(
        view.frame_px() * tc.frames_per_label(),
        tc.nominal(),
        f64::from(widest + LABEL_GAP),
        MIN_MINOR_PX,
    );
    let stroke = Stroke::new(1.0, paint.c(Role::BorderStrong));
    let label_y = rect.top() + (rect.height() - MAJOR_TICK) / 2.0;
    for tick in ticks(tc, spacing, first, last) {
        let x = (rect.left() + view.frame_to_x(tick.frame)).round() + 0.5;
        if tick.major {
            painter.vline(x, (rect.bottom() - MAJOR_TICK)..=rect.bottom(), stroke);
            paint.text(
                &painter,
                pos2(x + LABEL_INSET, label_y),
                Align2::LEFT_CENTER,
                &tc.format(tick.frame),
                Type::MonoCaption,
                Role::TextSecondary,
                f32::INFINITY,
            );
        } else {
            painter.vline(x, (rect.bottom() - MINOR_TICK)..=rect.bottom(), stroke);
        }
    }
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
    response
}
