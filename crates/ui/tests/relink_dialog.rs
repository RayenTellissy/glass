//! The relink dialog: picking rows, keys and buttons on a real egui context, the engine's
//! search results turned into rows, its layout against the board, and every state
//! within the frame budget.

mod support;

use std::path::PathBuf;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use engine::media_mgmt::relink::{Candidate, Check, Confidence, RelinkTarget, TargetMatches};
use project::MediaId;
use project::media_pool::Facts;
use support::FrameBudget;
use ui::dialogs::relink::{
    CandidateRow, ClipState, ClipView, Control, RelinkAction, RelinkDialog, RelinkView, Relinking,
    WIDTH, place_name,
};
use ui::headless::{HeadlessUi, Viewport};
use ui::theme::{self, ThemeKind};

const A: MediaId = MediaId(4);
const B: MediaId = MediaId(7);

fn row(path: &str, percent: u8, enabled: bool) -> CandidateRow {
    CandidateRow {
        path: path.into(),
        name: Check::Match,
        duration: Check::Match,
        hash: if percent == 100 {
            Check::Match
        } else {
            Check::Mismatch
        },
        percent: Some(percent),
        checking: false,
        enabled,
    }
}

fn view() -> RelinkView {
    let clip = |media, name: &str, rows| ClipView {
        media,
        name: name.into(),
        detail: "Last seen /Volumes/Card A · 4K · 23.976 fps · 12 s".into(),
        edits: 3,
        thumbnail: None,
        state: ClipState::Found {
            candidates: rows,
            selected: Some(0),
        },
    };
    RelinkView {
        project_name: "Harbour film".into(),
        clips: vec![
            clip(
                A,
                "A014_C011",
                vec![
                    row(
                        "/Volumes/Studio SSD/Harbour/Card A/A014_C011.mov",
                        100,
                        true,
                    ),
                    row(
                        "/Volumes/Studio SSD/Harbour/Exports/A014_C011.mov",
                        72,
                        true,
                    ),
                    row("/Volumes/Studio SSD/Harbour/Short/A014_C011.mov", 64, false),
                ],
            ),
            clip(
                B,
                "B002_C004",
                vec![row(
                    "/Volumes/Studio SSD/Harbour/Card B/B002_C004.MP4",
                    100,
                    true,
                )],
            ),
        ],
        place: "Studio SSD".into(),
        folders_searched: 3,
        elapsed_s: 0.8,
        same_folders: true,
        relinking: None,
    }
}

struct Driver {
    ctx: egui::Context,
    dialog: RelinkDialog,
    view: RelinkView,
}

impl Driver {
    fn new() -> Option<Self> {
        let ctx = egui::Context::default();
        if !ui::fonts::install(&ctx) {
            eprintln!("skipping: no font found");
            return None;
        }
        theme::set(&ctx, ThemeKind::Dark);
        let mut driver = Self {
            ctx,
            dialog: RelinkDialog::new(),
            view: view(),
        };
        driver.frame(Vec::new());
        driver.frame(Vec::new());
        Some(driver)
    }

    fn frame(&mut self, events: Vec<Event>) -> Option<RelinkAction> {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440.0, 900.0))),
            events,
            ..RawInput::default()
        };
        let mut action = None;
        let mut output = self.ctx.run_ui(input, |ui| {
            action = self.dialog.show(ui.ctx(), &mut self.view);
        });
        output.textures_delta.clear();
        action
    }

    fn click(&mut self, control: Control) -> Option<RelinkAction> {
        let at = self
            .dialog
            .control_rect(control)
            .unwrap_or_else(|| panic!("{control:?} not shown"))
            .center();
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let a = self.frame(vec![Event::PointerMoved(at), button(true)]);
        let b = self.frame(vec![button(false)]);
        let c = self.frame(Vec::new());
        a.or(b).or(c)
    }

    fn key(&mut self, key: Key) -> Option<RelinkAction> {
        let event = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        self.frame(vec![event(true), event(false)])
    }
}

