//! Project settings (schema v6): the format new sequences start with, the project colour
//! space and the cache folder, and the edits that change them.
//!
//! Settings live at two levels. Each sequence carries its own complete format
//! ([`SequenceFormat`]: canvas resolution, frame rate and drop-frame flag, sample rate and
//! channel layout); the project carries the format new sequences copy
//! ([`ProjectSettings::sequence`]) plus the settings that belong to the whole project: the
//! colour space, which picks the render working primaries (Decision 15), and the cache
//! folder. Changing the project's format changes no existing sequence; changing a
//! sequence's format ([`Project::set_sequence_format`]) changes only that sequence.
//!
//! A sequence's frame rate and sample rate set the grids its items sit on
//! ([`crate::model::Sequence::grid`]). Changing either is refused when anything on the
//! sequence would fall off the new grid (an item's start or length, a transition's length
//! or its fit around its cut, a sequence marker); nothing is moved or retimed. The canvas
//! size, the drop-frame flag and the channel layout change freely: transforms are offsets
//! from the canvas centre (Decision 45), drop-frame is how timecode is counted, and the
//! engine mixes any clip into any layout.
//!
//! The cache folder is a policy, not a folder: [`CacheFolder::resolve`] turns it into a
//! path from the project file's path, without touching the disk. `app::locations` probes
//! the result and falls back to the per-user cache folder when it cannot be written, and
//! a project without a file keeps its caches and autosave in the per-user cache folder
//! ([`crate::autosave::autosave_location`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::EditError;
use crate::id::SequenceId;
use crate::marker::check_marker;
use crate::model::Project;
use crate::time::Rational;
use crate::transition::check_track;

/// The narrowest and shortest canvas side, in pixels.
pub const MIN_SIDE: u32 = 16;
/// The widest and tallest canvas side, in pixels (8K DCI is 8192 wide).
pub const MAX_SIDE: u32 = 8192;
/// The slowest frame rate, in frames per second.
pub const MIN_FPS: i64 = 1;
/// The fastest frame rate, in frames per second.
pub const MAX_FPS: i64 = 240;
/// The audio sample rates a sequence may use, in Hz.
pub const SAMPLE_RATES: [u32; 3] = [44_100, 48_000, 96_000];
/// The cache folder of a project with a file, relative to the file's folder.
pub const DEFAULT_CACHE_FOLDER: &str = ".glass-cache";

/// The size of a sequence's canvas, in pixels. Both sides are even (4:2:0 video halves
/// them) and within [`MIN_SIDE`] to [`MAX_SIDE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const HD: Resolution = Resolution::new(1920, 1080);
    pub const HD_720: Resolution = Resolution::new(1280, 720);
    pub const UHD: Resolution = Resolution::new(3840, 2160);
    pub const DCI_4K: Resolution = Resolution::new(4096, 2160);
    pub const VERTICAL_HD: Resolution = Resolution::new(1080, 1920);
    pub const SQUARE: Resolution = Resolution::new(1080, 1080);

    /// The sizes offered in the settings dialog, landscape first.
    pub const PRESETS: [Resolution; 6] = [
        Resolution::HD,
        Resolution::UHD,
        Resolution::HD_720,
        Resolution::DCI_4K,
        Resolution::VERTICAL_HD,
        Resolution::SQUARE,
    ];

    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Both sides even and within [`MIN_SIDE`] to [`MAX_SIDE`].
    pub fn check(self) -> Result<(), EditError> {
        let ok = |side: u32| (MIN_SIDE..=MAX_SIDE).contains(&side) && side.is_multiple_of(2);
        if ok(self.width) && ok(self.height) {
            Ok(())
        } else {
            Err(EditError::InvalidResolution {
                width: self.width,
                height: self.height,
            })
        }
    }
}

/// A frame rate offered in the settings dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRatePreset {
    pub rate: Rational,
    /// How the dialog shows it (`23.976`); not translated, numbers read the same.
    pub name: &'static str,
}

