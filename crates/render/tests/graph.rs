//! The render graph: topological compile, culling, validation, aliasing of
//! intermediates (with the VRAM it saves at 4K), pool reuse across frames, per-pass GPU
//! timestamps, and a decoded frame through the source and gamut nodes against the CPU
//! colour maths. GPU tests skip when no adapter exists.

mod common;

use media::colour::Defaulted;
use media::{
    ChromaSiting, ColourSpace, Hdr, HwSurfaceFormat, Matrix, Primaries, Range, TransferFn,
};
use render::colour::{WorkingSpace, apply, gamut_matrix};
use render::graph::{Graph, GraphError, Node, NodeContext, TextureDesc, TexturePool};
use render::interop::plane_formats;
use render::nodes::{GamutNode, YuvSourceNode};
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::wgpu;
use render::yuv::{PlaneLayout, YuvParams, YuvPlanes};

/// A node that only describes itself; compile tests never execute it.
struct Dummy {
    label: &'static str,
    inputs: usize,
    size: (u32, u32),
}

impl Dummy {
    fn new(label: &'static str, inputs: usize, size: (u32, u32)) -> Self {
        Self {
            label,
            inputs,
            size,
        }
    }
}

impl Node for Dummy {
    fn label(&self) -> &str {
        self.label
    }

    fn input_count(&self) -> usize {
        self.inputs
    }

    fn outputs(&self, _inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![TextureDesc::working(self.size.0, self.size.1)]
    }

    fn record(&self, _: &mut NodeContext<'_>, _: &[&wgpu::TextureView], _: &[&wgpu::TextureView]) {}
}

fn target(device: &wgpu::Device, (w, h): (u32, u32)) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = common::blank(device, wgpu::TextureFormat::Rgba16Float, (w, h));
    let view = texture.create_view(&Default::default());
    (texture, view)
}

