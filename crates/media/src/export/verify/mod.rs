//! Output verification: a finished export is probed and read back and compared with what
//! was asked for, then the file can be revealed in the platform's file manager.
//!
//! [`verify`] opens the file once with [`probe`](crate::probe()) for the container,
//! codecs, size, frame rate, audio format and colour tags, reads every video packet to
//! count frames and time the stream (packets, not the container's frame count, so a
//! file cut short or a muxer that miscounts shows up), and decodes the audio at its own
//! rate and layout through [`AudioReader`] to count samples and measure integrated
//! loudness and true peak with the same meter as the analysis pool
//! ([`loudness::Meter`]). Each check that fails adds a [`Finding`] with an i18n message
//! key and its arguments; nothing in the report is English text.
//!
//! Tolerances: video duration within one frame of the expected duration, audio within
//! one coded audio frame (the codec's frame size, 1024 samples when it has none), frame
//! count exact. Loudness targets and platform limits come from the
//! [`Preset`](super::presets::Preset): a [`Strictness::Hard`] one outside its bound is a
//! [`Severity::Failure`], a [`Strictness::Soft`] one a [`Severity::Warning`]. Every other
//! mismatch is a failure.
//!
//! Image sequences are not verified here: each frame is its own file and
//! [`ExportSummary`](super::ExportSummary) lists them.

mod reveal;

use std::path::Path;

pub use reveal::{
    FileManager, Platform, RevealCommand, SystemFileManager, reveal_command,
    reveal_in_file_manager, reveal_with,
};

use crate::analysis::loudness::Meter;
use crate::analysis::{AnalysisError, Control};
use crate::audio::{AudioOptions, AudioReader, ChannelLayout};
use crate::demux::Demuxer;
use crate::format::ColourRange;
use crate::probe::{MediaInfo, probe};
use crate::time::Rational;

use super::presets::{Limit, LoudnessTarget, PlatformLimits, Preset, Strictness};
use super::settings::{AudioCodec, Container, ExportSettings, VideoCodec};

const READ_FRAMES: usize = 8192;
/// Samples per audio frame for codecs that do not state one (PCM).
const DEFAULT_AUDIO_FRAME: u32 = 1024;

/// What an export was asked to produce.
#[derive(Clone, Debug, PartialEq)]
pub struct Expected<'a> {
    pub settings: &'a ExportSettings,
    /// Video frames written.
    pub frames: u64,
    /// Seconds of video and of audio.
    pub duration: f64,
    pub loudness: Option<LoudnessTarget>,
    pub limits: PlatformLimits,
}

impl<'a> Expected<'a> {
    /// Format, frame count and duration only: no loudness target and no limits.
    pub fn new(settings: &'a ExportSettings, frames: u64, duration: f64) -> Self {
        Self {
            settings,
            frames,
            duration,
            loudness: None,
            limits: PlatformLimits::default(),
        }
    }

    /// The preset's loudness target and platform limits as well.
    pub fn with_preset(mut self, preset: &Preset) -> Self {
        self.loudness = preset.loudness;
        self.limits = preset.limits;
        self
    }
}

/// How serious a finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Outside a soft target or limit: worth telling, not wrong.
    Warning,
    /// The file is not what was asked for, or breaks a hard target or limit.
    Failure,
}

impl Severity {
    fn of(strictness: Strictness) -> Self {
        match strictness {
            Strictness::Hard => Self::Failure,
            Strictness::Soft => Self::Warning,
        }
    }
}

/// What a check looked at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Check {
    /// The file opens and its packets read to the end.
    Readable,
    Container,
    VideoCodec,
    Resolution,
    FrameRate,
    FrameCount,
    VideoDuration,
    AudioCodec,
    SampleRate,
    Channels,
    AudioDuration,
    Loudness,
    TruePeak,
    FileSize,
    MaxDuration,
    Aspect,
    Colour,
    HdrMetadata,
}

