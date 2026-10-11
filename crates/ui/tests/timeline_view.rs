//! The timeline canvas's view: time and track conversions round-trip at every zoom, zoom
//! keeps the time under its anchor, zoom to fit and to a range land on frames with their
//! margins, animations settle and give way to gestures, the ruler's ticks adapt from
//! frames to hours and count drop-frame timecode, wheel and pinch input reach the view,
//! the minimap scrolls on whole frames, and a 200-clip sequence scrolls and zooms within
//! the frame budget.

mod support;

use egui::{Event, Modifiers, MouseWheelUnit, PointerButton, Pos2, RawInput, Rect, pos2, vec2};
use project::{ClipSpec, Project, Rational, Sequence, Time, TimeRange, TrackId, TrackKind};
use support::FrameBudget;
use ui::headless::{HeadlessUi, Viewport};
use ui::panels::timeline::minimap::{self, MinimapContent, window_rect};
use ui::panels::timeline::ruler::{self, Timecode, steps, tick_spacing, ticks};
use ui::panels::timeline::track_header::{
    HEADER_WIDTH, HeightPreset, TrackFlags, TrackHeaderView, TrackRow, track_headers,
};
use ui::panels::timeline::view::{
    self, ANIMATION_SECONDS, Extent, FIT_MARGIN, MAX_FRAME_WIDTH, SNAP_DISTANCE, SnapKind,
    SnapTargets, TimelineView, TrackLayout, ViewPos, ZOOM_STEP, gesture, sequence_duration,
};
use ui::theme::{self, ThemeKind};

const RATES: [Rational; 5] = [
    Rational::new(24000, 1001),
    Rational::new(24, 1),
    Rational::new(25, 1),
    Rational::new(30000, 1001),
    Rational::new(60, 1),
];

fn view(rate: Rational, duration: Time, width: f32) -> TimelineView {
    let mut v = TimelineView::new(rate);
    v.set_extent(Extent {
        width,
        height: 300.0,
        duration,
        content_height: 600.0,
    });
    v
}

fn hours(h: i64) -> Time {
    Time::seconds(h * 3600)
}

/// Zooms from the farthest to the closest in 40 log steps.
fn zooms(v: &TimelineView) -> Vec<f64> {
    let (lo, hi) = (v.min_frame_px().ln(), v.max_frame_px().ln());
    (0..=40)
        .map(|i| (lo + (hi - lo) * f64::from(i) / 40.0).exp())
        .collect()
}

#[test]
fn time_and_x_round_trip_at_every_zoom() {
    for rate in RATES {
        let mut v = view(rate, hours(10), 1600.0);
        for frame_px in zooms(&v) {
            for start in [0.0, 1234.5, 600_000.25] {
                v.set_pos(ViewPos { start, frame_px });
                let span = v.visible_frames();
                let first = v.frame_at_x(0.0);
                for k in 0..50 {
                    let frame = first + (span * f64::from(k) / 50.0) as i64;
                    let t = Time::from_frames(frame, rate).unwrap();
                    let x = v.time_to_x(t);
                    assert_eq!(x, v.frame_to_x(frame));
                    assert_eq!(v.x_to_time(x), t, "{rate} {frame_px} {start} {frame}");
                    assert_eq!(v.x_to_frame(x), frame);
                    // A point inside a frame's cell names that frame.
                    if frame_px >= 2.0 {
                        assert_eq!(v.frame_at_x(x + (frame_px / 2.0) as f32), frame);
                    }
                }
            }
        }
    }
}

#[test]
fn x_to_time_snaps_to_the_nearest_frame_boundary() {
    let rate = Rational::new(30000, 1001);
    let mut v = view(rate, hours(1), 1000.0);
    v.set_pos(ViewPos {
        start: 100.0,
        frame_px: 10.0,
    });
    assert_eq!(v.x_to_frame(14.9), 101);
    assert_eq!(v.x_to_frame(15.0), 102);
    assert_eq!(v.frame_at_x(19.9), 101);
    assert_eq!(v.x_to_time(20.0), Time::new(102 * 1001, 30000).unwrap());
    assert_eq!(v.frame_time(102), Time::from_frames(102, rate).unwrap());
    let visible = v.visible_range();
    assert_eq!(visible.start, Time::from_frames(100, rate).unwrap());
    assert_eq!(
        visible.end().unwrap(),
        Time::from_frames(200, rate).unwrap()
    );
}

