//! Snapshot cases for the timeline canvas: the ruler at frame, second and minute zooms
//! and in 29.97 drop-frame across a minute, the minimap of a sample, an empty and a long
//! sequence, and the view's chrome (headers, ruler, lanes, scrollbar, minimap and zoom
//! slider) laid out together.

use egui::{Rect, pos2, vec2};
use project::{ClipSpec, Project, Rational, Sequence, SequenceFormat, Time, TimeRange, TrackKind};

use crate::panels::timeline::minimap::{self, MINIMAP_HEIGHT, MinimapContent};
use crate::panels::timeline::ruler::{self, RULER_HEIGHT, Timecode};
use crate::panels::timeline::track_header::{
    self, HEADER_WIDTH, HeightPreset, TrackHeaderState, TrackHeaderView,
};
use crate::panels::timeline::view::{
    self, Extent, SCROLLBAR_WIDTH, TimelineView, TrackLayout, ViewPos, ZOOM_SLIDER_WIDTH,
    sequence_duration,
};
use crate::t;
use crate::theme::{self, Role};

const STRIP_WIDTH: f32 = 640.0;
pub(super) const RULER_SIZE: egui::Vec2 = vec2(STRIP_WIDTH, RULER_HEIGHT);
pub(super) const MINIMAP_SIZE: egui::Vec2 = vec2(STRIP_WIDTH, MINIMAP_HEIGHT);
const VIEW_TRACKS: egui::Vec2 = vec2(632.0, 200.0);
pub(super) const VIEW_SIZE: egui::Vec2 = vec2(
    HEADER_WIDTH + VIEW_TRACKS.x + SCROLLBAR_WIDTH,
    RULER_HEIGHT + VIEW_TRACKS.y + MINIMAP_HEIGHT,
);

fn frames(n: i64, rate: Rational) -> Time {
    Time::from_frames(n, rate).expect("time")
}

/// A track's kind, name and clips (frame ranges).
type TrackSpec = (TrackKind, String, Vec<(i64, i64)>);

/// A sequence at `rate` with `tracks` holding clips at the given frame ranges.
fn sequence(rate: Rational, drop_frame: bool, tracks: &[TrackSpec]) -> Sequence {
    let p = Project::new("uid-timeline", "Timeline");
    let format = SequenceFormat {
        frame_rate: rate,
        drop_frame,
        ..SequenceFormat::default()
    };
    let (p, seq) = p.add_sequence_with("Main", format).expect("sequence");
    let (mut p, media) = p.add_media("hash", "clip.mov").expect("media");
    for (kind, name, clips) in tracks {
        let (next, track) = p.add_track(seq, *kind, name, None).expect("track");
        p = next;
        for &(a, b) in clips {
            let range = TimeRange::between(frames(a, rate), frames(b, rate)).expect("range");
            let spec = ClipSpec {
                media,
                range,
                source_start: Time::ZERO,
            };
            p = match kind {
                TrackKind::Video | TrackKind::Audio => p.insert_clip(track, spec).expect("clip").0,
                TrackKind::Subtitle => {
                    p.insert_subtitle(track, range, name.clone())
                        .expect("subtitle")
                        .0
                }
                TrackKind::Adjustment => p.insert_adjustment(track, range).expect("adj").0,
            };
        }
    }
    p.sequence(seq).expect("sequence").as_ref().clone()
}

