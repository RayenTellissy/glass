//! Snapshot cases for `panels::timeline::track_header`: a sequence's headers at rest, with
//! row hover, a pressed switch and keyboard focus, with switches mixed (solo silencing the other audio,
//! mute, hidden, locked), at every height preset, renaming, and mirrored for right to
//! left.

use egui::vec2;
use project::{TrackId, TrackKind};

use crate::panels::timeline::track_header::{
    self, HEADER_WIDTH, HeightPreset, Part, Rename, TrackFlags, TrackHeaderView, TrackRow, part_id,
};
use crate::t;
use crate::theme::{self, Role};

const MEDIUM: f32 = 48.0;

pub(super) const DEFAULT_SIZE: egui::Vec2 = vec2(HEADER_WIDTH, 6.0 * MEDIUM);
pub(super) const STATES_SIZE: egui::Vec2 = vec2(HEADER_WIDTH, 4.0 * MEDIUM);
pub(super) const MIX_SIZE: egui::Vec2 = vec2(HEADER_WIDTH, 6.0 * MEDIUM);
pub(super) const HEIGHTS_SIZE: egui::Vec2 = vec2(HEADER_WIDTH, 2.0 * (28.0 + 48.0 + 72.0));
pub(super) const RENAME_SIZE: egui::Vec2 = vec2(HEADER_WIDTH, 3.0 * MEDIUM);

fn row(id: u64, kind: TrackKind, name: String) -> TrackRow {
    TrackRow {
        id: TrackId(id),
        kind,
        name,
        flags: TrackFlags::default(),
        targeted: false,
    }
}

/// The sample sequence, top to bottom, with V1 targeted.
fn sequence() -> Vec<TrackRow> {
    let mut rows = vec![
        row(1, TrackKind::Adjustment, t!("sample.track.grade")),
        row(2, TrackKind::Video, t!("sample.track.v2")),
        row(3, TrackKind::Video, t!("sample.track.v1")),
        row(4, TrackKind::Subtitle, t!("sample.track.captions")),
        row(5, TrackKind::Audio, t!("sample.track.dialogue")),
        row(6, TrackKind::Audio, t!("sample.track.music")),
    ];
    rows[2].targeted = true;
    rows
}

fn draw(ui: &mut egui::Ui, mut view: TrackHeaderView) {
    let fill = theme::current(ui.ctx()).color(Role::Bg1);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(fill))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            let _ = track_header::track_headers(ui, rect, &mut view);
        });
}

fn view(rows: Vec<TrackRow>) -> TrackHeaderView {
    TrackHeaderView {
        rows,
        ..TrackHeaderView::default()
    }
}

pub(super) fn default(ui: &mut egui::Ui) {
    draw(ui, view(sequence()));
}

/// V2's row hovered, Music's solo pressed, and keyboard focus on V1's lock.
pub(super) fn states(ui: &mut egui::Ui) {
    let rows = vec![
        row(2, TrackKind::Video, t!("sample.track.v2")),
        row(3, TrackKind::Video, t!("sample.track.v1")),
        row(5, TrackKind::Audio, t!("sample.track.dialogue")),
        row(6, TrackKind::Audio, t!("sample.track.music")),
    ];
    ui.memory_mut(|m| m.request_focus(part_id(TrackId(3), Part::Lock)));
    let mut v = view(rows);
    v.hover = Some((TrackId(2), Part::Row));
    v.pressed = Some((TrackId(6), Part::Solo));
    draw(ui, v);
}

/// Dialogue soloed (Music silent by it), Effects muted, V2 hidden, V1 locked.
pub(super) fn mix(ui: &mut egui::Ui) {
    let mut rows = vec![
        row(2, TrackKind::Video, t!("sample.track.v2")),
        row(3, TrackKind::Video, t!("sample.track.v1")),
        row(4, TrackKind::Subtitle, t!("sample.track.captions")),
        row(5, TrackKind::Audio, t!("sample.track.dialogue")),
        row(6, TrackKind::Audio, t!("sample.track.music")),
        row(7, TrackKind::Audio, t!("sample.track.effects")),
    ];
    rows[0].flags.hidden = true;
    rows[1].flags.locked = true;
    rows[1].targeted = true;
    rows[3].flags.soloed = true;
    rows[5].flags.muted = true;
    draw(ui, view(rows));
}

/// A video and an audio track at each preset.
pub(super) fn heights(ui: &mut egui::Ui) {
    let mut rows = Vec::new();
    for (i, preset) in HeightPreset::ALL.into_iter().enumerate() {
        let id = i as u64 * 2;
        let mut video = row(id + 1, TrackKind::Video, t!("sample.track.v1"));
        let mut audio = row(id + 2, TrackKind::Audio, t!("sample.track.a1"));
        video.flags.height = preset;
        audio.flags.height = preset;
        rows.push(video);
        rows.push(audio);
    }
    draw(ui, view(rows));
}

/// V1 being renamed.
pub(super) fn renaming(ui: &mut egui::Ui) {
    let rows = vec![
        row(2, TrackKind::Video, t!("sample.track.v2")),
        row(3, TrackKind::Video, t!("sample.track.v1")),
        row(5, TrackKind::Audio, t!("sample.track.dialogue")),
    ];
    ui.memory_mut(|m| m.request_focus(part_id(TrackId(3), Part::Name)));
    let mut v = view(rows);
    v.renaming = Some(Rename {
        track: TrackId(3),
        text: t!("sample.track.interviews"),
    });
    draw(ui, v);
}

/// The sample sequence mirrored for a right-to-left locale.
pub(super) fn rtl(ui: &mut egui::Ui) {
    let mut v = view(sequence());
    v.rtl = true;
    v.rows[4].flags.soloed = true;
    draw(ui, v);
}
