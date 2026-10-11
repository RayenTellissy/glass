//! The media pool panel from `Sheet / Media pool` (page `02 Edit workspace`): bins on the
//! left, clips on the right, as a thumbnail grid for looking or a list with metadata
//! columns for sorting. Every clip says what state it is in with a word or a badge, not
//! just a colour: proxy, offline, analysing, labelled, tagged.
//!
//! The panel draws a [`MediaPoolView`] the app fills from `project::media_pool` (bins,
//! labels, tags, the search results from `SearchIndex`) and the engine (thumbnails,
//! proxy and analysis progress), and returns what the user did as a [`MediaPoolAction`].
//! Only the rows and tiles on screen are laid out, so a bin of thousands of clips costs
//! the same per frame as a bin of twenty.

use std::collections::BTreeSet;

use egui::{
    Align2, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, TextureId, Ui, pos2, vec2,
};
use project::media_pool::{ColourLabel, Facts, MediaItem, MediaKind, MediaPool, Query, SortBy};
use project::{BinId, MediaId};

use super::format;
use super::import::{self, DropArea, DropAreaAction};
use super::metadata::{METADATA_PANEL_HEIGHT, MetadataAction, MetadataView, metadata_panel};
use super::widgets::{Icon, Paint, Type, icon, icon_button, size};
use crate::t;
use crate::theme::Role;

/// Board sizes with no token of their own.
pub mod layout {
    /// The grid view's toolbar and the list view's (which holds the filter chips).
    pub const GRID_TOOLBAR: f32 = 36.0;
    pub const LIST_TOOLBAR: f32 = 40.0;
    pub const COLUMN_HEADER: f32 = 24.0;
    pub const BINS_WIDTH: f32 = 148.0;
    /// Bin tree section headers (`BINS`, `SMART BINS`).
    pub const BIN_SECTION: f32 = 24.0;
    /// Grid tiles: at least this wide, 8 apart, rows 12 apart; the name and meta lines
    /// under a 16:9 thumbnail.
    pub const TILE_MIN_WIDTH: f32 = 104.0;
    pub const TILE_GAP: f32 = 8.0;
    pub const TILE_ROW_GAP: f32 = 12.0;
    pub const TILE_TEXT: f32 = 4.0 + 16.0 + 4.0 + 14.0;
    /// The list view's search field.
    pub const LIST_SEARCH_WIDTH: f32 = 320.0;
    /// Fixed list columns after the flexible name column, in order.
    pub const LIST_COLUMNS: [f32; 7] = [64.0, 80.0, 48.0, 80.0, 96.0, 136.0, 112.0];
    /// Narrowest name column; below it the list drops the bins column.
    pub const LIST_NAME_MIN: f32 = 160.0;
}

/// Grid or list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PoolView {
    #[default]
    Grid,
    List,
}

/// The browser tabs in the panel header; the pool is `Media`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserTab {
    Media,
    Transitions,
    Titles,
    Effects,
    Audio,
}

impl BrowserTab {
    pub const ALL: [BrowserTab; 5] = [
        BrowserTab::Media,
        BrowserTab::Transitions,
        BrowserTab::Titles,
        BrowserTab::Effects,
        BrowserTab::Audio,
    ];

    fn label(self) -> String {
        match self {
            BrowserTab::Media => t!("pool.tab.media"),
            BrowserTab::Transitions => t!("pool.tab.transitions"),
            BrowserTab::Titles => t!("pool.tab.titles"),
            BrowserTab::Effects => t!("pool.tab.effects"),
            BrowserTab::Audio => t!("pool.tab.audio"),
        }
    }
}

/// One row of the bins tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinRow {
    pub id: BinId,
    pub name: String,
    /// 0 for the root (the project).
    pub depth: usize,
    /// Items directly in the bin.
    pub count: usize,
    pub has_children: bool,
    pub expanded: bool,
}

/// The icon a smart bin shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmartIcon {
    Offline,
    Recent,
    Film,
    Filter,
}

/// A saved search shown under the bins (`Offline`, `Added today`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmartBinRow {
    pub name: String,
    pub count: usize,
    pub icon: SmartIcon,
    /// The count reads as a problem (offline media).
    pub alert: bool,
}

/// Which bin the clips come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinSelection {
    Bin(BinId),
    Smart(usize),
}

/// Proxy state from the proxy jobs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ProxyState {
    #[default]
    None,
    /// A proxy is being made; 0 to 1.
    Making(f32),
    Ready,
}

/// One clip as the grid and list show it.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemRow {
    pub id: MediaId,
    pub name: String,
    pub kind: MediaKind,
    pub label: Option<ColourLabel>,
    pub duration_s: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    /// `ProRes 422 HQ`, `HEVC 10-bit`, `H.264` ([`codec_label`]).
    pub codec: String,
    /// `ILME-FX6V · 35 mm` ([`camera_label`]).
    pub camera: String,
    pub tags: Vec<String>,
    pub offline: bool,
    pub proxy: ProxyState,
    /// Analysis running (scene cuts, waveforms...); 0 to 1.
    pub analysing: Option<f32>,
    /// The poster frame from the thumbnail cache, once loaded.
    pub thumbnail: Option<TextureId>,
}

impl ItemRow {
    /// A row from a pool item; whether its file is offline, proxy, analysis and
    /// thumbnail come from the engine.
    pub fn from_item(item: &MediaItem, offline: bool) -> Self {
        let f = &item.facts;
        Self {
            id: item.id,
            name: item.name.clone(),
            kind: item.kind,
            label: item.label,
            duration_s: f.duration_s,
            width: f.width,
            height: f.height,
            fps: f.fps(),
            codec: codec_label(f),
            camera: camera_label(item),
            tags: item.tags.clone(),
            offline,
            proxy: ProxyState::None,
            analysing: None,
            thumbnail: None,
        }
    }
}

/// A filter chip in the list toolbar (`Type · Video`, `Label · Amber`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterChip {
    pub text: String,
    /// A colour label's dot.
    pub dot: Option<ColourLabel>,
}

/// The chips for a query's filters, in toolbar order: type, label, tags, offline.
pub fn filter_chips(query: &Query, pool: &MediaPool) -> Vec<FilterChip> {
    let mut chips = Vec::new();
    if let Some(kind) = query.kind {
        chips.push(FilterChip {
            text: t!("pool.filter.type", kind = kind_text(kind)),
            dot: None,
        });
    }
    if let Some(label) = query.label {
        let name = pool
            .label_name(label)
            .map(str::to_string)
            .unwrap_or_else(|| label_text(label));
        chips.push(FilterChip {
            text: t!("pool.filter.label", label = name),
            dot: Some(label),
        });
    }
    for tag in &query.tags {
        chips.push(FilterChip {
            text: t!("pool.filter.tag", tag = tag),
            dot: None,
        });
    }
    match query.offline {
        Some(true) => chips.push(FilterChip {
            text: t!("pool.filter.offline"),
            dot: None,
        }),
        Some(false) => chips.push(FilterChip {
            text: t!("pool.filter.online"),
            dot: None,
        }),
        None => {}
    }
    chips
}

/// Everything the panel shows, plus the state it keeps between frames (search text,
/// scroll, selection).
#[derive(Clone, Debug, PartialEq)]
pub struct MediaPoolView {
    pub view: PoolView,
    pub search: String,
    pub filters: Vec<FilterChip>,
    pub bins: Vec<BinRow>,
    pub smart_bins: Vec<SmartBinRow>,
    pub bin: BinSelection,
    /// The shown bin's name and total length (grid header).
    pub bin_name: String,
    pub bin_duration_s: f64,
    pub sort: SortBy,
    pub descending: bool,
    /// The clips to show, already searched, filtered and sorted.
    pub items: Vec<ItemRow>,
    /// Clips in the bin before searching (`6 of 94 clips`).
    pub total: usize,
    pub selected: BTreeSet<MediaId>,
    /// Pixels scrolled in the clip area and in the bins tree.
    pub scroll: f32,
    pub bins_scroll: f32,
    /// The selected clip's metadata, shown under the list ([`metadata_view`]).
    pub metadata: Option<MetadataView>,
}

impl Default for MediaPoolView {
    fn default() -> Self {
        Self {
            view: PoolView::Grid,
            search: String::new(),
            filters: Vec::new(),
            bins: Vec::new(),
            smart_bins: Vec::new(),
            bin: BinSelection::Bin(BinId(0)),
            bin_name: String::new(),
            bin_duration_s: 0.0,
            sort: SortBy::Name,
            descending: false,
            items: Vec::new(),
            total: 0,
            selected: BTreeSet::new(),
            scroll: 0.0,
            bins_scroll: 0.0,
            metadata: None,
        }
    }
}

