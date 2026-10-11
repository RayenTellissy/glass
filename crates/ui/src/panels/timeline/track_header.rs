//! The timeline's track headers: the column left of the tracks (right of them in a
//! right-to-left locale) with each track's kind stripe, name, switches and height.
//!
//! Audio tracks have mute and solo, the other kinds a visibility switch; every track has
//! a lock and a height preset. While any audio track is soloed, the audio tracks that
//! are not are silent too: their mute switch shows that implied state and their name
//! dims. A locked track refuses every switch but its lock and its height (which only
//! changes the view), and cannot be renamed.
//!
//! [`track_headers`] draws a [`TrackHeaderView`] and returns a [`TrackHeaderAction`]; it
//! never changes the project. A rename is applied by the caller with
//! [`project::Project::rename_track`] (undo label [`project::EditLabel::RenameTrack`]).
//! Mute, solo, lock, visibility and height are not in the project file yet (that needs a
//! schema version), so they live in [`TrackHeaderState`], which applies those actions
//! and builds the rows from a sequence.
//!
//! Keyboard: Tab walks each row, then its switches in drawing order; Enter or Space
//! presses the focused one; F2 on a row (or a double click on its name) starts renaming,
//! Enter keeps the new name and Escape drops it.

use std::collections::HashMap;

use egui::{
    Align2, CornerRadius, Id, Key, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo,
    WidgetType, pos2, vec2,
};
use project::{Sequence, TrackId, TrackKind};

use crate::panels::widgets::{Icon, Paint, Type, icon, size};
use crate::t;
use crate::theme::Role;

/// The header column's width.
pub const HEADER_WIDTH: f32 = 200.0;
/// The kind stripe on the leading edge.
const STRIPE: f32 = 3.0;
const BUTTON: f32 = size::SMALL_CONTROL;
const BUTTON_GAP: f32 = 2.0;
const TRAILING_PAD: f32 = 4.0;
/// The centre of the caption line under the name, from the row's top.
const CAPTION_Y: f32 = 30.0;

/// A track's height in the timeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HeightPreset {
    /// One control row: name and switches only.
    Small,
    /// Adds the caption line (kind, locked) under the name.
    #[default]
    Medium,
    /// Room for thumbnails and waveforms.
    Large,
}

impl HeightPreset {
    pub const ALL: [HeightPreset; 3] = [
        HeightPreset::Small,
        HeightPreset::Medium,
        HeightPreset::Large,
    ];

    /// The row height in points (Small is the `row/pro` token).
    pub fn height(self) -> f32 {
        match self {
            HeightPreset::Small => size::ROW,
            HeightPreset::Medium => 48.0,
            HeightPreset::Large => 72.0,
        }
    }

    /// The next size up, wrapping from Large to Small (the height button's click).
    pub fn next(self) -> Self {
        match self {
            HeightPreset::Small => HeightPreset::Medium,
            HeightPreset::Medium => HeightPreset::Large,
            HeightPreset::Large => HeightPreset::Small,
        }
    }

    pub fn label(self) -> String {
        match self {
            HeightPreset::Small => t!("track_header.height_small"),
            HeightPreset::Medium => t!("track_header.height_medium"),
            HeightPreset::Large => t!("track_header.height_large"),
        }
    }
}

/// A track's switches and height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrackFlags {
    pub muted: bool,
    pub soloed: bool,
    pub locked: bool,
    pub hidden: bool,
    pub height: HeightPreset,
}

/// Whether an audio track is heard: not muted, and soloed whenever any track is.
pub fn audible(muted: bool, soloed: bool, any_solo: bool) -> bool {
    !muted && (soloed || !any_solo)
}

/// One header's content.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackRow {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub flags: TrackFlags,
    /// The track edits go to (drawn with the selected-row fill).
    pub targeted: bool,
}

/// The parts of a header, in keyboard focus order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    /// The row itself: target on click, rename on double click or F2.
    Row,
    /// The name field while renaming.
    Name,
    Mute,
    Solo,
    Visibility,
    Lock,
    Height,
}