/// The common frame rates, slowest first. Any other rate from [`MIN_FPS`] to [`MAX_FPS`]
/// is allowed too.
pub const FRAME_RATE_PRESETS: [FrameRatePreset; 8] = [
    FrameRatePreset {
        rate: Rational::new(24000, 1001),
        name: "23.976",
    },
    FrameRatePreset {
        rate: Rational::new(24, 1),
        name: "24",
    },
    FrameRatePreset {
        rate: Rational::new(25, 1),
        name: "25",
    },
    FrameRatePreset {
        rate: Rational::new(30000, 1001),
        name: "29.97",
    },
    FrameRatePreset {
        rate: Rational::new(30, 1),
        name: "30",
    },
    FrameRatePreset {
        rate: Rational::new(50, 1),
        name: "50",
    },
    FrameRatePreset {
        rate: Rational::new(60000, 1001),
        name: "59.94",
    },
    FrameRatePreset {
        rate: Rational::new(60, 1),
        name: "60",
    },
];

/// The same number of frames per second (`50/2` is `25/1`).
fn same_rate(a: Rational, b: Rational) -> bool {
    i64::from(a.num) * i64::from(b.den) == i64::from(b.num) * i64::from(a.den)
}

/// A usable frame rate: both parts positive and from [`MIN_FPS`] to [`MAX_FPS`] frames
/// per second.
pub fn check_frame_rate(rate: Rational) -> Result<(), EditError> {
    let (num, den) = (i64::from(rate.num), i64::from(rate.den));
    if rate.is_positive_rate() && num >= MIN_FPS * den && num <= MAX_FPS * den {
        Ok(())
    } else {
        Err(EditError::InvalidFrameRate {
            num: rate.num,
            den: rate.den,
        })
    }
}

/// Whether timecode at `rate` may count drop-frame: 29.97 and 59.94 (30000/1001 and
/// 60000/1001, in any form). Drop-frame skips frame numbers, never frames, so the grid
/// does not change with it.
pub fn allows_drop_frame(rate: Rational) -> bool {
    same_rate(rate, Rational::new(30000, 1001)) || same_rate(rate, Rational::new(60000, 1001))
}

/// One of [`SAMPLE_RATES`].
pub fn check_sample_rate(rate: u32) -> Result<(), EditError> {
    if SAMPLE_RATES.contains(&rate) {
        Ok(())
    } else {
        Err(EditError::InvalidSampleRate(rate))
    }
}

/// The speaker layout a sequence mixes to (the same layouts as `media::ChannelLayout`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelLayout {
    Mono,
    #[default]
    Stereo,
    /// FL, FR, FC, LFE, SL, SR.
    Surround51,
}

impl ChannelLayout {
    pub const ALL: [ChannelLayout; 3] = [
        ChannelLayout::Mono,
        ChannelLayout::Stereo,
        ChannelLayout::Surround51,
    ];

    pub fn channels(self) -> usize {
        match self {
            ChannelLayout::Mono => 1,
            ChannelLayout::Stereo => 2,
            ChannelLayout::Surround51 => 6,
        }
    }
}

/// The colour space a project is graded and delivered in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColourSpace {
    /// BT.709 primaries, SDR (BT.1886 display).
    #[default]
    Rec709,
    /// BT.2020 primaries, PQ (SMPTE ST 2084) HDR.
    Rec2020Pq,
    /// BT.2020 primaries, HLG (ARIB STD-B67) HDR.
    Rec2020Hlg,
}

/// The primaries of the render working space (linear light in these primaries).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkingPrimaries {
    Bt709,
    Bt2020,
}

impl ColourSpace {
    pub const ALL: [ColourSpace; 3] = [
        ColourSpace::Rec709,
        ColourSpace::Rec2020Pq,
        ColourSpace::Rec2020Hlg,
    ];

    pub fn is_hdr(self) -> bool {
        !matches!(self, ColourSpace::Rec709)
    }

    /// BT.709 for SDR, BT.2020 for either HDR space (Decision 15).
    pub fn working_primaries(self) -> WorkingPrimaries {
        if self.is_hdr() {
            WorkingPrimaries::Bt2020
        } else {
            WorkingPrimaries::Bt709
        }
    }
}

/// Where a project keeps its caches (analysis, proxies, rendered frames, autosave).
///
/// Serialised as `{"relative": ".glass-cache"}` or `{"absolute": "/Volumes/Fast/cache"}`.
/// The kind is stored rather than guessed from the path, so a file moved between macOS
/// and Windows keeps its meaning.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheFolder {
    /// Relative to the folder holding the project file, so the cache travels with the
    /// project. `/` and `\` both separate folders.
    Relative(String),
    /// A folder anywhere, the same for every location of the project file.
    Absolute(String),
}