/// One problem verification found: an i18n key under `export.verify.` and the values
/// its placeholders take, already formatted (numbers in C locale).
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub check: Check,
    pub severity: Severity,
    pub key: &'static str,
    pub args: Vec<(&'static str, String)>,
}

impl Finding {
    fn failure(check: Check, key: &'static str, args: Vec<(&'static str, String)>) -> Self {
        Self {
            check,
            severity: Severity::Failure,
            key,
            args,
        }
    }

    /// The value of one argument.
    pub fn arg(&self, name: &str) -> Option<&str> {
        self.args
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What verification measured, for display beside the findings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measured {
    pub file_size: u64,
    /// Video packets read.
    pub frames: Option<u64>,
    /// From the first frame's start to the last frame's end, seconds.
    pub video_duration: Option<f64>,
    /// Decoded samples per channel.
    pub samples: Option<u64>,
    pub audio_duration: Option<f64>,
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
}

/// The result of [`verify`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VerifyReport {
    /// Problems, in the order of [`Check`].
    pub findings: Vec<Finding>,
    /// Every check that ran, passed or not.
    pub checked: Vec<Check>,
    pub measured: Measured,
}

impl VerifyReport {
    /// No failures (warnings allowed).
    pub fn passed(&self) -> bool {
        !self
            .findings
            .iter()
            .any(|f| f.severity == Severity::Failure)
    }

    pub fn failures(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Failure)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
    }

    fn check(&mut self, check: Check) {
        if !self.checked.contains(&check) {
            self.checked.push(check);
        }
    }

    fn push(&mut self, finding: Finding) {
        self.check(finding.check);
        self.findings.push(finding);
    }

    /// Records `check`, and a failure when `expected` and `found` differ.
    fn compare(&mut self, check: Check, key: &'static str, expected: String, found: String) {
        self.check(check);
        if expected != found {
            self.push(Finding::failure(
                check,
                key,
                vec![("expected", expected), ("found", found)],
            ));
        }
    }
}

/// Why verification produced no report.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("cancelled")]
    Stopped,
    #[error("image sequences are verified frame by frame from the export summary")]
    ImageSequence,
}

/// Probes and reads back `path` and compares it with `expected`. See the
/// [module docs](self). Checks the pool's `control` between reads.
pub fn verify(
    path: &Path,
    expected: &Expected<'_>,
    control: &dyn Control,
) -> Result<VerifyReport, VerifyError> {
    let settings = expected.settings;
    if settings.container == Container::ImageSequence {
        return Err(VerifyError::ImageSequence);
    }
    let mut report = VerifyReport::default();
    report.check(Check::Readable);
    report.measured.file_size = std::fs::metadata(path).map_or(0, |m| m.len());
    let info = match probe(path) {
        Ok(info) => info,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "exported file does not open");
            report.push(Finding::failure(
                Check::Readable,
                "export.verify.unreadable",
                Vec::new(),
            ));
            return Ok(report);
        }
    };

    check_container(&mut report, &info, settings.container);
    if let Some(video) = &settings.video {
        check_video(&mut report, path, &info, video, expected, control)?;
    }
    if let Some(audio) = &settings.audio {
        check_audio(&mut report, path, &info, audio, expected, control)?;
    }
    check_limits(&mut report, &info, &expected.limits);
    if let Some(video) = &settings.video {
        check_colour(&mut report, &info, video);
    }
    sort(&mut report);
    Ok(report)
}

fn sort(report: &mut VerifyReport) {
    let order = |c: Check| c as u8;
    report.findings.sort_by_key(|f| order(f.check));
}

