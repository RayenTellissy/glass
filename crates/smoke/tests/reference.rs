//! The committed reference project (`tests/fixtures/reference.proj`): it loads at the
//! current schema, it is what `smoke --write-reference` writes today, and it holds the
//! sequence `smoke` plays and exports. A schema bump that changes the saved bytes fails
//! here with the command that regenerates the file.

use std::path::PathBuf;
use std::process::Command;

use engine::project::{self, Project, Rational, Time, TrackKind, Transform};
use engine::timeline::TimelinePlan;

const REGENERATE: &str = "regenerate it with `cargo run --bin smoke -- --write-reference` \
                          and commit tests/fixtures/reference.proj";

fn reference_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/reference.proj")
}

fn load() -> Project {
    let bytes = std::fs::read(reference_path()).expect("read tests/fixtures/reference.proj");
    project::schema::load(&bytes)
        .unwrap_or_else(|e| panic!("reference.proj does not load ({e}); {REGENERATE}"))
}

fn at(frames: i64) -> Time {
    Time::from_frames(frames, Rational::new(30, 1)).expect("time")
}

#[test]
fn the_committed_file_is_what_the_generator_writes() {
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("reference.proj");
    let status = Command::new(env!("CARGO_BIN_EXE_smoke"))
        .arg("--write-reference")
        .arg(&out)
        .status()
        .expect("run smoke --write-reference");
    assert!(
        status.success(),
        "smoke --write-reference exited with {status}"
    );
    let written = std::fs::read(&out).expect("read the written project");
    let committed = std::fs::read(reference_path()).expect("read the committed project");
    assert!(
        written == committed,
        "tests/fixtures/reference.proj differs from the generator's output; {REGENERATE}"
    );
}

#[test]
fn the_sequence_has_the_tracks_clips_and_transitions_smoke_plays() {
    let project = load();
    for media in project.media().iter() {
        assert!(
            media.path.starts_with("generated/"),
            "media path {} is not relative to the project file",
            media.path
        );
    }
    let [seq] = project.browser_sequences().collect::<Vec<_>>()[..] else {
        panic!("one sequence in the browser expected");
    };
    assert_eq!(project.sequences().len(), 2, "and one compound clip's");
    assert_eq!((seq.resolution.width, seq.resolution.height), (1920, 1080));
    assert_eq!(seq.frame_rate, Rational::new(30, 1));
    let tracks: Vec<_> = seq
        .tracks
        .iter()
        .map(|t| (t.kind, t.name.as_str(), t.items.len()))
        .collect();
    assert_eq!(
        tracks,
        [
            (TrackKind::Video, "V2", 2),
            (TrackKind::Video, "V1", 4),
            (TrackKind::Audio, "A1", 1),
        ]
    );
    let end = seq
        .tracks
        .iter()
        .flat_map(|t| t.items.iter())
        .map(|i| i.range.end().expect("end"))
        .max()
        .expect("items");
    assert_eq!(end, at(300), "the sequence lasts 10 s");

    let clips: Vec<_> = project
        .sequences()
        .iter()
        .flat_map(|s| s.tracks.iter())
        .flat_map(|t| t.items.iter())
        .map(|i| i.clip().expect("only clips"))
        .collect();
    assert!(
        clips.iter().any(|c| !c.keyframes.is_empty()),
        "a clip is keyframed"
    );
    assert!(
        clips
            .iter()
            .any(|c| c.speed.speed().expect("speed") == Rational::new(1, 2)),
        "a clip plays at half speed"
    );
    assert!(
        clips
            .iter()
            .any(|c| c.opacity < 1.0 && c.transform != Transform::IDENTITY),
        "a clip is moved and translucent"
    );
    let audio = &seq.tracks[2].items[0];
    assert_eq!(audio.clip().expect("clip").gain_db, -6.0);

    let mut kinds: Vec<_> = seq
        .tracks
        .iter()
        .flat_map(|t| t.transitions.iter())
        .map(|t| t.kind.as_str())
        .collect();
    kinds.sort_unstable();
    assert_eq!(kinds, ["cross-dissolve", "dip-to-black"]);
    assert_eq!(seq.tracks[1].transitions.len(), 2, "both transitions on V1");
    assert!(seq.markers.len() >= 3, "sequence markers");

    // It compiles for playback: six clips (one inside the compound), the transitions
    // between V1's clips.
    let plan = TimelinePlan::build(&project, seq.id).expect("plan");
    assert_eq!(plan.canvas(), (1920, 1080));
    assert_eq!(plan.end(), 300);
    assert_eq!(plan.segments().segments().len(), 6);
    let [compound] = plan.nested_looks() else {
        panic!("one compound clip expected");
    };
    assert!(compound.plan().is_animated(0), "its clip fades in");
    assert_eq!(plan.nested_clips(), 0);
    assert_eq!(plan.transitions().len(), 2);
    assert_eq!((plan.missing_media(), plan.skipped_effects()), (0, 0));
}