impl Part {
    /// The switches a track of `kind` shows, leading to trailing.
    pub fn switches(kind: TrackKind) -> &'static [Part] {
        match kind {
            TrackKind::Audio => &[Part::Mute, Part::Solo, Part::Lock, Part::Height],
            _ => &[Part::Visibility, Part::Lock, Part::Height],
        }
    }
}

/// The egui id of a header part, for focus and tests.
pub fn part_id(track: TrackId, part: Part) -> Id {
    Id::new(("glass.track_header", track.0, part))
}

/// An inline rename in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    pub track: TrackId,
    pub text: String,
}

/// What the column shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackHeaderView {
    /// Top to bottom, as in the sequence.
    pub rows: Vec<TrackRow>,
    pub renaming: Option<Rename>,
    /// Points scrolled down, kept in step with the tracks.
    pub scroll: f32,
    /// Mirror the column for a right-to-left locale.
    pub rtl: bool,
    /// Drawn hovered whatever the pointer does (snapshots).
    pub hover: Option<(TrackId, Part)>,
    /// Drawn pressed whatever the pointer does (snapshots).
    pub pressed: Option<(TrackId, Part)>,
}

impl TrackHeaderView {
    /// Rows in the current locale's direction.
    pub fn new(rows: Vec<TrackRow>) -> Self {
        Self {
            rows,
            rtl: assets::i18n::current().is_rtl(),
            ..Self::default()
        }
    }

    /// Whether any audio track is soloed.
    pub fn any_solo(&self) -> bool {
        self.rows
            .iter()
            .any(|r| r.kind == TrackKind::Audio && r.flags.soloed)
    }

    /// The total height of the rows.
    pub fn content_height(&self) -> f32 {
        self.rows.iter().map(|r| r.flags.height.height()).sum()
    }

    /// Where `part` of `track`'s header sits in a column drawn in `rect`, if shown
    /// (the name only while renaming that track; rows scrolled out are still placed).
    pub fn part_rect(&self, rect: Rect, track: TrackId, part: Part) -> Option<Rect> {
        let mut top = rect.top() - self.scroll;
        for row in &self.rows {
            let h = row.flags.height.height();
            if row.id == track {
                let layout = RowLayout::new(
                    Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), h)),
                    row.kind,
                    self.rtl,
                );
                return match part {
                    Part::Row => Some(layout.row),
                    Part::Name => self
                        .renaming
                        .as_ref()
                        .is_some_and(|r| r.track == track)
                        .then_some(layout.name),
                    _ => layout.switch(part),
                };
            }
            top += h;
        }
        None
    }
}

/// What the user did in the headers this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackHeaderAction {
    None,
    /// Make this the track edits go to.
    Target(TrackId),
    /// Apply with [`project::Project::rename_track`]; the name is trimmed and not empty.
    Rename {
        track: TrackId,
        name: String,
    },
    ToggleMute(TrackId),
    ToggleSolo(TrackId),
    ToggleVisible(TrackId),
    ToggleLock(TrackId),
    SetHeight {
        track: TrackId,
        height: HeightPreset,
    },
}

/// The switches and heights of a sequence's tracks, kept beside the project until the
/// project file stores them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackHeaderState {
    flags: HashMap<TrackId, TrackFlags>,
}

impl TrackHeaderState {
    pub fn flags(&self, track: TrackId) -> TrackFlags {
        self.flags.get(&track).copied().unwrap_or_default()
    }

    pub fn is_locked(&self, track: TrackId) -> bool {
        self.flags(track).locked
    }

    /// Applies a switch or height action; returns whether anything changed. Mute, solo
    /// and visibility are refused on a locked track. Target and rename are the caller's.
    pub fn apply(&mut self, action: &TrackHeaderAction) -> bool {
        let (track, switch) = match *action {
            TrackHeaderAction::ToggleMute(t)
            | TrackHeaderAction::ToggleSolo(t)
            | TrackHeaderAction::ToggleVisible(t) => (t, true),
            TrackHeaderAction::ToggleLock(t) | TrackHeaderAction::SetHeight { track: t, .. } => {
                (t, false)
            }
            TrackHeaderAction::None
            | TrackHeaderAction::Target(_)
            | TrackHeaderAction::Rename { .. } => return false,
        };
        if switch && self.is_locked(track) {
            return false;
        }
        let flags = self.flags.entry(track).or_default();
        let before = *flags;
        match *action {
            TrackHeaderAction::ToggleMute(_) => flags.muted = !flags.muted,
            TrackHeaderAction::ToggleSolo(_) => flags.soloed = !flags.soloed,
            TrackHeaderAction::ToggleVisible(_) => flags.hidden = !flags.hidden,
            TrackHeaderAction::ToggleLock(_) => flags.locked = !flags.locked,
            TrackHeaderAction::SetHeight { height, .. } => flags.height = height,
            _ => {}
        }
        *flags != before
    }

