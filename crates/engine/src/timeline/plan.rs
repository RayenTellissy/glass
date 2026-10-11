//! [`TimelinePlan`]: a sequence compiled for playback. It holds the decode segments the
//! look-ahead and the scrubber read, what each timeline frame composites (per video
//! track, bottom first: a clip, a nested sequence's frame, or a transition between two
//! clips with its progress), and the frame cache's segment hash of every run of frames
//! that composite the same things.
//!
//! A clip with keyframes on what it draws (Decision 63) has its look evaluated per frame
//! ([`TimelinePlan::look_at`]); a clip of a sequence shows that sequence's own plan, its
//! decode segments flattened into this one (see `nested`). Runs of frames that show
//! either are keyed per frame by what they show (Decision 75).

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use media::{Rational as MediaRational, Timestamp};
use project::params::ParamValues;
use project::{
    BlendMode as ProjectBlend, ClipSource, Crop, EditError, ItemId, Project, SequenceId, Time,
    TimeError, TrackKind, Transform, TransitionId,
};
use render::blend::BlendMode;
use render::frame_cache::SegmentHash;
use render::lut::LutSpace;
use render::transition::Easing;

use super::animate::{Animation, effect_look};
use super::nested::{NestedLook, NestedTime, flatten};
use crate::lookahead::{PlanSegment, SegmentList, SourceId, SourceRef};

/// How deep sequences nest before the plan leaves the deeper clips out (counted in
/// [`TimelinePlan::nested_clips`]). The project refuses cycles; this guards the
/// recursion anyway.
pub const MAX_NESTING_DEPTH: usize = 16;

/// Why a sequence could not be compiled.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("the project has no sequence {0:?}")]
    UnknownSequence(SequenceId),
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error(transparent)]
    Time(#[from] TimeError),
}

/// How a clip's picture is placed and mixed: its crop, transform, opacity, blend mode and
/// the effects of its stack this build renders.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipLook {
    pub item: ItemId,
    pub transform: Transform,
    pub crop: Crop,
    pub opacity: f64,
    pub blend: BlendMode,
    /// Enabled effects with a renderer, first to last.
    pub effects: Vec<EffectLook>,
}

/// An effect of a clip's stack, as the renderer applies it.
#[derive(Clone, Debug, PartialEq)]
pub enum EffectLook {
    /// A `.cube` file applied in `space` (Decision 36).
    Lut {
        file: PathBuf,
        space: LutSpace,
        intensity: f32,
    },
}

/// A transition on a track, in timeline frames.
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionLook {
    pub id: TransitionId,
    /// Timeline frames it covers, end exclusive.
    pub span: Range<i64>,
    /// The render-side transition id (`cross-dissolve`).
    pub kind: String,
    pub direction: Option<String>,
    pub easing: Easing,
    pub params: ParamValues,
    /// Decode segments of the outgoing and incoming clips; `None` for nothing (a fade, a
    /// disabled clip, or a clip of a sequence).
    pub outgoing: Option<usize>,
    pub incoming: Option<usize>,
    /// The outgoing and incoming clips when they nest a sequence (indexes into
    /// [`TimelinePlan::nested`]).
    pub outgoing_nested: Option<usize>,
    pub incoming_nested: Option<usize>,
}

impl TransitionLook {
    /// Linear progress at timeline frame `t`: 0 at the span's first frame, `(t - start) /
    /// length` after, as `project::Track::segment_at` defines it.
    pub fn linear_at(&self, t: i64) -> f64 {
        let length = (self.span.end - self.span.start).max(1);
        (t - self.span.start) as f64 / length as f64
    }
}

/// What one video track shows at a timeline frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackFrame {
    /// A clip alone: the index of its decode segment.
    Clip(usize),
    /// A transition (index into [`TimelinePlan::transition`]) at a linear progress.
    Transition { transition: usize, linear: f64 },
    /// A clip of a sequence alone (index into [`TimelinePlan::nested`]), showing that
    /// sequence's frame `frame`.
    Nested { nested: usize, frame: i64 },
}

/// What a timeline frame composites: one entry per video track that shows something,
/// bottom track first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameDesc {
    pub frame: i64,
    pub tracks: Vec<TrackFrame>,
}

impl FrameDesc {
    /// The decode segments the frame reads, each once, in track order (a nested
    /// sequence's as the parent segments that carry them).
    pub fn segments(&self, plan: &TimelinePlan) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.tracks.len() + 1);
        self.collect_segments(plan, &mut |s| {
            if !out.contains(&s) {
                out.push(s);
            }
        });
        out
    }

    fn collect_segments(&self, plan: &TimelinePlan, out: &mut dyn FnMut(usize)) {
        let nested = |k: usize, frame: Option<i64>, out: &mut dyn FnMut(usize)| {
            let look = &plan.nested[k];
            if let Some(frame) = frame {
                look.plan
                    .frame(frame)
                    .collect_segments(&look.plan, &mut |s| {
                        if let Some(s) = look.segment(s) {
                            out(s);
                        }
                    });
            }
        };
        for track in &self.tracks {
            match *track {
                TrackFrame::Clip(s) => out(s),
                TrackFrame::Transition { transition, .. } => {
                    let t = &plan.transitions[transition];
                    for s in [t.outgoing, t.incoming].into_iter().flatten() {
                        out(s);
                    }
                    for k in [t.outgoing_nested, t.incoming_nested].into_iter().flatten() {
                        nested(k, plan.nested[k].frame_at(self.frame), out);
                    }
                }
                TrackFrame::Nested { nested: k, frame } => nested(k, Some(frame), out),
            }
        }
    }
}

/// What a track item shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    /// A clip's decode segment.
    Segment(usize),
    /// A clip of a sequence (index into `nested`).
    Nested(usize),
}

