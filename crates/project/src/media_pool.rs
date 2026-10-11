//! The media pool: every source file a project uses, organised into bins, with colour
//! labels, tags and camera metadata. Plain serde data with no dependency on the rest of
//! the document model, so the project root can embed a [`MediaPool`] as it is.

use serde::{Deserialize, Serialize};

mod pool;
mod search;

pub use pool::{
    Bin, ColourLabel, MediaItem, MediaKind, MediaPool, MediaStatus, OfflineReason, PoolError,
    SortBy, natural_cmp,
};
pub use search::{Hit, Query, SearchIndex, tokens};

/// Camera and capture metadata for one media item, gathered from the container's tags,
/// EXIF for stills and camera sidecar files. Every field is optional: most files carry
/// only a few of them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub f_number: Option<f32>,
    pub iso: Option<u32>,
    /// Shutter as the camera states it (`1/50`, `1/1000.0`, `180.0°`).
    pub shutter: Option<String>,
    /// SMPTE timecode of the first frame.
    pub timecode: Option<String>,
    /// Reel or card name (`A014`).
    pub reel: Option<String>,
    pub gps: Option<Gps>,
    /// When the recording started, ISO 8601 as stored (with its offset when the file has
    /// one).
    pub created: Option<String>,
}

impl Metadata {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Fills every field that is `None` here from `other`.
    pub fn fill_from(&mut self, other: &Metadata) {
        fn fill<T: Clone>(a: &mut Option<T>, b: &Option<T>) {
            if a.is_none() {
                a.clone_from(b);
            }
        }
        fill(&mut self.camera_make, &other.camera_make);
        fill(&mut self.camera_model, &other.camera_model);
        fill(&mut self.lens, &other.lens);
        fill(&mut self.focal_length_mm, &other.focal_length_mm);
        fill(&mut self.f_number, &other.f_number);
        fill(&mut self.iso, &other.iso);
        fill(&mut self.shutter, &other.shutter);
        fill(&mut self.timecode, &other.timecode);
        fill(&mut self.reel, &other.reel);
        fill(&mut self.gps, &other.gps);
        fill(&mut self.created, &other.created);
    }
}

/// A position in WGS 84 degrees (north and east positive), altitude in metres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gps {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude_m: Option<f64>,
}

/// Stream facts read when the file was imported, kept so the pool can show and search
/// them while the file is offline.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Facts {
    /// Seconds; `None` for stills and unknown lengths.
    pub duration_s: Option<f64>,
    /// Display size (after rotation); 0 when there is no picture.
    pub width: u32,
    pub height: u32,
    /// Frames per second as a fraction (`24000/1001`).
    pub frame_rate: Option<(i32, i32)>,
    /// FFmpeg's codec name (`h264`, `prores`) and profile (`High`, `HQ`).
    pub video_codec: Option<String>,
    pub video_profile: Option<String>,
    pub bit_depth: u8,
    /// `4:2:0`, `4:2:2`, `4:4:4` or `RGB`.
    pub chroma: Option<String>,
    /// The colour primaries' short name (`bt709`, `bt2020`, `p3`).
    pub primaries: Option<String>,
    /// `pq` or `hlg` for HDR sources.
    pub hdr: Option<String>,
    pub audio_codec: Option<String>,
    pub audio_channels: u16,
    pub sample_rate: u32,
    pub audio_bit_depth: Option<u8>,
    /// Frames in an image sequence.
    pub sequence_frames: Option<u64>,
    /// The container's short name (`mov,mp4,m4a,3gp,3g2,mj2`).
    pub container: Option<String>,
}

impl Facts {
    pub fn has_video(&self) -> bool {
        self.width > 0 && self.height > 0
    }

    pub fn has_audio(&self) -> bool {
        self.audio_channels > 0
    }

    pub fn fps(&self) -> Option<f64> {
        self.frame_rate
            .filter(|(_, d)| *d != 0)
            .map(|(n, d)| n as f64 / d as f64)
    }
}