    /// Whether `track` of `sequence` is heard: audio, not muted, and soloed whenever any
    /// audio track of the sequence is.
    pub fn audible(&self, sequence: &Sequence, track: TrackId) -> bool {
        let Some(t) = sequence.tracks.iter().find(|t| t.id == track) else {
            return false;
        };
        let any_solo = sequence
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Audio && self.flags(t.id).soloed);
        let flags = self.flags(track);
        t.kind == TrackKind::Audio && audible(flags.muted, flags.soloed, any_solo)
    }

    /// The rows of `sequence`'s tracks, top first.
    pub fn rows(&self, sequence: &Sequence, targeted: Option<TrackId>) -> Vec<TrackRow> {
        sequence
            .tracks
            .iter()
            .map(|t| TrackRow {
                id: t.id,
                kind: t.kind,
                name: t.name.clone(),
                flags: self.flags(t.id),
                targeted: targeted == Some(t.id),
            })
            .collect()
    }

    /// Forgets tracks no longer in `sequence`.
    pub fn retain(&mut self, sequence: &Sequence) {
        self.flags
            .retain(|id, _| sequence.tracks.iter().any(|t| t.id == *id));
    }
}

/// A row's parts, mirrored for right to left.
struct RowLayout {
    row: Rect,
    stripe: Rect,
    name: Rect,
    switches: [(Part, Rect); 4],
    count: usize,
}

impl RowLayout {
    fn new(row: Rect, kind: TrackKind, rtl: bool) -> Self {
        // `lead` is measured from the leading edge, and mirrored in right to left.
        let place = |lead: f32, width: f32, top: f32, height: f32| {
            let x = if rtl {
                row.right() - lead - width
            } else {
                row.left() + lead
            };
            Rect::from_min_size(pos2(x, row.top() + top), vec2(width, height))
        };
        let parts = Part::switches(kind);
        let line_top = (size::ROW - BUTTON) / 2.0;
        let mut switches = [(Part::Row, Rect::NOTHING); 4];
        let first =
            row.width() - TRAILING_PAD - parts.len() as f32 * (BUTTON + BUTTON_GAP) + BUTTON_GAP;
        for (i, part) in parts.iter().enumerate() {
            let lead = first + i as f32 * (BUTTON + BUTTON_GAP);
            switches[i] = (*part, place(lead, BUTTON, line_top, BUTTON));
        }
        let name_lead = STRIPE + 8.0;
        let name = place(
            name_lead,
            (first - 8.0 - name_lead).max(0.0),
            line_top,
            BUTTON,
        );
        Self {
            row,
            stripe: place(0.0, STRIPE, 0.0, row.height() - 1.0),
            name,
            switches,
            count: parts.len(),
        }
    }

    fn switch(&self, part: Part) -> Option<Rect> {
        self.switches[..self.count]
            .iter()
            .find(|(p, _)| *p == part)
            .map(|(_, r)| *r)
    }
}

fn kind_role(kind: TrackKind) -> Role {
    match kind {
        TrackKind::Video => Role::ClipVideo,
        TrackKind::Audio => Role::ClipAudio,
        TrackKind::Subtitle => Role::ClipSubtitle,
        TrackKind::Adjustment => Role::ClipAdjustment,
    }
}

fn kind_label(kind: TrackKind) -> String {
    match kind {
        TrackKind::Video => t!("track_header.kind_video"),
        TrackKind::Audio => t!("track_header.kind_audio"),
        TrackKind::Subtitle => t!("track_header.kind_subtitle"),
        TrackKind::Adjustment => t!("track_header.kind_adjustment"),
    }
}