/// What the user did.
#[derive(Clone, Debug, PartialEq)]
pub enum MediaPoolAction {
    None,
    Tab(BrowserTab),
    /// The search text changed (the view already holds it).
    SearchChanged,
    SetView(PoolView),
    SelectBin(BinSelection),
    ToggleBin(BinId),
    /// The selection changed (the view already holds it).
    SelectionChanged,
    /// Double-click: open in the source viewer.
    Open(MediaId),
    Relink(MediaId),
    RemoveFilter(usize),
    SortBy(SortBy),
    /// The sort menu in the grid header.
    SortMenu,
    FilterMenu,
    NewBin,
    Import,
    More,
    /// `Edit…` in the metadata strip under the list.
    EditMetadata,
}

/// The metadata strip's view of a pool item.
pub fn metadata_view(item: &MediaItem) -> MetadataView {
    let file_name = project::media_pool::file_name(&item.path);
    MetadataView::new(
        &item.name,
        (!file_name.is_empty()).then_some(file_name),
        (item.size_bytes > 0).then_some(item.size_bytes),
        &item.facts,
        &item.metadata,
    )
}

/// The rows of the bins tree for `pool`, depth first in natural order, children of
/// collapsed bins left out. The root is always expanded.
pub fn bin_rows(pool: &MediaPool, expanded: &BTreeSet<BinId>) -> Vec<BinRow> {
    let counts = pool.counts();
    let mut rows = Vec::new();
    let mut stack = vec![(pool.root(), 0usize)];
    while let Some((id, depth)) = stack.pop() {
        let Some(bin) = pool.bin(id) else { continue };
        let children = pool.children(id);
        let open = id == pool.root() || expanded.contains(&id);
        rows.push(BinRow {
            id,
            name: bin.name.clone(),
            depth,
            count: counts.get(&id).copied().unwrap_or(0),
            has_children: !children.is_empty(),
            expanded: open,
        });
        if open {
            stack.extend(children.iter().rev().map(|c| (c.id, depth + 1)));
        }
    }
    rows
}

/// The label's colour: labels use the clip palette so they read the same everywhere.
pub fn label_role(label: ColourLabel) -> Role {
    match label {
        ColourLabel::Amber => Role::ClipTitle,
        ColourLabel::Blue => Role::ClipVideo,
        ColourLabel::Green => Role::ClipAudio,
        ColourLabel::Red => Role::ClipOffline,
        ColourLabel::Violet => Role::ClipAdjustment,
    }
}

/// The label's colour name (`Amber`).
pub fn label_text(label: ColourLabel) -> String {
    match label {
        ColourLabel::Amber => t!("pool.label.amber"),
        ColourLabel::Blue => t!("pool.label.blue"),
        ColourLabel::Green => t!("pool.label.green"),
        ColourLabel::Red => t!("pool.label.red"),
        ColourLabel::Violet => t!("pool.label.violet"),
    }
}

pub fn kind_text(kind: MediaKind) -> String {
    match kind {
        MediaKind::Video => t!("pool.kind.video"),
        MediaKind::Audio => t!("pool.kind.audio"),
        MediaKind::Still => t!("pool.kind.still"),
        MediaKind::Sequence => t!("pool.kind.sequence"),
    }
}

/// The codec as editors name it: `ProRes 422 HQ`, `HEVC 10-bit`, `H.264`, `PCM`.
pub fn codec_label(facts: &Facts) -> String {
    let Some(codec) = facts
        .video_codec
        .as_deref()
        .or(facts.audio_codec.as_deref())
    else {
        return String::new();
    };
    let profile = facts.video_profile.as_deref().unwrap_or("");
    let name = match codec {
        "prores" => {
            return match profile {
                "4444" | "4444 XQ" | "XQ" => format!("ProRes {profile}"),
                "Standard" | "" => "ProRes 422".to_string(),
                p => format!("ProRes 422 {p}"),
            };
        }
        "h264" => "H.264",
        "hevc" => "HEVC",
        "av1" => "AV1",
        "vp9" => "VP9",
        "dnxhd" => "DNxHD",
        "mjpeg" => "JPEG",
        "aac" => "AAC",
        c if c.starts_with("pcm_") => "PCM",
        c => return c.to_uppercase(),
    };
    if facts.video_codec.is_some() && facts.bit_depth > 8 {
        t!(
            "pool.codec.bit_depth",
            codec = name,
            depth = facts.bit_depth
        )
    } else {
        name.to_string()
    }
}

/// The camera and focal length: `ILME-FX6V · 35 mm`.
pub fn camera_label(item: &MediaItem) -> String {
    let m = &item.metadata;
    let model = m.camera_model.as_deref().or(m.camera_make.as_deref());
    match (model, m.focal_length_mm) {
        (Some(model), Some(mm)) => {
            t!(
                "pool.camera.with_focal",
                model = model,
                focal = format!("{mm:.0}")
            )
        }
        (Some(model), None) => model.to_string(),
        (None, Some(mm)) => t!("pool.camera.focal", focal = format!("{mm:.0}")),
        (None, None) => String::new(),
    }
}

/// What the status column and the grid badges say, most important first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ItemStatus {
    Offline,
    Analysing(f32),
    MakingProxy(f32),
    ProxyReady,
    NoProxy,
}

impl ItemStatus {
    pub fn of(row: &ItemRow) -> Self {
        if row.offline {
            ItemStatus::Offline
        } else if let Some(p) = row.analysing {
            ItemStatus::Analysing(p)
        } else {
            match row.proxy {
                ProxyState::Making(p) => ItemStatus::MakingProxy(p),
                ProxyState::Ready => ItemStatus::ProxyReady,
                ProxyState::None => ItemStatus::NoProxy,
            }
        }
    }
}

fn percent(fraction: f32) -> u32 {
    (fraction.clamp(0.0, 1.0) * 100.0).round() as u32
}

/// The grid's layout for `width` points of clip area: columns, tile width, thumbnail
/// height and the height of one tile row including its gap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridLayout {
    pub columns: usize,
    pub tile_width: f32,
    pub thumb_height: f32,
    pub row_height: f32,
}

impl GridLayout {
    pub fn for_width(width: f32) -> Self {
        let inner = (width - 16.0).max(layout::TILE_MIN_WIDTH);
        let columns = (((inner + layout::TILE_GAP) / (layout::TILE_MIN_WIDTH + layout::TILE_GAP))
            .floor() as usize)
            .max(1);
        let tile_width = (inner - layout::TILE_GAP * (columns as f32 - 1.0)) / columns as f32;
        let thumb_height = (tile_width * 9.0 / 16.0).round();
        Self {
            columns,
            tile_width,
            thumb_height,
            row_height: thumb_height + layout::TILE_TEXT + layout::TILE_ROW_GAP,
        }
    }

    pub fn rows(&self, items: usize) -> usize {
        items.div_ceil(self.columns)
    }
}

/// The first and one-past-last of `count` rows of `row_height` visible in a viewport of
/// `height` scrolled by `scroll`.
pub fn visible_rows(count: usize, row_height: f32, scroll: f32, height: f32) -> (usize, usize) {
    let first = ((scroll / row_height).floor().max(0.0) as usize).min(count);
    let last = (((scroll + height) / row_height).ceil().max(0.0) as usize).min(count);
    (first, last)
}

/// Draws the panel into `rect`.
pub fn media_pool(ui: &mut Ui, rect: Rect, view: &mut MediaPoolView) -> MediaPoolAction {
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, paint.c(Role::Bg1));
    let mut action = MediaPoolAction::None;

    let header = Rect::from_min_size(rect.min, vec2(rect.width(), size::ROW));
    panel_header(ui, &paint, header, &mut action);

    let toolbar_h = match view.view {
        PoolView::Grid => layout::GRID_TOOLBAR,
        PoolView::List => layout::LIST_TOOLBAR,
    };
    let toolbar = Rect::from_min_size(
        pos2(rect.min.x, header.max.y),
        vec2(rect.width(), toolbar_h),
    );
    match view.view {
        PoolView::Grid => grid_toolbar(ui, &paint, toolbar, view, &mut action),
        PoolView::List => list_toolbar(ui, &paint, toolbar, view, &mut action),
    }

    let body = Rect::from_min_max(pos2(rect.min.x, toolbar.max.y), rect.max);
    let list_needs = 16.0
        + 16.0
        + layout::LIST_NAME_MIN
        + layout::LIST_COLUMNS.iter().sum::<f32>()
        + 8.0 * layout::LIST_COLUMNS.len() as f32;
    let show_bins = match view.view {
        PoolView::Grid => true,
        PoolView::List => body.width() >= list_needs + layout::BINS_WIDTH,
    };
    let clips = if show_bins {
        let bins = Rect::from_min_size(body.min, vec2(layout::BINS_WIDTH, body.height()));
        bins_tree(ui, &paint, bins, view, &mut action);
        Rect::from_min_max(pos2(bins.max.x, body.min.y), body.max)
    } else {
        body
    };
    match view.view {
        PoolView::Grid => grid(ui, &paint, clips, view, &mut action),
        PoolView::List => list(ui, &paint, clips, view, &mut action),
    }
    action
}

