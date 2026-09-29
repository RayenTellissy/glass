//! The analysis kinds on the fixtures through the pool. Skips without fixtures
//! (`tests/fixtures/generate.sh`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use engine::analysis::{
    AnalysisJob, AnalysisPool, JobEvent, JobEventKind, Outcome, PoolConfig, Priority, ThumbnailJob,
    TimelineJob,
};
use media::analysis::thumbnails::{ThumbnailSettings, ThumbnailStrips};
use media::{CacheDir, CacheEntry, SourceFile, SourceTimeline, StreamSelect, TimelineCache};

const LIMIT: Duration = Duration::from_secs(120);

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

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("glass-jobs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn cache(&self) -> CacheDir {
        CacheDir::new(&self.0)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn pool(dir: &TempDir) -> (AnalysisPool, Receiver<JobEvent>) {
    AnalysisPool::new(PoolConfig::new(dir.cache())).expect("pool")
}

/// The next job to finish, and how.
fn next_outcome(events: &Receiver<JobEvent>) -> Outcome {
    loop {
        let event = events.recv_timeout(LIMIT).expect("event");
        if let JobEventKind::Finished(outcome) = event.kind {
            return outcome;
        }
    }
}

#[test]
fn timeline_job_shares_the_timeline_cache_file() {
    let Some(path) = fixture("vfr_h264_720p.mp4") else {
        return;
    };
    let dir = TempDir::new("timeline");
    let source = SourceFile::open(&path).expect("hash");
    let (pool, events) = pool(&dir);
    let job = TimelineJob {
        source: source.clone(),
        stream: StreamSelect::BestVideo,
    };
    let timeline = pool
        .submit(job.clone(), Priority::Visible)
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    let (scanned, _) = SourceTimeline::scan(&path, StreamSelect::BestVideo).expect("scan");
    assert_eq!(*timeline, scanned);
    // The media-side cache reads the file the job wrote, without scanning.
    let (loaded, how) =
        SourceTimeline::load_or_scan(&dir.cache(), &source, StreamSelect::BestVideo).expect("load");
    assert_eq!(how, None);
    assert_eq!(loaded, scanned);
    let memory = TimelineCache::with_disk(dir.cache());
    assert_eq!(
        *memory
            .get_or_scan(&source, StreamSelect::BestVideo)
            .expect("load"),
        scanned
    );
    assert_eq!(next_outcome(&events), Outcome::Computed);
    pool.submit(job, Priority::Visible)
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    assert_eq!(next_outcome(&events), Outcome::Cached);
}

fn thumbnails(dir: &TempDir, file: &str) -> Option<(Arc<ThumbnailStrips>, Vec<JobEventKind>)> {
    let path = fixture(file)?;
    let (pool, events) = pool(dir);
    let handle = pool.submit(
        ThumbnailJob {
            source: SourceFile::open(&path).expect("hash"),
            settings: ThumbnailSettings::default(),
        },
        Priority::Visible,
    );
    let strips = handle
        .wait_timeout(LIMIT)
        .expect("finished")
        .unwrap_or_else(|e| panic!("{file}: {e}"));
    let mut kinds = Vec::new();
    loop {
        let event = events.recv_timeout(LIMIT).expect("event");
        let last = matches!(event.kind, JobEventKind::Finished(_));
        kinds.push(event.kind);
        if last {
            break;
        }
    }
    Some((strips, kinds))
}

fn entry_of(dir: &TempDir, file: &str) -> CacheEntry {
    let source = SourceFile::open(fixture(file).expect("fixture")).expect("hash");
    dir.cache().entry(
        &source.hash,
        ThumbnailJob::KIND,
        &ThumbnailSettings::default().params(),
        ThumbnailJob::VERSION,
    )
}

/// Level counts, sizes, and each thumbnail's time: at or before its target and within
/// half a second of it.
#[test]
fn thumbnail_levels_have_the_expected_counts_and_times() {
    // (file, duration-derived counts for 1 s, 5 s, 30 s, width x height)
    let cases: [(&str, [usize; 3], (u32, u32)); 7] = [
        ("h264_1080p30.mp4", [3, 1, 1], (192, 108)),
        ("h264_4k30.mp4", [2, 1, 1], (192, 108)),
        ("h264_bframes_gop120_720p60.mp4", [3, 1, 1], (192, 108)),
        ("vfr_h264_720p.mp4", [3, 1, 1], (192, 108)),
        ("anim_vfr_160x120.gif", [2, 1, 1], (144, 108)),
        ("still_1080p.png", [1, 1, 1], (192, 108)),
        ("sequence_640x360_24", [2, 1, 1], (192, 108)),
    ];
    for (file, counts, size) in cases {
        let dir = TempDir::new("thumb-levels");
        let Some((strips, events)) = thumbnails(&dir, file) else {
            return;
        };
        assert_eq!((strips.width, strips.height), size, "{file}");
        assert_eq!(strips.levels.len(), 3);
        for (level, strip) in strips.levels.iter().enumerate() {
            assert_eq!(strip.level as usize, level);
            assert_eq!(strip.interval, [1, 5, 30][level]);
            assert_eq!(strip.len(), counts[level], "{file} level {level}");
            for (i, &t) in strip.times_us.iter().enumerate() {
                let target = i as i64 * i64::from(strip.interval) * 1_000_000;
                assert!(
                    t <= target && target - t <= 500_000,
                    "{file} level {level} thumb {i}: {t} us for {target} us"
                );
            }
            assert_eq!(strip.atlases.len(), strip.len().div_ceil(64));
        }
        // Coarse first: level 2, then 1, then 0, before Finished.
        let partials: Vec<u32> = events
            .iter()
            .filter_map(|k| match k {
                JobEventKind::Partial(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(partials, [2, 1, 0], "{file}");
        assert_eq!(
            events.last(),
            Some(&JobEventKind::Finished(Outcome::Computed))
        );
    }
    // Constant-rate clips with a keyframe every second land exactly on the targets.
    let dir = TempDir::new("thumb-exact");
    if let Some((strips, _)) = thumbnails(&dir, "h264_1080p30.mp4") {
        assert_eq!(strips.levels[0].times_us, [0, 1_000_000, 2_000_000]);
    }
}

fn close(pixel: Option<[u8; 4]>, rgb: [u8; 3]) -> bool {
    pixel.is_some_and(|p| (0..3).all(|c| p[c].abs_diff(rgb[c]) <= 40))
}

const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];

/// Loads thumbnail 0 of level 0 from its atlas.
fn first_thumbnail(dir: &TempDir, file: &str, strips: &ThumbnailStrips) -> media::RgbaImage {
    let entry = entry_of(dir, file);
    let at = strips.locate(0, 0).expect("thumbnail");
    let atlas = ThumbnailStrips::load_atlas(&entry, at.atlas).expect("atlas");
    ThumbnailStrips::crop(&atlas, &at).expect("crop")
}

#[test]
fn thumbnails_are_upright() {
    // Video with a 90-degree display rotation: stored 640x360 with the red block top
    // left, shown 360x640 with it bottom left.
    let dir = TempDir::new("thumb-rotated");
    if let Some((strips, _)) = thumbnails(&dir, "rotated_marked_h264_640x360.mov") {
        assert_eq!((strips.width, strips.height), (61, 108));
        let thumb = first_thumbnail(&dir, "rotated_marked_h264_640x360.mov", &strips);
        assert!(close(thumb.pixel(4, 104), RED), "{:?}", thumb.pixel(4, 104));
        assert!(close(thumb.pixel(4, 4), BLUE), "{:?}", thumb.pixel(4, 4));
        assert!(
            close(thumb.pixel(56, 104), BLUE),
            "{:?}",
            thumb.pixel(56, 104)
        );
    }
    // EXIF orientation 6: upright 360x640 with the red block top right.
    let dir = TempDir::new("thumb-exif");
    if let Some((strips, _)) = thumbnails(&dir, "still_exif6_640x360.jpg") {
        assert_eq!((strips.width, strips.height), (61, 108));
        let thumb = first_thumbnail(&dir, "still_exif6_640x360.jpg", &strips);
        assert!(close(thumb.pixel(56, 4), RED), "{:?}", thumb.pixel(56, 4));
        assert!(close(thumb.pixel(4, 4), BLUE), "{:?}", thumb.pixel(4, 4));
    }
    // Unrotated reference: red top left.
    let dir = TempDir::new("thumb-plain");
    if let Some((strips, _)) = thumbnails(&dir, "still_marked_640x360.tiff") {
        let thumb = first_thumbnail(&dir, "still_marked_640x360.tiff", &strips);
        assert!(close(thumb.pixel(4, 4), RED), "{:?}", thumb.pixel(4, 4));
        assert!(
            close(thumb.pixel(180, 100), BLUE),
            "{:?}",
            thumb.pixel(180, 100)
        );
    }
}

#[test]
fn thumbnails_are_reused_from_the_cache() {
    let file = "h264_1080p30.mp4";
    if fixture(file).is_none() {
        return;
    }
    let dir = TempDir::new("thumb-cache");
    let (first, _) = thumbnails(&dir, file).expect("fixture");
    let entry = entry_of(&dir, file);
    // Levels are readable on their own (what the UI reads while the job still runs).
    let levels = ThumbnailStrips::load_levels(&entry);
    assert_eq!(levels.len(), 3);
    assert_eq!(levels, first.levels);
    let (second, events) = thumbnails(&dir, file).expect("fixture");
    assert_eq!(first, second);
    assert_eq!(
        events.last(),
        Some(&JobEventKind::Finished(Outcome::Cached))
    );
    assert!(
        !events
            .iter()
            .any(|k| matches!(k, JobEventKind::Progress(_) | JobEventKind::Partial(_))),
        "no work on a hit"
    );
    // A missing atlas makes the entry invalid, so the job runs again.
    std::fs::remove_file(entry.dir().join(&first.levels[0].atlases[0])).expect("remove");
    let (third, events) = thumbnails(&dir, file).expect("fixture");
    assert_eq!(first, third);
    assert_eq!(
        events.last(),
        Some(&JobEventKind::Finished(Outcome::Computed))
    );
}
