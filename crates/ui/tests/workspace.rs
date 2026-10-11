//! The docked workspace: every preset draws each of its panels once per frame through
//! the host, layouts save and load through the workspaces file, and the whole Edit
//! workspace with a 200-clip sequence stays within the frame budget.

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use egui::{Id, Rect, Ui};
use project::media_pool::{MediaItem, MediaKind};
use project::{ClipSpec, Project, Rational, Sequence, Time, TimeRange, TrackKind};
use support::FrameBudget;
use ui::headless::{HeadlessUi, Viewport};
use ui::panels::media_pool::{ItemRow, MediaPoolView};
use ui::panels::metadata::MetadataView;
use ui::panels::preview::PreviewView;
use ui::panels::timeline::clip_media::ClipMedia;
use ui::panels::timeline::clips::{self, ClipsView};
use ui::panels::timeline::panel::TimelinePanel;
use ui::panels::timeline::ruler::Timecode;
use ui::panels::timeline::view::{TimelineView, TrackLayout};
use ui::theme::{self, ThemeKind};
use ui::workspace::dock::{self, PanelHost};
use ui::workspace::panels::{Panels, TimelineContent};
use ui::workspace::{Active, PanelKind, Preset, Workspace, Workspaces};

/// `clips` clips back to back on each of `tracks` tracks (half video, half audio), each
/// `2 * tracks` seconds long, at 25 fps, all of one media file.
fn sample(tracks: usize, clips: i64) -> (Project, Sequence) {
    let rate = Rational::new(25, 1);
    let p = Project::new("uid-workspace", "Workspace");
    let (p, seq) = p.add_sequence("Main", rate, 48_000).expect("sequence");
    let (mut p, media) = p.add_media("hash", "clip.mov").expect("media");
    for t in 0..tracks {
        let kind = if t < tracks / 2 {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        let (next, track) = p
            .add_track(seq, kind, format!("T{t}"), None)
            .expect("track");
        p = next;
        for i in 0..clips {
            let len = Time::seconds(2 * tracks as i64);
            let spec = ClipSpec {
                media,
                range: TimeRange::new(Time::seconds(2 * tracks as i64 * i), len),
                source_start: Time::ZERO,
            };
            p = p.insert_clip(track, spec).expect("clip").0;
        }
    }
    let sequence = p.sequence(seq).expect("sequence").as_ref().clone();
    (p, sequence)
}

/// Records which panels were drawn and where.
#[derive(Default)]
struct Recorder {
    drawn: Vec<(PanelKind, Rect)>,
}

impl PanelHost for Recorder {
    fn show(&mut self, _ui: &mut Ui, rect: Rect, panel: PanelKind) {
        self.drawn.push((panel, rect));
    }
}

#[test]
fn every_preset_draws_each_visible_panel_once() {
    let ctx = egui::Context::default();
    theme::set(&ctx, ThemeKind::Dark);
    for preset in Preset::ALL {
        let mut layout = Workspace::preset(preset);
        let mut recorder = Recorder::default();
        // Two frames: the dock measures its splits on the first.
        for _ in 0..2 {
            recorder.drawn.clear();
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 900.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                dock::show(ui, Id::new(("test", preset)), &mut layout, &mut recorder);
            });
            out.textures_delta.clear();
        }
        // One tab per split is in front.
        let shown: BTreeSet<PanelKind> = recorder.drawn.iter().map(|(p, _)| *p).collect();
        assert_eq!(shown.len(), recorder.drawn.len(), "{preset:?}: drawn twice");
        assert!(shown.contains(&PanelKind::Preview), "{preset:?}");
        assert!(shown.contains(&PanelKind::Timeline) || preset == Preset::Colour);
        for (panel, rect) in &recorder.drawn {
            assert!(
                rect.width() > 100.0 && rect.height() > 60.0,
                "{preset:?} {panel:?} {rect:?}"
            );
        }
        // No two panels overlap.
        for (i, (a, ra)) in recorder.drawn.iter().enumerate() {
            for (b, rb) in &recorder.drawn[i + 1..] {
                let overlap = ra.intersect(*rb);
                assert!(
                    overlap.width() <= 1.0 || overlap.height() <= 1.0,
                    "{preset:?}: {a:?} and {b:?} overlap"
                );
            }
        }
    }
}

