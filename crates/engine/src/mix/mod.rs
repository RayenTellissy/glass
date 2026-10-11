//! The sequence audio mix: a sequence's audio tracks through the `audio` crate's mixer
//! graph, live on the audio device ([`AudioPlayback`]) or offline for export
//! ([`OfflineMix`], [`mix_range`]), with the same code and the same samples.
//!
//! **Clips** ([`source`]): each enabled clip on an audio track is decoded by its own
//! `media::AudioReader`, resampled to the mix rate and mixed to the mix layout (`media`'s
//! channel rules), played at the clip's constant speed (the reader runs at the mix rate
//! divided by the speed, rounded to a whole rate, so a 0.5x clip plays an octave down,
//! like tape; speed ramps play silence until pitch-preserving varispeed exists), and
//! scaled by its gain, evaluated through the project's own keyframe curve
//! (`Clip::gain_db_at`) at every `CONTROL_FRAMES`-frame control point and ramped linearly
//! between them. A clip of a nested sequence plays that sequence's own clip mix. The clips
//! of a track sum into that track's feed.
//!
//! **Edges** ([`edges`]): clip edges and transition spans play at exact sample positions
//! (a time rounds to the nearest sample at the mix rate, halves later; exact at the
//! sequence's own rate, whose sample grid audio tracks sit on). The read position of mix
//! frame `k` of a clip is its source start rounded the same way at the reader rate, plus
//! `k` less the clip's first frame, so a clip split anywhere plays on without a seam.
//! Transitions on audio tracks are crossfades (equal power by default, linear or S-curve
//! through the `crossfade` kind's `curve`, `project::audio_transition`), and fades from or
//! to nothing next to gaps; declick ramps at hard cuts are an option.
//!
//! **Track mute and solo** are the project's own track switches (schema v9, Decision 77):
//! the graph reads `Track::muted` and `Track::solo` from the snapshot it plays, so muting
//! a track is an edit, with undo, and the next [`AudioPlayback::update`] or offline mix
//! hears it.
//!
//! **Mixer settings** ([`MixSettings`]): fader, pan, output, sends and fader and pan
//! automation per track, user buses and the master. The project file has no place for
//! them yet (a schema version will add one), so they are an input here. Automation curves are project
//! [`Curve`]s with keys in sequence time, turned into mixer lanes by [`lane_from_curve`]:
//! linear and hold keys map one to one, bezier segments are flattened through
//! `Curve::value_at` every [`BEZIER_STEP_SECONDS`].
//!
//! **Tracks** are the sequence's audio tracks, top to bottom; each strip's id is its
//! `TrackId`'s number, so meters and setting changes address tracks by id.

mod edges;
mod offline;
mod playback;
mod source;

use std::collections::BTreeMap;

use audio::mixer::{
    GraphError, GraphSpec, Lane, LaneError, Layout, MasterSpec, Point, Route, SendSpec, StripSpec,
};
use project::{
    ChannelLayout, Curve, Interpolation, Project, Sequence, SequenceId, Time, TimeError,
};
use project::{TrackId, TrackKind};

pub use edges::DECLICK_SECONDS;
pub use offline::{OfflineMix, mix_range, mix_range_with};
pub use playback::{AudioPlayback, AudioPlaybackConfig, FeedStats};
pub use source::SourceStats;

/// Frames per block the offline mix renders, and the largest block the live graph takes.
pub const MIX_BLOCK: usize = 1024;

/// Spacing of the points a bezier automation segment is flattened into.
pub const BEZIER_STEP_SECONDS: f64 = 0.005;