fn hairline(paint: &Paint) -> Stroke {
    Stroke::new(1.0, paint.c(Role::BorderSubtle))
}

fn panel_header(ui: &mut Ui, paint: &Paint, rect: Rect, action: &mut MediaPoolAction) {
    let painter = ui.painter_at(rect);
    painter.hline(rect.x_range(), rect.max.y - 0.5, hairline(paint));
    let mut x = rect.min.x + 4.0;
    for tab in BrowserTab::ALL {
        let label = tab.label();
        let active = tab == BrowserTab::Media;
        let ty = if active {
            Type::LabelMedium
        } else {
            Type::Label
        };
        let w = paint.width(&painter, &label, ty) + 16.0;
        let tab_rect = Rect::from_min_size(pos2(x, rect.center().y - 10.0), vec2(w, 20.0));
        let response = ui.interact(
            tab_rect,
            ui.id().with(("pool.tab", x as i32)),
            Sense::click(),
        );
        if active {
            paint.glass_fill(&painter, tab_rect, paint.control_radius());
        } else if response.hovered() {
            painter.rect_filled(tab_rect, paint.control_radius(), paint.c(Role::Bg3));
        }
        if response.clicked() && !active {
            *action = MediaPoolAction::Tab(tab);
        }
        paint.text(
            &painter,
            tab_rect.center(),
            Align2::CENTER_CENTER,
            &label,
            ty,
            if active {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            },
            w,
        );
        x += w + 2.0;
    }
    let more = Rect::from_center_size(pos2(rect.max.x - 14.0, rect.center().y), vec2(20.0, 20.0));
    if icon_button(paint, ui, more, Icon::More, false).clicked() {
        *action = MediaPoolAction::More;
    }
}

/// The search field: `bg/2`, search icon, text, and a clear button while it has text.
/// Focused or holding text, it shows the accent ring.
fn search_field(
    ui: &mut Ui,
    paint: &Paint,
    rect: Rect,
    view: &mut MediaPoolView,
    action: &mut MediaPoolAction,
) {
    let painter = ui.painter().clone();
    let corner = paint.control_radius();
    painter.rect_filled(rect, corner, paint.c(Role::Bg2));
    let active = !view.search.is_empty();
    icon(
        &painter,
        Rect::from_center_size(
            pos2(rect.min.x + 8.0 + 7.0, rect.center().y),
            vec2(14.0, 14.0),
        ),
        Icon::Search,
        paint.c(if active {
            Role::TextSecondary
        } else {
            Role::TextMuted
        }),
        2.4,
    );
    let clear_w = if active { 12.0 + 8.0 } else { 0.0 };
    let text_rect = Rect::from_min_max(
        pos2(rect.min.x + 8.0 + 14.0 + 8.0, rect.min.y),
        pos2(rect.max.x - 8.0 - clear_w, rect.max.y),
    );
    let hint = egui::RichText::new(t!("pool.search.hint")).color(paint.c(Role::TextMuted));
    let edit = egui::TextEdit::singleline(&mut view.search)
        .frame(egui::Frame::NONE)
        .font(paint.font(Type::Label))
        .text_color(paint.c(Role::TextPrimary))
        .hint_text(hint)
        .vertical_align(egui::Align::Center)
        .desired_width(text_rect.width());
    let response = ui.put(text_rect, edit);
    if response.changed() {
        view.scroll = 0.0;
        *action = MediaPoolAction::SearchChanged;
    }
    if active {
        let clear = Rect::from_center_size(
            pos2(rect.max.x - 8.0 - 6.0, rect.center().y),
            vec2(16.0, 16.0),
        );
        let clear_response = ui.interact(clear, ui.id().with("pool.search.clear"), Sense::click());
        icon(
            &painter,
            clear.shrink(2.0),
            Icon::Close,
            paint.c(if clear_response.hovered() {
                Role::TextPrimary
            } else {
                Role::TextMuted
            }),
            2.6,
        );
        if clear_response.clicked() {
            view.search.clear();
            view.scroll = 0.0;
            *action = MediaPoolAction::SearchChanged;
        }
    }
    if response.has_focus() || active {
        painter.rect_stroke(
            rect.expand(1.0),
            corner,
            Stroke::new(2.0, paint.c(Role::AccentSoft)),
            StrokeKind::Outside,
        );
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, paint.c(Role::Accent)),
            StrokeKind::Inside,
        );
    }
}

/// The grid/list segmented control: `bg/0` well, the current view in glass.
fn view_toggle(
    ui: &mut Ui,
    paint: &Paint,
    right_center: Pos2,
    current: PoolView,
    action: &mut MediaPoolAction,
) -> Rect {
    let well = Rect::from_min_size(
        right_center - vec2(2.0 + 20.0 + 2.0 + 20.0 + 2.0, 12.0),
        vec2(46.0, 24.0),
    );
    let painter = ui.painter();
    painter.rect_filled(well, paint.control_radius(), paint.c(Role::Bg0));
    painter.rect_stroke(
        well,
        paint.control_radius(),
        hairline(paint),
        StrokeKind::Inside,
    );
    for (i, (mode, which)) in [(PoolView::List, Icon::List), (PoolView::Grid, Icon::Grid)]
        .into_iter()
        .enumerate()
    {
        let button = Rect::from_min_size(
            well.min + vec2(2.0 + i as f32 * 22.0, 2.0),
            vec2(20.0, 20.0),
        );
        if icon_button(paint, ui, button, which, mode == current).clicked() && mode != current {
            *action = MediaPoolAction::SetView(mode);
        }
    }
    well
}

fn grid_toolbar(
    ui: &mut Ui,
    paint: &Paint,
    rect: Rect,
    view: &mut MediaPoolView,
    action: &mut MediaPoolAction,
) {
    ui.painter()
        .hline(rect.x_range(), rect.max.y - 0.5, hairline(paint));
    let cy = rect.center().y;
    // Right to left: new bin (glass outline), filter, view toggle; the search takes the
    // rest.
    let plus = Rect::from_min_size(pos2(rect.max.x - 8.0 - 24.0, cy - 12.0), vec2(24.0, 24.0));
    let response = ui.allocate_rect(plus, Sense::click());
    paint.glass_outline(ui.painter(), plus, paint.control_radius());
    if response.hovered() {
        ui.painter()
            .rect_filled(plus, paint.control_radius(), paint.mix(Role::Bg3, 0.6));
    }
    icon(
        ui.painter(),
        Rect::from_center_size(plus.center(), vec2(14.0, 14.0)),
        Icon::Plus,
        paint.c(Role::TextPrimary),
        2.4,
    );
    if response.clicked() {
        *action = MediaPoolAction::NewBin;
    }
    let filter = Rect::from_min_size(pos2(plus.min.x - 4.0 - 24.0, cy - 12.0), vec2(24.0, 24.0));
    if icon_button(paint, ui, filter, Icon::Filter, false).clicked() {
        *action = MediaPoolAction::FilterMenu;
    }
    let toggle = view_toggle(ui, paint, pos2(filter.min.x - 4.0, cy), view.view, action);
    let search = Rect::from_min_max(
        pos2(rect.min.x + 8.0, cy - 12.0),
        pos2(toggle.min.x - 4.0, cy + 12.0),
    );
    search_field(ui, paint, search, view, action);
}

