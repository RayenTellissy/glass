//! The Glass engine: scheduler, playback state, timeline evaluation into render graphs,
//! dirty-range invalidation, rendered-frame cache policy, look-ahead, background render,
//! the export pipeline, the frame queues between decode, render and present, and the
//! analysis job system, plus logging setup for every binary.

pub mod analysis;
pub mod autosave;
pub mod frame_queue;
pub mod logging;
pub mod media_check;
pub mod media_mgmt;

/// Names of the optional codec and AI features compiled into this build.
pub fn enabled_features() -> Vec<&'static str> {
    let mut features = media::enabled_features();
    if cfg!(feature = "ai-whisper") {
        features.push("ai-whisper");
    }
    if cfg!(feature = "ai-segmentation") {
        features.push("ai-segmentation");
    }
    features
}