#[test]
fn zoom_keeps_the_time_under_the_pointer() {
    for rate in RATES {
        let mut v = view(rate, hours(2), 1200.0);
        for frame_px in [0.05, 0.4, 3.0, 20.0] {
            for anchor in [0.0f32, 137.0, 600.0, 1199.0] {
                for factor in [0.8, 1.1, 1.5, 3.0] {
                    v.set_pos(ViewPos {
                        start: 40_000.0,
                        frame_px,
                    });
                    let before = v.x_to_frames(anchor);
                    v.zoom_at(factor, anchor);
                    assert!((v.frame_px() - frame_px * factor).abs() < 1e-9 * frame_px);
                    let after = v.x_to_frames(anchor);
                    assert!(
                        (after - before).abs() < 1e-6,
                        "{rate} {frame_px} x{factor} at {anchor}: {before} -> {after}"
                    );
                }
            }
        }
    }
}

#[test]
fn zoom_stops_at_one_frame_wide_and_at_the_whole_sequence() {
    let rate = Rational::new(25, 1);
    let mut v = view(rate, Time::seconds(600), 1048.0);
    v.zoom_at(1e9, 500.0);
    assert_eq!(v.frame_px(), MAX_FRAME_WIDTH);
    v.zoom_at(1e-9, 500.0);
    assert_eq!(v.frame_px(), v.fit_frame_px());
    // 1000 points of room over 600 s.
    assert!((v.fit_frame_px() - 1000.0 / (600.0 * 25.0)).abs() < 1e-12);
    // An empty sequence zooms out to ten seconds.
    let empty = view(rate, Time::ZERO, 1048.0);
    assert!((empty.fit_frame_px() - 1000.0 / 250.0).abs() < 1e-12);
    // A short clip in a wide window still stops at the frame-level limit.
    let short = view(rate, Time::new(1, 25).unwrap(), 100_000.0);
    assert_eq!(short.fit_frame_px(), MAX_FRAME_WIDTH);
}

#[test]
fn the_left_edge_stays_between_a_margin_before_zero_and_the_end_mid_view() {
    let rate = Rational::new(25, 1);
    let mut v = view(rate, Time::seconds(100), 1000.0);
    v.set_pos(ViewPos {
        start: -1e6,
        frame_px: 2.0,
    });
    assert_eq!(v.start(), (-f64::from(FIT_MARGIN) / 2.0).floor());
    v.pan(-1e7, 0.0);
    assert_eq!(v.start(), 2500.0 - 250.0);
    // Vertical scroll stops at the last row.
    v.pan(0.0, -1e6);
    assert_eq!(v.scroll_y(), 300.0);
    v.pan(0.0, 1e6);
    assert_eq!(v.scroll_y(), 0.0);
}

#[test]
fn zoom_to_fit_shows_the_whole_sequence_on_a_frame_boundary() {
    for rate in RATES {
        for duration in [Time::ZERO, Time::seconds(7), Time::seconds(95), hours(3)] {
            let mut v = view(rate, duration, 1280.0);
            v.reduced_motion = true;
            v.zoom_at(8.0, 300.0);
            v.zoom_to_fit(0.0);
            let pos = v.pos();
            assert_eq!(
                pos.start.fract(),
                0.0,
                "{rate} {duration}: start on a frame"
            );
            let zero = v.time_to_x(Time::ZERO);
            let px = pos.frame_px as f32;
            assert!(zero >= FIT_MARGIN - 0.01, "{rate} {duration}: {zero}");
            assert!(zero <= FIT_MARGIN + px + 0.01);
            let end = v.time_to_x(duration);
            assert!(end <= 1280.0 - FIT_MARGIN + px + 0.01, "{end}");
            if duration >= Time::seconds(10) {
                // Fit fills the room: the end sits at most a frame from the right margin.
                assert!(end >= 1280.0 - FIT_MARGIN - px - 0.01, "{end}");
            }
        }
    }
}