fn list_toolbar(
    ui: &mut Ui,
    paint: &Paint,
    rect: Rect,
    view: &mut MediaPoolView,
    action: &mut MediaPoolAction,
) {
    let painter = ui.painter().clone();
    painter.hline(rect.x_range(), rect.max.y - 0.5, hairline(paint));
    let cy = rect.center().y;
    let toggle = view_toggle(ui, paint, pos2(rect.max.x - 8.0, cy), view.view, action);
    let count = t!(
        "pool.count",
        shown = view.items.len(),
        total = view.total.max(view.items.len())
    );
    let count_rect = paint.text(
        &painter,
        pos2(toggle.min.x - 8.0, cy),
        Align2::RIGHT_CENTER,
        &count,
        Type::MonoCaption,
        Role::TextMuted,
        200.0,
    );
    let search_w = layout::LIST_SEARCH_WIDTH.min(count_rect.min.x - 16.0 - rect.min.x);
    let search = Rect::from_min_size(pos2(rect.min.x + 8.0, cy - 12.0), vec2(search_w, 24.0));
    search_field(ui, paint, search, view, action);
    let mut x = search.max.x + 8.0;
    for (i, chip) in view.filters.iter().enumerate() {
        let text_w = paint.width(&painter, &chip.text, Type::Caption);
        let dot_w = if chip.dot.is_some() { 6.0 + 4.0 } else { 0.0 };
        let w = 8.0 + dot_w + text_w + 4.0 + 12.0 + 4.0;
        if x + w > count_rect.min.x - 8.0 {
            break;
        }
        let pill = Rect::from_min_size(pos2(x, cy - 10.0), vec2(w, 20.0));
        paint.glass_outline(&painter, pill, CornerRadius::same(255));
        let mut tx = pill.min.x + 8.0;
        if let Some(label) = chip.dot {
            paint.dot(
                &painter,
                pos2(tx + 3.0, cy),
                6.0,
                paint.c(label_role(label)),
            );
            tx += dot_w;
        }
        paint.text(
            &painter,
            pos2(tx, cy),
            Align2::LEFT_CENTER,
            &chip.text,
            Type::Caption,
            Role::TextPrimary,
            text_w + 1.0,
        );
        let close = Rect::from_center_size(pos2(pill.max.x - 4.0 - 6.0, cy), vec2(12.0, 12.0));
        let response = ui.interact(
            close.expand(2.0),
            ui.id().with(("pool.filter", i)),
            Sense::click(),
        );
        icon(
            &painter,
            close,
            Icon::Close,
            paint.c(if response.hovered() {
                Role::TextPrimary
            } else {
                Role::TextMuted
            }),
            2.6,
        );
        if response.clicked() {
            *action = MediaPoolAction::RemoveFilter(i);
        }
        x = pill.max.x + 8.0;
    }
}

/// A small clock for the `Added today` smart bin (the shared icon set has none).
fn clock_icon(painter: &egui::Painter, rect: Rect, color: egui::Color32, stroke_24: f32) {
    let s = rect.width() / 24.0;
    let c = rect.center();
    let stroke = Stroke::new(stroke_24 * s, color);
    painter.circle_stroke(c, 10.0 * s, stroke);
    painter.line(
        vec![c + vec2(0.0, -6.0) * s, c, c + vec2(4.0, 2.0) * s],
        stroke,
    );
}

fn scroll_by_wheel(ui: &Ui, area: Rect, scroll: &mut f32, content: f32) {
    if ui.rect_contains_pointer(area) {
        let delta = ui.input(|i| i.smooth_scroll_delta.y);
        if delta.abs() > 0.1 {
            *scroll -= delta;
        }
    }
    *scroll = scroll.clamp(0.0, (content - area.height()).max(0.0));
}

fn bins_tree(
    ui: &mut Ui,
    paint: &Paint,
    rect: Rect,
    view: &mut MediaPoolView,
    action: &mut MediaPoolAction,
) {
    let painter = ui.painter_at(rect);
    painter.vline(rect.max.x - 0.5, rect.y_range(), hairline(paint));
    let inner = Rect::from_min_max(rect.min + vec2(4.0, 4.0), rect.max - vec2(5.0, 4.0));
    let content = layout::BIN_SECTION
        + view.bins.len() as f32 * size::ROW
        + if view.smart_bins.is_empty() {
            0.0
        } else {
            layout::BIN_SECTION + 8.0 + view.smart_bins.len() as f32 * size::ROW
        };
    scroll_by_wheel(ui, inner, &mut view.bins_scroll, content);
    let clip = painter.with_clip_rect(inner);
    let mut y = inner.min.y - view.bins_scroll;

    let section = |y: f32, key: &str| {
        paint.text(
            &clip,
            pos2(inner.min.x + 8.0, y + layout::BIN_SECTION / 2.0),
            Align2::LEFT_CENTER,
            key,
            Type::Caps,
            Role::TextMuted,
            inner.width() - 16.0,
        );
    };
    section(y, &t!("pool.bins"));
    y += layout::BIN_SECTION;

    for row in &view.bins {
        let row_rect = Rect::from_min_size(pos2(inner.min.x, y), vec2(inner.width(), size::ROW));
        y += size::ROW;
        if !row_rect.intersects(inner) {
            continue;
        }
        let selected = view.bin == BinSelection::Bin(row.id);
        let response = ui.interact(
            row_rect.intersect(inner),
            ui.id().with(("pool.bin", row.id.0)),
            Sense::click(),
        );
        if selected {
            paint.glass_fill(&clip, row_rect, paint.control_radius());
            clip.rect_filled(
                Rect::from_min_size(pos2(row_rect.min.x, row_rect.min.y + 6.0), vec2(2.0, 16.0)),
                CornerRadius::same(255),
                paint.c(Role::Accent),
            );
        } else if response.hovered() {
            clip.rect_filled(row_rect, paint.control_radius(), paint.c(Role::Bg3));
        }
        let folder_x = row_rect.min.x + 20.0 + 16.0 * row.depth.saturating_sub(1) as f32;
        if row.has_children {
            let chevron = Rect::from_min_size(
                pos2(folder_x - 16.0, row_rect.center().y - 6.0),
                vec2(12.0, 12.0),
            );
            icon(
                &clip,
                chevron,
                if row.expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                paint.c(Role::TextMuted),
                2.4,
            );
            let toggle = ui.interact(
                chevron.expand(3.0).intersect(inner),
                ui.id().with(("pool.bin.toggle", row.id.0)),
                Sense::click(),
            );
            if toggle.clicked() && row.depth > 0 {
                *action = MediaPoolAction::ToggleBin(row.id);
            }
        }
        icon(
            &clip,
            Rect::from_min_size(pos2(folder_x, row_rect.center().y - 7.0), vec2(14.0, 14.0)),
            Icon::Folder,
            paint.c(if selected || row.depth == 0 {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            }),
            2.4,
        );
        let strong = selected || row.depth == 0;
        let count_x = row_rect.max.x - 8.0;
        let count_rect = if row.depth > 0 {
            paint.text(
                &clip,
                pos2(count_x, row_rect.center().y),
                Align2::RIGHT_CENTER,
                &row.count.to_string(),
                Type::MonoCaption,
                if selected {
                    Role::TextSecondary
                } else {
                    Role::TextMuted
                },
                48.0,
            )
        } else {
            Rect::from_min_size(pos2(count_x, row_rect.min.y), egui::Vec2::ZERO)
        };
        let name_x = folder_x + 14.0 + 8.0;
        paint.text(
            &clip,
            pos2(name_x, row_rect.center().y),
            Align2::LEFT_CENTER,
            &row.name,
            if strong {
                Type::LabelMedium
            } else {
                Type::Label
            },
            if strong {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            },
            count_rect.min.x - 8.0 - name_x,
        );
        if response.clicked() && !selected {
            view.bin = BinSelection::Bin(row.id);
            view.scroll = 0.0;
            *action = MediaPoolAction::SelectBin(view.bin);
        }
    }

    if view.smart_bins.is_empty() {
        return;
    }
    section(y + 8.0, &t!("pool.smart_bins"));
    y += layout::BIN_SECTION + 8.0;
    for (i, smart) in view.smart_bins.iter().enumerate() {
        let row_rect = Rect::from_min_size(pos2(inner.min.x, y), vec2(inner.width(), size::ROW));
        y += size::ROW;
        if !row_rect.intersects(inner) {
            continue;
        }
        let selected = view.bin == BinSelection::Smart(i);
        let response = ui.interact(
            row_rect.intersect(inner),
            ui.id().with(("pool.smart", i)),
            Sense::click(),
        );
        if selected {
            paint.glass_fill(&clip, row_rect, paint.control_radius());
        } else if response.hovered() {
            clip.rect_filled(row_rect, paint.control_radius(), paint.c(Role::Bg3));
        }
        let icon_rect = Rect::from_min_size(
            pos2(row_rect.min.x + 8.0, row_rect.center().y - 7.0),
            vec2(14.0, 14.0),
        );
        match smart.icon {
            SmartIcon::Offline => {
                icon(&clip, icon_rect, Icon::Info, paint.c(Role::StateError), 2.4)
            }
            SmartIcon::Recent => clock_icon(&clip, icon_rect, paint.c(Role::TextSecondary), 2.4),
            SmartIcon::Film => icon(
                &clip,
                icon_rect,
                Icon::Film,
                paint.c(Role::TextSecondary),
                2.4,
            ),
            SmartIcon::Filter => icon(
                &clip,
                icon_rect,
                Icon::Filter,
                paint.c(Role::TextSecondary),
                2.4,
            ),
        }
        let count_rect = paint.text(
            &clip,
            pos2(row_rect.max.x - 8.0, row_rect.center().y),
            Align2::RIGHT_CENTER,
            &smart.count.to_string(),
            Type::MonoCaption,
            if smart.alert && smart.count > 0 {
                Role::StateError
            } else {
                Role::TextMuted
            },
            48.0,
        );
        let name_x = icon_rect.max.x + 8.0;
        paint.text(
            &clip,
            pos2(name_x, row_rect.center().y),
            Align2::LEFT_CENTER,
            &smart.name,
            if selected {
                Type::LabelMedium
            } else {
                Type::Label
            },
            if selected {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            },
            count_rect.min.x - 8.0 - name_x,
        );
        if response.clicked() && !selected {
            view.bin = BinSelection::Smart(i);
            view.scroll = 0.0;
            *action = MediaPoolAction::SelectBin(view.bin);
        }
    }
}

