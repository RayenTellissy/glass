//! Meters: peak, RMS and peak hold per channel for every track strip, bus and the master,
//! published by the graph about 60 times a second and read by the UI without locking.
//!
//! The graph accumulates each meter over a period of [`publish_period`] frames (a whole
//! number of control segments near a sixtieth of a second: 800 frames at 48 kHz), then
//! stores the period's peak (largest absolute sample), RMS and the held peak into atomics.
//! The hold keeps the highest peak for [`HOLD_SECONDS`], then drops to the current peak.
//! A reader may see one value from a newer period than another; for a meter that is fine.
//!
//! The master also reports clipping: samples above full scale before the clip guard and the
//! highest absolute sample since the last [`MeterBank::reset_clip`].

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::{CONTROL_FRAMES, MAX_CHANNELS};

/// How long the peak hold keeps a peak.
pub const HOLD_SECONDS: f64 = 1.5;

/// Publications per second, roughly (the UI's frame rate).
pub const PUBLISH_HZ: u32 = 60;

/// Frames per meter period at `sample_rate`: the nearest whole number of control segments
/// to `sample_rate / 60` (800 at 48 kHz, 736 at 44.1 kHz), at least one.
pub fn publish_period(sample_rate: u32) -> usize {
    let target = f64::from(sample_rate) / f64::from(PUBLISH_HZ);
    ((target / CONTROL_FRAMES as f64).round() as usize).max(1) * CONTROL_FRAMES
}

/// One meter's latest values, linear (1.0 is full scale), per channel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterReading {
    pub channels: usize,
    pub peak: [f32; MAX_CHANNELS],
    pub rms: [f32; MAX_CHANNELS],
    pub hold: [f32; MAX_CHANNELS],
}

/// What the master's clip guard saw since the last reset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClipReading {
    /// Samples above full scale (absolute value over 1.0) before the guard.
    pub overs: u64,
    /// The highest absolute sample before the guard.
    pub max_peak: f32,
}

#[derive(Debug, Default)]
struct Slot {
    peak: [AtomicU32; MAX_CHANNELS],
    rms: [AtomicU32; MAX_CHANNELS],
    hold: [AtomicU32; MAX_CHANNELS],
}

/// The published meters of one graph. Shared (`Arc`) between the graph, which writes it,
/// and any number of readers. Each graph has its own bank, made with it, so a reader takes
/// the new bank whenever the graph changes.
#[derive(Debug)]
pub struct MeterBank {
    channels: usize,
    track_ids: Vec<u64>,
    bus_ids: Vec<u64>,
    /// Tracks, then buses, then the master.
    slots: Vec<Slot>,
    publications: AtomicU64,
    overs: AtomicU64,
    max_peak: AtomicU32,
}

impl MeterBank {
    pub(crate) fn new(channels: usize, track_ids: Vec<u64>, bus_ids: Vec<u64>) -> Self {
        let count = track_ids.len() + bus_ids.len() + 1;
        Self {
            channels,
            track_ids,
            bus_ids,
            slots: (0..count).map(|_| Slot::default()).collect(),
            publications: AtomicU64::new(0),
            overs: AtomicU64::new(0),
            max_peak: AtomicU32::new(0),
        }
    }

    /// Channels per meter (the graph's layout).
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The meter of the track strip with this id.
    pub fn track(&self, id: u64) -> Option<MeterReading> {
        let index = self.track_ids.iter().position(|&t| t == id)?;
        Some(self.read(index))
    }

    /// The meter of the bus with this id.
    pub fn bus(&self, id: u64) -> Option<MeterReading> {
        let index = self.bus_ids.iter().position(|&b| b == id)?;
        Some(self.read(self.track_ids.len() + index))
    }

    /// The master meter, after the clip guard.
    pub fn master(&self) -> MeterReading {
        self.read(self.slots.len() - 1)
    }

    /// How many periods have been published (a reader can skip unchanged values).
    pub fn publications(&self) -> u64 {
        self.publications.load(Ordering::Acquire)
    }

    pub fn clip(&self) -> ClipReading {
        ClipReading {
            overs: self.overs.load(Ordering::Relaxed),
            max_peak: f32::from_bits(self.max_peak.load(Ordering::Relaxed)),
        }
    }

    /// Clears the clip report (the UI's clip light).
    pub fn reset_clip(&self) {
        self.overs.store(0, Ordering::Relaxed);
        self.max_peak.store(0, Ordering::Relaxed);
    }

