//! Colour descriptions on decoded frames: tagged files carry their tags, untagged files get
//! the documented defaults, hardware frames carry the same description as software ones,
//! and the bar fixtures hold the codes `generate.sh` wrote and convert to the expected
//! R'G'B'. Skips without fixtures.

mod common;

use media::colour::Defaulted;
use media::{
    ChromaSiting, ColourSpace, DecoderOptions, FrameReader, Hdr, HwPreference, Matrix, Primaries,
    Range, StreamDetail, StreamSelect, TransferFn, VideoFrame,
};

fn first_frame(file: &str, hw: HwPreference) -> Option<VideoFrame> {
    let path = common::fixture(file)?;
    let mut reader =
        FrameReader::open_with(&path, StreamSelect::BestVideo, DecoderOptions::with_hw(hw))
            .expect("open");
    Some(
        reader
            .next_frame()
            .expect("decode")
            .expect("a frame")
            .into_video()
            .expect("video"),
    )
}

fn probed(file: &str) -> Option<ColourSpace> {
    let info = media::probe(common::fixture(file)?).expect("probe");
    info.streams.iter().find_map(|s| match &s.detail {
        StreamDetail::Video(v) => Some(v.colour_space),
        _ => None,
    })
}

const PQ: &str = "hevc_pq_10bit_640x360.mp4";
const HLG: &str = "hevc_hlg_10bit_640x360.mp4";
const FULL: &str = "h264_fullrange_640x360.mp4";

/// Bar centres of the 640-wide bar fixtures (five bars of 128 pixels).
const BAR_X: [u32; 5] = [64, 192, 320, 448, 576];

#[test]
fn pq_flags_are_carried() {
    let Some(frame) = first_frame(PQ, HwPreference::Off) else {
        return;
    };
    let c = frame.colour_space();
    assert_eq!(
        (c.primaries, c.transfer, c.matrix, c.range, c.siting),
        (
            Primaries::Bt2020,
            TransferFn::Pq,
            Matrix::Bt2020,
            Range::Limited,
            ChromaSiting::TopLeft
        )
    );
    assert_eq!((c.bit_depth, c.hdr), (10, Hdr::Pq));
    assert_eq!(c.defaulted, Defaulted::default(), "everything is tagged");
    assert_eq!(probed(PQ).map(|p| p.hdr), Some(Hdr::Pq));
}

#[test]
fn hlg_flags_are_carried() {
    let Some(frame) = first_frame(HLG, HwPreference::Off) else {
        return;
    };
    let c = frame.colour_space();
    assert_eq!(
        (c.primaries, c.transfer, c.matrix, c.range),
        (
            Primaries::Bt2020,
            TransferFn::Hlg,
            Matrix::Bt2020,
            Range::Limited
        )
    );
    // HEVC's default chroma location is type 0, which FFmpeg reports as left.
    assert_eq!(c.siting, ChromaSiting::Left);
    assert_eq!((c.bit_depth, c.hdr), (10, Hdr::Hlg));
}

#[test]
fn full_range_flags_are_carried() {
    let Some(frame) = first_frame(FULL, HwPreference::Off) else {
        return;
    };
    let c = frame.colour_space();
    assert_eq!(
        (c.primaries, c.transfer, c.matrix, c.range, c.bit_depth),
        (
            Primaries::Bt709,
            TransferFn::Bt1886,
            Matrix::Bt709,
            Range::Full,
            8
        )
    );
    assert_eq!(probed(FULL).map(|p| p.range), Some(Range::Full));
}