#[test]
fn zoom_to_a_range_frames_it_with_margins() {
    let rate = Rational::new(30000, 1001);
    let mut v = view(rate, hours(1), 1000.0);
    v.reduced_motion = true;
    let range = TimeRange::between(Time::seconds(600), Time::seconds(640)).unwrap();
    v.zoom_to_range(range, 0.0);
    let pos = v.pos();
    assert_eq!(pos.start.fract(), 0.0);
    let a = v.time_to_x(range.start);
    let b = v.time_to_x(range.end().unwrap());
    assert!(a >= FIT_MARGIN - 1e-3 && a <= FIT_MARGIN + pos.frame_px as f32 + 1e-3);
    assert!(
        (b - (1000.0 - FIT_MARGIN)).abs() <= pos.frame_px as f32 + 1e-3,
        "{b}"
    );

    // A range too short for the closest zoom is centred at that zoom.
    let frame = Time::from_frames(36_000, rate).unwrap();
    v.zoom_to_range(TimeRange::new(frame, Time::ZERO), 0.0);
    assert_eq!(v.frame_px(), MAX_FRAME_WIDTH);
    let x = v.time_to_x(frame);
    assert!((x - 500.0).abs() <= MAX_FRAME_WIDTH as f32, "{x}");
    assert_eq!(v.start().fract(), 0.0);
}

#[test]
fn animations_settle_and_gestures_stop_them() {
    let rate = Rational::new(25, 1);
    let mut v = view(rate, Time::seconds(600), 1000.0);
    v.set_pos(ViewPos {
        start: 5000.0,
        frame_px: 4.0,
    });
    let fit = v.fit();
    v.zoom_to_fit(10.0);
    assert!(v.is_animating());
    assert_eq!(v.target(), fit);
    assert!(v.advance(10.0 + ANIMATION_SECONDS / 2.0));
    let mid = v.pos();
    assert!(mid.frame_px < 4.0 && mid.frame_px > fit.frame_px);
    assert!(!v.advance(10.0 + ANIMATION_SECONDS));
    assert_eq!(v.pos(), fit);

    // A gesture during an animation stops it where it is.
    v.zoom_step(2, None, 20.0);
    v.advance(20.0 + ANIMATION_SECONDS / 3.0);
    let here = v.pos();
    v.pan(10.0, 0.0);
    assert!(!v.is_animating());
    assert!((v.start() - (here.start - 10.0 / here.frame_px)).abs() < 1e-9);

    // Reduced motion jumps.
    v.reduced_motion = true;
    v.zoom_to_fit(30.0);
    assert!(!v.is_animating());
    assert_eq!(v.pos(), fit);
}

#[test]
fn keyboard_zoom_anchors_at_the_playhead_and_builds_on_the_running_animation() {
    let rate = Rational::new(25, 1);
    let mut v = view(rate, Time::seconds(600), 1000.0);
    v.set_pos(ViewPos {
        start: 1000.0,
        frame_px: 0.5,
    });
    let playhead = Time::seconds(60);
    let x = v.time_to_x(playhead);
    assert!((0.0..1000.0).contains(&x));
    v.zoom_step(1, Some(playhead), 0.0);
    // Halfway through, the playhead has not moved.
    v.advance(ANIMATION_SECONDS / 2.0);
    assert!((v.time_to_x(playhead) - x).abs() < 1e-3);
    // A second press goes on from the first one's end.
    v.zoom_step(1, Some(playhead), ANIMATION_SECONDS / 2.0);
    assert!((v.target().frame_px - 0.5 * ZOOM_STEP * ZOOM_STEP).abs() < 1e-12);
    v.advance(1.0);
    assert!((v.time_to_x(playhead) - x).abs() < 1e-3);
    // Off screen, the middle is the anchor.
    let middle = v.x_to_frames(500.0);
    v.zoom_step(-1, Some(Time::seconds(590)), 2.0);
    v.advance(3.0);
    assert!((v.x_to_frames(500.0) - middle).abs() < 1e-6);

    assert!(v.run_command("view.zoom_in", None, 4.0));
    assert!(v.run_command("view.zoom_out", None, 4.0));
    assert!(v.run_command("view.zoom_to_fit", None, 4.0));
    assert!(!v.run_command("edit.undo", None, 4.0));
    v.advance(5.0);
    assert_eq!(v.pos(), v.fit());
}