#[derive(Clone, Debug, Default)]
struct TrackPlan {
    /// Clip items: timeline frames (a nested clip's where it shows a nested frame) and
    /// what they show, by start.
    items: Vec<(Range<i64>, Shown)>,
    /// Transitions on this track (indexes), by span start.
    transitions: Vec<usize>,
}

/// A sequence compiled for playback (see the module docs). Immutable: an edit builds a
/// new plan, and every segment and span the edit did not touch keeps its hash.
#[derive(Clone, Debug)]
pub struct TimelinePlan {
    canvas: (u32, u32),
    rate: project::Rational,
    frame_duration: MediaRational,
    end: i64,
    segments: SegmentList,
    looks: Vec<ClipLook>,
    /// Hash of each look (of what it shows for an animated one), for the span hashes.
    look_hashes: Vec<SegmentHash>,
    /// What each segment shows of its source, for the per-frame hashes of animated looks.
    pictures: Vec<SegmentHash>,
    /// How each segment's look moves; `None` for a constant look (and the segments of
    /// nested sequences, whose own plans animate them).
    animations: Vec<Option<Arc<Animation>>>,
    by_hash: HashMap<SegmentHash, usize>,
    transitions: Vec<TransitionLook>,
    transition_hashes: Vec<SegmentHash>,
    nested: Vec<NestedLook>,
    /// Video tracks, bottom first.
    tracks: Vec<TrackPlan>,
    /// Runs of frames that composite the same things, in order, with their cache hash.
    spans: Vec<(Range<i64>, SegmentHash)>,
    /// Per span: whether its frames show an animated look or a nested sequence, so each
    /// frame's hash adds what it shows.
    per_frame: Vec<bool>,
    missing_media: usize,
    nested_clips: usize,
    skipped_effects: usize,
}

/// One clip on its way to a decode segment.
struct Draft {
    rank: usize,
    parity: u32,
    item: ItemId,
    item_range: Range<i64>,
    timeline: Range<i64>,
    source: SourceRef,
    content_hash: String,
    source_start: Time,
    speed: MediaRational,
    look: ClipLook,
    animation: Option<Arc<Animation>>,
}

/// One clip of a sequence on its way to a [`NestedLook`].
struct NestedDraft {
    rank: usize,
    parity: u32,
    item: ItemId,
    item_range: Range<i64>,
    timeline: Range<i64>,
    sequence: SequenceId,
    plan: Arc<TimelinePlan>,
    time: NestedTime,
    look: ClipLook,
    animation: Option<Arc<Animation>>,
}

/// A decode segment before the segments are put in order.
struct Entry {
    segment: PlanSegment,
    look: ClipLook,
    look_hash: SegmentHash,
    picture: SegmentHash,
    animation: Option<Arc<Animation>>,
    origin: Origin,
}

enum Origin {
    /// A clip of this sequence on the track of `rank`.
    Clip {
        item: ItemId,
        rank: usize,
        item_range: Range<i64>,
    },
    /// Segment `segment` of nested clip `nested`'s plan.
    Nested { nested: usize, segment: usize },
}

impl TimelinePlan {
    /// Compiles the video tracks of `sequence`. Clips that are disabled, whose media the
    /// project does not list, or that sit on other kinds of tracks are left out; effects
    /// without a renderer (unknown schemas) are skipped and counted. A clip of a
    /// sequence compiles that sequence too (each once per build).
    pub fn build(project: &Project, sequence: SequenceId) -> Result<Self, PlanError> {
        Self::build_nested(project, sequence, 0, &mut HashMap::new())
    }

