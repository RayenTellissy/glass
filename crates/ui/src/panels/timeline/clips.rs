//! The clips in the timeline's track lanes, drawn over [`super::view::lanes`] from the
//! project and the analysis cache.
//!
//! Each item is a rounded block in its track's clip colour (or its colour label's,
//! Decision 26) with a name bar on top: the clip's name (a nested sequence's name, else
//! the media file's name), cut with an ellipsis and kept on screen when the clip starts
//! left of the view, and badges on the bar's far side: the effect count when the stack
//! is not empty, the speed when it is not 100%, a stacked-frames mark for a nested
//! sequence or compound clip, and a diamond when anything on the clip is keyframed. In
//! a right-to-left locale the name sits at the bar's right and the badges at its left;
//! time still runs left to right.
//!
//! Below the bar, video clips show their thumbnail strip and audio clips their
//! waveform, both read from [`ClipMedia`] and never waited for: what is missing is a
//! placeholder this frame and a request to the analysis feed.
//!
//! - **Thumbnails** tile the clip from its start, one per thumbnail width (the strip's
//!   aspect ratio at the body's height), so they do not swim while scrolling. Each tile
//!   shows the source time at its left edge, `source_start + offset × speed`
//!   ([`thumb_slots`]), from the coarsest strip level whose interval still gives every
//!   tile its own picture ([`thumb_level`]). Only the tiles on screen are looked up, and
//!   only their atlases are asked for.
//! - **Waveforms** draw one column per point from the peak level with at most as many
//!   samples per peak as the column covers ([`samples_per_point`],
//!   [`Peaks::level_for`]), scaled by the clip's gain (its curve when keyframed),
//!   clipped at full scale; mono sources get one lane, stereo and wider two (left and
//!   right).
//!
//! Disabled clips are dimmed to the `opacity.disabled` token. Transitions are glass
//! blocks over their span, so their alignment (centred, starting or ending at the cut)
//! reads from where the block sits against the cut line. Selection is a hook
//! ([`ClipHooks::is_selected`]) drawn as the accent outline.
//!
//! **Cost.** Each track is binary-searched for the first item in the visible range and
//! walked to its end, so off-screen clips cost nothing. Clips narrower than
//! [`DETAIL_WIDTH`], or every clip below [`BLOCK_ZOOM`], are plain blocks, and touching
//! blocks of one colour merge into one rectangle, so a whole sequence zoomed out is at
//! most a few shapes per point of width. Everything is collected into a handful of
//! shape lists and meshes (one per atlas texture, one for all waveforms) and handed to
//! the painter once.

use std::borrow::Cow;
use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, Mesh, Rangef, Rect, Shape, Stroke, StrokeKind, TextureId, Ui,
    pos2, vec2,
};
use engine::media::analysis::peaks::Peaks;
use engine::media::analysis::thumbnails::ThumbnailLevel;
use project::{
    ClipParam, ClipSource, ColourLabel, Item, ItemId, ItemKind, Project, Rational, Sequence, Time,
    TrackKind,
};

use super::clip_media::{AtlasKey, ClipMedia};
use super::view::{TimelineView, TrackLayout};
use crate::panels::media_pool::label_role;
use crate::panels::widgets::{Paint, Type};
use crate::t;
use crate::theme::{Role, TEXTURE_TINT};

/// The name bar's height.
pub const NAME_BAR: f32 = 16.0;
/// Narrower clips are plain blocks.
pub const DETAIL_WIDTH: f32 = 12.0;
/// Points per second below which every clip is a plain block.
pub const BLOCK_ZOOM: f64 = 0.5;
/// The least room a name gets before badges are dropped.
pub const NAME_MIN_WIDTH: f32 = 24.0;
/// The least body height that shows thumbnails or a waveform.
pub const MIN_BODY: f32 = 10.0;

/// What the clips are drawn from.
#[derive(Clone, Copy)]
pub struct ClipsView<'a> {
    /// For media names and nested sequence names.
    pub project: &'a Project,
    pub sequence: &'a Sequence,
    /// Name at the bar's right, badges at its left.
    pub rtl: bool,
}

