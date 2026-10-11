//! [`MediaDecoder`]: the look-ahead's decoder for media files: hardware decode when the
//! file allows it (`HwPreference::Auto`), software otherwise, and optionally a software
//! pre-roll that hands over to hardware decode at the next keyframe.
//!
//! VideoToolbox through FFmpeg decodes one frame at a time, so an exact seek that has to
//! decode forward from a keyframe was slow on the M2 (4K median 122 ms, against 40 ms in
//! software). With [`PreRoll::Software`] a segment that plays through hardware decode
//! starts in software instead: a software decoder seeks exactly to the segment's first
//! frame and decodes up to the next keyframe, while the hardware decoder only seeks to
//! that keyframe (one decoded frame, no decoding forward). From the keyframe on, frames
//! come from the hardware decoder. Both seeks happen in the pre-roll, before the cut.
//!
//! The default is [`PreRoll::Steady`]: on the M5 a pre-roll (open plus exact seek) with
//! VideoToolbox took no longer than in software (1080p mean 42 ms against 38 ms, 4K 145
//! against 147 ms), and the hand-off added a second open and the GOP scan (1080p 51 ms,
//! 4K 176 ms) while keeping short segments in software. Either way the seek happens in
//! the pre-roll, before the cut, so it costs no late frames (`tests/lookahead.rs`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use media::{
    DecoderOptions, GopIndex, HwPreference, MediaError, Rational, SourceOptions, StreamSelect,
    Timestamp, VideoFrame, VideoSource,
};

use super::{SegmentDecoder, SourceId, SourceRef};

/// How a segment's first frame is reached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreRoll {
    /// Seek with the playback decoder itself.
    #[default]
    Steady,
    /// When the playback decoder is hardware: seek exactly in software, switch to the
    /// hardware decoder at the next keyframe. For machines whose hardware seeks are slow.
    Software,
}

/// How a [`MediaDecoder`] opens sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaDecoderOptions {
    /// The playback decoder (hardware when available).
    pub decoder: DecoderOptions,
    /// The project rate, for image sequences.
    pub project_rate: Rational,
    pub pre_roll: PreRoll,
}

impl Default for MediaDecoderOptions {
    fn default() -> Self {
        Self {
            decoder: DecoderOptions::with_hw(HwPreference::Auto),
            project_rate: Rational::new(30, 1),
            pre_roll: PreRoll::default(),
        }
    }
}

/// GOP indexes shared by every worker of a pipeline, built once per source.
#[derive(Debug, Default)]
pub struct GopIndexes {
    indexes: Mutex<HashMap<SourceId, Arc<GopIndex>>>,
}

impl GopIndexes {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Hands a known index to the workers (from the analysis cache).
    pub fn insert(&self, source: SourceId, index: Arc<GopIndex>) {
        self.lock().insert(source, index);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SourceId, Arc<GopIndex>>> {
        self.indexes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The source's index, scanned outside the lock on first use.
    fn get(&self, source: &SourceRef) -> Result<Arc<GopIndex>, MediaError> {
        if let Some(index) = self.lock().get(&source.id) {
            return Ok(index.clone());
        }
        let (index, _) = GopIndex::build(&source.path, StreamSelect::BestVideo)?;
        let index = Arc::new(index);
        self.lock().insert(source.id, index.clone());
        Ok(index)
    }
}

/// An open source and the id it was opened for.
struct Open {
    id: SourceId,
    source: VideoSource,
}

enum State {
    Idle,
    /// Frames from the playback decoder; `first` is the seek's frame.
    Steady {
        first: Option<VideoFrame>,
    },
    /// Frames from the software decoder until `until`, then `handoff` and the playback
    /// decoder.
    Software {
        first: Option<VideoFrame>,
        until: Option<Timestamp>,
        handoff: Option<VideoFrame>,
    },
}

/// Decodes segments of media files (see the module docs). One per worker; it keeps the
/// last source open, so consecutive segments of one file skip the open.
pub struct MediaDecoder {
    options: MediaDecoderOptions,
    gops: Arc<GopIndexes>,
    steady: Option<Open>,
    software: Option<Open>,
    state: State,
}

impl MediaDecoder {
    pub fn new(options: MediaDecoderOptions, gops: Arc<GopIndexes>) -> Self {
        Self {
            options,
            gops,
            steady: None,
            software: None,
            state: State::Idle,
        }
    }

    /// Whether the playback decoder of the open source decodes in hardware.
    pub fn is_hardware(&self) -> bool {
        self.steady
            .as_ref()
            .and_then(|open| open.source.decode_path())
            .is_some_and(|path| path.is_hardware())
    }

    fn open<'a>(
        slot: &'a mut Option<Open>,
        source: &SourceRef,
        options: SourceOptions,
    ) -> Result<&'a mut VideoSource, MediaError> {
        if slot.as_ref().is_none_or(|open| open.id != source.id) {
            *slot = None;
            *slot = Some(Open {
                id: source.id,
                source: VideoSource::open(&source.path, options)?,
            });
        }
        match slot {
            Some(open) => Ok(&mut open.source),
            None => unreachable!("opened above"),
        }
    }

