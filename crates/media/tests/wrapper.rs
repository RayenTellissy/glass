//! The safe FFmpeg wrapper on the generated fixtures: demux, decode, flush, seek, errors.

mod common;

use media::{
    Decoder, DecoderOptions, Demuxer, Frame, MediaError, MediaKind, Rational, Received, SampleType,
    Timestamp,
};

/// Decodes every frame of one stream, calling `each` per frame.
fn decode_all(demuxer: &mut Demuxer, decoder: &mut Decoder, mut each: impl FnMut(Frame)) {
    while let Some(packet) = demuxer.read_packet().expect("read packet") {
        decoder.send_packet(&packet).expect("send packet");
        while let Received::Frame(frame) = decoder.receive_frame().expect("receive") {
            each(frame);
        }
    }
    decoder.send_eof().expect("send eof");
    loop {
        match decoder.receive_frame().expect("receive") {
            Received::Frame(frame) => each(frame),
            Received::Finished => break,
            Received::NeedsInput => panic!("decoder asked for input after end of stream"),
        }
    }
}

#[test]
fn demuxer_lists_streams_and_reads_packets() {
    let Some(path) = common::fixture("h264_1080p30.mp4") else {
        return;
    };
    let mut demuxer = Demuxer::open(&path).expect("open");
    assert!(
        demuxer.container().contains("mp4"),
        "{}",
        demuxer.container()
    );
    let video = demuxer.best_stream(MediaKind::Video).expect("video stream");
    let audio = demuxer.best_stream(MediaKind::Audio).expect("audio stream");
    let v = demuxer.stream(video).expect("stream").clone();
    assert_eq!(v.kind, MediaKind::Video);
    assert_eq!(v.codec, "h264");
    assert!(v.time_base.is_valid());
    assert_eq!(v.frames, Some(90));
    assert_eq!(demuxer.stream(audio).expect("stream").codec, "aac");
    let duration = demuxer.duration().expect("duration").seconds();
    assert!((duration - 3.0).abs() < 0.1, "duration {duration}");

    let (mut video_packets, mut keys, mut audio_packets) = (0, 0, 0);
    while let Some(packet) = demuxer.read_packet().expect("read") {
        assert!(packet.size() > 0 && packet.data().len() == packet.size());
        if packet.stream_index() == video {
            video_packets += 1;
            keys += usize::from(packet.is_key());
            assert_eq!(packet.time_base(), v.time_base);
            assert!(packet.pts().is_some() && packet.dts().is_some());
        } else if packet.stream_index() == audio {
            audio_packets += 1;
        }
    }
    assert_eq!(video_packets, 90);
    // -g 30 at 30 fps: a keyframe every second.
    assert_eq!(keys, 3);
    assert!(audio_packets > 100, "{audio_packets} audio packets");
    assert!(demuxer.read_packet().expect("read after end").is_none());
}

#[test]
fn decodes_video_frames_with_planes() {
    let Some(path) = common::fixture("h264_1080p30.mp4") else {
        return;
    };
    let mut demuxer = Demuxer::open(&path).expect("open");
    let index = demuxer.best_stream(MediaKind::Video).expect("video");
    let mut decoder = Decoder::open(&demuxer, index).expect("decoder");
    assert_eq!(decoder.kind(), MediaKind::Video);
    let mut pts = Vec::new();
    decode_all(&mut demuxer, &mut decoder, |frame| {
        let Frame::Video(f) = frame else {
            panic!("audio frame from a video decoder");
        };
        if pts.is_empty() {
            assert_eq!((f.width(), f.height()), (1920, 1080));
            assert_eq!(f.format().name(), "yuv420p");
            assert!(f.is_key());
            let planes = f.planes();
            assert_eq!(planes.len(), 3);
            assert_eq!((planes[0].width, planes[0].height), (1920, 1080));
            assert_eq!((planes[1].width, planes[1].height), (960, 540));
            for p in &planes {
                assert!(p.stride >= p.width as usize);
                assert_eq!(p.data.len(), p.stride * p.height as usize);
            }
            assert!(f.plane(3).is_none());
        }
        pts.push(f.pts().expect("pts"));
    });
    assert_eq!(pts.len(), 90);
    assert_eq!(pts[0].seconds(), 0.0);
    let step = Rational::new(1, 30);
    for (n, t) in pts.iter().enumerate() {
        let expected = Timestamp::new(n as i64, step);
        assert_eq!(t.cmp_time(expected), std::cmp::Ordering::Equal, "frame {n}");
    }
}

