//! The real-time half: the code that runs inside the audio callback, shared by the device
//! and offline backends, and [`Feeder`], its non-real-time producer.
//!
//! The callback's audio comes from one of two sources. A plain feed: interleaved f32
//! frames through one `rtrb` ring, copied into the device buffer, any shortfall filled with
//! silence (counting an underrun while the feeder is feeding). Or a [`Mixer`]: the mixer
//! graph mixes the tracks' feed rings (underruns are its feeds running dry while playing).
//! Either way the callback then advances the frame counter and, when the backend has
//! timestamps, updates the drift estimator and publishes its line. It never allocates,
//! locks, logs or waits.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::clock::Shared;
use crate::drift::DriftEstimator;
use crate::mixer::Mixer;

/// Frames the mixer renders per step when the device has other channels than the mix.
const MIX_SCRATCH_FRAMES: usize = 1024;

/// Timing for one callback, in seconds on the clock's monotonic timeline.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// When the callback runs.
    pub(crate) now: f64,
    /// From `now` until the callback's first frame is audible.
    pub(crate) latency: f64,
}

/// Where the callback's audio comes from.
enum Source {
    Ring {
        consumer: Consumer<f32>,
        feed_channels: usize,
    },
    Mixer {
        mixer: Box<Mixer>,
        /// Mixer output when the device has another channel count.
        scratch: Vec<f32>,
    },
}

/// The callback's state. Owned by the audio thread once the stream starts.
pub(crate) struct Callback {
    source: Source,
    shared: Arc<Shared>,
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
        source: Source::Ring {
            consumer,
            feed_channels,
        },
        estimator: DriftEstimator::new(shared.sample_rate),
        shared: Arc::clone(&shared),
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

/// The callback state for a mixer source.
pub(crate) fn mixing(shared: Arc<Shared>, mixer: Mixer, device_channels: usize) -> Callback {
    let channels = mixer.channels();
    Callback {
        source: Source::Mixer {
            mixer: Box::new(mixer),
            scratch: vec![0.0; MIX_SCRATCH_FRAMES * channels],
        },
        estimator: DriftEstimator::new(shared.sample_rate),
        shared,
        device_channels: device_channels.max(1),
        frames: 0,
    }
}

/// Copies `frames` frames of `from` (`from_channels` per frame) into `out`
/// (`out_channels` per frame): extra source channels are dropped, extra output channels
/// get silence.
fn map_channels(
    from: impl Iterator<Item = f32>,
    from_channels: usize,
    out: &mut [f32],
    out_channels: usize,
    frames: usize,
) {
    let mut samples = from;
    for frame in out.chunks_exact_mut(out_channels).take(frames) {
        for c in 0..from_channels {
            let sample = samples.next().unwrap_or(0.0);
            if let Some(slot) = frame.get_mut(c) {
                *slot = sample;
            }
        }
        for slot in frame.iter_mut().skip(from_channels) {
            *slot = 0.0;
        }
    }
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

        let device_channels = self.device_channels;
        match &mut self.source {
            Source::Ring {
                consumer,
                feed_channels,
            } => {
                let feed_channels = *feed_channels;
                let available = consumer.slots() / feed_channels;
                let take = available.min(frames);
                let mut written = 0;
                if let Ok(chunk) = consumer.read_chunk(take * feed_channels) {
                    let (first, second) = chunk.as_slices();
                    if feed_channels == device_channels {
                        out[..first.len()].copy_from_slice(first);
                        out[first.len()..first.len() + second.len()].copy_from_slice(second);
                    } else {
                        let samples = first.iter().chain(second.iter()).copied();
                        map_channels(samples, feed_channels, out, device_channels, take);
                    }
                    chunk.commit_all();
                    written = take;
                }
                out[written * device_channels..].fill(0.0);
                if written < frames && self.shared.feeding.load(Ordering::Relaxed) {
                    self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                    self.shared
                        .silent_frames
                        .fetch_add((frames - written) as u64, Ordering::Relaxed);
                }
            }
            Source::Mixer { mixer, scratch } => {
                let mix_channels = mixer.channels();
                let mut silent = 0;
                if mix_channels == device_channels {
                    silent = mixer.render(&mut out[..frames * device_channels]);
                } else {
                    let mut done = 0;
                    while done < frames {
                        let n = (frames - done).min(MIX_SCRATCH_FRAMES);
                        let mixed = &mut scratch[..n * mix_channels];
                        silent += mixer.render(mixed);
                        let out = &mut out[done * device_channels..(done + n) * device_channels];
                        map_channels(mixed.iter().copied(), mix_channels, out, device_channels, n);
                        done += n;
                    }
                }
                out[frames * device_channels..].fill(0.0);
                if silent > 0 {
                    self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                    self.shared
                        .silent_frames
                        .fetch_add(silent as u64, Ordering::Relaxed);
                }
            }
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