/// Click selects one clip, Command or Shift click adds or removes it, double-click opens.
fn handle_item_click(
    ui: &Ui,
    response: &egui::Response,
    id: MediaId,
    view: &mut MediaPoolView,
    action: &mut MediaPoolAction,
) {
    if response.double_clicked() {
        *action = MediaPoolAction::Open(id);
    } else if response.clicked() {
        let extend = ui.input(|i| i.modifiers.command || i.modifiers.shift);
        if extend {
            if !view.selected.remove(&id) {
                view.selected.insert(id);
            }
        } else {
            view.selected.clear();
            view.selected.insert(id);
        }
        *action = MediaPoolAction::SelectionChanged;
    }
}

/// A thumbnail: the poster frame, or `bg/0` with the kind's icon until it loads.
fn thumbnail(
    paint: &Paint,
    painter: &egui::Painter,
    rect: Rect,
    row: &ItemRow,
    corner: CornerRadius,
) {
    painter.rect_filled(rect, corner, paint.c(Role::Bg0));
    if row.offline {
        let painter = &painter.with_clip_rect(rect.intersect(painter.clip_rect()));
        // The offline look: the clip's colours behind a halftone screen.
        let r = rect.size() * 0.6;
        paint.glow(
            painter,
            rect.lerp_inside(vec2(0.3, 0.4)),
            r,
            paint.mix(Role::ClipOffline, 0.7),
        );
        paint.glow(
            painter,
            rect.lerp_inside(vec2(0.75, 0.65)),
            r,
            paint.mix(Role::ClipVideo, 0.7),
        );
        let cell = if rect.height() < 24.0 { 3.0 } else { 5.0 };
        paint.halftone(painter, rect, cell, paint.c(Role::Bg0));
        return;
    }
    if let Some(texture) = row.thumbnail {
        painter.image(
            texture,
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            crate::theme::TEXTURE_TINT,
        );
        return;
    }
    let which = match row.kind {
        MediaKind::Video | MediaKind::Sequence => Icon::Film,
        MediaKind::Audio => Icon::Music,
        MediaKind::Still => Icon::Image,
    };
    let side = (rect.height() * 0.4).clamp(8.0, 20.0);
    icon(
        painter,
        Rect::from_center_size(rect.center(), vec2(side, side)),
        which,
        paint.c(Role::TextMuted),
        1.8,
    );
}

/// The label dot on a thumbnail: 8 pt with a `bg/0` 60% ring.
fn label_dot(paint: &Paint, painter: &egui::Painter, center: Pos2, label: ColourLabel) {
    painter.circle_filled(center, 4.5, paint.mix(Role::Bg0, 0.6));
    painter.circle_filled(center, 4.0, paint.c(label_role(label)));
}

fn grid(
    ui: &mut Ui,
    paint: &Paint,
    rect: Rect,
    view: &mut MediaPoolView,
    action: &mut MediaPoolAction,
) {
    let painter = ui.painter_at(rect);
    // Header: bin name, count and length, sort.
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), size::ROW));
    let sort_label = match view.sort {
        SortBy::Name => t!("pool.sort.name"),
        SortBy::Added => t!("pool.sort.added"),
        SortBy::Duration => t!("pool.sort.duration"),
        SortBy::Kind => t!("pool.sort.kind"),
        SortBy::Size => t!("pool.sort.size"),
    };
    let sort_w = paint.width(&painter, &sort_label, Type::Caption) + 8.0 + 4.0 + 12.0 + 4.0;
    let sort = Rect::from_min_size(
        pos2(header.max.x - 8.0 - sort_w, header.center().y - 10.0),
        vec2(sort_w, 20.0),
    );
    let sort_response = ui.interact(sort, ui.id().with("pool.sort"), Sense::click());
    if sort_response.hovered() {
        painter.rect_filled(sort, paint.control_radius(), paint.c(Role::Bg3));
    }
    paint.text(
        &painter,
        pos2(sort.min.x + 8.0, sort.center().y),
        Align2::LEFT_CENTER,
        &sort_label,
        Type::Caption,
        Role::TextSecondary,
        sort_w,
    );
    icon(
        &painter,
        Rect::from_center_size(
            pos2(sort.max.x - 4.0 - 6.0, sort.center().y),
            vec2(12.0, 12.0),
        ),
        Icon::ChevronDown,
        paint.c(Role::TextSecondary),
        2.4,
    );
    if sort_response.clicked() {
        *action = MediaPoolAction::SortMenu;
    }
    let name_rect = paint.text(
        &painter,
        pos2(header.min.x + 8.0, header.center().y),
        Align2::LEFT_CENTER,
        &view.bin_name,
        Type::LabelMedium,
        Role::TextPrimary,
        (sort.min.x - header.min.x - 80.0).max(40.0),
    );
    let summary = t!(
        "pool.bin.summary",
        count = view.items.len(),
        duration = format::short_clock(view.bin_duration_s)
    );
    paint.text(
        &painter,
        pos2(name_rect.max.x + 8.0, header.center().y + 1.0),
        Align2::LEFT_CENTER,
        &summary,
        Type::MonoCaption,
        Role::TextMuted,
        (sort.min.x - name_rect.max.x - 16.0).max(1.0),
    );

    let area = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max);
    if view.items.is_empty() {
        if view.search.is_empty() && view.filters.is_empty() {
            if import::drop_area(ui, area.shrink(8.0), &view.bin_name, &DropArea::Empty)
                == DropAreaAction::OpenFilePicker
            {
                *action = MediaPoolAction::Import;
            }
        } else {
            no_results(paint, &painter, area, &view.search);
        }
        return;
    }
    let grid = GridLayout::for_width(area.width());
    let content = 4.0 + grid.rows(view.items.len()) as f32 * grid.row_height;
    scroll_by_wheel(ui, area, &mut view.scroll, content);
    let origin = pos2(area.min.x + 8.0, area.min.y + 4.0 - view.scroll);
    let (first, last) = visible_rows(
        grid.rows(view.items.len()),
        grid.row_height,
        (view.scroll - 4.0).max(0.0),
        area.height(),
    );
    let clip = painter.with_clip_rect(area);
    let corner = paint.control_radius();
    for index in first * grid.columns..(last * grid.columns).min(view.items.len()) {
        let (r, c) = (index / grid.columns, index % grid.columns);
        let min = origin
            + vec2(
                c as f32 * (grid.tile_width + layout::TILE_GAP),
                r as f32 * grid.row_height,
            );
        let tile = Rect::from_min_size(
            min,
            vec2(grid.tile_width, grid.thumb_height + layout::TILE_TEXT),
        );
        let row = &view.items[index];
        let response = ui.interact(
            tile.intersect(area),
            ui.id().with(("pool.tile", row.id.0)),
            Sense::click(),
        );
        let thumb = Rect::from_min_size(tile.min, vec2(tile.width(), grid.thumb_height));
        let selected = view.selected.contains(&row.id);
        thumbnail(paint, &clip, thumb, row, corner);
        grid_badges(paint, &clip, thumb, row);
        if selected {
            clip.rect_stroke(
                thumb,
                corner,
                Stroke::new(1.5, paint.c(Role::Accent)),
                StrokeKind::Outside,
            );
        } else if response.hovered() {
            clip.rect_stroke(
                thumb,
                corner,
                Stroke::new(1.0, paint.c(Role::BorderStrong)),
                StrokeKind::Outside,
            );
        }
        let name_y = thumb.max.y + 4.0 + 8.0;
        paint.text(
            &clip,
            pos2(tile.min.x, name_y),
            Align2::LEFT_CENTER,
            &row.name,
            if selected {
                Type::LabelMedium
            } else {
                Type::Label
            },
            if selected {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            },
            tile.width(),
        );
        let (meta, role) = grid_meta(row);
        let meta_rect = paint.text(
            &clip,
            pos2(tile.min.x, name_y + 8.0 + 4.0 + 7.0),
            Align2::LEFT_CENTER,
            &meta,
            Type::MonoCaption,
            role,
            tile.width(),
        );
        if row.offline {
            let relink = ui.interact(
                meta_rect.intersect(area),
                ui.id().with(("pool.relink", row.id.0)),
                Sense::click(),
            );
            if relink.clicked() {
                *action = MediaPoolAction::Relink(row.id);
                continue;
            }
        }
        handle_item_click(ui, &response, row.id, view, action);
    }
}

