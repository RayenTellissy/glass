//! Relinking with real media: fixtures copied and renamed into a temp folder, then moved
//! (a moved folder, a renamed file, a re-encoded file under the same name, near-duplicate
//! durations and a file too short for its clips), searched, and relinked as one edit.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use engine::media_mgmt::import::facts_of;
use engine::media_mgmt::offline::{Verify, check_project};
use engine::media_mgmt::relink::{
    self, Check, Choice, Confidence, FolderMove, RelinkEvent, RelinkSearch, SearchRequest,
    SearchResult,
};
use media::ContentHash;
use project::history::{EditLabel, History};
use project::media_pool::{MediaItem, MediaKind};
use project::{ClipSpec, EditError, MediaId, Project, Rational, Time, TimeRange, TrackKind};

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn fixture(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/generated")
        .join(name);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipped: {name} missing; run tests/fixtures/generate.sh to create it");
        None
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-relink-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
    std::fs::copy(from, to).expect("copy");
}

/// A project and pool that imported `files` (name, fixture, seconds of source used),
/// one clip each.
struct Imported {
    project: Project,
    ids: Vec<MediaId>,
}

fn import(files: &[(&Path, f64)]) -> Imported {
    let mut project = Project::new("uid-relink", "Harbour film");
    let mut ids = Vec::new();
    let mut entries = Vec::new();
    for (path, _) in files {
        let mut item = MediaItem::new(
            path.file_stem().expect("stem").to_string_lossy(),
            MediaKind::Video,
            path.to_string_lossy(),
        );
        item.facts = facts_of(&media::probe(path).expect("probe"));
        item.content_hash = ContentHash::of_file(path).expect("hash").to_hex();
        item.size_bytes = std::fs::metadata(path).expect("size").len();
        let (p, new) = project
            .import_media(project.media().root(), [item])
            .expect("media");
        project = p;
        let id = new[0];
        ids.push(id);
        entries.push(id);
    }
    let (p, seq) = project
        .add_sequence("Main sequence", FPS, 48_000)
        .expect("seq");
    let (mut p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let mut at = 0;
    for (id, (_, used)) in entries.iter().zip(files) {
        let len = (used * 25.0).round() as i64;
        p = p
            .insert_clip(
                v1,
                ClipSpec {
                    media: *id,
                    range: TimeRange::new(frames(at), frames(len)),
                    source_start: Time::ZERO,
                },
            )
            .expect("clip")
            .0;
        at += len + 10;
    }
    Imported { project: p, ids }
}

fn run(request: &SearchRequest) -> SearchResult {
    let cancel = AtomicBool::new(false);
    relink::search(request, &mut |_| {}, &cancel).expect("search")
}

#[test]
fn moved_renamed_and_reencoded_files_are_told_apart() {
    let (Some(a), Some(b), Some(c), Some(reencode), Some(short), Some(near1), Some(near2)) = (
        fixture("h264_1080p30.mp4"),
        fixture("sync_h264_aac_48k.mp4"),
        fixture("h264_10bit_720p30.mp4"),
        fixture("vfr_h264_720p.mp4"),
        fixture("h264_4k30.mp4"),
        fixture("vfr_h264_nob_640x360.mp4"),
        fixture("hevc_10bit_1080p25.mp4"),
    ) else {
        return;
    };
    let dir = temp_dir("search");
    let old = dir.join("Card");
    let a_old = old.join("Card A/A014_C011.mp4");
    let b_old = old.join("Card B/B002_C004.mp4");
    let c_old = old.join("Card A/C003.mp4");
    copy(&a, &a_old);
    copy(&b, &b_old);
    copy(&c, &c_old);
    // A014 uses 2.4 of its 3 s, B002 2.4 of 3 s, C003 1 of 2 s.
    let imported = import(&[(&a_old, 2.4), (&b_old, 2.4), (&c_old, 1.0)]);
    let (ida, idb, idc) = (imported.ids[0], imported.ids[1], imported.ids[2]);

    // The card folder moves into the project folder; B002 is renamed on the way; C003
    // is lost. Elsewhere: a re-encode of A014 and a 2 s file named B002 (too short), and
    // two different files named C003 with near the same length.
    let new = dir.join("Studio SSD/Harbour");
    std::fs::create_dir_all(&new).expect("mkdir");
    std::fs::rename(&old, new.join("Card")).expect("move folder");
    std::fs::rename(
        new.join("Card/Card B/B002_C004.mp4"),
        new.join("Card/Card B/gulls_final.mp4"),
    )
    .expect("rename");
    std::fs::remove_file(new.join("Card/Card A/C003.mp4")).expect("lose");
    copy(&reencode, &new.join("Exports/A014_C011.mp4"));
    copy(&short, &new.join("Exports/B002_C004.mp4"));
    copy(&near1, &new.join("Other/C003.mp4"));
    copy(&near2, &new.join("Archive/C003.mp4"));

    let availability = check_project(&imported.project, Verify::Quick);
    assert_eq!(availability.offline_count(), 3);
    let targets = relink::targets(&imported.project, &availability);
    assert_eq!(targets.len(), 3);
    assert_eq!(targets[0].clips, 1);
    assert!((targets[0].used_until.expect("used") - 2.4).abs() < 1e-9);
    let result = run(&SearchRequest {
        targets: targets.clone(),
        folders: vec![dir.join("Studio SSD")],
        ..SearchRequest::default()
    });
    assert_eq!(result.folders_searched, 1);
    assert_eq!(result.files_seen, 6);

    // A014: the moved original is exact; the re-encode has the same name and length
    // but not the same bytes, so it ranks below and never as exact.
    let ma = result.get(ida).expect("a");
    let best = &ma.candidates[ma.best().expect("picked")];
    assert_eq!(best.path, new.join("Card/Card A/A014_C011.mp4"));
    assert_eq!(best.confidence, Confidence::Exact);
    assert_eq!(best.percent(), 100);
    assert_eq!(
        (best.name, best.duration, best.hash),
        (Check::Match, Check::Match, Check::Match)
    );
    let re = ma
        .candidates
        .iter()
        .find(|c| c.path.ends_with("Exports/A014_C011.mp4"))
        .expect("re-encode offered");
    assert_eq!(re.hash, Check::Mismatch);
    assert_eq!(re.name, Check::Match);
    assert_ne!(re.confidence, Confidence::Exact);
    assert!(re.score < best.score && re.percent() < 100);

    // B002: renamed, found by its hash alone; the same-named short file is offered but
    // cannot be relinked.
    let mb = result.get(idb).expect("b");
    let best = &mb.candidates[mb.best().expect("picked")];
    assert_eq!(best.path, new.join("Card/Card B/gulls_final.mp4"));
    assert_eq!((best.name, best.hash), (Check::Mismatch, Check::Match));
    let decoy = mb
        .candidates
        .iter()
        .find(|c| c.path.ends_with("Exports/B002_C004.mp4"))
        .expect("short file offered");
    assert!(decoy.too_short && !decoy.relinkable());

    // C003: two near-duplicates, neither exact; the one with the source's codec and
    // frame rate ranks first.
    let mc = result.get(idc).expect("c");
    assert!(mc.candidates.len() >= 2, "{:?}", mc.candidates);
    assert!(
        mc.candidates
            .iter()
            .all(|c| c.confidence != Confidence::Exact)
    );
    assert!(
        mc.candidates[0].path.ends_with("Other/C003.mp4"),
        "{:?}",
        mc.candidates
    );
    assert!(mc.candidates[0].score > mc.candidates[1].score);

    // The exact match's folder move is reported for the rest.
    assert!(
        result.moves.contains(&FolderMove {
            from: dir.clone(),
            to: new.clone(),
        }),
        "{:?}",
        result.moves
    );

    // Relink A014 and B002 as one edit: only the references change.
    let choice = |id: MediaId, m: &relink::TargetMatches| Choice {
        media: id,
        candidate: m.candidates[m.best().expect("best")].clone(),
    };
    let choices = vec![choice(ida, ma), choice(idb, mb)];
    let next = relink::apply(&imported.project, &choices).expect("apply");
    assert!(
        next.sequences()
            .iter()
            .zip(imported.project.sequences())
            .all(|(a, b)| std::sync::Arc::ptr_eq(a, b))
    );
    next.validate().expect("valid");
    let mut history = History::new(imported.project.clone());
    history.commit(EditLabel::Relink, next.clone());
    let after = check_project(&next, Verify::Content);
    assert!(!after.is_offline(ida) && !after.is_offline(idb));
    assert!(after.is_offline(idc));
    let mut availability = availability;
    relink::mark_relinked(&choices, &mut availability);
    assert_eq!(availability.offline_count(), 1);
    // The edit pointed the pool item at its new file and kept the rest of it.
    let item = next.media().get(idb).expect("item");
    assert_eq!(
        item.path,
        new.join("Card/Card B/gulls_final.mp4").to_string_lossy()
    );
    let mut old = project::MediaItem::clone(imported.project.media().get(idb).expect("item"));
    old.path.clone_from(&item.path);
    old.content_hash.clone_from(&item.content_hash);
    assert_eq!(**item, old);
    history.undo();
    assert!(history.current().content_eq(&imported.project));

    // The too-short file is refused, and with it the whole batch.
    let refused = relink::apply(
        &imported.project,
        &[
            choice(ida, ma),
            Choice {
                media: idb,
                candidate: decoy.clone(),
            },
        ],
    );
    assert!(
        matches!(refused, Err(EditError::RelinkTooShort { media, .. }) if media == idb),
        "{refused:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn one_found_clip_brings_the_rest_by_the_same_move() {
    let (Some(a), Some(b)) = (
        fixture("h264_1080p30.mp4"),
        fixture("sync_h264_aac_48k.mp4"),
    ) else {
        return;
    };
    let dir = temp_dir("move");
    let a_old = dir.join("Volumes/Card A/A014_C011.mp4");
    let b_old = dir.join("Volumes/Card B/B002_C004.mp4");
    copy(&a, &a_old);
    copy(&b, &b_old);
    let imported = import(&[(&a_old, 1.0), (&b_old, 1.0)]);
    let (ida, idb) = (imported.ids[0], imported.ids[1]);
    let new = dir.join("Studio SSD/Harbour");
    std::fs::create_dir_all(new.parent().expect("parent")).expect("mkdir");
    std::fs::rename(dir.join("Volumes"), &new).expect("move");

    let availability = check_project(&imported.project, Verify::Quick);
    let targets = relink::targets(&imported.project, &availability);
    // Only Card A is searched: B002 comes back by repeating A014's move.
    let search = RelinkSearch::start(SearchRequest {
        targets: targets.clone(),
        folders: vec![new.join("Card A")],
        ..SearchRequest::default()
    })
    .expect("thread");
    let events = search.wait();
    let Some(RelinkEvent::Finished(result)) = events.last() else {
        panic!("no result: {events:?}");
    };
    let mb = result.get(idb).expect("b");
    let found = &mb.candidates[mb.best().expect("picked")];
    assert_eq!(found.path, new.join("Card B/B002_C004.mp4"));
    assert!(found.from_move);
    assert_eq!(found.confidence, Confidence::Exact);
    assert!(result.get(ida).expect("a").best().is_some());

    // The user's own pick feeds a move the same way: with no folder at all, the move
    // alone finds both.
    let mv = FolderMove::infer(&a_old, &new.join("Card A/A014_C011.mp4")).expect("move");
    let result = run(&SearchRequest {
        targets,
        moves: vec![mv],
        ..SearchRequest::default()
    });
    assert_eq!(result.files_seen, 0);
    assert_eq!(result.matched(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_search_can_be_cancelled() {
    let dir = temp_dir("cancel");
    std::fs::write(dir.join("A014_C011.mov"), b"bytes").expect("write");
    let p = Project::new("uid", "p");
    let (p, _) = p
        .add_media("00", dir.join("gone/A014_C011.mov").to_string_lossy())
        .expect("media");
    let availability = check_project(&p, Verify::Quick);
    let cancel = AtomicBool::new(true);
    let request = SearchRequest {
        targets: relink::targets(&p, &availability),
        folders: vec![dir.clone()],
        ..SearchRequest::default()
    };
    assert!(relink::search(&request, &mut |_| {}, &cancel).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
