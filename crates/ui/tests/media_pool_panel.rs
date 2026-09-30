//! The media pool panel from `Sheet / Media pool`, drawn headless over a 3,000-clip pool:
//! the docked grid and the widened list side by side, scrolling every frame and searching
//! the pool through `SearchIndex` as the text changes, within the frame budget.

use std::collections::BTreeSet;

use egui::{Rect, pos2, vec2};
use project::media_pool::{
    ColourLabel, Facts, MediaItem, MediaKind, MediaPool, MediaStatus, OfflineReason, Query,
    SearchIndex,
};
use ui::frame_time::FrameTimeMonitor;
use ui::headless::{HeadlessUi, Viewport};
use ui::panels::media_pool::{
    BinSelection, ItemRow, MediaPoolView, PoolView, ProxyState, bin_rows, filter_chips, media_pool,
    metadata_view,
};
use ui::theme::ThemeKind;

const CLIPS: usize = 3_000;

fn shoot() -> MediaPool {
    let mut pool = MediaPool::new("Harbour film");
    let root = pool.root();
    let bins: Vec<_> = (1..=10)
        .flat_map(|day| ["A-cam", "B-cam"].map(|cam| vec![format!("Day {day}"), cam.to_string()]))
        .map(|path| pool.bin_path(root, &path).unwrap())
        .collect();
    let subjects = ["Harbour", "Gulls", "Rope", "Water", "Forest", "Market"];
    let tags = ["Golden hour", "Wide", "Interview", "Slow motion", "B-roll"];
    for i in 0..CLIPS {
        let name = format!("{} take {i}", subjects[i % subjects.len()]);
        let mut item = MediaItem::new(&name, MediaKind::Video, format!("/Volumes/Shoot/{i}.mov"));
        item.facts = Facts {
            duration_s: Some((i % 900) as f64 / 7.0),
            width: if i.is_multiple_of(4) { 1920 } else { 3840 },
            height: if i.is_multiple_of(4) { 1080 } else { 2160 },
            frame_rate: Some((24000, 1001)),
            video_codec: Some(
                if i.is_multiple_of(3) {
                    "hevc"
                } else {
                    "prores"
                }
                .into(),
            ),
            video_profile: Some("HQ".into()),
            bit_depth: 10,
            ..Facts::default()
        };
        item.metadata.camera_model = Some("ILME-FX6V".into());
        item.metadata.focal_length_mm = Some(35.0);
        item.tags = vec![
            tags[i % tags.len()].into(),
            tags[(i / 5) % tags.len()].into(),
        ];
        item.label = i.is_multiple_of(6).then_some(ColourLabel::ALL[i % 5]);
        if i.is_multiple_of(97) {
            item.status = MediaStatus::Offline {
                reason: OfflineReason::VolumeNotConnected,
            };
        }
        pool.add_item(bins[i % bins.len()], item).unwrap();
    }
    pool
}

fn rows(pool: &MediaPool, ids: &[project::MediaId]) -> Vec<ItemRow> {
    ids.iter()
        .filter_map(|id| pool.item(*id))
        .map(|item| {
            let mut row = ItemRow::from_item(item);
            let n = item.id.0 as usize;
            row.proxy = match n % 3 {
                0 => ProxyState::Ready,
                1 => ProxyState::Making((n % 100) as f32 / 100.0),
                _ => ProxyState::None,
            };
            row.analysing = n.is_multiple_of(11).then_some(0.4);
            row
        })
        .collect()
}

#[test]
fn view_rows_follow_the_search() {
    let pool = shoot();
    let index = SearchIndex::build(&pool);
    let all = index.query(&pool, &Query::default());
    assert_eq!(all.len(), CLIPS);
    let query = Query::parse("harbour type:video", &pool);
    let found = index.query(&pool, &query);
    assert_eq!(found.len(), CLIPS / 6);
    let chips = filter_chips(&query, &pool);
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].text, "Type · Video");
    let tree = bin_rows(&pool, &BTreeSet::new());
    assert_eq!(tree.len(), 1 + 10);
}

#[test]
fn media_pool_draws_thousands_of_clips_within_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("media pool panel", Viewport::default())
    else {
        return;
    };
    ui::theme::builtin(ThemeKind::Dark).apply(headless.context());
    let pool = shoot();
    let mut index = SearchIndex::build(&pool);
    let ids = index.query(&pool, &Query::default());
    let items = rows(&pool, &ids);
    let expanded: BTreeSet<_> = pool.children(pool.root()).iter().map(|b| b.id).collect();
    let base = MediaPoolView {
        bins: bin_rows(&pool, &expanded),
        bin: BinSelection::Bin(pool.root()),
        bin_name: "Harbour film".into(),
        bin_duration_s: 3600.0,
        total: items.len(),
        selected: ids.iter().step_by(40).copied().collect(),
        items,
        ..MediaPoolView::default()
    };
    let mut grid = MediaPoolView {
        view: PoolView::Grid,
        ..base.clone()
    };
    let mut list = MediaPoolView {
        view: PoolView::List,
        metadata: pool.item(ids[0]).map(|item| metadata_view(item)),
        ..base
    };
    // The search results for what the user types, queried through the index up front:
    // the query itself runs once per keystroke, and its cost (well under a millisecond
    // for 10,000 items in release) is measured by the `project` bench `media_pool`. Here
    // the panel shows each result set for 25 frames while both views scroll.
    let searches = ["", "harb", "gulls wide", "intervew", "", "label:amber"];
    index.refresh(&pool);
    let mut results: Vec<_> = searches
        .iter()
        .map(|text| {
            let query = Query::parse(text, &pool);
            let found = index.query(&pool, &query);
            (
                text.to_string(),
                filter_chips(&query, &pool),
                rows(&pool, &found),
            )
        })
        .collect();
    assert!(results.iter().all(|(_, _, rows)| !rows.is_empty()));
    let mut shown = usize::MAX;
    let mut monitor = FrameTimeMonitor::default();
    headless
        .run(30, 300, &mut monitor, |ui, frame| {
            let next = (frame / 25) % results.len();
            if next != shown {
                if shown != usize::MAX {
                    std::mem::swap(&mut list.items, &mut results[shown].2);
                }
                let (text, chips, rows) = &mut results[next];
                std::mem::swap(&mut list.items, rows);
                list.search.clone_from(text);
                list.filters.clone_from(chips);
                shown = next;
            }
            grid.scroll = (frame as f32 * 37.0) % 40_000.0;
            list.scroll = (frame as f32 * 53.0) % 20_000.0;
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let full = ui.max_rect();
                    let left = Rect::from_min_size(full.min, vec2(400.0, full.height()));
                    let right = Rect::from_min_max(pos2(left.max.x + 8.0, full.min.y), full.max);
                    media_pool(ui, left, &mut grid);
                    media_pool(ui, right, &mut list);
                });
        })
        .expect("headless frames");
    let stats = monitor.stats();
    eprintln!("media pool panel ({}): {stats}", headless.adapter());
    headless.assert_p99_within_budget("media pool panel", &stats);
}
