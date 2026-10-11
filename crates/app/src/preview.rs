//! The engine side of the preview: one `TimelinePlayback` for the open sequence on the
//! shared GPU device (Decision 60), and its audio through the live mix on the default
//! output device (Decision 73).
//!
//! The audio callback is the master clock (Decision 34): playback's frames are due on
//! the device's clock, so picture follows sound. Without an output device (or when it
//! cannot open at the sequence's rate) playback runs on the system clock without sound
//! and [`Preview::silent`] says so; nothing fails.
//!
//! Each UI frame [`Preview::poll`] takes every presented frame that is due, keeps the
//! newest on screen and drops the rest (they were late). Edits reach playback through
//! [`Preview::set_plan`] with the frames they dirtied (Decision 67) and the mix through
//! [`Preview::update_mix`].

use std::sync::Arc;

use engine::audio::mixer::{Layout, Mixer, MixerConfig};
use engine::audio::{Clock, OutputConfig, OutputStream};
use engine::mix::{AudioPlayback, AudioPlaybackConfig, MixSettings};
use engine::project::{Project, SequenceId};
use engine::render::wgpu;
use engine::timeline::{
    DirtyFrames, FormatChanged, Presented, PresentedFrame, TimelineConfig, TimelinePlan,
    TimelinePlayback,
};

/// The GPU device and queue the preview renders with: eframe's in the app, a test
/// device headless.
#[derive(Clone, Debug)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// The features the renderer needs (10-bit sources upload as 16-bit normalised
    /// planes); eframe asks for them when it creates its device.
    pub const FEATURES: wgpu::Features = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;

    /// A device of its own, for tests: `None` without an adapter that has
    /// [`Gpu::FEATURES`].
    pub fn headless() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        if !adapter.features().contains(Self::FEATURES) {
            return None;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("glass preview"),
            required_features: Self::FEATURES,
            ..Default::default()
        }))
        .ok()?;
        Some(Self { device, queue })
    }
}

/// Whether to open the audio device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioOut {
    /// The default output device, falling back to silence.
    Device,
    /// No device: the system clock, no sound (tests).
    Silent,
}

struct Sound {
    mix: AudioPlayback,
    /// Kept open while the preview lives; dropping it stops the device.
    _stream: OutputStream,
}

/// A running preview of one sequence.
pub struct Preview {
    playback: TimelinePlayback,
    presented: Presented,
    shown: Option<PresentedFrame>,
    sound: Option<Sound>,
    clock: Clock,
    fps: f64,
    playing: bool,
}

impl Preview {
    /// Starts playback of `sequence` paused at frame `start`; its first frame arrives
    /// through [`Preview::poll`].
    pub fn start(
        gpu: &Gpu,
        plan: Arc<TimelinePlan>,
        project: &Project,
        sequence: SequenceId,
        mix: &MixSettings,
        audio: AudioOut,
        start: i64,
    ) -> Result<Self, String> {
        let fps = plan.rate().to_f64().unwrap_or(30.0);
        let sample_rate = project.sequence(sequence).map_or(48_000, |s| s.sample_rate);
        let (sound, clock) = match audio {
            AudioOut::Device => open_sound(project, sequence, mix, sample_rate, fps, start),
            AudioOut::Silent => (None, Clock::system(sample_rate)),
        };
        let (playback, presented) = TimelinePlayback::start(
            plan.clone(),
            &gpu.device,
            &gpu.queue,
            clock.clone(),
            TimelineConfig::new(plan.canvas()),
        )
        .map_err(|e| e.to_string())?;
        if start != 0 {
            playback.seek(start);
        }
        Ok(Self {
            playback,
            presented,
            shown: None,
            sound,
            clock,
            fps,
            playing: false,
        })
    }

