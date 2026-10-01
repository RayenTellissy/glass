//! Offline media: a missing, unreadable or replaced file is offline with its reason, and a
//! project whose media is offline still loads, edits, undoes and saves unchanged.

use std::path::PathBuf;

use engine::media_mgmt::offline::{
    self, Availability, OfflineReason, Verify, check_project, offline_clips, slate_label,
};
use media::ContentHash;
use project::history::{EditLabel, History};
use project::{ClipSpec, MediaId, Project, Rational, Time, TimeRange, TrackId, TrackKind};

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-offline-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn clip(media: MediaId, start: i64, len: i64) -> ClipSpec {
    ClipSpec {
        media,
        range: TimeRange::new(frames(start), frames(len)),
        source_start: frames(12),
    }
}

/// Two sources: `present` exists on disk, `gone` does not. Each has clips on V1.
fn project(dir: &std::path::Path) -> (Project, MediaId, MediaId, TrackId) {
    let present = dir.join("A014_C007.mov");
    std::fs::write(&present, b"not really a movie, but bytes to hash").expect("write");
    let hash = ContentHash::of_file(&present).expect("hash").to_hex();
    let p = Project::new("uid-offline", "Harbour film");
    let (p, online) = p.add_media(hash, present.to_string_lossy()).expect("media");
    let (p, gone) = p
        .add_media("00ff", dir.join("A014_C011.mov").to_string_lossy())
        .expect("media");
    let (p, seq) = p.add_sequence("Main sequence", FPS, 48_000).expect("seq");
    let (mut p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    for (i, media) in [online, gone, gone, online].into_iter().enumerate() {
        p = p
            .insert_clip(v1, clip(media, i as i64 * 50, 40))
            .expect("clip")
            .0;
    }
    (p, online, gone, v1)
}

#[test]
fn a_missing_file_is_offline_with_its_reason() {
    let dir = temp_dir("missing");
    let (p, online, gone, _) = project(&dir);
    for verify in [Verify::Quick, Verify::Content] {
        let status = check_project(&p, verify);
        assert_eq!(status.get(online), &Availability::Online);
        assert_eq!(
            status.get(gone),
            &Availability::Offline(OfflineReason::Missing)
        );
        assert_eq!(status.offline_count(), 1);
    }
    let status = check_project(&p, Verify::Content);
    let reason = status.get(gone).reason().expect("offline");
    let media = p.media().get(gone).expect("ref");
    let message = reason.message(&media.path);
    assert!(message.contains(&media.path), "{message}");
    assert!(!message.contains('⟦'), "missing key: {message}");

    // The timeline sees exactly the clips of the missing file, with their ranges.
    let clips = offline_clips(&p, &status);
    assert_eq!(clips.len(), 2);
    assert!(clips.iter().all(|c| c.media == gone));
    assert_eq!(clips[0].range, TimeRange::new(frames(50), frames(40)));
    assert_eq!(clips[1].range, TimeRange::new(frames(100), frames(40)));

    // The slate says what and which file.
    let label = slate_label(media, None);
    assert_eq!(label.title, "Media offline");
    assert_eq!(label.file_name, "A014_C011.mov");
    assert_eq!(label.detail, "Edits are kept.");
    let label = slate_label(media, Some("Card A"));
    assert_eq!(label.detail, "Last seen on Card A. Edits are kept.");

    // The file comes back: online again.
    std::fs::write(&media.path, b"restored").expect("write");
    let status = check_project(&p, Verify::Quick);
    assert!(!status.is_offline(gone));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_replaced_or_unreadable_file_is_offline() {
    let dir = temp_dir("changed");
    let (p, online, _, _) = project(&dir);
    let media = p.media().get(online).expect("ref").clone();

    std::fs::write(&media.path, b"another take entirely").expect("write");
    assert_eq!(
        offline::check_media(&media, Verify::Quick),
        Availability::Online
    );
    match offline::check_media(&media, Verify::Content) {
        Availability::Offline(OfflineReason::Changed { expected, found }) => {
            assert_eq!(expected, media.content_hash);
            assert_ne!(found, expected);
        }
        other => panic!("expected a content change, got {other:?}"),
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = std::path::Path::new(&media.path);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        // Root reads anything; the check only means something for other users.
        if std::fs::File::open(path).is_err() {
            match offline::check_media(&media, Verify::Quick) {
                Availability::Offline(OfflineReason::Unreadable { error }) => {
                    assert!(!error.is_empty());
                    let message = OfflineReason::Unreadable { error }.message(&media.path);
                    assert!(message.contains(&media.path), "{message}");
                }
                other => panic!("expected unreadable, got {other:?}"),
            }
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_project_with_offline_media_round_trips_unchanged() {
    let dir = temp_dir("round-trip");
    let (p, _, gone, v1) = project(&dir);
    let saved = serde_json::to_string_pretty(&p).expect("save");

    // Load, check (media offline), and the tree is exactly what was saved.
    let loaded: Project = serde_json::from_str(&saved).expect("load");
    let status = check_project(&loaded, Verify::Content);
    assert!(status.is_offline(gone));
    assert!(loaded.content_eq(&p));
    assert_eq!(serde_json::to_string_pretty(&loaded).expect("save"), saved);

    // Edit the offline clips: move, trim, remove, each undone; then redo and undo all.
    let mut history = History::new(loaded.clone());
    let offline = offline_clips(history.current(), &status);
    let first = offline[0].item;
    let second = offline[1].item;
    let moved = history
        .current()
        .move_item(first, v1, frames(400))
        .expect("move");
    history.commit(EditLabel::Move, moved);
    let trimmed = history
        .current()
        .trim_item(second, TimeRange::new(frames(105), frames(20)))
        .expect("trim");
    history.commit(EditLabel::Trim, trimmed);
    let removed = history.current().remove_item(second).expect("remove");
    history.commit(EditLabel::Delete, removed);

    // Offline clips move with their edits: still offline, still labelled the same file.
    let after = offline_clips(
        history.current(),
        &check_project(history.current(), Verify::Quick),
    );
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].range.start, frames(400));
    let edited = serde_json::to_string_pretty(history.current()).expect("save");
    assert_ne!(edited, saved);
    let reloaded: Project = serde_json::from_str(&edited).expect("load");
    assert!(reloaded.content_eq(history.current()));

    while history.undo().is_some() {}
    assert!(history.current().content_eq(&p));
    assert_eq!(
        serde_json::to_string_pretty(history.current()).expect("save"),
        saved
    );
    while history.redo().is_some() {}
    assert_eq!(
        serde_json::to_string_pretty(history.current()).expect("save"),
        edited
    );
    while history.undo().is_some() {}
    assert_eq!(
        serde_json::to_string_pretty(history.current()).expect("save"),
        saved
    );

    // Checking never touched the tree.
    assert!(p.media().get(gone).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
