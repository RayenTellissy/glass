//! The registered cases, one explicit list. A panel, dialog or parameter widget adds a
//! line to [`all`] pointing at a draw function with realistic sample content (its own
//! `snapshot_cases()` or a function here that calls its public API).

use egui::{Align2, Rect, RichText, Sense, Stroke, StrokeKind, vec2};

use super::SnapshotCase;
use crate::t;
use crate::theme::{self, Role};

pub(super) fn all() -> Vec<SnapshotCase> {
    vec![
        SnapshotCase {
            name: "theme_showcase",
            module: "theme",
            size: vec2(480.0, 320.0),
            draw: theme_showcase,
        },
        SnapshotCase {
            name: "sample_timeline",
            module: "theme",
            size: vec2(560.0, 200.0),
            draw: sample_timeline,
        },
    ]
}

const CLIP_ROLES: [Role; 6] = [
    Role::ClipVideo,
    Role::ClipAudio,
    Role::ClipTitle,
    Role::ClipAdjustment,
    Role::ClipSubtitle,
    Role::ClipOffline,
];

/// The type scale, the egui widgets under the theme's style, state colours and the clip
/// palette: one image per theme that shows most tokens.
fn theme_showcase(ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        let theme = theme::current(ui.ctx());
        let styled = |key: &str| egui::TextStyle::Name(key.into());
        ui.label(RichText::new(t!("app.name")).text_style(styled(theme::DISPLAY)));
        ui.label(RichText::new(t!("sample.showcase.title")).text_style(styled(theme::TITLE)));
        ui.label(t!("sample.showcase.body"));
        ui.label(
            RichText::new(t!("sample.showcase.caption"))
                .text_style(styled(theme::CAPTION))
                .color(theme.color(Role::TextMuted)),
        );
        ui.label(RichText::new(t!("sample.timecode")).monospace());
        ui.separator();
        ui.horizontal(|ui| {
            let _ = ui.button(t!("sample.action.import"));
            let _ = ui.button(t!("sample.action.export"));
            let _ = ui.add_enabled(false, egui::Button::new(t!("sample.action.render")));
            let _ = ui.selectable_label(true, t!("sample.tool.blade"));
            let _ = ui.selectable_label(false, t!("sample.tool.select"));
        });
        ui.horizontal(|ui| {
            let mut snap = true;
            let mut ripple = false;
            ui.checkbox(&mut snap, t!("sample.toggle.snap"));
            ui.checkbox(&mut ripple, t!("sample.toggle.ripple"));
            let mut opacity = 72.0f32;
            ui.add(egui::Slider::new(&mut opacity, 0.0..=100.0).text(t!("sample.param.opacity")));
        });
        ui.horizontal(|ui| {
            let mut name = t!("sample.clip.interview");
            ui.add(egui::TextEdit::singleline(&mut name).desired_width(200.0));
            let _ = ui.link(t!("sample.action.relink"));
        });
        ui.separator();
        ui.horizontal(|ui| {
            for (role, key) in [
                (Role::StateSuccess, t!("sample.status.ready")),
                (Role::StateInfo, t!("sample.status.proxy")),
                (Role::StateWarning, t!("sample.status.rendering")),
                (Role::StateError, t!("sample.status.offline")),
            ] {
                ui.label(RichText::new(key).color(theme.color(role)));
            }
        });
        ui.horizontal(|ui| {
            let swatch = vec2(
                theme.tokens().sizes.control_pro,
                theme.tokens().sizes.control_pro,
            );
            for role in CLIP_ROLES.iter().chain(&[
                Role::Accent,
                Role::AccentSoft,
                Role::Bg0,
                Role::Bg2,
                Role::Bg3,
            ]) {
                let (rect, _) = ui.allocate_exact_size(swatch, Sense::hover());
                ui.painter().rect(
                    rect,
                    theme.tokens().radius.control,
                    theme.color(*role),
                    Stroke::new(1.0, theme.color(Role::BorderSubtle)),
                    StrokeKind::Inside,
                );
            }
        });
    });
}

