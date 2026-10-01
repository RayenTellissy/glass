//! Video slaved to the master clock: which video frame each display refresh shows.
//!
//! The render thread asks the [`crate::Clock`] for the presentation time at the next
//! refresh and hands it to [`VideoSync::frame_at`]. The frame shown is the one on screen
//! at the middle of the refresh interval, so a video rate that divides the refresh rate
//! (30 fps at 60 Hz) keeps the largest margin from frame boundaries. Drift between the
//! audio device and the display slowly moves that point across a boundary; a small
//! hysteresis keeps the choice from flickering between two frames while it does, so the
//! correction is one repeated or skipped frame. The audio is never adjusted.
//!
//! Pure arithmetic with no allocation.

/// Half the width of the hysteresis band, in video frames: the choice ignores jitter in
/// the presentation time up to this much (2.5 ms at 60 fps, 6.25 ms at 24 fps).
pub const HYSTERESIS: f64 = 0.15;

/// Picks the video frame for each display refresh. One per playing stream; call
/// [`VideoSync::reset`] after a seek or a change of speed.
#[derive(Clone, Debug)]
pub struct VideoSync {
    fps: f64,
    /// Added to the frame position; moved only to keep it out of the band around a
    /// boundary, never by more than [`HYSTERESIS`] either way.
    offset: f64,
    last: Option<i64>,
    repeats: u64,
    skips: u64,
}

impl VideoSync {
    /// Video at `fps_num / fps_den` frames per second (24000/1001 for 23.976).
    pub fn new(fps_num: u32, fps_den: u32) -> Self {
        Self {
            fps: f64::from(fps_num.max(1)) / f64::from(fps_den.max(1)),
            offset: 0.0,
            last: None,
            repeats: 0,
            skips: 0,
        }
    }

    /// The frame index to show for the refresh whose presentation time is `secs`
    /// (`Clock::at` the refresh instant) and which lasts `refresh` seconds. Frame `n`
    /// covers `[n / fps, (n + 1) / fps)` on the clock's timeline; mapping it to a source
    /// frame is the scheduler's job.
    pub fn frame_at(&mut self, secs: f64, refresh: f64) -> i64 {
        let position = (secs + 0.5 * refresh.max(0.0)) * self.fps + self.offset;
        let distance = position - position.round();
        if distance.abs() < HYSTERESIS {
            // Near a boundary: stay on the side the position is on, out of the band.
            let side = if distance < 0.0 { -1.0 } else { 1.0 };
            self.offset =
                (self.offset + side * HYSTERESIS - distance).clamp(-HYSTERESIS, HYSTERESIS);
        }
        let frame = (secs + 0.5 * refresh.max(0.0)) * self.fps + self.offset;
        let frame = frame.floor() as i64;

        if let Some(last) = self.last {
            let ideal = self.fps * refresh.max(0.0);
            let step = frame - last;
            if step >= 0 {
                let step = step as f64;
                if step > ideal.ceil() {
                    self.skips += (step - ideal.ceil()) as u64;
                } else if step < ideal.floor() {
                    self.repeats += (ideal.floor() - step) as u64;
                }
            }
        }
        self.last = Some(frame);
        frame
    }

    /// Forgets the previous refresh (after a seek, a pause or a change of speed).
    pub fn reset(&mut self) {
        self.offset = 0.0;
        self.last = None;
    }

    /// Refreshes that showed a frame for longer than the cadence asks, so video caught
    /// up with a slower clock.
    pub fn repeats(&self) -> u64 {
        self.repeats
    }

    /// Frames never shown because the clock ran ahead of the cadence.
    pub fn skips(&self) -> u64 {
        self.skips
    }
}
