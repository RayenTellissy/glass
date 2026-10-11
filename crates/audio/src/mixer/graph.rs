//! The mixer graph: built off the audio thread from a [`GraphSpec`] with every buffer it
//! needs, then processed block by block without allocating, locking or waiting.

use std::sync::Arc;

use super::meter::{self, Accumulator, MeterBank, Meters};
use super::{
    CONTROL_FRAMES, GraphError, GraphSpec, Lane, Layout, MAX_CHANNELS, MAX_GAIN_DB, Param, Route,
    StripRef, StripSpec, db_to_gain, pan_gains, soft_clip,
};

/// Per-channel gains.
type Gains = [f32; MAX_CHANNELS];

#[derive(Debug)]
struct Send {
    bus: usize,
    level_db: f64,
    pre_fader: bool,
    gain: Gains,
}

#[derive(Debug)]
struct Strip {
    id: u64,
    fader_db: f64,
    pan: f64,
    mute: bool,
    solo: bool,
    output: Route,
    sends: Vec<Send>,
    fader_lane: Option<Lane>,
    pan_lane: Option<Lane>,
    /// The output gain per channel at the current frame (the ramp's position).
    gain: Gains,
}

/// A strip's gains at one control point.
struct Targets {
    out: Gains,
    sends: [Gains; MAX_SENDS],
}

/// Sends per strip whose targets live on the stack; more are refused at build time.
const MAX_SENDS: usize = 16;

impl Strip {
    fn new(spec: &StripSpec) -> Self {
        Self {
            id: spec.id,
            fader_db: spec.fader_db,
            pan: spec.pan,
            mute: spec.mute,
            solo: spec.solo,
            output: spec.output,
            sends: spec
                .sends
                .iter()
                .map(|s| Send {
                    bus: s.bus,
                    level_db: s.level_db,
                    pre_fader: s.pre_fader,
                    gain: [0.0; MAX_CHANNELS],
                })
                .collect(),
            fader_lane: spec.fader_lane.clone(),
            pan_lane: spec.pan_lane.clone(),
            gain: [0.0; MAX_CHANNELS],
        }
    }

    /// The gains at timeline frame `frame` for a strip that is audible or not.
    fn targets(&self, frame: i64, layout: Layout, audible: bool) -> Targets {
        let mut targets = Targets {
            out: [0.0; MAX_CHANNELS],
            sends: [[0.0; MAX_CHANNELS]; MAX_SENDS],
        };
        if !audible {
            return targets;
        }
        let fader_db = self
            .fader_lane
            .as_ref()
            .map_or(self.fader_db, |l| l.value_at(frame))
            .min(MAX_GAIN_DB);
        let fader = db_to_gain(fader_db);
        let pan = self
            .pan_lane
            .as_ref()
            .map_or(self.pan, |l| l.value_at(frame));
        let pans = pan_gains(layout, pan);
        let channels = layout.channels();
        for (out, pan) in targets.out.iter_mut().zip(pans).take(channels) {
            *out = fader * pan;
        }
        for (send, out) in self.sends.iter().zip(targets.sends.iter_mut()) {
            let level = db_to_gain(send.level_db.min(MAX_GAIN_DB));
            for (gain, strip) in out.iter_mut().zip(targets.out).take(channels) {
                *gain = if send.pre_fader { level } else { level * strip };
            }
        }
        targets
    }

    /// Sets every gain to its value at `position`, between the control points around it.
    fn prime(&mut self, position: i64, layout: Layout, audible: bool) {
        let control = CONTROL_FRAMES as i64;
        let before = position.div_euclid(control) * control;
        let x = (position - before) as f32 / CONTROL_FRAMES as f32;
        let a = self.targets(before, layout, audible);
        let b = self.targets(before + control, layout, audible);
        for c in 0..MAX_CHANNELS {
            self.gain[c] = a.out[c] + (b.out[c] - a.out[c]) * x;
        }
        for (i, send) in self.sends.iter_mut().enumerate() {
            for c in 0..MAX_CHANNELS {
                send.gain[c] = a.sends[i][c] + (b.sends[i][c] - a.sends[i][c]) * x;
            }
        }
    }

    fn set(&mut self, param: Param) -> bool {
        match param {
            Param::Fader(db) => self.fader_db = db,
            Param::Pan(pan) => self.pan = pan,
            Param::Mute(mute) => self.mute = mute,
            Param::Solo(solo) => self.solo = solo,
            Param::SendLevel { send, level_db } => match self.sends.get_mut(send) {
                Some(s) => s.level_db = level_db,
                None => return false,
            },
        }
        true
    }
}

/// One segment of a block: frames `offset..offset + len` of the block, at timeline frames
/// from `frame`, with the next control point `span` frames after `frame`.
#[derive(Clone, Copy)]
struct Segment {
    offset: usize,
    len: usize,
    span: usize,
    control: i64,
}

