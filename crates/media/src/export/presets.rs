//! Export presets: named recipes for platforms and deliverables (YouTube, TikTok,
//! Instagram, X, Vimeo, broadcast, archival) that resolve against a timeline's size,
//! frame rate and channel count into [`ExportSettings`].
//!
//! A [`Preset`] holds what the platform or deliverable asks for: the container, the video
//! codec with a fixed size or the timeline's, a frame-rate policy, rate control, pixel
//! format, colour tags and HDR metadata, the audio codec and layout, a loudness target
//! and the platform's limits. Loudness targets and limits are not used by the encoder;
//! output verification and the export dialog read them. A limit is [`Strictness::Hard`]
//! only when the platform or standard states it and it has held for years; anything that
//! varies by account, region or app version is a [`Strictness::Soft`] hint.
//!
//! The built-in presets are Rust data ([`BUILT_IN`]), so every one is type-checked and
//! a test encodes each. Their names are i18n keys. Only codecs this build encodes are
//! used (no AV1, VP9 or MP3, Decision 47).

use crate::encode::PcmFormat;
use crate::format::PixelFormat;
use crate::time::Rational;

use super::settings::{
    AudioCodec, AudioSettings, ColourTags, Container, ContentLight, ExportSettings, HdrMetadata,
    MasteringDisplay, ProResProfile, RateControl, VideoCodec, VideoSettings,
};
use super::{ExportError, Result};

/// What a preset is for, as the export dialog groups them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresetCategory {
    YouTube,
    TikTok,
    Instagram,
    X,
    Vimeo,
    Broadcast,
    Archival,
}

impl PresetCategory {
    pub const ALL: [Self; 7] = [
        Self::YouTube,
        Self::TikTok,
        Self::Instagram,
        Self::X,
        Self::Vimeo,
        Self::Broadcast,
        Self::Archival,
    ];

    pub fn name_key(self) -> &'static str {
        match self {
            Self::YouTube => "export.preset.category.youtube",
            Self::TikTok => "export.preset.category.tiktok",
            Self::Instagram => "export.preset.category.instagram",
            Self::X => "export.preset.category.x",
            Self::Vimeo => "export.preset.category.vimeo",
            Self::Broadcast => "export.preset.category.broadcast",
            Self::Archival => "export.preset.category.archival",
        }
    }
}

/// The picture size a preset writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresetSize {
    /// The timeline's size, rounded down to even for codecs with subsampled chroma.
    MatchTimeline,
    /// This size whatever the timeline's; the export render fits the timeline into it.
    Fixed { width: u32, height: u32 },
}

/// The frame rate a preset writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameRatePolicy {
    MatchTimeline,
    /// The timeline's rate, halved until it is at most this (119.88 becomes 59.94), so
    /// every written frame is a timeline frame.
    UpTo(Rational),
    /// This rate whatever the timeline's (broadcast).
    Fixed(Rational),
}

impl FrameRatePolicy {
    pub fn resolve(self, timeline: Rational) -> Rational {
        match self {
            Self::MatchTimeline => timeline,
            Self::Fixed(rate) => rate,
            Self::UpTo(max) => {
                let limit = max.to_f64().unwrap_or(f64::INFINITY);
                let mut rate = timeline;
                // A small allowance so 60000/1001 stays under a 60 cap and 60 under itself.
                while rate.to_f64().is_some_and(|r| r > limit + 1e-6) {
                    rate = if rate.num % 2 == 0 {
                        Rational::new(rate.num / 2, rate.den)
                    } else {
                        Rational::new(rate.num, rate.den.saturating_mul(2))
                    };
                }
                rate
            }
        }
    }
}

/// Rate control of a preset's video.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PresetRate {
    /// The codec's own (ProRes has no rate setting).
    Default,
    Crf(f32),
    /// Average and peak bits per second at up to 30 fps; above 30 fps both are scaled by
    /// 1.5, as platforms' tables for 48 to 60 fps uploads are.
    Bitrate {
        average: u64,
        max: Option<u64>,
    },
}

