//! The editor's panels behind one [`PanelHost`]: the app fills a [`Panels`] each frame
//! from its state (views to draw, the timeline's state to update), the dock draws the
//! open ones, and the app applies the [`PanelEvent`]s they returned afterwards. Tests
//! and snapshots fill it with sample content to draw the whole workspace.

use std::sync::Arc;

use egui::{Rect, Ui};
use project::Sequence;

use super::PanelKind;
use super::dock::PanelHost;
use crate::panels::inspector::inspector_panel;
use crate::panels::media_pool::{MediaPoolAction, MediaPoolView, media_pool};
use crate::panels::metadata::{MetadataAction, MetadataView, metadata_panel};
use crate::panels::placeholder::placeholder_panel;
use crate::panels::preview::{PreviewAction, PreviewView, preview_panel};
use crate::panels::timeline::panel::{LaneLayer, TimelineAction, TimelinePanel, timeline_panel};

/// The open sequence as the timeline panel draws it.
pub struct TimelineContent<'a> {
    pub panel: &'a mut TimelinePanel,
    pub sequence: &'a Arc<Sequence>,
    pub playhead: i64,
    /// Draws the clips over the lanes.
    pub layer: Option<LaneLayer<'a>>,
}

/// What a panel returned this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelEvent {
    MediaPool(MediaPoolAction),
    Preview(PreviewAction),
    Timeline(TimelineAction),
    Metadata(MetadataAction),
}

/// Everything the panels show this frame, and what they returned.
pub struct Panels<'a> {
    pub pool: &'a mut MediaPoolView,
    pub metadata: &'a MetadataView,
    pub preview: &'a PreviewView,
    /// `None` while no sequence is open.
    pub timeline: Option<TimelineContent<'a>>,
    pub events: Vec<PanelEvent>,
}

impl PanelHost for Panels<'_> {
    fn show(&mut self, ui: &mut Ui, rect: Rect, panel: PanelKind) {
        match panel {
            PanelKind::MediaPool => {
                let action = media_pool(ui, rect, self.pool);
                if action != MediaPoolAction::None {
                    self.events.push(PanelEvent::MediaPool(action));
                }
            }
            PanelKind::Preview => {
                let action = preview_panel(ui, rect, self.preview);
                if action != PreviewAction::None {
                    self.events.push(PanelEvent::Preview(action));
                }
            }
            PanelKind::Timeline => match &mut self.timeline {
                Some(content) => {
                    let layer: Option<LaneLayer<'_>> = match &mut content.layer {
                        Some(layer) => Some(&mut **layer),
                        None => None,
                    };
                    let action = timeline_panel(
                        ui,
                        rect,
                        content.panel,
                        content.sequence,
                        content.playhead,
                        layer,
                    );
                    if action != TimelineAction::None {
                        self.events.push(PanelEvent::Timeline(action));
                    }
                }
                None => placeholder_panel(ui, rect, &crate::t!("preview.no_sequence")),
            },
            PanelKind::Inspector => inspector_panel(ui, rect),
            PanelKind::Metadata => {
                let action = metadata_panel(ui, rect, self.metadata);
                if action != MetadataAction::None {
                    self.events.push(PanelEvent::Metadata(action));
                }
            }
            PanelKind::Scopes
            | PanelKind::ColourWheels
            | PanelKind::Mixer
            | PanelKind::EffectsBrowser => placeholder_panel(ui, rect, &panel.title()),
        }
    }
}