    fn build_nested(
        project: &Project,
        sequence: SequenceId,
        depth: usize,
        built: &mut HashMap<SequenceId, Arc<TimelinePlan>>,
    ) -> Result<Self, PlanError> {
        let seq = project
            .sequence(sequence)
            .ok_or(PlanError::UnknownSequence(sequence))?;
        let rate = seq.frame_rate;
        let grid = seq.grid(TrackKind::Video)?;
        let frames = |t: Time| t.frames_floor(rate);
        let mut drafts: Vec<Draft> = Vec::new();
        let mut nested_drafts: Vec<NestedDraft> = Vec::new();
        let mut raw_transitions: Vec<(usize, TransitionLook, Option<ItemId>, Option<ItemId>)> =
            Vec::new();
        let mut missing_media = 0;
        let mut nested_clips = 0;
        let mut skipped_effects = 0;

        let video: Vec<_> = seq
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .collect();
        let ranks = video.len();
        for (rank, track) in video.iter().rev().enumerate() {
            for (index, item) in track.items.iter().enumerate() {
                let Some(clip) = item.clip() else {
                    continue;
                };
                if !clip.enabled {
                    continue;
                }
                let range = frames(item.range.start)..frames(item.range.end()?);
                let mut effects_out = Vec::new();
                for effect in clip.effects.iter().filter(|e| e.enabled) {
                    match effect_look(effect, None) {
                        Some(look) => effects_out.push(look),
                        None => skipped_effects += 1,
                    }
                }
                let look = ClipLook {
                    item: item.id,
                    transform: clip.transform,
                    crop: clip.crop,
                    opacity: clip.opacity,
                    blend: blend(clip.blend),
                    effects: effects_out,
                };
                let animation = Animation::new(clip, item.range.start, rate).map(Arc::new);
                let speed = clip.speed.speed()?;
                let media_id = match clip.source {
                    ClipSource::Media(media_id) => media_id,
                    ClipSource::Sequence(child) => {
                        let plan = match built.get(&child) {
                            Some(plan) => Some(plan.clone()),
                            None if depth + 1 < MAX_NESTING_DEPTH
                                && project.sequence(child).is_some() =>
                            {
                                let plan =
                                    Arc::new(Self::build_nested(project, child, depth + 1, built)?);
                                built.insert(child, plan.clone());
                                Some(plan)
                            }
                            None => None,
                        };
                        let Some(plan) = plan else {
                            nested_clips += 1;
                            continue;
                        };
                        let nested_rate = plan.rate();
                        nested_drafts.push(NestedDraft {
                            rank,
                            parity: (index % 2) as u32,
                            item: item.id,
                            item_range: range.clone(),
                            timeline: range,
                            sequence: child,
                            time: NestedTime {
                                rate,
                                item_start: item.range.start,
                                source_start: clip.source_start,
                                speed,
                                nested_frame: nested_rate.frame_duration()?,
                                duration: project.sequence_duration(child)?,
                            },
                            plan,
                            look,
                            animation,
                        });
                        continue;
                    }
                };
                let Some(media) = project.media().get(media_id) else {
                    missing_media += 1;
                    continue;
                };
                drafts.push(Draft {
                    rank,
                    parity: (index % 2) as u32,
                    item: item.id,
                    item_range: range.clone(),
                    timeline: range,
                    source: SourceRef::new(SourceId(media_id.0), &media.path),
                    content_hash: media.content_hash.clone(),
                    source_start: clip.source_start,
                    speed: MediaRational::new(speed.num, speed.den),
                    look,
                    animation,
                });
            }

            for t in &track.transitions {
                let span = track.transition_span(grid, t)?;
                let needs = project.transition_needs(t.id)?;
                let span = frames(span.start)..frames(span.end()?);
                // The incoming clip shows its media from before its start over the part
                // before the cut, the outgoing clip past its end over the part after it.
                if let Some((item, _, from)) = needs.incoming {
                    if let Some(d) = drafts.iter_mut().find(|d| d.item == item)
                        && span.start < d.timeline.start
                    {
                        d.timeline.start = span.start;
                        d.source_start = from;
                    }
                    if let Some(d) = nested_drafts.iter_mut().find(|d| d.item == item) {
                        d.timeline.start = d.timeline.start.min(span.start);
                    }
                }
                if let Some((item, ..)) = needs.outgoing {
                    if let Some(d) = drafts.iter_mut().find(|d| d.item == item) {
                        d.timeline.end = d.timeline.end.max(span.end);
                    }
                    if let Some(d) = nested_drafts.iter_mut().find(|d| d.item == item) {
                        d.timeline.end = d.timeline.end.max(span.end);
                    }
                }
                raw_transitions.push((
                    rank,
                    TransitionLook {
                        id: t.id,
                        span,
                        kind: t.kind.clone(),
                        direction: t.direction.clone(),
                        easing: easing(&t.easing),
                        params: t.params.clone(),
                        outgoing: None,
                        incoming: None,
                        outgoing_nested: None,
                        incoming_nested: None,
                    },
                    needs.outgoing.map(|n| n.0),
                    needs.incoming.map(|n| n.0),
                ));
            }
        }

        let frame_duration = MediaRational::new(rate.den, rate.num);
        let mut entries = Vec::with_capacity(drafts.len());
        for d in drafts {
            let source_start = timestamp(d.source_start, d.speed, frame_duration);
            let mut h = Hash128::new("decode segment");
            h.add(&d.item.0);
            h.add(&d.source.id.0);
            h.add(&d.source.path);
            h.add(&d.content_hash);
            h.add(&(d.timeline.start, d.timeline.end));
            h.add(&(d.source_start.num(), d.source_start.den()));
            h.add(&(d.speed.num, d.speed.den));
            let hash = h.finish();
            // What a frame shows of the clip: the file and the source time at timeline
            // zero, so trimming either end or stretching over a transition's handle
            // keeps the frames the clip still shows.
            let at_zero = d.source_start.checked_sub(
                Time::from_frames(d.timeline.start, rate)?
                    .mul_ratio(i64::from(d.speed.num), i64::from(d.speed.den))?,
            )?;
            let mut p = Hash128::new("picture");
            p.add(&d.source.path);
            p.add(&d.content_hash);
            p.add(&(at_zero.num(), at_zero.den()));
            p.add(&(d.speed.num, d.speed.den));
            let picture = p.finish();
            entries.push(Entry {
                look_hash: static_look_hash(picture, &d.look, d.animation.is_some()),
                picture,
                look: d.look,
                animation: d.animation,
                segment: PlanSegment {
                    source: d.source,
                    timeline: d.timeline,
                    source_start,
                    speed: d.speed,
                    layer: d.rank as u32 * 2 + d.parity,
                    hash,
                },
                origin: Origin::Clip {
                    item: d.item,
                    rank: d.rank,
                    item_range: d.item_range,
                },
            });
        }
        let mut nested = Vec::with_capacity(nested_drafts.len());
        let mut nested_items = Vec::with_capacity(nested_drafts.len());
        for (k, d) in nested_drafts.into_iter().enumerate() {
            let layer = d.rank as u32 * 2 + d.parity;
            for (segment, flat) in flatten(
                d.item,
                layer,
                &d.time,
                d.timeline.clone(),
                &d.plan,
                frame_duration,
            )? {
                entries.push(Entry {
                    segment: flat,
                    look: d.plan.looks[segment].clone(),
                    look_hash: d.plan.look_hashes[segment],
                    picture: d.plan.pictures[segment],
                    animation: None,
                    origin: Origin::Nested { nested: k, segment },
                });
            }
            // What the clip shows of the nested sequence: its canvas, and its time at
            // timeline zero, speed, rate and length (the frames themselves are keyed
            // per frame by the nested plan's hashes).
            let at_zero = d
                .time
                .source_start
                .checked_sub(d.time.item_start.mul_rational(d.time.speed)?)?;
            let mut p = Hash128::new("nested picture");
            p.add(&d.plan.canvas);
            p.add(&(at_zero.num(), at_zero.den()));
            p.add(&(d.time.speed.num, d.time.speed.den));
            p.add(&(d.plan.rate.num, d.plan.rate.den));
            p.add(&(d.time.duration.num(), d.time.duration.den()));
            let hash = static_look_hash(p.finish(), &d.look, d.animation.is_some());
            nested_items.push((d.rank, d.time.visible(d.item_range.clone()), d.item));
            nested.push(NestedLook {
                item: d.item,
                sequence: d.sequence,
                look: d.look,
                segments: vec![None; d.plan.segments.segments().len()],
                plan: d.plan,
                time: d.time,
                animation: d.animation,
                hash,
            });
        }

        // Decode segments in order of timeline start (stable, so bottom tracks first on a
        // tie), with their looks at the same indexes; `SegmentList` leaves empty ones out.
        entries.retain(|e| e.segment.timeline.start < e.segment.timeline.end);
        entries.sort_by_key(|e| e.segment.timeline.start);
        let mut segments = Vec::with_capacity(entries.len());
        let mut looks = Vec::with_capacity(entries.len());
        let mut look_hashes = Vec::with_capacity(entries.len());
        let mut pictures = Vec::with_capacity(entries.len());
        let mut animations = Vec::with_capacity(entries.len());
        let mut tracks = vec![TrackPlan::default(); ranks];
        let mut index_of = HashMap::new();
        for (i, e) in entries.into_iter().enumerate() {
            match e.origin {
                Origin::Clip {
                    item,
                    rank,
                    item_range,
                } => {
                    index_of.insert(item, i);
                    if item_range.start < item_range.end {
                        tracks[rank].items.push((item_range, Shown::Segment(i)));
                    }
                }
                Origin::Nested { nested: k, segment } => nested[k].segments[segment] = Some(i),
            }
            segments.push(e.segment);
            looks.push(e.look);
            look_hashes.push(e.look_hash);
            pictures.push(e.picture);
            animations.push(e.animation);
        }
        let mut nested_of = HashMap::new();
        for (k, (rank, range, item)) in nested_items.into_iter().enumerate() {
            nested_of.insert(item, k);
            if range.start < range.end {
                tracks[rank].items.push((range, Shown::Nested(k)));
            }
        }
        for track in &mut tracks {
            track.items.sort_by_key(|(r, _)| r.start);
        }
        let mut transitions = Vec::with_capacity(raw_transitions.len());
        let mut transition_hashes = Vec::with_capacity(raw_transitions.len());
        for (rank, mut look, outgoing, incoming) in raw_transitions {
            look.outgoing = outgoing.and_then(|item| index_of.get(&item).copied());
            look.incoming = incoming.and_then(|item| index_of.get(&item).copied());
            look.outgoing_nested = outgoing.and_then(|item| nested_of.get(&item).copied());
            look.incoming_nested = incoming.and_then(|item| nested_of.get(&item).copied());
            let mut h = Hash128::new("transition");
            h.add(&look.id.0);
            h.add(&(look.span.start, look.span.end));
            h.add(&look.kind);
            h.add(&look.direction);
            h.add(&format!("{:?}", look.easing));
            h.add(&serde_json::to_string(&look.params).unwrap_or_default());
            transition_hashes.push(h.finish());
            tracks[rank].transitions.push(transitions.len());
            transitions.push(look);
        }
        for track in &mut tracks {
            track
                .transitions
                .sort_by_key(|&i| transitions[i].span.start);
        }

        let segments = SegmentList::new(segments);
        let by_hash = segments
            .segments()
            .iter()
            .enumerate()
            .map(|(i, s)| (s.hash, i))
            .collect();
        let end = segments
            .segments()
            .iter()
            .map(|s| s.timeline.end)
            .chain(transitions.iter().map(|t| t.span.end))
            .chain(
                tracks
                    .iter()
                    .flat_map(|t| t.items.iter().map(|(r, _)| r.end)),
            )
            .max()
            .unwrap_or(0);
        let mut plan = Self {
            canvas: (seq.resolution.width, seq.resolution.height),
            rate,
            frame_duration,
            end,
            segments,
            looks,
            look_hashes,
            pictures,
            animations,
            by_hash,
            transitions,
            transition_hashes,
            nested,
            tracks,
            spans: Vec::new(),
            per_frame: Vec::new(),
            missing_media,
            nested_clips,
            skipped_effects,
        };
        (plan.spans, plan.per_frame) = plan.compute_spans();
        Ok(plan)
    }