/// Draws the header column into `rect` and returns what the user did.
pub fn track_headers(ui: &mut Ui, rect: Rect, view: &mut TrackHeaderView) -> TrackHeaderAction {
    let paint = Paint::new(ui.ctx());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, paint.c(Role::Bg1));
    let mut action = TrackHeaderAction::None;
    let any_solo = view.any_solo();

    let mut top = rect.top() - view.scroll;
    for index in 0..view.rows.len() {
        let h = view.rows[index].flags.height.height();
        let row_rect = Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), h));
        top += h;
        if !row_rect.intersects(rect) {
            continue;
        }
        let row = view.rows[index].clone();
        let layout = RowLayout::new(row_rect, row.kind, view.rtl);
        let done = header(ui, &paint, rect, &layout, &row, any_solo, view);
        if done != TrackHeaderAction::None {
            action = done;
        }
    }

    let edge = if view.rtl { rect.left() } else { rect.right() } - 0.5;
    painter.vline(
        edge,
        rect.y_range(),
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
    action
}

/// One header: background, stripe, name (or the rename field), caption and switches.
fn header(
    ui: &mut Ui,
    paint: &Paint,
    clip: Rect,
    layout: &RowLayout,
    row: &TrackRow,
    any_solo: bool,
    view: &mut TrackHeaderView,
) -> TrackHeaderAction {
    let painter = ui.painter_at(clip);
    let flags = row.flags;
    let locked = flags.locked;
    let (hover, pressed) = (view.hover, view.pressed);
    let forced = |part| hover == Some((row.id, part));
    let silent_by_solo =
        row.kind == TrackKind::Audio && !flags.muted && !audible(false, flags.soloed, any_solo);
    let mut action = TrackHeaderAction::None;

    let row_id = part_id(row.id, Part::Row);
    let response = ui.interact(layout.row.intersect(clip), row_id, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &row.name));
    let hovered = response.hovered() || forced(Part::Row);
    let fill_rect = Rect::from_min_max(
        layout.row.min,
        pos2(layout.row.max.x, layout.row.max.y - 1.0),
    );
    if row.targeted {
        paint.glass_fill(&painter, fill_rect, CornerRadius::ZERO);
    } else if hovered {
        painter.rect_filled(fill_rect, 0.0, paint.c(Role::Bg2));
    }
    painter.hline(
        layout.row.x_range(),
        layout.row.bottom() - 0.5,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );
    let stripe = paint.c(kind_role(row.kind));
    let dimmed = flags.hidden || flags.muted || silent_by_solo;
    painter.rect_filled(
        layout.stripe,
        0.0,
        if dimmed {
            stripe.gamma_multiply(paint.theme.tokens().opacity.disabled)
        } else {
            stripe
        },
    );

    let renaming = view.renaming.as_ref().is_some_and(|r| r.track == row.id);
    if response.clicked() {
        action = TrackHeaderAction::Target(row.id);
    }
    let start_rename = !locked
        && !renaming
        && ((response.double_clicked()
            && response
                .interact_pointer_pos()
                .is_some_and(|p| layout.name.contains(p)))
            || (response.has_focus() && ui.input(|i| i.key_pressed(Key::F2))));
    if start_rename {
        view.renaming = Some(Rename {
            track: row.id,
            text: row.name.clone(),
        });
        ui.memory_mut(|m| m.request_focus(part_id(row.id, Part::Name)));
    }
    response.context_menu(|ui| {
        if ui
            .add_enabled(!locked, egui::Button::new(t!("track_header.menu_rename")))
            .clicked()
        {
            view.renaming = Some(Rename {
                track: row.id,
                text: row.name.clone(),
            });
            ui.memory_mut(|m| m.request_focus(part_id(row.id, Part::Name)));
            ui.close();
        }
        ui.separator();
        for preset in HeightPreset::ALL {
            if ui.radio(flags.height == preset, preset.label()).clicked() {
                action = TrackHeaderAction::SetHeight {
                    track: row.id,
                    height: preset,
                };
                ui.close();
            }
        }
    });

    if view.renaming.as_ref().is_some_and(|r| r.track == row.id) {
        if let Some(done) = rename_field(ui, paint, layout.name, row, view) {
            action = done;
        }
    } else {
        let role = if dimmed {
            Role::TextMuted
        } else if locked {
            Role::TextSecondary
        } else {
            Role::TextPrimary
        };
        paint.text(
            &painter,
            pos2(
                if view.rtl {
                    layout.name.right()
                } else {
                    layout.name.left()
                },
                layout.name.center().y,
            ),
            if view.rtl {
                Align2::RIGHT_CENTER
            } else {
                Align2::LEFT_CENTER
            },
            &row.name,
            Type::LabelMedium,
            role,
            layout.name.width(),
        );
    }

    if flags.height != HeightPreset::Small {
        let caption = if locked {
            t!("track_header.caption_locked", kind = kind_label(row.kind))
        } else {
            kind_label(row.kind)
        };
        let y = layout.row.top() + CAPTION_Y;
        let (x, align) = if view.rtl {
            (layout.name.right(), Align2::RIGHT_CENTER)
        } else {
            (layout.name.left(), Align2::LEFT_CENTER)
        };
        paint.text(
            &painter,
            pos2(x, y),
            align,
            &caption,
            Type::Caption,
            Role::TextMuted,
            layout.name.width(),
        );
    }

    if response.has_focus() {
        focus_ring(paint, &painter, fill_rect.shrink(1.0));
    }

    for &(part, rect) in &layout.switches[..layout.count] {
        let (glyph, look, tooltip, enabled) = match part {
            Part::Mute => (
                Glyph::Letter(t!("track_header.mute_glyph")),
                Look {
                    on: flags.muted,
                    implied: silent_by_solo,
                    tint: Role::StateWarning,
                },
                if flags.muted {
                    t!("track_header.unmute")
                } else if silent_by_solo {
                    t!("track_header.muted_by_solo")
                } else {
                    t!("track_header.mute")
                },
                !locked,
            ),
            Part::Solo => (
                Glyph::Letter(t!("track_header.solo_glyph")),
                Look {
                    on: flags.soloed,
                    implied: false,
                    tint: Role::Accent,
                },
                if flags.soloed {
                    t!("track_header.unsolo")
                } else {
                    t!("track_header.solo")
                },
                !locked,
            ),
            Part::Visibility => (
                Glyph::Icon(if flags.hidden {
                    Icon::EyeOff
                } else {
                    Icon::Eye
                }),
                Look {
                    on: flags.hidden,
                    implied: false,
                    tint: Role::TextPrimary,
                },
                if flags.hidden {
                    t!("track_header.show")
                } else {
                    t!("track_header.hide")
                },
                !locked,
            ),
            Part::Lock => (
                Glyph::Icon(if locked { Icon::Lock } else { Icon::Unlock }),
                Look {
                    on: locked,
                    implied: false,
                    tint: Role::TextPrimary,
                },
                if locked {
                    t!("track_header.unlock")
                } else {
                    t!("track_header.lock")
                },
                true,
            ),
            Part::Height => (
                Glyph::Icon(Icon::Height),
                Look {
                    on: false,
                    implied: false,
                    tint: Role::TextPrimary,
                },
                t!("track_header.height", height = flags.height.label()),
                true,
            ),
            Part::Row | Part::Name => continue,
        };
        let state = SwitchState {
            enabled,
            hover: forced(part),
            pressed: pressed == Some((row.id, part)),
        };
        let clicked = switch(
            ui,
            paint,
            clip,
            rect,
            part_id(row.id, part),
            &glyph,
            look,
            state,
            &tooltip,
        );
        if clicked {
            action = match part {
                Part::Mute => TrackHeaderAction::ToggleMute(row.id),
                Part::Solo => TrackHeaderAction::ToggleSolo(row.id),
                Part::Visibility => TrackHeaderAction::ToggleVisible(row.id),
                Part::Lock => TrackHeaderAction::ToggleLock(row.id),
                _ => TrackHeaderAction::SetHeight {
                    track: row.id,
                    height: flags.height.next(),
                },
            };
        }
    }
    action
}