    /// Playing without sound: no output device, or it would not open.
    pub fn silent(&self) -> bool {
        self.sound.is_none()
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    fn mix_frame(&self, frame: i64) -> i64 {
        (frame as f64 * f64::from(self.clock.sample_rate()) / self.fps).round() as i64
    }

    pub fn play(&mut self) {
        self.playing = true;
        if let Some(sound) = &self.sound {
            sound.mix.play();
        }
        self.playback.play();
    }

    pub fn pause(&mut self) {
        self.playing = false;
        if let Some(sound) = &self.sound {
            sound.mix.pause();
        }
        self.playback.pause();
    }

    /// Moves to timeline frame `frame` (playing on from there when playing).
    pub fn seek(&mut self, frame: i64) {
        if let Some(sound) = &self.sound {
            sound.mix.seek(self.mix_frame(frame));
        }
        self.playback.seek(frame);
    }

    /// Hands playback the edited sequence's plan and the frames the edit dirtied.
    pub fn set_plan(
        &mut self,
        plan: Arc<TimelinePlan>,
        dirty: DirtyFrames,
    ) -> Result<(), FormatChanged> {
        self.playback.set_plan(plan, dirty)
    }

    /// A new project snapshot or new mute and solo switches for the live mix.
    pub fn update_mix(&self, project: &Project, mix: &MixSettings) {
        if let Some(sound) = &self.sound {
            sound.mix.update(project.clone(), mix.clone());
        }
    }

    /// The plan playback renders.
    pub fn plan(&self) -> &Arc<TimelinePlan> {
        self.playback.plan()
    }

    /// Takes the frames that are due on the master clock and keeps the newest on
    /// screen. Returns whether the frame on screen changed. At the end of the sequence
    /// playback pauses.
    pub fn poll(&mut self) -> bool {
        let now = self.clock.now();
        let mut changed = false;
        while let Some(next) = self.presented.peek() {
            if next.frame.due.is_some_and(|due| due > now) {
                break;
            }
            match self.presented.try_pop() {
                Ok(stamped) => {
                    self.shown = Some(stamped.frame);
                    changed = true;
                }
                Err(_) => break,
            }
        }
        if self.playing && self.playback.stats().ended && self.presented.peek().is_none() {
            self.pause();
        }
        changed
    }

    /// The frame on screen.
    pub fn shown(&self) -> Option<&PresentedFrame> {
        self.shown.as_ref()
    }

    /// Seconds until the next queued frame is due (for scheduling a repaint), if any.
    pub fn next_due_in(&mut self) -> Option<f64> {
        let now = self.clock.now();
        self.presented
            .peek()
            .map(|next| next.frame.due.map_or(0.0, |due| (due - now).max(0.0)))
    }

    pub fn stats(&self) -> engine::timeline::PlaybackStats {
        self.playback.stats()
    }

    /// Stops every thread and waits for them.
    pub fn shutdown(self) {
        if let Some(sound) = self.sound {
            sound.mix.shutdown();
        }
        self.playback.shutdown();
    }
}

/// Opens the default output device with a live mix of `sequence` from timeline frame
/// `start`; `(None, system clock)` when there is no device or it refuses the rate.
fn open_sound(
    project: &Project,
    sequence: SequenceId,
    mix: &MixSettings,
    sample_rate: u32,
    fps: f64,
    start: i64,
) -> (Option<Sound>, Clock) {
    let (mixer, control) = Mixer::new(MixerConfig {
        sample_rate,
        layout: Layout::Stereo,
        ..MixerConfig::default()
    });
    let (stream, clock) = match OutputStream::open_with_mixer(&OutputConfig::default(), mixer) {
        Ok(opened) => opened,
        Err(error) => {
            tracing::info!("no audio output ({error}); playing without sound");
            return (None, Clock::system(sample_rate));
        }
    };
    let start = (start as f64 * f64::from(sample_rate) / fps).round() as i64;
    match AudioPlayback::start(
        project.clone(),
        sequence,
        mix.clone(),
        control,
        start,
        AudioPlaybackConfig::default(),
    ) {
        Ok(mix) => (
            Some(Sound {
                mix,
                _stream: stream,
            }),
            clock,
        ),
        Err(error) => {
            tracing::warn!("the live mix did not start ({error}); playing without sound");
            // The stream closes here, which would stop its clock.
            drop(clock);
            (None, Clock::system(sample_rate))
        }
    }
}
