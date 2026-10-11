//! The device backend: a `cpal` output stream whose callback is the master clock.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, SampleFormat, StreamConfig};

use crate::OutputConfig;
use crate::callback::{self, Callback, Feeder, Timing};
use crate::clock::{Clock, Shared, Source};
use crate::mixer::Mixer;

/// Why an output stream could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("no audio output device")]
    NoDevice,
    #[error("the output device has no f32 format at {sample_rate} Hz")]
    Unsupported { sample_rate: u32 },
    #[error("audio device: {0}")]
    Device(#[from] cpal::Error),
}

/// A running output stream on the default device. Dropping it stops the stream; the
/// [`Clock`] then stays at its last value.
pub struct OutputStream {
    stream: cpal::Stream,
    shared: Arc<Shared>,
    device_channels: u16,
}

impl OutputStream {
    /// Opens the default output device at the configured rate (the device's own rate when
    /// unset) and starts it. Returns the stream, the feeder for the audio decode thread
    /// and the clock.
    ///
    /// The stream asks for the configured channel count and falls back to the nearest the
    /// device offers: extra device channels get silence, missing ones are dropped.
    pub fn open_default(config: &OutputConfig) -> Result<(Self, Feeder, Clock), OutputError> {
        let mut feeder = None;
        let (stream, clock) = Self::open(config, |shared, device_channels| {
            let (callback, f) = callback::channel(
                shared,
                usize::from(config.channels.max(1)),
                device_channels,
                config.ring_frames,
            );
            feeder = Some(f);
            callback
        })?;
        let feeder = feeder.expect("the callback is made before the stream opens");
        Ok((stream, feeder, clock))
    }

    /// Opens the default output device like [`open_default`](Self::open_default), at the
    /// mixer's rate and channel count, with `mixer` filling the device buffer.
    pub fn open_with_mixer(
        config: &OutputConfig,
        mixer: Mixer,
    ) -> Result<(Self, Clock), OutputError> {
        let mix = mixer.config();
        let config = OutputConfig {
            sample_rate: Some(mix.sample_rate),
            channels: mix.layout.channels() as u16,
            ..*config
        };
        let mut mixer = Some(mixer);
        Self::open(&config, |shared, device_channels| {
            let mixer = mixer.take().expect("the callback is made once");
            callback::mixing(shared, mixer, device_channels)
        })
    }

    fn open(
        config: &OutputConfig,
        make_callback: impl FnOnce(Arc<Shared>, usize) -> Callback,
    ) -> Result<(Self, Clock), OutputError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(OutputError::NoDevice)?;
        let sample_rate = match config.sample_rate {
            Some(rate) => rate,
            None => device.default_output_config()?.sample_rate(),
        };
        let wanted = config.channels.max(1);
        let device_channels = device
            .supported_output_configs()?
            .filter(|range| {
                range.sample_format() == SampleFormat::F32
                    && (range.min_sample_rate()..=range.max_sample_rate()).contains(&sample_rate)
            })
            .map(|range| range.channels())
            .min_by_key(|&channels| (channels < wanted, channels.abs_diff(wanted)))
            .ok_or(OutputError::Unsupported { sample_rate })?;
        let stream_config = StreamConfig {
            channels: device_channels,
            sample_rate,
            buffer_size: config
                .buffer_frames
                .map_or(BufferSize::Default, BufferSize::Fixed),
        };

        let shared = Shared::new(sample_rate, Source::Device);
        let mut callback = make_callback(Arc::clone(&shared), usize::from(device_channels));
        let timing_shared = Arc::clone(&shared);
        let channels = usize::from(device_channels);
        let rate = f64::from(sample_rate);
        let data = move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let now = timing_shared.secs(Instant::now());
            let stamp = info.timestamp();
            let latency = match stamp.playback.checked_duration_since(stamp.callback) {
                Some(d) if !d.is_zero() => d.as_secs_f64(),
                // No playback prediction from this host: the buffer itself is the latency.
                _ => (out.len() / channels) as f64 / rate,
            };
            callback.process(out, Some(Timing { now, latency }));
        };
        let error_shared = Arc::clone(&shared);
        let error = move |error: cpal::Error| {
            let counter = match error.kind() {
                ErrorKind::Xrun => &error_shared.xruns,
                _ => &error_shared.errors,
            };
            counter.fetch_add(1, Ordering::Relaxed);
        };
        let stream = device.build_output_stream::<f32, _, _>(stream_config, data, error, None)?;
        stream.play()?;
        let clock = Clock::new(Arc::clone(&shared));
        let output = Self {
            stream,
            shared,
            device_channels,
        };
        Ok((output, clock))
    }

    /// The device's channel count (may differ from the feeder's).
    pub fn device_channels(&self) -> u16 {
        self.device_channels
    }

    /// Errors the device reported since the stream opened (a disconnect, a route change,
    /// a backend failure), overloads excepted. The callback cannot log, so the owner polls
    /// this and logs.
    pub fn errors(&self) -> u64 {
        self.shared.errors.load(Ordering::Relaxed)
    }

    /// Overloads the device reported since the stream opened: the device itself missed a
    /// deadline (a busy machine), as opposed to the ring running dry
    /// ([`Clock::underruns`]).
    pub fn xruns(&self) -> u64 {
        self.shared.xruns.load(Ordering::Relaxed)
    }

    /// Pauses the device. The clock holds while no callback runs and the drift estimator
    /// re-seeds on resume.
    pub fn pause(&self) -> Result<(), OutputError> {
        Ok(self.stream.pause()?)
    }

    /// Resumes a paused device.
    pub fn resume(&self) -> Result<(), OutputError> {
        Ok(self.stream.play()?)
    }
}
