//! Snapshot cases for `panels::media_pool`: the docked grid with its bins tree, the
//! widened list while searching, and the colour label menu, filled from a real
//! `project::media_pool::MediaPool` so the rows go through the panel's own conversion.

use std::collections::BTreeSet;

use egui::{Rect, vec2};
use project::media_pool::{ColourLabel, Facts, MediaItem, MediaKind, MediaPool, Query, SortBy};
use project::{BinId, MediaId};

use crate::panels::media_pool::{
    self, BinRow, BinSelection, ItemRow, MediaPoolView, PoolView, ProxyState, SmartBinRow,
    SmartIcon,
};
use crate::t;
use crate::theme::{self, Role};

/// Sizes of the three cases.
pub(super) const GRID_SIZE: egui::Vec2 = vec2(400.0, 580.0);
pub(super) const LIST_SIZE: egui::Vec2 = vec2(888.0, 410.0);
pub(super) const MENU_SIZE: egui::Vec2 = vec2(232.0, 240.0);

struct Clip {
    name: String,
    seconds: f64,
    width: u32,
    fps: (i32, i32),
    codec: &'static str,
    profile: Option<&'static str>,
    bit_depth: u8,
    camera: String,
    focal: f32,
    tags: Vec<String>,
    label: Option<ColourLabel>,
    offline: bool,
}

/// The `Selects` bin of the harbour film, with label names, and the clips whose files
/// are offline (engine state, beside the pool).
fn sample_pool() -> (MediaPool, Vec<MediaId>, BTreeSet<MediaId>) {
    let mut pool = MediaPool::new(t!("sample.pool.project"));
    for (label, name) in [
        (ColourLabel::Amber, t!("sample.pool.label.selects")),
        (ColourLabel::Blue, t!("sample.pool.label.interviews")),
        (ColourLabel::Green, t!("sample.pool.label.approved")),
        (ColourLabel::Red, t!("sample.pool.label.fix")),
        (ColourLabel::Violet, t!("sample.pool.label.music")),
    ] {
        pool.set_label_name(label, &name);
    }
    let bin = pool
        .add_bin(pool.root(), t!("sample.bin.selects"))
        .expect("root bin");
    let fx6 = t!("sample.pool.camera.fx6");
    let clips = [
        Clip {
            name: t!("sample.pool.clip.dusk"),
            seconds: 12.2,
            width: 3840,
            fps: (24000, 1001),
            codec: "prores",
            profile: Some("HQ"),
            bit_depth: 10,
            camera: fx6.clone(),
            focal: 35.0,
            tags: vec![t!("sample.pool.tag.golden"), t!("sample.pool.tag.wide")],
            label: Some(ColourLabel::Amber),
            offline: false,
        },
        Clip {
            name: t!("sample.pool.clip.wide"),
            seconds: 31.4,
            width: 3840,
            fps: (24000, 1001),
            codec: "prores",
            profile: Some("HQ"),
            bit_depth: 10,
            camera: fx6.clone(),
            focal: 24.0,
            tags: vec![t!("sample.pool.tag.wide")],
            label: Some(ColourLabel::Amber),
            offline: false,
        },
        Clip {
            name: t!("sample.pool.clip.harbourmaster"),
            seconds: 18.0 * 60.0 + 42.25,
            width: 3840,
            fps: (24000, 1001),
            codec: "hevc",
            profile: None,
            bit_depth: 10,
            camera: t!("sample.pool.camera.fx3"),
            focal: 50.0,
            tags: vec![t!("sample.pool.tag.interview")],
            label: Some(ColourLabel::Blue),
            offline: false,
        },
        Clip {
            name: t!("sample.pool.clip.water"),
            seconds: 9.5,
            width: 3840,
            fps: (24000, 1001),
            codec: "prores",
            profile: Some("HQ"),
            bit_depth: 10,
            camera: fx6.clone(),
            focal: 85.0,
            tags: vec![t!("sample.pool.tag.detail")],
            label: None,
            offline: false,
        },
        Clip {
            name: t!("sample.pool.clip.gulls"),
            seconds: 8.03,
            width: 3840,
            fps: (60000, 1001),
            codec: "h264",
            profile: None,
            bit_depth: 8,
            camera: t!("sample.pool.camera.drone"),
            focal: 24.0,
            tags: vec![t!("sample.pool.tag.slow")],
            label: None,
            offline: false,
        },
        Clip {
            name: t!("sample.pool.clip.market"),
            seconds: 24.0,
            width: 1920,
            fps: (25, 1),
            codec: "h264",
            profile: None,
            bit_depth: 8,
            camera: t!("sample.pool.camera.fx3"),
            focal: 24.0,
            tags: vec![t!("sample.pool.tag.night")],
            label: None,
            offline: false,
        },
        Clip {
            name: t!("sample.pool.clip.forest"),
            seconds: 16.0,
            width: 3840,
            fps: (24000, 1001),
            codec: "prores",
            profile: Some("HQ"),
            bit_depth: 10,
            camera: fx6.clone(),
            focal: 35.0,
            tags: vec![t!("sample.pool.tag.wide")],
            label: Some(ColourLabel::Green),
            offline: false,
        },
        Clip {
            name: t!("sample.pool.clip.rope"),
            seconds: 6.75,
            width: 3840,
            fps: (24000, 1001),
            codec: "prores",
            profile: None,
            bit_depth: 10,
            camera: fx6,
            focal: 85.0,
            tags: vec![t!("sample.pool.tag.broll")],
            label: None,
            offline: true,
        },
    ];
    let mut offline = BTreeSet::new();
    let ids = clips
        .into_iter()
        .map(|c| {
            let mut item =
                MediaItem::new(&c.name, MediaKind::Video, format!("/card/{}.mov", c.name));
            item.facts = Facts {
                duration_s: Some(c.seconds),
                width: c.width,
                height: c.width * 9 / 16,
                frame_rate: Some(c.fps),
                video_codec: Some(c.codec.into()),
                video_profile: c.profile.map(str::to_string),
                bit_depth: c.bit_depth,
                ..Facts::default()
            };
            item.metadata.camera_model = Some(c.camera);
            item.metadata.focal_length_mm = Some(c.focal);
            item.tags = c.tags;
            item.size_bytes = (c.seconds * 22_000_000.0) as u64;
            item.metadata.reel = Some(t!("sample.pool.reel"));
            item.metadata.timecode = Some(t!("sample.timecode"));
            item.facts.audio_channels = 2;
            item.facts.sample_rate = 48_000;
            item.label = c.label;
            let id = pool.add_item(bin, item).expect("bin exists");
            if c.offline {
                offline.insert(id);
            }
            id
        })
        .collect();
    (pool, ids, offline)
}

