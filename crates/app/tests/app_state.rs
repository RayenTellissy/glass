//! The app state: edits, undo and redo keep the playback plan (and a running preview's
//! plan) in step with the project; the keymap file round-trips and its keys run
//! commands; the reference project opens with its relative media resolved and plays a
//! second headless.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use app::preview::{AudioOut, Gpu};
use app::state::{AppState, Engines, Keys, Outcome, ShellRequest, new_project};
use engine::project::{
    ClipSpec, EditLabel, History, Project, Rational, SequenceId, Time, TimeRange, TrackId,
};
use engine::timeline::DirtyFrames;
use ui::panels::timeline::view::{Extent, sequence_duration};
use ui::shortcuts::{Chord, Keymap, PresetId};

fn engines(gpu: Option<Gpu>) -> Engines {
    Engines {
        gpu,
        audio: AudioOut::Silent,
        autosave: None,
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-app-state-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A 30 fps project with one 2-second clip on V1 (its media need not exist: plans are
/// built from the project alone).
fn project_with_clip() -> (Project, SequenceId, TrackId) {
    let p = new_project("Edits");
    let seq = p.browser_sequences().next().expect("sequence").id;
    let v1 = p.sequence(seq).expect("seq").tracks[0].id;
    let (p, media) = p.add_media("hash", "/nowhere/clip.mov").expect("media");
    let (p, _) = p.insert_clip(v1, clip(media, 0, 60)).expect("first clip");
    (p, seq, v1)
}

fn clip(media: engine::project::MediaId, from: i64, to: i64) -> ClipSpec {
    let rate = Rational::new(30, 1);
    ClipSpec {
        media,
        range: TimeRange::between(
            Time::from_frames(from, rate).expect("time"),
            Time::from_frames(to, rate).expect("time"),
        )
        .expect("range"),
        source_start: Time::ZERO,
    }
}

fn require_gpu() -> bool {
    std::env::var("GLASS_REQUIRE_GPU").as_deref() == Ok("1")
}

#[test]
fn edits_undo_and_redo_rebuild_the_plan_with_their_dirty_frames() {
    let (p, _, v1) = project_with_clip();
    let media = p.media().iter().next().expect("media").id;
    let mut state = AppState::new(
        History::new(p),
        None,
        Keys::with(Keymap::default(), None),
        engines(None),
    );
    assert_eq!(state.plan().expect("plan").end(), 60);
    let builds = state.plan_builds();
    assert!(!state.is_edited());

    state
        .edit(EditLabel::Insert, |p| {
            p.insert_clip(v1, clip(media, 60, 90)).map(|(p, _)| p)
        })
        .expect("insert");
    assert_eq!(state.plan().expect("plan").end(), 90);
    assert_eq!(state.plan_builds(), builds + 1);
    assert_eq!(
        state.last_dirty(),
        Some(&DirtyFrames::from_ranges([60..90]))
    );
    assert!(state.is_edited());

    assert!(state.undo());
    assert_eq!(state.plan().expect("plan").end(), 60);
    assert_eq!(
        state.last_dirty(),
        Some(&DirtyFrames::from_ranges([60..90]))
    );
    assert!(state.redo());
    assert_eq!(state.plan().expect("plan").end(), 90);
    assert!(!state.redo());
    assert_eq!(state.plan_builds(), builds + 3);

    // A rename dirties no frame but still compiles the new tree.
    state.timeline_action(ui::panels::timeline::panel::TimelineAction::Header(
        ui::panels::timeline::track_header::TrackHeaderAction::Rename {
            track: v1,
            name: "Pictures".into(),
        },
    ));
    assert_eq!(state.history().undo_label(), Some(EditLabel::RenameTrack));
    assert_eq!(state.last_dirty(), Some(&DirtyFrames::none()));
}

#[test]
fn a_running_preview_gets_every_new_plan() {
    let Some(gpu) = Gpu::headless() else {
        assert!(
            !require_gpu(),
            "GLASS_REQUIRE_GPU=1 but no GPU with 16-bit textures"
        );
        eprintln!("SKIPPED a_running_preview_gets_every_new_plan: no GPU");
        return;
    };
    let (p, _, v1) = project_with_clip();
    let media = p.media().iter().next().expect("media").id;
    let mut state = AppState::new(
        History::new(p),
        None,
        Keys::with(Keymap::default(), None),
        engines(Some(gpu)),
    );
    let same = |state: &AppState| {
        Arc::ptr_eq(
            state.plan().expect("plan"),
            state.preview().expect("preview").plan(),
        )
    };
    assert!(same(&state));
    state
        .edit(EditLabel::Insert, |p| {
            p.insert_clip(v1, clip(media, 60, 120)).map(|(p, _)| p)
        })
        .expect("insert");
    assert!(same(&state));
    assert!(state.undo());
    assert!(same(&state));
    state.close();
}

/// One frame of `ctx` with `events` as input, running the state's key dispatch.
fn press(state: &mut AppState, ctx: &egui::Context, events: Vec<egui::Event>) -> Vec<ShellRequest> {
    let mut requests = Vec::new();
    let raw = egui::RawInput {
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| requests = state.dispatch_keys(ui.ctx()));
    out.textures_delta.clear();
    requests
}

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn command() -> egui::Modifiers {
    egui::Modifiers {
        mac_cmd: cfg!(target_os = "macos"),
        command: true,
        ctrl: !cfg!(target_os = "macos"),
        ..egui::Modifiers::NONE
    }
}

#[test]
fn the_keymap_file_round_trips_and_its_keys_run_commands() {
    let dir = temp("keymap");
    let file = dir.join("keymap.json");
    let keys = Keys::load(Some(file.clone()));
    assert_eq!(keys.keymap(), &Keymap::new(PresetId::Glass));
    assert!(!file.exists(), "loading never writes");

    let (p, _, v1) = project_with_clip();
    let mut state = AppState::new(History::new(p), None, keys, engines(None));
    let ctx = egui::Context::default();

    // Cmd+Z undoes through the Glass preset.
    state
        .edit(EditLabel::RenameTrack, |p| p.rename_track(v1, "Renamed"))
        .expect("rename");
    assert!(press(&mut state, &ctx, vec![key(egui::Key::Z, command())]).is_empty());
    assert!(!state.history().can_undo());

    // Rebind undo to F6, a free key: the file is written, the old chord stops working
    // and the new one runs.
    let mut keymap = state.keys.keymap().clone();
    keymap.set("edit.undo", vec![Chord::key(egui::Key::F6)]);
    keymap.set("edit.save", vec![Chord::key(egui::Key::F5)]);
    state.keys.set(keymap.clone()).expect("save keymap");
    let reloaded = Keys::load(Some(file.clone()));
    assert_eq!(reloaded.keymap(), &keymap);
    assert_eq!(Keymap::load(&file).expect("read").expect("file"), keymap);

    state
        .edit(EditLabel::RenameTrack, |p| p.rename_track(v1, "Again"))
        .expect("rename");
    press(&mut state, &ctx, vec![key(egui::Key::Z, command())]);
    assert!(state.history().can_undo(), "the old chord is gone");
    press(
        &mut state,
        &ctx,
        vec![key(egui::Key::F6, egui::Modifiers::NONE)],
    );
    assert!(!state.history().can_undo());
    // Commands the window owns come back to it.
    assert_eq!(
        press(
            &mut state,
            &ctx,
            vec![key(egui::Key::F5, egui::Modifiers::NONE)]
        ),
        [ShellRequest::Save]
    );
    assert_eq!(
        state.run_command("no.such_command", 0.0),
        Outcome::Unhandled
    );

    // An unreadable file falls back to the preset and is left alone.
    std::fs::write(&file, "{ not json").expect("write");
    assert_eq!(
        Keys::load(Some(file.clone())).keymap(),
        &Keymap::new(PresetId::Glass)
    );
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "{ not json");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn view_commands_zoom_the_timeline() {
    let (p, _, _) = project_with_clip();
    let mut state = AppState::new(
        History::new(p),
        None,
        Keys::with(Keymap::default(), None),
        engines(None),
    );
    // The view zooms once it knows its width; give it one as the panel does.
    let seq = state
        .sequence()
        .and_then(|s| state.project().sequence(s))
        .cloned()
        .expect("sequence");
    let timeline = state.timeline_mut().expect("timeline");
    timeline.view.set_extent(Extent {
        width: 1000.0,
        height: 200.0,
        duration: sequence_duration(&seq),
        content_height: 96.0,
    });
    let before = state.timeline().expect("timeline").view.target().frame_px;
    assert_eq!(state.run_command("view.zoom_in", 0.0), Outcome::Done);
    let after = state.timeline().expect("timeline").view.target().frame_px;
    assert!((after / before - 2.0).abs() < 1e-9, "{before} -> {after}");
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
}

#[test]
fn the_reference_project_opens_and_plays_a_second() {
    let file = repo_root().join("tests/fixtures/reference.proj");
    let opened = app::project_io::open(&file).expect("open the reference project");
    assert!(
        opened.missing.is_empty(),
        "generate the fixtures (scripts/worktree-setup.sh): {:?}",
        opened.missing
    );
    // Every media path was resolved against the file's folder.
    for media in opened.project.media().iter() {
        assert!(Path::new(&media.path).is_absolute(), "{}", media.path);
    }
    let Some(gpu) = Gpu::headless() else {
        assert!(
            !require_gpu(),
            "GLASS_REQUIRE_GPU=1 but no GPU with 16-bit textures"
        );
        eprintln!("SKIPPED playing the reference project: no GPU");
        return;
    };
    let mut state = AppState::new(
        History::new(opened.project),
        Some(file),
        Keys::with(Keymap::default(), None),
        engines(Some(gpu)),
    );
    let plan = state.plan().expect("plan").clone();
    assert_eq!(plan.missing_media(), 0);
    let second = plan.rate().to_f64().expect("rate").round() as i64;
    assert!(state.preview().expect("preview").silent());

    // The paused first frame arrives, then a second of playback.
    let deadline = Instant::now() + Duration::from_secs(30);
    while state.preview().expect("preview").shown().is_none() {
        assert!(Instant::now() < deadline, "no first frame");
        state.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    let started = Instant::now();
    state.play();
    while state.playhead() < second {
        assert!(
            Instant::now() < deadline,
            "playback stalled at {}",
            state.playhead()
        );
        state.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    let played = started.elapsed();
    state.pause();
    let stats = state.preview().expect("preview").stats();
    eprintln!(
        "reference project: {second} frames in {:.2} s, {} presented, {} late, {} dropped \
         (debug test build, M5 standing in for the M1, machine shared with other agents)",
        played.as_secs_f64(),
        stats.presented,
        stats.late,
        stats.dropped
    );
    assert!(stats.presented >= second as u64 / 2, "{stats:?}");
    // Paced by the clock: a second of timeline takes about a second.
    assert!(played >= Duration::from_millis(800), "{played:?}");
    state.close();
}
