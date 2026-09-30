//! The recovery dialog: its flow driven by pointer and keys on a real egui context, its
//! layout against the board, and every state within the frame budget.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use ui::dialogs::recovery::{
    Control, RecoveryAction, RecoveryDialog, RecoveryPrompt, RecoveryState, WIDTH,
};
use ui::frame_time::FrameTimeMonitor;
use ui::headless::{HeadlessUi, Viewport};
use ui::theme::{self, ThemeKind};

fn prompt() -> RecoveryPrompt {
    RecoveryPrompt {
        project_name: "Harbour film".into(),
        sequence_name: "Main sequence".into(),
        sequence_duration: "7:48".into(),
        autosave_time: "Today 14:32:08".into(),
        saved_time: Some("13:58".into()),
        before_crash_ms: 2 * 60_000 + 5_000,
        edits: 38,
        poster: None,
        timecode: Some("01:04:22:17".into()),
    }
}

struct Driver {
    ctx: egui::Context,
    dialog: RecoveryDialog,
    prompt: RecoveryPrompt,
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
            dialog: RecoveryDialog::new(),
            prompt: prompt(),
        };
        driver.frame(Vec::new());
        driver.frame(Vec::new());
        Some(driver)
    }

    fn frame(&mut self, events: Vec<Event>) -> Option<RecoveryAction> {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440.0, 900.0))),
            events,
            ..RawInput::default()
        };
        let mut action = None;
        let _ = self.ctx.run_ui(input, |ui| {
            action = self.dialog.show(ui.ctx(), &self.prompt);
        });
        action
    }

    fn click(&mut self, control: Control) -> Option<RecoveryAction> {
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

    fn key(&mut self, key: Key) -> Option<RecoveryAction> {
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
fn restore_shows_progress_until_done_or_cancelled() {
    let Some(mut d) = Driver::new() else { return };
    assert_eq!(d.dialog.state(), RecoveryState::Prompt);
    assert_eq!(d.click(Control::Restore), Some(RecoveryAction::Restore));
    assert_eq!(d.dialog.state(), RecoveryState::Restoring { progress: 0.0 });
    d.dialog.set_progress(0.62);
    d.frame(Vec::new());
    assert!(d.dialog.control_rect(Control::Restore).is_none());
    assert_eq!(
        d.click(Control::Cancel),
        Some(RecoveryAction::CancelRestore)
    );
    // The close button does nothing while restoring.
    assert_eq!(d.click(Control::Close), None);
}

#[test]
fn discard_asks_once_more() {
    let Some(mut d) = Driver::new() else { return };
    assert_eq!(d.click(Control::Discard), None);
    assert_eq!(d.dialog.state(), RecoveryState::ConfirmDiscard);
    assert_eq!(d.click(Control::Keep), None);
    assert_eq!(d.dialog.state(), RecoveryState::Prompt);
    assert_eq!(d.click(Control::Discard), None);
    assert_eq!(
        d.key(Key::Escape),
        None,
        "Escape backs out of the confirmation"
    );
    assert_eq!(d.dialog.state(), RecoveryState::Prompt);
    d.click(Control::Discard);
    assert_eq!(
        d.click(Control::ConfirmDiscard),
        Some(RecoveryAction::Discard)
    );
}

#[test]
fn keys_folder_and_close() {
    let Some(mut d) = Driver::new() else { return };
    assert_eq!(
        d.click(Control::ShowFolder),
        Some(RecoveryAction::ShowFolder)
    );
    assert_eq!(d.click(Control::Close), Some(RecoveryAction::Later));
    assert_eq!(d.key(Key::Escape), Some(RecoveryAction::Later));
    assert_eq!(d.key(Key::Enter), Some(RecoveryAction::Restore));
}

#[test]
fn layout_follows_the_board() {
    let Some(d) = Driver::new() else { return };
    let rect = |c| d.dialog.control_rect(c).expect("shown");
    let close = rect(Control::Close);
    let restore = rect(Control::Restore);
    let discard = rect(Control::Discard);
    let folder = rect(Control::ShowFolder);
    // Centred on the 1440 x 900 screen, 520 wide.
    let left = 720.0 - WIDTH / 2.0;
    let right = 720.0 + WIDTH / 2.0;
    assert_eq!(close.size(), vec2(32.0, 32.0));
    assert_eq!(close.right(), right - 8.0);
    assert_eq!(restore.right(), right - 16.0);
    assert_eq!(restore.height(), 32.0);
    assert_eq!(discard.right(), restore.left() - 8.0);
    assert_eq!(folder.left(), left + 8.0);
    assert!((restore.center().y - folder.center().y).abs() < 0.01);
    assert!(close.center().y < restore.center().y);
    let height = d.dialog.height(&d.ctx, &d.prompt);
    // Footer buttons sit in the middle of the 56 pt footer at the bottom.
    let bottom = 450.0 + height / 2.0;
    assert!((restore.center().y - (bottom - 28.0)).abs() < 1.0);
}

#[test]
fn every_state_within_the_frame_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip(
        "recovery dialog",
        Viewport {
            width_px: 1440,
            height_px: 900,
            pixels_per_point: 1.0,
        },
    ) else {
        return;
    };
    let prompt = prompt();
    let states = [
        RecoveryState::Prompt,
        RecoveryState::ConfirmDiscard,
        RecoveryState::Restoring { progress: 0.62 },
    ];
    for kind in ThemeKind::ALL {
        theme::set(headless.context(), kind);
        let mut dialog = RecoveryDialog::new();
        let mut monitor = FrameTimeMonitor::default();
        headless
            .run(30, 240, &mut monitor, |ui, frame| {
                let state = states[(frame / 20) % states.len()];
                if let RecoveryState::Restoring { .. } = state {
                    dialog.set_progress((frame % 100) as f32 / 100.0);
                } else {
                    dialog.set_state(state);
                }
                egui::CentralPanel::default().show(ui, |_| {});
                dialog.show(ui.ctx(), &prompt);
            })
            .expect("headless frames");
        assert!(dialog.control_rect(Control::Close).is_some(), "{kind:?}");
        let stats = monitor.stats();
        eprintln!("recovery dialog {kind:?} ({}): {stats}", headless.adapter());
        headless.assert_p99_within_budget("recovery dialog", &stats);
    }
}