impl Default for CacheFolder {
    fn default() -> Self {
        CacheFolder::Relative(DEFAULT_CACHE_FOLDER.to_string())
    }
}

impl CacheFolder {
    /// Not empty, no NUL, and a relative path that does not start at a root or a drive
    /// (`/`, `\`, `C:`), or an absolute one that does. The check reads the text only, the
    /// same on every platform.
    pub fn check(&self) -> Result<(), EditError> {
        let (path, absolute) = match self {
            CacheFolder::Relative(p) => (p, false),
            CacheFolder::Absolute(p) => (p, true),
        };
        let rooted = path.starts_with(['/', '\\'])
            || matches!(path.as_bytes(), [d, b':', ..] if d.is_ascii_alphabetic());
        if path.trim().is_empty() || path.contains('\0') || rooted != absolute {
            return Err(EditError::InvalidCacheFolder(self.clone()));
        }
        Ok(())
    }

    /// The cache folder of a project saved at `project_file`: a relative folder joined to
    /// the file's folder, an absolute one as it is. `None` for a relative folder when the
    /// project has no file yet (its caches stay in the per-user cache folder). Pure path
    /// arithmetic: nothing is created or probed.
    pub fn resolve(&self, project_file: Option<&Path>) -> Option<PathBuf> {
        match self {
            CacheFolder::Absolute(path) => Some(PathBuf::from(path)),
            CacheFolder::Relative(path) => {
                let file = project_file?;
                let dir = file.parent().unwrap_or(Path::new(""));
                Some(
                    path.split(['/', '\\'])
                        .fold(dir.to_path_buf(), |p, c| p.join(c)),
                )
            }
        }
    }
}

/// A sequence's format: everything about it that is not its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SequenceFormat {
    pub resolution: Resolution,
    /// Frames per second; video, subtitle and adjustment items sit on its grid.
    pub frame_rate: Rational,
    /// Timecode counts drop-frame; only at 29.97 and 59.94 ([`allows_drop_frame`]).
    pub drop_frame: bool,
    /// Samples per second, one of [`SAMPLE_RATES`]; audio items sit on its grid.
    pub sample_rate: u32,
    pub channel_layout: ChannelLayout,
}

impl Default for SequenceFormat {
    /// 1920 x 1080 at 30 fps, 48 kHz stereo.
    fn default() -> Self {
        Self {
            resolution: Resolution::HD,
            frame_rate: Rational::new(30, 1),
            drop_frame: false,
            sample_rate: 48_000,
            channel_layout: ChannelLayout::Stereo,
        }
    }
}

impl SequenceFormat {
    /// Checks, in order: the resolution ([`EditError::InvalidResolution`]), the frame
    /// rate ([`EditError::InvalidFrameRate`]), the drop-frame flag
    /// ([`EditError::DropFrameNotAllowed`]) and the sample rate
    /// ([`EditError::InvalidSampleRate`]).
    pub fn check(&self) -> Result<(), EditError> {
        self.resolution.check()?;
        check_frame_rate(self.frame_rate)?;
        if self.drop_frame && !allows_drop_frame(self.frame_rate) {
            return Err(EditError::DropFrameNotAllowed(self.frame_rate));
        }
        check_sample_rate(self.sample_rate)
    }
}

/// The project-wide settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectSettings {
    /// The format new sequences start with. Existing sequences keep their own.
    pub sequence: SequenceFormat,
    /// Picks the working primaries for every sequence (Decision 15) and the default
    /// export tags.
    pub colour_space: ColourSpace,
    pub cache_folder: CacheFolder,
}

impl ProjectSettings {
    /// Checks the sequence format ([`SequenceFormat::check`]), then the cache folder
    /// ([`EditError::InvalidCacheFolder`]).
    pub fn check(&self) -> Result<(), EditError> {
        self.sequence.check()?;
        self.cache_folder.check()
    }
}

impl Project {
    pub fn settings(&self) -> &ProjectSettings {
        &self.settings
    }

