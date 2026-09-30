//! Every source path lands in the working space: software-decoded Y'CbCr and planar RGB
//! stills, uploaded with `render::upload`, reach the graph through `YuvSourceNode`, which
//! folds the gamut matrix from the frame's primaries into the working primaries into the
//! conversion pass. Each fixture is converted once with the working space in its own
//! primaries (identity gamut) and once into BT.709 or BT.2020; the second must equal the
//! first with the gamut matrix applied on the CPU. Skips without fixtures or a GPU.

mod common;

use std::path::PathBuf;

use media::Primaries;
use render::colour::{WorkingSpace, apply, gamut_matrix};
use render::graph::{Graph, TexturePool};
use render::nodes::YuvSourceNode;
use render::renderer::Renderer;
use render::upload::{UploadedFrame, upload_frame};
use render::wgpu;

fn fixture(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/generated")
        .join(name);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipped {name}: run tests/fixtures/generate.sh");
        None
    }
}

fn to_working(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    frame: &UploadedFrame,
    working: WorkingSpace,
    size: (u32, u32),
) -> Vec<[f32; 4]> {
    let target = common::blank(device, wgpu::TextureFormat::Rgba16Float, size);
    let view = target.create_view(&Default::default());
    let mut g = Graph::new();
    let source = g.add(YuvSourceNode::uploaded(frame, working), &[]);
    g.export(source.out(0), &view);
    g.compile()
        .expect("compiles")
        .execute(renderer, &mut TexturePool::new(), None);
    common::read(device, queue, &target)
}

#[test]
fn every_uploaded_source_is_gamut_converted_into_the_working_space() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    if !device
        .features()
        .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
    {
        eprintln!("skipped: no 16-bit normalised textures");
        return;
    }
    let renderer = Renderer::new(&device, &queue);
    for (file, primaries, format, working) in [
        // Planar RGB still tagged Display P3 (PNG cICP) into an SDR project.
        (
            "still_p3_cicp_640x360.png",
            Primaries::DisplayP3,
            "gbrp",
            WorkingSpace::BT709,
        ),
        // 10-bit Y'CbCr in BT.2020 into an SDR project (gamut is compressed, may go
        // negative) and a BT.709 H.264 frame into an HDR project.
        (
            "hevc_pq_10bit_640x360.mp4",
            Primaries::Bt2020,
            "yuv420p10le",
            WorkingSpace::BT709,
        ),
        (
            "h264_1080p30.mp4",
            Primaries::Bt709,
            "yuv420p",
            WorkingSpace::BT2020,
        ),
    ] {
        let Some(path) = fixture(file) else {
            continue;
        };
        let mut source = media::VideoSource::open(
            &path,
            media::SourceOptions::new(media::Rational::new(30, 1)),
        )
        .expect("open");
        let frame = source.next_frame().expect("decode").expect("frame");
        assert_eq!(frame.format().name(), format, "{file}");
        let uploaded = upload_frame(&device, &queue, &frame).expect("upload");
        assert_eq!(uploaded.primaries, primaries, "{file}");
        let size = (frame.width(), frame.height());
        let native = to_working(
            &device,
            &queue,
            &renderer,
            &uploaded,
            WorkingSpace { primaries },
            size,
        );
        let converted = to_working(&device, &queue, &renderer, &uploaded, working, size);
        let matrix = gamut_matrix(primaries, working.primaries);
        let mut worst = 0f64;
        let mut moved = 0f64;
        for (n, c) in native.iter().zip(&converted) {
            let rgb = [n[0], n[1], n[2]].map(f64::from);
            let want = apply(&matrix, rgb);
            for ch in 0..3 {
                let got = f64::from(c[ch]);
                // f16 relative precision, floored for values near zero.
                let err = (got - want[ch]).abs() / want[ch].abs().max(0.25);
                worst = worst.max(err);
                moved = moved.max((got - rgb[ch]).abs());
            }
            assert_eq!(c[3], 1.0, "{file}: opaque");
        }
        println!(
            "{file} ({format}, {}) into {}: max relative error {worst:.5} against the CPU gamut matrix, largest change {moved:.4}",
            primaries.name(),
            working.primaries.name()
        );
        assert!(worst < 2e-3, "{file}: {worst}");
        assert!(moved > 0.01, "{file}: the gamut conversion changed nothing");
    }
}