impl PresetRate {
    fn resolve(self, frame_rate: Rational) -> RateControl {
        match self {
            Self::Default => RateControl::Default,
            Self::Crf(crf) => RateControl::Crf(crf),
            Self::Bitrate { average, max } => {
                let high = frame_rate.to_f64().is_some_and(|r| r > 31.0);
                let scale = |v: u64| if high { v * 3 / 2 } else { v };
                RateControl::Bitrate {
                    average: scale(average),
                    max: max.map(scale),
                }
            }
        }
    }
}

/// A preset's video stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresetVideo {
    pub codec: VideoCodec,
    pub size: PresetSize,
    pub frame_rate: FrameRatePolicy,
    pub rate: PresetRate,
    /// FFmpeg's name of the pixel format, which also sets the bit depth (`yuv420p10le`);
    /// `None` is the codec's default (4:2:0 8-bit, 10-bit 4:2:2 or 4:4:4 for ProRes).
    pub pixel_format: Option<&'static str>,
    pub colour: ColourTags,
    pub hdr_metadata: Option<HdrMetadata>,
    /// Keyframe interval in seconds; `None` keeps the encoder's (every frame for ProRes).
    pub keyframe_seconds: Option<f32>,
    pub max_b_frames: Option<u32>,
}

/// A preset's audio stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PresetAudio {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    /// `None` keeps the timeline's channel count.
    pub channels: Option<usize>,
    /// Bits per second for AAC.
    pub bit_rate: Option<u64>,
}

/// Whether a target or limit is a requirement or a hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Strictness {
    /// Stated by the standard or the platform and stable: a file outside it is rejected.
    Hard,
    /// Recommended, or varying by account, region or app version: worth a warning only.
    Soft,
}

/// A limit with how firmly it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Limit<T> {
    pub value: T,
    pub strictness: Strictness,
}

impl<T> Limit<T> {
    pub const fn hard(value: T) -> Self {
        Self {
            value,
            strictness: Strictness::Hard,
        }
    }

    pub const fn soft(value: T) -> Self {
        Self {
            value,
            strictness: Strictness::Soft,
        }
    }
}

/// The loudness measurement a target follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LoudnessStandard {
    /// Streaming platforms' normalisation level (ITU-R BS.1770 integrated loudness).
    Streaming,
    /// EBU R128.
    EbuR128,
    /// ATSC A/85.
    AtscA85,
}

/// An integrated loudness target, for output verification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessTarget {
    pub standard: LoudnessStandard,
    /// Integrated loudness in LUFS (LKFS for ATSC).
    pub integrated: f32,
    /// How far the measured loudness may be from the target, in LU.
    pub tolerance: f32,
    /// Highest true peak in dBTP.
    pub max_true_peak: f32,
    pub strictness: Strictness,
}

impl LoudnessTarget {
    /// Streaming platforms turn louder uploads down to about -14 LUFS.
    pub const STREAMING: Self = Self {
        standard: LoudnessStandard::Streaming,
        integrated: -14.0,
        tolerance: 1.0,
        max_true_peak: -1.0,
        strictness: Strictness::Soft,
    };

    /// EBU R128: -23 LUFS within 0.5 LU, true peak at most -1 dBTP.
    pub const EBU_R128: Self = Self {
        standard: LoudnessStandard::EbuR128,
        integrated: -23.0,
        tolerance: 0.5,
        max_true_peak: -1.0,
        strictness: Strictness::Hard,
    };

    /// ATSC A/85: -24 LKFS within 2 dB, true peak at most -2 dBTP.
    pub const ATSC_A85: Self = Self {
        standard: LoudnessStandard::AtscA85,
        integrated: -24.0,
        tolerance: 2.0,
        max_true_peak: -2.0,
        strictness: Strictness::Hard,
    };
}