    /// The sequence's canvas in pixels: what [`TimelineConfig::new`] takes.
    ///
    /// [`TimelineConfig::new`]: super::TimelineConfig::new
    pub fn canvas(&self) -> (u32, u32) {
        self.canvas
    }

    /// The sequence's frame rate.
    pub fn rate(&self) -> project::Rational {
        self.rate
    }

    /// One timeline frame's duration.
    pub fn frame_duration(&self) -> MediaRational {
        self.frame_duration
    }

    /// The first frame after everything on the video tracks.
    pub fn end(&self) -> i64 {
        self.end
    }

    /// The decode segments, the look-ahead's and the scrubber's [`DecodePlan`]
    /// (cheap to clone), nested sequences' included.
    ///
    /// [`DecodePlan`]: crate::lookahead::DecodePlan
    pub fn segments(&self) -> &SegmentList {
        &self.segments
    }

    pub fn segment(&self, index: usize) -> &PlanSegment {
        &self.segments.segments()[index]
    }

    /// The decode segment with frame cache key `hash` (a look-ahead layer's `segment`).
    pub fn segment_index(&self, hash: SegmentHash) -> Option<usize> {
        self.by_hash.get(&hash).copied()
    }

    /// How decode segment `index`'s clip is placed and mixed, with its constant values
    /// ([`Self::look_at`] evaluates animated ones). For a nested sequence's segment, the
    /// look its own clip has in that sequence.
    pub fn look(&self, index: usize) -> &ClipLook {
        &self.looks[index]
    }

