//! The timeline's track headers driven by pointer and keys on a real egui context: each
//! switch emits its action, locked tracks refuse edits, inline rename keeps or drops the
//! name, Tab walks a row's switches in order; solo silences the other audio tracks; and
//! 40 headers draw within the frame budget.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};
use project::{Project, Rational, TrackId, TrackKind};
use ui::frame_time::FrameTimeMonitor;
use ui::headless::{HeadlessUi, Viewport};
use ui::panels::timeline::track_header::{
    HEADER_WIDTH, HeightPreset, Part, TrackFlags, TrackHeaderAction, TrackHeaderState,
    TrackHeaderView, TrackRow, part_id, track_headers,
};
use ui::theme::{self, ThemeKind};

const V1: TrackId = TrackId(1);
const A1: TrackId = TrackId(2);
const A2: TrackId = TrackId(3);

fn rows() -> Vec<TrackRow> {
    let row = |id, kind, name: &str| TrackRow {
        id,
        kind,
        name: name.into(),
        flags: TrackFlags::default(),
        targeted: false,
    };
    vec![
        row(V1, TrackKind::Video, "V1"),
        row(A1, TrackKind::Audio, "A1"),
        row(A2, TrackKind::Audio, "A2"),
    ]
}

fn column() -> Rect {
    Rect::from_min_size(pos2(0.0, 0.0), vec2(HEADER_WIDTH, 400.0))
}

struct Driver {
    ctx: egui::Context,
    view: TrackHeaderView,
}

impl Driver {
    /// `None` when no font is available to lay out text.
    fn new() -> Option<Self> {
        let ctx = egui::Context::default();
        if !ui::fonts::install(&ctx) {
            eprintln!("skipping: no font found");
            return None;
        }
        theme::set(&ctx, ThemeKind::Dark);
        let mut driver = Self {
            ctx,
            view: TrackHeaderView {
                rows: rows(),
                ..TrackHeaderView::default()
            },
        };
        driver.frame(Vec::new());
        Some(driver)
    }

    fn frame(&mut self, events: Vec<Event>) -> TrackHeaderAction {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            events,
            ..RawInput::default()
        };
        let mut action = TrackHeaderAction::None;
        let view = &mut self.view;
        let mut output = self.ctx.run_ui(input, |ui| {
            action = track_headers(ui, column(), view);
        });
        output.textures_delta.clear();
        action
    }

    fn click(&mut self, track: TrackId, part: Part) -> TrackHeaderAction {
        let at = self
            .view
            .part_rect(column(), track, part)
            .unwrap_or_else(|| panic!("{part:?} of {track} not shown"))
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
        [a, b, c]
            .into_iter()
            .find(|a| *a != TrackHeaderAction::None)
            .unwrap_or(TrackHeaderAction::None)
    }

    fn key(&mut self, key: Key) -> TrackHeaderAction {
        let event = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        self.frame(vec![event(true), event(false)])
    }

    fn focus(&mut self, track: TrackId, part: Part) {
        self.ctx
            .memory_mut(|m| m.request_focus(part_id(track, part)));
        self.frame(Vec::new());
    }

    fn focused(&self) -> Option<egui::Id> {
        self.ctx.memory(|m| m.focused())
    }
}

#[test]
fn switches_emit_their_actions() {
    let Some(mut d) = Driver::new() else { return };
    assert_eq!(d.click(A1, Part::Mute), TrackHeaderAction::ToggleMute(A1));
    assert_eq!(d.click(A1, Part::Solo), TrackHeaderAction::ToggleSolo(A1));
    assert_eq!(d.click(A1, Part::Lock), TrackHeaderAction::ToggleLock(A1));
    assert_eq!(
        d.click(V1, Part::Visibility),
        TrackHeaderAction::ToggleVisible(V1)
    );
    assert_eq!(
        d.click(V1, Part::Height),
        TrackHeaderAction::SetHeight {
            track: V1,
            height: HeightPreset::Large
        }
    );
    assert_eq!(d.click(A2, Part::Row), TrackHeaderAction::Target(A2));
    // Video tracks have no mute or solo, audio tracks no visibility switch.
    assert!(d.view.part_rect(column(), V1, Part::Mute).is_none());
    assert!(d.view.part_rect(column(), A1, Part::Visibility).is_none());
}

#[test]
fn locked_tracks_refuse_switches_and_rename_but_unlock() {
    let Some(mut d) = Driver::new() else { return };
    d.view.rows[1].flags.locked = true;
    d.frame(Vec::new());
    assert_eq!(d.click(A1, Part::Mute), TrackHeaderAction::None);
    assert_eq!(d.click(A1, Part::Solo), TrackHeaderAction::None);
    d.focus(A1, Part::Row);
    d.key(Key::F2);
    assert_eq!(d.view.renaming, None, "a locked track is not renamed");
    assert_eq!(
        d.click(A1, Part::Height),
        TrackHeaderAction::SetHeight {
            track: A1,
            height: HeightPreset::Large
        }
    );
    assert_eq!(d.click(A1, Part::Lock), TrackHeaderAction::ToggleLock(A1));
}