/// Rows for `ids` with the engine's side: availability, proxies, analysis.
fn rows(pool: &MediaPool, ids: &[MediaId], offline: &BTreeSet<MediaId>) -> Vec<ItemRow> {
    ids.iter()
        .filter_map(|id| pool.item(*id))
        .enumerate()
        .map(|(i, item)| {
            let mut row = ItemRow::from_item(item, offline.contains(&item.id));
            match i {
                0 => row.proxy = ProxyState::Ready,
                3 => row.proxy = ProxyState::Making(0.31),
                4 => row.analysing = Some(0.64),
                _ => {}
            }
            row
        })
        .collect()
}

fn bins() -> Vec<BinRow> {
    let row = |id: u64, name: String, depth: usize, count: usize| BinRow {
        id: BinId(id),
        name,
        depth,
        count,
        has_children: depth == 0,
        expanded: depth == 0,
    };
    vec![
        row(0, t!("sample.pool.project"), 0, 0),
        row(2, t!("sample.bin.interviews"), 1, 24),
        row(3, t!("sample.pool.bin.broll"), 1, 61),
        row(1, t!("sample.bin.selects"), 1, 8),
        row(4, t!("sample.pool.bin.music"), 1, 7),
        row(5, t!("sample.pool.bin.titles"), 1, 4),
    ]
}

fn smart_bins() -> Vec<SmartBinRow> {
    vec![
        SmartBinRow {
            name: t!("pool.smart.offline"),
            count: 2,
            icon: SmartIcon::Offline,
            alert: true,
        },
        SmartBinRow {
            name: t!("pool.smart.added_today"),
            count: 12,
            icon: SmartIcon::Recent,
            alert: false,
        },
        SmartBinRow {
            name: t!("pool.smart.no_proxy"),
            count: 18,
            icon: SmartIcon::Film,
            alert: false,
        },
    ]
}

fn panel(ui: &mut egui::Ui, draw: impl FnOnce(&mut egui::Ui, Rect)) {
    let fill = theme::current(ui.ctx()).color(Role::Bg1);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(fill))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            draw(ui, rect);
        });
}

/// The docked panel at real size: bins with `Selects` open, eight clips in every state.
pub(super) fn grid(ui: &mut egui::Ui) {
    panel(ui, |ui, rect| {
        let mut view = grid_view();
        let _ = media_pool::media_pool(ui, rect, &mut view);
    });
}

/// The grid's view: `Selects` open with the first clip selected.
pub(super) fn grid_view() -> MediaPoolView {
    let (pool, ids, offline) = sample_pool();
    let items = rows(&pool, &ids, &offline);
    MediaPoolView {
        view: PoolView::Grid,
        bins: bins(),
        smart_bins: smart_bins(),
        bin: BinSelection::Bin(BinId(1)),
        bin_name: t!("sample.bin.selects"),
        bin_duration_s: 108.0,
        sort: SortBy::Added,
        total: items.len(),
        selected: BTreeSet::from([ids[0]]),
        items,
        ..MediaPoolView::default()
    }
}

/// The same panel widened into the list, searching `harbour` with two filters.
pub(super) fn list(ui: &mut egui::Ui) {
    panel(ui, |ui, rect| {
        let (pool, ids, offline) = sample_pool();
        let order = [0, 1, 2, 4, 7, 3];
        let items: Vec<ItemRow> = {
            let all = rows(&pool, &ids, &offline);
            order.iter().map(|i| all[*i].clone()).collect()
        };
        let query = Query {
            kind: Some(MediaKind::Video),
            label: Some(ColourLabel::Amber),
            ..Query::default()
        };
        let mut view = MediaPoolView {
            view: PoolView::List,
            search: t!("sample.pool.search"),
            filters: media_pool::filter_chips(&query, &pool),
            bins: bins(),
            bin: BinSelection::Bin(BinId(1)),
            bin_name: t!("sample.bin.selects"),
            total: 94,
            selected: BTreeSet::from([ids[0]]),
            metadata: pool
                .item(ids[0])
                .map(|item| media_pool::metadata_view(item)),
            items,
            ..MediaPoolView::default()
        };
        let _ = media_pool::media_pool(ui, rect, &mut view);
    });
}

/// The colour label menu with `Amber · Selects` current.
pub(super) fn label_menu(ui: &mut egui::Ui) {
    panel(ui, |ui, rect| {
        let (pool, _, _) = sample_pool();
        let _ = media_pool::label_menu(
            ui,
            rect.min + vec2(16.0, 16.0),
            Some(ColourLabel::Amber),
            &pool,
        );
    });
}