/// The FFmpeg demuxer name (one of the comma-separated names) a container reads back as,
/// and for MP4 and QuickTime, which share a demuxer, whether the major brand is `qt  `.
fn container_identity(container: Container) -> (&'static str, Option<bool>) {
    match container {
        Container::Mp4 => ("mp4", Some(false)),
        Container::Mov => ("mov", Some(true)),
        Container::M4a => ("m4a", Some(false)),
        Container::Matroska => ("matroska", None),
        Container::WebM => ("webm", None),
        Container::Wav => ("wav", None),
        Container::Flac => ("flac", None),
        Container::Mp3 => ("mp3", None),
        Container::Gif => ("gif", None),
        Container::ImageSequence => ("image2", None),
    }
}

fn check_container(report: &mut VerifyReport, info: &MediaInfo, container: Container) {
    let (name, quicktime) = container_identity(container);
    let names_match = info.container.split(',').any(|n| n == name);
    let brand_match = match quicktime {
        Some(qt) => info
            .tag("major_brand")
            .is_none_or(|brand| (brand.trim() == "qt") == qt),
        None => true,
    };
    let found = match info.tag("major_brand") {
        Some(brand) if quicktime.is_some() => format!("{} ({})", info.container, brand.trim()),
        _ => info.container.clone(),
    };
    report.check(Check::Container);
    if !(names_match && brand_match) {
        report.push(Finding::failure(
            Check::Container,
            "export.verify.container",
            vec![("expected", name.to_string()), ("found", found)],
        ));
    }
}

fn video_codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::Hevc => "hevc",
        VideoCodec::Av1 => "av1",
        VideoCodec::Vp9 => "vp9",
        VideoCodec::ProRes(_) => "prores",
        VideoCodec::Png => "png",
        VideoCodec::Tiff => "tiff",
        VideoCodec::Gif => "gif",
    }
}

fn audio_codec_name(codec: AudioCodec) -> &'static str {
    use crate::encode::PcmFormat;
    match codec {
        AudioCodec::Pcm(PcmFormat::S16) => "pcm_s16le",
        AudioCodec::Pcm(PcmFormat::S24) => "pcm_s24le",
        AudioCodec::Pcm(PcmFormat::F32) => "pcm_f32le",
        AudioCodec::Aac => "aac",
        AudioCodec::Mp3 => "mp3",
        AudioCodec::Flac { .. } => "flac",
    }
}

