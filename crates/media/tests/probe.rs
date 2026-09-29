//! `probe` against every entry of the generated fixture manifest.

mod common;

use media::{Hdr, MediaKind, Rational, StreamDetail, VfrSource, probe};

fn parse_rate(text: &str) -> Rational {
    let (n, d) = text.split_once('/').expect("rate is n/d");
    Rational::new(
        n.parse().expect("numerator"),
        d.parse().expect("denominator"),
    )
    .reduced()
}

#[test]
fn every_manifest_entry_matches() {
    let Some(files) = common::manifest() else {
        return;
    };
    let mut checked = 0;
    for entry in &files {
        let file = entry["file"].as_str().expect("file");
        let kind = entry["kind"].as_str().expect("kind");
        let path = common::generated_dir().join(file);
        let info = probe(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
        let what = |field: &str| format!("{file}: {field}");

        if kind != "audio" {
            let (_, v) = info
                .video()
                .unwrap_or_else(|| panic!("{file}: no video stream"));
            let (stream, _) = info.video().expect("video");
            assert_eq!(
                stream.codec.name,
                entry["codec"].as_str().expect("codec"),
                "{}",
                what("codec")
            );
            assert_eq!(
                u64::from(v.width),
                entry["width"].as_u64().expect("width"),
                "{}",
                what("width")
            );
            assert_eq!(
                u64::from(v.height),
                entry["height"].as_u64().expect("height"),
                "{}",
                what("height")
            );
            assert_eq!(
                v.pixel_format.map(|p| p.name()),
                entry["pix_fmt"].as_str(),
                "{}",
                what("pixel format")
            );
            assert_eq!(
                v.vfr.is_vfr,
                entry["vfr"].as_bool().expect("vfr"),
                "{}: {:?}",
                what("vfr"),
                v.vfr
            );
            // Image sequences take their rate from the project, not the file; FFmpeg's
            // image2 default (25) is not what the manifest records.
            if kind == "video"
                && let Some(rate) = entry["frame_rate"].as_str()
            {
                let expected = parse_rate(rate);
                assert_eq!(
                    v.real_frame_rate,
                    Some(expected),
                    "{}",
                    what("r_frame_rate")
                );
                if !v.vfr.is_vfr {
                    assert_eq!(v.frame_rate, Some(expected), "{}", what("frame rate"));
                    assert_ne!(
                        v.vfr.source,
                        VfrSource::NotChecked,
                        "{}",
                        what("vfr source")
                    );
                }
            }
            assert_eq!(
                u64::from(v.rotation),
                entry["rotation"].as_u64().unwrap_or(0),
                "{}",
                what("rotation")
            );
            assert_eq!(
                info.timecode.as_deref(),
                entry["timecode"].as_str(),
                "{}",
                what("timecode")
            );
            assert!(v.bit_depth >= 8, "{}", what("bit depth"));
        }

        if kind != "image_sequence"
            && let Some(expected) = entry["duration"].as_f64()
        {
            let d = info.duration.expect("duration").seconds();
            assert!(
                (d - expected).abs() <= 0.1,
                "{}: {d} s, expected {expected} s",
                what("duration")
            );
        }

        match entry["audio"].as_object() {
            Some(a) => {
                let (stream, audio) = info.audio().unwrap_or_else(|| panic!("{file}: no audio"));
                assert_eq!(
                    Some(stream.codec.name),
                    a["codec"].as_str(),
                    "{}",
                    what("audio codec")
                );
                assert_eq!(
                    Some(u64::from(audio.sample_rate)),
                    a["sample_rate"].as_u64(),
                    "{}",
                    what("sample rate")
                );
                assert_eq!(
                    Some(u64::from(audio.channels)),
                    a["channels"].as_u64(),
                    "{}",
                    what("channels")
                );
                assert!(audio.sample_format.is_some(), "{}", what("sample format"));
            }
            None => assert!(info.audio().is_none(), "{}", what("unexpected audio")),
        }
        checked += 1;
    }
    assert_eq!(checked, files.len());
}

#[test]
fn colour_profile_and_depth_details() {
    let Some(path) = common::fixture("hevc_10bit_1080p25.mp4") else {
        return;
    };
    let info = probe(&path).expect("probe");
    let (stream, v) = info.video().expect("video");
    assert_eq!(stream.codec.profile, Some("Main 10"));
    assert_eq!(stream.codec.fourcc.as_deref(), Some("hvc1"));
    assert!(stream.codec.level.is_some());
    assert_eq!(v.bit_depth, 10);
    assert_eq!(v.pixel_format.map(|p| p.bit_depth()), Some(10));
    assert_eq!(v.display_aspect_ratio, Rational::new(16, 9));
    assert_eq!(v.hdr, Hdr::None);
    assert_eq!(v.vfr.source, VfrSource::ContainerIndex);

    let Some(path) = common::fixture("prores422_1080p23976.mov") else {
        return;
    };
    let info = probe(&path).expect("probe");
    let (stream, v) = info.video().expect("video");
    assert_eq!(stream.codec.fourcc.as_deref(), Some("apcn"));
    assert_eq!(v.frame_rate, Some(Rational::new(24000, 1001)));
    let (a_stream, a) = info.audio().expect("audio");
    assert_eq!(a_stream.codec.name, "pcm_s24le");
    assert_eq!(a.bit_depth, Some(24));
    assert_eq!(a.channel_layout.as_deref(), Some("stereo"));

    let Some(path) = common::fixture("vp9_720p30.webm") else {
        return;
    };
    let info = probe(&path).expect("probe");
    // Matroska indexes keyframes only, so the check reads packets instead.
    assert_eq!(
        info.video().expect("video").1.vfr.source,
        VfrSource::PacketScan
    );
}

#[test]
fn rotation_and_timecode() {
    let Some(path) = common::fixture("rotated_timecode_h264_640x360_25.mov") else {
        return;
    };
    let info = probe(&path).expect("probe");
    let (_, v) = info.video().expect("video");
    assert_eq!(v.rotation, 90);
    assert!(!v.mirrored);
    assert_eq!((v.width, v.height), (640, 360));
    assert_eq!(v.display_size(), (360, 640));
    assert_eq!(info.timecode.as_deref(), Some("01:02:03:04"));
    let tmcd = info
        .streams
        .iter()
        .find(|s| s.codec.fourcc.as_deref() == Some("tmcd"))
        .expect("tmcd track");
    assert_eq!(tmcd.kind, MediaKind::Data);
    assert!(matches!(tmcd.detail, StreamDetail::Other));
}

#[test]
fn vfr_evidence() {
    let Some(path) = common::fixture("vfr_h264_720p.mp4") else {
        return;
    };
    let info = probe(&path).expect("probe");
    let (_, v) = info.video().expect("video");
    assert!(v.vfr.is_vfr);
    let min = v.vfr.min_interval.expect("min").seconds();
    let max = v.vfr.max_interval.expect("max").seconds();
    // Frames 3 to 5 of every 10 are dropped: intervals of 1/30 s and 4/30 s.
    assert!((min - 1.0 / 30.0).abs() < 1e-3, "min interval {min}");
    assert!((max - 4.0 / 30.0).abs() < 1e-3, "max interval {max}");
    // VFR streams use the average rate: 7 of every 10 frames at 30 fps.
    let avg = v.frame_rate.and_then(|r| r.to_f64()).expect("rate");
    assert!((avg - 21.0).abs() < 0.5, "average rate {avg}");
}

#[test]
fn tags_and_file_facts() {
    let Some(path) = common::fixture("h264_1080p30.mp4") else {
        return;
    };
    let info = probe(&path).expect("probe");
    assert!(info.container.contains("mp4"));
    assert!(info.size_bytes.is_some_and(|s| s > 0));
    assert!(info.bit_rate.is_some());
    assert!(info.tag("encoder").is_some_and(|e| e.starts_with("Lavf")));
    let (stream, _) = info.video().expect("video");
    assert_eq!(stream.codec.profile, Some("High"));
    assert_eq!(stream.frames, Some(90));
    assert!(stream.codec.long_name.is_some());
}
