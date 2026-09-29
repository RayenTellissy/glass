//! The analysis kinds on the fixtures through the pool. Skips without fixtures
//! (`tests/fixtures/generate.sh`).

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use engine::analysis::{
    AnalysisPool, JobEvent, JobEventKind, Outcome, PoolConfig, Priority, TimelineJob,
};
use media::{CacheDir, SourceFile, SourceTimeline, StreamSelect, TimelineCache};

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