#[test]
fn rows_pick_the_match_and_relink_goes_on_enter() {
    let Some(mut d) = Driver::new() else { return };
    assert_eq!(
        d.click(Control::Candidate(0, 1)),
        Some(RelinkAction::Select { media: A, index: 1 })
    );
    assert_eq!(d.view.clips[0].selected(), Some(1));
    // A file that cannot be used is not picked.
    assert_eq!(d.click(Control::Candidate(0, 2)), None);
    assert_eq!(d.view.clips[0].selected(), Some(1));
    assert_eq!(
        d.click(Control::SameFolders),
        Some(RelinkAction::SameFolders(false))
    );
    assert!(!d.view.same_folders);
    assert_eq!(d.key(Key::Enter), Some(RelinkAction::Relink));
    assert_eq!(d.click(Control::Relink), Some(RelinkAction::Relink));
    assert_eq!(
        d.click(Control::SearchFolder),
        Some(RelinkAction::SearchFolder)
    );
    assert_eq!(d.key(Key::Escape), Some(RelinkAction::Close));
    assert_eq!(d.click(Control::Cancel), Some(RelinkAction::Close));
    assert_eq!(d.click(Control::Close), Some(RelinkAction::Close));
}

#[test]
fn while_relinking_only_stop_works() {
    let Some(mut d) = Driver::new() else { return };
    d.view.relinking = Some(Relinking {
        done: 1,
        total: 2,
        name: "B002_C004".into(),
    });
    d.frame(Vec::new());
    assert!(d.dialog.control_rect(Control::Cancel).is_none());
    assert_eq!(d.click(Control::Stop), Some(RelinkAction::Stop));
    assert_eq!(d.click(Control::Relink), None);
    assert_eq!(d.click(Control::Close), None);
    assert_eq!(d.click(Control::Candidate(0, 1)), None);
    assert_eq!(d.key(Key::Enter), None);
    assert_eq!(d.key(Key::Escape), None);
}

#[test]
fn no_match_offers_a_folder_or_leaving_it_offline() {
    let Some(mut d) = Driver::new() else { return };
    d.view.clips[1].state = ClipState::NoMatch {
        place: "Studio SSD".into(),
    };
    d.frame(Vec::new());
    assert_eq!(d.view.matched(), 1);
    assert_eq!(
        d.click(Control::ClipSearchFolder(1)),
        Some(RelinkAction::SearchFolderFor(B))
    );
    assert_eq!(
        d.click(Control::LeaveOffline(1)),
        Some(RelinkAction::LeaveOffline(B))
    );
    assert_eq!(d.view.clips.len(), 1);
}

#[test]
fn layout_follows_the_board() {
    let Some(d) = Driver::new() else { return };
    let rect = |c| d.dialog.control_rect(c).expect("shown");
    let left = 720.0 - WIDTH / 2.0;
    let right = 720.0 + WIDTH / 2.0;
    let close = rect(Control::Close);
    assert_eq!(close.size(), vec2(24.0, 24.0));
    assert_eq!(close.right(), right - 12.0);
    let relink = rect(Control::Relink);
    let cancel = rect(Control::Cancel);
    assert_eq!(relink.right(), right - 16.0);
    assert_eq!(relink.height(), 32.0);
    assert_eq!(cancel.right(), relink.left() - 8.0);
    let search = rect(Control::SearchFolder);
    assert_eq!(search.right(), right - 16.0);
    assert_eq!(search.height(), 24.0);
    // Candidate rows: 28 tall, across the card, which is 16 in from the dialog's sides.
    let row = rect(Control::Candidate(0, 0));
    assert_eq!(row.height(), 28.0);
    assert_eq!(row.left(), left + 16.0);
    assert_eq!(row.right(), right - 16.0);
    let next = rect(Control::Candidate(0, 1));
    assert_eq!(next.top(), row.bottom());
    // Header 48 + 16 padding + 24 summary + 16 gap + card header 53 + columns 24.
    let top = 450.0 - d.dialog.height(&d.ctx, &d.view) / 2.0;
    assert!((row.top() - (top.round() + 48.0 + 16.0 + 24.0 + 16.0 + 53.0 + 24.0)).abs() < 0.01);
}

