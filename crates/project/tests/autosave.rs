//! Autosave diffs, the crash-safe log and recovery.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use project::autosave::{self, AutosaveWriter, SNAPSHOT_EVERY, Tick};
use project::diff::{Diff, flatten, rebuild};
use project::history::{EditLabel, History};
use project::{ClipSpec, MediaId, Project, Rational, Time, TimeRange, TrackId, TrackKind};

const FPS: Rational = Rational::new(24, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-autosave-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

struct Setup {
    project: Project,
    media: MediaId,
    v1: TrackId,
    a1: TrackId,
}

fn setup() -> Setup {
    let p = Project::new("uid-autosave", "Harbour film");
    let (p, media) = p.add_media("abcd", "/footage/harbour.mov").expect("media");
    let (p, seq) = p.add_sequence("Main sequence", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    Setup {
        project: p,
        media,
        v1,
        a1,
    }
}

fn clip(media: MediaId, start: i64, len: i64) -> ClipSpec {
    ClipSpec {
        media,
        range: TimeRange::new(frames(start), frames(len)),
        source_start: Time::ZERO,
    }
}

/// A project of `tracks` video tracks with `per_track` clips each.
fn big_project(tracks: usize, per_track: usize) -> (Project, Vec<TrackId>) {
    let p = Project::new("uid-big", "Feature cut");
    let (p, media) = p.add_media("beef", "/footage/a.mov").expect("media");
    let (mut p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let mut ids = Vec::new();
    for t in 0..tracks {
        let (next, track) = p
            .add_track(seq, TrackKind::Video, format!("V{}", t + 1), None)
            .expect("track");
        p = next;
        ids.push(track);
        for c in 0..per_track as i64 {
            p = p
                .insert_clip(track, clip(media, c * 30, 24))
                .expect("clip")
                .0;
        }
    }
    (p, ids)
}

/// A run of edits touching every kind of node; returns every root along the way.
fn edit_run(s: &Setup) -> Vec<Project> {
    let mut roots = vec![s.project.clone()];
    let mut p = s.project.clone();
    let mut ids = Vec::new();
    for i in 0..6 {
        let (next, id) = p
            .insert_clip(s.v1, clip(s.media, i * 30, 20))
            .expect("clip");
        p = next;
        ids.push(id);
        roots.push(p.clone());
    }
    let (next, a) = p.insert_clip(s.a1, clip(s.media, 0, 48)).expect("audio");
    roots.push(next.clone());
    p = next.move_item(ids[2], s.v1, frames(65)).expect("move");
    roots.push(p.clone());
    p = p
        .trim_item(ids[3], TimeRange::new(frames(92), frames(10)))
        .expect("trim");
    roots.push(p.clone());
    p = p.remove_item(ids[0]).expect("remove");
    roots.push(p.clone());
    p = p.remove_item(a).expect("remove audio");
    roots.push(p.clone());
    let (next, _) = p.add_media("ef01", "/footage/b.mov").expect("media");
    p = next.remove_track(s.a1).expect("remove track");
    roots.push(p.clone());
    let seq = p.sequences()[0].id;
    let (next, v2) = p
        .add_track(seq, TrackKind::Video, "V2", Some(0))
        .expect("v2");
    let (next, _) = next.insert_clip(v2, clip(s.media, 5, 5)).expect("clip");
    roots.push(next);
    roots
}

fn log_file(dir: &Path) -> PathBuf {
    let mut logs: Vec<PathBuf> = fs::read_dir(dir)
        .expect("dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    logs.sort();
    logs.pop().expect("log")
}

#[test]
fn diffs_rebuild_every_next_root() {
    let s = setup();
    let roots = edit_run(&s);
    for pair in roots.windows(2) {
        let (old, new) = (flatten(&pair[0]), flatten(&pair[1]));
        let diff = Diff::between(&old, &new);
        assert!(!diff.is_empty());
        let json = serde_json::to_string(&diff).expect("json");
        let diff: Diff = serde_json::from_str(&json).expect("parse");
        let mut applied = old.clone();
        diff.apply(&mut applied);
        assert_eq!(rebuild(&applied).expect("rebuild"), pair[1]);
    }
    // Across the whole run at once, too.
    let first = flatten(&roots[0]);
    let mut applied = first.clone();
    Diff::between(&first, &flatten(roots.last().expect("last"))).apply(&mut applied);
    assert_eq!(
        rebuild(&applied).expect("rebuild"),
        *roots.last().expect("last")
    );
}

#[test]
fn a_move_diff_holds_only_the_moved_item() {
    let s = setup();
    let (p, id) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 10))
        .expect("clip");
    let (p, _) = p.insert_clip(s.v1, clip(s.media, 20, 10)).expect("clip");
    let moved = p.move_item(id, s.v1, frames(5)).expect("move");
    let diff = Diff::between(&flatten(&p), &flatten(&moved));
    assert_eq!(diff.removals, vec![]);
    assert_eq!(diff.upserts.len(), 1);
    assert_eq!(diff.upserts[0].id(), id.node());
    assert!(Diff::between(&flatten(&p), &flatten(&p)).is_empty());
}

#[test]
fn a_diff_is_a_small_fraction_of_a_full_snapshot() {
    let (p, tracks) = big_project(4, 250);
    let item = p.find_track(tracks[1]).expect("track").1.items[100].id;
    let moved = p
        .move_item(item, tracks[1], frames(3001 * 30))
        .expect("move");
    let dir = temp_dir("size");
    let mut writer = AutosaveWriter::open(&dir, &p, None, 0, 0).expect("open");
    let snapshot = fs::metadata(dir.join("snapshot-00000001.json"))
        .expect("snapshot")
        .len() as usize;
    let Tick::Appended { bytes, upserts, .. } = writer.tick(&moved, 1, 10).expect("tick") else {
        panic!("expected a diff");
    };
    eprintln!(
        "1000 clips: snapshot {snapshot} bytes, one-move diff {bytes} bytes ({:.3}%)",
        bytes as f64 * 100.0 / snapshot as f64
    );
    assert_eq!(upserts, 1);
    assert!(
        bytes * 100 < snapshot,
        "diff {bytes} B vs snapshot {snapshot} B"
    );
    drop(writer);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn a_thousand_clip_project_autosaves_quickly() {
    let (mut p, tracks) = big_project(4, 250);
    let dir = temp_dir("timing");
    let started = Instant::now();
    let mut writer = AutosaveWriter::open(&dir, &p, None, 0, 0).expect("open");
    let open = started.elapsed();
    let mut ticks = Vec::new();
    for i in 0..20 {
        let track = tracks[i % tracks.len()];
        let item = p.find_track(track).expect("track").1.items[i * 10].id;
        p = p
            .trim_item(item, TimeRange::new(frames(i as i64 * 10 * 30), frames(12)))
            .expect("trim");
        let started = Instant::now();
        let tick = writer.tick(&p, 1, i as u64).expect("tick");
        ticks.push(started.elapsed());
        assert!(matches!(tick, Tick::Appended { .. }), "{tick:?}");
    }
    let started = Instant::now();
    assert_eq!(writer.tick(&p, 0, 99).expect("tick"), Tick::Unchanged);
    let unchanged = started.elapsed();
    ticks.sort();
    let median = ticks[ticks.len() / 2];
    let max = *ticks.last().expect("ticks");
    let started = Instant::now();
    let recovered = autosave::recover(&dir).expect("recover");
    let recover = started.elapsed();
    eprintln!(
        "1000 clips: open (full snapshot) {open:?}, tick with one change median {median:?} max {max:?}, \
         unchanged tick {unchanged:?}, recover 20 records {recover:?}"
    );
    assert_eq!(recovered.project, p);
    // Debug build on a shared machine; the tick runs on `glass-io`, never the UI thread.
    assert!(max.as_millis() < 250, "tick took {max:?}");
    drop(writer);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn a_crash_leaves_a_recoverable_log() {
    let s = setup();
    let dir = temp_dir("crash");
    let roots = edit_run(&s);
    let mut writer = AutosaveWriter::open(&dir, &roots[0], Some(1_000), 0, 1_000).expect("open");
    assert_eq!(
        autosave::find_recovery(&dir).expect("find"),
        None,
        "nothing changed yet"
    );
    for (i, root) in roots.iter().enumerate().skip(1) {
        let tick = writer
            .tick(root, 2, 1_000 + i as u64 * 10_000)
            .expect("tick");
        assert!(matches!(tick, Tick::Appended { .. }), "{tick:?}");
    }
    assert_eq!(
        writer
            .tick(roots.last().expect("last"), 0, 999_000)
            .expect("tick"),
        Tick::Unchanged
    );
    drop(writer); // the crash: no close, so the unclean-exit marker stays

    let info = autosave::find_recovery(&dir)
        .expect("find")
        .expect("recoverable");
    assert_eq!(info.project_name, "Harbour film");
    assert_eq!(info.edits_since_save, 2 * (roots.len() as u64 - 1));
    assert_eq!(info.saved_at_ms, Some(1_000));
    assert_eq!(
        info.autosaved_at_ms,
        1_000 + (roots.len() as u64 - 1) * 10_000
    );
    assert_eq!(info.alive_ms, 999_000);
    let recovered = autosave::recover(&dir).expect("recover");
    assert_eq!(&recovered.project, roots.last().expect("last"));
    assert_eq!(recovered.dropped_records, 0);
    assert!(!recovered.fell_back);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn a_torn_log_tail_is_skipped() {
    let s = setup();
    let dir = temp_dir("torn");
    let roots = edit_run(&s);
    let mut writer = AutosaveWriter::open(&dir, &roots[0], None, 0, 0).expect("open");
    for (i, root) in roots.iter().enumerate().skip(1) {
        writer.tick(root, 1, i as u64).expect("tick");
    }
    drop(writer);
    // The crash hit mid-write: the last line lost its end.
    let log = log_file(&dir);
    let text = fs::read_to_string(&log).expect("log");
    let lines: Vec<&str> = text.lines().collect();
    let keep = lines[..lines.len() - 1].join("\n") + "\n";
    let torn = &lines[lines.len() - 1][..20];
    fs::write(&log, format!("{keep}{torn}")).expect("tear");
    let recovered = autosave::recover(&dir).expect("recover");
    assert_eq!(&recovered.project, &roots[roots.len() - 2]);
    assert_eq!(recovered.dropped_records, 1);
    assert_eq!(recovered.info.edits_since_save, roots.len() as u64 - 2);

    // A log cut inside its header line: the snapshot alone stands.
    fs::write(&log, "{\"autosave_for").expect("tear header");
    let recovered = autosave::recover(&dir).expect("recover");
    assert_eq!(recovered.project, roots[0]);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn every_file_carries_the_autosave_format_version() {
    let s = setup();
    let dir = temp_dir("format");
    let (p, _) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 5))
        .expect("clip");
    let mut writer = AutosaveWriter::open(&dir, &s.project, None, 0, 0).expect("open");
    writer.tick(&p, 1, 10).expect("tick");
    drop(writer);
    let version = autosave::FORMAT_VERSION;
    let json = |name: &str| -> serde_json::Value {
        serde_json::from_slice(&fs::read(dir.join(name)).expect("read")).expect("json")
    };
    assert_eq!(json("session.json")["autosave_format"], version);
    assert_eq!(json("snapshot-00000001.json")["autosave_format"], version);
    let log = fs::read_to_string(dir.join("log-00000001.jsonl")).expect("log");
    let header: serde_json::Value =
        serde_json::from_str(log.lines().next().expect("header")).expect("header");
    assert_eq!(header["autosave_format"], version);
    assert_eq!(header["generation"], 1);

    // An autosave from another format version is not read.
    let snapshot = dir.join("snapshot-00000001.json");
    let text = fs::read_to_string(&snapshot).expect("snapshot");
    let other = text.replacen(
        &format!("\"autosave_format\":{version}"),
        &format!("\"autosave_format\":{}", version + 1),
        1,
    );
    fs::write(&snapshot, other).expect("write");
    assert!(matches!(
        autosave::recover(&dir),
        Err(autosave::AutosaveError::Format(v)) if v == version + 1
    ));
    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Autosaves written before project schema v2 (format 1, `media` as a bare list) are
/// refused by their format version, not as damaged JSON, at recovery and at the launch
/// scan.
#[test]
fn a_format_1_autosave_from_before_schema_v2_is_refused_by_version() {
    let s = setup();
    let dir = temp_dir("format-1");
    let (p, _) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 5))
        .expect("clip");
    let mut writer = AutosaveWriter::open(&dir, &s.project, None, 0, 0).expect("open");
    writer.tick(&p, 1, 10).expect("tick");
    // Dropped without `close`: the session marker stays, as after a crash.
    drop(writer);
    let snapshot = dir.join("snapshot-00000001.json");
    let mut old: serde_json::Value =
        serde_json::from_slice(&fs::read(&snapshot).expect("snapshot")).expect("json");
    old["autosave_format"] = 1.into();
    let refs = old["project"]["media"]["refs"].take();
    old["project"]["media"] = refs;
    assert!(
        serde_json::from_value::<Project>(old["project"].clone()).is_err(),
        "a format 1 project no longer parses"
    );
    fs::write(&snapshot, serde_json::to_vec(&old).expect("encode")).expect("write");

    assert_eq!(autosave::FORMAT_VERSION, 5);
    assert!(matches!(
        autosave::recover(&dir),
        Err(autosave::AutosaveError::Format(1))
    ));
    assert!(matches!(
        autosave::find_recovery(&dir),
        Err(autosave::AutosaveError::Format(1))
    ));
    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Autosaves written before project schema v3 (format 2, no `markers`) are refused by
/// their format version too.
#[test]
fn a_format_2_autosave_from_before_schema_v3_is_refused_by_version() {
    let s = setup();
    let dir = temp_dir("format-2");
    let mut writer = AutosaveWriter::open(&dir, &s.project, None, 0, 0).expect("open");
    writer.tick(&s.project, 1, 10).expect("tick");
    drop(writer);
    let snapshot = dir.join("snapshot-00000001.json");
    let mut old: serde_json::Value =
        serde_json::from_slice(&fs::read(&snapshot).expect("snapshot")).expect("json");
    old["autosave_format"] = 2.into();
    for seq in old["project"]["sequences"]
        .as_array_mut()
        .expect("sequences")
    {
        seq.as_object_mut().expect("sequence").remove("markers");
    }
    assert!(
        serde_json::from_value::<Project>(old["project"].clone()).is_err(),
        "a format 2 project no longer parses"
    );
    fs::write(&snapshot, serde_json::to_vec(&old).expect("encode")).expect("write");
    assert!(matches!(
        autosave::recover(&dir),
        Err(autosave::AutosaveError::Format(2))
    ));
    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Autosaves written before project schema v4 (format 3, clips without properties) are
/// refused by their format version too.
#[test]
fn a_format_3_autosave_from_before_schema_v4_is_refused_by_version() {
    let s = setup();
    let dir = temp_dir("format-3");
    let (p, _) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 5))
        .expect("clip");
    let mut writer = AutosaveWriter::open(&dir, &p, None, 0, 0).expect("open");
    writer.tick(&p, 1, 10).expect("tick");
    drop(writer);
    let snapshot = dir.join("snapshot-00000001.json");
    let mut old: serde_json::Value =
        serde_json::from_slice(&fs::read(&snapshot).expect("snapshot")).expect("json");
    old["autosave_format"] = 3.into();
    let item = old["project"]["sequences"][0]["tracks"][0]["items"][0]
        .as_object_mut()
        .expect("clip");
    for key in [
        "speed",
        "enabled",
        "gain_db",
        "transform",
        "crop",
        "opacity",
        "blend",
        "effects",
    ] {
        item.remove(key).expect("a v4 clip property");
    }
    assert!(
        serde_json::from_value::<Project>(old["project"].clone()).is_err(),
        "a format 3 project no longer parses"
    );
    fs::write(&snapshot, serde_json::to_vec(&old).expect("encode")).expect("write");
    assert!(matches!(
        autosave::recover(&dir),
        Err(autosave::AutosaveError::Format(3))
    ));
    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Autosaves written before project schema v5 (format 4, tracks without transitions) are
/// refused by their format version too.
#[test]
fn a_format_4_autosave_from_before_schema_v5_is_refused_by_version() {
    let s = setup();
    let dir = temp_dir("format-4");
    let (p, _) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 5))
        .expect("clip");
    let mut writer = AutosaveWriter::open(&dir, &p, None, 0, 0).expect("open");
    writer.tick(&p, 1, 10).expect("tick");
    drop(writer);
    let snapshot = dir.join("snapshot-00000001.json");
    let mut old: serde_json::Value =
        serde_json::from_slice(&fs::read(&snapshot).expect("snapshot")).expect("json");
    old["autosave_format"] = 4.into();
    for seq in old["project"]["sequences"]
        .as_array_mut()
        .expect("sequences")
    {
        for track in seq["tracks"].as_array_mut().expect("tracks") {
            let track = track.as_object_mut().expect("track");
            track.remove("transitions").expect("a v5 track list");
        }
    }
    assert!(
        serde_json::from_value::<Project>(old["project"].clone()).is_err(),
        "a format 4 project no longer parses"
    );
    fs::write(&snapshot, serde_json::to_vec(&old).expect("encode")).expect("write");
    assert!(matches!(
        autosave::recover(&dir),
        Err(autosave::AutosaveError::Format(4))
    ));
    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Clip properties and effect stacks travel inside their item's record and recover.
#[test]
fn clip_property_edits_recover_from_the_log() {
    let s = setup();
    let dir = temp_dir("clip-props");
    let (p, item) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 50))
        .expect("clip");
    let mut writer = AutosaveWriter::open(&dir, &p, None, 0, 0).expect("open");
    let lut = project::params::effects::lut();
    let (p, effect) = p.add_effect(item, &lut, None).expect("effect");
    let p = p
        .set_effect_param(
            effect,
            &lut,
            "intensity",
            project::params::ParamValue::Float(0.25),
        )
        .expect("param");
    let p = p
        .set_clip_speed(item, Rational::new(2, 1), project::SpeedKeep::Source)
        .expect("speed");
    let p = p.set_clip_opacity(item, 0.5).expect("opacity");
    let tick = writer.tick(&p, 4, 10).expect("tick");
    assert!(
        matches!(
            tick,
            autosave::Tick::Appended {
                upserts: 2,
                removals: 0,
                ..
            }
        ),
        "the root (next id) and the one item: {tick:?}"
    );
    let p = p.remove_effect(effect).expect("remove");
    writer.tick(&p, 1, 20).expect("tick");
    drop(writer);
    let recovered = autosave::recover(&dir).expect("recovers");
    assert!(recovered.project.content_eq(&p));
    assert_eq!(recovered.info.edits_since_save, 5);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn generations_roll_and_fall_back() {
    let s = setup();
    let dir = temp_dir("roll");
    let mut p = s.project.clone();
    let mut writer = AutosaveWriter::open(&dir, &p, None, 0, 0).expect("open");
    let mut snapshots = 0;
    let ticks = SNAPSHOT_EVERY * 2 + 5;
    for i in 0..ticks as i64 {
        p = p
            .insert_clip(s.v1, clip(s.media, i * 10, 5))
            .expect("clip")
            .0;
        if matches!(
            writer.tick(&p, 1, i as u64).expect("tick"),
            Tick::Snapshot { .. }
        ) {
            snapshots += 1;
        }
    }
    assert_eq!(snapshots, 2);
    let mut names: Vec<String> = fs::read_dir(&dir)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "log-00000002.jsonl",
            "log-00000003.jsonl",
            "session.json",
            "snapshot-00000002.json",
            "snapshot-00000003.json"
        ]
    );
    let recovered = autosave::recover(&dir).expect("recover");
    assert_eq!(recovered.project, p);
    assert_eq!(recovered.info.edits_since_save, ticks);

    // A damaged newest snapshot falls back to the generation before.
    fs::write(dir.join("snapshot-00000003.json"), b"{\"autosave_format\":").expect("damage");
    let recovered = autosave::recover(&dir).expect("recover");
    assert!(recovered.fell_back);
    assert_eq!(
        recovered
            .project
            .find_track(s.v1)
            .expect("v1")
            .1
            .items
            .len(),
        SNAPSHOT_EVERY as usize * 2 + 1
    );
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn clean_quit_and_save_leave_nothing_to_recover() {
    let s = setup();
    let dir = temp_dir("clean");
    let (p, _) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 5))
        .expect("clip");
    let mut writer = AutosaveWriter::open(&dir, &s.project, None, 0, 0).expect("open");
    writer.tick(&p, 1, 10).expect("tick");
    assert!(autosave::find_recovery(&dir).expect("find").is_some());
    writer.mark_saved(&p, 20).expect("saved");
    assert_eq!(autosave::find_recovery(&dir).expect("find"), None);
    writer.close().expect("close");
    assert!(!dir.exists());
    assert_eq!(autosave::find_recovery(&dir).expect("find"), None);
}

