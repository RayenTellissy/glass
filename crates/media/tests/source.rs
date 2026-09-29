//! `media::source`: stills, animations and image sequences through `VideoSource`, against
//! the `still`, `animation` and `sequence` entries of the fixture manifest. Skips when
//! `tests/fixtures/generate.sh` has not been run.

mod common;

use media::colour::{Primaries, Range, TransferFn};
use media::timemap::FrameTimes;
use media::{
    ImageSequence, Matrix, Rational, SourceKind, SourceOptions, Timestamp, TimestampMap,
    VideoFrame, VideoSource,
};

const PROJECT_RATE: Rational = Rational::new(24, 1);

fn options() -> SourceOptions {
    SourceOptions::new(PROJECT_RATE)
}

fn rgba_at(frame: &VideoFrame, x: u32, y: u32) -> [u8; 4] {
    let image = frame.to_rgba8().expect("to rgba");
    image.pixel(x, y).expect("pixel in range")
}

fn assert_close(label: &str, got: [u8; 4], want: [u8; 3], tolerance: u8) {
    for c in 0..3 {
        assert!(
            got[c].abs_diff(want[c]) <= tolerance,
            "{label}: got {got:?}, want {want:?} (±{tolerance})"
        );
    }
}

fn entries(kind: &str) -> Vec<serde_json::Value> {
    common::manifest()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e["kind"] == kind)
        .collect()
}

/// Checks the red block of the marker pattern and the blue elsewhere.
fn check_marker(label: &str, frame: &VideoFrame, marker: &str, tolerance: u8) {
    let (w, h) = (frame.width(), frame.height());
    let (red, blue) = match marker {
        "top_left" => ((10, 10), (w - 10, h - 10)),
        "top_right" => ((w - 10, 10), (10, h - 10)),
        other => panic!("{label}: unknown marker {other}"),
    };
    assert_close(label, rgba_at(frame, red.0, red.1), [255, 0, 0], tolerance);
    assert_close(
        label,
        rgba_at(frame, blue.0, blue.1),
        [0, 0, 255],
        tolerance,
    );
}

#[test]
fn every_still_decodes() {
    let stills = entries("still");
    for entry in &stills {
        let file = entry["file"].as_str().expect("file");
        let path = common::generated_dir().join(file);
        let mut source =
            VideoSource::open(&path, options()).unwrap_or_else(|e| panic!("{file}: {e}"));
        let info = source.info().clone();
        assert_eq!(info.kind, SourceKind::Still, "{file}");
        let size = (u64::from(info.width), u64::from(info.height));
        assert_eq!(
            size,
            (
                entry["width"].as_u64().expect("width"),
                entry["height"].as_u64().expect("height")
            ),
            "{file}: upright size"
        );
        assert_eq!(
            u64::from(info.orientation),
            entry["orientation"].as_u64().unwrap_or(1),
            "{file}: orientation"
        );
        let timeline = source.timeline().expect("timeline");
        assert!(matches!(timeline.frames, FrameTimes::Still), "{file}");

        let frame = source.next_frame().expect("decode").expect("one frame");
        assert!(
            source.next_frame().expect("decode").is_none(),
            "{file}: one frame only"
        );
        assert_eq!(
            (frame.width(), frame.height()),
            (info.width, info.height),
            "{file}"
        );
        assert_eq!(frame.pts().map(|p| p.value), Some(0), "{file}: pts");

        let colour = frame.colour_space();
        assert_eq!(
            colour, info.colour_space,
            "{file}: frame and info colour agree"
        );
        assert_eq!(colour.matrix, Matrix::Identity, "{file}");
        assert_eq!(colour.range, Range::Full, "{file}");
        assert_eq!(colour.transfer, TransferFn::Srgb, "{file}");
        let primaries = match entry["primaries"].as_str() {
            Some("smpte432") => Primaries::DisplayP3,
            Some(other) => panic!("{file}: unexpected primaries {other}"),
            None => Primaries::Bt709,
        };
        assert_eq!(colour.primaries, primaries, "{file}: primaries");
        if primaries == Primaries::DisplayP3 {
            assert!(
                info.embedded_colour
                    .as_ref()
                    .is_some_and(|c| c.profile == Some(media::source::IccProfile::DisplayP3)),
                "{file}: flagged Display P3: {:?}",
                info.embedded_colour
            );
        }

        let bit_depth = entry["bit_depth"].as_u64().unwrap_or(8);
        let alpha = entry["alpha"].as_bool().unwrap_or(false);
        assert_eq!(u64::from(info.bit_depth), bit_depth, "{file}: bit depth");
        assert_eq!(info.alpha, alpha, "{file}: alpha");
        let expected_format = match (bit_depth > 8, alpha) {
            (false, false) => "gbrp",
            (false, true) => "gbrap",
            (true, false) => "gbrp16le",
            (true, true) => "gbrap16le",
        };
        assert_eq!(
            frame.format().name(),
            expected_format,
            "{file}: pixel format"
        );

        let lossy = matches!(entry["codec"].as_str(), Some("mjpeg" | "hevc" | "av1"));
        if let Some(marker) = entry["marker"].as_str() {
            check_marker(file, &frame, marker, if lossy { 12 } else { 0 });
        }
        if alpha {
            let px = rgba_at(&frame, 5, 5);
            assert_close(file, px, [0x33, 0x66, 0x99], 1);
            assert!(px[3].abs_diff(128) <= 1, "{file}: alpha {}", px[3]);
        }

        // Any time shows the picture, and a map on any rate never runs out.
        let again = source
            .seek_exact(Timestamp::new(3_600_000_000, Rational::MICROS))
            .expect("seek")
            .expect("frame");
        assert_eq!(again.width(), frame.width(), "{file}");
        let map = TimestampMap::new(timeline, Rational::new(30000, 1001));
        assert_eq!(map.len(), u64::MAX);
        assert_eq!(map.source_frame(123_456), Some(0));
    }
    eprintln!("{} stills checked", stills.len());
}