/// A number with up to `decimals` decimals, trailing zeros dropped (`29.97`, `30`).
fn number(value: f64, decimals: usize) -> String {
    let s = format!("{value:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

fn rate_text(rate: Rational) -> String {
    rate.to_f64()
        .map_or_else(|| "?".to_string(), |r| number(r, 3))
}

fn same_rate(a: Rational, b: Rational) -> bool {
    i64::from(a.num) * i64::from(b.den) == i64::from(b.num) * i64::from(a.den)
}

/// Video packets of the main stream: count and the span from the first start to the
/// last end, in seconds; `damaged` when a read failed before the end.
struct PacketScan {
    frames: u64,
    span: Option<f64>,
    damaged: bool,
}

fn scan_video(
    path: &Path,
    index: usize,
    frame_rate: Rational,
    control: &dyn Control,
) -> Result<PacketScan, VerifyError> {
    let mut scan = PacketScan {
        frames: 0,
        span: None,
        damaged: false,
    };
    let Ok(mut demuxer) = Demuxer::open(path) else {
        scan.damaged = true;
        return Ok(scan);
    };
    let interval = frame_rate.invert().to_f64().unwrap_or(0.0);
    let mut first = f64::INFINITY;
    let mut end = f64::NEG_INFINITY;
    loop {
        if scan.frames % 256 == 0 {
            control.checkpoint().map_err(|_| VerifyError::Stopped)?;
        }
        let packet = match demuxer.read_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(_) => {
                scan.damaged = true;
                break;
            }
        };
        if packet.stream_index() != index {
            continue;
        }
        scan.frames += 1;
        if let Some(pts) = packet.pts() {
            let start = pts.seconds();
            let length = packet.duration().map_or(interval, |d| d.seconds()).max(0.0);
            first = first.min(start);
            end = end.max(start + length);
        }
    }
    if end.is_finite() && first.is_finite() {
        scan.span = Some(end - first);
    }
    Ok(scan)
}

fn check_video(
    report: &mut VerifyReport,
    path: &Path,
    info: &MediaInfo,
    video: &super::VideoSettings,
    expected: &Expected<'_>,
    control: &dyn Control,
) -> Result<(), VerifyError> {
    let Some((stream, found)) = info.video() else {
        report.push(Finding::failure(
            Check::VideoCodec,
            "export.verify.video.missing",
            Vec::new(),
        ));
        return Ok(());
    };
    report.compare(
        Check::VideoCodec,
        "export.verify.video.codec",
        video_codec_name(video.codec).to_string(),
        stream.codec.name.to_string(),
    );
    report.compare(
        Check::Resolution,
        "export.verify.resolution",
        format!("{}x{}", video.width, video.height),
        format!("{}x{}", found.width, found.height),
    );
    // GIF stores delays in hundredths of a second, so its rate and duration are only
    // approximate; its frame count is still exact.
    let gif = video.codec == VideoCodec::Gif;
    if !gif {
        report.check(Check::FrameRate);
        let rate = found.frame_rate.or(found.avg_frame_rate);
        if !rate.is_some_and(|r| same_rate(r, video.frame_rate)) {
            report.push(Finding::failure(
                Check::FrameRate,
                "export.verify.frame_rate",
                vec![
                    ("expected", rate_text(video.frame_rate)),
                    ("found", rate.map_or_else(|| "?".to_string(), rate_text)),
                ],
            ));
        }
    }

    let scan = scan_video(path, stream.index, video.frame_rate, control)?;
    report.measured.frames = Some(scan.frames);
    report.measured.video_duration = scan.span;
    if scan.damaged {
        report.push(Finding::failure(
            Check::Readable,
            "export.verify.video.damaged",
            Vec::new(),
        ));
    }
    report.compare(
        Check::FrameCount,
        "export.verify.frame_count",
        expected.frames.to_string(),
        scan.frames.to_string(),
    );
    if !gif {
        report.check(Check::VideoDuration);
        let frame = video.frame_rate.invert().to_f64().unwrap_or(0.0);
        let span = scan.span.unwrap_or(0.0);
        if (span - expected.duration).abs() > frame + 1e-6 {
            report.push(Finding::failure(
                Check::VideoDuration,
                "export.verify.video.duration",
                vec![
                    ("expected", number(expected.duration, 3)),
                    ("found", number(span, 3)),
                ],
            ));
        }
    }
    Ok(())
}

/// Decoded audio: samples per channel and the meter's reading.
struct AudioRead {
    rate: u32,
    samples: u64,
    integrated: Option<f64>,
    true_peak: Option<f64>,
    damaged: bool,
}

fn read_audio(path: &Path, control: &dyn Control) -> Result<Option<AudioRead>, VerifyError> {
    let reader = AudioReader::open_for_source(path, AudioOptions::default(), |source, o| {
        o.sample_rate = source.sample_rate.max(8_000);
        o.layout = ChannelLayout::for_channels(source.channels);
    });
    let Ok(mut reader) = reader else {
        return Ok(None);
    };
    let rate = reader.sample_rate();
    let channels = reader.channels();
    let Ok(mut meter) = Meter::new(channels, rate) else {
        return Ok(None);
    };
    let mut planes = vec![vec![0f32; READ_FRAMES]; channels];
    let mut read = AudioRead {
        rate,
        samples: 0,
        integrated: None,
        true_peak: None,
        damaged: false,
    };
    loop {
        control.checkpoint().map_err(|_| VerifyError::Stopped)?;
        let frames = {
            let mut out: Vec<&mut [f32]> = planes.iter_mut().map(Vec::as_mut_slice).collect();
            match reader.read(&mut out) {
                Ok(n) => n,
                Err(_) => {
                    read.damaged = true;
                    break;
                }
            }
        };
        if frames == 0 {
            break;
        }
        let views: Vec<&[f32]> = planes.iter().map(|p| &p[..frames]).collect();
        if meter.push(&views, frames).is_err() {
            read.damaged = true;
            break;
        }
        read.samples += frames as u64;
    }
    match meter.finish() {
        Ok(loudness) => {
            read.integrated = loudness.integrated_lufs;
            read.true_peak = loudness.true_peak_dbtp;
        }
        Err(AnalysisError::Stopped) => return Err(VerifyError::Stopped),
        Err(_) => read.damaged = true,
    }
    Ok(Some(read))
}

fn check_audio(
    report: &mut VerifyReport,
    path: &Path,
    info: &MediaInfo,
    audio: &super::AudioSettings,
    expected: &Expected<'_>,
    control: &dyn Control,
) -> Result<(), VerifyError> {
    let Some((stream, found)) = info.audio() else {
        report.push(Finding::failure(
            Check::AudioCodec,
            "export.verify.audio.missing",
            Vec::new(),
        ));
        return Ok(());
    };
    report.compare(
        Check::AudioCodec,
        "export.verify.audio.codec",
        audio_codec_name(audio.codec).to_string(),
        stream.codec.name.to_string(),
    );
    report.compare(
        Check::SampleRate,
        "export.verify.audio.sample_rate",
        audio.sample_rate.to_string(),
        found.sample_rate.to_string(),
    );
    report.compare(
        Check::Channels,
        "export.verify.audio.channels",
        audio.channels.to_string(),
        found.channels.to_string(),
    );

    let Some(read) = read_audio(path, control)? else {
        report.push(Finding::failure(
            Check::Readable,
            "export.verify.audio.damaged",
            Vec::new(),
        ));
        return Ok(());
    };
    let rate = f64::from(read.rate.max(1));
    let duration = read.samples as f64 / rate;
    report.measured.samples = Some(read.samples);
    report.measured.audio_duration = Some(duration);
    report.measured.integrated_lufs = read.integrated;
    report.measured.true_peak_dbtp = read.true_peak;
    if read.damaged {
        report.push(Finding::failure(
            Check::Readable,
            "export.verify.audio.damaged",
            Vec::new(),
        ));
    }
    report.check(Check::AudioDuration);
    let frame = f64::from(
        found
            .frame_size
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_AUDIO_FRAME),
    );
    if (duration - expected.duration).abs() * rate > frame + 0.5 {
        report.push(Finding::failure(
            Check::AudioDuration,
            "export.verify.audio.duration",
            vec![
                ("expected", number(expected.duration, 3)),
                ("found", number(duration, 3)),
            ],
        ));
    }
    // No samples at all is already a duration failure, not a loudness reading.
    if let Some(target) = &expected.loudness
        && read.samples > 0
    {
        check_loudness(report, target, read.integrated, read.true_peak);
    }
    Ok(())
}