#[test]
fn layouts_round_trip_through_the_workspaces_file() {
    let dir = std::env::temp_dir().join(format!("glass-workspaces-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("workspaces.json");
    assert!(Workspaces::load(&file).expect("missing is fine").is_none());

    let mut all = Workspaces::default();
    all.current.close(PanelKind::Metadata);
    all.current.restore(PanelKind::Scopes);
    assert!(all.save_as("Grading"));
    all.apply_preset(Preset::Audio);
    all.current.close(PanelKind::Mixer);
    all.save(&file).expect("save");

    let loaded = Workspaces::load(&file).expect("load").expect("file");
    assert_eq!(loaded, all);
    assert_eq!(loaded.active, Active::Preset(Preset::Audio));
    assert!(!loaded.current.is_open(PanelKind::Mixer));
    let grading = &loaded.saved["Grading"];
    assert!(grading.is_open(PanelKind::Scopes));
    assert!(!grading.is_open(PanelKind::Metadata));
    // Saving what was loaded writes the same bytes.
    assert_eq!(loaded.to_json(), all.to_json());

    // A layout with every panel closed opens as its preset instead.
    let mut empty = Workspaces::default();
    for panel in empty.current.panels() {
        empty.current.close(panel);
    }
    let reopened = Workspaces::from_json(&empty.to_json()).expect("parse");
    assert_eq!(reopened.current, Workspace::preset(Preset::Edit));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_edit_workspace_with_200_clips_is_within_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("workspace", Viewport::default()) else {
        return;
    };
    theme::builtin(ThemeKind::Dark).apply(headless.context());
    let (project, sequence) = sample(8, 25);
    assert_eq!(
        sequence.tracks.iter().map(|t| t.items.len()).sum::<usize>(),
        200
    );
    let sequence = Arc::new(sequence);
    let mut pool = MediaPoolView {
        items: (0..60)
            .map(|i| {
                let item = MediaItem::new(
                    format!("A{i:03}.mov"),
                    MediaKind::Video,
                    format!("/footage/A{i:03}.mov"),
                );
                ItemRow::from_item(&item, false)
            })
            .collect(),
        total: 60,
        ..MediaPoolView::default()
    };
    let metadata = MetadataView::default();
    let tc = Timecode::for_sequence(&sequence);
    let end = 25 * 16 * 25;
    let mut timeline = TimelinePanel::new(&sequence);
    let mut clip_media = ClipMedia::new();
    let mut layout = Workspace::preset(Preset::Edit);
    let stats = FrameBudget::run(&mut headless, 30, 240, |ui, frame| {
        // Playing: the playhead moves a frame per UI frame; every so often a zoom step.
        let playhead = frame as i64 * 2;
        if frame % 40 == 0 {
            let id = if frame % 80 == 0 {
                "view.zoom_in"
            } else {
                "view.zoom_out"
            };
            timeline.view.run_command(id, None, ui.input(|i| i.time));
        }
        let preview = PreviewView {
            canvas: (1920, 1080),
            frame: playhead,
            end,
            timecode: tc.format(playhead),
            duration: tc.format(end),
            playing: true,
            ..PreviewView::default()
        };
        let mut layer = |ui: &mut Ui, rect: Rect, view: &TimelineView, rows: &TrackLayout| {
            let _ = clips::clips(
                ui,
                rect,
                view,
                rows,
                &ClipsView::new(&project, &sequence),
                &mut clip_media,
                &(),
            );
        };
        let mut panels = Panels {
            pool: &mut pool,
            metadata: &metadata,
            preview: &preview,
            timeline: Some(TimelineContent {
                panel: &mut timeline,
                sequence: &sequence,
                playhead,
                layer: Some(&mut layer),
            }),
            events: Vec::new(),
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                dock::show(ui, Id::new("budget"), &mut layout, &mut panels);
            });
    })
    .expect("headless frames");
    eprintln!(
        "edit workspace, 200 clips ({}): {stats} (M5 standing in for the M1, machine shared \
         with other builds)",
        headless.adapter()
    );
    stats.assert_within_budget(&headless, "edit workspace");
}
