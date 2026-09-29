//! Thumbnail strips for the timeline and media pool, at three zoom levels.
//!
//! | Level | One thumbnail every | Use |
//! | --- | --- | --- |
//! | 0 | 1 s | zoomed in: a 108 px tall clip shows a new picture about every thumbnail width |
//! | 1 | 5 s | the default timeline zoom |
//! | 2 | 30 s | zoomed out, and the first thing on screen |
//!
//! Thumbnail `i` of a level shows the frame on screen `i * interval` after the clip's
//! first frame; a clip of duration `d` has `max(1, ceil(d / interval))` of them, and a
//! still has one. Every thumbnail is [`ThumbnailSettings::height`] pixels tall (108 by
//! default: 54 points on a 2x display, the timeline's video track) and as wide as the
//! display aspect ratio gives (sample aspect ratio, then the display rotation), between
//! a quarter and four times the height.
//!
//! **Speed.** Frames are not decoded in sequence. A video thumbnail seeks: when a
//! keyframe (from the cached [`GopIndex`]) lies at most `min(interval / 2, 1 s)` before
//! the target, the keyframe itself is shown ([`VideoSource::seek_keyframe`]: one demuxer
//! seek, usually one decoded frame); otherwise [`VideoSource::seek_exact`] decodes
//! forward from the keyframe to the exact frame. The
//! actual time of each thumbnail is stored. Levels are produced coarse first (2, 1, 0) so
//! the UI can show something at once, and a frame already scaled for a coarser level is
//! reused (every 30 s and 5 s target is also a 1 s target). Conversion is CPU swscale
//! straight from the decoded planes to the thumbnail size ([`VideoFrame::to_rgba8_scaled`]):
//! it keeps the GPU and the UI thread free for playback, and it costs well under a
//! millisecond per thumbnail, so a GPU round trip would save nothing. Decoding is
//! software with two threads (hardware surfaces would need the GPU to convert).
//!
//! **Storage** (under the job's [`CacheEntry`]): each level is a set of atlases of up to
//! 64 thumbnails (8 columns, row-major), `<stem>.l<level>.c<chunk>.jpg` (JPEG quality
//! 80; PNG `.png` when the source has alpha), a per-level index `<stem>.l<level>.json`
//! written after its atlases (the level is then usable), and the main index
//! `<stem>.json` last. JPEG was chosen over PNG, WebP (the `image` crate writes lossless
//! WebP only) and raw RGBA with LZ4: a 192 x 108 thumbnail is about 4 KB as JPEG
//! (measured on the test-pattern fixtures; detailed footage up to about 8 KB)
//! against 30 to 60 KB lossless and 81 KB raw, encodes in well under a millisecond, and
//! decodes on any platform. At 1 s per thumbnail an hour of 16:9 video is 3600 + 720 +
//! 120 thumbnails, about 17 to 35 MB. The UI loads atlases lazily, only the chunks of the
//! visible range.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::analysis::{AnalysisError, Control};
use crate::cache::{CacheDir, CacheEntry};
use crate::convert::RgbaImage;
use crate::decode::{DecoderOptions, VideoFrame};
use crate::gop::GopIndex;
use crate::probe::probe;
use crate::reader::StreamSelect;
use crate::source::{SourceKind, SourceOptions, VideoSource};
use crate::time::{Rational, Timestamp};

/// Seconds between thumbnails at each level, finest first.
pub const LEVEL_INTERVALS: [u32; 3] = [1, 5, 30];
/// Thumbnails per atlas file.
pub const PER_ATLAS: u32 = 64;
/// Atlas columns.
pub const ATLAS_COLUMNS: u32 = 8;
/// Bumped when the output or the algorithm changes.
pub const THUMBNAILS_VERSION: u32 = 1;

/// How thumbnails are made; part of the cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbnailSettings {
    /// Thumbnail height in pixels.
    pub height: u32,
    /// JPEG quality, 1 to 100.
    pub quality: u8,
    /// The rate an image sequence plays at (the project's).
    pub sequence_rate: Rational,
}