fn check_loudness(
    report: &mut VerifyReport,
    target: &LoudnessTarget,
    integrated: Option<f64>,
    true_peak: Option<f64>,
) {
    let severity = Severity::of(target.strictness);
    let goal = f64::from(target.integrated);
    let tolerance = f64::from(target.tolerance);
    report.check(Check::Loudness);
    match integrated {
        None => report.push(Finding {
            check: Check::Loudness,
            severity,
            key: "export.verify.loudness.silent",
            args: vec![("target", number(goal, 1))],
        }),
        // Compared at the 0.1 LU the reading is shown with.
        Some(lufs) if ((lufs - goal).abs() * 10.0).round() > (tolerance * 10.0).round() => {
            report.push(Finding {
                check: Check::Loudness,
                severity,
                key: "export.verify.loudness.integrated",
                args: vec![
                    ("found", number(lufs, 1)),
                    ("target", number(goal, 1)),
                    ("tolerance", number(tolerance, 1)),
                ],
            });
        }
        Some(_) => {}
    }
    report.check(Check::TruePeak);
    let max = f64::from(target.max_true_peak);
    if let Some(peak) = true_peak
        && (peak * 10.0).round() > (max * 10.0).round()
    {
        report.push(Finding {
            check: Check::TruePeak,
            severity,
            key: "export.verify.loudness.true_peak",
            args: vec![("found", number(peak, 1)), ("max", number(max, 1))],
        });
    }
}