#[test]
fn every_animation_decodes_with_its_delays() {
    let animations = entries("animation");
    for entry in &animations {
        let file = entry["file"].as_str().expect("file");
        let path = common::generated_dir().join(file);
        let mut source =
            VideoSource::open(&path, options()).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!(source.kind(), SourceKind::Animation, "{file}");
        let delays: Vec<i64> = entry["delays_ms"]
            .as_array()
            .expect("delays")
            .iter()
            .map(|d| d.as_i64().expect("delay"))
            .collect();
        let sources: Vec<u64> = entry["source_frames"]
            .as_array()
            .expect("source frames")
            .iter()
            .map(|d| d.as_u64().expect("index"))
            .collect();
        let step = entry["colour_step"].as_u64().unwrap_or(16);
        let frames = entry["frames"].as_u64().expect("frames");
        assert_eq!(source.info().frame_count, frames, "{file}");
        assert_eq!(
            (
                u64::from(source.info().width),
                u64::from(source.info().height)
            ),
            (
                entry["width"].as_u64().expect("width"),
                entry["height"].as_u64().expect("height")
            ),
            "{file}"
        );

        let timeline = source.timeline().expect("timeline");
        assert_eq!(timeline.time_base, Rational::new(1, 1000), "{file}");
        assert_eq!(timeline.frame_count(), frames, "{file}");
        assert_eq!(
            timeline.duration().value,
            delays.iter().sum::<i64>(),
            "{file}: duration"
        );

        let mut expected_pts = 0i64;
        let mut count = 0u64;
        while let Some(frame) = source.next_frame().expect("decode") {
            let k = count as usize;
            assert_eq!(
                frame.pts().map(|p| p.value),
                Some(expected_pts),
                "{file} frame {k}"
            );
            assert_eq!(
                frame.duration().map(|d| d.value),
                Some(delays[k]),
                "{file} frame {k}"
            );
            // geq wraps values past 255.
            let level = (sources[k] * step) as u8;
            assert_close(
                &format!("{file} frame {k}"),
                rgba_at(&frame, 80, 60),
                [level, 255 - level, 128],
                3,
            );
            expected_pts += delays[k];
            count += 1;
        }
        assert_eq!(count, frames, "{file}: frame count");
    }
    eprintln!("{} animations checked", animations.len());
}