/// Why a mix could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum MixError {
    #[error("no sequence {0:?}")]
    UnknownSequence(SequenceId),
    #[error("mixer graph: {0}")]
    Graph(#[from] GraphError),
    #[error("automation: {0}")]
    Lane(#[from] LaneError),
    #[error("automation curves must hold numbers")]
    CurveKind,
    #[error("time: {0}")]
    Time(#[from] TimeError),
    #[error("the mix rate is zero")]
    ZeroRate,
    #[error("audio decode thread: {0}")]
    Thread(#[from] std::io::Error),
    #[error("mixer: {0}")]
    Mixer(#[from] audio::mixer::MixerError),
}

/// The rate and speaker layout a mix renders at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixFormat {
    pub sample_rate: u32,
    pub layout: Layout,
}

impl MixFormat {
    /// The sequence's own rate and layout.
    pub fn of(sequence: &Sequence) -> Self {
        Self {
            sample_rate: sequence.sample_rate,
            layout: layout(sequence.channel_layout),
        }
    }

    pub fn channels(self) -> usize {
        self.layout.channels()
    }
}

/// The mixer layout of a project layout.
pub fn layout(layout: ChannelLayout) -> Layout {
    match layout {
        ChannelLayout::Mono => Layout::Mono,
        ChannelLayout::Stereo => Layout::Stereo,
        ChannelLayout::Surround51 => Layout::Surround51,
    }
}

/// One audio track's mixer settings; its mute and solo are the project track's.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackMix {
    pub fader_db: f64,
    /// -1 (left) to 1 (right).
    pub pan: f64,
    pub output: Route,
    pub sends: Vec<SendSpec>,
    /// Fader automation in dB, keys in sequence time.
    pub fader_curve: Option<Curve>,
    /// Pan automation, keys in sequence time.
    pub pan_curve: Option<Curve>,
}

impl Default for TrackMix {
    fn default() -> Self {
        Self {
            fader_db: 0.0,
            pan: 0.0,
            output: Route::Master,
            sends: Vec::new(),
            fader_curve: None,
            pan_curve: None,
        }
    }
}

/// A user bus.
#[derive(Clone, Debug, PartialEq)]
pub struct BusMix {
    /// The bus's id for meters and setting changes, unique among the buses.
    pub id: u64,
    pub fader_db: f64,
    pub pan: f64,
    pub mute: bool,
    /// The master, or a bus later in [`MixSettings::buses`].
    pub output: Route,
    pub sends: Vec<SendSpec>,
    pub fader_curve: Option<Curve>,
    pub pan_curve: Option<Curve>,
}

impl BusMix {
    /// A bus at unity, centred, to the master.
    pub fn new(id: u64) -> Self {
        Self {
            id,
            fader_db: 0.0,
            pan: 0.0,
            mute: false,
            output: Route::Master,
            sends: Vec::new(),
            fader_curve: None,
            pan_curve: None,
        }
    }
}

/// The master's settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MasterMix {
    pub fader_db: f64,
    pub fader_curve: Option<Curve>,
    /// The soft clip of the clip guard; off by default.
    pub soft_clip: bool,
}

/// Everything about a mix the project does not store. See the module docs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixSettings {
    /// Per track; a track without an entry plays at its defaults.
    pub tracks: BTreeMap<TrackId, TrackMix>,
    pub buses: Vec<BusMix>,
    pub master: MasterMix,
    /// Short ramps ([`DECLICK_SECONDS`]) at hard cuts where the waveform would jump (see
    /// [`edges`]); off by default, since they change the samples next to a cut.
    pub declick: bool,
}

impl MixSettings {
    /// The settings of `track` (its defaults when it has no entry).
    pub fn track(&self, track: TrackId) -> TrackMix {
        self.tracks.get(&track).cloned().unwrap_or_default()
    }

    /// The entry of `track`, made at its defaults when missing.
    pub fn track_mut(&mut self, track: TrackId) -> &mut TrackMix {
        self.tracks.entry(track).or_default()
    }
}

/// The audio tracks of `sequence` the mix plays, top to bottom.
pub(crate) fn audio_tracks(
    sequence: &Sequence,
) -> impl Iterator<Item = &std::sync::Arc<project::Track>> {
    sequence
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
}