#[test]
fn decodes_audio_in_native_format() {
    let Some(path) = common::fixture("h264_1080p30.mp4") else {
        return;
    };
    let mut demuxer = Demuxer::open(&path).expect("open");
    let index = demuxer.best_stream(MediaKind::Audio).expect("audio");
    let mut decoder = Decoder::open(&demuxer, index).expect("decoder");
    assert_eq!(decoder.kind(), MediaKind::Audio);
    let mut samples = 0;
    decode_all(&mut demuxer, &mut decoder, |frame| {
        let f = frame.into_audio().expect("audio frame");
        let format = f.format().expect("format");
        // FFmpeg's AAC decoder outputs planar float.
        assert_eq!(format.sample, SampleType::F32);
        assert!(format.planar);
        assert_eq!(f.channels(), 2);
        assert_eq!(f.channel_layout().as_deref(), Some("stereo"));
        assert_eq!(f.sample_rate(), 48000);
        assert_eq!(f.plane_count(), 2);
        let plane = f.plane(1).expect("second channel");
        assert_eq!(plane.len(), f.samples() * 4);
        assert!(f.plane(2).is_none());
        samples += f.samples();
    });
    let seconds = samples as f64 / 48000.0;
    assert!((seconds - 3.0).abs() < 0.05, "{seconds} s of audio");
}

#[test]
fn seek_then_flush_resumes_at_a_keyframe() {
    let Some(path) = common::fixture("h264_1080p30.mp4") else {
        return;
    };
    let mut demuxer = Demuxer::open(&path).expect("open");
    let index = demuxer.best_stream(MediaKind::Video).expect("video");
    let mut decoder =
        Decoder::open_with(&demuxer, index, DecoderOptions { threads: 2 }).expect("decoder");
    // Decode a few frames, then jump.
    let mut got = 0;
    while got < 5 {
        let packet = demuxer.read_packet().expect("read").expect("packet");
        decoder.send_packet(&packet).expect("send");
        while let Received::Frame(_) = decoder.receive_frame().expect("receive") {
            got += 1;
        }
    }
    let target = Timestamp::from_seconds(1.5, Rational::MICROS);
    demuxer.seek(target).expect("seek");
    decoder.flush();
    let first = loop {
        let packet = demuxer.read_packet().expect("read").expect("packet");
        decoder.send_packet(&packet).expect("send");
        if let Received::Frame(f) = decoder.receive_frame().expect("receive") {
            break f;
        }
    };
    let t = first.pts().expect("pts").seconds();
    // The keyframe at or before 1.5 s is the one at 1.0 s.
    assert!((t - 1.0).abs() < 1e-6, "first frame after seek at {t} s");

    demuxer.rewind().expect("rewind");
    decoder.flush();
    let first = loop {
        let packet = demuxer.read_packet().expect("read").expect("packet");
        decoder.send_packet(&packet).expect("send");
        if let Received::Frame(f) = decoder.receive_frame().expect("receive") {
            break f;
        }
    };
    assert_eq!(first.pts().expect("pts").value, 0);
}

#[test]
fn errors_name_the_file_and_ffmpeg_code() {
    let missing = common::generated_dir().join("does-not-exist.mp4");
    let error = Demuxer::open(&missing).expect_err("missing file");
    let MediaError::Open { path, source } = &error else {
        panic!("unexpected error {error:?}");
    };
    assert_eq!(path, &missing);
    assert!(source.code < 0);
    let text = error.to_string();
    assert!(text.contains("does-not-exist.mp4"), "{text}");
    assert!(text.contains("FFmpeg error"), "{text}");
    assert_eq!(error.ffmpeg().map(|e| e.code), Some(source.code));

    let Some(path) = common::fixture("h264_4k30.mp4") else {
        return;
    };
    let demuxer = Demuxer::open(&path).expect("open");
    let error = Decoder::open(&demuxer, 7).expect_err("no stream 7");
    assert!(
        matches!(error, MediaError::NoSuchStream { index: 7, .. }),
        "{error:?}"
    );
}

#[test]
fn not_a_media_file() {
    let Some(path) = common::fixture("MANIFEST.json") else {
        return;
    };
    let error = Demuxer::open(&path).expect_err("JSON is not media");
    assert!(matches!(error, MediaError::Open { .. }), "{error:?}");
}