#[test]
fn untagged_files_get_the_defaults() {
    // x264 writes no colour tags: BT.709 limited range with the decoder's left siting.
    if let Some(frame) = first_frame("h264_1080p30.mp4", HwPreference::Off) {
        let c = frame.colour_space();
        assert_eq!(
            (c.primaries, c.transfer, c.matrix, c.range, c.siting),
            (
                Primaries::Bt709,
                TransferFn::Bt1886,
                Matrix::Bt709,
                Range::Limited,
                ChromaSiting::Left
            )
        );
        assert!(c.defaulted.matrix && c.defaulted.primaries && c.defaulted.transfer);
        assert!(c.defaulted.range);
    }
    // PNG: RGB, so identity matrix, full range, sRGB.
    if let Some(frame) = first_frame("still_1080p.png", HwPreference::Off) {
        let c = frame.colour_space();
        assert_eq!(
            (c.matrix, c.range, c.transfer, c.primaries),
            (
                Matrix::Identity,
                Range::Full,
                TransferFn::Srgb,
                Primaries::Bt709
            )
        );
    }
    // JPEG: FFmpeg tags BT.601 full range and centred chroma; the transfer is sRGB.
    if let Some(frame) = first_frame("still_1080p.jpg", HwPreference::Off) {
        let c = frame.colour_space();
        assert_eq!(
            (c.matrix, c.range, c.siting, c.transfer),
            (
                Matrix::Bt601,
                Range::Full,
                ChromaSiting::Center,
                TransferFn::Srgb
            )
        );
    }
    // 10-bit 4:2:2 ProRes: untagged matrix at 1080 lines is BT.709; 10-bit is carried.
    if let Some(frame) = first_frame("prores422_1080p23976.mov", HwPreference::Off) {
        let c = frame.colour_space();
        assert_eq!((c.matrix, c.bit_depth), (Matrix::Bt709, 10));
    }
}

/// Luma codes at the bar centres, from the first CPU plane.
fn luma_codes(frame: &VideoFrame) -> Vec<u32> {
    let plane = frame.plane(0).expect("luma plane");
    let wide = frame.format().bit_depth() > 8;
    let y = frame.height() as usize / 2;
    BAR_X
        .iter()
        .map(|&x| {
            let at = y * plane.stride + x as usize * if wide { 2 } else { 1 };
            if wide {
                u32::from(u16::from_le_bytes([plane.data[at], plane.data[at + 1]]))
            } else {
                u32::from(plane.data[at])
            }
        })
        .collect()
}

fn assert_codes(file: &str, got: &[u32], want: &[u32], tolerance: u32) {
    for (g, w) in got.iter().zip(want) {
        assert!(
            g.abs_diff(*w) <= tolerance,
            "{file}: codes {got:?}, want {want:?}"
        );
    }
}

fn assert_rgb(file: &str, frame: &VideoFrame, want: &[[u8; 3]; 5]) {
    let rgba = frame.to_rgba8().expect("rgba");
    for (x, want) in BAR_X.iter().zip(want) {
        let px = rgba.pixel(*x, rgba.height / 2).expect("pixel");
        for c in 0..3 {
            assert!(
                px[c].abs_diff(want[c]) <= 2,
                "{file} at x {x}: {px:?}, want {want:?}"
            );
        }
    }
}

#[test]
fn bar_fixtures_hold_their_codes_and_convert() {
    for file in [PQ, HLG] {
        let Some(frame) = first_frame(file, HwPreference::Off) else {
            return;
        };
        assert_codes(file, &luma_codes(&frame), &[64, 572, 721, 940, 237], 2);
        // Encoded R'G'B': black, 58%, 75%, 100% and BT.2020 75% red.
        assert_rgb(
            file,
            &frame,
            &[
                [0, 0, 0],
                [148, 148, 148],
                [191, 191, 191],
                [255, 255, 255],
                [191, 0, 0],
            ],
        );
    }
    let Some(frame) = first_frame(FULL, HwPreference::Off) else {
        return;
    };
    assert_codes(FULL, &luma_codes(&frame), &[0, 191, 255, 41, 128], 1);
    assert_rgb(
        FULL,
        &frame,
        &[
            [0, 0, 0],
            [191, 191, 191],
            [255, 255, 255],
            [191, 0, 0],
            [128, 128, 128],
        ],
    );
}

#[cfg(target_os = "macos")]
#[test]
fn hardware_frames_carry_the_same_description() {
    for file in [PQ, HLG, FULL, "h264_1080p30.mp4"] {
        let (Some(sw), Some(hw)) = (
            first_frame(file, HwPreference::Off),
            first_frame(file, HwPreference::Require),
        ) else {
            return;
        };
        let surface = hw.hardware().expect("a hardware frame");
        assert_eq!(surface.colour_space(), hw.colour_space());
        assert_eq!(hw.colour_space(), sw.colour_space(), "{file}");
    }
}