    fn source_options(&self, decoder: DecoderOptions) -> SourceOptions {
        SourceOptions {
            decoder,
            ..SourceOptions::new(self.options.project_rate)
        }
    }

    fn seek_software(&mut self, source: &SourceRef, at: Timestamp) -> Result<(), MediaError> {
        let gop = self.gops.get(source)?;
        let until = gop
            .keyframes
            .iter()
            .map(|k| Timestamp::new(k.pts, gop.time_base))
            .find(|k| k.cmp_time(at).is_gt());
        let options = self.source_options(DecoderOptions {
            hw: HwPreference::Off,
            ..self.options.decoder
        });
        let software = Self::open(&mut self.software, source, options)?;
        software.set_gop_index(gop.clone());
        let first = software.seek_exact(at)?;
        let handoff = match until {
            Some(keyframe) => match &mut self.steady {
                Some(open) => {
                    open.source.set_gop_index(gop);
                    open.source.seek_keyframe(keyframe)?
                }
                None => None,
            },
            None => None,
        };
        // Without the keyframe's frame from the playback decoder, software plays on.
        let until = handoff.as_ref().and(until);
        self.state = State::Software {
            first,
            until,
            handoff,
        };
        Ok(())
    }
}

impl SegmentDecoder for MediaDecoder {
    type Frame = VideoFrame;

    fn seek(&mut self, source: &SourceRef, at: Timestamp) -> Result<(), MediaError> {
        self.state = State::Idle;
        let options = self.source_options(self.options.decoder);
        let steady = Self::open(&mut self.steady, source, options)?;
        let hardware = steady.decode_path().is_some_and(|path| path.is_hardware());
        if self.options.pre_roll == PreRoll::Software && hardware {
            return self.seek_software(source, at);
        }
        let first = steady.seek_exact(at)?;
        self.state = State::Steady { first };
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<(Timestamp, VideoFrame)>, MediaError> {
        let frame = match &mut self.state {
            State::Idle => None,
            State::Steady { first } => match first.take() {
                Some(frame) => Some(frame),
                None => match &mut self.steady {
                    Some(open) => open.source.next_frame()?,
                    None => None,
                },
            },
            State::Software {
                first,
                until,
                handoff,
            } => match first.take() {
                Some(frame) => Some(frame),
                None => {
                    let frame = match &mut self.software {
                        Some(open) => open.source.next_frame()?,
                        None => None,
                    };
                    let switch = match (until, &frame) {
                        (Some(keyframe), Some(frame)) => frame
                            .pts()
                            .is_some_and(|pts| pts.cmp_time(*keyframe).is_ge()),
                        (Some(_), None) => true,
                        (None, _) => false,
                    };
                    if switch {
                        let first = handoff.take();
                        self.state = State::Steady { first: None };
                        first
                    } else {
                        frame
                    }
                }
            },
        };
        Ok(frame.map(|frame| {
            let pts = frame
                .pts()
                .unwrap_or_else(|| Timestamp::new(0, Rational::new(1, 1)));
            (pts, frame)
        }))
    }
}