/// Multiplies `input[offset..offset + len]` by a gain ramping from `*gain` towards
/// `target` (reached `span` frames after `offset`) and adds it to `dest`. Returns the
/// peak and sum of squares of what was added when `measure` is set.
#[inline]
fn ramp_add(
    input: &[f32],
    dest: &mut [f32],
    gain: &mut f32,
    target: f32,
    seg: Segment,
    measure: bool,
) -> (f32, f64) {
    let g0 = *gain;
    let step = (target - g0) / seg.span as f32;
    let (mut peak, mut sum) = (0.0f32, 0.0f64);
    let input = &input[seg.offset..seg.offset + seg.len];
    let dest = &mut dest[seg.offset..seg.offset + seg.len];
    if step == 0.0 {
        if g0 != 0.0 || measure {
            for (d, &x) in dest.iter_mut().zip(input) {
                let y = x * g0;
                *d += y;
                peak = peak.max(y.abs());
                sum += f64::from(y) * f64::from(y);
            }
        }
    } else {
        for (k, (d, &x)) in dest.iter_mut().zip(input).enumerate() {
            let y = x * (g0 + step * k as f32);
            *d += y;
            peak = peak.max(y.abs());
            sum += f64::from(y) * f64::from(y);
        }
    }
    *gain = if seg.len == seg.span {
        target
    } else {
        g0 + step * seg.len as f32
    };
    (peak, sum)
}

/// The planar slice of channel `c` of a buffer of `max_block`-frame planes.
#[inline]
fn plane(buffer: &mut [f32], c: usize, max_block: usize) -> &mut [f32] {
    &mut buffer[c * max_block..(c + 1) * max_block]
}

/// A built mixer graph. See the [module docs](super).
#[derive(Debug)]
pub struct Graph {
    sample_rate: u32,
    layout: Layout,
    channels: usize,
    max_block: usize,
    tracks: Vec<Strip>,
    buses: Vec<Strip>,
    master_fader_db: f64,
    master_lane: Option<Lane>,
    soft_clip: bool,
    master_gain: f32,
    /// Interleaved feed frames, `width()` samples each: the tracks' channels in order.
    input: Vec<f32>,
    /// One strip's input, planar.
    work: Vec<f32>,
    /// Each bus's sum, planar, bus after bus.
    bus_sums: Vec<f32>,
    master_sum: Vec<f32>,
    bank: Arc<MeterBank>,
    meters: Meters,
    primed: bool,
}

