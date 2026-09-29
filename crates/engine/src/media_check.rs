//! A headless probe-and-decode pass over one file, used by `smoke` until real playback
//! exists. It runs the same `media` calls the decode pool will make.

use std::path::Path;
use std::time::{Duration, Instant};

use media::{FrameReader, MediaError, MediaInfo, StreamSelect, Timestamp};

/// What [`probe_and_decode`] saw.
#[derive(Debug)]
pub struct DecodeCheck {
    pub info: MediaInfo,
    pub video_frames: u64,
    pub audio_samples: u64,
    pub first_video_pts: Option<Timestamp>,
    pub last_video_pts: Option<Timestamp>,
    pub elapsed: Duration,
}

/// Probes `path`, then decodes its main video stream (and audio stream, if any) from the
/// start until `seconds` of presentation time have been decoded.
pub fn probe_and_decode(path: &Path, seconds: f64) -> Result<DecodeCheck, MediaError> {
    let start = Instant::now();
    let info = media::probe(path)?;
    let mut check = DecodeCheck {
        info,
        video_frames: 0,
        audio_samples: 0,
        first_video_pts: None,
        last_video_pts: None,
        elapsed: Duration::ZERO,
    };
    if check.info.video().is_some() {
        let mut reader = FrameReader::open(path, StreamSelect::BestVideo)?;
        while let Some(frame) = reader.next_frame()? {
            let Some(pts) = frame.pts() else { continue };
            if pts.seconds() >= seconds {
                break;
            }
            check.first_video_pts.get_or_insert(pts);
            check.last_video_pts = Some(pts);
            check.video_frames += 1;
        }
    }
    if check.info.audio().is_some() {
        let mut reader = FrameReader::open(path, StreamSelect::BestAudio)?;
        while let Some(frame) = reader.next_frame()? {
            if frame.pts().is_some_and(|pts| pts.seconds() >= seconds) {
                break;
            }
            if let Some(audio) = frame.into_audio() {
                check.audio_samples += audio.samples() as u64;
            }
        }
    }
    check.elapsed = start.elapsed();
    Ok(check)
}
