//! The clips in the track lanes: thumbnail tiles pick their source times through the
//! clip's speed and only the visible ones are looked up, strip and peak levels follow
//! the zoom, missing analysis becomes one request each, off-screen clips are culled and
//! zoomed-out clips merge into blocks, and 500 clips with thumbnails and waveforms
//! scroll and zoom within the frame budget.

mod support;

use std::sync::Arc;

use egui::{Rangef, Rect, pos2};
use engine::media::RgbaImage;
use engine::media::analysis::peaks::{LEVELS, Peak, PeakLevel, Peaks};
use engine::media::analysis::thumbnails::{
    ATLAS_COLUMNS, AtlasFormat, PER_ATLAS, THUMBNAILS_VERSION, ThumbnailLevel, ThumbnailStrips,
};
use project::{
    ClipSpec, MediaId, Project, Rational, Sequence, SequenceFormat, SpeedKeep, Time, TimeRange,
    TrackKind,
};
use support::FrameBudget;
use ui::headless::{HeadlessUi, Viewport};
use ui::panels::timeline::clip_media::{AtlasKey, ClipMedia, MediaRequest};
use ui::panels::timeline::clips::{
    self, ClipStats, ClipsView, clip_speed, samples_per_point, speed_text, thumb_level,
    thumb_slots, waveform_level,
};
use ui::panels::timeline::view::{self, Extent, TimelineView, TrackLayout, ViewPos};
use ui::theme::{self, ThemeKind};

const RATE: Rational = Rational::new(25, 1);

fn level(level: u32, interval: u32, n: u32) -> ThumbnailLevel {
    ThumbnailLevel {
        level,
        interval,
        times_us: (0..n)
            .map(|i| i64::from(i * interval) * 1_000_000)
            .collect(),
        atlases: (0..n.div_ceil(PER_ATLAS))
            .map(|c| format!("l{level}.c{c}.jpg"))
            .collect(),
    }
}

fn strips(seconds: u32) -> ThumbnailStrips {
    ThumbnailStrips {
        version: THUMBNAILS_VERSION,
        width: 64,
        height: 36,
        format: AtlasFormat::Jpeg,
        columns: ATLAS_COLUMNS,
        per_atlas: PER_ATLAS,
        levels: [1, 5, 30]
            .iter()
            .enumerate()
            .map(|(i, &iv)| level(i as u32, iv, seconds.div_ceil(iv)))
            .collect(),
    }
}

fn atlas() -> RgbaImage {
    let (w, h) = (64 * ATLAS_COLUMNS, 36 * PER_ATLAS / ATLAS_COLUMNS);
    RgbaImage {
        width: w,
        height: h,
        data: (0..w * h * 4).map(|i| (i % 251) as u8).collect(),
    }
}

/// Peaks of a steady half-scale tone, `seconds` long.
fn peaks(seconds: u32, channels: usize) -> Peaks {
    let total = u64::from(seconds) * 48_000;
    let levels = LEVELS
        .iter()
        .map(|&spp| PeakLevel {
            samples_per_peak: spp,
            peaks: vec![
                Peak {
                    min: -16000,
                    max: 16000,
                    rms: 11000,
                };
                total.div_ceil(u64::from(spp)) as usize * channels
            ],
        })
        .collect();
    Peaks {
        sample_rate: 48_000,
        channels,
        total_samples: total,
        levels,
    }
}

#[test]
fn thumbnail_tiles_show_source_time_through_the_speed() {
    // 100 points per second, tiles 50 points wide: half a second of timeline each.
    let all: Vec<_> =
        thumb_slots(0.0, 400.0, Rangef::new(0.0, 1000.0), 50.0, 10.0, 1.0, 100.0).collect();
    assert_eq!(all.len(), 8);
    assert_eq!(all[0].source_seconds, 10.0);
    assert_eq!(all[3].source_seconds, 11.5);
    // At 200% each tile covers a second of source; at 50% a quarter.
    let fast: Vec<_> =
        thumb_slots(0.0, 400.0, Rangef::new(0.0, 1000.0), 50.0, 10.0, 2.0, 100.0).collect();
    assert_eq!(fast[3].source_seconds, 13.0);
    let slow: Vec<_> =
        thumb_slots(0.0, 400.0, Rangef::new(0.0, 1000.0), 50.0, 10.0, 0.5, 100.0).collect();
    assert_eq!(slow[3].source_seconds, 10.75);
    // A partial last tile is still a tile.
    assert_eq!(
        thumb_slots(0.0, 410.0, Rangef::new(0.0, 1000.0), 50.0, 0.0, 1.0, 100.0).count(),
        9
    );
}