impl Graph {
    /// Builds a graph and allocates everything it will use.
    pub fn build(spec: &GraphSpec) -> Result<Self, GraphError> {
        validate(spec)?;
        let channels = spec.layout.channels();
        let max_block = spec.max_block;
        let bank = Arc::new(MeterBank::new(
            channels,
            spec.tracks.iter().map(|t| t.id).collect(),
            spec.buses.iter().map(|b| b.id).collect(),
        ));
        Ok(Self {
            sample_rate: spec.sample_rate,
            layout: spec.layout,
            channels,
            max_block,
            tracks: spec.tracks.iter().map(Strip::new).collect(),
            buses: spec.buses.iter().map(Strip::new).collect(),
            master_fader_db: spec.master.fader_db,
            master_lane: spec.master.fader_lane.clone(),
            soft_clip: spec.master.soft_clip,
            master_gain: 0.0,
            input: vec![0.0; max_block * spec.tracks.len() * channels],
            work: vec![0.0; max_block * channels],
            bus_sums: vec![0.0; max_block * channels * spec.buses.len()],
            master_sum: vec![0.0; max_block * channels],
            meters: Meters::new(spec.tracks.len() + spec.buses.len() + 1, spec.sample_rate),
            bank,
            primed: false,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn max_block(&self) -> usize {
        self.max_block
    }

    pub fn tracks(&self) -> usize {
        self.tracks.len()
    }

    /// Samples per feed frame: every track's channels.
    pub fn width(&self) -> usize {
        self.tracks.len() * self.channels
    }

    /// This graph's meters, for the UI.
    pub fn meters(&self) -> Arc<MeterBank> {
        Arc::clone(&self.bank)
    }

    /// The feed buffer for the next [`process`](Self::process) call: `frames` interleaved
    /// frames of [`width`](Self::width) samples (track 0's channels, then track 1's, ...).
    pub fn input_mut(&mut self, frames: usize) -> &mut [f32] {
        let n = frames.min(self.max_block) * self.width();
        &mut self.input[..n]
    }

    /// Changes a setting of a strip, ramping to it by the next control point. Returns
    /// false for an unknown strip or send.
    pub fn set(&mut self, strip: StripRef, param: Param) -> bool {
        let find = |strips: &mut Vec<Strip>, id: u64| strips.iter_mut().position(|s| s.id == id);
        match strip {
            StripRef::Track(id) => match find(&mut self.tracks, id) {
                Some(i) => self.tracks[i].set(param),
                None => false,
            },
            StripRef::Bus(id) => match find(&mut self.buses, id) {
                Some(i) => self.buses[i].set(param),
                None => false,
            },
            StripRef::Master => match param {
                Param::Fader(db) => {
                    self.master_fader_db = db;
                    true
                }
                _ => false,
            },
        }
    }

    fn master_target(&self, frame: i64) -> f32 {
        let db = self
            .master_lane
            .as_ref()
            .map_or(self.master_fader_db, |l| l.value_at(frame));
        db_to_gain(db.min(MAX_GAIN_DB))
    }

    fn prime(&mut self, position: i64) {
        let any_solo = self.tracks.iter().any(|t| t.solo);
        for track in &mut self.tracks {
            let audible = !track.mute && (!any_solo || track.solo);
            track.prime(position, self.layout, audible);
        }
        for bus in &mut self.buses {
            let audible = !bus.mute;
            bus.prime(position, self.layout, audible);
        }
        let control = CONTROL_FRAMES as i64;
        let before = position.div_euclid(control) * control;
        let x = (position - before) as f32 / CONTROL_FRAMES as f32;
        let (a, b) = (
            self.master_target(before),
            self.master_target(before + control),
        );
        self.master_gain = a + (b - a) * x;
        self.primed = true;
    }

    /// Mixes `frames` frames (at most [`max_block`](Self::max_block)) of the feed in
    /// [`input_mut`](Self::input_mut), starting at timeline frame `position`, into `out`
    /// (interleaved, [`channels`](Self::channels) per frame). Real-time safe.
    pub fn process(&mut self, position: i64, frames: usize, out: &mut [f32]) {
        let frames = frames
            .min(self.max_block)
            .min(out.len() / self.channels.max(1));
        if !self.primed {
            self.prime(position);
        }
        let channels = self.channels;
        let max_block = self.max_block;
        let layout = self.layout;
        let width = self.width();
        let track_count = self.tracks.len();
        for c in 0..channels {
            plane(&mut self.master_sum, c, max_block)[..frames].fill(0.0);
        }
        for b in 0..self.buses.len() {
            for c in 0..channels {
                let base = b * channels * max_block;
                plane(&mut self.bus_sums[base..], c, max_block)[..frames].fill(0.0);
            }
        }
        let any_solo = self.tracks.iter().any(|t| t.solo);
        let control = CONTROL_FRAMES as i64;

        let mut offset = 0;
        while offset < frames {
            let at = position + offset as i64;
            let span = (control - at.rem_euclid(control)) as usize;
            let len = span.min(frames - offset).min(self.meters.remaining());
            let seg = Segment {
                offset,
                len,
                span,
                control: at + span as i64,
            };

            for t in 0..track_count {
                for c in 0..channels {
                    let work = &mut plane(&mut self.work, c, max_block)[offset..offset + len];
                    let feed = self.input[offset * width + t * channels + c..]
                        .iter()
                        .step_by(width);
                    for (w, &x) in work.iter_mut().zip(feed) {
                        *w = x;
                    }
                }
                let strip = &mut self.tracks[t];
                let audible = !strip.mute && (!any_solo || strip.solo);
                run_strip(
                    strip,
                    seg,
                    layout,
                    audible,
                    &self.work,
                    &mut self.bus_sums,
                    &mut self.master_sum,
                    &mut self.meters.accumulators[t],
                    max_block,
                );
            }
            for b in 0..self.buses.len() {
                let base = b * channels * max_block;
                for c in 0..channels {
                    let from = base + c * max_block + offset;
                    self.work[c * max_block + offset..c * max_block + offset + len]
                        .copy_from_slice(&self.bus_sums[from..from + len]);
                }
                let strip = &mut self.buses[b];
                let audible = !strip.mute;
                run_strip(
                    strip,
                    seg,
                    layout,
                    audible,
                    &self.work,
                    &mut self.bus_sums,
                    &mut self.master_sum,
                    &mut self.meters.accumulators[track_count + b],
                    max_block,
                );
            }

            // The master: fader ramp, clip report, guard, meter, interleave.
            let target = self.master_target(seg.control);
            let g0 = self.master_gain;
            let step = (target - g0) / span as f32;
            let master_meter = self.meters.accumulators.len() - 1;
            for c in 0..channels {
                let sum = &mut plane(&mut self.master_sum, c, max_block)[offset..offset + len];
                for (k, s) in sum.iter_mut().enumerate() {
                    *s *= g0 + step * k as f32;
                }
                meter::note_clip(&self.bank, sum);
                if self.soft_clip {
                    for s in sum.iter_mut() {
                        *s = soft_clip(*s);
                    }
                }
                self.meters.accumulators[master_meter].add(c, sum);
                for (k, &s) in sum.iter().enumerate() {
                    out[(offset + k) * channels + c] = s;
                }
            }
            self.master_gain = if len == span {
                target
            } else {
                g0 + step * len as f32
            };
            self.meters.advance(len, &self.bank);
            offset += len;
        }
    }

    /// Mixes any number of interleaved feed frames from `input` into `out`, block by
    /// block, starting at timeline frame `position`: the offline path (export, tests).
    pub fn mix(&mut self, position: i64, input: &[f32], out: &mut [f32]) {
        let width = self.width();
        let frames =
            (out.len() / self.channels).min(input.len().checked_div(width).unwrap_or(usize::MAX));
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(self.max_block);
            self.input_mut(n)
                .copy_from_slice(&input[done * width..(done + n) * width]);
            let out = &mut out[done * self.channels..(done + n) * self.channels];
            self.process(position + done as i64, n, out);
            done += n;
        }
    }

    /// Counts `frames` frames of silence into the meters (while paused), so they fall.
    pub fn idle(&mut self, frames: usize) {
        let mut left = frames;
        while left > 0 {
            let n = left.min(self.meters.remaining());
            self.meters.advance(n, &self.bank);
            left -= n;
        }
    }
}

/// Runs one strip over one segment: its input is in `work` (planar).
#[allow(clippy::too_many_arguments)]
fn run_strip(
    strip: &mut Strip,
    seg: Segment,
    layout: Layout,
    audible: bool,
    work: &[f32],
    bus_sums: &mut [f32],
    master_sum: &mut [f32],
    meter: &mut Accumulator,
    max_block: usize,
) {
    let channels = layout.channels();
    let targets = strip.targets(seg.control, layout, audible);
    for c in 0..channels {
        let input = &work[c * max_block..(c + 1) * max_block];
        let dest = match strip.output {
            Route::Master => plane(master_sum, c, max_block),
            Route::Bus(b) => plane(&mut bus_sums[b * channels * max_block..], c, max_block),
        };
        let (peak, sum) = ramp_add(input, dest, &mut strip.gain[c], targets.out[c], seg, true);
        meter.add_measured(c, peak, sum);
        for (i, send) in strip.sends.iter_mut().enumerate() {
            let dest = plane(
                &mut bus_sums[send.bus * channels * max_block..],
                c,
                max_block,
            );
            ramp_add(
                input,
                dest,
                &mut send.gain[c],
                targets.sends[i][c],
                seg,
                false,
            );
        }
    }
}

fn validate(spec: &GraphSpec) -> Result<(), GraphError> {
    if spec.sample_rate == 0 {
        return Err(GraphError::ZeroRate);
    }
    if spec.max_block == 0 {
        return Err(GraphError::ZeroBlock);
    }
    let buses = spec.buses.len();
    let number = |strip: u64, what, v: f64| {
        if v.is_nan() {
            Err(GraphError::NotANumber { strip, what })
        } else {
            Ok(())
        }
    };
    let mut ids: Vec<(bool, u64)> = Vec::new();
    for (index, (is_bus, strip)) in spec
        .tracks
        .iter()
        .map(|t| (false, t))
        .chain(spec.buses.iter().map(|b| (true, b)))
        .enumerate()
    {
        if ids.contains(&(is_bus, strip.id)) {
            return Err(GraphError::DuplicateId(strip.id));
        }
        ids.push((is_bus, strip.id));
        number(strip.id, "the fader", strip.fader_db)?;
        number(strip.id, "the pan", strip.pan)?;
        if strip.sends.len() > MAX_SENDS {
            return Err(GraphError::TooManySends {
                strip: strip.id,
                max: MAX_SENDS,
            });
        }
        // A bus's index among the buses; tracks may feed any bus.
        let own = is_bus.then(|| index - spec.tracks.len());
        let targets = strip
            .sends
            .iter()
            .map(|s| {
                number(strip.id, "a send level", s.level_db)?;
                Ok(s.bus)
            })
            .chain(match strip.output {
                Route::Bus(b) => Some(Ok(b)),
                Route::Master => None,
            });
        for target in targets {
            let bus = target?;
            if bus >= buses {
                return Err(GraphError::UnknownBus {
                    strip: strip.id,
                    bus,
                });
            }
            if let Some(own) = own
                && bus <= own
            {
                return Err(GraphError::BusOrder { bus: own, to: bus });
            }
        }
    }
    number(0, "the master fader", spec.master.fader_db)
}