    /// Decode segment `index`'s look at timeline frame `t`: animated values at the
    /// source time the frame shows.
    pub fn look_at(&self, index: usize, t: i64) -> Cow<'_, ClipLook> {
        match &self.animations[index] {
            Some(a) => Cow::Owned(a.look_at(&self.looks[index], t)),
            None => Cow::Borrowed(&self.looks[index]),
        }
    }

    /// Whether decode segment `index`'s look is animated (keyframes on what it draws).
    pub fn is_animated(&self, index: usize) -> bool {
        self.animations[index].is_some()
    }

    pub fn transition(&self, index: usize) -> &TransitionLook {
        &self.transitions[index]
    }

    pub fn transitions(&self) -> &[TransitionLook] {
        &self.transitions
    }

    /// A clip of a sequence ([`TrackFrame::Nested`]).
    pub fn nested(&self, index: usize) -> &NestedLook {
        &self.nested[index]
    }

    /// Every clip of a sequence on the video tracks.
    pub fn nested_looks(&self) -> &[NestedLook] {
        &self.nested
    }

    /// Clips left out because the project lists no media for them.
    pub fn missing_media(&self) -> usize {
        self.missing_media
    }

    /// Enabled clips of sequences left out: nested deeper than [`MAX_NESTING_DEPTH`].
    pub fn nested_clips(&self) -> usize {
        self.nested_clips
    }

    /// Enabled effects left out because this build has no renderer for them.
    pub fn skipped_effects(&self) -> usize {
        self.skipped_effects
    }

    /// What timeline frame `t` composites.
    pub fn frame(&self, t: i64) -> FrameDesc {
        let mut desc = FrameDesc::default();
        self.frame_into(t, &mut desc);
        desc
    }

    /// [`Self::frame`] into `desc`, reusing its allocation.
    pub fn frame_into(&self, t: i64, desc: &mut FrameDesc) {
        desc.frame = t;
        desc.tracks.clear();
        for track in &self.tracks {
            let at = track
                .transitions
                .partition_point(|&i| self.transitions[i].span.start <= t);
            if let Some(&i) = at.checked_sub(1).and_then(|k| track.transitions.get(k))
                && self.transitions[i].span.contains(&t)
            {
                desc.tracks.push(TrackFrame::Transition {
                    transition: i,
                    linear: self.transitions[i].linear_at(t),
                });
                continue;
            }
            let at = track.items.partition_point(|(r, _)| r.start <= t);
            if let Some((range, shown)) = at.checked_sub(1).and_then(|k| track.items.get(k))
                && range.contains(&t)
            {
                match *shown {
                    Shown::Segment(s) => desc.tracks.push(TrackFrame::Clip(s)),
                    Shown::Nested(k) => {
                        if let Some(frame) = self.nested[k].frame_at(t) {
                            desc.tracks.push(TrackFrame::Nested { nested: k, frame });
                        }
                    }
                }
            }
        }
    }

    /// The frame cache's segment hash for timeline frame `t`: one per run of frames that
    /// composite the same clips and transitions with the same constant looks, and per
    /// frame where an animated look or a nested sequence shows, from what the frame
    /// shows (evaluated values, the nested frame and its own hash). An edit changes the
    /// hashes of the frames it changes only.
    pub fn span_hash(&self, t: i64) -> SegmentHash {
        let at = self.spans.partition_point(|(r, _)| r.start <= t);
        match at
            .checked_sub(1)
            .and_then(|k| Some((k, self.spans.get(k)?)))
        {
            Some((k, (range, hash))) if range.contains(&t) => {
                if self.per_frame[k] {
                    self.frame_hash(t, *hash)
                } else {
                    *hash
                }
            }
            _ => Hash128::new("empty").finish(),
        }
    }

    /// The runs of frames with one cache hash, in order. A run whose frames show an
    /// animated look or a nested sequence holds the hash [`Self::span_hash`] extends per
    /// frame.
    pub fn spans(&self) -> &[(Range<i64>, SegmentHash)] {
        &self.spans
    }

    fn frame_hash(&self, t: i64, run: SegmentHash) -> SegmentHash {
        let desc = self.frame(t);
        let mut h = Hash128::new("frame");
        h.add(&run.0);
        for track in &desc.tracks {
            match *track {
                TrackFrame::Clip(s) => self.add_animated(&mut h, s, t),
                TrackFrame::Transition { transition, .. } => {
                    let tr = &self.transitions[transition];
                    for s in [tr.outgoing, tr.incoming].into_iter().flatten() {
                        self.add_animated(&mut h, s, t);
                    }
                    for k in [tr.outgoing_nested, tr.incoming_nested]
                        .into_iter()
                        .flatten()
                    {
                        self.add_nested(&mut h, k, t);
                    }
                }
                TrackFrame::Nested { nested, .. } => self.add_nested(&mut h, nested, t),
            }
        }
        h.finish()
    }

    fn add_animated(&self, h: &mut Hash128, s: usize, t: i64) {
        if let Some(a) = &self.animations[s] {
            h.add(&look_hash(self.pictures[s], &a.look_at(&self.looks[s], t)).0);
        }
    }

    fn add_nested(&self, h: &mut Hash128, k: usize, t: i64) {
        let nested = &self.nested[k];
        let frame = nested.frame_at(t);
        h.add(&frame);
        if let Some(frame) = frame {
            h.add(&nested.plan.span_hash(frame).0);
        }
        if let Some(a) = &nested.animation {
            h.add(&look_hash(nested.hash, &a.look_at(&nested.look, t)).0);
        }
    }

    fn compute_spans(&self) -> (Vec<(Range<i64>, SegmentHash)>, Vec<bool>) {
        let mut cuts = BTreeSet::new();
        for track in &self.tracks {
            for (range, _) in &track.items {
                cuts.insert(range.start);
                cuts.insert(range.end);
            }
            for &i in &track.transitions {
                cuts.insert(self.transitions[i].span.start);
                cuts.insert(self.transitions[i].span.end);
            }
        }
        let cuts: Vec<i64> = cuts.into_iter().collect();
        let mut spans: Vec<(Range<i64>, SegmentHash)> = Vec::new();
        let mut per_frame: Vec<bool> = Vec::new();
        let mut desc = FrameDesc::default();
        for pair in cuts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            self.frame_into(a, &mut desc);
            if desc.tracks.is_empty() {
                continue;
            }
            let mut h = Hash128::new("span");
            let mut moving = false;
            for track in &desc.tracks {
                match *track {
                    TrackFrame::Clip(s) => {
                        h.add(&0u8);
                        h.add(&self.look_hashes[s].0);
                        moving |= self.animations[s].is_some();
                    }
                    TrackFrame::Transition { transition, .. } => {
                        let t = &self.transitions[transition];
                        h.add(&1u8);
                        h.add(&self.transition_hashes[transition].0);
                        for side in [t.outgoing, t.incoming] {
                            h.add(&side.map(|s| self.look_hashes[s].0));
                            moving |= side.is_some_and(|s| self.animations[s].is_some());
                        }
                        for side in [t.outgoing_nested, t.incoming_nested] {
                            h.add(&side.map(|k| self.nested[k].hash.0));
                            moving |= side.is_some();
                        }
                    }
                    TrackFrame::Nested { nested, .. } => {
                        h.add(&2u8);
                        h.add(&self.nested[nested].hash.0);
                        moving = true;
                    }
                }
            }
            let hash = h.finish();
            match spans.last_mut() {
                Some((range, last)) if range.end == a && *last == hash => range.end = b,
                _ => {
                    spans.push((a..b, hash));
                    per_frame.push(moving);
                }
            }
        }
        (spans, per_frame)
    }
}

