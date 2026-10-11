//! [`SoftwareScrubDecoder`]: the scrubber's decoder for one media file, software only.
//!
//! Exact seeks were much faster in software than through VideoToolbox on the M2 (1080p
//! median 13.6 against 51.5 ms, 4K 39.5 against 121.8 ms), and hardware seeks are under
//! investigation on the M5, so scrubbing decodes in software and playback keeps the
//! hardware decoder.
//!
//! **Cancelling a GOP walk.** An exact seek decodes forward from the keyframe at or
//! before the target, which cannot be interrupted inside FFmpeg. The decoder therefore
//! walks in steps: it seeks exactly to points `cancel_step` apart between the keyframe
//! (or where the decoder already is in the same GOP) and the target, and checks the
//! cancel flag before each. Every step after the first continues forward in the same GOP
//! without a new demuxer seek (`VideoSource::seek_exact` decodes on from where it is),
//! so the steps add no decoding, and a superseded request stops within one step.

use std::sync::Arc;
use std::time::Duration;

use media::{
    DecoderOptions, GopIndex, HwPreference, MediaError, Rational, SourceKind, SourceOptions,
    Timestamp, VideoFrame, VideoSource,
};

use super::{Decoded, ScrubDecoder};
use crate::lookahead::{GopIndexes, SourceRef};

/// How a [`SoftwareScrubDecoder`] opens its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftwareDecoderOptions {
    /// FFmpeg decoder threads; 0 lets FFmpeg pick.
    pub threads: usize,
    /// The project rate, for image sequences.
    pub project_rate: Rational,
    /// Source time between cancel checks while walking a GOP: 100 ms is 3 frames at
    /// 30 fps, about 10 ms of 4K software decoding.
    pub cancel_step: Duration,
}

impl Default for SoftwareDecoderOptions {
    fn default() -> Self {
        Self {
            threads: 0,
            project_rate: Rational::new(30, 1),
            cancel_step: Duration::from_millis(100),
        }
    }
}

/// One source decoded in software for scrubbing, kept open while the scrub lasts.
pub struct SoftwareScrubDecoder {
    source: VideoSource,
    gop: Option<Arc<GopIndex>>,
    options: SoftwareDecoderOptions,
    /// The presentation time of the last frame handed out.
    position: Option<Timestamp>,
}

impl SoftwareScrubDecoder {
    /// Opens `source` with a software decoder and, for video files, its GOP index from
    /// `gops` (scanned on first use and shared).
    pub fn open(
        source: &SourceRef,
        gops: &GopIndexes,
        options: SoftwareDecoderOptions,
    ) -> Result<Self, MediaError> {
        let mut video = VideoSource::open(
            &source.path,
            SourceOptions {
                decoder: DecoderOptions {
                    threads: options.threads,
                    hw: HwPreference::Off,
                },
                ..SourceOptions::new(options.project_rate)
            },
        )?;
        let gop = if video.kind() == SourceKind::Video {
            let gop = gops.get(source)?;
            video.set_gop_index(gop.clone());
            Some(gop)
        } else {
            None
        };
        Ok(Self {
            source: video,
            gop,
            options,
            position: None,
        })
    }

    /// Where the walk to `target` starts: the decoder's position when it is already in
    /// the target's GOP and before the target, otherwise the GOP's keyframe.
    fn walk_start(&self, gop: &GopIndex, target: Timestamp) -> Option<Timestamp> {
        let keyframe = gop.keyframe_at_or_before(target)?;
        let from_keyframe = gop.keyframe_pts(keyframe)?;
        Some(match self.position {
            Some(position)
                if gop.keyframe_at_or_before(position) == Some(keyframe)
                    && position.cmp_time(target).is_lt() =>
            {
                position.rescale_floor(gop.time_base)
            }
            _ => from_keyframe,
        })
    }
}

impl ScrubDecoder for SoftwareScrubDecoder {
    type Frame = VideoFrame;

    fn decode_exact(
        &mut self,
        at: Timestamp,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Decoded<VideoFrame>, MediaError> {
        if let Some(gop) = self.gop.clone() {
            let target = at.rescale_floor(gop.time_base);
            let step =
                Timestamp::from_seconds(self.options.cancel_step.as_secs_f64(), gop.time_base)
                    .value
                    .max(1);
            if let Some(start) = self.walk_start(&gop, target) {
                let mut t = start.value.saturating_add(step);
                while t < target.value {
                    if cancel() {
                        return Ok(Decoded::Cancelled);
                    }
                    let frame = self.source.seek_exact(Timestamp::new(t, gop.time_base))?;
                    self.position = frame.as_ref().and_then(VideoFrame::pts);
                    t = t.saturating_add(step);
                }
            }
        }
        if cancel() {
            return Ok(Decoded::Cancelled);
        }
        let frame = self.source.seek_exact(at)?;
        self.position = frame.as_ref().and_then(VideoFrame::pts);
        Ok(match frame {
            Some(frame) => Decoded::Frame(frame.pts().unwrap_or(at), frame),
            None => Decoded::Empty,
        })
    }

    fn keyframes(&mut self, from: Timestamp, to: Timestamp, out: &mut Vec<Timestamp>) {
        let Some(gop) = &self.gop else {
            return;
        };
        let from = from.rescale_floor(gop.time_base).value;
        let to = to.rescale_floor(gop.time_base).value;
        let first = gop.keyframes.partition_point(|k| k.pts < from);
        out.extend(
            gop.keyframes[first..]
                .iter()
                .take_while(|k| k.pts <= to)
                .map(|k| Timestamp::new(k.pts, gop.time_base)),
        );
    }
}
