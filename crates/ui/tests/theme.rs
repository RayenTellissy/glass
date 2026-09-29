//! Every bundled theme applies to a real egui context, switches at run time, and draws a
//! themed editor frame headless within the frame budget.

use egui::{Rect, Sense, Stroke, StrokeKind, vec2};
use ui::frame_time::FrameTimeMonitor;
use ui::headless::{HeadlessUi, Viewport};
use ui::t;
use ui::theme::{self, Role, ThemeKind};

const WARMUP: usize = 30;
const FRAMES: usize = 240;
const CLIPS: [Role; 6] = [
    Role::ClipVideo,
    Role::ClipAudio,
    Role::ClipTitle,
    Role::ClipAdjustment,
    Role::ClipSubtitle,
    Role::ClipOffline,
];

/// Real widgets (so the egui style is exercised) plus a 200-clip timeline painted from
/// theme roles.
fn themed_frame(ui: &mut egui::Ui, frame: usize, text: bool) {
    let theme = theme::current(ui.ctx());
    egui::Panel::top("top").show(ui, |ui| {
        ui.horizontal(|ui| {
            for _ in 0..6 {
                if text {
                    let _ = ui.button(t!("app.name"));
                } else {
                    ui.allocate_exact_size(vec2(64.0, 24.0), Sense::click());
                }
            }
        });
    });
    egui::Panel::left("inspector")
        .exact_size(260.0)
        .show(ui, |ui| {
            for i in 0..12 {
                if text {
                    ui.label(t!("app.name"));
                    let mut on = (frame + i).is_multiple_of(2);
                    ui.checkbox(&mut on, t!("app.name"));
                } else {
                    ui.separator();
                }
            }
        });
    egui::CentralPanel::default().show(ui, |ui| {
        let area = ui.available_rect_before_wrap();
        let painter = ui.painter_at(area);
        let track_h = area.height() / 10.0;
        let clip_w = area.width() / 20.0;
        for track in 0..10 {
            for clip in 0..20 {
                let min = area.min + vec2(clip as f32 * clip_w, track as f32 * track_h);
                let rect = Rect::from_min_size(min, vec2(clip_w - 2.0, track_h - 4.0));
                painter.rect(
                    rect,
                    theme.tokens().radius.control,
                    theme.color(CLIPS[(track + clip) % CLIPS.len()]),
                    Stroke::new(1.0, theme.color(Role::BorderSubtle)),
                    StrokeKind::Inside,
                );
            }
        }
        let x = area.left() + (frame % 600) as f32 / 600.0 * area.width();
        painter.vline(
            x,
            area.y_range(),
            Stroke::new(2.0, theme.color(Role::Accent)),
        );
    });
}

#[test]
fn every_theme_renders_within_the_frame_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("themed frame budget", Viewport::default())
    else {
        return;
    };
    let text = headless.has_text();
    eprintln!("themed frame budget: fonts {}", headless.font_source());
    for kind in ThemeKind::ALL {
        theme::set(headless.context(), kind);
        let mut monitor = FrameTimeMonitor::default();
        headless
            .run(WARMUP, FRAMES, &mut monitor, |ui, frame| {
                themed_frame(ui, frame, text)
            })
            .expect("headless frames");
        let stats = monitor.stats();
        eprintln!(
            "themed frame budget, {} ({}): {stats}",
            kind.name(),
            headless.adapter()
        );
        assert_eq!(stats.frames_in_window, FRAMES);
        stats.assert_p99_within_budget();
    }
    if text {
        // The loaded font really has glyphs: the name lays out wider than a few points.
        let color = theme::current(headless.context()).color(Role::TextPrimary);
        let font = egui::FontId::proportional(14.0);
        let width = headless
            .context()
            .fonts_mut(|fonts| fonts.layout_no_wrap(t!("app.name"), font, color).size().x);
        assert!(width > 20.0, "text width {width}");
        for kind in ThemeKind::ALL {
            assert!(
                !theme::label(kind).starts_with('⟦'),
                "{kind:?} label key missing"
            );
        }
    }
}

#[test]
fn themes_switch_at_run_time() {
    let ctx = egui::Context::default();
    for kind in [ThemeKind::Light, ThemeKind::HighContrast, ThemeKind::Dark] {
        theme::set(&ctx, kind);
        let current = theme::current(&ctx);
        assert_eq!(current.kind(), kind);
        let style = ctx.global_style();
        assert_eq!(
            style.visuals.panel_fill,
            current.color(Role::Bg1),
            "{kind:?}"
        );
        assert_eq!(
            style.visuals.widgets.noninteractive.bg_stroke.color,
            current.color(Role::BorderSubtle),
            "{kind:?}"
        );
        assert_eq!(style.visuals.dark_mode, kind != ThemeKind::Light);
    }
    // High contrast draws every divider in border/strong.
    let hc = theme::builtin(ThemeKind::HighContrast);
    assert_eq!(hc.color(Role::BorderSubtle), hc.color(Role::BorderStrong));
}