/// The line under a tile's name: `4K · 23.976`, `48 kHz`, or `Relink…` when offline.
fn grid_meta(row: &ItemRow) -> (String, Role) {
    if row.offline {
        return (t!("pool.grid.relink"), Role::StateError);
    }
    let mut parts = Vec::new();
    if row.width > 0 && row.height > 0 {
        parts.push(format::resolution_class(row.width, row.height));
    }
    if let Some(fps) = row.fps.filter(|_| row.kind != MediaKind::Still) {
        parts.push(format::fps(fps));
    }
    if parts.is_empty() {
        parts.push(kind_text(row.kind));
    }
    (parts.join(" · "), Role::TextMuted)
}

/// Label dot and proxy badge top left, length bottom right; the analysing band or the
/// offline badge instead of the length.
fn grid_badges(paint: &Paint, painter: &egui::Painter, thumb: Rect, row: &ItemRow) {
    let mut x = thumb.min.x + 4.0;
    if let Some(label) = row.label {
        label_dot(paint, painter, pos2(x + 4.0, thumb.min.y + 8.0), label);
        x += 8.0 + 4.0;
    }
    match ItemStatus::of(row) {
        ItemStatus::Offline => {
            let text = t!("pool.badge.offline");
            let galley = paint.galley(
                painter,
                &text,
                Type::Caption,
                paint.c(Role::TextPrimary),
                thumb.width(),
            );
            let badge =
                Rect::from_center_size(thumb.center(), vec2(galley.size().x + 8.0, size::BADGE));
            painter.rect_filled(badge, paint.control_radius(), paint.mix(Role::Bg0, 0.8));
            painter.galley(
                pos2(badge.min.x + 4.0, badge.center().y - galley.size().y / 2.0),
                galley,
                paint.c(Role::TextPrimary),
            );
            return;
        }
        ItemStatus::Analysing(p) => {
            let band = Rect::from_min_max(pos2(thumb.min.x, thumb.max.y - 26.0), thumb.max);
            painter.rect_filled(
                band,
                CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: paint.control_radius().sw,
                    se: paint.control_radius().se,
                },
                paint.mix(Role::Bg0, 0.72),
            );
            let text_y = band.min.y + 4.0 + 7.0;
            paint.text(
                painter,
                pos2(band.min.x + 4.0, text_y),
                Align2::LEFT_CENTER,
                &t!("pool.status.analysing"),
                Type::Caption,
                Role::TextPrimary,
                band.width() - 40.0,
            );
            paint.text(
                painter,
                pos2(band.max.x - 4.0, text_y),
                Align2::RIGHT_CENTER,
                &t!("pool.percent", percent = percent(p)),
                Type::MonoCaption,
                Role::TextSecondary,
                40.0,
            );
            paint.progress(
                painter,
                Rect::from_min_size(
                    pos2(band.min.x + 4.0, band.max.y - 4.0 - 2.0),
                    vec2(band.width() - 8.0, 2.0),
                ),
                p,
                Role::Bg3,
            );
            if row.proxy == ProxyState::Ready {
                paint.badge(
                    painter,
                    pos2(x, thumb.min.y + 4.0),
                    Align2::LEFT_TOP,
                    &t!("pool.badge.proxy"),
                    Type::MonoCaption,
                );
            }
            return;
        }
        ItemStatus::MakingProxy(p) => {
            let text = t!("pool.badge.proxy_making", percent = percent(p));
            paint.badge(
                painter,
                pos2(x, thumb.min.y + 4.0),
                Align2::LEFT_TOP,
                &text,
                Type::MonoCaption,
            );
        }
        ItemStatus::ProxyReady => {
            paint.badge(
                painter,
                pos2(x, thumb.min.y + 4.0),
                Align2::LEFT_TOP,
                &t!("pool.badge.proxy"),
                Type::MonoCaption,
            );
        }
        ItemStatus::NoProxy => {}
    }
    if let Some(d) = row.duration_s.filter(|_| row.kind != MediaKind::Still) {
        paint.badge(
            painter,
            thumb.max - vec2(4.0, 4.0),
            Align2::RIGHT_BOTTOM,
            &format::clock(d),
            Type::MonoCaption,
        );
    }
}

fn no_results(paint: &Paint, painter: &egui::Painter, area: Rect, search: &str) {
    let text = if search.trim().is_empty() {
        t!("pool.empty.filters")
    } else {
        t!("pool.empty.search", query = search.trim())
    };
    let galley = paint.wrapped(
        painter,
        &text,
        Type::Label,
        paint.c(Role::TextMuted),
        area.width() - 32.0,
        true,
    );
    let pos = pos2(area.center().x - galley.size().x / 2.0, area.min.y + 48.0);
    painter.galley(pos, galley, paint.c(Role::TextMuted));
}

/// The list columns' x ranges: label slot, name, then [`layout::LIST_COLUMNS`].
fn list_columns(left: f32, right: f32) -> [(f32, f32); 9] {
    let gap = 8.0;
    let fixed: f32 =
        layout::LIST_COLUMNS.iter().sum::<f32>() + gap * layout::LIST_COLUMNS.len() as f32;
    let label = (left + 8.0, left + 8.0 + 16.0);
    let name_end = (right - 8.0 - fixed).max(label.1 + gap + 40.0);
    let mut out = [(0.0, 0.0); 9];
    out[0] = label;
    out[1] = (label.1 + gap, name_end);
    let mut x = name_end + gap;
    for (i, w) in layout::LIST_COLUMNS.iter().enumerate() {
        out[i + 2] = (x, x + w);
        x += w + gap;
    }
    out
}

