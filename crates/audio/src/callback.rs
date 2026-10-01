//! The real-time half: the code that runs inside the audio callback, shared by the device
//! and offline backends, and [`Feeder`], its non-real-time producer.
//!
//! Audio travels as interleaved f32 frames through one `rtrb` ring. The callback copies
//! what the ring holds into the device buffer, fills any shortfall with silence (counting
//! an underrun while the feeder is feeding), advances the frame counter and, when the
//! backend has timestamps, updates the drift estimator and publishes its line. It never
//! allocates, locks, logs or waits.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::clock::Shared;
use crate::drift::DriftEstimator;

/// Timing for one callback, in seconds on the clock's monotonic timeline.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// When the callback runs.
    pub(crate) now: f64,
    /// From `now` until the callback's first frame is audible.
    pub(crate) latency: f64,
}

/// The callback's state. Owned by the audio thread once the stream starts.
pub(crate) struct Callback {
    consumer: Consumer<f32>,
    shared: Arc<Shared>,
    feed_channels: usize,
    device_channels: usize,
    estimator: DriftEstimator,
    frames: u64,
}

/// Creates the ring and both ends: the callback state and its feeder.
pub(crate) fn channel(
    shared: Arc<Shared>,
    feed_channels: usize,
    device_channels: usize,
    ring_frames: usize,
) -> (Callback, Feeder) {
    let feed_channels = feed_channels.max(1);
    let (producer, consumer) = RingBuffer::new(ring_frames.max(1) * feed_channels);
    let callback = Callback {
        consumer,
        estimator: DriftEstimator::new(shared.sample_rate),
        shared: Arc::clone(&shared),
        feed_channels,
        device_channels: device_channels.max(1),
        frames: 0,
    };
    let feeder = Feeder {
        producer,
        shared,
        channels: feed_channels,
    };
    (callback, feeder)
}

impl Callback {
    /// Fills `out` (interleaved, `device_channels` per frame). Real-time safe.
    pub(crate) fn process(&mut self, out: &mut [f32], timing: Option<Timing>) {
        let frames = out.len() / self.device_channels;
        if let Some(timing) = timing {
            self.estimator
                .observe(timing.now + timing.latency, self.frames, timing.now);
            self.shared.publish_line(self.estimator.line());
            self.shared.latency_ns.store(
                (timing.latency.max(0.0) * 1e9).round() as u64,
                Ordering::Relaxed,
            );
            self.shared
                .rate_ratio
                .store(self.estimator.rate_ratio().to_bits(), Ordering::Relaxed);
        }

        let available = self.consumer.slots() / self.feed_channels;
        let take = available.min(frames);
        let mut written = 0;
        if let Ok(chunk) = self.consumer.read_chunk(take * self.feed_channels) {
            let (first, second) = chunk.as_slices();
            if self.feed_channels == self.device_channels {
                out[..first.len()].copy_from_slice(first);
                out[first.len()..first.len() + second.len()].copy_from_slice(second);
            } else {
                let mut samples = first.iter().chain(second.iter());
                for frame in out.chunks_exact_mut(self.device_channels).take(take) {
                    for c in 0..self.feed_channels {
                        let sample = samples.next().copied().unwrap_or(0.0);
                        if let Some(slot) = frame.get_mut(c) {
                            *slot = sample;
                        }
                    }
                    for slot in frame.iter_mut().skip(self.feed_channels) {
                        *slot = 0.0;
                    }
                }
            }
            chunk.commit_all();
            written = take;
        }
        out[written * self.device_channels..].fill(0.0);
        if written < frames && self.shared.feeding.load(Ordering::Relaxed) {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            self.shared
                .silent_frames
                .fetch_add((frames - written) as u64, Ordering::Relaxed);
        }

        self.frames += frames as u64;
        self.shared.frames.store(self.frames, Ordering::Release);
    }
}

/// The producer end of the callback's ring, for one non-real-time thread (the audio
/// decode thread). Never blocks: a full ring takes fewer frames than offered.
pub struct Feeder {
    producer: Producer<f32>,
    shared: Arc<Shared>,
    channels: usize,
}

impl Feeder {
    /// Channels per frame the ring carries.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frames that fit in the ring right now.
    pub fn free_frames(&self) -> usize {
        self.producer.slots() / self.channels
    }

    /// Frames waiting in the ring (an estimate: the callback may be draining it).
    pub fn queued_frames(&self) -> usize {
        (self.producer.buffer().capacity() - self.producer.slots()) / self.channels
    }

    /// Queues planar audio (one slice per channel, as `media::AudioReader` writes it) and
    /// returns how many frames fit. Extra planes are ignored; missing ones queue silence.
    pub fn push_planar(&mut self, planes: &[&[f32]]) -> usize {
        let offered = planes.iter().map(|p| p.len()).min().unwrap_or(0);
        let frames = offered.min(self.free_frames());
        let Ok(mut chunk) = self.producer.write_chunk(frames * self.channels) else {
            return 0;
        };
        let (first, second) = chunk.as_mut_slices();
        for (i, slot) in first.iter_mut().chain(second.iter_mut()).enumerate() {
            let (frame, channel) = (i / self.channels, i % self.channels);
            *slot = planes.get(channel).map_or(0.0, |p| p[frame]);
        }
        chunk.commit_all();
        frames
    }

    /// Queues interleaved audio (`channels()` samples per frame) and returns how many whole
    /// frames fit.
    pub fn push_interleaved(&mut self, samples: &[f32]) -> usize {
        let frames = (samples.len() / self.channels).min(self.free_frames());
        let Ok(mut chunk) = self.producer.write_chunk(frames * self.channels) else {
            return 0;
        };
        let (first, second) = chunk.as_mut_slices();
        let split = first.len();
        first.copy_from_slice(&samples[..split]);
        second.copy_from_slice(&samples[split..split + second.len()]);
        chunk.commit_all();
        frames
    }

    /// Whether the ring is meant to stay filled (playing) or may run dry (paused, stopped).
    /// A dry ring always plays silence; it counts as an underrun only while feeding.
    /// Feeding is on when the stream opens.
    pub fn set_feeding(&self, feeding: bool) {
        self.shared.feeding.store(feeding, Ordering::Relaxed);
    }
}