#[test]
fn tick_spacing_widens_from_frames_to_hours() {
    for nominal in [24, 25, 30, 50, 60] {
        let all = steps(nominal);
        let mut previous = 0;
        for i in 0..=80 {
            let label_px = 1e-5 * 1.25f64.powi(i);
            let s = tick_spacing(label_px, nominal, 90.0, 6.0);
            assert!(
                s.major as f64 * label_px >= 90.0,
                "{nominal} {label_px}: {s:?}"
            );
            if let Some(smaller) = all.iter().rev().find(|x| **x < s.major) {
                assert!(
                    *smaller as f64 * label_px < 90.0,
                    "{nominal}: not the smallest"
                );
            }
            assert_eq!(s.major % s.minor, 0);
            assert!(s.major / s.minor <= 10);
            assert!(s.minor == s.major || s.minor as f64 * label_px >= 6.0);
            assert!(
                previous == 0 || s.major <= previous,
                "zooming in never widens"
            );
            previous = s.major;
        }
    }
    // Named zooms at 25 fps with 90-point labels.
    let at = |label_px| tick_spacing(label_px, 25, 90.0, 6.0);
    assert_eq!(at(100.0).major, 1, "a frame per label");
    assert_eq!(at(10.0).major, 25, "a second");
    assert_eq!(at(10.0).minor, 5);
    assert_eq!(at(1.0).major, 5 * 25);
    assert_eq!(at(1.0).minor, 25);
    assert_eq!(at(0.02).major, 5 * 60 * 25, "five minutes");
    assert_eq!(at(0.001).major, 3600 * 25, "an hour");
}

#[test]
fn timecode_counts_drop_frame_at_29_97_and_59_94() {
    let df = Timecode::new(Rational::new(30000, 1001), true);
    assert!(df.is_drop_frame());
    for (frame, text) in [
        (0, "00:00:00;00"),
        (29, "00:00:00;29"),
        (1799, "00:00:59;29"),
        (1800, "00:01:00;02"),
        (3597, "00:01:59;29"),
        (3598, "00:02:00;02"),
        (17981, "00:09:59;29"),
        (17982, "00:10:00;00"),
        (17983, "00:10:00;01"),
        (19782, "00:11:00;02"),
        (107_892, "01:00:00;00"),
        (-1800, "-00:01:00;02"),
    ] {
        assert_eq!(df.format(frame), text, "frame {frame}");
    }
    // Every label number maps back to its frame.
    for frame in 0..60_000 {
        assert_eq!(df.frame_of_label(df.label_number(frame)), frame);
    }
    // Dropped numbers give the first frame of their minute.
    assert_eq!(df.frame_of_label(1800), 1800);
    assert_eq!(df.frame_of_label(1801), 1800);

    let df60 = Timecode::new(Rational::new(60000, 1001), true);
    assert_eq!(df60.format(3600), "00:01:00;04");
    assert_eq!(df60.format(35_964), "00:10:00;00");
    for frame in 0..80_000 {
        assert_eq!(df60.frame_of_label(df60.label_number(frame)), frame);
    }

    // Non-drop counts nominal frames; drop-frame is refused where it does not apply.
    let ndf = Timecode::new(Rational::new(30000, 1001), false);
    assert_eq!(ndf.format(1800), "00:01:00:00");
    assert_eq!(
        Timecode::new(Rational::new(24000, 1001), false).format(24),
        "00:00:01:00"
    );
    let pal = Timecode::new(Rational::new(25, 1), true);
    assert!(!pal.is_drop_frame());
    assert_eq!(pal.format(25 * 3725 + 3), "01:02:05:03");
    assert_eq!(pal.nominal(), 25);
}

