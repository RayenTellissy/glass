//! Import on synthesised camera cards and folders: small generated clips copied into
//! AVCHD, XDCAM, Sony XAVC, Canon, Canon Cinema and DJI layouts in a temp folder, next to
//! a plain folder tree. Skips without fixtures (`tests/fixtures/generate.sh`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use engine::media_mgmt::{
    AfterImport, CardKind, CardOptions, ImportEvent, ImportOptions, ImportedClip, Importer,
    MediaClass, ScanOptions, SkipReason, Unreadable,
};

/// 640x360 H.264, 1 s (2.5 KB).
const SMALL: &str = "h264_fullrange_640x360.mp4";
/// H.264 + AAC 48 kHz (8 KB).
const BIG: &str = "sync_h264_aac_48k.mp4";
const STILL: &str = "still_exif6_640x360.jpg";

fn fixtures() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated");
    if dir.join(SMALL).is_file() && dir.join(BIG).is_file() {
        Some(dir)
    } else {
        eprintln!("skipped: fixtures missing; run tests/fixtures/generate.sh to create them");
        None
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("glass-import-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Copies a fixture with a few trailing bytes that make its content hash unique (the
/// demuxers ignore data after the last box or marker).
fn copy(from: &Path, to: &Path) {
    static COUNT: AtomicUsize = AtomicUsize::new(1);
    copy_exact(from, to);
    let mut bytes = std::fs::read(to).expect("read copy");
    bytes.extend(std::iter::repeat_n(
        0u8,
        COUNT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(to, bytes).expect("write copy");
}

fn copy_exact(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
    std::fs::copy(from, to).expect("copy fixture");
}

fn write(to: &Path, text: &str) {
    std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
    std::fs::write(to, text).expect("write");
}

const SONY_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<NonRealTimeMeta xmlns="urn:schemas-professionalDisc:nonRealTimeMeta:ver.2.20">
  <LtcChangeTable tcFps="25"><LtcChange frameCount="0" value="11074819" status="increment"/></LtcChangeTable>
  <CreationDate value="2026-08-12T19:48:07+02:00"/>
  <Device manufacturer="Sony" modelName="ILME-FX6V"/>
  <Lens modelName="FE 35mm F1.4 GM"/>
  <AcquisitionRecord><Group name="ExifGPS">
    <Item name="LatitudeRef" value="N"/><Item name="Latitude" value="60:23:27.600"/>
    <Item name="LongitudeRef" value="E"/><Item name="Longitude" value="5:19:19.200"/>
  </Group></AcquisitionRecord>
</NonRealTimeMeta>"#;

const DJI_SRT: &str = "1\n00:00:00,000 --> 00:00:00,033\n<font size=\"28\">FrameCnt: 1, DiffTime: 33ms\n2026-08-12 19:48:07.123\n[iso: 100] [shutter: 1/1000.0] [fnum: 2.8] [latitude: 60.391234] [longitude: 5.322345] [rel_alt: 30.100 abs_alt: 110.300] </font>\n";

/// Builds the test volume and returns (root, avchd span threshold).
fn build(root: &Path, fx: &Path) -> u64 {
    let small = fx.join(SMALL);
    let big = fx.join(BIG);
    // AVCHD: 00000 (big) is continued by 00001; 00002 starts a new recording.
    let stream = root.join("Card A/PRIVATE/AVCHD/BDMV/STREAM");
    copy(&big, &stream.join("00000.MTS"));
    copy(&small, &stream.join("00001.MTS"));
    copy(&big, &stream.join("00002.MTS"));
    write(
        &root.join("Card A/PRIVATE/AVCHD/BDMV/CLIPINF/00000.CPI"),
        "index",
    );
    write(&root.join("Card A/PRIVATE/AVCHD/BDMV/INDEX.BDM"), "index");
    // XDCAM: C0014_01 continues C0014; Sub holds the card's proxies.
    let clip = root.join("Card X/XDROOT/Clip");
    copy(&small, &clip.join("C0012.MXF"));
    copy(&small, &clip.join("C0014.MXF"));
    copy(&big, &clip.join("C0014_01.MXF"));
    write(&clip.join("C0012M01.XML"), SONY_XML);
    copy(&small, &root.join("Card X/XDROOT/Sub/C0012S03.MXF"));
    write(&root.join("Card X/XDROOT/MEDIAPRO.XML"), "<MediaProfile/>");
    // Sony XAVC with its XML sidecar.
    copy(&big, &root.join("FX6/PRIVATE/M4ROOT/CLIP/C0001.MP4"));
    write(&root.join("FX6/PRIVATE/M4ROOT/CLIP/C0001M01.XML"), SONY_XML);
    copy(
        &fx.join(STILL),
        &root.join("FX6/PRIVATE/M4ROOT/THMBNL/C0001T01.JPG"),
    );
    // Canon consumer and Cinema.
    copy(&small, &root.join("R6/DCIM/100CANON/MVI_0001.MP4"));
    copy(&fx.join(STILL), &root.join("R6/DCIM/100CANON/IMG_0002.JPG"));
    write(&root.join("R6/DCIM/100CANON/IMG_0003.CR3"), "raw");
    copy(
        &small,
        &root.join("C70/CONTENTS/CLIPS001/A001C001_260812AB_CANON.MXF"),
    );
    write(
        &root.join("C70/CONTENTS/CLIPS001/A001C001_260812AB_CANON.XML"),
        r#"<ClipMetadata><Camera modelName="EOS C70"/><ReelName value="A001"/></ClipMetadata>"#,
    );
    // DJI with telemetry.
    copy(&small, &root.join("Mavic/DCIM/DJI_001/DJI_0001.MP4"));
    write(&root.join("Mavic/DCIM/DJI_001/DJI_0001.SRT"), DJI_SRT);
    write(&root.join("Mavic/DCIM/DJI_001/DJI_0001.LRF"), "low res");
    // A plain folder tree with a sequence, an unreadable file and litter.
    copy(&small, &root.join("Harbour shoot/Day 1/A-cam/wide.mp4"));
    copy(&big, &root.join("Harbour shoot/Day 1/B-cam/close.mov"));
    copy_exact(
        &root.join("Harbour shoot/Day 1/A-cam/wide.mp4"),
        &root.join("Harbour shoot/Day 1/A-cam/wide copy.mp4"),
    );
    write(&root.join("Harbour shoot/Notes/call sheet.txt"), "call 7am");
    write(&root.join("Harbour shoot/poster.psd"), "8BPS");
    write(&root.join("Harbour shoot/.DS_Store"), "x");
    write(&root.join("Harbour shoot/._wide.mp4"), "x");
    let frames = fx.join("sequence_640x360_24");
    for n in 1..=10 {
        copy(
            &frames.join(format!("frame_{n:04}.png")),
            &root.join(format!("Harbour shoot/Titles/shot_{n:04}.png")),
        );
    }
    std::fs::metadata(stream.join("00000.MTS"))
        .expect("big")
        .len()
}

fn options(span: u64) -> ScanOptions {
    ScanOptions {
        cards: CardOptions {
            avchd_span_bytes: span,
        },
        known_paths: HashSet::new(),
    }
}

fn planned(importer: Importer) -> Arc<engine::media_mgmt::ImportPlan> {
    importer
        .wait()
        .into_iter()
        .find_map(|e| match e {
            ImportEvent::Planned(plan) => Some(plan),
            _ => None,
        })
        .expect("a plan")
}

#[test]
fn cards_are_read_as_cameras_and_folders_become_bins() {
    let Some(fx) = fixtures() else { return };
    let tmp = TempDir::new("cards");
    let span = build(&tmp.0, &fx);
    let plan = planned(Importer::scan(vec![tmp.0.clone()], options(span)).expect("scan"));

    let names = |kind: CardKind| -> Vec<(String, usize)> {
        plan.sources
            .iter()
            .filter(|s| s.card == Some(kind))
            .map(|s| (s.name.clone(), s.files.len()))
            .collect()
    };
    assert_eq!(
        names(CardKind::Avchd),
        [("00000".into(), 2), ("00002".into(), 1)]
    );
    assert_eq!(
        names(CardKind::Xdcam),
        [("C0012".into(), 1), ("C0014".into(), 2)]
    );
    assert_eq!(names(CardKind::SonyXavc), [("C0001".into(), 1)]);
    assert_eq!(
        names(CardKind::Canon),
        [("IMG_0002".into(), 1), ("MVI_0001".into(), 1)]
    );
    assert_eq!(
        names(CardKind::CanonCinema),
        [("A001C001_260812AB_CANON".into(), 1)]
    );
    assert_eq!(names(CardKind::Dji), [("DJI_0001".into(), 1)]);
    let mut kinds: Vec<CardKind> = plan.cards.iter().map(|c| c.kind).collect();
    kinds.sort();
    assert_eq!(kinds, CardKind::ALL);

    // Card clips land in a bin named after the card's folder, not its folder maze.
    let xavc = plan
        .sources
        .iter()
        .find(|s| s.name == "C0001")
        .expect("xavc");
    let root_name = tmp.0.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(xavc.bin, [root_name.clone(), "FX6".into()]);
    assert_eq!(xavc.sidecars.len(), 1);
    // Proxies, thumbnails and indexes on the cards are never imported.
    for s in &plan.sources {
        for f in &s.files {
            let f = f.to_string_lossy();
            assert!(
                !f.contains("/Sub/") && !f.contains("THMBNL") && !f.contains("CLIPINF"),
                "{f}"
            );
        }
    }

    let wide = plan
        .sources
        .iter()
        .find(|s| s.name == "wide")
        .expect("wide");
    assert_eq!(
        wide.bin,
        [root_name.as_str(), "Harbour shoot", "Day 1", "A-cam"]
    );
    let titles = plan
        .sources
        .iter()
        .find(|s| s.name == "shot")
        .expect("sequence");
    assert_eq!(titles.class, MediaClass::Sequence);
    assert_eq!(titles.files.len(), 10);
    assert_eq!(plan.unreadable_by_kind(), [(Unreadable::Photoshop, 1)]);
    let notes = plan
        .folders
        .iter()
        .find(|f| f.bin.last().map(String::as_str) == Some("Notes"))
        .expect("notes");
    assert!(notes.left_out());
    let harbour = plan
        .folders
        .iter()
        .find(|f| f.bin.last().map(String::as_str) == Some("Harbour shoot"))
        .expect("harbour");
    assert_eq!(harbour.clips, 4);
    assert_eq!(plan.sources.len(), 13);
}

struct Counter(AtomicUsize, Mutex<Vec<bool>>);

impl AfterImport for Counter {
    fn clip_imported(&self, _clip: &ImportedClip, options: &ImportOptions) {
        self.0.fetch_add(1, Ordering::Relaxed);
        self.1.lock().unwrap().push(options.split_into_scenes);
    }
}

#[test]
fn import_probes_hashes_joins_spans_and_reads_sidecars() {
    let Some(fx) = fixtures() else { return };
    let tmp = TempDir::new("probe");
    let span = build(&tmp.0, &fx);
    let hook = Arc::new(Counter(AtomicUsize::new(0), Mutex::new(Vec::new())));
    let events = Importer::scan_and_import(
        vec![tmp.0.clone()],
        ImportOptions {
            scan: options(span),
            split_into_scenes: true,
            ..ImportOptions::default()
        },
        vec![hook.clone()],
    )
    .expect("import")
    .wait();

    let clips: Vec<&ImportedClip> = events
        .iter()
        .filter_map(|e| match e {
            ImportEvent::Imported(c) => Some(&**c),
            _ => None,
        })
        .collect();
    let skipped: Vec<(&str, &SkipReason)> = events
        .iter()
        .filter_map(|e| match e {
            ImportEvent::Skipped { name, reason } => Some((name.as_str(), reason)),
            _ => None,
        })
        .collect();
    // `wide copy.mp4` is byte for byte `wide.mp4`: whichever comes second is recognised
    // by content hash.
    let finished = events
        .iter()
        .find_map(|e| match e {
            ImportEvent::Finished(s) => Some(*s),
            _ => None,
        })
        .expect("finished");
    assert_eq!(finished.failed, 0, "{skipped:?}");
    assert_eq!(
        (finished.imported, finished.duplicates),
        (12, 1),
        "{skipped:?}"
    );
    assert_eq!(hook.0.load(Ordering::Relaxed), finished.imported);
    assert!(hook.1.lock().unwrap().iter().all(|split| *split));
    assert_eq!(skipped, [("wide", &SkipReason::Duplicate)]);

    // Started events count up to the total, with byte progress.
    let started: Vec<(usize, usize, u64, u64)> = events
        .iter()
        .filter_map(|e| match e {
            ImportEvent::Started {
                index,
                total,
                bytes_done,
                bytes_total,
                ..
            } => Some((*index, *total, *bytes_done, *bytes_total)),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 13);
    assert!(
        started
            .windows(2)
            .all(|w| w[0].0 + 1 == w[1].0 && w[0].2 <= w[1].2)
    );

    let find = |name: &str| clips.iter().find(|c| c.source.name == name).copied();
    // The spanned AVCHD recording lasts as long as its two files together.
    let avchd = find("00000").expect("avchd clip");
    let one = media::probe(fx.join(BIG))
        .unwrap()
        .duration
        .unwrap()
        .seconds();
    let two = media::probe(fx.join(SMALL))
        .unwrap()
        .duration
        .unwrap()
        .seconds();
    let joined = avchd.facts.duration_s.expect("duration");
    assert!(
        (joined - (one + two)).abs() < 0.05,
        "{joined} vs {one} + {two}"
    );
    let big = media::probe(fx.join(BIG)).unwrap();
    assert_eq!(
        (avchd.facts.width, avchd.facts.height),
        big.video().unwrap().1.display_size()
    );
    assert_eq!(avchd.facts.video_codec.as_deref(), Some("h264"));
    assert_eq!(avchd.facts.audio_channels, 2);

    let xavc = find("C0001").expect("xavc");
    assert_eq!(xavc.metadata.camera_model.as_deref(), Some("ILME-FX6V"));
    assert_eq!(xavc.metadata.timecode.as_deref(), Some("19:48:07:11"));
    assert!(xavc.metadata.gps.is_some());

    let sequence = find("shot").expect("sequence");
    assert_eq!(sequence.facts.sequence_frames, Some(10));
    assert_eq!(sequence.facts.duration_s, None);

    // Every imported clip has its own hash.
    let hashes: HashSet<_> = clips.iter().map(|c| c.hash).collect();
    assert_eq!(hashes.len(), clips.len());
}

#[test]
fn known_paths_and_hashes_are_skipped_and_cancel_stops() {
    let Some(fx) = fixtures() else { return };
    let tmp = TempDir::new("skip");
    let file = tmp.0.join("wide.mp4");
    copy(&fx.join(SMALL), &file);
    copy(&fx.join(BIG), &tmp.0.join("close.mp4"));
    let hash = media::ContentHash::of_file(tmp.0.join("close.mp4")).unwrap();
    let mut scan = ScanOptions::default();
    scan.known_paths.insert(file.clone());
    let events = Importer::scan_and_import(
        vec![file.clone(), tmp.0.join("close.mp4")],
        ImportOptions {
            scan,
            known_hashes: [hash].into(),
            ..ImportOptions::default()
        },
        Vec::new(),
    )
    .expect("import")
    .wait();
    let plan = events
        .iter()
        .find_map(|e| match e {
            ImportEvent::Planned(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(plan.already_in_project(), 1);
    let finished = events
        .iter()
        .find_map(|e| match e {
            ImportEvent::Finished(s) => Some(*s),
            _ => None,
        })
        .unwrap();
    assert_eq!((finished.imported, finished.duplicates), (0, 1));

    // A cancelled import says so and stops.
    let importer =
        Importer::scan_and_import(vec![tmp.0.clone()], ImportOptions::default(), Vec::new())
            .expect("import");
    importer.cancel();
    let events = importer.wait();
    assert!(
        events.iter().any(|e| matches!(e, ImportEvent::Cancelled))
            || events.iter().any(|e| matches!(e, ImportEvent::Finished(_))),
    );
}

#[test]
fn split_into_scenes_queues_scene_detection() {
    use engine::analysis::{AnalysisPool, JobEventKind, Outcome, PoolConfig};
    use engine::media_mgmt::SceneDetectionOnImport;
    let Some(fx) = fixtures() else { return };
    let tmp = TempDir::new("scenes");
    copy(&fx.join(SMALL), &tmp.0.join("wide.mp4"));
    let (pool, events) =
        AnalysisPool::new(PoolConfig::new(media::CacheDir::new(tmp.0.join("cache"))))
            .expect("pool");
    let hook = Arc::new(SceneDetectionOnImport::new(Arc::new(pool)));
    Importer::scan_and_import(
        vec![tmp.0.join("wide.mp4")],
        ImportOptions {
            split_into_scenes: true,
            ..ImportOptions::default()
        },
        vec![hook.clone()],
    )
    .expect("import")
    .wait();
    // The hook owns the pool; keep it alive until the job ends.
    let finished = loop {
        let event = events
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("a scene job event");
        if event.key.kind == "scenes"
            && let JobEventKind::Finished(outcome) = event.kind
        {
            break outcome;
        }
    };
    assert_eq!(finished, Outcome::Computed);
    drop(hook);
}