/// Four tracks of named clips painted from theme roles, with a ruler strip, a selected
/// clip and the playhead: the headless timeline the frame budget tests draw.
fn sample_timeline(ui: &mut egui::Ui) {
    let theme = theme::current(ui.ctx());
    let tokens = theme.tokens();
    egui::CentralPanel::default().show(ui, |ui| {
        let full = ui.available_rect_before_wrap();
        let header_w = 64.0;
        let area = Rect::from_min_max(full.min + vec2(header_w, 0.0), full.max);
        let painter = ui.painter_at(full);
        let track_h = tokens.sizes.row_pro * 1.5;
        let headers = [
            t!("sample.track.v2"),
            t!("sample.track.v1"),
            t!("sample.track.a1"),
            t!("sample.track.a2"),
        ];
        for (track, label) in headers.into_iter().enumerate() {
            let y = full.top() + tokens.sizes.control_pro + (track as f32 + 0.5) * track_h;
            painter.text(
                egui::pos2(full.left() + tokens.spacing.s8, y),
                Align2::LEFT_CENTER,
                label,
                theme.font(&tokens.type_scale.label),
                theme.color(Role::TextSecondary),
            );
        }
        painter.vline(
            area.left() - 1.0,
            full.y_range(),
            Stroke::new(1.0, theme.color(Role::BorderSubtle)),
        );
        let ruler = Rect::from_min_size(area.min, vec2(area.width(), tokens.sizes.control_pro));
        painter.rect_filled(ruler, 0.0, theme.color(Role::Bg2));
        let seconds = 10;
        for s in 0..=seconds {
            let x = ruler.left() + s as f32 / seconds as f32 * ruler.width();
            painter.vline(
                x,
                ruler.bottom() - tokens.spacing.s8..=ruler.bottom(),
                Stroke::new(1.0, theme.color(Role::BorderStrong)),
            );
        }
        let caption = theme.font(&tokens.type_scale.caption);
        painter.text(
            ruler.left_center() + vec2(tokens.spacing.s4, -tokens.spacing.s2),
            Align2::LEFT_CENTER,
            t!("sample.timecode"),
            caption.clone(),
            theme.color(Role::TextMuted),
        );

        let clips: [&[(f32, f32, Role, String)]; 4] = [
            &[(0.35, 0.55, Role::ClipTitle, t!("sample.clip.lower_third"))],
            &[
                (0.0, 0.42, Role::ClipVideo, t!("sample.clip.interview")),
                (0.42, 0.78, Role::ClipVideo, t!("sample.clip.broll")),
                (0.78, 1.0, Role::ClipOffline, t!("sample.clip.missing")),
            ],
            &[(0.0, 0.78, Role::ClipAudio, t!("sample.clip.interview"))],
            &[(0.1, 0.95, Role::ClipAudio, t!("sample.clip.music"))],
        ];
        let selected = (1, 1);
        for (track, row) in clips.iter().enumerate() {
            let top = ruler.bottom() + track as f32 * track_h;
            for (i, (start, end, role, label)) in row.iter().enumerate() {
                let rect = Rect::from_min_max(
                    egui::pos2(area.left() + start * area.width() + 1.0, top + 2.0),
                    egui::pos2(area.left() + end * area.width() - 1.0, top + track_h - 2.0),
                );
                let stroke = if (track, i) == selected {
                    Stroke::new(
                        tokens.stroke.selected_clip_outline,
                        theme.color(Role::Accent),
                    )
                } else {
                    Stroke::new(1.0, theme.color(Role::BorderSubtle))
                };
                painter.rect(
                    rect,
                    tokens.radius.control,
                    theme.color(*role),
                    stroke,
                    StrokeKind::Inside,
                );
                painter.with_clip_rect(rect.shrink(tokens.spacing.s4)).text(
                    rect.left_top() + vec2(tokens.spacing.s4, tokens.spacing.s4),
                    Align2::LEFT_TOP,
                    label,
                    caption.clone(),
                    theme.color(Role::TextInverse),
                );
            }
        }
        let x = area.left() + 0.6 * area.width();
        painter.vline(
            x,
            area.y_range(),
            Stroke::new(2.0, theme.color(Role::Accent)),
        );
    });
}
