//! The mixer: track strips, buses with sends, the master, meters and automation, as one
//! [`Graph`] that runs inside the audio callback (through [`Mixer`]) or offline for export
//! (called directly), with the same code and the same result.
//!
//! **Signal flow.** Every track strip takes one feed of its clips' audio, already decoded,
//! resampled to the graph's rate, mixed to its layout and scaled by clip gain and fades
//! (the engine's audio decode thread does that; crossfading clips overlap in the feed). A strip then applies its fader (dB), its pan and its
//! mute and solo, and adds the result to its output: the master or a bus. Sends add the
//! strip's signal to buses as well, before the fader (and pan) or after them, each at its
//! own level. Buses are strips too (fader, pan, mute, output, sends) and run after every
//! track in index order; a bus may only feed a bus with a higher index, so routing never
//! loops. The master sums what reaches it, applies its fader and the clip guard, and writes
//! the output frames.
//!
//! **Mute and solo.** A strip is audible when it is not muted and, if any track is soloed,
//! it is soloed itself (Decision 68: any audio solo silences the other audio tracks). A
//! silent strip's sends are silent too. Solo is a track control; buses only mute.
//!
//! **Pan law** ([`pan_gains`]): constant power, normalised to unity at the centre, so a
//! strip at its defaults passes its feed unchanged: with `θ = (pan + 1) π/4`, left gets
//! `√2 cos θ` and right `√2 sin θ` (+3 dB on one side and silence on the other at the
//! ends, and `l² + r² = 2` everywhere). Feeds are already in the graph's layout, so on a
//! stereo feed pan works as a balance. Mono ignores pan; 5.1 pans the front pair (FL, FR)
//! and the surround pair (SL, SR) by the same law and leaves FC and LFE alone.
//!
//! **Gains and automation.** Gains are evaluated at control points, every
//! [`CONTROL_FRAMES`] frames of the timeline (absolute multiples, so the result does not
//! depend on the block size), from the strip's settings and its automation [`Lane`]s for
//! fader (dB) and pan, and ramp linearly between them per sample. A change from a command
//! ramps from the current gain to the new one by the next control point, which keeps fader
//! moves and mutes free of zipper noise and clicks.
//!
//! **Clip guard.** The master never limits. Samples above full scale are counted and the
//! highest one is kept for the UI ([`MeterBank::clip`]); with [`MasterSpec::soft_clip`] on
//! (off by default) the output passes a soft clipper that leaves everything up to -1 dBFS
//! untouched and bends the rest towards full scale.
//!
//! **Fades** ([`fade`]): the crossfade and fade curves the engine applies to clips on
//! their way into a feed (equal power, linear, S-curve), sample by sample.

mod automation;
pub mod fade;
mod graph;
pub mod meter;
mod realtime;

pub use automation::{Lane, LaneError, Point, Shape};
pub use fade::FadeCurve;
pub use graph::Graph;
pub use meter::{ClipReading, MeterBank, MeterReading};
pub use realtime::{
    Feed, FeedConsumer, Mixer, MixerConfig, MixerControl, MixerError, MixerStatus, Param, StripRef,
    SwapAt,
};

/// Frames between control points, where gains and automation are evaluated.
pub const CONTROL_FRAMES: usize = 32;

/// The most channels a layout has (5.1).
pub const MAX_CHANNELS: usize = 6;

/// Fader, send and clip gains at or below this are silence.
pub const SILENT_DB: f64 = -96.0;

/// The highest fader and send gain.
pub const MAX_GAIN_DB: f64 = 12.0;

/// The soft clipper's knee: -1 dBFS.
pub const SOFT_CLIP_KNEE: f32 = 0.891_250_9;

/// The speaker layout a graph mixes to, the same layouts as the project's and `media`'s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Layout {
    Mono,
    #[default]
    Stereo,
    /// FL, FR, FC, LFE, SL, SR.
    Surround51,
}

impl Layout {
    pub fn channels(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Surround51 => 6,
        }
    }

    /// Mono for one channel, stereo for two (or none), 5.1 for more.
    pub fn for_channels(channels: usize) -> Self {
        match channels {
            1 => Self::Mono,
            0 | 2 => Self::Stereo,
            _ => Self::Surround51,
        }
    }
}

/// A gain in dB as a linear factor: silence at or below [`SILENT_DB`].
pub fn db_to_gain(db: f64) -> f32 {
    if db.is_nan() || db <= SILENT_DB {
        0.0
    } else {
        10f64.powf(db / 20.0) as f32
    }
}

