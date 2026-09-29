//! Hardware decode handle production: VideoToolbox frames come out as retained
//! `CVPixelBuffer`/`IOSurface` handles with no CPU planes. macOS only; the GPU import
//! half lives in `render::interop` and the end-to-end check in `engine`'s tests.

#![cfg(target_os = "macos")]

mod common;

use media::{
    DecodePath, DecoderOptions, FrameReader, FrameStorage, HwApi, HwPreference, HwSurfaceFormat,
    StreamSelect,
};

fn decode_in_hardware(file: &str, format: HwSurfaceFormat, fourcc: &[u8; 4]) {
    let Some(path) = common::fixture(file) else {
        return;
    };
    let options = DecoderOptions::with_hw(HwPreference::Require);
    let mut reader = FrameReader::open_with(&path, StreamSelect::BestVideo, options).expect("open");
    assert_eq!(
        reader.decode_path(),
        &DecodePath::Hardware {
            api: HwApi::VideoToolbox
        }
    );
    let mut frames = 0u64;
    let mut surfaces = std::collections::HashSet::new();
    while let Some(frame) = reader.next_frame().expect("decode") {
        let frame = frame.into_video().expect("video");
        assert_eq!(frame.plane_count(), 0, "no CPU planes");
        let FrameStorage::Hardware(hw) = frame.storage() else {
            panic!("{file}: frame {frames} is not a hardware surface");
        };
        assert_eq!(hw.api(), HwApi::VideoToolbox);
        assert_eq!(hw.format(), format);
        assert_eq!(&hw.fourcc(), fourcc);
        assert_eq!((hw.width(), hw.height()), (frame.width(), frame.height()));
        let surface = hw.io_surface().expect("IOSurface-backed");
        surfaces.insert(surface.id());
        assert!(frame.to_rgba8().is_err(), "no CPU conversion of a surface");
        frames += 1;
    }
    assert!(reader.is_hardware());
    let software = FrameReader::open(&path, StreamSelect::BestVideo)
        .expect("open")
        .count() as u64;
    assert_eq!(frames, software, "{file}: same frame count as software");
    // VideoToolbox recycles a small pool of surfaces once frames are released.
    assert!(
        surfaces.len() < frames as usize,
        "{file}: {} surfaces for {frames} frames",
        surfaces.len()
    );
    eprintln!("{file}: {frames} frames on {} surfaces", surfaces.len());
}

#[test]
fn h264_4k_decodes_to_nv12_surfaces() {
    decode_in_hardware("h264_4k30.mp4", HwSurfaceFormat::Nv12, b"420v");
}

#[test]
fn hevc_10bit_decodes_to_p010_surfaces() {
    decode_in_hardware("hevc_10bit_1080p25.mp4", HwSurfaceFormat::P010, b"x420");
}

#[test]
fn held_frames_keep_their_surfaces() {
    let Some(path) = common::fixture("h264_1080p30.mp4") else {
        return;
    };
    let options = DecoderOptions::with_hw(HwPreference::Require);
    let reader = FrameReader::open_with(&path, StreamSelect::BestVideo, options).expect("open");
    // Holding every frame forces the pool to hand out a new surface each time; the frames
    // keep their buffers after the reader is gone and release them on drop.
    let frames: Vec<_> = reader
        .map(|f| f.expect("decode").into_video().expect("video"))
        .collect();
    let ids: std::collections::HashSet<u32> = frames
        .iter()
        .map(|f| {
            f.hardware()
                .and_then(|h| h.io_surface())
                .expect("surface")
                .id()
        })
        .collect();
    assert_eq!(ids.len(), frames.len(), "one surface per held frame");
    let clone = frames[0].hardware().expect("hardware").clone();
    drop(frames);
    assert!(clone.io_surface().is_some(), "a cloned handle stays valid");
}
