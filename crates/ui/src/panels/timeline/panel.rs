//! The timeline panel as the workspace docks it: the playhead's timecode over the track
//! headers, the ruler over the track lanes, the track scrollbar, and the zoom slider and
//! minimap along the bottom, laid out as in `timeline_view` and kept in step through
//! one [`TimelineView`].
//!
//! Clips are drawn over the lanes by a [`LaneLayer`] the caller passes (the clip
//! renderer), so this module only owns the frame around them. A minimal playhead line
//! runs through the ruler and the lanes; clicking or dragging the ruler seeks. Track
//! header switches and heights live in [`TimelinePanel::headers`] (Decision 68); renames
//! and seeks come back as a [`TimelineAction`] for the app to apply.

use std::sync::Arc;

use egui::{Align2, Rect, Stroke, Ui, pos2, vec2};
use project::{Sequence, TrackId};

use super::minimap::{self, MINIMAP_HEIGHT, MinimapContent};
use super::ruler::{self, RULER_HEIGHT, Timecode};
use super::track_header::{
    self, HEADER_WIDTH, Rename, TrackHeaderAction, TrackHeaderState, TrackHeaderView,
};
use super::view::{
    self, Extent, SCROLLBAR_WIDTH, TimelineView, TrackLayout, ZOOM_SLIDER_WIDTH, sequence_duration,
};
use crate::panels::widgets::{Paint, Type};
use crate::theme::Role;

/// Draws over the lanes: `(ui, tracks rect, view, layout)`.
pub type LaneLayer<'a> = &'a mut dyn FnMut(&mut Ui, Rect, &TimelineView, &TrackLayout);

/// The panel's state between frames.
#[derive(Clone, Debug)]
pub struct TimelinePanel {
    pub view: TimelineView,
    pub headers: TrackHeaderState,
    /// The track edits go to.
    pub targeted: Option<TrackId>,
    renaming: Option<Rename>,
    /// The minimap's spans, rebuilt when the sequence changes.
    minimap: Option<(Arc<Sequence>, MinimapContent)>,
}

impl TimelinePanel {
    pub fn new(sequence: &Sequence) -> Self {
        Self {
            view: TimelineView::for_sequence(sequence),
            headers: TrackHeaderState::default(),
            targeted: None,
            renaming: None,
            minimap: None,
        }
    }
}

/// What the user did this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineAction {
    None,
    /// A header action the panel did not apply itself: a rename (apply with
    /// `Project::rename_track`). Switches, heights and targeting are already applied.
    Header(TrackHeaderAction),
    /// Mute or solo changed (already applied): the audio mix needs the new switches.
    MixChanged,
    /// Move the playhead to this frame.
    Seek(i64),
}

