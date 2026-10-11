//! The reference project, `tests/fixtures/reference.proj`: built here in code and saved
//! through `project::schema`, so `smoke --write-reference` regenerates it whenever the
//! schema changes, and opened here with its media resolved beside the file.
//!
//! One 1080p30 sequence, 10 s, from the generated fixtures the pinned FFmpeg writes:
//!
//! ```text
//!   frame  0        45        90        135                 285   300
//!   V2         [E scenes, PiP, 80%]          [F scenes, 150%, 60%   ]
//!   V1     [A h264 ][B prores][C hevc  ][D h264 at 0.5x     ]
//!                 ><        dissolve  ><  dip to black
//!   A1     [audio_48k_stereo, -6 dB]
//! ```
//!
//! Media paths are relative to the project file (`generated/<fixture>`), with no content
//! hash, so the file is the same in every checkout; [`open`] points them at the files
//! beside it (Decision 66).

use std::path::{Path, PathBuf};

use engine::project::{
    self, Alignment, ClipSpec, ColourLabel, MarkerOwner, MarkerSpec, MediaId, Project, Rational,
    Relink, Resolution, SequenceFormat, SequenceId, SpeedKeep, Time, TimeRange, TodoState,
    TrackKind, Transform, TransitionSpec,
};

/// The project file, relative to the repository root.
pub const FILE: &str = "tests/fixtures/reference.proj";

/// Frames per second of the reference sequence.
pub const FPS: i64 = 30;
/// Its length in frames.
pub const LENGTH: i64 = 300;
/// The timeline frames `smoke` exports: over the dip to black and into the slow motion.
pub const EXPORT: std::ops::Range<i64> = 120..150;

/// The fixtures the project uses, with their lengths in seconds (from
/// `tests/fixtures/generate.sh`), so the generator can check transition handles.
const H264: (&str, i64) = ("generated/h264_1080p30.mp4", 3);
const PRORES: (&str, i64) = ("generated/prores422_1080p23976.mov", 2);
const HEVC: (&str, i64) = ("generated/hevc_10bit_1080p25.mp4", 2);
const SCENES: (&str, i64) = ("generated/scenes_cuts_640x360_30.mp4", 12);
const AUDIO: (&str, i64) = ("generated/audio_48k_stereo.wav", 3);

fn at(frames: i64) -> Result<Time, String> {
    Time::from_frames(frames, Rational::new(FPS as i32, 1)).map_err(|e| e.to_string())
}

fn range(start: i64, end: i64) -> Result<TimeRange, String> {
    TimeRange::between(at(start)?, at(end)?).map_err(|e| e.to_string())
}

fn seconds(n: i64) -> Result<Time, String> {
    Time::new(n, 1).map_err(|e| e.to_string())
}

