//! The About dialog: its views and links driven by pointer and keys on a real egui
//! context, its layout against the board, and every view within the frame budget.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use ui::dialogs::about::{
    AboutAction, AboutDialog, AboutInfo, Control, Licence, View, WIDTH, components,
};
use ui::frame_time::FrameTimeMonitor;
use ui::headless::{HeadlessUi, Viewport};
use ui::theme::{self, ThemeKind};

fn info() -> AboutInfo {
    AboutInfo {
        commit: Some("a7f3c1e".into()),
        build_date: Some("2026.09.30".into()),
        machine: Some("MacBook Air · Apple M2 · Metal".into()),
        ..AboutInfo::new("0.1.0", "https://github.com/OWNER/glass")
    }
}

struct Driver {
    ctx: egui::Context,
    dialog: AboutDialog,
    info: AboutInfo,
    copied: Option<String>,
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
            dialog: AboutDialog::new(),
            info: info(),
            copied: None,
        };
        driver.frame(Vec::new());
        driver.frame(Vec::new());
        Some(driver)
    }

    fn frame(&mut self, events: Vec<Event>) -> Option<AboutAction> {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1440.0, 900.0))),
            events,
            ..RawInput::default()
        };
        let mut action = None;
        let mut output = self.ctx.run_ui(input, |ui| {
            action = self.dialog.show(ui.ctx(), &self.info);
        });
        for command in &output.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                self.copied = Some(text.clone());
            }
        }
        output.textures_delta.clear();
        action
    }

    fn click_at(&mut self, at: Pos2) -> Option<AboutAction> {
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

    fn click(&mut self, control: Control) -> Option<AboutAction> {
        let at = self
            .dialog
            .control_rect(control)
            .unwrap_or_else(|| panic!("{control:?} not shown"))
            .center();
        self.click_at(at)
    }

    fn key(&mut self, key: Key) -> Option<AboutAction> {
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
fn read_and_view_list_go_one_level_in_and_back() {
    let Some(mut d) = Driver::new() else { return };
    assert_eq!(d.click(Control::Read(Licence::Mit)), None);
    assert_eq!(d.dialog.view(), View::Licence(Licence::Mit));
    assert_eq!(d.click(Control::Back), None);
    assert_eq!(d.dialog.view(), View::Main);
    d.click(Control::Read(Licence::Gpl));
    assert_eq!(d.dialog.view(), View::Licence(Licence::Gpl));
    assert_eq!(
        d.click(Control::FullLicence),
        Some(AboutAction::OpenUrl(
            "https://www.gnu.org/licenses/gpl-3.0.html".into()
        ))
    );
    assert_eq!(d.key(Key::Escape), None, "Escape goes back a level");
    assert_eq!(d.dialog.view(), View::Main);
    d.frame(Vec::new());
    d.click(Control::ViewList);
    assert_eq!(d.dialog.view(), View::ThirdParty);
    assert_eq!(d.click(Control::Close), Some(AboutAction::Close));
}

#[test]
fn the_third_party_list_selects_filters_and_links_to_sources() {
    let Some(mut d) = Driver::new() else { return };
    d.dialog.set_view(View::ThirdParty);
    d.frame(Vec::new());
    assert_eq!(d.click(Control::Component(0)), None);
    assert_eq!(d.dialog.selected(), Some(0));
    assert_eq!(components()[0].name, "FFmpeg");
    assert_eq!(
        d.click(Control::ComponentSource),
        Some(AboutAction::OpenUrl("https://ffmpeg.org".into()))
    );
    // A filter keeps the matching rows only; rows are drawn only while in view.
    d.dialog.set_filter("x26");
    d.frame(Vec::new());
    let shown: Vec<_> = (0..components().len())
        .filter(|&i| d.dialog.control_rect(Control::Component(i)).is_some())
        .map(|i| components()[i].name)
        .collect();
    assert_eq!(shown, ["x264", "x265"]);
    d.dialog.set_filter("");
    d.frame(Vec::new());
    let drawn = (0..components().len())
        .filter(|&i| d.dialog.control_rect(Control::Component(i)).is_some())
        .count();
    assert!(drawn < 14, "{drawn} of {} rows drawn", components().len());
}

#[test]
fn links_copy_and_keys() {
    let Some(mut d) = Driver::new() else { return };
    assert_eq!(
        d.click(Control::SourceArchives),
        Some(AboutAction::OpenUrl(
            "https://github.com/OWNER/glass/releases/tag/v0.1.0".into()
        ))
    );
    assert_eq!(d.click(Control::CopyDetails), None);
    assert_eq!(
        d.copied.as_deref(),
        Some("Glass 0.1.0 · build 2026.09.30 · a7f3c1e\nMacBook Air · Apple M2 · Metal")
    );
    assert!(d.dialog.shows_copied(d.ctx.input(|i| i.time)));
    assert_eq!(d.click(Control::Done), Some(AboutAction::Close));
    assert_eq!(d.key(Key::Enter), Some(AboutAction::Close));
    assert_eq!(d.key(Key::Escape), Some(AboutAction::Close));
}

#[test]
fn layout_follows_the_board() {
    let Some(d) = Driver::new() else { return };
    let rect = |c| d.dialog.control_rect(c).expect("shown");
    let close = rect(Control::Close);
    let done = rect(Control::Done);
    let copy = rect(Control::CopyDetails);
    let read = rect(Control::Read(Licence::Mit));
    let list = rect(Control::ViewList);
    let right = 720.0 + WIDTH / 2.0;
    assert_eq!(close.size(), vec2(24.0, 24.0));
    assert_eq!(close.right(), right - 12.0);
    assert_eq!(done.right(), right - 16.0);
    assert_eq!(done.height(), 32.0);
    assert_eq!(copy.right(), done.left() - 8.0);
    // Read and View list end 4 pt inside the licence card, which is 16 pt in.
    assert_eq!(read.right(), right - 20.0);
    assert_eq!(list.right(), right - 20.0);
    assert_eq!(read.height(), 20.0);
    assert!((list.center().y - read.center().y - 56.0).abs() < 0.01);
    let height = d.dialog.height(&d.ctx, &d.info);
    let bottom = 450.0 + height / 2.0;
    assert!((done.center().y - (bottom - 28.0)).abs() < 1.0);
}

#[test]
fn every_view_within_the_frame_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip(
        "about dialog",
        Viewport {
            width_px: 1440,
            height_px: 900,
            pixels_per_point: 1.0,
        },
    ) else {
        return;
    };
    let info = info();
    let views = [
        View::Main,
        View::ThirdParty,
        View::Licence(Licence::Gpl),
        View::Licence(Licence::Mit),
    ];
    for kind in ThemeKind::ALL {
        theme::set(headless.context(), kind);
        let mut dialog = AboutDialog::new();
        let mut monitor = FrameTimeMonitor::default();
        headless
            // Each view is drawn for 20 frames; warming up through all of them lays out
            // every view's text once. The dialog keeps those layouts, so the frames that
            // switch views paint them instead of laying the text out again.
            .run(20 * views.len(), 240, &mut monitor, |ui, frame| {
                dialog.set_view(views[(frame / 20) % views.len()]);
                dialog.select(Some(frame % 7));
                egui::CentralPanel::default().show(ui, |_| {});
                dialog.show(ui.ctx(), &info);
            })
            .expect("headless frames");
        assert!(dialog.control_rect(Control::Close).is_some(), "{kind:?}");
        let stats = monitor.stats();
        eprintln!("about dialog {kind:?} ({}): {stats}", headless.adapter());
        headless.assert_p99_within_budget("about dialog", &stats);
    }
}