#[test]
fn a_restored_project_keeps_its_unsaved_edits() {
    let s = setup();
    let dir = temp_dir("restored");
    let (p, _) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 5))
        .expect("clip");
    // Restored with 7 unsaved edits, then the app crashes again before any change.
    drop(AutosaveWriter::open(&dir, &p, Some(5), 7, 100).expect("open"));
    let info = autosave::find_recovery(&dir)
        .expect("find")
        .expect("still recoverable");
    assert_eq!(info.edits_since_save, 7);
    assert_eq!(autosave::recover(&dir).expect("recover").project, p);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn garbage_after_a_complete_line_is_dropped() {
    let s = setup();
    let dir = temp_dir("append");
    let (p, _) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 5))
        .expect("clip");
    let mut writer = AutosaveWriter::open(&dir, &s.project, None, 0, 0).expect("open");
    writer.tick(&p, 1, 10).expect("tick");
    OpenOptions::new()
        .append(true)
        .open(dir.join("log-00000001.jsonl"))
        .and_then(|mut f| f.write_all(b"not json\n"))
        .expect("append");
    let recovered = autosave::recover(&dir).expect("recover");
    assert_eq!(recovered.project, p);
    assert_eq!(recovered.dropped_records, 1);
    drop(writer);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn pending_lists_unclean_sessions_with_edits() {
    let s = setup();
    let cache = temp_dir("pending");
    let (p, _) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 5))
        .expect("clip");
    let crashed = autosave::autosave_dir(&cache, "crashed");
    let mut writer = AutosaveWriter::open(&crashed, &s.project, None, 0, 0).expect("open");
    writer.tick(&p, 1, 10).expect("tick");
    drop(writer);
    let idle = autosave::autosave_dir(&cache, "idle");
    drop(AutosaveWriter::open(&idle, &s.project, None, 0, 0).expect("open"));
    let quit = autosave::autosave_dir(&cache, "quit");
    AutosaveWriter::open(&quit, &s.project, None, 0, 0)
        .expect("open")
        .close()
        .expect("close");
    let broken = autosave::autosave_dir(&cache, "broken");
    fs::create_dir_all(&broken).expect("dir");
    fs::write(broken.join("session.json"), b"{}").expect("marker");

    let found = autosave::pending(&cache).expect("pending");
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].0, broken);
    assert!(found[0].1.is_err());
    assert_eq!(found[1].0, crashed);
    let info = found[1].1.as_ref().expect("recoverable");
    assert_eq!(info.edits_since_save, 1);
    assert!(
        autosave::pending(&temp_dir("pending-none"))
            .expect("pending")
            .is_empty()
    );
    autosave::discard(&crashed).expect("discard");
    autosave::discard(&crashed).expect("discard twice");
    fs::remove_dir_all(&cache).expect("cleanup");
}