/// Builds the reference project. Deterministic: the same build always gives the same
/// tree, and so the same bytes from [`project::schema::save`].
pub fn build() -> Result<Project, String> {
    let e = |e: project::EditError| e.to_string();
    let p = Project::new("glass-reference", "Reference");
    let mut lengths: Vec<(MediaId, Time)> = Vec::new();
    let mut add =
        |p: &Project, (path, length): (&str, i64)| -> Result<(Project, MediaId), String> {
            let (p, id) = p.add_media("", path).map_err(e)?;
            lengths.push((id, seconds(length)?));
            Ok((p, id))
        };
    let (p, h264) = add(&p, H264)?;
    let (p, prores) = add(&p, PRORES)?;
    let (p, hevc) = add(&p, HEVC)?;
    let (p, scenes) = add(&p, SCENES)?;
    let (p, audio) = add(&p, AUDIO)?;

    let (p, seq) = p
        .add_sequence_with(
            "Reference",
            SequenceFormat {
                resolution: Resolution::new(1920, 1080),
                frame_rate: Rational::new(FPS as i32, 1),
                ..SequenceFormat::default()
            },
        )
        .map_err(e)?;
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).map_err(e)?;
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).map_err(e)?;
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).map_err(e)?;
    let clip = |media, start, end, source: Time| -> Result<ClipSpec, String> {
        Ok(ClipSpec {
            media,
            range: range(start, end)?,
            source_start: source,
        })
    };

    // V1: four clips, a centred cross dissolve on the first cut and a dip to black on the
    // last, which runs into a clip at half speed.
    let (p, a) = p
        .insert_clip(v1, clip(h264, 0, 45, Time::ZERO)?)
        .map_err(e)?;
    let (p, b) = p
        .insert_clip(v1, clip(prores, 45, 90, at(8)?)?)
        .map_err(e)?;
    let (p, c) = p
        .insert_clip(v1, clip(hevc, 90, 135, Time::ZERO)?)
        .map_err(e)?;
    let (p, d) = p
        .insert_clip(v1, clip(h264, 135, 285, at(9)?)?)
        .map_err(e)?;
    let p = p
        .set_clip_speed(d, Rational::new(1, 2), SpeedKeep::Timeline)
        .map_err(e)?;
    let (p, dissolve) = p
        .add_transition(TransitionSpec::new(Some(a), Some(b), at(10)?))
        .map_err(e)?;
    let (p, dip) = p
        .add_transition(TransitionSpec {
            kind: "dip-to-black".to_owned(),
            alignment: Alignment::Centre,
            ..TransitionSpec::new(Some(c), Some(d), at(12)?)
        })
        .map_err(e)?;
    let length = |m: MediaId| lengths.iter().find(|(id, _)| *id == m).map(|(_, t)| *t);
    for t in [dissolve, dip] {
        p.check_transition_media(t, length).map_err(e)?;
    }

    // V2: the scene-cut clip as a turned, translucent picture in picture, and later
    // scaled up at 60% over the slow motion.
    let (p, pip) = p
        .insert_clip(v2, clip(scenes, 15, 105, Time::ZERO)?)
        .map_err(e)?;
    let p = p
        .set_clip_transform(
            pip,
            Transform {
                position: [560.0, -290.0],
                rotation: 4.0,
                ..Transform::IDENTITY
            },
        )
        .map_err(e)?;
    let p = p.set_clip_opacity(pip, 0.8).map_err(e)?;
    let (p, wide) = p
        .insert_clip(v2, clip(scenes, 200, LENGTH, seconds(6)?)?)
        .map_err(e)?;
    let p = p
        .set_clip_transform(
            wide,
            Transform {
                position: [-400.0, 220.0],
                scale: [1.5, 1.5],
                ..Transform::IDENTITY
            },
        )
        .map_err(e)?;
    let p = p.set_clip_opacity(wide, 0.6).map_err(e)?;

    // A1: three seconds of tone at -6 dB.
    let (p, tone) = p
        .insert_clip(a1, clip(audio, 0, 90, Time::ZERO)?)
        .map_err(e)?;
    let p = p.set_clip_gain(tone, -6.0).map_err(e)?;

    let marker = |p: &Project, owner, start: Time, duration: i64, name: &str, colour| {
        let spec = MarkerSpec {
            duration: at(duration)?,
            name: name.to_owned(),
            ..MarkerSpec::at(start, colour)
        };
        p.add_marker(owner, spec).map_err(e)
    };
    let on_seq = MarkerOwner::Sequence(seq);
    let (p, opening) = marker(&p, on_seq, Time::ZERO, 0, "Opening", ColourLabel::Blue)?;
    let (p, _) = marker(
        &p,
        on_seq,
        at(40)?,
        10,
        "Cross dissolve",
        ColourLabel::Amber,
    )?;
    let (p, dip_marker) = marker(&p, on_seq, at(129)?, 12, "Dip to black", ColourLabel::Red)?;
    let (p, slow) = marker(&p, on_seq, at(135)?, 0, "Slow motion", ColourLabel::Green)?;
    let (p, _) = marker(
        &p,
        MarkerOwner::Clip(pip),
        seconds(1)?,
        0,
        "Scene cut",
        ColourLabel::Violet,
    )?;
    let mut p = p;
    for id in [opening, slow] {
        let (_, m) = p.find_marker(id).ok_or("marker")?;
        let spec = MarkerSpec {
            chapter: true,
            ..m.spec()
        };
        p = p.update_marker(id, spec).map_err(e)?;
    }
    let (_, m) = p.find_marker(dip_marker).ok_or("marker")?;
    let spec = MarkerSpec {
        todo: TodoState::Open,
        note: "Check the black frame".to_owned(),
        ..m.spec()
    };
    p.update_marker(dip_marker, spec).map_err(e)
}

/// The repository root: the first folder up from the working directory that holds
/// `tests/fixtures/generate.sh`, else the one this binary was built from.
pub fn repo_root() -> PathBuf {
    let marker = Path::new("tests/fixtures/generate.sh");
    if let Ok(cwd) = std::env::current_dir() {
        for dir in cwd.ancestors() {
            if dir.join(marker).is_file() {
                return dir.to_path_buf();
            }
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Writes [`build`] to `path` through the schema.
pub fn write(path: &Path) -> Result<(), String> {
    let bytes = project::schema::save(&build()?);
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// A project opened from disk with its media found.
pub struct Opened {
    pub project: Project,
    pub sequence: SequenceId,
}

/// Loads `path`, points every relative media path at the file beside the project, and
/// refuses when any media file is missing.
pub fn open(path: &Path) -> Result<Opened, String> {
    let bytes = std::fs::read(path).map_err(|e| {
        format!(
            "{}: {e}; regenerate it with `cargo run --bin smoke -- --write-reference`",
            path.display()
        )
    })?;
    let project = project::schema::load(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut relinks = Vec::new();
    let mut missing = Vec::new();
    for media in project.media().iter() {
        let file = Path::new(&media.path);
        let resolved = if file.is_relative() {
            dir.join(file)
        } else {
            file.to_path_buf()
        };
        if !resolved.exists() {
            missing.push(resolved.display().to_string());
        }
        relinks.push(Relink {
            media: media.id,
            path: resolved.to_string_lossy().into_owned(),
            content_hash: media.content_hash.clone(),
            duration: None,
        });
    }
    if !missing.is_empty() {
        return Err(format!(
            "the reference project's media is missing:\n  {}\ngenerate it with \
             `tests/fixtures/generate.sh` (or `scripts/worktree-setup.sh` in a worktree)",
            missing.join("\n  ")
        ));
    }
    let project = project.relink_media(&relinks).map_err(|e| e.to_string())?;
    let sequence = project
        .sequences()
        .front()
        .map(|s| s.id)
        .ok_or_else(|| format!("{} has no sequence", path.display()))?;
    Ok(Opened { project, sequence })
}