impl<'a> ClipsView<'a> {
    /// The current locale's direction.
    pub fn new(project: &'a Project, sequence: &'a Sequence) -> Self {
        Self {
            project,
            sequence,
            rtl: assets::i18n::current().is_rtl(),
        }
    }
}

/// What other timeline parts tell the clip renderer per item. Every method has a
/// default, so `&()` draws with nothing selected and no labels.
pub trait ClipHooks {
    /// Drawn with the accent outline.
    fn is_selected(&self, _item: ItemId) -> bool {
        false
    }

    /// A colour label replacing the track kind's clip colour.
    fn label(&self, _item: &Item) -> Option<ColourLabel> {
        None
    }
}

impl ClipHooks for () {}

/// What one call drew, for tests and the debug overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClipStats {
    /// Items overlapping the visible range on visible rows.
    pub items: usize,
    /// Items drawn with their name, contents and badges.
    pub detailed: usize,
    /// Plain-block rectangles after merging.
    pub blocks: usize,
    /// Thumbnail tiles drawn from an atlas.
    pub thumbnails: usize,
    /// Thumbnail tiles or waveforms still loading.
    pub placeholders: usize,
    /// Waveform columns.
    pub wave_columns: usize,
    pub transitions: usize,
}

/// A thumbnail tile: its left edge in points from the clip's start, and the source
/// time it shows, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThumbSlot {
    pub index: usize,
    pub x: f32,
    pub source_seconds: f64,
}

/// The thumbnail tiles of a clip `clip_width` points wide starting at screen x
/// `clip_x`, `tile` points apart, that overlap the screen span `visible`. A tile's
/// source time is `source_start + (x / px_per_second) × speed`.
pub fn thumb_slots(
    clip_x: f32,
    clip_width: f32,
    visible: Rangef,
    tile: f32,
    source_start: f64,
    speed: f64,
    px_per_second: f64,
) -> impl Iterator<Item = ThumbSlot> {
    let valid = tile > 0.0 && clip_width > 0.0 && px_per_second > 0.0;
    let count = if valid {
        (clip_width / tile).ceil() as usize
    } else {
        0
    };
    let first = if valid {
        ((visible.min - clip_x) / tile).floor().max(0.0) as usize
    } else {
        0
    };
    let last = if valid {
        (((visible.max - clip_x) / tile).ceil().max(0.0) as usize).min(count)
    } else {
        0
    };
    (first..last).map(move |index| {
        let x = index as f32 * tile;
        ThumbSlot {
            index,
            x,
            source_seconds: source_start + f64::from(x) / px_per_second * speed,
        }
    })
}

/// The strip level for tiles each covering `slot_seconds` of source: the coarsest one
/// whose interval is at most that (each tile its own picture, fewest atlases), else the
/// finest there is. `None` when no level is loaded.
pub fn thumb_level(levels: &[ThumbnailLevel], slot_seconds: f64) -> Option<usize> {
    let fits = levels
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.is_empty() && f64::from(l.interval) <= slot_seconds)
        .max_by_key(|(_, l)| l.interval);
    let finest = || {
        levels
            .iter()
            .enumerate()
            .filter(|(_, l)| !l.is_empty())
            .min_by_key(|(_, l)| l.interval)
    };
    fits.or_else(finest).map(|(i, _)| i)
}

/// Source samples (at `sample_rate`) one point of a clip playing at `speed` covers.
pub fn samples_per_point(sample_rate: u32, speed: f64, px_per_second: f64) -> f64 {
    if px_per_second <= 0.0 {
        return f64::INFINITY;
    }
    f64::from(sample_rate) * speed / px_per_second
}

/// The peak level a waveform column of one point uses at this zoom and speed.
pub fn waveform_level(peaks: &Peaks, speed: f64, px_per_second: f64) -> usize {
    peaks.level_for(samples_per_point(peaks.sample_rate, speed, px_per_second))
}