/// The sample edit: two minutes at 25 fps over six tracks.
fn sample() -> Sequence {
    let r = Rational::new(25, 1);
    let s = |sec: i64| sec * 25;
    sequence(
        r,
        false,
        &[
            (
                TrackKind::Adjustment,
                t!("sample.track.grade"),
                vec![(s(30), s(70))],
            ),
            (
                TrackKind::Video,
                t!("sample.track.v2"),
                vec![(s(12), s(20)), (s(44), s(52)), (s(90), s(98))],
            ),
            (
                TrackKind::Video,
                t!("sample.track.v1"),
                vec![
                    (0, s(18)),
                    (s(18), s(41)),
                    (s(41), s(66)),
                    (s(70), s(104)),
                    (s(104), s(120)),
                ],
            ),
            (
                TrackKind::Subtitle,
                t!("sample.track.captions"),
                vec![(s(3), s(9)), (s(22), s(31)), (s(75), s(83))],
            ),
            (
                TrackKind::Audio,
                t!("sample.track.dialogue"),
                vec![(0, s(41)), (s(41), s(66)), (s(70), s(104))],
            ),
            (
                TrackKind::Audio,
                t!("sample.track.music"),
                vec![(s(6), s(118))],
            ),
        ],
    )
}

/// A two-hour edit at 24 fps: 200 clips over four tracks.
fn long() -> Sequence {
    let r = Rational::new(24, 1);
    let step = 2 * 60 * 60 * 24 / 50;
    let clips = |offset: i64| -> Vec<(i64, i64)> {
        (0..50)
            .map(|i| {
                let a = i * step + offset * (i % 3) * 24;
                (a, a + step - 24 * (1 + i % 4) - offset * (i % 3) * 24)
            })
            .collect()
    };
    sequence(
        r,
        false,
        &[
            (TrackKind::Video, t!("sample.track.v2"), clips(7)),
            (TrackKind::Video, t!("sample.track.v1"), clips(0)),
            (TrackKind::Audio, t!("sample.track.a1"), clips(0)),
            (TrackKind::Audio, t!("sample.track.a2"), clips(13)),
        ],
    )
}

fn placed(sequence: &Sequence, width: f32, height: f32, content: f32) -> TimelineView {
    let mut view = TimelineView::for_sequence(sequence);
    view.set_extent(Extent {
        width,
        height,
        duration: sequence_duration(sequence),
        content_height: content,
    });
    view
}

fn fill(ui: &mut egui::Ui, draw: impl FnOnce(&mut egui::Ui, Rect)) {
    let bg = theme::current(ui.ctx()).color(Role::Bg1);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(bg))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            draw(ui, rect);
        });
}

fn ruler_at(ui: &mut egui::Ui, tc: Timecode, start: f64, frame_px: f64) {
    fill(ui, |ui, rect| {
        let mut view = TimelineView::new(tc.rate());
        view.set_extent(Extent {
            width: rect.width(),
            duration: Time::seconds(2 * 60 * 60),
            ..Extent::default()
        });
        view.set_pos(ViewPos { start, frame_px });
        let _ = ruler::ruler(ui, rect, &view, tc);
    });
}

/// 30 fps at 40 points a frame: a label every third frame, a tick on each.
pub(super) fn ruler_frames(ui: &mut egui::Ui) {
    ruler_at(ui, Timecode::new(Rational::new(30, 1), false), 20.0, 40.0);
}

/// 25 fps at 40 points a second.
pub(super) fn ruler_seconds(ui: &mut egui::Ui) {
    ruler_at(ui, Timecode::new(Rational::new(25, 1), false), 0.0, 1.6);
}

/// 24 fps with ten minutes across.
pub(super) fn ruler_minutes(ui: &mut egui::Ui) {
    let frame_px = f64::from(STRIP_WIDTH) / (10.0 * 60.0 * 24.0);
    ruler_at(
        ui,
        Timecode::new(Rational::new(24, 1), false),
        0.0,
        frame_px,
    );
}

/// 29.97 drop-frame across the first minute: `00:01:00;02` follows `00:00:59;00`.
pub(super) fn ruler_drop_frame(ui: &mut egui::Ui) {
    let tc = Timecode::new(Rational::new(30000, 1001), true);
    ruler_at(ui, tc, 1740.0, 4.0);
}