#[test]
fn only_visible_thumbnail_tiles_are_listed() {
    // A clip from x = -1000 to 1000, the screen from 0 to 300.
    let slots: Vec<_> = thumb_slots(
        -1000.0,
        2000.0,
        Rangef::new(0.0, 300.0),
        64.0,
        0.0,
        1.0,
        64.0,
    )
    .collect();
    let first = slots.first().expect("tiles");
    let last = slots.last().expect("tiles");
    assert!(-1000.0 + first.x <= 0.0 && -1000.0 + first.x + 64.0 > 0.0);
    assert!(-1000.0 + last.x < 300.0 && -1000.0 + last.x + 64.0 >= 300.0);
    assert_eq!(slots.len(), 6);
    // Tiles stay anchored on the clip's start: the same tile has the same time
    // however far the clip is scrolled.
    assert_eq!(first.source_seconds, f64::from(first.x) / 64.0);
    // A clip entirely off screen lists nothing.
    assert_eq!(
        thumb_slots(500.0, 100.0, Rangef::new(0.0, 300.0), 64.0, 0.0, 1.0, 64.0).count(),
        0
    );
}

#[test]
fn thumbnail_level_gives_each_tile_its_own_picture_with_the_fewest_atlases() {
    let s = strips(600);
    assert_eq!(thumb_level(&s.levels, 0.4), Some(0));
    assert_eq!(thumb_level(&s.levels, 1.0), Some(0));
    assert_eq!(thumb_level(&s.levels, 4.9), Some(0));
    assert_eq!(thumb_level(&s.levels, 5.0), Some(1));
    assert_eq!(thumb_level(&s.levels, 29.0), Some(1));
    assert_eq!(thumb_level(&s.levels, 300.0), Some(2));
    // Levels land coarse first: with only the 30 s level there, it is used.
    let coarse = vec![level(2, 30, 20)];
    assert_eq!(thumb_level(&coarse, 0.5), Some(0));
    assert_eq!(thumb_level(&[], 5.0), None);
    // The same tile at 200% covers twice the source, so it may use a coarser level.
    let tile_seconds = |speed: f64| 64.0 / 20.0 * speed;
    assert_eq!(thumb_level(&s.levels, tile_seconds(1.0)), Some(0));
    assert_eq!(thumb_level(&s.levels, tile_seconds(2.0)), Some(1));
}

#[test]
fn waveform_level_follows_zoom_and_speed() {
    let p = peaks(10, 2);
    // 48 kHz at 1000 points per second: 48 samples a point, below the finest level.
    assert_eq!(samples_per_point(48_000, 1.0, 1000.0), 48.0);
    assert_eq!(waveform_level(&p, 1.0, 1000.0), 0);
    // 48000 / 40 = 1200 samples a point: the 1024 level.
    assert_eq!(waveform_level(&p, 1.0, 40.0), 1);
    // The same zoom at 400% reads four times the samples: the 4096 level.
    assert_eq!(waveform_level(&p, 4.0, 40.0), 2);
    // An hour across 1600 points: 108000 samples a point, the coarsest level.
    assert_eq!(waveform_level(&p, 1.0, 1600.0 / 3600.0), 4);
    // Never more than four peaks a column: each level's peak is at most a column.
    for pps in [0.1, 1.0, 7.0, 33.0, 180.0, 900.0] {
        for speed in [0.25, 1.0, 3.0] {
            let spp = samples_per_point(48_000, speed, pps);
            let l = waveform_level(&p, speed, pps);
            let peak = f64::from(p.levels[l].samples_per_peak);
            assert!(peak <= spp || l == 0, "{pps} {speed}");
            if l + 1 < p.levels.len() {
                assert!(f64::from(p.levels[l + 1].samples_per_peak) > spp);
            }
        }
    }
}

