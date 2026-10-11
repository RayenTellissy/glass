//! [`TimelinePlan`]: a sequence compiled for playback. It holds the decode segments the
//! look-ahead and the scrubber read, what each timeline frame composites (per video
//! track, bottom first: a clip, or a transition between two clips with its progress),
//! and the frame cache's segment hash of every run of frames that composite the same
//! things.

use std::collections::{BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;

use media::{Rational as MediaRational, Timestamp};
use project::params::{ParamValues, effects};
use project::{
    BlendMode as ProjectBlend, Crop, EditError, ItemId, Project, SequenceId, Time, TimeError,
    TrackKind, Transform, TransitionId,
};
use render::blend::BlendMode;
use render::frame_cache::SegmentHash;
use render::lut::LutSpace;
use render::transition::Easing;

use crate::lookahead::{PlanSegment, SegmentList, SourceId, SourceRef};

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
    /// Decode segments of the outgoing and incoming clips; `None` for nothing (a fade, or
    /// a disabled clip).
    pub outgoing: Option<usize>,
    pub incoming: Option<usize>,
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
}

/// What a timeline frame composites: one entry per video track that shows something,
/// bottom track first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameDesc {
    pub frame: i64,
    pub tracks: Vec<TrackFrame>,
}

impl FrameDesc {
    /// The decode segments the frame reads, each once, in track order.
    pub fn segments(&self, plan: &TimelinePlan) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.tracks.len() + 1);
        for track in &self.tracks {
            let (a, b) = match *track {
                TrackFrame::Clip(s) => (Some(s), None),
                TrackFrame::Transition { transition, .. } => {
                    let t = &plan.transitions[transition];
                    (t.outgoing, t.incoming)
                }
            };
            for s in [a, b].into_iter().flatten() {
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
        out
    }
}

#[derive(Clone, Debug, Default)]
struct TrackPlan {
    /// Clip items: timeline frames and decode segment, by start.
    items: Vec<(Range<i64>, usize)>,
    /// Transitions on this track (indexes), by span start.
    transitions: Vec<usize>,
}

/// A sequence compiled for playback (see the module docs). Immutable: an edit builds a
/// new plan, and every segment and span the edit did not touch keeps its hash.
#[derive(Clone, Debug)]
pub struct TimelinePlan {
    rate: project::Rational,
    frame_duration: MediaRational,
    end: i64,
    segments: SegmentList,
    looks: Vec<ClipLook>,
    /// Hash of each look, for the span hashes.
    look_hashes: Vec<SegmentHash>,
    by_hash: HashMap<SegmentHash, usize>,
    transitions: Vec<TransitionLook>,
    transition_hashes: Vec<SegmentHash>,
    /// Video tracks, bottom first.
    tracks: Vec<TrackPlan>,
    /// Runs of frames that composite the same things, in order, with their cache hash.
    spans: Vec<(Range<i64>, SegmentHash)>,
    missing_media: usize,
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
}

