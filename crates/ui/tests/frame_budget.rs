//! The UI frame budget gate: a representative editor frame, drawn headless, must keep
//! its p99 under the frame-time monitor's budget. Later UI tasks add their own screens
//! the same way: build a `HeadlessUi`, run frames into a `FrameTimeMonitor`, assert.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, vec2};
use ui::frame_time::FrameTimeMonitor;
use ui::headless::{HeadlessUi, Viewport};

const TRACKS: usize = 10;
const CLIPS_PER_TRACK: usize = 20;
const WARMUP: usize = 30;
const FRAMES: usize = 240;

/// A top bar, a side panel of controls and a 200-clip timeline with a moving playhead,
/// the proto-ui scene. Colours come from the egui style until theme tokens exist; the
/// labels are fixtures, not user-facing text.
fn editor_frame(ui: &mut egui::Ui, frame: usize, text: bool) {
    let visuals = ui.visuals().clone();
    egui::Panel::top("top").show(ui, |ui| {
        ui.horizontal(|ui| {
            for i in 0..8 {
                let (rect, _) = ui.allocate_exact_size(vec2(64.0, 24.0), Sense::click());
                ui.painter()
                    .rect_filled(rect, 4.0, visuals.widgets.inactive.weak_bg_fill);
                if text {
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        format!("tool {i}"),
                        egui::FontId::proportional(12.0),
                        visuals.text_color(),
                    );
                }
            }
        });
    });
    egui::Panel::left("inspector")
        .exact_size(260.0)
        .show(ui, |ui| {
            for i in 0..12 {
                let (rect, _) = ui.allocate_exact_size(vec2(240.0, 28.0), Sense::hover());
                ui.painter()
                    .rect_filled(rect, 4.0, visuals.widgets.noninteractive.bg_fill);
                if text {
                    ui.painter().text(
                        rect.left_center() + vec2(8.0, 0.0),
                        Align2::LEFT_CENTER,
                        format!("property {i}: {:.2}", (frame + i) as f32 * 0.01),
                        egui::FontId::monospace(12.0),
                        visuals.text_color(),
                    );
                }
            }
        });
    egui::CentralPanel::default().show(ui, |ui| {
        let area = ui.available_rect_before_wrap();
        let painter = ui.painter_at(area);
        let track_h = area.height() / TRACKS as f32;
        let clip_w = area.width() / CLIPS_PER_TRACK as f32;
        for track in 0..TRACKS {
            for clip in 0..CLIPS_PER_TRACK {
                let min = area.min + vec2(clip as f32 * clip_w, track as f32 * track_h);
                let rect = Rect::from_min_size(min, vec2(clip_w - 2.0, track_h - 4.0));
                painter.rect(
                    rect,
                    4.0,
                    visuals.widgets.inactive.bg_fill,
                    Stroke::new(1.0, visuals.widgets.inactive.bg_stroke.color),
                    StrokeKind::Inside,
                );
                if text {
                    painter.text(
                        rect.left_top() + vec2(4.0, 4.0),
                        Align2::LEFT_TOP,
                        format!("clip {track}.{clip}"),
                        egui::FontId::proportional(11.0),
                        visuals.text_color(),
                    );
                }
            }
        }
        let x = area.left() + (frame % 600) as f32 / 600.0 * area.width();
        painter.vline(
            x,
            area.y_range(),
            Stroke::new(2.0, visuals.selection.stroke.color),
        );
    });
}

#[test]
fn editor_frame_p99_is_within_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("editor frame budget", Viewport::default())
    else {
        return;
    };
    let text = headless.has_text();
    if !text {
        eprintln!("editor frame budget: no fonts found, drawing shapes without text");
    }
    let mut monitor = FrameTimeMonitor::default();
    headless
        .run(WARMUP, FRAMES, &mut monitor, |ui, frame| {
            editor_frame(ui, frame, text)
        })
        .expect("headless frames");
    let stats = monitor.stats();
    eprintln!("editor frame budget ({}): {stats}", headless.adapter());
    assert_eq!(stats.frames_in_window, FRAMES);
    stats.assert_p99_within_budget();
}
