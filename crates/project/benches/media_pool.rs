//! Media pool costs on a 10,000-item pool: building the search index, the queries the
//! search field sends as the user types (prefix, exact, typo, several words, filters),
//! and one label edit on the persistent pool.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use project::media_pool::{
    ColourLabel, Facts, MediaItem, MediaKind, MediaPool, Metadata, Query, SearchIndex,
};

const ITEMS: usize = 10_000;

const SUBJECTS: [&str; 12] = [
    "Harbour",
    "Gulls",
    "Rope",
    "Water",
    "Forest",
    "Market",
    "Interview",
    "Boats",
    "Drone",
    "Street",
    "Kitchen",
    "Studio",
];
const SHOTS: [&str; 8] = [
    "wide", "detail", "at dusk", "take off", "close-up", "trail", "B-cam", "walk",
];
const TAGS: [&str; 10] = [
    "Golden hour",
    "Wide",
    "Interview",
    "Slow motion",
    "B-roll",
    "Detail",
    "Night",
    "Aerial",
    "Handheld",
    "Selects",
];
const CAMERAS: [&str; 5] = [
    "ILME-FX6V",
    "ILME-FX3",
    "EOS C70",
    "FC3582",
    "iPhone 15 Pro",
];
const CODECS: [&str; 4] = ["prores", "hevc", "h264", "dnxhd"];

/// A deterministic pool shaped like a long documentary shoot: 40 day/camera bins, camera
/// clip codes and descriptive names, one or two tags, camera metadata and a few labels.
fn pool() -> MediaPool {
    let mut pool = MediaPool::new("Harbour film");
    let root = pool.root();
    let bins: Vec<_> = (1..=20)
        .flat_map(|day| ["A-cam", "B-cam"].map(|cam| vec![format!("Day {day}"), cam.to_string()]))
        .map(|path| pool.bin_path(root, &path).unwrap())
        .collect();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as usize
    };
    for i in 0..ITEMS {
        let name = if i.is_multiple_of(3) {
            format!("A{:03}_C{:03}", i / 999 + 1, i % 999)
        } else {
            format!(
                "{} {} {}",
                SUBJECTS[next() % SUBJECTS.len()],
                SHOTS[next() % SHOTS.len()],
                i
            )
        };
        let mut item = MediaItem::new(
            &name,
            MediaKind::Video,
            format!("/Volumes/Shoot/{name}.mov"),
        );
        let width = [3840, 1920, 1280][next() % 3];
        item.facts = Facts {
            duration_s: Some((next() % 1200) as f64 / 10.0),
            width,
            height: width * 9 / 16,
            video_codec: Some(CODECS[next() % CODECS.len()].into()),
            ..Facts::default()
        };
        item.metadata = Metadata {
            camera_model: Some(CAMERAS[next() % CAMERAS.len()].into()),
            reel: Some(format!("A{:03}", i / 999 + 1)),
            ..Metadata::default()
        };
        item.tags = (0..1 + next() % 2)
            .map(|_| TAGS[next() % TAGS.len()].to_string())
            .collect();
        item.tags.dedup();
        item.label = next()
            .is_multiple_of(7)
            .then(|| ColourLabel::ALL[next() % 5]);
        pool.add_item(bins[i % bins.len()], item).unwrap();
    }
    pool
}

fn media_pool(c: &mut Criterion) {
    let pool = pool();
    let index = SearchIndex::build(&pool);
    eprintln!(
        "media_pool: {} items, {} bins, {} tokens",
        pool.len(),
        pool.bin_count(),
        index.token_count()
    );
    let mut group = c.benchmark_group("media_pool");
    group.sample_size(20);
    group.bench_function("build_index_10k", |b| {
        b.iter(|| black_box(SearchIndex::build(black_box(&pool))))
    });
    for (name, text) in [
        ("prefix", "harb"),
        ("exact", "interview"),
        ("typo", "intervew"),
        ("two_words", "harbour golden"),
        ("clip_code", "c042"),
        ("single_letter", "h"),
    ] {
        group.bench_function(format!("search_10k_{name}"), |b| {
            b.iter(|| black_box(index.search(black_box(text))))
        });
    }
    let filtered = Query::parse("type:video label:amber tag:wide harbour", &pool);
    group.bench_function("query_10k_filters_and_text", |b| {
        b.iter(|| black_box(index.query(&pool, black_box(&filtered), |_| false)))
    });
    let bin_only = Query {
        bin: pool.children(pool.root()).first().map(|b| b.id),
        recursive: true,
        ..Query::default()
    };
    group.bench_function("query_10k_bin_recursive", |b| {
        b.iter(|| black_box(index.query(&pool, black_box(&bin_only), |_| false)))
    });
    let first = pool.items().next().unwrap().id;
    group.bench_function("set_label_one_of_10k", |b| {
        b.iter(|| {
            let mut edited = pool.clone();
            edited
                .set_label(&[first], Some(ColourLabel::Violet))
                .unwrap();
            black_box(edited)
        })
    });
    let mut labelled = pool.clone();
    labelled
        .set_label(&[first], Some(ColourLabel::Violet))
        .unwrap();
    let mut following = SearchIndex::build(&pool);
    let mut flip = false;
    group.bench_function("refresh_after_one_label_edit_10k", |b| {
        b.iter(|| {
            flip = !flip;
            let target = if flip { &labelled } else { &pool };
            assert!(following.refresh(black_box(target)));
        })
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    targets = media_pool
}
criterion_main!(benches);