fn limit_finding<T>(
    report: &mut VerifyReport,
    check: Check,
    key: &'static str,
    limit: &Limit<T>,
    args: Vec<(&'static str, String)>,
) {
    report.push(Finding {
        check,
        severity: Severity::of(limit.strictness),
        key,
        args,
    });
}

fn check_limits(report: &mut VerifyReport, info: &MediaInfo, limits: &PlatformLimits) {
    const MIB: f64 = (1u64 << 20) as f64;
    if let Some(limit) = &limits.max_file_size {
        report.check(Check::FileSize);
        let size = report.measured.file_size;
        if size > limit.value {
            limit_finding(
                report,
                Check::FileSize,
                "export.verify.limit.file_size",
                limit,
                vec![
                    ("found", number(size as f64 / MIB, 1)),
                    ("limit", number(limit.value as f64 / MIB, 1)),
                ],
            );
        }
    }
    if let Some(limit) = &limits.max_duration {
        report.check(Check::MaxDuration);
        let measured = &report.measured;
        let duration = measured
            .video_duration
            .into_iter()
            .chain(measured.audio_duration)
            .fold(0f64, f64::max);
        if duration > f64::from(limit.value) {
            limit_finding(
                report,
                Check::MaxDuration,
                "export.verify.limit.duration",
                limit,
                vec![
                    ("found", number(duration, 1)),
                    ("limit", limit.value.to_string()),
                ],
            );
        }
    }
    if let Some(limit) = &limits.aspect
        && let Some((_, video)) = info.video()
    {
        report.check(Check::Aspect);
        let (width, height) = video.display_size();
        let (aw, ah) = limit.value;
        if u64::from(width) * u64::from(ah) != u64::from(height) * u64::from(aw) {
            limit_finding(
                report,
                Check::Aspect,
                "export.verify.limit.aspect",
                limit,
                vec![
                    ("expected", format!("{aw}:{ah}")),
                    ("found", format!("{width}x{height}")),
                ],
            );
        }
    }
}

fn range_name(range: ColourRange) -> &'static str {
    match range {
        ColourRange::Unspecified => "unspecified",
        ColourRange::Limited => "limited",
        ColourRange::Full => "full",
    }
}

fn check_colour(report: &mut VerifyReport, info: &MediaInfo, video: &super::VideoSettings) {
    // GIF carries no colour tags; PNG and TIFF are not verified here.
    if video.codec.is_rgb() {
        return;
    }
    let Some((_, found)) = info.video() else {
        return;
    };
    let want = video.colour.info(false);
    let tag = |v: Option<&'static str>| v.unwrap_or("unspecified").to_string();
    let fields = [
        (
            "export.verify.colour.primaries",
            tag(want.primaries),
            tag(found.colour.primaries),
        ),
        (
            "export.verify.colour.transfer",
            tag(want.transfer),
            tag(found.colour.transfer),
        ),
        (
            "export.verify.colour.matrix",
            tag(want.matrix),
            tag(found.colour.matrix),
        ),
        (
            "export.verify.colour.range",
            range_name(want.range).to_string(),
            range_name(found.colour.range).to_string(),
        ),
    ];
    for (key, expected, got) in fields {
        report.compare(Check::Colour, key, expected, got);
    }
    if let Some(hdr) = &video.hdr_metadata {
        report.check(Check::HdrMetadata);
        if hdr.mastering.is_some() && !found.mastering_display_metadata {
            report.push(Finding::failure(
                Check::HdrMetadata,
                "export.verify.hdr.mastering_display",
                Vec::new(),
            ));
        }
        if hdr.content_light.is_some() && !found.content_light_level {
            report.push(Finding::failure(
                Check::HdrMetadata,
                "export.verify.hdr.content_light",
                Vec::new(),
            ));
        }
    }
}

