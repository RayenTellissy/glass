//! End to end on macOS: VideoToolbox decode in `media`, zero-copy IOSurface import and
//! the Y'CbCr to RGB pass in `render`, checked against the software decode of the same
//! frame converted with swscale, plus hardware throughput. Skips without fixtures or a
//! GPU. The M2 here stands in for the M1 reference machine.

#![cfg(target_os = "macos")]

mod common;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use common::timing::{Samples, Stat};
use common::{diff, fixture, gpu, nth_frame, read_rgba8, target};

use media::{
    DecodePath, DecoderOptions, FrameReader, FrameStorage, HwApi, HwPreference, StreamSelect,
    VideoFrame,
};
use render::interop::SurfaceImporter;
use render::wgpu;
use render::yuv::{YuvParams, YuvPlanes};

/// Decodes frame `n` in hardware, imports it without a copy, converts it on the GPU and
/// compares it with the software decode of the same frame converted by swscale.
fn compare_with_software(file: &str, n: usize, formats: &[wgpu::TextureFormat]) {
    let Some(path) = fixture(file) else {
        return;
    };
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no Metal adapter");
        return;
    };
    let hw = nth_frame(&path, HwPreference::Require, n);
    let FrameStorage::Hardware(surface) = hw.storage() else {
        panic!("{file}: frame {n} is not a hardware surface");
    };
    assert_eq!(hw.plane_count(), 0, "no CPU planes on a hardware frame");
    let mut importer = SurfaceImporter::default();
    let imported = importer.import(&gpu.device, surface).expect("import");
    assert_eq!(
        imported.key.surface_id,
        surface.io_surface().expect("surface").id(),
        "the textures wrap the decoder's own surface"
    );
    let reference = nth_frame(&path, HwPreference::Off, n)
        .to_rgba8()
        .expect("swscale");
    let size = (surface.width(), surface.height());
    let params = YuvParams::for_frame(surface).encoded();
    for &format in formats {
        let out = target(&gpu, size, format);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        assert!(gpu.converter.encode(
            &gpu.device,
            &mut encoder,
            YuvPlanes::Biplanar {
                luma: &imported.luma,
                chroma: &imported.chroma,
            },
            &params,
            &out.create_view(&Default::default()),
            format,
        ));
        gpu.queue.submit([encoder.finish()]);
        let ours = read_rgba8(&gpu, &out);
        let d = diff(&ours, &reference.data);
        eprintln!(
            "{file} frame {n} ({}, {:?}, {:?} siting) into {format:?}: mean abs difference {:.3}, p99 {}, max {} (8-bit levels)",
            surface.format(),
            params.matrix,
            surface.colour_space().siting,
            d.mean,
            d.p99,
            d.max
        );
        assert!(d.mean < 0.05, "{file}: mean difference {}", d.mean);
        assert!(d.max <= 2, "{file}: max difference {}", d.max);
    }
}

#[test]
fn h264_4k_hardware_frame_matches_software() {
    compare_with_software(
        "h264_4k30.mp4",
        10,
        &[
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba16Float,
        ],
    );
}

#[test]
fn hevc_pq_topleft_hardware_frame_matches_software() {
    compare_with_software(
        "hevc_pq_10bit_640x360.mp4",
        3,
        &[wgpu::TextureFormat::Rgba16Float],
    );
}

#[test]
fn h264_full_range_hardware_frame_matches_software() {
    compare_with_software(
        "h264_fullrange_640x360.mp4",
        3,
        &[wgpu::TextureFormat::Rgba8Unorm],
    );
}

#[test]
fn hevc_10bit_hardware_frame_matches_software() {
    compare_with_software(
        "hevc_10bit_1080p25.mp4",
        10,
        &[
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Rgba8Unorm,
        ],
    );
}

/// The wall-clock time of each frame decoding the whole file `passes` times, with
/// `per_frame` run on each: from the end of the previous frame (or the start) to the end
/// of this one, so the mean is the inverse of the frame rate.
fn decode_times(
    path: &PathBuf,
    options: DecoderOptions,
    passes: usize,
    mut per_frame: impl FnMut(VideoFrame),
) -> Samples {
    let mut reader = FrameReader::open_with(path, StreamSelect::BestVideo, options).expect("open");
    let mut last = Instant::now();
    let mut times = Vec::new();
    for _ in 0..passes {
        while let Some(frame) = reader.next_frame().expect("decode") {
            per_frame(frame.into_video().expect("video"));
            let now = Instant::now();
            times.push((now - last).as_secs_f64() * 1e3);
            last = now;
        }
        reader.seek_to_start().expect("rewind");
    }
    Samples::new(times)
}