/// The inline name field: Enter keeps the name, Escape drops it, and leaving the field
/// keeps it too. Returns the rename when the trimmed name changed and is not empty.
fn rename_field(
    ui: &mut Ui,
    paint: &Paint,
    rect: Rect,
    row: &TrackRow,
    view: &mut TrackHeaderView,
) -> Option<TrackHeaderAction> {
    let rename = view.renaming.as_mut()?;
    // The field reaches 4 pt past the name, so the text does not move when it opens.
    let field = rect.expand2(vec2(4.0, 0.0));
    let corner = paint.control_radius();
    let painter = ui.painter();
    painter.rect_filled(field, corner, paint.c(Role::Bg0));
    painter.rect_stroke(
        field,
        corner,
        Stroke::new(1.0, paint.c(Role::Accent)),
        StrokeKind::Inside,
    );
    let id = part_id(row.id, Part::Name);
    let edit = egui::TextEdit::singleline(&mut rename.text)
        .id(id)
        .frame(egui::Frame::NONE)
        .font(paint.font(Type::LabelMedium))
        .text_color(paint.c(Role::TextPrimary))
        .horizontal_align(if view.rtl {
            egui::Align::Max
        } else {
            egui::Align::Min
        })
        .vertical_align(egui::Align::Center)
        .margin(egui::Margin::ZERO)
        .desired_width(rect.width());
    let response = ui.put(rect, edit);
    if !response.lost_focus() {
        return None;
    }
    let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
    let text = view.renaming.take().map(|r| r.text).unwrap_or_default();
    ui.memory_mut(|m| m.request_focus(part_id(row.id, Part::Row)));
    let name = text.trim();
    (!cancelled && !name.is_empty() && name != row.name).then(|| TrackHeaderAction::Rename {
        track: row.id,
        name: name.to_string(),
    })
}