    /// Replaces the project settings. No sequence changes: the format is what new
    /// sequences start with ([`Project::add_sequence_with`]). Settings equal to the
    /// current ones return this root unchanged.
    pub fn set_project_settings(&self, settings: ProjectSettings) -> Result<Project, EditError> {
        settings.check()?;
        if *self.settings == settings {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.settings = Arc::new(settings);
        Ok(next)
    }

    /// Changes a sequence's format. The format is checked first ([`SequenceFormat::check`]);
    /// then, when the frame rate or the sample rate changes, everything on the sequence
    /// must sit on the new grids: tracks top to bottom, each track's items in time order
    /// ([`EditError::OffGrid`]) then its transitions in id order ([`EditError::OffGrid`]
    /// for a length off the grid; [`EditError::TransitionRoom`] or
    /// [`EditError::TransitionHandle`] when a centred transition's split around its cut
    /// moves and no longer fits), then the sequence markers in order
    /// ([`EditError::MarkerOffGrid`]). Nothing is moved. A format equal to the current one
    /// returns this root unchanged.
    pub fn set_sequence_format(
        &self,
        sequence: SequenceId,
        format: SequenceFormat,
    ) -> Result<Project, EditError> {
        format.check()?;
        let index = self
            .sequences
            .iter()
            .position(|s| s.id == sequence)
            .ok_or(EditError::UnknownSequence(sequence))?;
        let current = &self.sequences[index];
        if current.format() == format {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        let seq = next.sequence_at_mut(index);
        let regrid = seq.frame_rate != format.frame_rate || seq.sample_rate != format.sample_rate;
        seq.set_format(format);
        if regrid {
            for track in &seq.tracks {
                let grid = seq.grid(track.kind)?;
                for item in &track.items {
                    crate::edit::check_item(grid, track.kind, item)?;
                }
                check_track(track, grid)?;
            }
            let frame = seq.frame_rate.frame_duration()?;
            for marker in &seq.markers {
                check_marker(Some(frame), marker.range())?;
            }
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid_and_drop_frame_only_at_29_97_and_59_94() {
        for preset in FRAME_RATE_PRESETS {
            check_frame_rate(preset.rate).expect(preset.name);
            let df = matches!(preset.name, "29.97" | "59.94");
            assert_eq!(allows_drop_frame(preset.rate), df, "{}", preset.name);
        }
        assert!(allows_drop_frame(Rational::new(60000 * 2, 1001 * 2)));
        for r in Resolution::PRESETS {
            r.check().expect("preset resolution");
        }
    }

    #[test]
    fn cache_folder_kinds_are_read_from_the_text() {
        let ok = [
            CacheFolder::Relative(".glass-cache".into()),
            CacheFolder::Relative("../shared/cache".into()),
            CacheFolder::Absolute("/Volumes/Fast/cache".into()),
            CacheFolder::Absolute("D:\\cache".into()),
            CacheFolder::Absolute("\\\\server\\share".into()),
        ];
        for folder in ok {
            folder.check().unwrap_or_else(|e| panic!("{folder:?}: {e}"));
        }
        let bad = [
            CacheFolder::Relative(String::new()),
            CacheFolder::Relative("  ".into()),
            CacheFolder::Relative("/abs".into()),
            CacheFolder::Relative("C:\\cache".into()),
            CacheFolder::Absolute("cache".into()),
            CacheFolder::Absolute("/a\0b".into()),
        ];
        for folder in bad {
            assert_eq!(
                folder.check(),
                Err(EditError::InvalidCacheFolder(folder.clone()))
            );
        }
    }

    #[test]
    fn relative_cache_folders_resolve_beside_the_project_file() {
        let file = Path::new("/films/harbour/Harbour.glass");
        assert_eq!(CacheFolder::default().resolve(None), None);
        assert_eq!(
            CacheFolder::default().resolve(Some(file)),
            Some(PathBuf::from("/films/harbour/.glass-cache"))
        );
        assert_eq!(
            CacheFolder::Relative("caches\\harbour".into()).resolve(Some(file)),
            Some(Path::new("/films/harbour").join("caches").join("harbour"))
        );
        let abs = CacheFolder::Absolute("/Volumes/Fast/cache".into());
        assert_eq!(
            abs.resolve(None),
            Some(PathBuf::from("/Volumes/Fast/cache"))
        );
        assert_eq!(
            abs.resolve(Some(file)),
            Some(PathBuf::from("/Volumes/Fast/cache"))
        );
    }
}