#[test]
fn drop_frame_ticks_land_on_the_minute_and_never_repeat() {
    let df = Timecode::new(Rational::new(30000, 1001), true);
    let spacing = tick_spacing(4.0 * df.frames_per_label(), 30, 90.0, 6.0);
    assert_eq!((spacing.major, spacing.minor), (30, 3));
    let ticks = ticks(df, spacing, 1740, 1900);
    assert!(ticks.windows(2).all(|w| w[0].frame < w[1].frame));
    let majors: Vec<String> = ticks
        .iter()
        .filter(|t| t.major)
        .map(|t| df.format(t.frame))
        .collect();
    assert_eq!(
        majors,
        [
            "00:00:58;00",
            "00:00:59;00",
            "00:01:00;02",
            "00:01:01;00",
            "00:01:02;00",
            "00:01:03;00"
        ]
    );
    // One-frame ticks skip the dropped numbers rather than doubling a frame.
    let fine = ruler::TickSpacing {
        major: 30,
        minor: 1,
    };
    let ticks = ruler::ticks(df, fine, 1795, 1805);
    let frames: Vec<i64> = ticks.iter().map(|t| t.frame).collect();
    assert!(frames.windows(2).all(|w| w[0] < w[1]));
    assert!(frames.contains(&1800) && frames.contains(&1801));

    // Non-drop ticks fall every step from zero, nothing before it.
    let ndf = Timecode::new(Rational::new(25, 1), false);
    let t = ticks_at(ndf, 25, 5, -100, 60);
    assert_eq!(t, [0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55, 60]);
}

fn ticks_at(tc: Timecode, major: i64, minor: i64, first: i64, last: i64) -> Vec<i64> {
    ticks(tc, ruler::TickSpacing { major, minor }, first, last)
        .iter()
        .map(|t| t.frame)
        .collect()
}

#[test]
fn tracks_map_to_rows_and_back_through_the_scroll() {
    let layout = TrackLayout::new([
        (TrackId(1), HeightPreset::Small.height()),
        (TrackId(2), HeightPreset::Medium.height()),
        (TrackId(3), HeightPreset::Large.height()),
    ]);
    assert_eq!(layout.content_height(), 148.0);
    assert_eq!(layout.track_at(0.0), Some(TrackId(1)));
    assert_eq!(layout.track_at(27.9), Some(TrackId(1)));
    assert_eq!(layout.track_at(28.0), Some(TrackId(2)));
    assert_eq!(layout.track_at(147.9), Some(TrackId(3)));
    assert_eq!(layout.track_at(148.0), None);
    assert_eq!(layout.track_at(-0.1), None);

    let mut v = TimelineView::new(Rational::new(25, 1));
    v.set_extent(Extent {
        width: 500.0,
        height: 100.0,
        duration: Time::seconds(10),
        content_height: layout.content_height(),
    });
    v.set_scroll_y(30.0);
    let y = v.track_to_y(&layout, TrackId(2)).unwrap();
    assert_eq!((y.min, y.max), (-2.0, 46.0));
    for (track, span) in layout.iter() {
        let on_screen = v.track_to_y(&layout, track).unwrap();
        assert_eq!(on_screen.min, span.min - 30.0);
        assert_eq!(v.y_to_track(&layout, on_screen.min + 1.0), Some(track));
    }
    // The headers' rows give the same layout.
    let rows: Vec<TrackRow> = [HeightPreset::Small, HeightPreset::Large]
        .into_iter()
        .enumerate()
        .map(|(i, height)| TrackRow {
            id: TrackId(i as u64),
            kind: TrackKind::Video,
            name: String::new(),
            flags: TrackFlags {
                height,
                ..TrackFlags::default()
            },
            targeted: false,
        })
        .collect();
    assert_eq!(TrackLayout::from_rows(&rows).content_height(), 100.0);
}

#[test]
fn edits_snap_to_targets_within_the_snap_distance() {
    let rate = Rational::new(25, 1);
    let mut v = view(rate, Time::seconds(100), 1000.0);
    v.set_pos(ViewPos {
        start: 0.0,
        frame_px: 2.0,
    });
    let mut targets = SnapTargets::new();
    targets.push(Time::seconds(4), SnapKind::Marker);
    targets.push(Time::seconds(2), SnapKind::ClipEdge);
    targets.push(Time::seconds(3), SnapKind::Playhead);
    assert_eq!(
        targets.iter().map(|t| t.0).collect::<Vec<_>>(),
        [2, 3, 4].map(Time::seconds)
    );
    // 2 points a frame: SNAP_DISTANCE points is 4 frames.
    let within = Time::from_frames(75 + (SNAP_DISTANCE / 2.0) as i64, rate).unwrap();
    let snap = v.snap(within, &targets);
    assert_eq!(snap.time, Time::seconds(3));
    assert_eq!(snap.target, Some(SnapKind::Playhead));
    // Out of reach, the time goes to the nearest frame.
    let far = Time::new(2 * 25 + 12, 25).unwrap();
    let off = far.checked_add(Time::new(1, 100).unwrap()).unwrap();
    let snap = v.snap(off, &targets);
    assert_eq!((snap.time, snap.target), (far, None));
    assert_eq!(SnapTargets::new().nearest(Time::ZERO), None);
}