enum Glyph {
    Letter(String),
    Icon(Icon),
}

#[derive(Clone, Copy)]
struct Look {
    /// The switch is away from its default (muted, soloed, hidden, locked).
    on: bool,
    /// Off, but the track behaves as if on (silent while another track is soloed).
    implied: bool,
    /// The glyph's colour when on or implied.
    tint: Role,
}

#[derive(Clone, Copy)]
struct SwitchState {
    enabled: bool,
    hover: bool,
    pressed: bool,
}

/// A 20 pt switch: glass when on, `bg/3` on hover, an accent edge while pressed, a
/// tinted hairline when implied, dimmed and inert when disabled. Returns whether it
/// was clicked.
#[allow(clippy::too_many_arguments)]
fn switch(
    ui: &mut Ui,
    paint: &Paint,
    clip: Rect,
    rect: Rect,
    id: Id,
    glyph: &Glyph,
    look: Look,
    state: SwitchState,
    tooltip: &str,
) -> bool {
    // A disabled switch still takes its clicks (so they do not target the row) but is
    // skipped by Tab.
    let sense = if state.enabled {
        Sense::click()
    } else {
        Sense::CLICK
    };
    let response: Response = ui.interact(rect.intersect(clip), id, sense);
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::Checkbox, state.enabled, look.on, tooltip)
    });
    let painter = ui.painter_at(clip);
    let corner = paint.control_radius();
    let hovered = state.enabled && (response.hovered() || state.hover);
    let pressed = state.enabled && (response.is_pointer_button_down_on() || state.pressed);
    if look.on {
        paint.glass_fill(&painter, rect, corner);
    } else if hovered || pressed {
        painter.rect_filled(rect, corner, paint.c(Role::Bg3));
    }
    if pressed {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, paint.c(Role::Accent)),
            StrokeKind::Inside,
        );
    } else if look.implied {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, paint.mix(look.tint, 0.6)),
            StrokeKind::Inside,
        );
    }
    let role = if look.on || look.implied {
        look.tint
    } else if hovered {
        Role::TextPrimary
    } else {
        Role::TextSecondary
    };
    let mut color = paint.c(role);
    if !state.enabled {
        color = color.gamma_multiply(paint.theme.tokens().opacity.disabled);
    }
    match glyph {
        Glyph::Letter(letter) => {
            paint.text_c(
                &painter,
                rect.center(),
                Align2::CENTER_CENTER,
                letter,
                Type::LabelMedium,
                color,
                rect.width(),
            );
        }
        Glyph::Icon(which) => icon(
            &painter,
            Rect::from_center_size(rect.center(), vec2(14.0, 14.0)),
            *which,
            color,
            2.4,
        ),
    }
    if response.has_focus() {
        focus_ring(paint, &painter, rect);
    }
    if state.enabled {
        response.on_hover_text(tooltip).clicked()
    } else {
        response.on_hover_text(tooltip);
        false
    }
}

