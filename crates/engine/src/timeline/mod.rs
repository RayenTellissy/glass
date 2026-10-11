//! Timeline playback: a `project::Sequence` played headless through one engine path.
//!
//! ```text
//!   Project + SequenceId ──▶ TimelinePlan (decode segments, per-frame layers, span hashes)
//!                                 │ segments                      │ frame(t), span_hash(t)
//!                                 ▼                               ▼
//!   glass-sched + glass-decode-<n> (Lookahead) ──▶ LookaheadFrames ──▶ glass-render:
//!       QualityGovernor (deadline from the master clock, decode wait) ──▶ TimelineRenderer
//!       (frame cache lookup, else one render graph) ──▶ present queue ──▶ UI
//!   glass-scrub (Scrubber) ── TimelineScrubRender ──▶ glass-render (same renderer, cache
//!       and CachedFrames index)
//! ```
//!
//! **Plan.** [`TimelinePlan::build`] compiles a sequence's video tracks, bottom first:
//! each enabled clip is one look-ahead [`PlanSegment`](crate::lookahead::PlanSegment)
//! (its source range through its speed), stretched over the handles its transitions show
//! (`Project::transition_needs`), so a transition is two overlapping segments. Segment
//! hashes come from the clip's item id, media and timing only, so an edit elsewhere keeps
//! them. [`TimelinePlan::frame`] says what each frame composites (per track a clip, or a
//! transition with its linear progress, as `Track::segment_at` defines it), and
//! [`TimelinePlan::span_hash`] keys the frame cache by runs of frames that composite the
//! same things with the same looks: an edit changes only the runs it touches.
//!
//! **Render.** [`TimelineRenderer`] builds one graph per frame (see its module docs) at
//! the sequence's canvas ([`TimelinePlan::canvas`], schema v6), scaled by the preview
//! quality, and caches the 8-bit preview picture.
//!
//! **Playback.** [`TimelinePlayback`] replaces the playback skeleton's per-frame round
//! robin: `glass-render` takes every timeline frame in order from the look-ahead, asks
//! the [`QualityGovernor`](crate::quality::QualityGovernor) whether and at which level to
//! render it, renders it (waiting for the GPU) and queues it for the presenter. The first
//! frame of a run (after play or seek) is due when it is ready on the master clock;
//! frame `t` after it is due one frame interval per frame later. The governor's budget
//! is the time from when render could start a frame to its deadline, at most one
//! interval, its decode lateness the time render waited for the look-ahead, and a frame
//! finishing after its deadline counts as late. Pausing at a reduced level renders the
//! frame on screen again at full resolution; a seek while paused presents the new
//! position's frame. The scrubber's exact frames go through the same renderer on the
//! render thread ([`TimelineScrubRender`]), into the same cache and index.

mod plan;
mod playback;
mod render;

pub use plan::{
    ClipLook, EffectLook, FrameDesc, PlanError, TimelinePlan, TrackFrame, TransitionLook,
};
pub use playback::{
    PlaybackStats, Presented, PresentedFrame, TimelineConfig, TimelinePlayback, TimelineScrubRender,
};
pub use render::{
    FrameOutput, PREVIEW_FORMAT, RenderStats, RenderedFrame, RendererConfig, TimelineRenderer,
};