#[test]
fn wheel_and_pinch_events_become_zoom_and_pan() {
    let wheel = |dy: f32, modifiers| Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: vec2(0.0, dy),
        phase: egui::TouchPhase::Move,
        modifiers,
    };
    let g = gesture(&[wheel(100.0, Modifiers::CTRL)], 50.0, 400.0);
    assert!((g.zoom - (0.5f64).exp()).abs() < 1e-9 && g.pan == egui::Vec2::ZERO);
    let g = gesture(&[wheel(-100.0, Modifiers::ALT)], 50.0, 400.0);
    assert!((g.zoom - (-0.5f64).exp()).abs() < 1e-9);
    let g = gesture(&[wheel(10.0, Modifiers::SHIFT)], 50.0, 400.0);
    assert_eq!((g.zoom, g.pan), (1.0, vec2(10.0, 0.0)));
    let g = gesture(&[wheel(10.0, Modifiers::NONE)], 50.0, 400.0);
    assert_eq!(g.pan, vec2(0.0, 10.0));
    let lines = Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: vec2(1.0, 0.0),
        phase: egui::TouchPhase::Move,
        modifiers: Modifiers::NONE,
    };
    assert_eq!(gesture(&[lines], 50.0, 400.0).pan, vec2(50.0, 0.0));
    let g = gesture(&[Event::Zoom(1.5), Event::Zoom(2.0)], 50.0, 400.0);
    assert_eq!(g.zoom, 3.0);
}

#[test]
fn sequence_duration_reaches_the_last_item_or_marker() {
    let s = sample_sequence(4, 10);
    assert_eq!(sequence_duration(&s), Time::seconds(4 * 10 * 2));
}

