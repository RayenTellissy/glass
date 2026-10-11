//! `Autosave::saved_as` against its contract: the new folder is opened before the old one
//! is removed, so a move that cannot start keeps the old autosave; the same folder,
//! however it is spelled, is a plain save.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use engine::autosave::{self, Autosave, AutosaveConfig, AutosaveEvent};
use project::autosave as files;
use project::{ClipSpec, Project, Rational, Time, TimeRange, TrackKind};

const WAIT: Duration = Duration::from_secs(20);

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "glass-saved-as-contract-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A project and the same project with one more clip.
fn project_and_edit() -> (Project, Project) {
    let fps = Rational::new(25, 1);
    let p = Project::new("uid-saved-as", "Harbour film");
    let (p, media) = p.add_media("abcd", "/footage/harbour.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", fps, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let spec = ClipSpec {
        media,
        range: TimeRange::new(Time::ZERO, Time::seconds(1)),
        source_start: Time::ZERO,
    };
    let (edited, _) = p.insert_clip(v1, spec).expect("clip");
    (p, edited)
}

/// Ticks only on `flush`, so nothing depends on timing.
fn start(dir: &Path, root: &Project) -> (Autosave, Receiver<AutosaveEvent>) {
    let config = AutosaveConfig {
        interval: Duration::from_secs(3600),
        ..AutosaveConfig::new(dir)
    };
    Autosave::start(config, root.clone(), 0).expect("start")
}

/// The next event other than `Started`.
fn next_event(events: &Receiver<AutosaveEvent>) -> AutosaveEvent {
    loop {
        match events.recv_timeout(WAIT).expect("autosave event") {
            AutosaveEvent::Started { .. } => {}
            event => return event,
        }
    }
}

#[test]
fn a_move_that_cannot_open_the_new_folder_keeps_the_old_autosave() {
    let root = temp_root("blocked");
    let user_cache = root.join("user-cache");
    let (p, edited) = project_and_edit();
    let old = autosave::autosave_location(&p, None, &user_cache);
    let (handle, events) = start(&old, &p);
    handle.submit(&edited, 1);
    handle.flush();
    assert!(matches!(next_event(&events), AutosaveEvent::Written { .. }));

    // The project's cache folder sits under a plain file, so it cannot be created.
    std::fs::create_dir_all(&root).expect("root");
    let blocker = root.join("films.glass-cache");
    std::fs::write(&blocker, b"not a folder").expect("blocker");
    let new = blocker.join("autosave").join(p.uid());
    handle.saved_as(&edited, 1, new.clone());
    assert!(
        matches!(next_event(&events), AutosaveEvent::Failed(_)),
        "the move is reported as failed"
    );
    handle.abandon();

    assert!(!new.exists());
    let info = files::find_recovery(&old)
        .expect("find")
        .expect("the old autosave is still recoverable");
    assert_eq!(info.edits_since_save, 1);
    assert!(
        files::recover(&old)
            .expect("recover")
            .project
            .content_eq(&edited)
    );
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn saving_as_into_the_same_folder_spelled_differently_is_a_plain_save() {
    let root = temp_root("same");
    let dir = root.join("films/.glass-cache/autosave/uid-saved-as");
    let (p, edited) = project_and_edit();
    let (handle, events) = start(&dir, &p);
    handle.submit(&edited, 1);
    handle.flush();
    assert!(matches!(next_event(&events), AutosaveEvent::Written { .. }));

    // `<dir>/.` names the same folder.
    let same = dir.join(".");
    assert_eq!(same, dir);
    handle.saved_as(&edited, 1, same);
    assert_eq!(next_event(&events), AutosaveEvent::Saved);
    assert!(dir.join("session.json").exists(), "the folder stays");
    assert_eq!(files::find_recovery(&dir).expect("find"), None);

    // Edits after the save count from it and are recovered from the same folder.
    let (later, _) = edited
        .add_sequence("Second", Rational::new(25, 1), 48_000)
        .expect("seq");
    handle.submit(&later, 2);
    handle.flush();
    assert!(matches!(next_event(&events), AutosaveEvent::Written { .. }));
    handle.abandon();
    let info = files::find_recovery(&dir)
        .expect("find")
        .expect("recoverable");
    assert_eq!(info.edits_since_save, 1);
    assert!(info.saved_at_ms.is_some());
    assert!(
        files::recover(&dir)
            .expect("recover")
            .project
            .content_eq(&later)
    );
    std::fs::remove_dir_all(&root).expect("cleanup");
}