fn minimap_of(ui: &mut egui::Ui, seq: &Sequence, pos: Option<ViewPos>) {
    let content = MinimapContent::from_sequence(seq);
    fill(ui, |ui, rect| {
        let mut view = placed(seq, rect.width(), 0.0, 0.0);
        if let Some(pos) = pos {
            view.set_pos(pos);
        }
        let _ = minimap::minimap(ui, rect, &mut view, &content);
    });
}

/// The sample edit with 30 s of it on screen, from 20 s.
pub(super) fn minimap_sample(ui: &mut egui::Ui) {
    let frame_px = f64::from(STRIP_WIDTH) / (30.0 * 25.0);
    minimap_of(
        ui,
        &sample(),
        Some(ViewPos {
            start: 500.0,
            frame_px,
        }),
    );
}

/// A sequence with no tracks, fitted.
pub(super) fn minimap_empty(ui: &mut egui::Ui) {
    minimap_of(ui, &sequence(Rational::new(25, 1), false, &[]), None);
}

/// The two-hour edit zoomed in to 20 seconds at about the 50-minute mark.
pub(super) fn minimap_long(ui: &mut egui::Ui) {
    let frame_px = f64::from(STRIP_WIDTH) / (20.0 * 24.0);
    minimap_of(
        ui,
        &long(),
        Some(ViewPos {
            start: 50.0 * 60.0 * 24.0,
            frame_px,
        }),
    );
}

/// The canvas's chrome over the sample edit: headers and lanes scrolled one row down,
/// the ruler over 45 seconds from 10 s, the scrollbar, the minimap and the zoom slider.
pub(super) fn view(ui: &mut egui::Ui) {
    let seq = sample();
    let content = MinimapContent::from_sequence(&seq);
    let mut state = TrackHeaderState::default();
    let music = seq.tracks[5].id;
    let _ = state.apply(&track_header::TrackHeaderAction::SetHeight {
        track: music,
        height: HeightPreset::Large,
    });
    let rows = state.rows(&seq, Some(seq.tracks[2].id));
    let layout = TrackLayout::from_rows(&rows);
    fill(ui, |ui, rect| {
        let tracks = Rect::from_min_size(
            pos2(rect.left() + HEADER_WIDTH, rect.top() + RULER_HEIGHT),
            VIEW_TRACKS,
        );
        let mut view = placed(
            &seq,
            tracks.width(),
            tracks.height(),
            layout.content_height(),
        );
        view.set_pos(ViewPos {
            start: 250.0,
            frame_px: f64::from(tracks.width()) / (45.0 * 25.0),
        });
        view.set_scroll_y(48.0);

        let ruler_rect = Rect::from_min_size(
            pos2(tracks.left(), rect.top()),
            vec2(tracks.width(), RULER_HEIGHT),
        );
        let _ = ruler::ruler(ui, ruler_rect, &view, Timecode::for_sequence(&seq));
        view::lanes(ui, tracks, &view, &layout);
        let mut headers = TrackHeaderView {
            rows,
            scroll: view.scroll_y(),
            ..TrackHeaderView::default()
        };
        let header_rect = Rect::from_min_size(
            pos2(rect.left(), tracks.top()),
            vec2(HEADER_WIDTH, tracks.height()),
        );
        let _ = track_header::track_headers(ui, header_rect, &mut headers);
        let bar = Rect::from_min_size(
            pos2(tracks.right(), tracks.top()),
            vec2(SCROLLBAR_WIDTH, tracks.height()),
        );
        let _ = view::track_scrollbar(ui, bar, &mut view);
        let map = Rect::from_min_size(
            pos2(tracks.left(), tracks.bottom()),
            vec2(tracks.width(), MINIMAP_HEIGHT),
        );
        let _ = minimap::minimap(ui, map, &mut view, &content);
        let slider = Rect::from_center_size(
            pos2(rect.left() + HEADER_WIDTH / 2.0, map.center().y),
            vec2(ZOOM_SLIDER_WIDTH, MINIMAP_HEIGHT),
        );
        let _ = view::zoom_slider(ui, slider, &mut view);
    });
}