#[test]
fn old_autosaves_are_pruned_after_seven_days() {
    let s = setup();
    let cache = temp_dir("prune");
    let old = autosave::autosave_dir(&cache, "old");
    let recent = autosave::autosave_dir(&cache, "recent");
    drop(AutosaveWriter::open(&old, &s.project, None, 0, 0).expect("open"));
    drop(AutosaveWriter::open(&recent, &s.project, None, 0, autosave::KEEP_MS).expect("open"));
    assert_eq!(
        autosave::prune(&cache, autosave::KEEP_MS + 1).expect("prune"),
        1
    );
    assert!(!old.exists());
    assert!(recent.exists());
    assert_eq!(
        autosave::prune(&temp_dir("prune-none"), 0).expect("prune"),
        0
    );
    fs::remove_dir_all(&cache).expect("cleanup");
}

#[test]
fn history_counts_changes_for_autosave() {
    let s = setup();
    let mut history = History::new(s.project.clone());
    assert_eq!(history.changes(), 0);
    let (p1, id) = s
        .project
        .insert_clip(s.v1, clip(s.media, 0, 5))
        .expect("clip");
    history.commit(EditLabel::Insert, p1.clone());
    history.commit(EditLabel::Insert, p1.clone()); // no-op
    assert_eq!(history.changes(), 1);
    history.begin_gesture(EditLabel::Move);
    for x in 1..5 {
        history.update(p1.move_item(id, s.v1, frames(x)).expect("move"));
    }
    history.end_gesture();
    assert_eq!(history.changes(), 2, "a gesture counts once");
    history.undo();
    history.redo();
    assert_eq!(history.changes(), 4);
    history.begin_gesture(EditLabel::Move);
    history.update(p1.move_item(id, s.v1, frames(9)).expect("move"));
    history.cancel_gesture();
    assert_eq!(history.changes(), 6);
}