#[test]
fn nodes_added_out_of_order_are_sorted_and_dead_ones_culled() {
    let Some((device, _queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (_t, view) = target(&device, (8, 8));
    let mut g = Graph::new();
    let sink = g.add_node(Dummy::new("sink", 2, (8, 8)));
    let middle = g.add_node(Dummy::new("middle", 1, (8, 8)));
    let source = g.add_node(Dummy::new("source", 0, (8, 8)));
    let unused = g.add(Dummy::new("unused", 1, (8, 8)), &[source.out(0)]);
    g.connect(source.out(0), middle, 0);
    g.connect(middle.out(0), sink, 0);
    g.connect(source.out(0), sink, 1);
    let _ = unused;
    g.export(sink.out(0), &view);
    let compiled = g.compile().expect("compiles");
    assert_eq!(compiled.pass_labels(), ["source", "middle", "sink"]);
    assert_eq!(compiled.stats().culled, 1);
    assert_eq!(compiled.stats().passes, 3);
}

#[test]
fn invalid_graphs_are_rejected() {
    let Some((device, _queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (_t, view) = target(&device, (8, 8));
    let (_t2, wrong) = target(&device, (4, 4));

    let mut g = Graph::new();
    let a = g.add_node(Dummy::new("a", 1, (8, 8)));
    let b = g.add(Dummy::new("b", 1, (8, 8)), &[a.out(0)]);
    g.connect(b.out(0), a, 0);
    g.export(b.out(0), &view);
    assert!(matches!(g.compile(), Err(GraphError::Cycle(_))));

    let mut g = Graph::new();
    let a = g.add_node(Dummy::new("a", 1, (8, 8)));
    g.export(a.out(0), &view);
    assert!(matches!(
        g.compile(),
        Err(GraphError::Unconnected { input: 0, .. })
    ));

    let mut g = Graph::new();
    let a = g.add(Dummy::new("a", 0, (8, 8)), &[]);
    g.export(a.out(0), &wrong);
    assert!(matches!(
        g.compile(),
        Err(GraphError::ExportMismatch { .. })
    ));

    let mut g = Graph::new();
    g.add(Dummy::new("a", 0, (8, 8)), &[]);
    assert_eq!(g.compile().err(), Some(GraphError::NoExports));
}

/// Five 4K layers composited pairwise the unfused way: each layer's source goes through
/// a gamut node, then a blend with the running result. Aliasing packs the intermediates
/// into a few textures.
#[test]
fn aliasing_packs_five_4k_layers_into_few_textures() {
    let Some((device, _queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let size = (3840, 2160);
    let (_t, view) = target(&device, size);
    let mut g = Graph::new();
    let mut acc: Option<render::graph::NodeId> = None;
    for _ in 0..5 {
        let source = g.add(Dummy::new("source", 0, size), &[]);
        let gamut = g.add(Dummy::new("gamut", 1, size), &[source.out(0)]);
        let transform = g.add(Dummy::new("transform", 1, size), &[gamut.out(0)]);
        acc = Some(match acc {
            None => transform,
            Some(below) => g.add(
                Dummy::new("blend", 2, size),
                &[below.out(0), transform.out(0)],
            ),
        });
    }
    let last = acc.expect("five layers");
    g.export(last.out(0), &view);
    let compiled = g.compile().expect("compiles");
    let stats = compiled.stats();
    println!("{}", compiled.describe());
    println!(
        "5 layers at 4K: {} intermediates ({:.1} MB) in {} textures ({:.1} MB), saved {:.1} MB",
        stats.transient_textures,
        stats.transient_bytes as f64 / 1e6,
        stats.physical_textures,
        stats.physical_bytes as f64 / 1e6,
        stats.saved_bytes() as f64 / 1e6
    );
    assert_eq!(stats.transient_textures, 18);
    assert!(stats.physical_textures <= 3, "{stats:?}");
    assert_eq!(
        stats.physical_bytes,
        stats.physical_textures as u64 * 3840 * 2160 * 8
    );

    // No texture holds two intermediates whose lifetimes overlap.
    let resources = compiled.resources();
    for (i, a) in resources.iter().enumerate() {
        for b in &resources[i + 1..] {
            if a.placement == b.placement {
                assert!(a.last < b.first || b.last < a.first, "{a:?} and {b:?}");
            }
        }
    }
}

const W: u32 = 64;
const H: u32 = 16;

/// Limited-range BT.709 codes of black, white, red and blue bars.
const BARS: [(u8, u8, u8, [f64; 3]); 4] = [
    (16, 128, 128, [0.0, 0.0, 0.0]),
    (235, 128, 128, [1.0, 1.0, 1.0]),
    (63, 102, 240, [1.0, 0.0, 0.0]),
    (32, 240, 118, [0.0, 0.0, 1.0]),
];

struct Bars {
    luma: wgpu::TextureView,
    chroma: wgpu::TextureView,
    params: YuvParams,
}

fn bars(device: &wgpu::Device, queue: &wgpu::Queue) -> Bars {
    let bar = |x: u32| &BARS[(x * 4 / W) as usize];
    let luma: Vec<u8> = (0..H).flat_map(|_| (0..W).map(|x| bar(x).0)).collect();
    let chroma: Vec<u8> = (0..H / 2)
        .flat_map(|_| (0..W / 2).flat_map(|x| [bar(x * 2).1, bar(x * 2).2]))
        .collect();
    let (lf, cf) = plane_formats(HwSurfaceFormat::Nv12);
    let colour = ColourSpace {
        primaries: Primaries::Bt709,
        transfer: TransferFn::Bt1886,
        matrix: Matrix::Bt709,
        range: Range::Limited,
        siting: ChromaSiting::Center,
        bit_depth: 8,
        hdr: Hdr::None,
        defaulted: Defaulted::default(),
    };
    Bars {
        luma: common::texture(device, queue, lf, (W, H), &luma).create_view(&Default::default()),
        chroma: common::texture(device, queue, cf, (W / 2, H / 2), &chroma)
            .create_view(&Default::default()),
        params: YuvParams::new(&colour, PlaneLayout::from_surface(HwSurfaceFormat::Nv12)),
    }
}

fn check_bars(pixels: &[[f32; 4]], matrix: &render::colour::Mat3) {
    for y in 0..H {
        for x in [2, W / 4 + 4, W / 2 + 4, 3 * W / 4 + 4] {
            let px = pixels[(y * W + x) as usize];
            let want = apply(matrix, BARS[(x * 4 / W) as usize].3);
            for c in 0..3 {
                assert!(
                    (f64::from(px[c]) - want[c]).abs() < 0.01,
                    "({x}, {y}): {px:?}, want {want:?}"
                );
            }
            assert_eq!(px[3], 1.0);
        }
    }
}

#[test]
fn decoded_frame_through_source_and_gamut_nodes() {
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let renderer = Renderer::new(&device, &queue);
    let mut pool = TexturePool::new();
    let mut profiler = GpuProfiler::new(&device, &queue, 16);
    let bars = bars(&device, &queue);
    let planes = YuvPlanes::Biplanar {
        luma: &bars.luma,
        chroma: &bars.chroma,
    };
    let (out, out_view) = target(&device, (W, H));

    // BT.709 frame into a BT.709 working space, then out to BT.2020.
    let build = || {
        let mut g = Graph::new();
        let source = g.add(
            YuvSourceNode::new(planes, bars.params, Primaries::Bt709, WorkingSpace::BT709),
            &[],
        );
        let gamut = g.add(
            GamutNode::new(Primaries::Bt709, Primaries::Bt2020),
            &[source.out(0)],
        );
        g.export(gamut.out(0), &out_view);
        g.compile().expect("compiles")
    };
    build().execute(&renderer, &mut pool, Some(&mut profiler));
    check_bars(
        &common::read(&device, &queue, &out),
        &gamut_matrix(Primaries::Bt709, Primaries::Bt2020),
    );
    let created = pool.created();
    build().execute(&renderer, &mut pool, Some(&mut profiler));
    assert_eq!(pool.created(), created, "the second frame reuses the pool");

    if profiler.enabled() {
        let timings = profiler.wait(&device).expect("first frame timed");
        let labels: Vec<&str> = timings.passes.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["source", "gamut"]);
        assert!(
            timings.passes.iter().all(|p| p.gpu_ms.is_some()),
            "{timings:?}"
        );
        assert!(timings.total_ms.is_some_and(|t| t > 0.0), "{timings:?}");
        assert!(profiler.wait(&device).is_some(), "second frame timed");
    } else {
        eprintln!("timestamps not supported: timing checks skipped");
    }

    // A BT.2020 frame into a BT.709 working space, folded into the source pass.
    let mut g = Graph::new();
    let source = g.add(
        YuvSourceNode::new(planes, bars.params, Primaries::Bt2020, WorkingSpace::BT709),
        &[],
    );
    g.export(source.out(0), &out_view);
    g.compile()
        .expect("compiles")
        .execute(&renderer, &mut pool, None);
    check_bars(
        &common::read(&device, &queue, &out),
        &gamut_matrix(Primaries::Bt2020, Primaries::Bt709),
    );
}
