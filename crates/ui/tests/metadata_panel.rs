//! The metadata panel from `Sheet / Media pool`, drawn headless with the selection moving
//! through a camera clip, a phone clip and a voice memo every frame (the view is rebuilt
//! from the pool's facts and metadata each time), within the frame budget.

use egui::{Rect, pos2, vec2};
use project::media_pool::{Facts, Gps, Metadata};
use ui::frame_time::FrameTimeMonitor;
use ui::headless::{HeadlessUi, Viewport};
use ui::panels::metadata::{METADATA_PANEL_HEIGHT, MetadataView, metadata_panel};
use ui::theme::ThemeKind;

fn items() -> Vec<(String, Facts, Metadata)> {
    let camera = (
        "Harbour at dusk".to_string(),
        Facts {
            width: 3840,
            height: 2160,
            bit_depth: 10,
            chroma: Some("4:2:2".into()),
            primaries: Some("bt709".into()),
            audio_channels: 2,
            sample_rate: 48_000,
            audio_bit_depth: Some(24),
            ..Facts::default()
        },
        Metadata {
            camera_make: Some("Sony".into()),
            camera_model: Some("ILME-FX6V".into()),
            focal_length_mm: Some(35.0),
            f_number: Some(2.8),
            iso: Some(800),
            reel: Some("A014".into()),
            timecode: Some("19:48:07:11".into()),
            created: Some("2026-08-12T19:48:07+02:00".into()),
            gps: Some(Gps {
                latitude: 60.391,
                longitude: 5.322,
                altitude_m: None,
            }),
            ..Metadata::default()
        },
    );
    let phone = (
        "Ferry".to_string(),
        Facts {
            width: 1080,
            height: 1920,
            bit_depth: 10,
            chroma: Some("4:2:0".into()),
            primaries: Some("bt2020".into()),
            hdr: Some("hlg".into()),
            audio_channels: 2,
            sample_rate: 44_100,
            ..Facts::default()
        },
        Metadata {
            camera_make: Some("Apple".into()),
            camera_model: Some("iPhone 15 Pro".into()),
            lens: Some("iPhone 15 Pro back camera 6.765mm f/1.78".into()),
            created: Some("2026-08-12T19:50:12+0200".into()),
            ..Metadata::default()
        },
    );
    let memo = (
        "Voice memo".to_string(),
        Facts {
            audio_channels: 1,
            sample_rate: 48_000,
            ..Facts::default()
        },
        Metadata::default(),
    );
    vec![camera, phone, memo]
}

#[test]
fn metadata_panel_draws_within_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("metadata panel", Viewport::default()) else {
        return;
    };
    ui::theme::builtin(ThemeKind::Dark).apply(headless.context());
    let items = items();
    let mut monitor = FrameTimeMonitor::default();
    headless
        .run(30, 240, &mut monitor, |ui, frame| {
            egui::CentralPanel::default().show(ui, |ui| {
                for row in 0..4 {
                    let (name, facts, metadata) = &items[(frame + row) % items.len()];
                    let view = MetadataView::new(
                        name,
                        Some("A014_C003.mov"),
                        Some(2_100_000_000 + frame as u64),
                        facts,
                        metadata,
                    );
                    let rect = Rect::from_min_size(
                        pos2(24.0, 24.0 + row as f32 * (METADATA_PANEL_HEIGHT + 16.0)),
                        vec2(880.0, METADATA_PANEL_HEIGHT),
                    );
                    metadata_panel(ui, rect, &view);
                }
            });
        })
        .expect("headless frames");
    let stats = monitor.stats();
    eprintln!("metadata panel ({}): {stats}", headless.adapter());
    headless.assert_p99_within_budget("metadata panel", &stats);
}
