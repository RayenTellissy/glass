//! Snapshot cases for `dialogs::relink`: the board's two clips found on the studio drive,
//! a clip still being searched beside one with no match, and the footer while the
//! relink runs.

use egui::vec2;
use engine::media_mgmt::relink::Check;
use project::MediaId;

use crate::dialogs::relink::{
    self, CandidateRow, ClipState, ClipView, RelinkDialog, RelinkView, Relinking,
};
use crate::t;
use crate::theme::{self, Role};

/// The dialog's full height in each case, plus 16 pt of margin on every side.
pub(super) const FOUND_SIZE: egui::Vec2 = vec2(relink::WIDTH + 32.0, 538.0);
pub(super) const STATES_SIZE: egui::Vec2 = vec2(relink::WIDTH + 32.0, 510.0);

fn row(path: String, name: Check, hash: Check, percent: u8) -> CandidateRow {
    CandidateRow {
        path,
        name,
        duration: Check::Match,
        hash,
        percent: Some(percent),
        checking: false,
        enabled: true,
    }
}

fn clip_a(state: ClipState) -> ClipView {
    ClipView {
        media: MediaId(4),
        name: t!("sample.relink.clip_a"),
        detail: t!("sample.relink.detail_a"),
        edits: 3,
        thumbnail: None,
        state,
    }
}

fn clip_b(state: ClipState) -> ClipView {
    ClipView {
        media: MediaId(7),
        name: t!("sample.relink.clip_b"),
        detail: t!("sample.relink.detail_b"),
        edits: 1,
        thumbnail: None,
        state,
    }
}

/// What the board shows: both clips found, the exact match picked for each.
fn found_view() -> RelinkView {
    RelinkView {
        project_name: t!("sample.recovery.project"),
        clips: vec![
            clip_a(ClipState::Found {
                candidates: vec![
                    row(t!("sample.relink.path_a1"), Check::Match, Check::Match, 100),
                    row(
                        t!("sample.relink.path_a2"),
                        Check::Match,
                        Check::Mismatch,
                        72,
                    ),
                ],
                selected: Some(0),
            }),
            clip_b(ClipState::Found {
                candidates: vec![
                    row(t!("sample.relink.path_b1"), Check::Match, Check::Match, 100),
                    row(
                        t!("sample.relink.path_b2"),
                        Check::Mismatch,
                        Check::Mismatch,
                        41,
                    ),
                ],
                selected: Some(0),
            }),
        ],
        place: t!("sample.relink.place"),
        folders_searched: 3,
        elapsed_s: 0.8,
        same_folders: true,
        relinking: None,
    }
}

fn draw(ui: &mut egui::Ui, view: &RelinkView) {
    let fill = theme::current(ui.ctx()).color(Role::Bg1);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(fill))
        .show(ui, |ui| {
            let origin = ui.max_rect().min + vec2(16.0, 16.0);
            let _ = RelinkDialog::new().draw(ui, origin, view, f32::INFINITY);
        });
}

pub(super) fn found(ui: &mut egui::Ui) {
    draw(ui, &found_view());
}

/// B002 still being searched beside A014, which had no match.
pub(super) fn searching_and_no_match(ui: &mut egui::Ui) {
    let place = t!("sample.relink.place");
    let view = RelinkView {
        clips: vec![
            clip_b(ClipState::Searching {
                place: place.clone(),
                files_done: 1284,
                files_total: 3960,
                folder: t!("sample.relink.folder_b"),
            }),
            clip_a(ClipState::NoMatch {
                place: place.clone(),
            }),
        ],
        same_folders: false,
        ..found_view()
    };
    draw(ui, &view);
}

/// Relink pressed: progress in the footer, Stop, the primary button faded.
pub(super) fn relinking(ui: &mut egui::Ui) {
    let view = RelinkView {
        relinking: Some(Relinking {
            done: 0,
            total: 2,
            name: t!("sample.relink.clip_a"),
        }),
        ..found_view()
    };
    draw(ui, &view);
}
