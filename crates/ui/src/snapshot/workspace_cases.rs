//! Snapshot cases for the docked workspace (`workspace`): each preset with sample
//! content (the media pool's `Selects` bin, the sample edit on the timeline with its
//! clips loading, an empty viewer), and the panels it adds: the viewer with and without
//! a sequence, the inspector's empty state and the placeholder of a panel still to come.

use std::sync::Arc;

use egui::{Id, Rect, vec2};

use super::{pool_cases, timeline_cases};
use crate::panels::inspector::inspector_panel;
use crate::panels::metadata::MetadataView;
use crate::panels::placeholder::placeholder_panel;
use crate::panels::preview::{PreviewView, preview_panel};
use crate::panels::timeline::clip_media::ClipMedia;
use crate::panels::timeline::clips::{self, ClipsView};
use crate::panels::timeline::panel::TimelinePanel;
use crate::panels::timeline::ruler::Timecode;
use crate::panels::timeline::view::{TimelineView, TrackLayout};
use crate::t;
use crate::theme::{self, Role};
use crate::workspace::panels::{Panels, TimelineContent};
use crate::workspace::{PanelKind, Preset, Workspace, dock};

/// A small window: every preset's splits stay readable at this size.
pub(super) const WORKSPACE_SIZE: egui::Vec2 = vec2(1200.0, 720.0);
pub(super) const PANEL_SIZE: egui::Vec2 = vec2(480.0, 300.0);

fn fill(ui: &mut egui::Ui, draw: impl FnOnce(&mut egui::Ui, Rect)) {
    let bg = theme::current(ui.ctx()).color(Role::Bg1);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(bg))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            draw(ui, rect);
        });
}

/// The viewer of the sample edit, paused 1:04 in, no frame presented yet.
fn sample_preview(sequence: &project::Sequence, frame: i64) -> PreviewView {
    let tc = Timecode::for_sequence(sequence);
    let end = 120 * 25;
    PreviewView {
        picture: None,
        canvas: (sequence.resolution.width, sequence.resolution.height),
        frame,
        end,
        timecode: tc.format(frame),
        duration: tc.format(end),
        playing: false,
        silent: true,
        empty: false,
    }
}

fn workspace(ui: &mut egui::Ui, preset: Preset) {
    let (project, sequence) = timeline_cases::sample_project();
    let sequence = Arc::new(sequence);
    let mut pool = pool_cases::grid_view();
    let metadata = MetadataView {
        clip_name: t!("sample.pool.clip.dusk"),
        camera: Some(t!("sample.pool.camera.fx6")),
        ..MetadataView::default()
    };
    let playhead = 64 * 25;
    let preview = sample_preview(&sequence, playhead);
    let mut timeline = TimelinePanel::new(&sequence);
    let mut media = ClipMedia::new();
    let mut layer = |ui: &mut egui::Ui, rect: Rect, view: &TimelineView, layout: &TrackLayout| {
        let _ = clips::clips(
            ui,
            rect,
            view,
            layout,
            &ClipsView::new(&project, &sequence),
            &mut media,
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
    let mut layout = Workspace::preset(preset);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            dock::show(
                ui,
                Id::new(("glass.snapshot.workspace", preset)),
                &mut layout,
                &mut panels,
            );
        });
}

pub(super) fn edit(ui: &mut egui::Ui) {
    workspace(ui, Preset::Edit);
}

pub(super) fn colour(ui: &mut egui::Ui) {
    workspace(ui, Preset::Colour);
}

pub(super) fn audio(ui: &mut egui::Ui) {
    workspace(ui, Preset::Audio);
}

pub(super) fn effects(ui: &mut egui::Ui) {
    workspace(ui, Preset::Effects);
}

/// The viewer paused with no frame yet, without an audio device.
pub(super) fn preview(ui: &mut egui::Ui) {
    let sequence = timeline_cases::sample();
    let view = sample_preview(&sequence, 64 * 25);
    fill(ui, |ui, rect| {
        let _ = preview_panel(ui, rect, &view);
    });
}

/// The viewer with no sequence open.
pub(super) fn preview_empty(ui: &mut egui::Ui) {
    let view = PreviewView {
        empty: true,
        ..PreviewView::default()
    };
    fill(ui, |ui, rect| {
        let _ = preview_panel(ui, rect, &view);
    });
}

pub(super) fn inspector(ui: &mut egui::Ui) {
    fill(ui, inspector_panel);
}

pub(super) fn placeholder(ui: &mut egui::Ui) {
    fill(ui, |ui, rect| {
        placeholder_panel(ui, rect, &PanelKind::Scopes.title());
    });
}