    fn read(&self, index: usize) -> MeterReading {
        let slot = &self.slots[index];
        let mut reading = MeterReading {
            channels: self.channels,
            ..MeterReading::default()
        };
        for c in 0..self.channels {
            reading.peak[c] = f32::from_bits(slot.peak[c].load(Ordering::Relaxed));
            reading.rms[c] = f32::from_bits(slot.rms[c].load(Ordering::Relaxed));
            reading.hold[c] = f32::from_bits(slot.hold[c].load(Ordering::Relaxed));
        }
        reading
    }
}

/// The graph's side of one meter: accumulators for the current period and the hold.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Accumulator {
    peak: [f32; MAX_CHANNELS],
    sum_squares: [f64; MAX_CHANNELS],
    hold: [f32; MAX_CHANNELS],
    hold_age: [usize; MAX_CHANNELS],
}

impl Accumulator {
    /// Adds `samples` (one channel, one segment).
    #[inline]
    pub(crate) fn add(&mut self, channel: usize, samples: &[f32]) {
        let mut peak = self.peak[channel];
        let mut sum = 0.0f64;
        for &s in samples {
            peak = peak.max(s.abs());
            sum += f64::from(s) * f64::from(s);
        }
        self.peak[channel] = peak;
        self.sum_squares[channel] += sum;
    }

    /// Adds a segment already measured: its peak and sum of squares.
    #[inline]
    pub(crate) fn add_measured(&mut self, channel: usize, peak: f32, sum_squares: f64) {
        self.peak[channel] = self.peak[channel].max(peak);
        self.sum_squares[channel] += sum_squares;
    }
}

/// The graph's side of a bank: every accumulator and the period count.
#[derive(Debug)]
pub(crate) struct Meters {
    pub(crate) accumulators: Vec<Accumulator>,
    pub(crate) frames: usize,
    pub(crate) period: usize,
    hold_frames: usize,
}

impl Meters {
    pub(crate) fn new(slots: usize, sample_rate: u32) -> Self {
        Self {
            accumulators: vec![Accumulator::default(); slots],
            frames: 0,
            period: publish_period(sample_rate),
            hold_frames: (HOLD_SECONDS * f64::from(sample_rate)).round() as usize,
        }
    }

    /// Frames left in the current period: the graph ends a segment there, so every period
    /// is exactly [`publish_period`] frames long.
    pub(crate) fn remaining(&self) -> usize {
        self.period - self.frames
    }

    /// Counts `frames` more frames into the period and publishes when it is complete.
    pub(crate) fn advance(&mut self, frames: usize, bank: &MeterBank) {
        self.frames += frames;
        if self.frames < self.period {
            return;
        }
        let n = self.frames as f64;
        for (acc, slot) in self.accumulators.iter_mut().zip(&bank.slots) {
            for c in 0..bank.channels {
                let peak = acc.peak[c];
                let rms = (acc.sum_squares[c] / n).sqrt() as f32;
                if peak >= acc.hold[c] || acc.hold_age[c] >= self.hold_frames {
                    acc.hold[c] = peak;
                    acc.hold_age[c] = 0;
                } else {
                    acc.hold_age[c] += self.frames;
                }
                slot.peak[c].store(peak.to_bits(), Ordering::Relaxed);
                slot.rms[c].store(rms.to_bits(), Ordering::Relaxed);
                slot.hold[c].store(acc.hold[c].to_bits(), Ordering::Relaxed);
                acc.peak[c] = 0.0;
                acc.sum_squares[c] = 0.0;
            }
        }
        self.frames = 0;
        bank.publications.fetch_add(1, Ordering::Release);
    }
}

/// Records the master's samples before the clip guard. Single writer (the graph).
#[inline]
pub(crate) fn note_clip(bank: &MeterBank, samples: &[f32]) {
    let mut overs = 0u64;
    let mut peak = 0.0f32;
    for &s in samples {
        let a = s.abs();
        peak = peak.max(a);
        if a > 1.0 {
            overs += 1;
        }
    }
    if overs > 0 {
        bank.overs.fetch_add(overs, Ordering::Relaxed);
    }
    if peak > f32::from_bits(bank.max_peak.load(Ordering::Relaxed)) {
        bank.max_peak.store(peak.to_bits(), Ordering::Relaxed);
    }
}