#[test]
fn speed_badge_text() {
    let map = |n, d| project::SpeedMap::constant(Rational::new(n, d)).expect("speed");
    assert_eq!(speed_text(&map(1, 1)), None);
    assert_eq!(speed_text(&map(2, 1)).as_deref(), Some("200%"));
    assert_eq!(speed_text(&map(1, 2)).as_deref(), Some("50%"));
    assert_eq!(speed_text(&map(1, 3)).as_deref(), Some("33.3%"));
    assert_eq!(clip_speed(&map(3, 2)), 1.5);
}

/// `tracks` video and audio tracks of `per_track` clips of `seconds` each, every
/// fifth one at 200%, over `media` sources alternating.
fn sequence(tracks: usize, per_track: i64, seconds: i64) -> (Project, Sequence, Vec<MediaId>) {
    let p = Project::new("uid", "Clips");
    let format = SequenceFormat {
        frame_rate: RATE,
        ..SequenceFormat::default()
    };
    let (mut p, seq) = p.add_sequence_with("Main", format).expect("sequence");
    let mut media = Vec::new();
    for i in 0..4 {
        let (next, m) = p
            .add_media(format!("h{i}"), format!("clip{i}.mov"))
            .expect("m");
        p = next;
        media.push(m);
    }
    for t in 0..tracks {
        let kind = if t % 2 == 0 {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        let (next, track) = p.add_track(seq, kind, format!("T{t}"), None).expect("t");
        p = next;
        for i in 0..per_track {
            let a = Time::from_frames(i * seconds * 25, RATE).unwrap();
            let range = TimeRange::new(a, Time::seconds(seconds - 1));
            let spec = ClipSpec {
                media: media[(i as usize + t) % 4],
                range,
                source_start: Time::seconds(5),
            };
            let (next, item) = p.insert_clip(track, spec).expect("clip");
            p = next;
            if i % 5 == 0 {
                p = p
                    .set_clip_speed(item, Rational::new(2, 1), SpeedKeep::Timeline)
                    .expect("speed");
            }
        }
    }
    let s = p.sequence(seq).expect("seq").as_ref().clone();
    (p, s, media)
}

fn loaded(ctx: &egui::Context, media: &[MediaId]) -> ClipMedia {
    let mut store = ClipMedia::new();
    let image = atlas();
    for &m in media {
        let s = Arc::new(strips(600));
        store.insert_strips(m, s.clone());
        for (l, lvl) in s.levels.iter().enumerate() {
            for c in 0..lvl.atlases.len() {
                let key = AtlasKey {
                    level: l as u32,
                    chunk: c as u32,
                };
                store.insert_atlas(ctx, m, key, &image);
            }
        }
        store.insert_peaks(m, Arc::new(peaks(1300, 2)));
    }
    store
}

/// Runs one headless frame drawing the clips with `pos`; returns the stats.
fn frame(
    headless: &mut HeadlessUi,
    p: &Project,
    s: &Sequence,
    store: &mut ClipMedia,
    pos: ViewPos,
) -> ClipStats {
    let layout = TrackLayout::new(s.tracks.iter().map(|t| (t.id, 48.0)));
    let mut stats = ClipStats::default();
    headless
        .frame(|ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let rect = ui.max_rect();
                let mut v = TimelineView::for_sequence(s);
                v.set_extent(Extent {
                    width: rect.width(),
                    height: rect.height(),
                    duration: view::sequence_duration(s),
                    content_height: layout.content_height(),
                });
                v.set_pos(pos);
                let cv = ClipsView::new(p, s);
                stats = clips::clips(ui, rect, &v, &layout, &cv, store, &());
            });
        })
        .expect("frame");
    stats
}