/// Draws the timeline of `sequence` into `rect` with the playhead at frame `playhead`.
pub fn timeline_panel(
    ui: &mut Ui,
    rect: Rect,
    panel: &mut TimelinePanel,
    sequence: &Arc<Sequence>,
    playhead: i64,
    layer: Option<LaneLayer<'_>>,
) -> TimelineAction {
    let paint = Paint::new(ui.ctx());
    let rtl = assets::i18n::current().is_rtl();
    let mut action = TimelineAction::None;
    ui.painter_at(rect)
        .rect_filled(rect, 0.0, paint.c(Role::Bg1));

    panel.headers.retain(sequence);
    let rows = panel.headers.rows(sequence, panel.targeted);
    let layout = TrackLayout::from_rows(&rows);

    let tracks_w = (rect.width() - HEADER_WIDTH - SCROLLBAR_WIDTH).max(1.0);
    let tracks_h = (rect.height() - RULER_HEIGHT - MINIMAP_HEIGHT).max(1.0);
    let (header_x, tracks_x) = if rtl {
        (rect.right() - HEADER_WIDTH, rect.left() + SCROLLBAR_WIDTH)
    } else {
        (rect.left(), rect.left() + HEADER_WIDTH)
    };
    let tracks = Rect::from_min_size(
        pos2(tracks_x, rect.top() + RULER_HEIGHT),
        vec2(tracks_w, tracks_h),
    );
    let header_rect =
        Rect::from_min_size(pos2(header_x, tracks.top()), vec2(HEADER_WIDTH, tracks_h));
    let corner = Rect::from_min_size(pos2(header_x, rect.top()), vec2(HEADER_WIDTH, RULER_HEIGHT));
    let ruler_rect = Rect::from_min_size(
        pos2(tracks.left(), rect.top()),
        vec2(tracks_w, RULER_HEIGHT),
    );
    let bar_x = if rtl { rect.left() } else { tracks.right() };
    let bar = Rect::from_min_size(pos2(bar_x, tracks.top()), vec2(SCROLLBAR_WIDTH, tracks_h));
    let map = Rect::from_min_size(
        pos2(tracks.left(), tracks.bottom()),
        vec2(tracks_w, MINIMAP_HEIGHT),
    );

    panel.view.set_extent(Extent {
        width: tracks_w,
        height: tracks_h,
        duration: sequence_duration(sequence),
        content_height: layout.content_height(),
    });
    panel
        .view
        .handle_input(ui, header_rect.union(tracks), tracks.left());

    let tc = Timecode::for_sequence(sequence);
    let ruler_response = ruler::ruler(ui, ruler_rect, &panel.view, tc);
    if (ruler_response.clicked() || ruler_response.dragged())
        && let Some(p) = ruler_response.interact_pointer_pos()
    {
        action = TimelineAction::Seek(panel.view.frame_at_x(p.x - tracks.left()).max(0));
    }
    paint.text(
        &ui.painter_at(corner),
        pos2(corner.left() + paint.s().s12, corner.center().y),
        Align2::LEFT_CENTER,
        &tc.format(playhead),
        Type::Mono,
        Role::TextPrimary,
        corner.width(),
    );
    ui.painter_at(corner).hline(
        corner.x_range(),
        corner.bottom() - 0.5,
        Stroke::new(1.0, paint.c(Role::BorderSubtle)),
    );

    view::lanes(ui, tracks, &panel.view, &layout);
    if let Some(layer) = layer {
        layer(ui, tracks, &panel.view, &layout);
    }

    let mut headers = TrackHeaderView {
        rows,
        renaming: panel.renaming.take(),
        scroll: panel.view.scroll_y(),
        rtl,
        ..TrackHeaderView::default()
    };
    let header_action = track_header::track_headers(ui, header_rect, &mut headers);
    panel.renaming = headers.renaming;
    match header_action {
        TrackHeaderAction::None => {}
        TrackHeaderAction::Target(track) => panel.targeted = Some(track),
        TrackHeaderAction::Rename { .. } => action = TimelineAction::Header(header_action),
        ref switch => {
            let mix = matches!(
                switch,
                TrackHeaderAction::ToggleMute(_) | TrackHeaderAction::ToggleSolo(_)
            );
            if panel.headers.apply(switch) && mix {
                action = TimelineAction::MixChanged;
            }
        }
    }

    let _ = view::track_scrollbar(ui, bar, &mut panel.view);
    if panel
        .minimap
        .as_ref()
        .is_none_or(|(s, _)| !Arc::ptr_eq(s, sequence))
    {
        panel.minimap = Some((sequence.clone(), MinimapContent::from_sequence(sequence)));
    }
    if let Some((_, content)) = &panel.minimap {
        let _ = minimap::minimap(ui, map, &mut panel.view, content);
    }
    let slider = Rect::from_center_size(
        pos2(header_rect.center().x, map.center().y),
        vec2(ZOOM_SLIDER_WIDTH, MINIMAP_HEIGHT),
    );
    let _ = view::zoom_slider(ui, slider, &mut panel.view);

    // The playhead through the ruler and the lanes.
    let x = tracks.left() + panel.view.frame_to_x(playhead);
    if (tracks.left()..=tracks.right()).contains(&x) {
        let painter = ui.painter_at(ruler_rect.union(tracks));
        painter.vline(
            x.round() + 0.5,
            ruler_rect.top()..=tracks.bottom(),
            Stroke::new(1.0, paint.c(Role::Accent)),
        );
    }
    action
}