impl Default for ThumbnailSettings {
    fn default() -> Self {
        Self {
            height: 108,
            quality: 80,
            sequence_rate: Rational::new(30, 1),
        }
    }
}

impl ThumbnailSettings {
    /// The canonical parameter string for the cache key.
    pub fn params(&self) -> String {
        format!(
            "h={};q={};seq={}/{};levels=1,5,30",
            self.height, self.quality, self.sequence_rate.num, self.sequence_rate.den
        )
    }
}

/// Atlas image format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AtlasFormat {
    Jpeg,
    Png,
}

impl AtlasFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }
}

/// One zoom level.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThumbnailLevel {
    pub level: u32,
    /// Seconds between thumbnails.
    pub interval: u32,
    /// Time of the frame each thumbnail shows, microseconds after the clip's first frame.
    pub times_us: Vec<i64>,
    /// Atlas file names, in the cache entry's folder.
    pub atlases: Vec<String>,
}

impl ThumbnailLevel {
    pub fn len(&self) -> usize {
        self.times_us.len()
    }

    pub fn is_empty(&self) -> bool {
        self.times_us.is_empty()
    }

    /// The thumbnail at `t` seconds after the clip's start (the one whose interval
    /// covers it), clamped to the strip.
    pub fn index_at(&self, seconds: f64) -> usize {
        let i = (seconds.max(0.0) / f64::from(self.interval)).floor() as usize;
        i.min(self.len().saturating_sub(1))
    }
}

/// Where a thumbnail is: atlas file and pixel rectangle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThumbnailLocation<'a> {
    pub atlas: &'a str,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The index of a source's thumbnail strips (the main cache file).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThumbnailStrips {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub format: AtlasFormat,
    pub columns: u32,
    pub per_atlas: u32,
    /// Finest first: `levels[0]` is 1 s.
    pub levels: Vec<ThumbnailLevel>,
}

impl ThumbnailStrips {
    /// Where thumbnail `index` of `level` is.
    pub fn locate(&self, level: usize, index: usize) -> Option<ThumbnailLocation<'_>> {
        let level = self.levels.get(level)?;
        if index >= level.len() {
            return None;
        }
        let chunk = index / self.per_atlas as usize;
        let within = (index % self.per_atlas as usize) as u32;
        Some(ThumbnailLocation {
            atlas: level.atlases.get(chunk)?,
            x: (within % self.columns) * self.width,
            y: (within / self.columns) * self.height,
            width: self.width,
            height: self.height,
        })
    }

    /// The finished levels on disk, including those of a run still in progress (each
    /// level's index is written once its atlases are).
    pub fn load_levels(entry: &CacheEntry) -> Vec<ThumbnailLevel> {
        (0..LEVEL_INTERVALS.len())
            .filter_map(|l| entry.read_json(&format!("l{l}.json")))
            .collect()
    }

    /// Reads and decodes one atlas (off the UI thread).
    pub fn load_atlas(entry: &CacheEntry, atlas: &str) -> Result<RgbaImage, AnalysisError> {
        let bytes = std::fs::read(entry.dir().join(atlas))?;
        let image =
            image::load_from_memory(&bytes).map_err(|e| AnalysisError::Encode(e.to_string()))?;
        let rgba = image.into_rgba8();
        Ok(RgbaImage {
            width: rgba.width(),
            height: rgba.height(),
            data: rgba.into_raw(),
        })
    }

    /// Cuts one thumbnail out of a loaded atlas.
    pub fn crop(atlas: &RgbaImage, at: &ThumbnailLocation<'_>) -> Option<RgbaImage> {
        if at.x + at.width > atlas.width || at.y + at.height > atlas.height {
            return None;
        }
        let mut data = Vec::with_capacity((at.width * at.height * 4) as usize);
        for y in at.y..at.y + at.height {
            let start = ((y * atlas.width + at.x) * 4) as usize;
            data.extend_from_slice(&atlas.data[start..start + (at.width * 4) as usize]);
        }
        Some(RgbaImage {
            width: at.width,
            height: at.height,
            data,
        })
    }
}