fn list(
    ui: &mut Ui,
    paint: &Paint,
    rect: Rect,
    view: &mut MediaPoolView,
    action: &mut MediaPoolAction,
) {
    let rect = match &view.metadata {
        Some(metadata) if rect.height() > METADATA_PANEL_HEIGHT + 3.0 * size::ROW => {
            let strip = Rect::from_min_max(
                pos2(rect.min.x, rect.max.y - METADATA_PANEL_HEIGHT),
                rect.max,
            );
            if metadata_panel(ui, strip, metadata) == MetadataAction::Edit {
                *action = MediaPoolAction::EditMetadata;
            }
            Rect::from_min_max(rect.min, pos2(rect.max.x, strip.min.y))
        }
        _ => rect,
    };
    let painter = ui.painter_at(rect);
    let columns = list_columns(rect.min.x, rect.max.x);
    // Column header.
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), layout::COLUMN_HEADER));
    painter.rect_filled(header, 0.0, paint.c(Role::Bg0));
    painter.hline(header.x_range(), header.max.y - 0.5, hairline(paint));
    let titles = [
        (1, t!("pool.column.name"), Some(SortBy::Name)),
        (2, t!("pool.column.duration"), Some(SortBy::Duration)),
        (3, t!("pool.column.resolution"), None),
        (4, t!("pool.column.fps"), None),
        (5, t!("pool.column.codec"), None),
        (6, t!("pool.column.camera"), None),
        (7, t!("pool.column.tags"), None),
        (8, t!("pool.column.status"), None),
    ];
    for (col, title, sort) in titles {
        let (x0, x1) = columns[col];
        let sorted = sort == Some(view.sort);
        let r = paint.text(
            &painter,
            pos2(x0, header.center().y),
            Align2::LEFT_CENTER,
            &title,
            Type::Caption,
            if sorted {
                Role::TextPrimary
            } else {
                Role::TextMuted
            },
            x1 - x0,
        );
        if sorted {
            icon(
                &painter,
                Rect::from_min_size(
                    pos2(r.max.x + 4.0, header.center().y - 5.0),
                    vec2(10.0, 10.0),
                ),
                Icon::ChevronDown,
                paint.c(Role::TextPrimary),
                3.0,
            );
        }
        if let Some(by) = sort {
            let hit = Rect::from_x_y_ranges(x0..=x1, header.y_range());
            if ui
                .interact(hit, ui.id().with(("pool.column", col)), Sense::click())
                .clicked()
            {
                *action = MediaPoolAction::SortBy(by);
            }
        }
    }

    let area = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max);
    if view.items.is_empty() {
        no_results(paint, &painter, area, &view.search);
        return;
    }
    let content = 8.0 + view.items.len() as f32 * size::ROW;
    scroll_by_wheel(ui, area, &mut view.scroll, content);
    let (first, last) = visible_rows(
        view.items.len(),
        size::ROW,
        (view.scroll - 4.0).max(0.0),
        area.height(),
    );
    let clip = painter.with_clip_rect(area);
    let top = area.min.y + 4.0 - view.scroll;
    for index in first..last {
        let row = &view.items[index];
        let row_rect = Rect::from_min_size(
            pos2(area.min.x + 4.0, top + index as f32 * size::ROW),
            vec2(area.width() - 8.0, size::ROW),
        );
        let response = ui.interact(
            row_rect.intersect(area),
            ui.id().with(("pool.row", row.id.0)),
            Sense::click(),
        );
        let selected = view.selected.contains(&row.id);
        if selected {
            paint.glass_fill(&clip, row_rect, paint.control_radius());
            clip.rect_filled(
                Rect::from_min_size(pos2(row_rect.min.x, row_rect.min.y + 6.0), vec2(2.0, 16.0)),
                CornerRadius::same(255),
                paint.c(Role::Accent),
            );
        } else if response.hovered() {
            clip.rect_filled(row_rect, paint.control_radius(), paint.c(Role::Bg3));
        }
        if list_row(ui, paint, &clip, row_rect, &columns, row, selected) {
            *action = MediaPoolAction::Relink(row.id);
            continue;
        }
        handle_item_click(ui, &response, row.id, view, action);
    }
}

/// Draws one list row's cells; returns true when its `Relink` was clicked.
fn list_row(
    ui: &mut Ui,
    paint: &Paint,
    painter: &egui::Painter,
    rect: Rect,
    columns: &[(f32, f32); 9],
    row: &ItemRow,
    selected: bool,
) -> bool {
    let cy = rect.center().y;
    let muted = row.offline;
    let dim = |role: Role| if muted { Role::TextMuted } else { role };
    if let Some(label) = row.label {
        let (x0, x1) = columns[0];
        paint.dot(
            painter,
            pos2((x0 + x1) / 2.0, cy),
            8.0,
            paint.c(label_role(label)),
        );
    }
    // Name with its small thumbnail.
    let (x0, x1) = columns[1];
    let thumb = Rect::from_min_size(pos2(x0, cy - 8.0), vec2(28.0, 16.0));
    thumbnail(paint, painter, thumb, row, CornerRadius::same(2));
    let strong = selected && !muted;
    paint.text(
        painter,
        pos2(thumb.max.x + 8.0, cy),
        Align2::LEFT_CENTER,
        &row.name,
        if strong {
            Type::LabelMedium
        } else {
            Type::Label
        },
        if muted {
            Role::TextMuted
        } else if selected {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        },
        x1 - thumb.max.x - 8.0,
    );
    let none = t!("pool.none");
    let cell = |col: usize, text: &str, ty: Type, role: Role| {
        let (x0, x1) = columns[col];
        paint.text(
            painter,
            pos2(x0, cy),
            Align2::LEFT_CENTER,
            text,
            ty,
            role,
            x1 - x0,
        );
    };
    let duration = match row.duration_s {
        Some(d) if row.kind != MediaKind::Still => format::timecode(d, row.fps),
        _ => none.clone(),
    };
    cell(2, &duration, Type::MonoCaption, dim(Role::TextPrimary));
    let (resolution, fps) = if muted || row.width == 0 {
        (none.clone(), none.clone())
    } else {
        (
            format!("{}×{}", row.width, row.height),
            row.fps
                .filter(|_| row.kind != MediaKind::Still)
                .map(format::fps)
                .unwrap_or_else(|| none.clone()),
        )
    };
    cell(3, &resolution, Type::MonoCaption, dim(Role::TextSecondary));
    cell(4, &fps, Type::MonoCaption, dim(Role::TextSecondary));
    cell(5, &row.codec, Type::Caption, dim(Role::TextSecondary));
    cell(6, &row.camera, Type::Caption, dim(Role::TextSecondary));
    // Tags: chips while they fit, keeping room for a `+N` chip after the last one shown.
    // One layout per tag: the chip is drawn from the measured galley.
    let (x0, x1) = columns[7];
    let more_room = 28.0 + 4.0;
    let mut x = x0;
    for (i, tag) in row.tags.iter().enumerate() {
        let rest = row.tags.len() - i - 1;
        let limit = x1 - x - if rest > 0 { more_room } else { 0.0 };
        let text = paint.c(Role::TextPrimary);
        let mut galley = paint.galley(painter, tag, Type::Caption, text, f32::INFINITY);
        if galley.size().x + 12.0 > limit {
            if i > 0 {
                paint.chip(
                    painter,
                    pos2(x, cy),
                    &t!("pool.tags.more", count = rest + 1),
                    x1 - x,
                );
                break;
            }
            galley = paint.galley(painter, tag, Type::Caption, text, (limit - 12.0).max(12.0));
        }
        let chip = Rect::from_min_size(
            pos2(x, cy - size::BADGE / 2.0),
            vec2(galley.size().x + 12.0, size::BADGE),
        );
        paint.glass_outline(painter, chip, CornerRadius::same(255));
        painter.galley(
            pos2(chip.min.x + 6.0, cy - galley.size().y / 2.0),
            galley,
            text,
        );
        x = chip.max.x + 4.0;
    }
    // Status.
    let (x0, x1) = columns[8];
    let dot = |role: Role| paint.dot(painter, pos2(x0 + 3.0, cy), 6.0, paint.c(role));
    let text_x = x0 + 6.0 + 6.0;
    match ItemStatus::of(row) {
        ItemStatus::Offline => {
            dot(Role::StateError);
            let r = paint.text(
                painter,
                pos2(text_x, cy),
                Align2::LEFT_CENTER,
                &t!("pool.status.offline"),
                Type::Caption,
                Role::StateError,
                x1 - text_x,
            );
            return ui
                .interact(
                    r,
                    ui.id().with(("pool.row.relink", row.id.0)),
                    Sense::click(),
                )
                .clicked();
        }
        ItemStatus::Analysing(p) => {
            let text_y = cy - 3.0;
            paint.text(
                painter,
                pos2(x0, text_y),
                Align2::LEFT_CENTER,
                &t!("pool.status.analysing"),
                Type::Caption,
                Role::TextSecondary,
                x1 - x0 - 32.0,
            );
            paint.text(
                painter,
                pos2(x1, text_y),
                Align2::RIGHT_CENTER,
                &t!("pool.percent", percent = percent(p)),
                Type::MonoCaption,
                Role::TextMuted,
                32.0,
            );
            paint.progress(
                painter,
                Rect::from_min_size(pos2(x0, text_y + 7.0 + 3.0), vec2(x1 - x0, 2.0)),
                p,
                Role::Bg3,
            );
        }
        ItemStatus::MakingProxy(p) => {
            dot(Role::StateInfo);
            let text = t!("pool.status.making_proxy", percent = percent(p));
            paint.text(
                painter,
                pos2(text_x, cy),
                Align2::LEFT_CENTER,
                &text,
                Type::Caption,
                Role::TextSecondary,
                x1 - text_x,
            );
        }
        ItemStatus::ProxyReady => {
            dot(Role::StateSuccess);
            let text = t!("pool.status.proxy_ready");
            paint.text(
                painter,
                pos2(text_x, cy),
                Align2::LEFT_CENTER,
                &text,
                Type::Caption,
                Role::TextSecondary,
                x1 - text_x,
            );
        }
        ItemStatus::NoProxy => {
            dot(Role::TextMuted);
            let text = t!("pool.status.no_proxy");
            paint.text(
                painter,
                pos2(text_x, cy),
                Align2::LEFT_CENTER,
                &text,
                Type::Caption,
                Role::TextSecondary,
                x1 - text_x,
            );
        }
    }
    false
}