/// `clips` clips back to back on each of `tracks` tracks (half video, half audio), each
/// `2 * tracks` seconds long, at 25 fps.
fn sample_sequence(tracks: usize, clips: i64) -> Sequence {
    let rate = Rational::new(25, 1);
    let p = Project::new("uid-view", "View");
    let (p, seq) = p.add_sequence("Main", rate, 48_000).expect("sequence");
    let (mut p, media) = p.add_media("hash", "clip.mov").expect("media");
    for t in 0..tracks {
        let kind = if t < tracks / 2 {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        let (next, track) = p.add_track(seq, kind, format!("T{t}"), None).unwrap();
        p = next;
        for i in 0..clips {
            let len = Time::seconds(2 * tracks as i64);
            let start = Time::seconds(2 * tracks as i64 * i);
            let spec = ClipSpec {
                media,
                range: TimeRange::new(start, len),
                source_start: Time::ZERO,
            };
            p = p.insert_clip(track, spec).unwrap().0;
        }
    }
    p.sequence(seq).unwrap().as_ref().clone()
}

/// Drives the view's input and the minimap on a real egui context.
struct Driver {
    ctx: egui::Context,
    view: TimelineView,
    content: MinimapContent,
    time: f64,
}

const TRACKS: Rect = Rect {
    min: Pos2 { x: 200.0, y: 28.0 },
    max: Pos2 {
        x: 1000.0,
        y: 428.0,
    },
};
const MINIMAP: Rect = Rect {
    min: Pos2 { x: 200.0, y: 428.0 },
    max: Pos2 {
        x: 1000.0,
        y: 452.0,
    },
};

impl Driver {
    fn new() -> Option<Self> {
        let ctx = egui::Context::default();
        if !ui::fonts::install(&ctx) {
            eprintln!("skipping: no font found");
            return None;
        }
        theme::set(&ctx, ThemeKind::Dark);
        let seq = sample_sequence(4, 30);
        let mut view = TimelineView::for_sequence(&seq);
        view.set_extent(Extent {
            width: TRACKS.width(),
            height: TRACKS.height(),
            duration: sequence_duration(&seq),
            content_height: 4.0 * 72.0 + 200.0,
        });
        let mut d = Self {
            ctx,
            view,
            content: MinimapContent::from_sequence(&seq),
            time: 0.0,
        };
        d.frame(Vec::new());
        Some(d)
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.time += 1.0 / 60.0;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 600.0))),
            time: Some(self.time),
            events,
            ..RawInput::default()
        };
        let (view, content) = (&mut self.view, &self.content);
        let mut output = self.ctx.run_ui(input, |ui| {
            let hover = Rect::from_min_max(pos2(0.0, TRACKS.top()), TRACKS.max);
            view.handle_input(ui, hover, TRACKS.left());
            let _ = minimap::minimap(ui, MINIMAP, view, content);
        });
        output.textures_delta.clear();
    }

    fn wheel(&mut self, at: Pos2, delta: egui::Vec2, modifiers: Modifiers) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta,
                phase: egui::TouchPhase::Move,
                modifiers,
            },
        ]);
    }

    fn drag(&mut self, from: Pos2, to: Pos2) {
        let button = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        self.frame(vec![Event::PointerMoved(from), button(from, true)]);
        for i in 1..=8 {
            let p = from + (to - from) * (i as f32 / 8.0);
            self.frame(vec![Event::PointerMoved(p)]);
        }
        self.frame(vec![button(to, false)]);
        self.frame(Vec::new());
    }

    fn click(&mut self, at: Pos2) {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        self.frame(vec![Event::PointerMoved(at), button(true)]);
        self.frame(vec![button(false)]);
        self.frame(Vec::new());
    }
}

#[test]
fn modifier_scroll_and_pinch_zoom_about_the_pointer_and_scroll_pans() {
    let Some(mut d) = Driver::new() else { return };
    d.view.set_pos(ViewPos {
        start: 2000.0,
        frame_px: 1.0,
    });
    let at = pos2(TRACKS.left() + 300.0, 200.0);
    let under = d.view.x_to_frames(300.0);
    d.wheel(at, vec2(0.0, 100.0), Modifiers::CTRL);
    assert!((d.view.frame_px() - 0.5f64.exp()).abs() < 1e-6);
    assert!((d.view.x_to_frames(300.0) - under).abs() < 1e-6);
    d.wheel(at, vec2(0.0, -100.0), Modifiers::ALT);
    assert!((d.view.frame_px() - 1.0).abs() < 1e-6);
    d.frame(vec![Event::PointerMoved(at), Event::Zoom(2.0)]);
    assert!((d.view.frame_px() - 2.0).abs() < 1e-6);
    assert!((d.view.x_to_frames(300.0) - under).abs() < 1e-6);

    let start = d.view.start();
    d.wheel(at, vec2(0.0, -40.0), Modifiers::SHIFT);
    assert!(
        (d.view.start() - (start + 20.0)).abs() < 1e-9,
        "shift pans right"
    );
    d.wheel(at, vec2(0.0, -50.0), Modifiers::NONE);
    assert_eq!(d.view.scroll_y(), 50.0);
    // Over the headers the wheel still scrolls the tracks.
    d.wheel(pos2(100.0, 200.0), vec2(0.0, 20.0), Modifiers::NONE);
    assert_eq!(d.view.scroll_y(), 30.0);
    // Outside the timeline nothing moves.
    let before = d.view.pos();
    d.wheel(pos2(1100.0, 500.0), vec2(0.0, 100.0), Modifiers::CTRL);
    assert_eq!(d.view.pos(), before);
}