/// Picture geometry of a source as displayed.
#[derive(Clone, Copy, Debug)]
struct Geometry {
    /// Size to scale decoded frames to, before rotation.
    scaled: (u32, u32),
    /// Counter-clockwise quarter turns.
    quarter_turns: u32,
    mirrored: bool,
}

impl Geometry {
    fn output(&self) -> (u32, u32) {
        if self.quarter_turns % 2 == 1 {
            (self.scaled.1, self.scaled.0)
        } else {
            self.scaled
        }
    }
}

/// Makes the thumbnail strips of a picture source and writes them into `entry`. `cache`
/// holds (or receives) the GOP index of a video. Levels are written coarse first, each
/// announced with [`Control::partial`].
pub fn generate(
    path: &std::path::Path,
    settings: &ThumbnailSettings,
    entry: &CacheEntry,
    cache: Option<&CacheDir>,
    control: &dyn Control,
) -> Result<ThumbnailStrips, AnalysisError> {
    let mut options = SourceOptions::new(settings.sequence_rate);
    options.decoder = DecoderOptions {
        threads: 2,
        ..DecoderOptions::default()
    };
    let mut source = VideoSource::open(path, options)?;
    let info = source.info().clone();
    let (rotation, mirrored, sar) = if info.kind == SourceKind::Video {
        let probed = probe(path)?;
        match probed.video() {
            Some((_, v)) => (v.rotation, v.mirrored, v.sample_aspect_ratio),
            None => (0, false, Rational::new(1, 1)),
        }
    } else {
        (0, false, Rational::new(1, 1))
    };
    let quarter_turns = u32::from(((rotation + 45) / 90) % 4);
    let sar = sar.to_f64().filter(|r| *r > 0.0).unwrap_or(1.0);
    let display_w = f64::from(info.width.max(1)) * sar;
    let display_h = f64::from(info.height.max(1));
    let aspect = if quarter_turns % 2 == 1 {
        display_h / display_w
    } else {
        display_w / display_h
    };
    let height = settings.height.max(8);
    let width = ((f64::from(height) * aspect).round() as u32).clamp(height / 4, height * 4);
    let geometry = Geometry {
        scaled: if quarter_turns % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        },
        quarter_turns,
        mirrored,
    };
    let format = if info.alpha {
        AtlasFormat::Png
    } else {
        AtlasFormat::Jpeg
    };

    let timeline = source.timeline()?;
    let start = Timestamp::new(timeline.start(), timeline.time_base);
    let duration_us = if timeline.is_still() {
        0
    } else {
        timeline.duration().rescale(Rational::MICROS).value.max(0)
    };
    let gop = if info.kind == SourceKind::Video {
        let built = match cache {
            Some(cache) => GopIndex::load_or_build(cache, path, StreamSelect::BestVideo),
            None => GopIndex::build(path, StreamSelect::BestVideo),
        };
        match built {
            Ok((gop, _)) => Some(gop),
            Err(error) => {
                tracing::debug!(path = %path.display(), %error, "no GOP index; exact seeks only");
                None
            }
        }
    } else {
        None
    };

    let counts: Vec<u64> = LEVEL_INTERVALS
        .iter()
        .map(|&s| {
            let interval = i64::from(s) * 1_000_000;
            ((duration_us + interval - 1) / interval).max(1) as u64
        })
        .collect();
    // Distinct targets over all levels, for progress (coarse targets repeat in finer ones).
    let total = counts[0].max(1) as f32;
    let mut done_targets: HashMap<i64, (i64, RgbaImage)> = HashMap::new();
    let mut levels: Vec<Option<ThumbnailLevel>> = vec![None; LEVEL_INTERVALS.len()];

    for level in (0..LEVEL_INTERVALS.len()).rev() {
        let interval_us = i64::from(LEVEL_INTERVALS[level]) * 1_000_000;
        let mut times = Vec::with_capacity(counts[level] as usize);
        let mut thumbs: Vec<RgbaImage> = Vec::with_capacity(counts[level] as usize);
        for i in 0..counts[level] as i64 {
            control.checkpoint()?;
            let nominal = i * interval_us;
            let (time, image) = match done_targets.get(&nominal) {
                Some(hit) => hit.clone(),
                None => {
                    let target = start.rescale(Rational::MICROS).value + nominal;
                    let frame = match snap_to_keyframe(gop.as_ref(), target, interval_us) {
                        Some(keyframe) => {
                            source.seek_keyframe(Timestamp::new(keyframe, Rational::MICROS))?
                        }
                        None => source.seek_exact(Timestamp::new(target, Rational::MICROS))?,
                    };
                    let (time, image) = match frame {
                        Some(frame) => {
                            let time = frame_time_us(&frame, start);
                            (time, thumbnail(&frame, &geometry)?)
                        }
                        None => (nominal, blank(geometry.output())),
                    };
                    done_targets.insert(nominal, (time, image.clone()));
                    control.progress(done_targets.len() as f32 / total);
                    (time, image)
                }
            };
            times.push(time);
            thumbs.push(image);
        }
        let mut atlases = Vec::new();
        for (chunk, group) in thumbs.chunks(PER_ATLAS as usize).enumerate() {
            control.checkpoint()?;
            let suffix = format!("l{level}.c{chunk}.{}", format.extension());
            let bytes = encode_atlas(group, geometry.output(), format, settings.quality)?;
            entry.write(&suffix, &bytes)?;
            atlases.push(entry.file_name(&suffix));
        }
        let strip = ThumbnailLevel {
            level: level as u32,
            interval: LEVEL_INTERVALS[level],
            times_us: times,
            atlases,
        };
        entry.write_json(&format!("l{level}.json"), &strip)?;
        control.partial(level as u32);
        levels[level] = Some(strip);
    }

    let (width, height) = geometry.output();
    Ok(ThumbnailStrips {
        version: THUMBNAILS_VERSION,
        width,
        height,
        format,
        columns: ATLAS_COLUMNS,
        per_atlas: PER_ATLAS,
        levels: levels.into_iter().flatten().collect(),
    })
}