impl TimelinePlan {
    /// Compiles the video tracks of `sequence`. Clips that are disabled, whose media the
    /// project does not list, or that sit on other kinds of tracks are left out; effects
    /// without a renderer (unknown schemas) are skipped and counted.
    pub fn build(project: &Project, sequence: SequenceId) -> Result<Self, PlanError> {
        let seq = project
            .sequence(sequence)
            .ok_or(PlanError::UnknownSequence(sequence))?;
        let rate = seq.frame_rate;
        let grid = seq.grid(TrackKind::Video)?;
        let frames = |t: Time| t.frames_floor(rate);
        let mut drafts: Vec<Draft> = Vec::new();
        let mut track_items: Vec<Vec<ItemId>> = Vec::new();
        let mut raw_transitions: Vec<(usize, TransitionLook, Option<ItemId>, Option<ItemId>)> =
            Vec::new();
        let mut missing_media = 0;
        let mut skipped_effects = 0;

        let video: Vec<_> = seq
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .collect();
        for (rank, track) in video.iter().rev().enumerate() {
            let mut items = Vec::new();
            for (index, item) in track.items.iter().enumerate() {
                let Some(clip) = item.clip() else {
                    continue;
                };
                if !clip.enabled {
                    continue;
                }
                let Some(media) = project.media().get(clip.media) else {
                    missing_media += 1;
                    continue;
                };
                let range = frames(item.range.start)..frames(item.range.end()?);
                let speed = clip.speed.speed()?;
                let mut effects_out = Vec::new();
                for effect in clip.effects.iter().filter(|e| e.enabled) {
                    match effect_look(&effect.schema, &effect.params) {
                        Some(look) => effects_out.push(look),
                        None => skipped_effects += 1,
                    }
                }
                items.push(item.id);
                drafts.push(Draft {
                    rank,
                    parity: (index % 2) as u32,
                    item: item.id,
                    item_range: range.clone(),
                    timeline: range,
                    source: SourceRef::new(SourceId(clip.media.0), &media.path),
                    content_hash: media.content_hash.clone(),
                    source_start: clip.source_start,
                    speed: MediaRational::new(speed.num, speed.den),
                    look: ClipLook {
                        item: item.id,
                        transform: clip.transform,
                        crop: clip.crop,
                        opacity: clip.opacity,
                        blend: blend(clip.blend),
                        effects: effects_out,
                    },
                });
            }
            track_items.push(items);

            for t in &track.transitions {
                let span = track.transition_span(grid, t)?;
                let needs = project.transition_needs(t.id)?;
                let span = frames(span.start)..frames(span.end()?);
                // The incoming clip shows its media from before its start over the part
                // before the cut, the outgoing clip past its end over the part after it.
                if let Some((item, _, from)) = needs.incoming
                    && let Some(d) = drafts.iter_mut().find(|d| d.item == item)
                    && span.start < d.timeline.start
                {
                    d.timeline.start = span.start;
                    d.source_start = from;
                }
                if let Some((item, ..)) = needs.outgoing
                    && let Some(d) = drafts.iter_mut().find(|d| d.item == item)
                {
                    d.timeline.end = d.timeline.end.max(span.end);
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
                    },
                    needs.outgoing.map(|n| n.0),
                    needs.incoming.map(|n| n.0),
                ));
            }
        }

        // Decode segments in order of timeline start (stable, so bottom tracks first on a
        // tie), with their looks at the same indexes.
        drafts.sort_by_key(|d| d.timeline.start);
        let frame_duration = MediaRational::new(rate.den, rate.num);
        let mut segments = Vec::with_capacity(drafts.len());
        let mut looks = Vec::with_capacity(drafts.len());
        let mut look_hashes = Vec::with_capacity(drafts.len());
        let mut item_ranges = Vec::with_capacity(drafts.len());
        let mut index_of = HashMap::new();
        for (i, d) in drafts.into_iter().enumerate() {
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
            index_of.insert(d.item, i);
            look_hashes.push(look_hash(hash, &d.look));
            looks.push(d.look);
            item_ranges.push((d.rank, d.item_range));
            segments.push(PlanSegment {
                source: d.source,
                timeline: d.timeline,
                source_start,
                speed: d.speed,
                layer: d.rank as u32 * 2 + d.parity,
                hash,
            });
        }

        let mut tracks = vec![TrackPlan::default(); track_items.len()];
        for (i, (rank, range)) in item_ranges.into_iter().enumerate() {
            if range.start < range.end {
                tracks[rank].items.push((range, i));
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
            .max()
            .unwrap_or(0);
        let mut plan = Self {
            rate,
            frame_duration,
            end,
            segments,
            looks,
            look_hashes,
            by_hash,
            transitions,
            transition_hashes,
            tracks,
            spans: Vec::new(),
            missing_media,
            skipped_effects,
        };
        plan.spans = plan.compute_spans();
        Ok(plan)
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
    /// (cheap to clone).
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

    /// How decode segment `index`'s clip is placed and mixed.
    pub fn look(&self, index: usize) -> &ClipLook {
        &self.looks[index]
    }

    pub fn transition(&self, index: usize) -> &TransitionLook {
        &self.transitions[index]
    }

    pub fn transitions(&self) -> &[TransitionLook] {
        &self.transitions
    }

    /// Clips left out because the project lists no media for them.
    pub fn missing_media(&self) -> usize {
        self.missing_media
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
            if let Some((range, s)) = at.checked_sub(1).and_then(|k| track.items.get(k))
                && range.contains(&t)
            {
                desc.tracks.push(TrackFrame::Clip(*s));
            }
        }
    }

    /// The frame cache's segment hash for timeline frame `t`: one per run of frames that
    /// composite the same clips and transitions with the same looks. An edit changes the
    /// hashes of the runs it touches only.
    pub fn span_hash(&self, t: i64) -> SegmentHash {
        let at = self.spans.partition_point(|(r, _)| r.start <= t);
        match at.checked_sub(1).and_then(|k| self.spans.get(k)) {
            Some((range, hash)) if range.contains(&t) => *hash,
            _ => Hash128::new("empty").finish(),
        }
    }

    /// The runs of frames with one cache hash, in order.
    pub fn spans(&self) -> &[(Range<i64>, SegmentHash)] {
        &self.spans
    }

    fn compute_spans(&self) -> Vec<(Range<i64>, SegmentHash)> {
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
        let mut desc = FrameDesc::default();
        for pair in cuts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            self.frame_into(a, &mut desc);
            if desc.tracks.is_empty() {
                continue;
            }
            let mut h = Hash128::new("span");
            for track in &desc.tracks {
                match *track {
                    TrackFrame::Clip(s) => {
                        h.add(&0u8);
                        h.add(&self.look_hashes[s].0);
                    }
                    TrackFrame::Transition { transition, .. } => {
                        let t = &self.transitions[transition];
                        h.add(&1u8);
                        h.add(&self.transition_hashes[transition].0);
                        for side in [t.outgoing, t.incoming] {
                            h.add(&side.map(|s| self.look_hashes[s].0));
                        }
                    }
                }
            }
            let hash = h.finish();
            match spans.last_mut() {
                Some((range, last)) if range.end == a && *last == hash => range.end = b,
                _ => spans.push((a..b, hash)),
            }
        }
        spans
    }
}

