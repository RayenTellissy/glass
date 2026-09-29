//! The GPU Y'CbCr conversion of software-decoded frames against swscale with the same
//! chroma siting (and against a deliberately wrong siting, to show siting is honoured),
//! and linear-light output on the known bar patches of the PQ, HLG and full-range
//! fixtures, for software frames and (on macOS) VideoToolbox surfaces. Skips without
//! fixtures or a GPU with 16-bit normalised textures.

mod common;

use common::{Gpu, convert, diff, fixture, gpu, nth_frame, read_rgba, read_rgba8};
use media::{ChromaSiting, HwPreference, TransferFn, VideoFrame};
use render::upload::upload_frame;
use render::wgpu;
use render::yuv::{YuvOutput, YuvParams};

/// GPU conversion of a software frame (encoded R'G'B') read back as 8-bit RGBA.
fn gpu_encoded(gpu: &Gpu, frame: &VideoFrame, siting: Option<ChromaSiting>) -> Vec<u8> {
    let uploaded = upload_frame(&gpu.device, &gpu.queue, frame).expect("upload");
    let mut params = uploaded.params.encoded();
    if let Some(siting) = siting {
        params.chroma_offset = siting.offset(params.layout.chroma_shift);
    }
    let out = convert(
        gpu,
        uploaded.planes(),
        &params,
        (frame.width(), frame.height()),
        wgpu::TextureFormat::Rgba16Float,
    );
    read_rgba8(gpu, &out)
}

/// Frame 10 of `file`: GPU against swscale with the frame's own siting, then the GPU
/// sampling centred (the converter before siting was honoured) against the same swscale
/// output. Returns the two mean differences.
fn compare_siting(file: &str) -> Option<(f64, f64)> {
    let path = fixture(file)?;
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter with 16-bit normalised textures");
        return None;
    };
    let frame = nth_frame(&path, HwPreference::Off, 10);
    let colour = frame.colour_space();
    let reference = frame.to_rgba8().expect("swscale");
    let sited = diff(&gpu_encoded(&gpu, &frame, None), &reference.data);
    let centred = diff(
        &gpu_encoded(&gpu, &frame, Some(ChromaSiting::Center)),
        &reference.data,
    );
    eprintln!(
        "{file} ({}, {:?}, {:?} siting): GPU vs swscale with matching siting: mean {:.4}, p99 {}, max {}; GPU centred vs the same: mean {:.4}, p99 {}, max {}",
        frame.format(),
        colour.matrix,
        colour.siting,
        sited.mean,
        sited.p99,
        sited.max,
        centred.mean,
        centred.p99,
        centred.max
    );
    assert!(sited.mean < 0.05, "{file}: mean difference {}", sited.mean);
    assert!(sited.max <= 2, "{file}: max difference {}", sited.max);
    Some((sited.mean, centred.mean))
}

#[test]
fn software_frames_match_swscale_with_their_siting() {
    for file in [
        "h264_1080p30.mp4",
        "h264_10bit_720p30.mp4",
        "hevc_10bit_1080p25.mp4",
        "prores422_1080p23976.mov",
        "vp9_720p30.webm",
        "hevc_pq_10bit_640x360.mp4",
        "h264_fullrange_640x360.mp4",
    ] {
        compare_siting(file);
    }
}

#[test]
fn siting_changes_the_result_on_detailed_content() {
    // testsrc2 has sharp coloured edges, where a quarter-sample chroma shift shows.
    let Some((sited, centred)) = compare_siting("h264_1080p30.mp4") else {
        return;
    };
    assert!(
        centred > sited * 2.0,
        "centred sampling ({centred:.4}) should differ from swscale's left siting more than matched sampling ({sited:.4})"
    );
}

/// Bar centres of the 640-wide bar fixtures.
const BAR_X: [u32; 5] = [64, 192, 320, 448, 576];

/// Linear light at the bar centres, converted on the GPU into Rgba16Float.
fn linear_bars(gpu: &Gpu, frame: &VideoFrame) -> Vec<[f32; 3]> {
    let (w, h) = (frame.width(), frame.height());
    let out = if let Some(surface) = frame.hardware() {
        #[cfg(target_os = "macos")]
        {
            let mut importer = render::interop::SurfaceImporter::default();
            let imported = importer.import(&gpu.device, surface).expect("import");
            let params = YuvParams::for_frame(surface);
            let out = convert(
                gpu,
                render::yuv::YuvPlanes::Biplanar {
                    luma: &imported.luma,
                    chroma: &imported.chroma,
                },
                &params,
                (w, h),
                wgpu::TextureFormat::Rgba16Float,
            );
            gpu.device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("poll");
            out
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = surface;
            unreachable!("hardware frames only exist on macOS so far")
        }
    } else {
        let uploaded = upload_frame(&gpu.device, &gpu.queue, frame).expect("upload");
        assert_eq!(uploaded.params.output, YuvOutput::Linear);
        convert(
            gpu,
            uploaded.planes(),
            &uploaded.params,
            (w, h),
            wgpu::TextureFormat::Rgba16Float,
        )
    };
    let pixels = read_rgba(gpu, &out);
    BAR_X
        .iter()
        .map(|&x| {
            let at = ((h / 2 * w + x) * 4) as usize;
            [pixels[at], pixels[at + 1], pixels[at + 2]]
        })
        .collect()
}