/// The mixer graph description of `sequence` with `settings`, at `format`.
pub fn graph_spec(
    project: &Project,
    sequence: SequenceId,
    settings: &MixSettings,
    format: MixFormat,
    max_block: usize,
) -> Result<GraphSpec, MixError> {
    let seq = project
        .sequence(sequence)
        .ok_or(MixError::UnknownSequence(sequence))?;
    let rate = format.sample_rate;
    if rate == 0 {
        return Err(MixError::ZeroRate);
    }
    let lane = |curve: &Option<Curve>| -> Result<Option<Lane>, MixError> {
        curve.as_ref().map(|c| lane_from_curve(c, rate)).transpose()
    };
    let tracks = audio_tracks(seq)
        .map(|track| {
            let mix = settings.track(track.id);
            Ok(StripSpec {
                id: track.id.0,
                fader_db: mix.fader_db,
                pan: mix.pan,
                mute: track.muted,
                solo: track.solo,
                output: mix.output,
                sends: mix.sends.clone(),
                fader_lane: lane(&mix.fader_curve)?,
                pan_lane: lane(&mix.pan_curve)?,
            })
        })
        .collect::<Result<Vec<_>, MixError>>()?;
    let buses = settings
        .buses
        .iter()
        .map(|bus| {
            Ok(StripSpec {
                id: bus.id,
                fader_db: bus.fader_db,
                pan: bus.pan,
                mute: bus.mute,
                solo: false,
                output: bus.output,
                sends: bus.sends.clone(),
                fader_lane: lane(&bus.fader_curve)?,
                pan_lane: lane(&bus.pan_curve)?,
            })
        })
        .collect::<Result<Vec<_>, MixError>>()?;
    Ok(GraphSpec {
        sample_rate: rate,
        layout: format.layout,
        max_block,
        tracks,
        buses,
        master: MasterSpec {
            fader_db: settings.master.fader_db,
            fader_lane: lane(&settings.master.fader_curve)?,
            soft_clip: settings.master.soft_clip,
        },
    })
}

/// The sample index of `time` at `rate`, rounded to the nearest sample, halves away from
/// zero (to the later sample for the non-negative times of a timeline).
pub(crate) fn sample_of(time: Time, rate: u32) -> Result<i64, TimeError> {
    time.round_to_multiple(Time::from_samples(1, rate)?)
}

/// A mixer lane from a project curve whose keys are in sequence time and whose values are
/// numbers (see the module docs).
pub fn lane_from_curve(curve: &Curve, rate: u32) -> Result<Lane, MixError> {
    let step = ((BEZIER_STEP_SECONDS * f64::from(rate)).round() as i64).max(1);
    let mut points: Vec<Point> = Vec::with_capacity(curve.keys.len());
    let push = |points: &mut Vec<Point>, point: Point| {
        if points.last().is_none_or(|last| last.frame < point.frame) {
            points.push(point);
        }
    };
    for (i, key) in curve.keys.iter().enumerate() {
        let frame = sample_of(key.time, rate)?;
        let value = key.value.as_f64().ok_or(MixError::CurveKind)?;
        match &key.interpolation {
            Interpolation::Hold => push(&mut points, Point::hold(frame, value)),
            Interpolation::Linear => push(&mut points, Point::linear(frame, value)),
            Interpolation::Bezier { .. } => {
                push(&mut points, Point::linear(frame, value));
                if let Some(next) = curve.keys.get(i + 1) {
                    let end = sample_of(next.time, rate)?;
                    let mut f = frame + step;
                    while f < end {
                        let at = Time::from_samples(f, rate)?;
                        let v = curve
                            .value_at(at)
                            .and_then(|v| v.as_f64())
                            .ok_or(MixError::CurveKind)?;
                        push(&mut points, Point::linear(f, v));
                        f += step;
                    }
                }
            }
        }
    }
    Ok(Lane::new(points)?)
}