/// Frames per second decoding the whole file `passes` times, with `per_frame` run on each.
fn decode_fps(
    path: &PathBuf,
    options: DecoderOptions,
    passes: usize,
    per_frame: impl FnMut(VideoFrame),
) -> f64 {
    1e3 / decode_times(path, options, passes, per_frame).mean()
}

#[test]
fn h264_4k_hardware_throughput() {
    const PASSES: usize = 4;
    const IN_FLIGHT: usize = 3;
    let Some(path) = fixture("h264_4k30.mp4") else {
        return;
    };
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no Metal adapter");
        return;
    };
    let _serial = common::timing::serial();
    let hw = DecoderOptions::with_hw(HwPreference::Require);
    let decode_only = decode_fps(&path, hw, PASSES, |f| assert!(f.is_hardware()));

    // Decode, import and convert every frame, keeping up to IN_FLIGHT conversions queued
    // on the GPU; each keeps its frame (and so its surface) until its submission is done.
    let mut importer = SurfaceImporter::default();
    let size = (3840, 2160);
    let outputs: Vec<wgpu::Texture> = (0..IN_FLIGHT)
        .map(|_| target(&gpu, size, wgpu::TextureFormat::Rgba16Float))
        .collect();
    let views: Vec<wgpu::TextureView> = outputs
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect();
    let mut in_flight = VecDeque::new();
    let mut n = 0usize;
    let full_times = decode_times(&path, hw, PASSES, |frame| {
        let surface = frame.hardware().expect("hardware frame");
        let imported = importer.import(&gpu.device, surface).expect("import");
        if in_flight.len() == IN_FLIGHT {
            let (index, _done): (wgpu::SubmissionIndex, _) =
                in_flight.pop_front().expect("in flight");
            gpu.device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(index),
                    timeout: None,
                })
                .expect("poll");
        }
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        gpu.converter.encode(
            &gpu.device,
            &mut encoder,
            YuvPlanes::Biplanar {
                luma: &imported.luma,
                chroma: &imported.chroma,
            },
            &YuvParams::for_frame(surface),
            &views[n % IN_FLIGHT],
            wgpu::TextureFormat::Rgba16Float,
        );
        let index = gpu.queue.submit([encoder.finish()]);
        in_flight.push_back((index, imported));
        n += 1;
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    in_flight.clear();
    let (hits, misses) = importer.stats();
    let full = 1e3 / full_times.mean();

    let software = decode_fps(&path, DecoderOptions::default(), PASSES, |f| {
        assert!(!f.is_hardware())
    });
    let software_one_thread = decode_fps(
        &path,
        DecoderOptions {
            threads: 1,
            hw: HwPreference::Off,
        },
        1,
        |_| {},
    );
    eprintln!(
        "h264 4K, {} frames: VideoToolbox decode {decode_only:.0} fps; decode + zero-copy import + Rgba16Float conversion {full:.0} fps (surface cache {hits} hits, {misses} misses); software decode {software:.0} fps with a thread per core, {software_one_thread:.0} fps on one thread",
        n
    );
    assert!(
        misses <= 32,
        "surfaces are wrapped once and reused ({misses} misses)"
    );
    // 60 fps: the mean frame time (the frame rate) under `GLASS_STRICT_TIMING=1`, the
    // median otherwise, by the shared timing rules (`common::timing`).
    common::timing::assert_wall_within(
        "4K hardware decode, import and conversion per frame",
        &full_times,
        Stat::Mean,
        1e3 / 60.0,
    );
}

#[test]
fn hardware_path_has_no_cpu_download() {
    // The media crate's sources never call av_hwframe_transfer_data (the CPU download of
    // a hardware frame), so a hardware frame cannot silently turn into a copy.
    for source in [
        include_str!("../../media/src/ffi.rs"),
        include_str!("../../media/src/decode.rs"),
        include_str!("../../media/src/reader.rs"),
        include_str!("../../media/src/hw.rs"),
        include_str!("../../media/src/convert.rs"),
    ] {
        assert!(!source.contains("av_hwframe_transfer_data"));
        assert!(!source.contains("av_hwframe_map"));
    }
    let Some(path) = fixture("h264_1080p30.mp4") else {
        return;
    };
    let reader = FrameReader::open_with(
        &path,
        StreamSelect::BestVideo,
        DecoderOptions::with_hw(HwPreference::Auto),
    )
    .expect("open");
    assert_eq!(
        reader.decode_path(),
        &DecodePath::Hardware {
            api: HwApi::VideoToolbox
        }
    );
    for frame in reader {
        let frame = frame.expect("decode").into_video().expect("video");
        assert!(matches!(frame.storage(), FrameStorage::Hardware(_)));
    }
}