/// A clip's look hashed with its decode segment.
fn look_hash(segment: SegmentHash, look: &ClipLook) -> SegmentHash {
    let mut h = Hash128::new("look");
    h.add(&segment.0);
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

/// The renderer for an effect of the stack, or `None` when this build has none (or a LUT
/// names no file).
fn effect_look(schema: &str, params: &ParamValues) -> Option<EffectLook> {
    match schema {
        "lut" => {
            let schema = effects::lut();
            if params.boolean(&schema, "enabled") == Some(false) {
                return None;
            }
            let file = params.text(&schema, "file").filter(|f| !f.is_empty())?;
            let space = params
                .choice(&schema, "input")
                .and_then(|id| LutSpace::from_id(&id))
                .unwrap_or_default();
            let intensity = params.float(&schema, "intensity").unwrap_or(1.0) as f32;
            Some(EffectLook::Lut {
                file: PathBuf::from(file),
                space,
                intensity,
            })
        }
        _ => None,
    }
}

/// `t` as a media timestamp in a base fine enough for every frame step of `speed` at
/// `frame_duration` to land on a tick (the least common multiple of the denominators),
/// or in microseconds, rounded down, when that does not fit.
fn timestamp(t: Time, speed: MediaRational, frame_duration: MediaRational) -> Timestamp {
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
struct Hash128 {
    a: DefaultHasher,
    b: DefaultHasher,
}

impl Hash128 {
    fn new(domain: &str) -> Self {
        let mut a = DefaultHasher::new();
        let mut b = DefaultHasher::new();
        0x61u8.hash(&mut a);
        0x62u8.hash(&mut b);
        domain.hash(&mut a);
        domain.hash(&mut b);
        Self { a, b }
    }

    fn add<T: Hash + ?Sized>(&mut self, value: &T) {
        value.hash(&mut self.a);
        value.hash(&mut self.b);
    }

    fn add_f64(&mut self, value: f64) {
        self.add(&value.to_bits());
    }

    fn finish(self) -> SegmentHash {
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
