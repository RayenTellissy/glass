//! The analysis kinds on the fixtures through the pool. Skips without fixtures
//! (`tests/fixtures/generate.sh`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use engine::analysis::{
    AnalysisJob, AnalysisPool, JobEvent, JobEventKind, Outcome, PeaksJob, PoolConfig, Priority,
    ProxyJob, ThumbnailJob, TimelineJob,
};
use media::analysis::peaks::{LEVELS, PeakSettings, Peaks};
use media::analysis::proxy::{ProxyEncoder, ProxyFormat, ProxySettings};
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

/// A 16-bit PCM WAV file.
fn write_wav(path: &std::path::Path, rate: u32, channels: &[Vec<f32>]) {
    let frames = channels[0].len();
    let n = channels.len() as u16;
    let data_len = (frames * channels.len() * 2) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(n) * 2).to_le_bytes());
    out.extend_from_slice(&(n * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        for channel in channels {
            let v = (channel[i] * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    std::fs::write(path, out).expect("write wav");
}

const CLICK_AT: usize = 100_000;
const SINE_FRAMES: usize = 240_000;

/// A 1 kHz sine at 0.5 (-6 dBFS), 48 kHz, as the 16-bit file stores it.
fn sine() -> Vec<f32> {
    (0..SINE_FRAMES)
        .map(|i| {
            let x = 0.5 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48_000.0).sin();
            ((x * 32768.0).round() / 32768.0) as f32
        })
        .collect()
}

/// Left: [`sine`]; right: silence with one 0.9 sample (a click).
fn sine_and_click(dir: &TempDir) -> SourceFile {
    let left = sine();
    let mut right = vec![0f32; SINE_FRAMES];
    right[CLICK_AT] = 0.9;
    let path = dir.0.join("sine_click.wav");
    write_wav(&path, 48_000, &[left, right]);
    SourceFile::open(&path).expect("hash")
}

fn peaks_of(dir: &TempDir, source: &SourceFile) -> (Arc<Peaks>, Outcome) {
    let (pool, events) = pool(dir);
    let peaks = pool
        .submit(
            PeaksJob {
                source: source.clone(),
                settings: PeakSettings::default(),
            },
            Priority::Visible,
        )
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    (peaks, next_outcome(&events))
}

#[test]
fn peaks_of_a_sine_and_a_click() {
    let dir = TempDir::new("peaks");
    let source = sine_and_click(&dir);
    let (peaks, outcome) = peaks_of(&dir, &source);
    assert_eq!(outcome, Outcome::Computed);
    assert_eq!(peaks.channels, 2);
    assert_eq!(peaks.sample_rate, 48_000);
    assert!(
        peaks.total_samples.abs_diff(SINE_FRAMES as u64) <= 1,
        "{}",
        peaks.total_samples
    );
    let total = peaks.total_samples;
    let rms = (0.5 / 2f64.sqrt() * 32767.0) as i32;
    let sine = sine();
    for (l, level) in peaks.levels.iter().enumerate() {
        let spp = u64::from(LEVELS[l]);
        assert_eq!(level.samples_per_peak, LEVELS[l]);
        let count = total.div_ceil(spp);
        let all = peaks.peaks(l, 0..total);
        assert_eq!(all.len() as u64, count, "level {l}");
        let click = CLICK_AT as u64 / spp;
        for i in 0..all.len() {
            let full = (i as u64 + 1) * spp <= total;
            let left = all.get(i, 0).expect("left");
            let right = all.get(i, 1).expect("right");
            if full {
                // 48 samples per cycle: every block holds the exact crest and trough.
                assert!(
                    (i32::from(left.max) - 16384).abs() <= 1,
                    "level {l} peak {i}: {left:?}"
                );
                assert!(
                    (i32::from(left.min) + 16384).abs() <= 1,
                    "level {l} peak {i}: {left:?}"
                );
                // RMS of exactly this block of the (16-bit) sine.
                let block = &sine[(i as u64 * spp) as usize..((i as u64 + 1) * spp) as usize];
                let mean: f64 =
                    block.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / block.len() as f64;
                let expected = mean.sqrt() * 32767.0;
                assert!(
                    (f64::from(left.rms) - expected).abs() <= 2.0,
                    "level {l} peak {i}: {left:?}, expected rms {expected}"
                );
                assert!((i32::from(left.rms) - rms).abs() <= rms / 50);
            }
            if i as u64 == click {
                assert!((i32::from(right.max) - 29491).abs() <= 2, "{right:?}");
                assert_eq!(right.min, 0);
                assert!(right.rms > 0);
            } else {
                assert_eq!(
                    (right.min, right.max, right.rms),
                    (0, 0, 0),
                    "level {l} peak {i}"
                );
            }
        }
        // Range lookup finds the click at every level.
        let around = peaks.peaks(l, CLICK_AT as u64..CLICK_AT as u64 + 1);
        assert_eq!((around.first, around.len()), (click, 1));
        assert!(around.get(0, 1).expect("peak").max > 29000);
    }
}

#[test]
fn coarser_levels_summarise_finer_ones() {
    let dir = TempDir::new("peaks-mip");
    let source = sine_and_click(&dir);
    let (peaks, _) = peaks_of(&dir, &source);
    let total = peaks.total_samples;
    for l in 1..peaks.levels.len() {
        let (fine, coarse) = (peaks.peaks(l - 1, 0..total), peaks.peaks(l, 0..total));
        let fine_spp = u64::from(LEVELS[l - 1]);
        for i in 0..coarse.len() {
            for c in 0..2 {
                let children: Vec<(u64, _)> = (i * 4..(i * 4 + 4).min(fine.len()))
                    .map(|j| {
                        let n = fine_spp.min(total - j as u64 * fine_spp);
                        (n, fine.get(j, c).expect("fine"))
                    })
                    .collect();
                let peak = coarse.get(i, c).expect("coarse");
                assert_eq!(
                    peak.min,
                    children.iter().map(|(_, p)| p.min).min().expect("min")
                );
                assert_eq!(
                    peak.max,
                    children.iter().map(|(_, p)| p.max).max().expect("max")
                );
                let weight: u64 = children.iter().map(|(n, _)| n).sum();
                let squares: f64 = children
                    .iter()
                    .map(|(n, p)| *n as f64 * f64::from(p.rms).powi(2))
                    .sum();
                let expected = (squares / weight as f64).sqrt();
                assert!(
                    (f64::from(peak.rms) - expected).abs() <= 2.0,
                    "level {l} peak {i} channel {c}: {} vs {expected}",
                    peak.rms
                );
            }
        }
    }
}

#[test]
fn peaks_are_reused_from_the_cache() {
    let dir = TempDir::new("peaks-cache");
    let source = sine_and_click(&dir);
    let (first, outcome) = peaks_of(&dir, &source);
    assert_eq!(outcome, Outcome::Computed);
    let entry = dir.cache().entry(
        &source.hash,
        PeaksJob::KIND,
        &PeakSettings::default().params(),
        PeaksJob::VERSION,
    );
    let bytes = entry.read("bin").expect("read").expect("peaks file");
    let values: usize = first.levels.iter().map(|l| l.peaks.len()).sum();
    assert_eq!(bytes.len(), 32 + 5 * 12 + values * 6);
    let (second, outcome) = peaks_of(&dir, &source);
    assert_eq!(outcome, Outcome::Cached);
    assert_eq!(first, second);
    // Mono sources keep one channel.
    if let Some(path) = fixture("audio_44k1_mono.mp3") {
        let mono = SourceFile::open(path).expect("hash");
        let (peaks, _) = peaks_of(&dir, &mono);
        assert_eq!(peaks.channels, 1);
        // 2 s at 44.1 kHz, resampled to 48 kHz.
        assert!(
            peaks.total_samples.abs_diff(96_000) <= 1200,
            "{}",
            peaks.total_samples
        );
    }
}

/// A proxy through the pool lands under `proxies/<hash>/`; cancelling mid-way and
/// submitting again finishes it without redoing finished segments; a further submission
/// is a cache hit.
#[test]
fn proxy_job_resumes_and_caches() {
    let Some(path) = fixture("h264_1080p30.mp4") else {
        return;
    };
    let dir = TempDir::new("proxy");
    let source = SourceFile::open(&path).expect("hash");
    let settings = ProxySettings {
        format: ProxyFormat::ProResProxy,
        encoder: ProxyEncoder::Software,
        segment_ms: 250,
        ..ProxySettings::default()
    };
    let job = ProxyJob {
        source: source.clone(),
        settings,
    };
    let (pool, events) = pool(&dir);
    let handle = pool.submit(job.clone(), Priority::Background);
    loop {
        let event = events.recv_timeout(LIMIT).expect("event");
        match event.kind {
            JobEventKind::Progress(p) if p >= 0.4 => {
                handle.cancel();
                break;
            }
            JobEventKind::Finished(_) => break,
            _ => {}
        }
    }
    let cancelled = handle.wait_timeout(LIMIT).expect("finished").is_err();
    let proxy = pool
        .submit(job.clone(), Priority::Visible)
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    let file = proxy.path(&dir.cache().entry_hashed_in(
        media::CacheArea::Proxies,
        &source.hash,
        ProxyJob::KIND,
        &media::cache::params_hash(&settings.params()),
        ProxyJob::VERSION,
    ));
    assert!(file.is_file());
    assert!(file.starts_with(dir.0.join("proxies").join(source.hash.to_hex())));
    assert_eq!(proxy.frames, 90);
    eprintln!(
        "cancelled: {cancelled}, then {} of {} segments encoded",
        proxy.segments_encoded, proxy.segments
    );
    if cancelled {
        assert!(
            proxy.segments_encoded < proxy.segments,
            "resumed: {} of {} segments encoded",
            proxy.segments_encoded,
            proxy.segments
        );
    }
    let again = pool
        .submit(job, Priority::Visible)
        .wait_timeout(LIMIT)
        .expect("finished")
        .expect("ok");
    assert_eq!(*again, *proxy);
    let mut outcomes = Vec::new();
    while let Ok(event) = events.recv_timeout(Duration::from_millis(200)) {
        if let JobEventKind::Finished(outcome) = event.kind {
            outcomes.push(outcome);
        }
    }
    assert_eq!(outcomes.last(), Some(&Outcome::Cached));
}