fn open_fixture(name: &str) -> Option<VideoSource> {
    let path = common::fixture(name)?;
    Some(VideoSource::open(path, options()).expect("open"))
}

fn red_of(frame: &VideoFrame, x: u32, y: u32) -> u8 {
    rgba_at(frame, x, y)[0]
}

#[test]
fn animated_gif_seeks() {
    let Some(mut gif) = open_fixture("anim_vfr_160x120.gif") else {
        return;
    };
    let ms = |v| Timestamp::new(v, Rational::new(1, 1000));
    // Frames at 0, 100, 200 (400 ms long), 600, 700, ... source frames 0, 1, 2, 6, 7.
    let cases = [
        (250, 200, 2),
        (599, 200, 2),
        (600, 600, 6),
        (0, 0, 0),
        (150, 100, 1),
        (1_300, 1_200, 12),
        (99_000, 1_900, 19),
        (-50, 0, 0),
    ];
    for (target, pts, source) in cases {
        let frame = gif.seek_exact(ms(target)).expect("seek").expect("frame");
        assert_eq!(frame.pts(), Some(ms(pts)), "seek to {target} ms");
        assert!(
            red_of(&frame, 5, 5).abs_diff((source * 16) as u8) <= 3,
            "seek to {target} ms shows source frame {source}"
        );
    }
    // After a seek, frames continue in order.
    gif.seek_exact(ms(250)).expect("seek");
    let next = gif.next_frame().expect("decode").expect("frame");
    assert_eq!(next.pts(), Some(ms(600)));

    // On a 30 fps sequence, sequence frame 8 (266.7 ms) shows the 200 ms frame.
    let map = TimestampMap::new(gif.timeline().expect("timeline"), Rational::new(30, 1));
    assert_eq!(map.len(), 60, "2 s x 30");
    let frame = gif
        .seek_to_sequence_frame(&map, 8)
        .expect("seek")
        .expect("frame");
    assert_eq!(frame.pts(), Some(ms(200)));
}

#[test]
fn sequence_gaps_are_reported_and_held() {
    let Some(first) = common::fixture("sequence_gaps_320x180/shot_0001.png") else {
        return;
    };
    let sequence = ImageSequence::detect(&first)
        .expect("scan")
        .expect("a sequence");
    assert_eq!(
        (sequence.first(), sequence.last(), sequence.len()),
        (1, 12, 9)
    );
    assert_eq!(sequence.gaps(), vec![(5, 6), (10, 10)]);
    assert_eq!(sequence.padding, Some(4));
    assert_eq!(
        ImageSequence::from_pattern(&sequence.pattern()).expect("scan"),
        Some(sequence.clone())
    );
    assert_eq!(
        ImageSequence::in_folder(&sequence.dir).expect("scan"),
        Some(sequence.clone())
    );

    let mut source = VideoSource::open_sequence(sequence, options()).expect("open");
    let info = source.info().clone();
    assert_eq!(info.kind, SourceKind::Sequence);
    assert_eq!((info.width, info.height, info.frame_count), (320, 180, 9));
    assert_eq!(
        info.sequence.as_ref().map(|s| s.gaps()),
        Some(vec![(5, 6), (10, 10)])
    );
    let timeline = source.timeline().expect("timeline");
    assert_eq!(timeline.time_base, Rational::new(1, 24), "project rate");
    assert_eq!(timeline.duration().value, 12);

    let tick = |v| Timestamp::new(v, Rational::new(1, 24));
    let numbers = [1u64, 2, 3, 4, 7, 8, 9, 11, 12];
    for (i, n) in numbers.iter().enumerate() {
        let frame = source.next_frame().expect("decode").expect("frame");
        assert_eq!(frame.pts(), Some(tick(*n as i64 - 1)), "file {n}");
        let level = ((n - 1) * 20) as u8;
        assert_close(
            &format!("file {n}"),
            rgba_at(&frame, 100, 100),
            [level, 0, 255 - level],
            0,
        );
        let held = numbers.get(i + 1).map_or(1, |next| next - n);
        assert_eq!(
            frame.duration().map(|d| d.value),
            Some(held as i64),
            "file {n}"
        );
    }
    assert!(source.next_frame().expect("decode").is_none());

    // Numbers 5 and 6 are missing: their times hold number 4.
    for missing in [4i64, 5] {
        let frame = source
            .seek_exact(tick(missing))
            .expect("seek")
            .expect("frame");
        assert_eq!(frame.pts(), Some(tick(3)), "time of file {}", missing + 1);
        assert_eq!(red_of(&frame, 0, 0), 60);
    }
    let map = TimestampMap::new(timeline, Rational::new(24, 1));
    assert_eq!(map.len(), 12);
    assert_eq!(map.source_frame(9), Some(6), "number 10 holds number 9");
}