/// A clip's speed as a float (a ramp's first key).
pub fn clip_speed(speed: &project::SpeedMap) -> f64 {
    speed
        .keys
        .first()
        .and_then(|k| k.speed.to_f64())
        .unwrap_or(1.0)
}

/// The speed badge's text: `None` at 100%.
pub fn speed_text(speed: &project::SpeedMap) -> Option<String> {
    match speed.constant_speed() {
        Some(r) if r == Rational::ONE => None,
        Some(r) => {
            let percent = f64::from(r.num) * 100.0 / f64::from(r.den);
            let percent = if (percent - percent.round()).abs() < 1e-9 {
                format!("{}", percent.round() as i64)
            } else {
                format!("{percent:.1}")
            };
            Some(t!("timeline.clip.speed", percent = percent))
        }
        None => Some(t!("timeline.clip.speed_ramp")),
    }
}

/// The name an item shows: a nested sequence's name, the media file's name, a
/// subtitle's text.
pub fn item_name<'a>(project: &'a Project, item: &'a Item) -> Cow<'a, str> {
    match &item.kind {
        ItemKind::Clip(clip) => match clip.source {
            ClipSource::Sequence(s) => project.sequence(s).map_or_else(
                || Cow::Owned(t!("timeline.clip.missing")),
                |s| Cow::Borrowed(s.name.as_str()),
            ),
            ClipSource::Media(m) => match project.media().get(m) {
                Some(r) => std::path::Path::new(&r.path)
                    .file_name()
                    .map_or(Cow::Borrowed(r.path.as_str()), |n| n.to_string_lossy()),
                None => Cow::Owned(t!("timeline.clip.missing")),
            },
        },
        ItemKind::Subtitle(s) => Cow::Borrowed(s.text.as_str()),
        ItemKind::Adjustment(_) => Cow::Owned(t!("timeline.clip.adjustment")),
    }
}

/// Whether anything on the item is keyframed.
pub fn is_animated(item: &Item) -> bool {
    item.clip().is_some_and(|c| {
        !c.keyframes.is_empty() || c.effects.iter().any(|e| !e.keyframes.is_empty())
    })
}

fn kind_role(kind: TrackKind) -> Role {
    match kind {
        TrackKind::Video => Role::ClipVideo,
        TrackKind::Audio => Role::ClipAudio,
        TrackKind::Subtitle => Role::ClipSubtitle,
        TrackKind::Adjustment => Role::ClipAdjustment,
    }
}

/// The shapes of one frame, painted in layers once everything is collected.
#[derive(Default)]
struct Batch {
    bodies: Vec<Shape>,
    images: Vec<Mesh>,
    wave: Mesh,
    fronts: Vec<Shape>,
    tops: Vec<Shape>,
}

impl Batch {
    fn image(&mut self, texture: TextureId) -> &mut Mesh {
        let i = match self.images.iter().position(|m| m.texture_id == texture) {
            Some(i) => i,
            None => {
                self.images.push(Mesh::with_texture(texture));
                self.images.len() - 1
            }
        };
        &mut self.images[i]
    }
}

/// A run of touching plain blocks of one colour.
struct Block {
    rect: Rect,
    color: Color32,
}

struct Ctx<'a> {
    paint: Paint,
    painter: egui::Painter,
    view: &'a TimelineView,
    clips: &'a ClipsView<'a>,
    hooks: &'a dyn ClipHooks,
    area: Rect,
    px_per_second: f64,
    radius: CornerRadius,
    pad: f32,
    disabled: f32,
    outline: f32,
}