/// What a platform accepts, where it is known.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PlatformLimits {
    /// Longest upload, in seconds.
    pub max_duration: Option<Limit<u32>>,
    /// Largest file, in bytes.
    pub max_file_size: Option<Limit<u64>>,
    /// The picture aspect the platform shows best (width : height).
    pub aspect: Option<Limit<(u32, u32)>>,
}

/// An export preset. See the [module docs](self).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    /// Stable identifier, stored in projects and settings.
    pub id: &'static str,
    pub name_key: &'static str,
    pub category: PresetCategory,
    pub container: Container,
    pub video: PresetVideo,
    pub audio: PresetAudio,
    /// MP4 and MOV: the index at the front of the file.
    pub fast_start: bool,
    pub loudness: Option<LoudnessTarget>,
    pub limits: PlatformLimits,
}

/// What a preset resolves against: the timeline's format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimelineFormat {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub audio_channels: usize,
}

impl Preset {
    /// The built-in preset with this id.
    pub fn built_in(id: &str) -> Option<&'static Self> {
        BUILT_IN.iter().find(|p| p.id == id)
    }

    /// The export settings for a timeline: container and mux settings, the video stream
    /// at the preset's or the timeline's size and rate, the audio stream. The encoder
    /// preference is left at its default (`Auto`); callers may change any field before
    /// [`ExportSession::create`](super::ExportSession::create).
    pub fn resolve(&self, timeline: &TimelineFormat) -> Result<ExportSettings> {
        let v = &self.video;
        let frame_rate = v.frame_rate.resolve(timeline.frame_rate);
        let pixel_format = v
            .pixel_format
            .map(|name| {
                PixelFormat::from_name(name)
                    .ok_or_else(|| ExportError::Settings(format!("FFmpeg does not know {name}")))
            })
            .transpose()?;
        let (width, height) = match v.size {
            PresetSize::Fixed { width, height } => (width, height),
            PresetSize::MatchTimeline => {
                let (sx, sy) = match pixel_format {
                    Some(f) => f.chroma_shift(),
                    None if v.codec.is_rgb() => (0, 0),
                    None => (1, 1),
                };
                let even = |n: u32, shift: u8| (n >> shift) << shift;
                (even(timeline.width, sx), even(timeline.height, sy))
            }
        };
        let mut video = VideoSettings::new(v.codec, width, height, frame_rate);
        video.pixel_format = pixel_format;
        video.rate = v.rate.resolve(frame_rate);
        video.colour = v.colour;
        video.hdr_metadata = v.hdr_metadata;
        video.max_b_frames = v.max_b_frames;
        video.gop = v.keyframe_seconds.and_then(|seconds| {
            let frames = frame_rate.to_f64()? * f64::from(seconds);
            Some(frames.round().max(1.0) as u32)
        });
        let a = &self.audio;
        let mut audio = AudioSettings::new(
            a.codec,
            a.sample_rate,
            a.channels.unwrap_or(timeline.audio_channels),
        );
        audio.bit_rate = a.bit_rate;
        let mut settings = ExportSettings::new(self.container)
            .with_video(video)
            .with_audio(audio);
        settings.fast_start = self.fast_start;
        settings.validate()?;
        Ok(settings)
    }
}

const MBIT: u64 = 1_000_000;
const KBIT: u64 = 1_000;
const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

const UP_TO_60: FrameRatePolicy = FrameRatePolicy::UpTo(Rational::new(60, 1));

/// The PQ preset's mastering display and light levels until the grade measures its own:
/// a 1000 cd/m² P3 monitor, MaxCLL 1000, MaxFALL 400.
const HDR10_DEFAULT: HdrMetadata = HdrMetadata {
    mastering: Some(MasteringDisplay::P3_D65_1000),
    content_light: Some(ContentLight {
        max_cll: 1000,
        max_fall: 400,
    }),
};