/// Expected linear light of bars given as encoded R'G'B'.
fn expected(transfer: TransferFn, encoded: &[[f64; 3]; 5]) -> Vec<[f64; 3]> {
    encoded.iter().map(|e| transfer.to_linear_rgb(*e)).collect()
}

fn check(label: &str, got: &[[f32; 3]], want: &[[f64; 3]]) {
    eprintln!("{label}: linear {got:?}");
    for (g, w) in got.iter().zip(want) {
        for c in 0..3 {
            let (g, w) = (f64::from(g[c]), w[c]);
            // f16 output keeps about 3 significant digits; codes are rounded to 10 bits.
            let tolerance = 0.01 + 0.015 * w.abs();
            assert!(
                (g - w).abs() <= tolerance,
                "{label}: got {got:?}, want {want:?}"
            );
        }
    }
}

fn ten_bit(code: f64) -> f64 {
    (code - 64.0) / 876.0
}

fn check_fixture(file: &str, hw: HwPreference) {
    let Some(path) = fixture(file) else {
        return;
    };
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no GPU adapter with 16-bit normalised textures");
        return;
    };
    let frame = nth_frame(&path, hw, 0);
    if hw == HwPreference::Require {
        assert!(frame.is_hardware());
    }
    let colour = frame.colour_space();
    let got = linear_bars(&gpu, &frame);
    let label = format!(
        "{file} ({})",
        if frame.is_hardware() {
            "hardware"
        } else {
            "software"
        }
    );
    match colour.transfer {
        TransferFn::Pq | TransferFn::Hlg => {
            let grey = |code: f64| [ten_bit(code); 3];
            let want = expected(
                colour.transfer,
                &[
                    grey(64.0),
                    grey(572.0),
                    grey(721.0),
                    grey(940.0),
                    [0.75, 0.0, 0.0],
                ],
            );
            check(&label, &got, &want);
            if colour.transfer == TransferFn::Pq {
                // BT.2408 reference white (PQ 58%, 203 nits) is 1.0; peak white is 49.3.
                assert!((got[1][1] - 1.0).abs() < 0.02, "{label}: {got:?}");
                assert!((got[3][1] - 49.26).abs() < 0.5, "{label}: {got:?}");
            } else {
                // HLG 75% is reference white; 100% is 1000 nits.
                assert!((got[2][1] - 1.0).abs() < 0.02, "{label}: {got:?}");
                assert!((got[3][1] - 4.926).abs() < 0.08, "{label}: {got:?}");
            }
        }
        _ => {
            let grey = |code: f64| [code / 255.0; 3];
            let want = expected(
                colour.transfer,
                &[
                    grey(0.0),
                    grey(191.0),
                    grey(255.0),
                    [0.75, 0.0, 0.0],
                    grey(128.0),
                ],
            );
            check(&label, &got, &want);
            // 100% white is 1.0 and the 75% bar is 0.75^2.4 under BT.1886.
            assert!((got[2][0] - 1.0).abs() < 0.005, "{label}: {got:?}");
            assert!(
                (got[1][0] - 0.75f32.powf(2.4)).abs() < 0.01,
                "{label}: {got:?}"
            );
        }
    }
}

#[test]
fn pq_bars_to_linear_light() {
    check_fixture("hevc_pq_10bit_640x360.mp4", HwPreference::Off);
}

#[test]
fn hlg_bars_to_linear_light() {
    check_fixture("hevc_hlg_10bit_640x360.mp4", HwPreference::Off);
}

#[test]
fn full_range_bars_to_linear_light() {
    check_fixture("h264_fullrange_640x360.mp4", HwPreference::Off);
}

#[cfg(target_os = "macos")]
#[test]
fn hardware_bars_to_linear_light() {
    for file in [
        "hevc_pq_10bit_640x360.mp4",
        "hevc_hlg_10bit_640x360.mp4",
        "h264_fullrange_640x360.mp4",
    ] {
        check_fixture(file, HwPreference::Require);
    }
}