/// Draws the clips of `clips.sequence` in the track area `rect` (the lanes' rect),
/// rows laid out by `layout` and scrolled and zoomed by `view`.
pub fn clips(
    ui: &Ui,
    rect: Rect,
    view: &TimelineView,
    layout: &TrackLayout,
    clips: &ClipsView<'_>,
    media: &mut ClipMedia,
    hooks: &dyn ClipHooks,
) -> ClipStats {
    let paint = Paint::new(ui.ctx());
    let tokens = paint.theme.tokens();
    let (radius, pad, disabled, outline) = (
        CornerRadius::same(tokens.radius.control.round() as u8),
        tokens.spacing.s4,
        tokens.opacity.disabled,
        tokens.stroke.selected_clip_outline,
    );
    let cx = Ctx {
        radius,
        pad,
        disabled,
        outline,
        painter: ui.painter_at(rect),
        paint,
        view,
        clips,
        hooks,
        area: rect,
        px_per_second: view.px_per_second(),
    };
    let mut stats = ClipStats::default();
    let mut batch = Batch::default();
    let visible = view.visible_range();
    let (vis_start, vis_end) = (visible.start, visible.end().unwrap_or(visible.start));
    let mut on_screen: Vec<(ItemId, Rect)> = Vec::new();
    for (track_id, _) in layout.iter() {
        let Some(span) = view.track_to_y(layout, track_id) else {
            continue;
        };
        let row = Rangef::new(rect.top() + span.min, rect.top() + span.max);
        if row.max <= rect.top() || row.min >= rect.bottom() {
            continue;
        }
        let Some(track) = clips.sequence.track(track_id) else {
            continue;
        };
        // Items are ordered and never overlap, so their ends are ordered too.
        let first = match track.items.binary_search_by(|i| {
            if i.range.end().is_ok_and(|e| e <= vis_start) {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        }) {
            Ok(i) | Err(i) => i,
        };
        on_screen.clear();
        let mut block: Option<Block> = None;
        let body_rows = Rangef::new(row.min + 1.0, row.max - 2.0);
        for index in first..track.items.len() {
            let Some(item) = track.items.get(index) else {
                break;
            };
            if item.range.start >= vis_end {
                break;
            }
            let Ok(end) = item.range.end() else {
                continue;
            };
            stats.items += 1;
            let x0 = rect.left() + view.time_to_x(item.range.start);
            let x1 = rect.left() + view.time_to_x(end);
            let full = Rect::from_x_y_ranges(x0..=x1, body_rows);
            let role = cx.role(track.kind, item);
            let enabled = item.clip().is_none_or(|c| c.enabled);
            if full.width() < DETAIL_WIDTH || cx.px_per_second < BLOCK_ZOOM {
                let mut color = cx.paint.c(role);
                if !enabled {
                    color = color.gamma_multiply(cx.disabled);
                }
                let rect = Rect::from_x_y_ranges(x0..=x1.max(x0 + 1.0), body_rows);
                match &mut block {
                    Some(b) if b.color == color && rect.min.x <= b.rect.max.x + 1.0 => {
                        b.rect.max.x = b.rect.max.x.max(rect.max.x);
                    }
                    _ => {
                        if let Some(b) = block.replace(Block { rect, color }) {
                            batch.bodies.push(Shape::rect_filled(b.rect, 0.0, b.color));
                            stats.blocks += 1;
                        }
                    }
                }
                on_screen.push((item.id, full));
                continue;
            }
            stats.detailed += 1;
            on_screen.push((item.id, full));
            cx.detailed(&mut batch, &mut stats, media, track.kind, item, full, role);
        }
        if let Some(b) = block {
            batch.bodies.push(Shape::rect_filled(b.rect, 0.0, b.color));
            stats.blocks += 1;
        }
        if !track.transitions.is_empty() {
            cx.transitions(&mut batch, &mut stats, track, &on_screen, body_rows);
        }
    }
    let Batch {
        bodies,
        images,
        wave,
        fronts,
        tops,
    } = batch;
    cx.painter.extend(bodies);
    cx.painter.extend(
        images
            .into_iter()
            .filter(|m| !m.is_empty())
            .map(Shape::mesh),
    );
    if !wave.is_empty() {
        cx.painter.add(Shape::mesh(wave));
    }
    cx.painter.extend(fronts);
    cx.painter.extend(tops);
    stats
}

impl Ctx<'_> {
    fn role(&self, kind: TrackKind, item: &Item) -> Role {
        if let Some(clip) = item.clip()
            && let ClipSource::Media(m) = clip.source
            && self.clips.project.media().get(m).is_none()
        {
            return Role::ClipOffline;
        }
        self.hooks
            .label(item)
            .map_or_else(|| kind_role(kind), label_role)
    }

    #[allow(clippy::too_many_arguments)]
    fn detailed(
        &self,
        batch: &mut Batch,
        stats: &mut ClipStats,
        media: &mut ClipMedia,
        kind: TrackKind,
        item: &Item,
        full: Rect,
        role: Role,
    ) {
        let p = &self.paint;
        let selected = self.hooks.is_selected(item.id);
        let clip = item.clip();
        let enabled = clip.is_none_or(|c| c.enabled);
        // The part of the clip on screen, for the name and the contents.
        let shown = Rangef::new(
            full.left().max(self.area.left()),
            full.right().min(self.area.right()),
        );
        let has_body = full.height() >= NAME_BAR + MIN_BODY;
        let bar = if has_body {
            Rect::from_min_max(full.min, pos2(full.right(), full.top() + NAME_BAR))
        } else {
            full
        };
        let body = Rect::from_min_max(pos2(full.left(), bar.bottom()), full.max);
        let radius = if full.width() < 3.0 * self.radius.nw as f32 {
            CornerRadius::ZERO
        } else {
            self.radius
        };
        batch
            .bodies
            .push(Shape::rect_filled(full, radius, p.mix(role, 0.3)));

        if has_body && let Some(clip) = clip {
            match (kind, clip.source) {
                (TrackKind::Video, ClipSource::Media(m)) => {
                    self.thumbnails(batch, stats, media, m, clip, full, body, shown);
                }
                (TrackKind::Audio, ClipSource::Media(m)) => {
                    self.waveform(batch, stats, media, m, clip, full, body, shown, role);
                }
                _ => {}
            }
        }

        // Name bar.
        let bar_radius = if has_body {
            CornerRadius {
                sw: 0,
                se: 0,
                ..radius
            }
        } else {
            radius
        };
        batch
            .fronts
            .push(Shape::rect_filled(bar, bar_radius, p.c(role)));
        self.name_and_badges(batch, item, bar, shown);

        let stroke = if selected {
            Stroke::new(self.outline, p.c(Role::Accent))
        } else {
            Stroke::new(1.0, p.c(Role::BorderSubtle))
        };
        batch
            .fronts
            .push(Shape::rect_stroke(full, radius, stroke, StrokeKind::Inside));
        if !enabled {
            batch.fronts.push(Shape::rect_filled(
                full,
                radius,
                p.mix(Role::Bg0, 1.0 - self.disabled),
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn thumbnails(
        &self,
        batch: &mut Batch,
        stats: &mut ClipStats,
        media: &mut ClipMedia,
        id: project::MediaId,
        clip: &project::Clip,
        full: Rect,
        body: Rect,
        shown: Rangef,
    ) {
        let Some(strips) = media.strips(id) else {
            stats.placeholders += 1;
            return;
        };
        if strips.height == 0 || strips.width == 0 {
            return;
        }
        let tile = body.height() * strips.width as f32 / strips.height as f32;
        let speed = clip_speed(&clip.speed);
        let slot_seconds = f64::from(tile) / self.px_per_second * speed;
        let Some(level) = thumb_level(&strips.levels, slot_seconds) else {
            stats.placeholders += 1;
            return;
        };
        let lvl = &strips.levels[level];
        let per_atlas = strips.per_atlas.max(1) as usize;
        let columns = strips.columns.max(1);
        let source_start = clip.source_start.as_secs_f64();
        let slots = thumb_slots(
            full.left(),
            full.width(),
            shown,
            tile,
            source_start,
            speed,
            self.px_per_second,
        );
        for slot in slots {
            let index = lvl.index_at(slot.source_seconds);
            let chunk = (index / per_atlas) as u32;
            let within = (index % per_atlas) as u32;
            let left = full.left() + slot.x;
            let right = (left + tile).min(full.right());
            let cell = Rect::from_x_y_ranges(left..=right, body.y_range());
            let Some((texture, size)) = media.atlas(
                id,
                AtlasKey {
                    level: level as u32,
                    chunk,
                },
            ) else {
                stats.placeholders += 1;
                continue;
            };
            let (aw, ah) = (size[0].max(1) as f32, size[1].max(1) as f32);
            let u0 = ((within % columns) * strips.width) as f32 / aw;
            let v0 = ((within / columns) * strips.height) as f32 / ah;
            let du = strips.width as f32 / aw * (cell.width() / tile);
            let dv = strips.height as f32 / ah;
            let uv = Rect::from_min_size(pos2(u0, v0), vec2(du, dv));
            batch
                .image(texture)
                .add_rect_with_uv(cell, uv, TEXTURE_TINT);
            stats.thumbnails += 1;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn waveform(
        &self,
        batch: &mut Batch,
        stats: &mut ClipStats,
        media: &mut ClipMedia,
        id: project::MediaId,
        clip: &project::Clip,
        full: Rect,
        body: Rect,
        shown: Rangef,
        role: Role,
    ) {
        let p = &self.paint;
        let Some(peaks) = media.peaks(id) else {
            stats.placeholders += 1;
            batch.bodies.push(Shape::hline(
                shown,
                body.center().y,
                Stroke::new(1.0, p.mix(role, 0.6)),
            ));
            return;
        };
        let speed = clip_speed(&clip.speed);
        let level = waveform_level(&peaks, speed, self.px_per_second);
        let lanes = if peaks.channels >= 2 { 2 } else { 1 };
        let rate = f64::from(peaks.sample_rate);
        let source_start = clip.source_start.as_secs_f64();
        let animated_gain = clip.keyframes.contains_key(&ClipParam::GainDb);
        let constant_gain = db_to_gain(clip.gain_db);
        let lane_h = body.height() / lanes as f32;
        let peak_color = p.c(role);
        let rms_color = p.mix(Role::TextPrimary, 0.35);
        let seconds_per_point = 1.0 / self.px_per_second;
        let mut x = shown.min.floor().max(full.left());
        while x < shown.max {
            let t0 = f64::from(x - full.left()) * seconds_per_point;
            let s0 = (source_start + t0 * speed) * rate;
            let s1 = (source_start + (t0 + seconds_per_point) * speed) * rate;
            let (a, b) = (s0.max(0.0) as u64, s1.max(0.0) as u64);
            let slice = peaks.peaks(level, a..b.max(a + 1));
            let gain = if animated_gain {
                let at = item_source_time(clip.source_start, t0 * speed);
                db_to_gain(clip.gain_db_at(at))
            } else {
                constant_gain
            };
            let col_right = (x + 1.0).min(full.right());
            for lane in 0..lanes {
                let (mut lo, mut hi, mut rms) = (0.0f32, 0.0f32, 0.0f32);
                for i in 0..slice.len() {
                    if let Some(peak) = slice.get(i, lane) {
                        lo = lo.min(peak.min_f32());
                        hi = hi.max(peak.max_f32());
                        rms = rms.max(peak.rms_f32());
                    }
                }
                let half = lane_h / 2.0 - 1.0;
                let centre = body.top() + lane_h * (lane as f32 + 0.5);
                let scale = |v: f32| (v * gain).clamp(-1.0, 1.0) * half;
                let top = centre - scale(hi).max(0.5);
                let bottom = centre - scale(lo).min(-0.5);
                batch.wave.add_colored_rect(
                    Rect::from_x_y_ranges(x..=col_right, top..=bottom),
                    peak_color,
                );
                let r = scale(rms);
                if r >= 0.5 {
                    batch.wave.add_colored_rect(
                        Rect::from_x_y_ranges(x..=col_right, centre - r..=centre + r),
                        rms_color,
                    );
                }
            }
            stats.wave_columns += 1;
            x += 1.0;
        }
    }

    fn name_and_badges(&self, batch: &mut Batch, item: &Item, bar: Rect, shown: Rangef) {
        let p = &self.paint;
        let pad = self.pad;
        let room = Rangef::new(
            shown.min.max(bar.left()) + pad,
            shown.max.min(bar.right()) - pad,
        );
        if room.span() < 8.0 {
            return;
        }
        let text_c = p.c(Role::TextInverse);
        let cy = bar.center().y;
        let mut badges: Vec<Badge> = Vec::new();
        if let Some(clip) = item.clip() {
            match clip.effects.len() {
                0 => {}
                1 => badges.push(Badge::Text(t!("timeline.clip.effect"))),
                n => badges.push(Badge::Text(t!("timeline.clip.effects", count = n))),
            }
            if let Some(s) = speed_text(&clip.speed) {
                badges.push(Badge::Text(s));
            }
            if matches!(clip.source, ClipSource::Sequence(_)) {
                badges.push(Badge::Nested);
            }
        }
        if is_animated(item) {
            badges.push(Badge::Keyframes);
        }
        let badge_h = (bar.height() - 4.0).clamp(8.0, 14.0);
        let mut laid: Vec<(f32, Badge, Option<Arc<egui::Galley>>)> = badges
            .into_iter()
            .map(|b| match &b {
                Badge::Text(s) => {
                    let g = p.galley(
                        &self.painter,
                        s,
                        Type::Caption,
                        p.c(Role::TextPrimary),
                        f32::INFINITY,
                    );
                    (g.size().x + 2.0 * self.pad, b, Some(g))
                }
                _ => (badge_h, b, None),
            })
            .collect();
        let gap = 2.0;
        let total = |l: &[(f32, Badge, Option<Arc<egui::Galley>>)]| -> f32 {
            l.iter().map(|(w, ..)| w + gap).sum()
        };
        while !laid.is_empty() && room.span() - total(&laid) < NAME_MIN_WIDTH {
            laid.pop();
        }
        let badges_w = total(&laid);
        // Badges from the far end inwards.
        let mut at = if self.clips.rtl { room.min } else { room.max };
        for (w, badge, galley) in laid {
            let r = if self.clips.rtl {
                let r = Rect::from_min_size(pos2(at, cy - badge_h / 2.0), vec2(w, badge_h));
                at += w + gap;
                r
            } else {
                let r = Rect::from_min_size(pos2(at - w, cy - badge_h / 2.0), vec2(w, badge_h));
                at -= w + gap;
                r
            };
            self.badge(batch, r, &badge, galley);
        }
        let name_w = room.span() - badges_w;
        if name_w < 8.0 {
            return;
        }
        let name = item_name(self.clips.project, item);
        let galley = p.galley(&self.painter, &name, Type::Caption, text_c, name_w);
        let size = galley.size();
        let anchor = if self.clips.rtl {
            Align2::RIGHT_CENTER.anchor_size(pos2(room.max, cy), size)
        } else {
            Align2::LEFT_CENTER.anchor_size(pos2(room.min, cy), size)
        };
        batch.fronts.push(Shape::galley(anchor.min, galley, text_c));
    }

    fn badge(&self, batch: &mut Batch, r: Rect, badge: &Badge, galley: Option<Arc<egui::Galley>>) {
        let p = &self.paint;
        batch.fronts.push(Shape::rect_filled(
            r,
            CornerRadius::same(3),
            p.mix(Role::Bg0, 0.55),
        ));
        let fg = p.c(Role::TextPrimary);
        match badge {
            Badge::Text(_) => {
                if let Some(g) = galley {
                    let pos = pos2(r.left() + self.pad, r.center().y - g.size().y / 2.0);
                    batch.fronts.push(Shape::galley(pos, g, fg));
                }
            }
            Badge::Keyframes => {
                let c = r.center();
                let s = r.height() * 0.3;
                batch.fronts.push(Shape::convex_polygon(
                    vec![
                        c + vec2(0.0, -s),
                        c + vec2(s, 0.0),
                        c + vec2(0.0, s),
                        c + vec2(-s, 0.0),
                    ],
                    fg,
                    Stroke::NONE,
                ));
            }
            Badge::Nested => {
                // Two stacked frames.
                let s = r.height() * 0.28;
                let c = r.center();
                let stroke = Stroke::new(1.0, fg);
                let back = Rect::from_center_size(c + vec2(1.5, -1.5), vec2(2.0 * s, 1.6 * s));
                let front = Rect::from_center_size(c + vec2(-1.5, 1.5), vec2(2.0 * s, 1.6 * s));
                batch
                    .fronts
                    .push(Shape::rect_stroke(back, 1.0, stroke, StrokeKind::Middle));
                batch
                    .fronts
                    .push(Shape::rect_filled(front, 1.0, p.mix(Role::Bg0, 0.55)));
                batch
                    .fronts
                    .push(Shape::rect_stroke(front, 1.0, stroke, StrokeKind::Middle));
            }
        }
    }

    fn transitions(
        &self,
        batch: &mut Batch,
        stats: &mut ClipStats,
        track: &project::Track,
        on_screen: &[(ItemId, Rect)],
        rows: Rangef,
    ) {
        let p = &self.paint;
        let Ok(grid) = self.clips.sequence.grid(track.kind) else {
            return;
        };
        let find = |id: Option<ItemId>| id.and_then(|id| on_screen.iter().find(|(i, _)| *i == id));
        let tall = rows.span() >= NAME_BAR + MIN_BODY;
        let top = if tall { rows.min + NAME_BAR } else { rows.min };
        for transition in &track.transitions {
            let out = find(transition.outgoing);
            let inc = find(transition.incoming);
            let cut = match (out, inc, transition.outgoing, transition.incoming) {
                (Some(_), _, Some(o), _) => track.item(o).and_then(|i| i.range.end().ok()),
                (_, Some(_), _, Some(i)) => track.item(i).map(|i| i.range.start),
                _ => None,
            };
            let Some(cut) = cut else {
                continue;
            };
            let Ok((before, after)) = transition.alignment.split(transition.duration, grid) else {
                continue;
            };
            let (Ok(a), Ok(b)) = (cut.checked_sub(before), cut.checked_add(after)) else {
                continue;
            };
            let x_cut = self.area.left() + self.view.time_to_x(cut);
            let xa = self.area.left() + self.view.time_to_x(a);
            let xb = self.area.left() + self.view.time_to_x(b);
            if xb < self.area.left() || xa > self.area.right() || xb - xa < 2.0 {
                continue;
            }
            let r = Rect::from_x_y_ranges(xa..=xb, top..=rows.max);
            let corner = CornerRadius::same(3);
            batch.tops.push(Shape::rect_filled(
                r,
                corner,
                p.mix(Role::Bg3, self.paint.theme.tokens().opacity.glass),
            ));
            let inner = r.shrink(2.0);
            if inner.width() > 4.0 && inner.height() > 4.0 {
                batch.tops.push(Shape::line_segment(
                    [inner.left_bottom(), inner.right_top()],
                    Stroke::new(1.0, p.c(Role::TextSecondary)),
                ));
            }
            batch.tops.push(Shape::vline(
                x_cut,
                r.y_range(),
                Stroke::new(1.0, p.c(Role::BorderStrong)),
            ));
            batch.tops.push(Shape::rect_stroke(
                r,
                corner,
                Stroke::new(1.0, p.c(Role::GlassEdge)),
                StrokeKind::Inside,
            ));
            stats.transitions += 1;
        }
    }
}

enum Badge {
    Text(String),
    Keyframes,
    Nested,
}

fn db_to_gain(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

/// Source time `offset` seconds after `start`, to the microsecond.
fn item_source_time(start: Time, offset: f64) -> Time {
    let us = (offset * 1e6).round() as i64;
    Time::new(us, 1_000_000)
        .and_then(|o| start.checked_add(o))
        .unwrap_or(start)
}