/// H.264 4:2:0 8-bit BT.709 for web platforms.
const fn web_h264(width: u32, height: u32, average: u64, max: u64, keyframe: f32) -> PresetVideo {
    PresetVideo {
        codec: VideoCodec::H264,
        size: PresetSize::Fixed { width, height },
        frame_rate: UP_TO_60,
        rate: PresetRate::Bitrate {
            average,
            max: Some(max),
        },
        pixel_format: Some("yuv420p"),
        colour: ColourTags::Bt709,
        hdr_metadata: None,
        keyframe_seconds: Some(keyframe),
        max_b_frames: Some(2),
    }
}

/// AAC-LC 48 kHz stereo.
const fn aac_stereo(bit_rate: u64) -> PresetAudio {
    PresetAudio {
        codec: AudioCodec::Aac,
        sample_rate: 48_000,
        channels: Some(2),
        bit_rate: Some(bit_rate),
    }
}

/// 24-bit PCM at 48 kHz, stereo or the timeline's channels.
const fn pcm24(channels: Option<usize>) -> PresetAudio {
    PresetAudio {
        codec: AudioCodec::Pcm(PcmFormat::S24),
        sample_rate: 48_000,
        channels,
        bit_rate: None,
    }
}

/// ProRes at 1920x1080, BT.709, at a fixed rate.
const fn broadcast_prores(rate: Rational) -> PresetVideo {
    PresetVideo {
        codec: VideoCodec::ProRes(ProResProfile::Hq),
        size: PresetSize::Fixed {
            width: 1920,
            height: 1080,
        },
        frame_rate: FrameRatePolicy::Fixed(rate),
        rate: PresetRate::Default,
        pixel_format: None,
        colour: ColourTags::Bt709,
        hdr_metadata: None,
        keyframe_seconds: None,
        max_b_frames: None,
    }
}

/// ProRes 4444 at the timeline's size and rate.
const ARCHIVAL_VIDEO: PresetVideo = PresetVideo {
    codec: VideoCodec::ProRes(ProResProfile::P4444),
    size: PresetSize::MatchTimeline,
    frame_rate: FrameRatePolicy::MatchTimeline,
    rate: PresetRate::Default,
    pixel_format: None,
    colour: ColourTags::Bt709,
    hdr_metadata: None,
    keyframe_seconds: None,
    max_b_frames: None,
};

const YOUTUBE_LIMITS: PlatformLimits = PlatformLimits {
    max_duration: Some(Limit::hard(12 * 60 * 60)),
    max_file_size: Some(Limit::hard(256 * GIB)),
    aspect: Some(Limit::soft((16, 9))),
};

const VERTICAL_LIMITS: PlatformLimits = PlatformLimits {
    max_duration: Some(Limit::soft(10 * 60)),
    max_file_size: None,
    aspect: Some(Limit::soft((9, 16))),
};

const BROADCAST_LIMITS: PlatformLimits = PlatformLimits {
    max_duration: None,
    max_file_size: None,
    aspect: Some(Limit::hard((16, 9))),
};

