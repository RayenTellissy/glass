//! Helpers for the audio mix tests: tone files written as 32-bit float WAV (decoded back
//! bit for bit, so expected sums can be worked out from the same samples) and a project of
//! audio tracks over them.

#![allow(dead_code)]

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use engine::project::{
    ClipSpec, ItemId, MediaId, Project, Rational, Resolution, SequenceFormat, SequenceId, Time,
    TimeRange, TrackId, TrackKind,
};

/// A tone file: `channels` sines (one frequency and amplitude per channel).
#[derive(Clone, Debug)]
pub struct Tone {
    pub rate: u32,
    pub seconds: f64,
    /// (frequency, amplitude) per channel.
    pub channels: Vec<(f64, f64)>,
}

impl Tone {
    pub fn frames(&self) -> usize {
        (self.seconds * f64::from(self.rate)).round() as usize
    }

    /// The stored sample of channel `c` at frame `k` (as f32, as in the file).
    pub fn sample(&self, c: usize, k: usize) -> f32 {
        let (hz, amplitude) = self.channels[c];
        (amplitude * (2.0 * PI * hz * k as f64 / f64::from(self.rate)).sin()) as f32
    }

    /// Writes the tone as a 32-bit float WAV.
    pub fn write(&self, path: &Path) {
        let channels = self.channels.len();
        let frames = self.frames();
        let data_bytes = (frames * channels * 4) as u32;
        let mut bytes = Vec::with_capacity(44 + data_bytes as usize);
        bytes.extend(b"RIFF");
        bytes.extend((36 + data_bytes).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(3u16.to_le_bytes()); // IEEE float
        bytes.extend((channels as u16).to_le_bytes());
        bytes.extend(self.rate.to_le_bytes());
        bytes.extend((self.rate * channels as u32 * 4).to_le_bytes());
        bytes.extend((channels as u16 * 4).to_le_bytes());
        bytes.extend(32u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data_bytes.to_le_bytes());
        for k in 0..frames {
            for c in 0..channels {
                bytes.extend(self.sample(c, k).to_le_bytes());
            }
        }
        std::fs::write(path, bytes).unwrap();
    }
}

/// A fresh folder for one test's files.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-mix-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A clip to place: which tone file, where on the timeline, from where in the file.
pub struct Place {
    pub media: usize,
    pub start: f64,
    pub length: f64,
    pub source_start: f64,
}

/// A 48 kHz stereo sequence with one audio track per entry of `tracks`, each holding its
/// clips, over the tone files in `files`.
pub struct Built {
    pub project: Project,
    pub sequence: SequenceId,
    pub tracks: Vec<TrackId>,
    pub clips: Vec<Vec<ItemId>>,
}

pub fn samples(seconds: f64) -> Time {
    Time::from_samples((seconds * 48_000.0).round() as i64, 48_000).unwrap()
}

pub fn build(files: &[PathBuf], tracks: &[Vec<Place>]) -> Built {
    let mut p = Project::new("mix-test", "Mix test");
    let mut media: Vec<MediaId> = Vec::new();
    for file in files {
        let (next, id) = p.add_media("", file.to_string_lossy()).unwrap();
        p = next;
        media.push(id);
    }
    let (next, sequence) = p
        .add_sequence_with(
            "Mix",
            SequenceFormat {
                resolution: Resolution::new(1920, 1080),
                frame_rate: Rational::new(30, 1),
                ..SequenceFormat::default()
            },
        )
        .unwrap();
    p = next;
    let mut track_ids = Vec::new();
    let mut clip_ids = Vec::new();
    for (i, places) in tracks.iter().enumerate() {
        let (next, track) = p
            .add_track(sequence, TrackKind::Audio, format!("A{}", i + 1), None)
            .unwrap();
        p = next;
        let mut items = Vec::new();
        for place in places {
            let (next, item) = p
                .insert_clip(
                    track,
                    ClipSpec {
                        media: media[place.media],
                        range: TimeRange::new(samples(place.start), samples(place.length)),
                        source_start: samples(place.source_start),
                    },
                )
                .unwrap();
            p = next;
            items.push(item);
        }
        track_ids.push(track);
        clip_ids.push(items);
    }
    Built {
        project: p,
        sequence,
        tracks: track_ids,
        clips: clip_ids,
    }
}