/// Every message key a [`Finding`] may carry, for the i18n check.
pub const MESSAGE_KEYS: &[&str] = &[
    "export.verify.unreadable",
    "export.verify.container",
    "export.verify.video.missing",
    "export.verify.video.codec",
    "export.verify.video.damaged",
    "export.verify.resolution",
    "export.verify.frame_rate",
    "export.verify.frame_count",
    "export.verify.video.duration",
    "export.verify.audio.missing",
    "export.verify.audio.codec",
    "export.verify.audio.damaged",
    "export.verify.audio.sample_rate",
    "export.verify.audio.channels",
    "export.verify.audio.duration",
    "export.verify.loudness.silent",
    "export.verify.loudness.integrated",
    "export.verify.loudness.true_peak",
    "export.verify.limit.file_size",
    "export.verify.limit.duration",
    "export.verify.limit.aspect",
    "export.verify.colour.primaries",
    "export.verify.colour.transfer",
    "export.verify.colour.matrix",
    "export.verify.colour.range",
    "export.verify.hdr.mastering_display",
    "export.verify.hdr.content_light",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_keys_are_english_strings_with_their_placeholders() {
        let english = assets::i18n::Locale::english();
        for key in MESSAGE_KEYS {
            assert!(english.get(key).is_some(), "{key} missing from en.json");
        }
        let text = english
            .get("export.verify.loudness.integrated")
            .expect("key");
        let mut names = assets::i18n::placeholders(text).expect("placeholders");
        names.sort_unstable();
        assert_eq!(names, ["found", "target", "tolerance"]);
    }

    #[test]
    fn numbers_drop_trailing_zeros() {
        assert_eq!(number(30.0, 3), "30");
        assert_eq!(number(30_000.0 / 1001.0, 3), "29.97");
        assert_eq!(number(-23.04, 1), "-23");
        assert_eq!(number(0.5, 3), "0.5");
    }

    fn target(strictness: Strictness) -> LoudnessTarget {
        LoudnessTarget {
            strictness,
            ..LoudnessTarget::EBU_R128
        }
    }

    #[test]
    fn loudness_outside_a_hard_target_fails_and_a_soft_one_warns() {
        let mut report = VerifyReport::default();
        check_loudness(
            &mut report,
            &target(Strictness::Hard),
            Some(-23.4),
            Some(-1.0),
        );
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(report.checked, [Check::Loudness, Check::TruePeak]);

        let mut report = VerifyReport::default();
        check_loudness(
            &mut report,
            &target(Strictness::Hard),
            Some(-20.0),
            Some(-0.5),
        );
        assert_eq!(
            report.findings,
            [
                Finding::failure(
                    Check::Loudness,
                    "export.verify.loudness.integrated",
                    vec![
                        ("found", "-20".into()),
                        ("target", "-23".into()),
                        ("tolerance", "0.5".into()),
                    ],
                ),
                Finding::failure(
                    Check::TruePeak,
                    "export.verify.loudness.true_peak",
                    vec![("found", "-0.5".into()), ("max", "-1".into())],
                ),
            ]
        );

        let mut report = VerifyReport::default();
        check_loudness(&mut report, &target(Strictness::Soft), None, None);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Warning);
        assert_eq!(report.findings[0].key, "export.verify.loudness.silent");
        assert!(report.passed());
    }
}
