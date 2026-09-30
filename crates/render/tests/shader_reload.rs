//! Shader hot reload: in a debug build an edited shader in a watched folder is validated
//! and its pipeline rebuilt on the next frame, a broken edit keeps the last good
//! pipeline, and the embedded library (what release builds use) never changes. GPU
//! checks skip when no adapter exists.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use media::Primaries;
use render::graph::{Graph, TexturePool};
use render::nodes::GamutNode;
use render::renderer::Renderer;
use render::shaders::{EMBEDDED, ShaderLibrary};
use render::wgpu;

/// FSEvents registration alone can take over ten seconds on a busy Mac.
const REGISTER: Duration = Duration::from_secs(120);

const PIXEL: [f32; 4] = [0.5, 0.25, 0.125, 1.0];

/// A fresh folder holding a copy of every embedded shader.
fn shader_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-shaders-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    for (name, source) in EMBEDDED {
        std::fs::write(dir.join(format!("{name}.wgsl")), source).expect("copy shader");
    }
    dir
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// One frame through the gamut node (BT.709 to BT.709, so the embedded shader passes the
/// pixel through), read back.
fn frame(
    renderer: &Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pool: &mut TexturePool,
) -> [f32; 4] {
    let input = common::rgba16f(device, queue, (1, 1), &[PIXEL]);
    let input_view = input.create_view(&Default::default());
    let out = common::blank(device, wgpu::TextureFormat::Rgba16Float, (1, 1));
    let out_view = out.create_view(&Default::default());
    let mut g = Graph::new();
    let src = g.import("input", &input_view);
    let gamut = g.add(GamutNode::new(Primaries::Bt709, Primaries::Bt709), &[src]);
    g.export(gamut.out(0), &out_view);
    g.compile().expect("compiles").execute(renderer, pool, None);
    common::read(device, queue, &out)[0]
}

fn edit(dir: &Path, body: &str) -> String {
    let source = EMBEDDED
        .iter()
        .find(|(n, _)| *n == "gamut")
        .expect("gamut")
        .1
        .replace(
            "return vec4<f32>(dot(params.r.xyz, c.rgb), dot(params.g.xyz, c.rgb), dot(params.b.xyz, c.rgb), c.a);",
            body,
        );
    std::fs::write(dir.join("gamut.wgsl"), &source).expect("write shader");
    source
}

#[test]
fn edited_shader_rebuilds_and_broken_shader_keeps_the_last_good_one() {
    let dir = shader_dir("reload");
    let library = ShaderLibrary::watching(&dir);
    assert!(library.wait_until_watching(REGISTER));
    let gpu = common::device();
    let renderer = gpu
        .as_ref()
        .map(|(device, queue)| Renderer::with_shaders(device, queue, library));
    let mut pool = TexturePool::new();
    let render = |renderer: &Renderer, pool: &mut TexturePool| {
        let (device, queue) = gpu.as_ref().expect("gpu");
        frame(renderer, device, queue, pool)
    };
    let Some(renderer) = renderer else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    assert_eq!(render(&renderer, &mut pool), PIXEL);

    let edited = edit(&dir, "return vec4<f32>(c.b, c.g, c.r, c.a);");
    wait_for("the edit to load", || {
        renderer.shaders().raw("gamut").as_deref() == Some(edited.as_str())
    });
    assert_eq!(
        render(&renderer, &mut pool),
        [PIXEL[2], PIXEL[1], PIXEL[0], PIXEL[3]],
        "the next frame uses the rebuilt pipeline"
    );

    edit(&dir, "return vec4<f32>(c.b, c.g, c.r);");
    wait_for("the broken edit to be rejected", || {
        renderer.shaders().last_error().is_some()
    });
    let error = renderer.shaders().last_error().unwrap_or_default();
    assert!(error.contains("gamut.wgsl"), "{error}");
    assert_eq!(
        renderer.shaders().raw("gamut").as_deref(),
        Some(edited.as_str())
    );
    assert_eq!(
        render(&renderer, &mut pool),
        [PIXEL[2], PIXEL[1], PIXEL[0], PIXEL[3]],
        "the last good pipeline stays in use"
    );

    drop(renderer);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edited_common_rebuilds_every_pipeline() {
    let dir = shader_dir("common");
    let library = ShaderLibrary::watching(&dir);
    assert!(library.wait_until_watching(REGISTER));
    let common = format!("{}\n// edited\n", EMBEDDED[0].1);
    std::fs::write(dir.join("common.wgsl"), &common).expect("write shader");
    wait_for("the common edit to load", || {
        library.raw("common").as_deref() == Some(common.as_str())
    });
    assert_eq!(library.poll_changes(), ["common"]);
    assert!(
        library
            .composed("gamut")
            .is_some_and(|s| s.contains("// edited"))
    );
    drop(library);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn embedded_library_ignores_the_shader_folder() {
    let library = ShaderLibrary::embedded_only();
    assert!(!library.is_watching());
    let Some((device, queue)) = common::device() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let renderer = Renderer::with_shaders(&device, &queue, library);
    let mut pool = TexturePool::new();
    assert_eq!(frame(&renderer, &device, &queue, &mut pool), PIXEL);
    assert!(renderer.begin_frame().is_empty());
}