/// The constant-power pan law, unity at the centre: `(left, right)` for `pan` from -1
/// (left) to 1 (right). See the module docs.
pub fn pan_law(pan: f64) -> (f32, f32) {
    let pan = if pan.is_nan() {
        0.0
    } else {
        pan.clamp(-1.0, 1.0)
    };
    let theta = (pan + 1.0) * std::f64::consts::FRAC_PI_4;
    let root2 = std::f64::consts::SQRT_2;
    ((root2 * theta.cos()) as f32, (root2 * theta.sin()) as f32)
}

/// Per-channel pan gains of `layout` (unity on channels pan leaves alone).
pub fn pan_gains(layout: Layout, pan: f64) -> [f32; MAX_CHANNELS] {
    let mut gains = [1.0; MAX_CHANNELS];
    let (l, r) = pan_law(pan);
    match layout {
        Layout::Mono => {}
        Layout::Stereo => {
            gains[0] = l;
            gains[1] = r;
        }
        Layout::Surround51 => {
            gains[0] = l;
            gains[1] = r;
            gains[4] = l;
            gains[5] = r;
        }
    }
    gains
}

/// The soft clipper: identity up to [`SOFT_CLIP_KNEE`], then a tanh curve that meets it
/// with the same slope and approaches full scale.
pub fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= SOFT_CLIP_KNEE {
        return x;
    }
    let room = 1.0 - SOFT_CLIP_KNEE;
    (SOFT_CLIP_KNEE + room * ((a - SOFT_CLIP_KNEE) / room).tanh()).copysign(x)
}

/// Where a strip's output goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Route {
    #[default]
    Master,
    /// The bus at this index of [`GraphSpec::buses`].
    Bus(usize),
}

/// A send from a strip to a bus.
#[derive(Clone, Debug, PartialEq)]
pub struct SendSpec {
    /// Index into [`GraphSpec::buses`].
    pub bus: usize,
    pub level_db: f64,
    /// Taken before the fader and pan (the strip's feed times the level), else after them.
    pub pre_fader: bool,
}

/// A track strip or a bus.
#[derive(Clone, Debug, PartialEq)]
pub struct StripSpec {
    /// The caller's id (the engine uses the track id), for meters and commands.
    pub id: u64,
    pub fader_db: f64,
    /// -1 (left) to 1 (right).
    pub pan: f64,
    pub mute: bool,
    /// Tracks only; ignored on buses.
    pub solo: bool,
    pub output: Route,
    pub sends: Vec<SendSpec>,
    /// Fader automation in dB; while set, `fader_db` is ignored.
    pub fader_lane: Option<Lane>,
    /// Pan automation; while set, `pan` is ignored.
    pub pan_lane: Option<Lane>,
}

impl StripSpec {
    /// A strip at unity, centred, to the master.
    pub fn new(id: u64) -> Self {
        Self {
            id,
            fader_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            output: Route::Master,
            sends: Vec::new(),
            fader_lane: None,
            pan_lane: None,
        }
    }
}

/// The master strip.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MasterSpec {
    pub fader_db: f64,
    pub fader_lane: Option<Lane>,
    /// Soft clip above -1 dBFS; off by default (export stays float and unlimited).
    pub soft_clip: bool,
}

/// Everything a [`Graph`] is built from.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphSpec {
    pub sample_rate: u32,
    pub layout: Layout,
    /// The most frames one [`Graph::process`] call takes; buffers are sized for it.
    pub max_block: usize,
    /// Track strips, each with one feed, in feed order.
    pub tracks: Vec<StripSpec>,
    pub buses: Vec<StripSpec>,
    pub master: MasterSpec,
}

/// Why a graph could not be built.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum GraphError {
    #[error("the sample rate is zero")]
    ZeroRate,
    #[error("the block size is zero")]
    ZeroBlock,
    #[error("strip {strip} routes to bus {bus}, which does not exist")]
    UnknownBus { strip: u64, bus: usize },
    #[error("bus {bus} routes to bus {to}; a bus may only feed a bus with a higher index")]
    BusOrder { bus: usize, to: usize },
    #[error("strip {strip}: {what} is not a number")]
    NotANumber { strip: u64, what: &'static str },
    #[error("strip {strip} has more than {max} sends")]
    TooManySends { strip: u64, max: usize },
    #[error("two strips share the id {0}")]
    DuplicateId(u64),
}