#[test]
fn sequence_rate_comes_from_the_caller() {
    let Some(first) = common::fixture("sequence_640x360_24/frame_0001.png") else {
        return;
    };
    let dir = first.parent().expect("dir").to_path_buf();
    // The project rate by default, never FFmpeg's 25.
    let mut at_project =
        VideoSource::open(&dir, SourceOptions::new(Rational::new(30, 1))).expect("open folder");
    let timeline = at_project.timeline().expect("timeline");
    assert_eq!(timeline.frame_count(), 48);
    assert!(matches!(timeline.frames, FrameTimes::Constant { .. }));
    assert!((timeline.duration().seconds() - 1.6).abs() < 1e-9);

    let mut chosen = SourceOptions::new(Rational::new(30, 1));
    chosen.sequence_rate = Some(Rational::new(24, 1));
    let mut at_24 = VideoSource::open(dir.join("frame_%04d.png"), chosen).expect("open pattern");
    assert!((at_24.timeline().expect("timeline").duration().seconds() - 2.0).abs() < 1e-9);
    let frame = at_24
        .seek_exact(Timestamp::new(1, Rational::new(1, 2)))
        .expect("seek")
        .expect("frame");
    assert_eq!(frame.pts(), Some(Timestamp::new(12, Rational::new(1, 24))));
    let map = TimestampMap::new(at_24.timeline().expect("timeline"), PROJECT_RATE);
    assert_eq!(map.len(), 48);
}

#[test]
fn video_files_open_through_the_same_api() {
    let Some(mut video) = open_fixture("h264_1080p30.mp4") else {
        return;
    };
    assert_eq!(video.kind(), SourceKind::Video);
    assert_eq!((video.info().width, video.info().height), (1920, 1080));
    assert!(video.decode_path().is_some());
    let timeline = video.timeline().expect("timeline");
    assert_eq!(timeline.frame_count(), 90);
    let frame = video
        .seek_exact(Timestamp::new(1, Rational::new(1, 1)))
        .expect("seek")
        .expect("frame");
    assert!((frame.pts().expect("pts").seconds() - 1.0).abs() < 1e-6);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn heif_is_unsupported_off_macos() {
    let dir = std::env::temp_dir().join(format!("glass-heif-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("photo.heic");
    std::fs::write(&path, b"\0\0\0\x18ftypheic\0\0\0\0mif1heic").expect("write");
    let error = VideoSource::open(&path, options()).expect_err("unsupported");
    let _ = std::fs::remove_dir_all(&dir);
    match error {
        media::MediaError::Unsupported { reason, .. } => {
            assert_eq!(reason, media::Unsupported::Heif);
            assert_eq!(reason.i18n_key(), "media.unsupported.heif");
            assert!(reason.message().contains("HEIC"));
        }
        other => panic!("expected Unsupported, got {other}"),
    }
}

#[test]
fn unsupported_messages_exist() {
    for reason in [media::Unsupported::Heif, media::Unsupported::Avif] {
        let english = assets::i18n::Locale::english();
        assert!(
            english.get(reason.i18n_key()).is_some(),
            "{}",
            reason.i18n_key()
        );
    }
}