/// The time (microseconds, rounded up so a seek lands on it) of the keyframe at or
/// before `target` when it lies at most `min(interval / 2, 1 s)` before it.
fn snap_to_keyframe(gop: Option<&GopIndex>, target: i64, interval_us: i64) -> Option<i64> {
    let gop = gop?;
    let tolerance = (interval_us / 2).min(1_000_000);
    let k = gop
        .keyframe_at_or_before(Timestamp::new(target, Rational::MICROS))
        .and_then(|k| gop.keyframe(k))?;
    let kf = -Timestamp::new(-k.pts, gop.time_base)
        .rescale_floor(Rational::MICROS)
        .value;
    (kf <= target && target - kf <= tolerance).then_some(kf)
}

fn frame_time_us(frame: &VideoFrame, start: Timestamp) -> i64 {
    frame.pts().map_or(0, |pts| {
        pts.rescale(Rational::MICROS).value - start.rescale(Rational::MICROS).value
    })
}

fn thumbnail(frame: &VideoFrame, geometry: &Geometry) -> Result<RgbaImage, AnalysisError> {
    let (w, h) = geometry.scaled;
    let scaled = frame.to_rgba8_scaled(w, h)?;
    Ok(orient(scaled, geometry.quarter_turns, geometry.mirrored))
}

fn blank((width, height): (u32, u32)) -> RgbaImage {
    RgbaImage {
        width,
        height,
        data: [0, 0, 0, 255].repeat((width * height) as usize),
    }
}

/// Mirrors horizontally (when the display matrix does), then turns counter-clockwise by
/// `quarter_turns` quarter turns.
fn orient(image: RgbaImage, quarter_turns: u32, mirrored: bool) -> RgbaImage {
    if quarter_turns == 0 && !mirrored {
        return image;
    }
    let (w, h) = (image.width as usize, image.height as usize);
    let (ow, oh) = if quarter_turns % 2 == 1 {
        (h, w)
    } else {
        (w, h)
    };
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..h {
        for x in 0..w {
            let sx = if mirrored { w - 1 - x } else { x };
            let (nx, ny) = match quarter_turns {
                1 => (y, w - 1 - x),
                2 => (w - 1 - x, h - 1 - y),
                3 => (h - 1 - y, x),
                _ => (x, y),
            };
            let src = (y * w + sx) * 4;
            let dst = (ny * ow + nx) * 4;
            out[dst..dst + 4].copy_from_slice(&image.data[src..src + 4]);
        }
    }
    RgbaImage {
        width: ow as u32,
        height: oh as u32,
        data: out,
    }
}