#[test]
fn dragging_the_minimap_window_scrolls_on_whole_frames() {
    let Some(mut d) = Driver::new() else { return };
    d.view.set_pos(ViewPos {
        start: 1000.0,
        frame_px: 0.75,
    });
    let window = window_rect(MINIMAP, &d.view, &d.content);
    let from = window.center();
    d.drag(from, from + vec2(123.4, 0.0));
    assert_eq!(d.view.start().fract(), 0.0, "{}", d.view.start());
    assert!(d.view.start() > 1000.0);
    let moved = window_rect(MINIMAP, &d.view, &d.content);
    assert!((moved.center().x - (from.x + 123.4)).abs() < 1.5);

    // A click outside the window centres it there.
    d.click(pos2(MINIMAP.left() + 10.0, MINIMAP.center().y));
    assert_eq!(d.view.start().fract(), 0.0);
    let centred = window_rect(MINIMAP, &d.view, &d.content);
    assert!(centred.left() <= MINIMAP.left() + 10.0 || d.view.start() <= 0.0);
}

#[test]
fn two_hundred_clips_scroll_and_zoom_within_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("timeline view", Viewport::default()) else {
        return;
    };
    theme::builtin(ThemeKind::Dark).apply(headless.context());
    let seq = sample_sequence(8, 25);
    assert_eq!(seq.tracks.iter().map(|t| t.items.len()).sum::<usize>(), 200);
    let tc = Timecode::for_sequence(&seq);
    let rows: Vec<TrackRow> = seq
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| TrackRow {
            id: t.id,
            kind: t.kind,
            name: t.name.clone(),
            flags: TrackFlags {
                height: HeightPreset::ALL[i % 3],
                ..TrackFlags::default()
            },
            targeted: i == 1,
        })
        .collect();
    let layout = TrackLayout::from_rows(&rows);
    let mut headers = TrackHeaderView {
        rows,
        ..TrackHeaderView::default()
    };
    let mut v = TimelineView::for_sequence(&seq);
    let duration = sequence_duration(&seq);
    let stats = FrameBudget::run(&mut headless, 30, 240, |ui, frame| {
        egui::CentralPanel::default().show(ui, |ui| {
            let full = ui.max_rect();
            let tracks = Rect::from_min_max(
                pos2(full.left() + HEADER_WIDTH, full.top() + ruler::RULER_HEIGHT),
                pos2(
                    full.right() - view::SCROLLBAR_WIDTH,
                    full.bottom() - minimap::MINIMAP_HEIGHT,
                ),
            );
            v.set_extent(Extent {
                width: tracks.width(),
                height: tracks.height(),
                duration,
                content_height: layout.content_height(),
            });
            // Zoom in and out about a moving point, then pan, as a user would.
            match frame % 60 {
                0..20 => v.zoom_at(1.15, (frame % 37) as f32 * 20.0),
                20..40 => v.zoom_at(1.0 / 1.15, (frame % 23) as f32 * 30.0),
                _ => v.pan(-25.0, if frame % 2 == 0 { -6.0 } else { 4.0 }),
            }
            v.handle_input(ui, tracks, tracks.left());
            let content = MinimapContent::from_sequence(&seq);
            let ruler_rect =
                Rect::from_min_max(pos2(tracks.left(), full.top()), tracks.right_top());
            let _ = ruler::ruler(ui, ruler_rect, &v, tc);
            view::lanes(ui, tracks, &v, &layout);
            headers.scroll = v.scroll_y();
            let header_rect = Rect::from_min_max(
                pos2(full.left(), tracks.top()),
                pos2(tracks.left(), tracks.bottom()),
            );
            let _ = track_headers(ui, header_rect, &mut headers);
            let bar = Rect::from_min_max(tracks.right_top(), pos2(full.right(), tracks.bottom()));
            let _ = view::track_scrollbar(ui, bar, &mut v);
            let map = Rect::from_min_max(tracks.left_bottom(), pos2(tracks.right(), full.bottom()));
            let _ = minimap::minimap(ui, map, &mut v, &content);
            let slider = Rect::from_center_size(
                pos2(full.left() + HEADER_WIDTH / 2.0, map.center().y),
                vec2(view::ZOOM_SLIDER_WIDTH, minimap::MINIMAP_HEIGHT),
            );
            let _ = view::zoom_slider(ui, slider, &mut v);
        });
    })
    .expect("headless frames");
    eprintln!("timeline view ({}): {stats}", headless.adapter());
    stats.assert_within_budget(&headless, "timeline view");
}