/// The keyboard focus ring: the accent at the `focus-ring` token's width.
fn focus_ring(paint: &Paint, painter: &egui::Painter, rect: Rect) {
    painter.rect_stroke(
        rect,
        paint.control_radius(),
        Stroke::new(
            paint.theme.tokens().rules.focus_ring.width_px,
            paint.c(Role::Accent),
        ),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_silences_the_other_audio_tracks() {
        // No solo: heard unless muted.
        assert!(audible(false, false, false));
        assert!(!audible(true, false, false));
        // A solo elsewhere silences this track; its own solo keeps it, mute still wins.
        assert!(!audible(false, false, true));
        assert!(audible(false, true, true));
        assert!(!audible(true, true, true));
    }

    #[test]
    fn height_presets_grow_and_wrap() {
        let heights: Vec<f32> = HeightPreset::ALL.iter().map(|h| h.height()).collect();
        assert_eq!(heights, [28.0, 48.0, 72.0]);
        assert_eq!(HeightPreset::Small.next(), HeightPreset::Medium);
        assert_eq!(HeightPreset::Large.next(), HeightPreset::Small);
        assert_eq!(HeightPreset::default(), HeightPreset::Medium);
    }

    #[test]
    fn state_applies_switches_and_refuses_them_on_locked_tracks() {
        let t = TrackId(7);
        let mut state = TrackHeaderState::default();
        assert!(state.apply(&TrackHeaderAction::ToggleMute(t)));
        assert!(state.flags(t).muted);
        assert!(state.apply(&TrackHeaderAction::SetHeight {
            track: t,
            height: HeightPreset::Large
        }));
        assert!(!state.apply(&TrackHeaderAction::SetHeight {
            track: t,
            height: HeightPreset::Large
        }));
        assert!(state.apply(&TrackHeaderAction::ToggleLock(t)));
        assert!(!state.apply(&TrackHeaderAction::ToggleMute(t)));
        assert!(!state.apply(&TrackHeaderAction::ToggleSolo(t)));
        assert!(!state.apply(&TrackHeaderAction::ToggleVisible(t)));
        assert!(state.flags(t).muted, "locked tracks keep their switches");
        // Height is a view setting and still changes.
        assert!(state.apply(&TrackHeaderAction::SetHeight {
            track: t,
            height: HeightPreset::Small
        }));
        assert!(state.apply(&TrackHeaderAction::ToggleLock(t)));
        assert!(state.apply(&TrackHeaderAction::ToggleMute(t)));
        assert!(!state.flags(t).muted);
        assert!(!state.apply(&TrackHeaderAction::Target(t)));
    }

    #[test]
    fn rtl_mirrors_the_row() {
        let row = Rect::from_min_size(pos2(0.0, 0.0), vec2(HEADER_WIDTH, 48.0));
        let ltr = RowLayout::new(row, TrackKind::Audio, false);
        let rtl = RowLayout::new(row, TrackKind::Audio, true);
        assert_eq!(ltr.stripe.left(), 0.0);
        assert_eq!(rtl.stripe.right(), HEADER_WIDTH);
        for part in Part::switches(TrackKind::Audio) {
            let a = ltr.switch(*part).expect("ltr");
            let b = rtl.switch(*part).expect("rtl");
            assert_eq!(a.left(), HEADER_WIDTH - b.right(), "{part:?}");
            assert_eq!(a.top(), b.top());
        }
        assert_eq!(ltr.name.left(), HEADER_WIDTH - rtl.name.right());
        // Switches sit right of the name and inside the row.
        let mute = ltr.switch(Part::Mute).expect("mute");
        assert!(ltr.name.right() < mute.left());
        assert!(ltr.switch(Part::Height).expect("height").right() <= HEADER_WIDTH);
        assert!(ltr.switch(Part::Visibility).is_none());
    }
}