/// The presets Glass ships, in the order the export dialog lists them.
pub const BUILT_IN: &[Preset] = &[
    // YouTube's upload recommendations: MP4 with the index first, H.264 High with
    // closed GOPs of half a second and 2 B-frames, AAC-LC 384 kbit/s.
    Preset {
        id: "youtube_1080p",
        name_key: "export.preset.name.youtube_1080p",
        category: PresetCategory::YouTube,
        container: Container::Mp4,
        video: web_h264(1920, 1080, 8 * MBIT, 12 * MBIT, 0.5),
        audio: aac_stereo(384 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: YOUTUBE_LIMITS,
    },
    Preset {
        id: "youtube_4k",
        name_key: "export.preset.name.youtube_4k",
        category: PresetCategory::YouTube,
        container: Container::Mp4,
        video: web_h264(3840, 2160, 45 * MBIT, 68 * MBIT, 0.5),
        audio: aac_stereo(384 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: YOUTUBE_LIMITS,
    },
    Preset {
        id: "youtube_4k_hdr",
        name_key: "export.preset.name.youtube_4k_hdr",
        category: PresetCategory::YouTube,
        container: Container::Mp4,
        video: PresetVideo {
            codec: VideoCodec::Hevc,
            size: PresetSize::Fixed {
                width: 3840,
                height: 2160,
            },
            frame_rate: UP_TO_60,
            rate: PresetRate::Bitrate {
                average: 56 * MBIT,
                max: Some(85 * MBIT),
            },
            pixel_format: Some("yuv420p10le"),
            colour: ColourTags::Bt2020Pq,
            hdr_metadata: Some(HDR10_DEFAULT),
            keyframe_seconds: Some(0.5),
            max_b_frames: Some(2),
        },
        audio: aac_stereo(384 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: YOUTUBE_LIMITS,
    },
    Preset {
        id: "tiktok_vertical",
        name_key: "export.preset.name.tiktok_vertical",
        category: PresetCategory::TikTok,
        container: Container::Mp4,
        video: web_h264(1080, 1920, 10 * MBIT, 15 * MBIT, 1.0),
        audio: aac_stereo(192 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: VERTICAL_LIMITS,
    },
    Preset {
        id: "instagram_reels",
        name_key: "export.preset.name.instagram_reels",
        category: PresetCategory::Instagram,
        container: Container::Mp4,
        video: web_h264(1080, 1920, 10 * MBIT, 15 * MBIT, 1.0),
        audio: aac_stereo(192 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: PlatformLimits {
            max_duration: Some(Limit::soft(3 * 60)),
            max_file_size: None,
            aspect: Some(Limit::soft((9, 16))),
        },
    },
    Preset {
        id: "instagram_feed_portrait",
        name_key: "export.preset.name.instagram_feed_portrait",
        category: PresetCategory::Instagram,
        container: Container::Mp4,
        video: web_h264(1080, 1350, 8 * MBIT, 12 * MBIT, 1.0),
        audio: aac_stereo(192 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: PlatformLimits {
            max_duration: None,
            max_file_size: None,
            aspect: Some(Limit::soft((4, 5))),
        },
    },
    Preset {
        id: "instagram_feed_square",
        name_key: "export.preset.name.instagram_feed_square",
        category: PresetCategory::Instagram,
        container: Container::Mp4,
        video: web_h264(1080, 1080, 8 * MBIT, 12 * MBIT, 1.0),
        audio: aac_stereo(192 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: PlatformLimits {
            max_duration: None,
            max_file_size: None,
            aspect: Some(Limit::soft((1, 1))),
        },
    },
    // X: 2 min 20 s and 512 MB on standard accounts, longer with a subscription.
    Preset {
        id: "x_1080p",
        name_key: "export.preset.name.x_1080p",
        category: PresetCategory::X,
        container: Container::Mp4,
        video: web_h264(1920, 1080, 10 * MBIT, 20 * MBIT, 1.0),
        audio: aac_stereo(192 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: PlatformLimits {
            max_duration: Some(Limit::soft(140)),
            max_file_size: Some(Limit::soft(512 * MIB)),
            aspect: Some(Limit::soft((16, 9))),
        },
    },
    // Vimeo's limits depend on the plan, so none are stored.
    Preset {
        id: "vimeo_1080p",
        name_key: "export.preset.name.vimeo_1080p",
        category: PresetCategory::Vimeo,
        container: Container::Mp4,
        video: web_h264(1920, 1080, 20 * MBIT, 30 * MBIT, 1.0),
        audio: aac_stereo(320 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: PlatformLimits {
            max_duration: None,
            max_file_size: None,
            aspect: Some(Limit::soft((16, 9))),
        },
    },
    Preset {
        id: "vimeo_4k",
        name_key: "export.preset.name.vimeo_4k",
        category: PresetCategory::Vimeo,
        container: Container::Mp4,
        video: web_h264(3840, 2160, 50 * MBIT, 75 * MBIT, 1.0),
        audio: aac_stereo(320 * KBIT),
        fast_start: true,
        loudness: Some(LoudnessTarget::STREAMING),
        limits: PlatformLimits {
            max_duration: None,
            max_file_size: None,
            aspect: Some(Limit::soft((16, 9))),
        },
    },
    // Broadcast masters: ProRes 422 HQ 1080p in QuickTime with 24-bit PCM.
    Preset {
        id: "broadcast_ebu_1080p25",
        name_key: "export.preset.name.broadcast_ebu_1080p25",
        category: PresetCategory::Broadcast,
        container: Container::Mov,
        video: broadcast_prores(Rational::new(25, 1)),
        audio: pcm24(Some(2)),
        fast_start: false,
        loudness: Some(LoudnessTarget::EBU_R128),
        limits: BROADCAST_LIMITS,
    },
    Preset {
        id: "broadcast_atsc_1080p2997",
        name_key: "export.preset.name.broadcast_atsc_1080p2997",
        category: PresetCategory::Broadcast,
        container: Container::Mov,
        video: broadcast_prores(Rational::new(30_000, 1001)),
        audio: pcm24(Some(2)),
        fast_start: false,
        loudness: Some(LoudnessTarget::ATSC_A85),
        limits: BROADCAST_LIMITS,
    },
    // Archival masters keep the timeline's size, rate and channels.
    Preset {
        id: "archival_prores4444_pcm",
        name_key: "export.preset.name.archival_prores4444_pcm",
        category: PresetCategory::Archival,
        container: Container::Mov,
        video: ARCHIVAL_VIDEO,
        audio: pcm24(None),
        fast_start: false,
        loudness: None,
        limits: PlatformLimits {
            max_duration: None,
            max_file_size: None,
            aspect: None,
        },
    },
    Preset {
        id: "archival_prores4444_flac",
        name_key: "export.preset.name.archival_prores4444_flac",
        category: PresetCategory::Archival,
        container: Container::Matroska,
        video: ARCHIVAL_VIDEO,
        audio: PresetAudio {
            codec: AudioCodec::Flac { bits: 24 },
            sample_rate: 48_000,
            channels: None,
            bit_rate: None,
        },
        fast_start: false,
        loudness: None,
        limits: PlatformLimits {
            max_duration: None,
            max_file_size: None,
            aspect: None,
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    const HD30: TimelineFormat = TimelineFormat {
        width: 1920,
        height: 1080,
        frame_rate: Rational::new(30, 1),
        audio_channels: 2,
    };

    #[test]
    fn ids_are_unique_and_every_category_has_a_preset() {
        let mut ids: Vec<&str> = BUILT_IN.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), BUILT_IN.len());
        for category in PresetCategory::ALL {
            assert!(
                BUILT_IN.iter().any(|p| p.category == category),
                "{category:?}"
            );
        }
        assert_eq!(
            Preset::built_in("youtube_4k_hdr").map(|p| p.video.codec),
            Some(VideoCodec::Hevc)
        );
        assert!(Preset::built_in("nope").is_none());
    }

    #[test]
    fn names_and_categories_are_english_keys() {
        let english = assets::i18n::Locale::english();
        let keys = BUILT_IN
            .iter()
            .map(|p| p.name_key)
            .chain(PresetCategory::ALL.iter().map(|c| c.name_key()));
        for key in keys {
            assert!(english.get(key).is_some(), "{key} missing from en.json");
        }
    }

    #[test]
    fn no_preset_uses_a_codec_this_build_lacks() {
        for p in BUILT_IN {
            assert!(
                !matches!(p.video.codec, VideoCodec::Av1 | VideoCodec::Vp9),
                "{}",
                p.id
            );
            assert_ne!(p.audio.codec, AudioCodec::Mp3, "{}", p.id);
        }
    }

    #[test]
    fn frame_rates_are_capped_by_halving() {
        let up_to = FrameRatePolicy::UpTo(Rational::new(60, 1));
        assert_eq!(
            up_to.resolve(Rational::new(120_000, 1001)),
            Rational::new(60_000, 1001)
        );
        assert_eq!(up_to.resolve(Rational::new(60, 1)), Rational::new(60, 1));
        assert_eq!(up_to.resolve(Rational::new(24, 1)), Rational::new(24, 1));
        assert_eq!(up_to.resolve(Rational::new(25, 1)), Rational::new(25, 1));
        assert_eq!(up_to.resolve(Rational::new(100, 1)), Rational::new(50, 1));
        let fixed = FrameRatePolicy::Fixed(Rational::new(25, 1));
        assert_eq!(fixed.resolve(Rational::new(30, 1)), Rational::new(25, 1));
    }

    #[test]
    fn every_preset_resolves_against_common_timelines() {
        let timelines = [
            HD30,
            TimelineFormat {
                width: 3840,
                height: 2160,
                frame_rate: Rational::new(60_000, 1001),
                audio_channels: 6,
            },
            TimelineFormat {
                width: 1081,
                height: 1921,
                frame_rate: Rational::new(120, 1),
                audio_channels: 1,
            },
        ];
        for p in BUILT_IN {
            for t in &timelines {
                let s = p
                    .resolve(t)
                    .unwrap_or_else(|e| panic!("{} on {t:?}: {e}", p.id));
                let v = s.video.as_ref().expect("video");
                let rate = v.frame_rate.to_f64().expect("rate");
                match p.video.frame_rate {
                    FrameRatePolicy::UpTo(max) => {
                        assert!(rate <= max.to_f64().expect("cap") + 1e-6, "{}", p.id);
                    }
                    FrameRatePolicy::Fixed(fixed) => assert_eq!(v.frame_rate, fixed),
                    FrameRatePolicy::MatchTimeline => assert_eq!(v.frame_rate, t.frame_rate),
                }
                let a = s.audio.as_ref().expect("audio");
                assert_eq!(a.sample_rate, 48_000);
            }
        }
    }

    #[test]
    fn resolution_follows_the_policies() {
        let yt = Preset::built_in("youtube_1080p").expect("preset");
        let at_60 = TimelineFormat {
            frame_rate: Rational::new(60, 1),
            ..HD30
        };
        let s = yt.resolve(&at_60).expect("resolves");
        let v = s.video.expect("video");
        assert_eq!((v.width, v.height), (1920, 1080));
        assert_eq!(v.gop, Some(30));
        assert_eq!(
            v.rate,
            RateControl::Bitrate {
                average: 12 * MBIT,
                max: Some(18 * MBIT)
            }
        );
        assert!(s.fast_start);

        let archival = Preset::built_in("archival_prores4444_pcm").expect("preset");
        let odd = TimelineFormat {
            width: 1279,
            height: 721,
            frame_rate: Rational::new(24_000, 1001),
            audio_channels: 6,
        };
        let s = archival.resolve(&odd).expect("resolves");
        let v = s.video.expect("video");
        assert_eq!((v.width, v.height), (1278, 720));
        assert_eq!(v.frame_rate, Rational::new(24_000, 1001));
        assert_eq!(s.audio.expect("audio").channels, 6);

        let hdr = Preset::built_in("youtube_4k_hdr").expect("preset");
        let v = hdr.resolve(&HD30).expect("resolves").video.expect("video");
        assert_eq!(v.colour, ColourTags::Bt2020Pq);
        assert_eq!(v.pixel_format.map(|f| f.bit_depth()), Some(10));
        assert_eq!(v.hdr_metadata, Some(HDR10_DEFAULT));
    }
}