#[test]
fn rename_keeps_with_enter_and_drops_with_escape() {
    let Some(mut d) = Driver::new() else { return };
    d.focus(A1, Part::Row);
    d.key(Key::F2);
    assert_eq!(d.view.renaming.as_ref().map(|r| r.track), Some(A1));
    d.frame(Vec::new());
    assert_eq!(d.focused(), Some(part_id(A1, Part::Name)));
    d.frame(vec![Event::Text(" room tone".into())]);
    assert_eq!(
        d.key(Key::Enter),
        TrackHeaderAction::Rename {
            track: A1,
            name: "A1 room tone".into()
        }
    );
    assert_eq!(d.view.renaming, None);

    d.focus(A1, Part::Row);
    d.key(Key::F2);
    d.frame(Vec::new());
    d.frame(vec![Event::Text("x".into())]);
    assert_eq!(d.key(Key::Escape), TrackHeaderAction::None);
    assert_eq!(d.view.renaming, None);

    // An unchanged or blank name renames nothing.
    d.focus(A1, Part::Row);
    d.key(Key::F2);
    d.frame(Vec::new());
    assert_eq!(d.key(Key::Enter), TrackHeaderAction::None);
}

#[test]
fn tab_walks_the_row_then_its_switches() {
    let Some(mut d) = Driver::new() else { return };
    d.focus(A1, Part::Row);
    let mut order = vec![d.focused()];
    for _ in 0..5 {
        d.key(Key::Tab);
        order.push(d.focused());
    }
    let expected: Vec<_> = [
        (A1, Part::Row),
        (A1, Part::Mute),
        (A1, Part::Solo),
        (A1, Part::Lock),
        (A1, Part::Height),
        (A2, Part::Row),
    ]
    .into_iter()
    .map(|(t, p)| Some(part_id(t, p)))
    .collect();
    assert_eq!(order, expected);
    // Enter presses the focused switch.
    d.focus(A1, Part::Solo);
    assert_eq!(d.key(Key::Enter), TrackHeaderAction::ToggleSolo(A1));
}

#[test]
fn solo_silences_the_other_audio_tracks_of_the_sequence() {
    let p = Project::new("uid-th", "Headers");
    let (p, seq) = p
        .add_sequence("Main", Rational::new(25, 1), 48_000)
        .expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, a2) = p.add_track(seq, TrackKind::Audio, "A2", None).expect("a2");
    let sequence = p.sequence(seq).expect("sequence");

    let mut state = TrackHeaderState::default();
    assert!(state.audible(sequence, a1) && state.audible(sequence, a2));
    assert!(!state.audible(sequence, v1), "video is never heard");
    state.apply(&TrackHeaderAction::ToggleSolo(a1));
    assert!(state.audible(sequence, a1));
    assert!(!state.audible(sequence, a2));
    state.apply(&TrackHeaderAction::ToggleMute(a1));
    assert!(!state.audible(sequence, a1), "mute wins over solo");
    state.apply(&TrackHeaderAction::ToggleSolo(a1));
    assert!(state.audible(sequence, a2));

    let rows = state.rows(sequence, Some(a2));
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [v1, a1, a2]);
    assert!(rows[0].flags == TrackFlags::default() && rows[1].flags.muted);
    assert!(rows[2].targeted && !rows[1].targeted);
}

#[test]
fn forty_track_headers_draw_within_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("track headers", Viewport::default()) else {
        return;
    };
    ui::theme::builtin(ThemeKind::Dark).apply(headless.context());
    let kinds = [
        TrackKind::Video,
        TrackKind::Audio,
        TrackKind::Subtitle,
        TrackKind::Adjustment,
    ];
    let mut view = TrackHeaderView {
        rows: (0..40)
            .map(|i| TrackRow {
                id: TrackId(i),
                kind: kinds[i as usize % 4],
                name: format!("Track {i}"),
                flags: TrackFlags {
                    muted: i % 5 == 0,
                    soloed: i == 9,
                    locked: i % 7 == 0,
                    hidden: i % 6 == 0,
                    height: HeightPreset::ALL[i as usize % 3],
                },
                targeted: i == 3,
            })
            .collect(),
        ..TrackHeaderView::default()
    };
    let mut monitor = FrameTimeMonitor::default();
    headless
        .run(30, 240, &mut monitor, |ui, frame| {
            egui::CentralPanel::default().show(ui, |ui| {
                // Scroll through the column so rows enter and leave.
                let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(HEADER_WIDTH, 720.0));
                view.scroll = (frame % 120) as f32 * 10.0;
                let _ = track_headers(ui, rect, &mut view);
            });
        })
        .expect("headless frames");
    let stats = monitor.stats();
    eprintln!("track headers ({}): {stats}", headless.adapter());
    headless.assert_p99_within_budget("track headers", &stats);
}