#[test]
fn search_results_become_rows() {
    let target = RelinkTarget {
        media: A,
        name: "A014_C011".into(),
        path: PathBuf::from("/Volumes/Card A/A014_C011.mov"),
        content_hash: "aa".into(),
        facts: Some(Facts {
            duration_s: Some(12.0),
            width: 3840,
            height: 2160,
            frame_rate: Some((24000, 1001)),
            ..Facts::default()
        }),
        size_bytes: None,
        class: None,
        used_until: Some(10.0),
        clips: 3,
    };
    let candidate = |path: &str, score: f32, too_short: bool| Candidate {
        path: path.into(),
        score,
        confidence: if score >= 1.0 {
            Confidence::Exact
        } else {
            Confidence::Possible
        },
        name: Check::Match,
        duration: Check::Match,
        hash: Check::Mismatch,
        content_hash: String::new(),
        facts: None,
        too_short,
        from_move: false,
    };
    let matches = TargetMatches {
        media: A,
        candidates: vec![
            candidate("/b/A014_C011.mov", 1.0, true),
            candidate("/c/A014_C011.mov", 0.62, false),
        ],
    };
    let clip = ClipView::new(&target, Some(&matches), "Studio SSD");
    assert_eq!(
        clip.detail,
        "Last seen /Volumes/Card A · 4K · 23.976 fps · 12 s"
    );
    assert_eq!(clip.edits, 3);
    let ClipState::Found {
        candidates,
        selected,
    } = &clip.state
    else {
        panic!("{:?}", clip.state);
    };
    // The short exact file is shown but not usable; the possible one is not picked.
    assert!(!candidates[0].enabled);
    assert_eq!(candidates[1].percent, Some(62));
    assert_eq!(*selected, None);
    let none = ClipView::new(
        &target,
        Some(&TargetMatches {
            media: A,
            candidates: Vec::new(),
        }),
        "Studio SSD",
    );
    assert!(matches!(none.state, ClipState::NoMatch { .. }));
    assert!(matches!(
        ClipView::new(&target, None, "Studio SSD").state,
        ClipState::Searching { .. }
    ));
    assert_eq!(
        place_name(std::path::Path::new("/Volumes/Studio SSD/Harbour")),
        "Studio SSD"
    );
    assert_eq!(
        place_name(std::path::Path::new("/Users/me/Footage")),
        "Footage"
    );
}

#[test]
fn every_state_within_the_frame_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip(
        "relink dialog",
        Viewport {
            width_px: 1440,
            height_px: 900,
            pixels_per_point: 1.0,
        },
    ) else {
        return;
    };
    // Twelve offline clips, so the list scrolls, in every state in turn.
    let base = view();
    let mut clips = Vec::new();
    for i in 0..12 {
        let mut c = base.clips[i % 2].clone();
        c.media = MediaId(100 + i as u64);
        clips.push(c);
    }
    for kind in ThemeKind::ALL {
        theme::set(headless.context(), kind);
        let mut dialog = RelinkDialog::new();
        let stats = FrameBudget::run(&mut headless, 80, 240, |ui, frame| {
            let mut v = RelinkView {
                clips: clips.clone(),
                ..view()
            };
            // Every state shows during the warm-up, so its glyphs are in the atlas.
            match (frame / 20) % 4 {
                0 => {}
                1 => {
                    v.clips[0].state = ClipState::Searching {
                        place: "Studio SSD".into(),
                        files_done: frame * 10,
                        files_total: 3960,
                        folder: "/Volumes/Studio SSD/Harbour/Card B".into(),
                    }
                }
                2 => {
                    v.clips[1].state = ClipState::NoMatch {
                        place: "Studio SSD".into(),
                    }
                }
                _ => {
                    v.relinking = Some(Relinking {
                        done: frame % 12,
                        total: 12,
                        name: "A014_C011".into(),
                    })
                }
            }
            egui::CentralPanel::default().show(ui, |_| {});
            dialog.show(ui.ctx(), &mut v);
        })
        .expect("headless frames");
        assert!(dialog.control_rect(Control::Close).is_some(), "{kind:?}");
        eprintln!("relink dialog {kind:?} ({}): {stats}", headless.adapter());
        stats.assert_within_budget(&headless, "relink dialog");
    }
}
