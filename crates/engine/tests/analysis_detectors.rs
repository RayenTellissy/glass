//! Scene cut, silence, beat and loudness jobs through the pool: results, the disk cache
//! and keys that follow the settings. Skips without fixtures (`tests/fixtures/generate.sh`).

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use engine::analysis::{
    AnalysisJob, AnalysisPool, JobEvent, JobEventKind, Outcome, PoolConfig, Priority, SceneJob,
    SilenceJob,
};
use media::analysis::scenes::SceneSettings;
use media::analysis::silence::SilenceSettings;
use media::{CacheDir, SourceFile};

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
        let dir = std::env::temp_dir().join(format!("glass-detect-{name}-{}", std::process::id()));
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

fn next_outcome(events: &Receiver<JobEvent>) -> Outcome {
    loop {
        let event = events.recv_timeout(LIMIT).expect("event");
        if let JobEventKind::Finished(outcome) = event.kind {
            return outcome;
        }
    }
}

/// Runs `job` twice (computed, then cached), then `other` (another key: computed).
fn computed_cached_rekeyed<J: AnalysisJob + Clone>(
    name: &str,
    job: J,
    other: J,
) -> Option<std::sync::Arc<J::Output>> {
    let dir = TempDir::new(name);
    let (pool, events) = AnalysisPool::new(PoolConfig::new(CacheDir::new(&dir.0))).expect("pool");
    assert_ne!(job.key(), other.key());
    let first = pool
        .submit(job.clone(), Priority::Visible)
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    assert_eq!(next_outcome(&events), Outcome::Computed);
    pool.submit(job, Priority::Visible)
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    assert_eq!(next_outcome(&events), Outcome::Cached);
    pool.submit(other, Priority::Visible)
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    assert_eq!(next_outcome(&events), Outcome::Computed);
    Some(first)
}

#[test]
fn scene_job_caches_by_settings() {
    let Some(path) = fixture("scenes_cuts_640x360_30.mp4") else {
        return;
    };
    let source = SourceFile::open(&path).expect("hash");
    let job = SceneJob {
        source: source.clone(),
        settings: SceneSettings::default(),
    };
    let other = SceneJob {
        source,
        settings: SceneSettings {
            sensitivity: 0.8,
            ..SceneSettings::default()
        },
    };
    let cuts = computed_cached_rekeyed("scenes", job, other).expect("cuts");
    let frames: Vec<u64> = cuts.cuts.iter().map(|c| c.frame).collect();
    assert_eq!(frames, [60, 120, 180, 240, 300]);
}

#[test]
fn silence_job_caches_by_settings() {
    let Some(path) = fixture("silence_48k_stereo.wav") else {
        return;
    };
    let source = SourceFile::open(&path).expect("hash");
    let job = SilenceJob {
        source: source.clone(),
        settings: SilenceSettings::default(),
    };
    let other = SilenceJob {
        source,
        settings: SilenceSettings {
            min_ms: 1000,
            ..SilenceSettings::default()
        },
    };
    let silences = computed_cached_rekeyed("silence", job, other).expect("silences");
    let starts: Vec<u64> = silences.ranges.iter().map(|r| r.start / 48).collect();
    assert_eq!(starts.len(), 5, "{silences:?}");
    for (got, ms) in starts.iter().zip([0, 1500, 3500, 5020, 7200]) {
        assert!(got.abs_diff(ms) <= 3, "{silences:?}");
    }
}