/// What the user picked in the colour label menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelMenuAction {
    None,
    Set(Option<ColourLabel>),
    Rename,
}

/// The colour label menu's width from the board.
pub const LABEL_MENU_WIDTH: f32 = 200.0;

/// The colour label menu at `origin`: `None`, the five labels with their names (always
/// shown, so colour is never the only cue), a check on the current one, and `Rename
/// labels…`. Returns the choice and the menu's rectangle.
pub fn label_menu(
    ui: &mut Ui,
    origin: Pos2,
    current: Option<ColourLabel>,
    pool: &MediaPool,
) -> (LabelMenuAction, Rect) {
    let paint = Paint::new(ui.ctx());
    let rows = 1 + ColourLabel::ALL.len() + 1;
    let rect = Rect::from_min_size(
        origin,
        vec2(LABEL_MENU_WIDTH, 4.0 + rows as f32 * size::ROW + 1.0 + 4.0),
    );
    let painter = ui.painter().clone();
    paint.floating(&painter, rect);
    let mut action = LabelMenuAction::None;
    let mut y = rect.min.y + 4.0;
    let choices = std::iter::once(None).chain(ColourLabel::ALL.into_iter().map(Some));
    for choice in choices {
        let row = Rect::from_min_size(
            pos2(rect.min.x + 4.0, y),
            vec2(rect.width() - 8.0, size::ROW),
        );
        y += size::ROW;
        let response = ui.interact(
            row,
            ui.id().with(("pool.label_menu", choice.map(|l| l.key()))),
            Sense::click(),
        );
        let on = choice == current;
        if on {
            paint.glass_fill(&painter, row, paint.control_radius());
        } else if response.hovered() {
            painter.rect_filled(row, paint.control_radius(), paint.c(Role::Bg3));
        }
        let dot = pos2(row.min.x + 8.0 + 5.0, row.center().y);
        let text = match choice {
            None => {
                painter.circle_stroke(dot, 4.5, Stroke::new(1.0, paint.c(Role::BorderStrong)));
                t!("pool.label.none")
            }
            Some(label) => {
                painter.circle_filled(dot, 5.0, paint.c(label_role(label)));
                match pool.label_name(label) {
                    Some(name) => t!("pool.label.named", label = label_text(label), name = name),
                    None => label_text(label),
                }
            }
        };
        paint.text(
            &painter,
            pos2(row.min.x + 8.0 + 10.0 + 8.0, row.center().y),
            Align2::LEFT_CENTER,
            &text,
            Type::Label,
            if on {
                Role::TextPrimary
            } else {
                Role::TextSecondary
            },
            row.width() - 60.0,
        );
        if on {
            icon(
                &painter,
                Rect::from_center_size(
                    pos2(row.max.x - 8.0 - 6.0, row.center().y),
                    vec2(12.0, 12.0),
                ),
                Icon::Check,
                paint.c(Role::TextPrimary),
                2.8,
            );
        }
        if response.clicked() {
            action = LabelMenuAction::Set(choice);
        }
    }
    painter.hline(
        rect.min.x + 4.0..=rect.max.x - 4.0,
        y + 0.5,
        hairline(&paint),
    );
    y += 1.0;
    let row = Rect::from_min_size(
        pos2(rect.min.x + 4.0, y),
        vec2(rect.width() - 8.0, size::ROW),
    );
    let response = ui.interact(row, ui.id().with("pool.label_menu.rename"), Sense::click());
    if response.hovered() {
        painter.rect_filled(row, paint.control_radius(), paint.c(Role::Bg3));
    }
    paint.text(
        &painter,
        pos2(row.min.x + 8.0, row.center().y),
        Align2::LEFT_CENTER,
        &t!("pool.label.rename"),
        Type::Label,
        Role::TextSecondary,
        row.width() - 16.0,
    );
    if response.clicked() {
        action = LabelMenuAction::Rename;
    }
    (action, rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fits_the_board() {
        // The docked board: 400 wide, bins 148, so 252 for clips: two tiles of 114.
        let g = GridLayout::for_width(400.0 - layout::BINS_WIDTH);
        assert_eq!(g.columns, 2);
        assert_eq!(g.tile_width, 114.0);
        assert_eq!(g.thumb_height, 64.0);
        assert_eq!(g.rows(9), 5);
        assert_eq!(GridLayout::for_width(40.0).columns, 1);
        assert_eq!(GridLayout::for_width(1000.0).columns, 8);
        assert_eq!(visible_rows(100, 28.0, 0.0, 280.0), (0, 10));
        assert_eq!(visible_rows(100, 28.0, 30.0, 280.0), (1, 12));
        assert_eq!(visible_rows(5, 28.0, 0.0, 280.0), (0, 5));
    }

    #[test]
    fn list_columns_match_the_board() {
        // The board's list is 888 wide: name gets what the fixed columns leave.
        let c = list_columns(0.0, 888.0);
        assert_eq!(c[0], (8.0, 24.0));
        assert_eq!(c[1].0, 32.0);
        assert_eq!(c[8].1, 880.0);
        assert_eq!(c[2].1 - c[2].0, 64.0);
    }

    #[test]
    fn rows_from_the_pool() {
        let mut pool = MediaPool::new("Harbour film");
        let root = pool.root();
        let selects = pool.add_bin(root, "Selects").unwrap();
        let day2 = pool.add_bin(root, "Day 2").unwrap();
        let a_cam = pool.add_bin(day2, "A-cam").unwrap();
        let mut item = MediaItem::new("Harbour at dusk", MediaKind::Video, "/a.mov");
        item.facts = Facts {
            duration_s: Some(12.2),
            width: 3840,
            height: 2160,
            frame_rate: Some((24000, 1001)),
            video_codec: Some("prores".into()),
            video_profile: Some("HQ".into()),
            bit_depth: 10,
            ..Facts::default()
        };
        item.metadata.camera_model = Some("ILME-FX6V".into());
        item.metadata.focal_length_mm = Some(35.0);
        let id = pool.add_item(selects, item).unwrap();

        let rows = bin_rows(&pool, &BTreeSet::new());
        let names: Vec<(&str, usize)> = rows.iter().map(|r| (r.name.as_str(), r.depth)).collect();
        assert_eq!(names, [("Harbour film", 0), ("Day 2", 1), ("Selects", 1)]);
        assert!(rows[1].has_children && !rows[1].expanded);
        assert_eq!(rows[2].count, 1);
        let open = bin_rows(&pool, &BTreeSet::from([day2]));
        assert_eq!(open[2].id, a_cam);
        assert_eq!(open[2].depth, 2);

        let row = ItemRow::from_item(pool.item(id).unwrap(), true);
        assert_eq!(row.codec, "ProRes 422 HQ");
        assert_eq!(row.camera, "ILME-FX6V · 35 mm");
        assert_eq!(ItemStatus::of(&row), ItemStatus::Offline);
        assert_eq!(grid_meta(&row).1, Role::StateError);
        let online = ItemRow {
            offline: false,
            analysing: Some(0.64),
            proxy: ProxyState::Ready,
            ..row.clone()
        };
        assert_eq!(ItemStatus::of(&online), ItemStatus::Analysing(0.64));
        assert_eq!(grid_meta(&online).0, "4K · 23.976");

        let hevc = Facts {
            video_codec: Some("hevc".into()),
            bit_depth: 10,
            ..Facts::default()
        };
        assert_eq!(codec_label(&hevc), "HEVC 10-bit");
        let h264 = Facts {
            video_codec: Some("h264".into()),
            bit_depth: 8,
            ..Facts::default()
        };
        assert_eq!(codec_label(&h264), "H.264");
    }
}