#[test]
fn missing_analysis_is_requested_once_and_drawn_as_placeholders() {
    let Some(mut headless) = HeadlessUi::new_or_skip("timeline clips", Viewport::default()) else {
        return;
    };
    theme::builtin(ThemeKind::Dark).apply(headless.context());
    let (p, s, media) = sequence(2, 40, 10);
    let mut store = ClipMedia::new();
    let pos = ViewPos {
        start: 0.0,
        frame_px: 1.0,
    };
    let stats = frame(&mut headless, &p, &s, &mut store, pos);
    assert!(stats.detailed > 0 && stats.placeholders > 0, "{stats:?}");
    let first = store.take_requests();
    assert!(first.iter().all(|r| match r {
        MediaRequest::Thumbnails(m) | MediaRequest::Peaks(m) => media.contains(m),
        MediaRequest::Atlas { .. } => false,
    }));
    let unique: std::collections::HashSet<_> = first.iter().collect();
    assert_eq!(unique.len(), first.len(), "duplicate requests");
    // Drawing again asks for nothing new.
    let _ = frame(&mut headless, &p, &s, &mut store, pos);
    assert!(store.take_requests().is_empty());
    // With the strips in, only the atlases of visible tiles are asked for.
    for &m in &media {
        store.insert_strips(m, Arc::new(strips(3600)));
    }
    let _ = frame(&mut headless, &p, &s, &mut store, pos);
    let atlases = store.take_requests();
    assert!(!atlases.is_empty());
    for r in &atlases {
        match r {
            // A second per point at most 1600 points: tiles cover far less than 64
            // thumbnails of any level, so only the first chunks are needed.
            MediaRequest::Atlas { atlas, .. } => assert!(atlas.chunk <= 1, "{r:?}"),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn off_screen_clips_are_culled_and_zoomed_out_clips_merge_into_blocks() {
    let Some(mut headless) = HeadlessUi::new_or_skip("timeline clips", Viewport::default()) else {
        return;
    };
    theme::builtin(ThemeKind::Dark).apply(headless.context());
    let (p, s, media) = sequence(4, 125, 10);
    let mut store = loaded(headless.context(), &media);
    // 40 points a second from 100 s: about 40 s of 1250 s on screen.
    let close = frame(
        &mut headless,
        &p,
        &s,
        &mut store,
        ViewPos {
            start: 2500.0,
            frame_px: 1.6,
        },
    );
    assert!(close.items <= 4 * 6, "{close:?}");
    assert_eq!(close.items, close.detailed);
    assert!(close.thumbnails > 0 && close.wave_columns > 0, "{close:?}");
    assert_eq!(close.placeholders, 0);
    // The whole edit on screen (zoom to fit, about a point a second): 500 clips, each
    // a few points wide, are plain blocks.
    let fit = frame(
        &mut headless,
        &p,
        &s,
        &mut store,
        ViewPos {
            start: 0.0,
            frame_px: 0.0,
        },
    );
    assert_eq!(fit.items, 500, "{fit:?}");
    assert_eq!(fit.detailed, 0);
    assert!(fit.blocks <= 500, "{fit:?}");
    // Four-second clips a second apart at the fit zoom: the gaps are under a point, so
    // each track merges into one block.
    let (p, s, media) = sequence(4, 400, 4);
    let mut store = loaded(headless.context(), &media);
    let fit = frame(
        &mut headless,
        &p,
        &s,
        &mut store,
        ViewPos {
            start: 0.0,
            frame_px: 0.0,
        },
    );
    assert_eq!(fit.items, 1600, "{fit:?}");
    assert_eq!(fit.detailed, 0);
    assert_eq!(fit.blocks, 4, "{fit:?}");
}

#[test]
fn five_hundred_clips_scroll_and_zoom_within_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("timeline clips", Viewport::default()) else {
        return;
    };
    theme::builtin(ThemeKind::Dark).apply(headless.context());
    let (p, s, media) = sequence(8, 63, 8);
    let count: usize = s.tracks.iter().map(|t| t.items.len()).sum();
    assert!(count >= 500);
    let mut store = loaded(headless.context(), &media);
    let layout = TrackLayout::new(s.tracks.iter().map(|t| (t.id, 48.0)));
    let mut v = TimelineView::for_sequence(&s);
    let duration = view::sequence_duration(&s);
    let report = FrameBudget::run(&mut headless, 30, 240, |ui, frame| {
        egui::CentralPanel::default().show(ui, |ui| {
            let full = ui.max_rect();
            let tracks = Rect::from_min_max(pos2(full.left() + 160.0, full.top()), full.max);
            v.set_extent(Extent {
                width: tracks.width(),
                height: tracks.height(),
                duration,
                content_height: layout.content_height(),
            });
            // Zoom in and out about a moving point, then pan, as a user would.
            match frame % 60 {
                0..20 => v.zoom_at(1.25, (frame % 37) as f32 * 20.0),
                20..40 => v.zoom_at(1.0 / 1.25, (frame % 23) as f32 * 30.0),
                _ => v.pan(-40.0, if frame % 2 == 0 { -8.0 } else { 6.0 }),
            }
            view::lanes(ui, tracks, &v, &layout);
            let cv = ClipsView::new(&p, &s);
            let _ = clips::clips(ui, tracks, &v, &layout, &cv, &mut store, &());
        });
    })
    .expect("headless frames");
    eprintln!("timeline clips ({}): {report}", headless.adapter());
    report.assert_within_budget(&headless, "timeline clips");
}

#[test]
fn analysis_feed_turns_requests_into_jobs_and_results_into_the_store() {
    use engine::analysis::{AnalysisPool, PoolConfig};
    use engine::media::analysis::peaks::PeakSettings;
    use engine::media::analysis::thumbnails::ThumbnailSettings;
    use engine::media::{CacheDir, SourceFile};
    use ui::panels::timeline::clip_media::AnalysisFeed;

    let fixtures =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    let (video, audio) = (
        fixtures.join("h264_fullrange_640x360.mp4"),
        fixtures.join("audio_48k_stereo.wav"),
    );
    if !video.exists() || !audio.exists() {
        eprintln!("skipped: fixtures missing; run tests/fixtures/generate.sh");
        return;
    }
    let dir = std::env::temp_dir().join(format!("glass-clip-feed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (pool, _events) = AnalysisPool::new(PoolConfig::new(CacheDir::new(&dir))).expect("pool");
    let (v, a) = (MediaId(1), MediaId(2));
    let sources = [
        (v, SourceFile::open(&video).expect("hash")),
        (a, SourceFile::open(&audio).expect("hash")),
    ];
    let source = |m: MediaId| {
        sources
            .iter()
            .find(|(id, _)| *id == m)
            .map(|(_, s)| s.clone())
    };
    let mut store = ClipMedia::new();
    let mut feed = AnalysisFeed::new(ThumbnailSettings::default(), PeakSettings::default());
    assert!(store.strips(v).is_none());
    assert!(store.peaks(a).is_none());
    // A source nobody knows is dropped, not retried.
    assert!(store.peaks(MediaId(9)).is_none());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut loads = Vec::new();
    loop {
        loads.extend(feed.pump(&pool, &mut store, source));
        let done = store.strips(v).is_some() && store.peaks(a).is_some();
        if done || std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(store.strips(v).is_some(), "thumbnails did not arrive");
    let peaks = store.peaks(a).expect("peaks did not arrive");
    assert_eq!(peaks.channels, 2);
    assert_eq!(feed.in_flight(), 0);
    // An atlas request comes back out as a load naming the file in the strips.
    let key = AtlasKey { level: 0, chunk: 0 };
    assert!(store.atlas(v, key).is_none());
    loads.extend(feed.pump(&pool, &mut store, source));
    let load = loads.iter().find(|l| l.media == v).expect("atlas load");
    assert_eq!(load.atlas, key);
    assert_eq!(load.file, load.strips.levels[0].atlases[0]);
    drop(pool);
    let _ = std::fs::remove_dir_all(&dir);
}