/// The span hash part of a clip's look: the look hashed with what it shows of its source
/// when constant, what it shows alone when animated (its frames add their values).
fn static_look_hash(picture: SegmentHash, look: &ClipLook, animated: bool) -> SegmentHash {
    if animated {
        let mut h = Hash128::new("animated look");
        h.add(&picture.0);
        h.finish()
    } else {
        look_hash(picture, look)
    }
}

/// A clip's look hashed with what it shows of its source.
fn look_hash(picture: SegmentHash, look: &ClipLook) -> SegmentHash {
    let mut h = Hash128::new("look");
    h.add(&picture.0);
    let t = &look.transform;
    for v in t.position.iter().chain(&t.scale).chain(&t.anchor) {
        h.add_f64(*v);
    }
    h.add_f64(t.rotation);
    let c = &look.crop;
    for v in [c.left, c.top, c.right, c.bottom, c.feather, look.opacity] {
        h.add_f64(v);
    }
    h.add(&look.blend.id());
    for effect in &look.effects {
        match effect {
            EffectLook::Lut {
                file,
                space,
                intensity,
            } => {
                h.add(file);
                h.add(&space.id());
                h.add(&intensity.to_bits());
            }
        }
    }
    h.finish()
}

fn blend(mode: ProjectBlend) -> BlendMode {
    BlendMode::from_id(mode.id()).unwrap_or(BlendMode::Normal)
}

/// The easing a transition names; ids this build does not know play linear.
fn easing(id: &str) -> Easing {
    match id {
        "ease-in" => Easing::EaseIn,
        "ease-out" => Easing::EaseOut,
        "ease-in-out" => Easing::EaseInOut,
        _ => Easing::Linear,
    }
}

