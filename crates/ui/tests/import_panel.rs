//! The import surfaces from `Sheet / Import`, drawn headless: every drop area state, the
//! folder import dialog for a 200-folder shoot and the progress strip, within the frame
//! budget.

mod support;

use std::path::PathBuf;

use egui::{Rect, pos2, vec2};
use engine::media_mgmt::{
    DropSummary, ImportPlan, ImportSource, MediaClass, PlannedFolder, Unreadable,
};
use support::FrameBudget;
use ui::headless::{HeadlessUi, Viewport};
use ui::panels::import::{
    DropArea, FolderImport, ImportProgress, drop_area, folder_dialog, kinds_line, progress_strip,
    unreadable_message,
};
use ui::theme::ThemeKind;

fn folder(bin: &[&str], clips: usize) -> PlannedFolder {
    PlannedFolder {
        path: PathBuf::from("/Volumes/Shuttle SSD").join(bin.join("/")),
        bin: bin.iter().map(|s| s.to_string()).collect(),
        clips,
        new_clips: clips,
        card: None,
    }
}

fn shoot_plan() -> ImportPlan {
    let mut folders = vec![folder(&["Harbour shoot"], 212)];
    for day in 1..=20 {
        let d = format!("Day {day}");
        folders.push(folder(&["Harbour shoot", &d], 9));
        for cam in ["A-cam", "B-cam", "Sound"] {
            folders.push(folder(&["Harbour shoot", &d, cam], 3));
        }
    }
    folders.push(folder(&["Harbour shoot", "Notes"], 0));
    let sources = (0..212)
        .map(|i| ImportSource {
            name: format!("A014_C{i:03}"),
            class: MediaClass::Video,
            files: vec![PathBuf::from(format!(
                "/Volumes/Shuttle SSD/Harbour shoot/A014_C{i:03}.mov"
            ))],
            sidecars: Vec::new(),
            card: None,
            bin: vec!["Harbour shoot".into()],
            size_bytes: 900_000_000,
            already_in_project: i < 24,
        })
        .collect();
    ImportPlan {
        roots: vec![PathBuf::from("/Volumes/Shuttle SSD/Harbour shoot")],
        sources,
        folders,
        ..ImportPlan::default()
    }
}

#[test]
fn folder_dialog_view_model_follows_the_plan() {
    let view = FolderImport::from_plan(&shoot_plan());
    assert_eq!(view.folder_name, "Harbour shoot");
    assert_eq!(view.already_in_project, 24);
    assert_eq!(view.new_files, 188);
    assert_eq!(view.rows.len(), 1 + 20 * 4 + 1);
    let notes = view.rows.last().unwrap();
    assert!(notes.left_out && !notes.included);
    assert!(view.rows[0].has_children && view.rows[1].has_children && !view.rows[2].has_children);
    assert_eq!(view.bins, 81);
}

#[test]
fn drop_copy_reads_like_the_board() {
    let summary = DropSummary::of(&[
        "a.mov", "b.mov", "c.mov", "d.mov", "e.mov", "f.mov", "g.mov", "h.mov", "1.wav", "2.wav",
        "3.wav", "p.png",
    ]);
    assert_eq!(kinds_line(&summary), "8 video · 3 audio · 1 still");
    assert_eq!(
        unreadable_message(Unreadable::Photoshop, 2),
        "2 Photoshop files can't be read. Export them as PNG or TIFF, then drop them again."
    );
}

#[test]
fn import_surfaces_draw_within_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("import panel", Viewport::default()) else {
        return;
    };
    ui::theme::builtin(ThemeKind::Dark).apply(headless.context());
    let plan = shoot_plan();
    let mut dialog = FolderImport::from_plan(&plan);
    let over = DropArea::Over(DropSummary::of(&["a.mov", "b.mp4", "c.wav", "d.png"]));
    let warn = DropArea::Over(DropSummary::of(&["a.mov", "b.psd", "c.psd"]));
    let progress = ImportProgress {
        source: "Card B".into(),
        bin: "B-roll".into(),
        done: 18,
        total: 42,
        current_file: "B002_C019.MP4".into(),
        current_bytes: 1_200_000_000,
        bytes_done: 43,
        bytes_total: 100,
        elapsed_s: 45.0,
    };
    let stats = FrameBudget::run(&mut headless, 30, 240, |ui, frame| {
        egui::CentralPanel::default().show(ui, |ui| {
            let areas = [DropArea::Empty, over.clone(), warn.clone()];
            for (i, state) in areas.iter().enumerate() {
                let rect =
                    Rect::from_min_size(pos2(24.0 + i as f32 * 424.0, 24.0), vec2(382.0, 234.0));
                drop_area(ui, rect, "Pickups", state);
            }
            let strip = Rect::from_min_size(
                pos2(24.0, 280.0),
                vec2(400.0, ui::panels::import::PROGRESS_STRIP_HEIGHT),
            );
            let mut p = progress.clone();
            p.bytes_done = (frame % 100) as u64;
            progress_strip(ui, strip, &p);
            folder_dialog(ui, pos2(480.0, 280.0), &mut dialog);
        });
    })
    .expect("headless frames");
    eprintln!("import panel ({}): {stats}", headless.adapter());
    stats.assert_within_budget(&headless, "import panel");
}