fn encode_atlas(
    thumbs: &[RgbaImage],
    (width, height): (u32, u32),
    format: AtlasFormat,
    quality: u8,
) -> Result<Vec<u8>, AnalysisError> {
    use image::ImageEncoder;
    let count = thumbs.len() as u32;
    let columns = ATLAS_COLUMNS.min(count.max(1));
    let rows = count.div_ceil(ATLAS_COLUMNS).max(1);
    let (aw, ah) = (columns * width, rows * height);
    let channels = match format {
        AtlasFormat::Jpeg => 3,
        AtlasFormat::Png => 4,
    };
    let mut pixels = vec![0u8; (aw * ah) as usize * channels];
    for (i, thumb) in thumbs.iter().enumerate() {
        let (cx, cy) = (
            (i as u32 % ATLAS_COLUMNS) * width,
            (i as u32 / ATLAS_COLUMNS) * height,
        );
        for y in 0..height.min(thumb.height) {
            for x in 0..width.min(thumb.width) {
                let src = ((y * thumb.width + x) * 4) as usize;
                let dst = (((cy + y) * aw + cx + x) as usize) * channels;
                pixels[dst..dst + channels].copy_from_slice(&thumb.data[src..src + channels]);
            }
        }
    }
    let mut out = Vec::new();
    let result = match format {
        AtlasFormat::Jpeg => image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .write_image(&pixels, aw, ah, image::ExtendedColorType::Rgb8),
        AtlasFormat::Png => image::codecs::png::PngEncoder::new(&mut out).write_image(
            &pixels,
            aw,
            ah,
            image::ExtendedColorType::Rgba8,
        ),
    };
    result.map_err(|e| AnalysisError::Encode(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(w: u32, h: u32) -> RgbaImage {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[x as u8, y as u8, 0, 255]);
            }
        }
        RgbaImage {
            width: w,
            height: h,
            data,
        }
    }

    #[test]
    fn quarter_turns_move_the_top_left_corner_counter_clockwise() {
        let image = marked(4, 2);
        // 90 CCW: top-left goes to bottom-left.
        let turned = orient(image.clone(), 1, false);
        assert_eq!((turned.width, turned.height), (2, 4));
        assert_eq!(turned.pixel(0, 3), Some([0, 0, 0, 255]));
        assert_eq!(turned.pixel(0, 0), Some([3, 0, 0, 255]));
        let half = orient(image.clone(), 2, false);
        assert_eq!(half.pixel(3, 1), Some([0, 0, 0, 255]));
        let three = orient(image.clone(), 3, false);
        assert_eq!(three.pixel(1, 0), Some([0, 0, 0, 255]));
        let mirrored = orient(image, 0, true);
        assert_eq!(mirrored.pixel(3, 0), Some([0, 0, 0, 255]));
    }

    #[test]
    fn atlases_place_thumbnails_row_major() {
        let strips = ThumbnailStrips {
            version: THUMBNAILS_VERSION,
            width: 192,
            height: 108,
            format: AtlasFormat::Jpeg,
            columns: ATLAS_COLUMNS,
            per_atlas: PER_ATLAS,
            levels: vec![ThumbnailLevel {
                level: 0,
                interval: 1,
                times_us: (0..70).map(|i| i * 1_000_000).collect(),
                atlases: vec!["a".into(), "b".into()],
            }],
        };
        let at = strips.locate(0, 9).expect("located");
        assert_eq!((at.atlas, at.x, at.y), ("a", 192, 108));
        let at = strips.locate(0, 65).expect("located");
        assert_eq!((at.atlas, at.x, at.y), ("b", 192, 0));
        assert!(strips.locate(0, 70).is_none());
        assert_eq!(strips.levels[0].index_at(9.5), 9);
        assert_eq!(strips.levels[0].index_at(1e9), 69);
    }
}