/// `t` as a media timestamp in a base fine enough for every frame step of `speed` at
/// `frame_duration` to land on a tick (the least common multiple of the denominators),
/// or in microseconds, rounded down, when that does not fit.
pub(super) fn timestamp(t: Time, speed: MediaRational, frame_duration: MediaRational) -> Timestamp {
    let step_den = i64::from(frame_duration.den) * i64::from(speed.den);
    let step_num = i64::from(frame_duration.num) * i64::from(speed.num);
    let step_den = step_den / gcd(step_num.unsigned_abs(), step_den.unsigned_abs()).max(1) as i64;
    let lcm = |a: i64, b: i64| a / gcd(a.unsigned_abs(), b.unsigned_abs()).max(1) as i64 * b;
    let den = lcm(t.den(), step_den.abs().max(1));
    match i32::try_from(den) {
        Ok(base) if base > 0 => {
            Timestamp::new(t.num() * (den / t.den()), MediaRational::new(1, base))
        }
        _ => {
            let micros = (i128::from(t.num()) * 1_000_000).div_euclid(i128::from(t.den()));
            Timestamp::new(micros as i64, MediaRational::new(1, 1_000_000))
        }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A 128-bit hash from two independently seeded SipHashes. Stable within a build, which
/// is all the in-memory frame cache needs.
pub(super) struct Hash128 {
    a: DefaultHasher,
    b: DefaultHasher,
}

impl Hash128 {
    pub(super) fn new(domain: &str) -> Self {
        let mut a = DefaultHasher::new();
        let mut b = DefaultHasher::new();
        0x61u8.hash(&mut a);
        0x62u8.hash(&mut b);
        domain.hash(&mut a);
        domain.hash(&mut b);
        Self { a, b }
    }

    pub(super) fn add<T: Hash + ?Sized>(&mut self, value: &T) {
        value.hash(&mut self.a);
        value.hash(&mut self.b);
    }

    fn add_f64(&mut self, value: f64) {
        self.add(&value.to_bits());
    }

    pub(super) fn finish(self) -> SegmentHash {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&self.a.finish().to_le_bytes());
        bytes[8..].copy_from_slice(&self.b.finish().to_le_bytes());
        SegmentHash(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use project::{ClipSpec, Rational, SpeedKeep, TimeRange, TransitionSpec};

    struct Fixture {
        project: Project,
        seq: SequenceId,
        a: ItemId,
        b: ItemId,
        c: ItemId,
        d: ItemId,
    }

    fn at(frames: i64) -> Time {
        Time::from_frames(frames, Rational::new(30, 1)).unwrap()
    }

    fn range(start: i64, end: i64) -> TimeRange {
        TimeRange::between(at(start), at(end)).unwrap()
    }

    /// Two video tracks: A and B with a centred 10-frame dissolve on V1, C and D (at 2x)
    /// on V2, an audio track that the plan ignores.
    fn fixture() -> Fixture {
        let p = Project::new("uid", "plan");
        let (p, m1) = p.add_media("h1", "/media/one.mp4").unwrap();
        let (p, m2) = p.add_media("h2", "/media/two.mov").unwrap();
        let (p, seq) = p
            .add_sequence("main", Rational::new(30, 1), 48_000)
            .unwrap();
        let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).unwrap();
        let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).unwrap();
        let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).unwrap();
        let clip = |media, start, end, source| ClipSpec {
            media,
            range: range(start, end),
            source_start: at(source),
        };
        let (p, a) = p.insert_clip(v1, clip(m1, 0, 50, 0)).unwrap();
        let (p, b) = p.insert_clip(v1, clip(m1, 50, 90, 30)).unwrap();
        let (p, c) = p.insert_clip(v2, clip(m2, 10, 40, 0)).unwrap();
        let (p, d) = p.insert_clip(v2, clip(m1, 60, 90, 0)).unwrap();
        let (p, _) = p.insert_clip(a1, clip(m1, 0, 90, 0)).unwrap();
        let p = p
            .set_clip_speed(d, Rational::new(2, 1), SpeedKeep::Timeline)
            .unwrap();
        let (p, _) = p
            .add_transition(TransitionSpec::new(Some(a), Some(b), at(10)))
            .unwrap();
        Fixture {
            project: p,
            seq,
            a,
            b,
            c,
            d,
        }
    }

    fn item_of(plan: &TimelinePlan, s: usize) -> ItemId {
        plan.look(s).item
    }

    #[test]
    fn segments_cover_clips_and_their_transition_handles() {
        let f = fixture();
        let plan = TimelinePlan::build(&f.project, f.seq).unwrap();
        let fd = plan.frame_duration();
        let by_item = |item| {
            (0..plan.segments().segments().len())
                .find(|&s| item_of(&plan, s) == item)
                .map(|s| plan.segment(s).clone())
                .unwrap()
        };
        let a = by_item(f.a);
        let b = by_item(f.b);
        let d = by_item(f.d);
        // The centred 10-frame dissolve covers 45..55: A plays on to 55, B starts at 45
        // showing its media from 5 frames before its source start (1 s).
        assert_eq!(a.timeline, 0..55);
        assert_eq!(b.timeline, 45..90);
        assert_eq!(b.source_time_at(45, fd).seconds(), 25.0 / 30.0);
        assert_eq!(b.source_time_at(50, fd).seconds(), 1.0);
        // D plays its media twice as fast.
        assert_eq!(d.speed, MediaRational::new(2, 1));
        assert_eq!(d.source_time_at(75, fd).seconds(), 1.0);
        // Bottom track below top track, and the incoming clip of a cut on the other parity.
        assert!(a.layer < by_item(f.c).layer);
        assert_ne!(a.layer, b.layer);
        assert_eq!(
            plan.segments().segments().len(),
            4,
            "audio and other tracks left out"
        );
        assert_eq!(plan.end(), 90);
    }

    #[test]
    fn frames_agree_with_the_projects_segment_at() {
        let f = fixture();
        let plan = TimelinePlan::build(&f.project, f.seq).unwrap();
        let seq = f.project.sequence(f.seq).unwrap();
        let video: Vec<_> = seq
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .rev()
            .collect();
        for t in -2..95 {
            let desc = plan.frame(t);
            let mut expected = Vec::new();
            for track in &video {
                match seq.segment_at(track.id, at(t)).unwrap() {
                    None => {}
                    Some(project::Segment::Item(item)) => {
                        expected.push((Some(item.id), None, None))
                    }
                    Some(project::Segment::Transition(tr)) => expected.push((
                        tr.outgoing.map(|i| i.id),
                        tr.incoming.map(|i| i.id),
                        Some(tr.progress),
                    )),
                }
            }
            let actual: Vec<_> = desc
                .tracks
                .iter()
                .map(|tf| match *tf {
                    TrackFrame::Clip(s) => (Some(item_of(&plan, s)), None, None),
                    TrackFrame::Transition { transition, linear } => {
                        let tr = plan.transition(transition);
                        (
                            tr.outgoing.map(|s| item_of(&plan, s)),
                            tr.incoming.map(|s| item_of(&plan, s)),
                            Some(linear),
                        )
                    }
                    TrackFrame::Nested { .. } => unreachable!("no nested clips"),
                })
                .collect();
            assert_eq!(actual, expected, "frame {t}");
        }
    }

    #[test]
    fn an_edit_changes_only_the_hashes_it_touches() {
        let f = fixture();
        let before = TimelinePlan::build(&f.project, f.seq).unwrap();
        let edited = f.project.set_clip_opacity(f.d, 0.5).unwrap();
        let after = TimelinePlan::build(&edited, f.seq).unwrap();
        // Decode segments do not depend on the look.
        let hashes = |p: &TimelinePlan| -> Vec<SegmentHash> {
            p.segments().segments().iter().map(|s| s.hash).collect()
        };
        assert_eq!(hashes(&before), hashes(&after));
        for t in 0..90 {
            let same = before.span_hash(t) == after.span_hash(t);
            assert_eq!(same, !(60..90).contains(&t), "frame {t}");
        }
        // Moving C changes C's segment and the frames it leaves and enters only.
        let moved = f.project.move_item(f.c, video_track(&f, 0), at(5)).unwrap();
        let after = TimelinePlan::build(&moved, f.seq).unwrap();
        let changed: Vec<bool> = hashes(&before)
            .iter()
            .zip(hashes(&after))
            .map(|(x, y)| *x != y)
            .collect();
        assert_eq!(changed.iter().filter(|c| **c).count(), 1);
        for t in 0..90 {
            let same = before.span_hash(t) == after.span_hash(t);
            assert_eq!(same, !(5..40).contains(&t), "frame {t}");
        }
        assert_ne!(
            before.span_hash(20),
            before.span_hash(41),
            "C on and off differ"
        );
    }

    #[test]
    fn trims_keep_the_hashes_of_the_frames_a_clip_still_shows() {
        let f = fixture();
        let before = TimelinePlan::build(&f.project, f.seq).unwrap();
        let changed = |after: &TimelinePlan| -> Vec<i64> {
            (0..90)
                .filter(|&t| before.span_hash(t) != after.span_hash(t))
                .collect()
        };
        // C on V2 covers 10..40 over A: trimming its tail to 30 changes 30..40 only, and
        // its head to 20 (the source start moving with it) changes 10..20 only.
        let tail = f.project.trim_item(f.c, range(10, 30)).unwrap();
        let tail = TimelinePlan::build(&tail, f.seq).unwrap();
        assert_eq!(changed(&tail), (30..40).collect::<Vec<_>>());
        let head = f.project.trim_item(f.c, range(20, 40)).unwrap();
        let head = TimelinePlan::build(&head, f.seq).unwrap();
        assert_eq!(changed(&head), (10..20).collect::<Vec<_>>());
        // Slipping C shows other source frames everywhere it plays.
        let slipped = f.project.slip_clip(f.c, at(3)).unwrap();
        let slipped = TimelinePlan::build(&slipped, f.seq).unwrap();
        assert_eq!(changed(&slipped), (10..40).collect::<Vec<_>>());
        // Lengthening the dissolve stretches A's and B's decode segments, yet only the
        // frames it covers change.
        let longer = f
            .project
            .set_transition_duration(
                f.project.sequence(f.seq).unwrap().tracks[1].transitions[0].id,
                at(20),
            )
            .unwrap();
        let longer = TimelinePlan::build(&longer, f.seq).unwrap();
        assert_eq!(changed(&longer), (40..60).collect::<Vec<_>>());
    }

    #[test]
    fn animated_frames_hash_their_values() {
        use project::params::ParamValue;
        use project::{ClipParam, KeyTarget};
        let f = fixture();
        // C (10..40, source 0) fades in over source 0..0.5 s and holds.
        let fade = KeyTarget::Clip {
            item: f.c,
            param: ClipParam::Opacity,
        };
        let keyed = f
            .project
            .animate_param(fade, at(0))
            .and_then(|p| p.set_key(fade, at(0), ParamValue::Float(0.0)))
            .and_then(|p| p.set_key(fade, at(15), ParamValue::Float(1.0)))
            .unwrap();
        let before = TimelinePlan::build(&f.project, f.seq).unwrap();
        let plan = TimelinePlan::build(&keyed, f.seq).unwrap();
        let s = (0..plan.segments().segments().len())
            .find(|&s| item_of(&plan, s) == f.c)
            .unwrap();
        assert!(plan.is_animated(s));
        assert_eq!(plan.look_at(s, 10).opacity, 0.0);
        assert_eq!(plan.look_at(s, 16).opacity, 6.0 / 15.0);
        assert_eq!(plan.look_at(s, 39).opacity, 1.0);
        // Frames C does not show keep their keys; frames it shows change.
        for t in 0..90 {
            let same = before.span_hash(t) == plan.span_hash(t);
            assert_eq!(same, !(10..40).contains(&t), "frame {t}");
        }
        // Moving the last key to 0.5 changes the frames after the first key only.
        let edited = keyed.set_key(fade, at(15), ParamValue::Float(0.5)).unwrap();
        let after = TimelinePlan::build(&edited, f.seq).unwrap();
        let changed: Vec<i64> = (0..90)
            .filter(|&t| plan.span_hash(t) != after.span_hash(t))
            .collect();
        assert_eq!(changed, (11..40).collect::<Vec<_>>());
        // A move keeps the values on their source frames.
        let moved = keyed.move_item(f.c, video_track(&f, 0), at(5)).unwrap();
        let moved = TimelinePlan::build(&moved, f.seq).unwrap();
        let s = (0..moved.segments().segments().len())
            .find(|&s| item_of(&moved, s) == f.c)
            .unwrap();
        assert_eq!(moved.look_at(s, 11).opacity, 6.0 / 15.0);
    }

    fn video_track(f: &Fixture, index: usize) -> project::TrackId {
        let seq = f.project.sequence(f.seq).unwrap();
        seq.tracks[index].id
    }

    #[test]
    fn timestamps_land_on_every_frame_step() {
        let fd = MediaRational::new(1, 30);
        let ts = timestamp(Time::seconds(1), MediaRational::new(2, 1), fd);
        assert_eq!(ts.seconds(), 1.0);
        assert_eq!(ts.time_base.den % 15, 0);
        let ts = timestamp(
            Time::new(1001, 30000).unwrap(),
            MediaRational::new(1, 1),
            fd,
        );
        assert_eq!(ts.value * 30000, 1001 * i64::from(ts.time_base.den));
    }
}
