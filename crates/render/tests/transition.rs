//! The transition framework: discovery of `assets/transitions/` (the bundled set, and a
//! folder of broken manifests and shaders that are reported, not fatal), the uniform
//! layout shared by Rust and `transition.wgsl`, extra passes (discovered, broken ones
//! reported, drawn in order at their sizes and read by the later ones), cross
//! dissolve's endpoints (exactly A at progress 0 and B at 1) and middle, hot reload of
//! an edited transition shader and of a pass's entry point, and the GPU time of a 4K
//! cross dissolve. GPU checks skip without an adapter.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use render::graph::{Graph, Node, TextureDesc, TexturePool};
use render::profiler::GpuProfiler;
use render::renderer::Renderer;
use render::shaders::ShaderLibrary;
use render::transition::{
    Direction, Easing, ParamKind, ParamValue, Pass, Transition, TransitionNode, TransitionRegistry,
};
use render::wgpu;

/// A fresh empty folder.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-transitions-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write(dir: &Path, folder: &str, manifest: Option<&str>, shader: Option<&str>) {
    let folder = dir.join(folder);
    std::fs::create_dir_all(&folder).expect("folder");
    if let Some(m) = manifest {
        std::fs::write(folder.join("transition.json"), m).expect("manifest");
    }
    if let Some(s) = shader {
        std::fs::write(folder.join("shader.wgsl"), s).expect("shader");
    }
}

fn manifest(id: &str, extra: &str) -> String {
    format!(
        r#"{{"id": "{id}", "name": "transition.name.{}", "category": "test"{extra}}}"#,
        id.replace('-', "_")
    )
}

/// Writes the uniform's fields into the output so the test can read them back: the
/// pixel's x picks which (0 header, 1 dir and size, 2.. the parameters).
const PROBE: &str = r#"
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let x = u32(in.pos.x);
    if x == 0u {
        return vec4<f32>(tr.progress, tr.linear, f32(tr.direction), 0.0);
    }
    if x == 1u {
        return vec4<f32>(tr.dir, tr.size);
    }
    return tr.p[min(x - 2u, 7u)];
}
"#;

/// Every parameter kind, one of each, with defaults.
const PROBE_PARAMS: &str = r#", "directions": ["right", "down_left"], "thumbnail": 0.25, "params": [
    {"id": "amount", "label": "transition.param.amount", "kind": {"float": {"default": 0.5, "range": {"min": 0.0, "max": 1.0}, "soft": null, "decimals": 2}}, "unit": "percent", "keyframeable": true},
    {"id": "count", "label": "transition.param.count", "kind": {"int": {"default": 3, "range": {"min": 1, "max": 9}, "soft": null}}, "unit": "none", "keyframeable": true},
    {"id": "on", "label": "transition.param.on", "kind": {"bool": {"default": true}}, "unit": "none", "keyframeable": false},
    {"id": "shape", "label": "transition.param.shape", "kind": {"choice": {"default": "star", "options": [{"id": "circle", "label": "a.b"}, {"id": "star", "label": "a.c"}]}}, "unit": "none", "keyframeable": false},
    {"id": "tint", "label": "transition.param.tint", "kind": {"colour": {"default": [1.0, 0.5, 0.25, 0.75], "alpha": true}}, "unit": "none", "keyframeable": true},
    {"id": "centre", "label": "transition.param.centre", "kind": {"point": {"default": [10.0, -20.0], "range": {"min": null, "max": null}, "decimals": 1}}, "unit": "pixels", "keyframeable": true},
    {"id": "turn", "label": "transition.param.turn", "kind": {"angle": {"default": 90.0, "range": {"min": null, "max": null}}}, "unit": "degrees", "keyframeable": true}
]"#;

#[test]
fn the_bundled_set_loads_without_problems_and_names_exist() {
    let english = assets::i18n::Locale::english();
    for registry in [
        TransitionRegistry::bundled(),
        TransitionRegistry::discover(&assets::transitions::source_dir()),
    ] {
        assert!(registry.problems().is_empty(), "{:#?}", registry.problems());
        let dissolve = registry.get("cross-dissolve").expect("cross dissolve");
        assert_eq!(dissolve.category(), "dissolve");
        assert!(dissolve.directions().is_empty());
        assert!(dissolve.params().is_empty());
        assert_eq!(dissolve.shader(), "transition/cross-dissolve");
        for t in registry.iter() {
            assert!(
                english.get(t.name()).is_some(),
                "{}: no {}",
                t.id(),
                t.name()
            );
            let category = format!("transition.category.{}", t.category());
            assert!(
                english.get(&category).is_some(),
                "{}: no {category}",
                t.id()
            );
            for p in t.schema_params() {
                let label = p["label"].as_str().unwrap_or_default();
                assert!(english.get(label).is_some(), "{}: no {label}", t.id());
            }
        }
    }
    let standard = TransitionRegistry::standard();
    assert!(standard.get("cross-dissolve").is_some());
    assert!(Arc::ptr_eq(&standard, &TransitionRegistry::standard()));
}

#[test]
fn broken_folders_are_reported_and_the_rest_load() {
    let dir = temp_dir("discovery");
    write(
        &dir,
        "probe",
        Some(&manifest("probe", PROBE_PARAMS)),
        Some(PROBE),
    );
    write(&dir, "fade", Some(&manifest("fade", "")), Some(PROBE));
    write(
        &dir,
        "bad-json",
        Some("{\"id\": \"bad-json\","),
        Some(PROBE),
    );
    write(&dir, "renamed", Some(&manifest("other", "")), Some(PROBE));
    write(&dir, "no-manifest", None, Some(PROBE));
    write(&dir, "no-shader", Some(&manifest("no-shader", "")), None);
    write(
        &dir,
        "bad-shader",
        Some(&manifest("bad-shader", "")),
        Some("@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { return 1.0; }"),
    );
    write(
        &dir,
        "no-entry",
        Some(&manifest("no-entry", "")),
        Some("fn helper() -> f32 { return tr.progress; }"),
    );
    write(
        &dir,
        "text-param",
        Some(&manifest(
            "text-param",
            r#", "params": [{"id": "t", "label": "a.b", "kind": {"text": {"default": "", "multiline": false}}, "unit": "none", "keyframeable": false}]"#,
        )),
        Some(PROBE),
    );
    write(
        &dir,
        "bad-direction",
        Some(&manifest(
            "bad-direction",
            r#", "directions": ["sideways"]"#,
        )),
        Some(PROBE),
    );
    write(
        &dir,
        "bad-default",
        Some(&manifest(
            "bad-default",
            r#", "params": [{"id": "s", "label": "a.b", "kind": {"choice": {"default": "x", "options": [{"id": "y", "label": "a.c"}]}}, "unit": "none", "keyframeable": false}]"#,
        )),
        Some(PROBE),
    );
    write(
        &dir,
        "unknown-field",
        Some(&manifest("unknown-field", r#", "speed": 2"#)),
        Some(PROBE),
    );
    std::fs::write(dir.join("README.md"), "not a folder").expect("file");

    let registry = TransitionRegistry::discover(&dir);
    let ids: Vec<&str> = registry.iter().map(|t| t.id()).collect();
    assert_eq!(ids, ["fade", "probe"]);
    let mut reported: Vec<(&str, &str)> = registry
        .problems()
        .iter()
        .map(|p| (p.folder.as_str(), p.message.as_str()))
        .collect();
    reported.sort();
    let expect = [
        ("bad-default", "not one of the options"),
        ("bad-direction", "unknown direction \"sideways\""),
        ("bad-json", "transition.json: EOF"),
        ("bad-shader", "shader.wgsl"),
        ("no-entry", "no `@fragment fn fs`"),
        ("no-manifest", "transition.json is missing"),
        ("no-shader", "shader.wgsl is missing"),
        ("renamed", "differs from the folder name"),
        ("text-param", "text parameters cannot reach a shader"),
        ("unknown-field", "unknown field `speed`"),
    ];
    assert_eq!(
        reported.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
        expect.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
        "{reported:#?}"
    );
    for ((folder, message), (_, wanted)) in reported.iter().zip(expect) {
        assert!(message.contains(wanted), "{folder}: {message}");
    }

    let probe = registry.get("probe").expect("probe");
    assert_eq!(probe.directions(), [Direction::Right, Direction::DownLeft]);
    assert_eq!(probe.thumbnail(), 0.25);
    assert_eq!(probe.schema_params().len(), 7);
    assert_eq!(
        probe.params()[3].kind,
        ParamKind::Choice(vec!["circle".into(), "star".into()])
    );
    assert_eq!(
        probe.shader_path(),
        Some(dir.join("probe/shader.wgsl").as_path())
    );
    assert_eq!(
        probe.direction_or_default(Some(Direction::Up)),
        Some(Direction::Right),
        "a direction not offered falls back to the default"
    );

    let missing = TransitionRegistry::discover(&dir.join("absent"));
    assert!(missing.is_empty());
    assert_eq!(missing.problems().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
}

fn gpu(registry: &TransitionRegistry) -> Option<Gpu> {
    let (device, queue) = common::device()?;
    let library = ShaderLibrary::embedded_only();
    registry.add_shaders(&library);
    let renderer = Renderer::with_shaders(&device, &queue, library);
    Some(Gpu {
        device,
        queue,
        renderer,
    })
}

/// One transition pass over `a` and `b` (premultiplied pixels of `size`), read back.
fn run(
    gpu: &Gpu,
    size: (u32, u32),
    a: &[[f32; 4]],
    b: &[[f32; 4]],
    node: TransitionNode,
) -> Vec<[f32; 4]> {
    let ta = common::rgba16f(&gpu.device, &gpu.queue, size, a);
    let tb = common::rgba16f(&gpu.device, &gpu.queue, size, b);
    let (va, vb) = (
        ta.create_view(&Default::default()),
        tb.create_view(&Default::default()),
    );
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, size);
    let target_view = target.create_view(&Default::default());
    let mut g = Graph::new();
    let (ia, ib) = (g.import("a", &va), g.import("b", &vb));
    let out = g.add(node, &[ia, ib]);
    g.export(out.out(0), &target_view);
    g.compile()
        .expect("compiles")
        .execute(&gpu.renderer, &mut TexturePool::new(), None);
    common::read(&gpu.device, &gpu.queue, &target)
}

fn f16(v: f32) -> f32 {
    common::f16_to_f32(common::f32_to_f16(v))
}

fn noise(n: usize, seed: u32) -> Vec<[f32; 4]> {
    let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state >> 8) as f32 / (1 << 24) as f32
    };
    (0..n)
        .map(|_| {
            let a = 0.25 + 0.75 * next();
            // Some HDR values above 1, all premultiplied.
            [next() * 2.0 * a, next() * a, next() * a, a].map(f16)
        })
        .collect()
}

#[test]
fn the_uniform_layout_matches_the_prelude() {
    let dir = temp_dir("layout");
    write(
        &dir,
        "probe",
        Some(&manifest("probe", PROBE_PARAMS)),
        Some(PROBE),
    );
    let registry = TransitionRegistry::discover(&dir);
    let Some(gpu) = gpu(&registry) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let probe = registry.get("probe").expect("probe").clone();
    let size = (10, 1);
    let blank = vec![[0.0; 4]; 10];
    let read = |node| run(&gpu, size, &blank, &blank, node);

    let defaults = read(TransitionNode::new(probe.clone(), 0.5, Easing::Linear));
    let d = std::f32::consts::FRAC_1_SQRT_2;
    assert_eq!(defaults[0], [0.5, 0.5, 1.0, 0.0], "progress, linear, right");
    assert_eq!(defaults[1], [1.0, 0.0, 10.0, 1.0], "dir and size");
    assert_eq!(defaults[2], [0.5, 0.0, 0.0, 0.0], "amount");
    assert_eq!(defaults[3], [3.0, 0.0, 0.0, 0.0], "count");
    assert_eq!(defaults[4], [1.0, 0.0, 0.0, 0.0], "on");
    assert_eq!(defaults[5], [1.0, 0.0, 0.0, 0.0], "shape: star is option 1");
    assert_eq!(defaults[6], [1.0, 0.5, 0.25, 0.75], "tint");
    assert_eq!(defaults[7], [10.0, -20.0, 0.0, 0.0], "centre");
    assert!((defaults[8][0] - f16(std::f32::consts::FRAC_PI_2)).abs() < 1e-3);
    assert_eq!(defaults[9], [0.0; 4], "unused slot");

    let set = read(
        TransitionNode::new(probe.clone(), 0.25, Easing::EaseIn)
            .direction(Direction::DownLeft)
            .value("amount", &ParamValue::Number(0.75))
            .value("on", &ParamValue::Bool(false))
            .value("shape", &ParamValue::Choice("circle".into()))
            .value("count", &ParamValue::Bool(true))
            .value("missing", &ParamValue::Number(1.0)),
    );
    let eased = Easing::EaseIn.apply(0.25) as f32;
    assert!((set[0][0] - eased).abs() < 1e-3, "eased: {set:?}");
    assert_eq!(set[0][1..], [0.25, 6.0, 0.0], "linear, down-left");
    assert!((set[1][0] + d).abs() < 1e-3 && (set[1][1] - d).abs() < 1e-3);
    assert_eq!(set[1][2..], [10.0, 1.0]);
    assert_eq!(set[2][0], 0.75);
    assert_eq!(set[3][0], 3.0, "a value of the wrong kind is ignored");
    assert_eq!(set[4][0], 0.0);
    assert_eq!(set[5][0], 0.0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two extra passes and `fs`, each writing what it sees: pass 0 its index and size,
/// pass 1 pass 0's values (its x plus ten times its own index) and its own size, `fs`
/// pass 1's values, the targets' sizes and its own index and size.
const CHAIN: &str = r#"
@fragment
fn first(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(f32(tr.pass_index), tr.pass_size, 1.0);
}

@fragment
fn second(in: VsOut) -> @location(0) vec4<f32> {
    let before = sample_pass(0u, in.uv);
    return vec4<f32>(before.x + 10.0 * f32(tr.pass_index), tr.pass_size, before.y);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let x = u32(in.pos.x);
    if x == 0u {
        return sample_pass(1u, vec2<f32>(0.5));
    }
    if x == 1u {
        return vec4<f32>(size_of_pass(0u), size_of_pass(1u));
    }
    if x == 2u {
        return vec4<f32>(f32(tr.pass_index), tr.pass_size, size_of_pass(2u).x);
    }
    return here_a(in);
}
"#;

const CHAIN_PASSES: &str = r#", "directions": ["right", "down"], "passes": [
    {"entry": "first", "scale": 0.5},
    {"entry": "second", "scale": {"along": 0.25}, "min": 3}
]"#;

#[test]
fn extra_passes_are_discovered_and_drawn_in_order_at_their_sizes() {
    let dir = temp_dir("passes");
    write(
        &dir,
        "chain",
        Some(&manifest("chain", CHAIN_PASSES)),
        Some(CHAIN),
    );
    let registry = TransitionRegistry::discover(&dir);
    assert!(registry.problems().is_empty(), "{:#?}", registry.problems());
    let chain = registry.get("chain").expect("chain").clone();
    assert_eq!(
        chain.passes(),
        [
            Pass {
                entry: "first",
                along: 0.5,
                across: 0.5,
                min: 1
            },
            Pass {
                entry: "second",
                along: 0.25,
                across: 1.0,
                min: 3
            }
        ]
    );
    assert_eq!(chain.entries(), ["fs", "first", "second"]);
    // The result first, then one target per pass; "along" follows the direction, and
    // the second pass is at least 3 pixels on each side.
    let a = TextureDesc::working(16, 8);
    let sizes = |d: Direction| {
        TransitionNode::new(chain.clone(), 0.5, Easing::Linear)
            .direction(d)
            .outputs(&[a, a])
            .iter()
            .map(|t| (t.width, t.height))
            .collect::<Vec<_>>()
    };
    assert_eq!(sizes(Direction::Right), [(16, 8), (8, 4), (4, 8)]);
    assert_eq!(sizes(Direction::Down), [(16, 8), (8, 4), (16, 3)]);
    // Without passes, only the result.
    let plain = TransitionRegistry::bundled()
        .get("cross-dissolve")
        .expect("bundled")
        .clone();
    assert!(plain.passes().is_empty());
    assert_eq!(
        TransitionNode::new(plain, 0.5, Easing::Linear)
            .outputs(&[a, a])
            .len(),
        1
    );

    let Some(gpu) = gpu(&registry) else {
        eprintln!("skipped: no GPU adapter");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let pixels = noise(16 * 8, 5);
    for (d, second) in [
        (Direction::Right, [4.0, 8.0]),
        (Direction::Down, [16.0, 3.0]),
    ] {
        let node = TransitionNode::new(chain.clone(), 0.5, Easing::Linear).direction(d);
        let out = run(&gpu, (16, 8), &pixels, &pixels, node);
        let what = d.id();
        assert_eq!(
            out[0],
            [10.0, second[0], second[1], 8.0],
            "{what}: pass 1 read pass 0, and fs read pass 1"
        );
        assert_eq!(
            out[1],
            [8.0, 4.0, second[0], second[1]],
            "{what}: target sizes"
        );
        assert_eq!(
            out[2],
            [2.0, 16.0, 8.0, 1.0],
            "{what}: fs is pass 2, at the output's size; no pass 2 target"
        );
        assert_eq!(out[3], pixels[3], "{what}: A as usual");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn broken_passes_are_reported() {
    let dir = temp_dir("broken-passes");
    let cases = [
        (
            "missing-entry",
            r#"{"entry": "blur"}"#.to_owned(),
            "no `@fragment fn blur`",
        ),
        (
            "bad-scale",
            r#"{"entry": "first", "scale": 0.01}"#.to_owned(),
            "pass 0: scale 0.01 is not a number from 1/16 to 1",
        ),
        (
            "big-scale",
            r#"{"entry": "first"}, {"entry": "second", "scale": 2}"#.to_owned(),
            "pass 1: scale 2",
        ),
        (
            "bad-axis",
            r#"{"entry": "first", "scale": {"sideways": 0.5}}"#.to_owned(),
            "unknown scale axis \"sideways\"",
        ),
        (
            "final-entry",
            r#"{"entry": "fs"}"#.to_owned(),
            "the final pass",
        ),
        (
            "bad-entry",
            r#"{"entry": "2x"}"#.to_owned(),
            "entry \"2x\" is not ASCII letters",
        ),
        (
            "no-entry",
            r#"{"scale": 0.5}"#.to_owned(),
            "no string \"entry\"",
        ),
        (
            "unknown-field",
            r#"{"entry": "first", "size": 2}"#.to_owned(),
            "unknown field \"size\"",
        ),
        (
            "bad-min",
            r#"{"entry": "first", "min": 0}"#.to_owned(),
            "min 0 is not a whole number",
        ),
        (
            "too-many",
            [r#"{"entry": "first"}"#; 9].join(", "),
            "9 passes; at most 8",
        ),
    ];
    for (folder, passes, _) in &cases {
        write(
            &dir,
            folder,
            Some(&manifest(folder, &format!(r#", "passes": [{passes}]"#))),
            Some(CHAIN),
        );
    }
    write(
        &dir,
        "fine",
        Some(&manifest("fine", CHAIN_PASSES)),
        Some(CHAIN),
    );
    let registry = TransitionRegistry::discover(&dir);
    let ids: Vec<&str> = registry.iter().map(|t| t.id()).collect();
    assert_eq!(ids, ["fine"], "{:#?}", registry.problems());
    assert_eq!(registry.problems().len(), cases.len());
    for (folder, _, wanted) in cases {
        let problem = registry
            .problems()
            .iter()
            .find(|p| p.folder == folder)
            .unwrap_or_else(|| panic!("{folder} not reported"));
        assert!(problem.message.contains(wanted), "{folder}: {problem}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn cross_dissolve() -> Arc<Transition> {
    TransitionRegistry::bundled()
        .get("cross-dissolve")
        .expect("bundled")
        .clone()
}

#[test]
fn cross_dissolve_is_exactly_a_then_exactly_b() {
    let registry = TransitionRegistry::bundled();
    let Some(gpu) = gpu(&registry) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let size = (64, 32);
    let (a, b) = (noise(64 * 32, 1), noise(64 * 32, 2));
    let at = |t: f64, easing| {
        run(
            &gpu,
            size,
            &a,
            &b,
            TransitionNode::new(cross_dissolve(), t, easing),
        )
    };
    for easing in [Easing::Linear, Easing::EaseInOut] {
        assert_eq!(at(0.0, easing), a, "progress 0 is A");
        assert_eq!(at(1.0, easing), b, "progress 1 is B");
    }
    let mut worst = 0.0f32;
    for (i, px) in at(0.5, Easing::Linear).iter().enumerate() {
        for c in 0..4 {
            let want = 0.5 * a[i][c] + 0.5 * b[i][c];
            let tol = want.abs() * 2f32.powi(-10) + 1e-6;
            let error = (px[c] - want).abs();
            worst = worst.max(error);
            assert!(error <= tol, "pixel {i} channel {c}: {} vs {want}", px[c]);
        }
    }
    println!("cross dissolve at 0.5: max error {worst:.6}");
    // Eased: ease-in at a quarter of the way is under a quarter of B.
    let t = Easing::EaseIn.apply(0.25) as f32;
    let eased = at(0.25, Easing::EaseIn);
    for c in 0..4 {
        let want = (1.0 - t) * a[0][c] + t * b[0][c];
        assert!((eased[0][c] - want).abs() <= want.abs() * 2f32.powi(-10) + 1e-6);
    }

    // B at another size is filtered over the same area: a flat B stays flat.
    let flat = vec![[0.2, 0.4, 0.6, 1.0]; 16];
    let ta = common::rgba16f(&gpu.device, &gpu.queue, size, &a);
    let tb = common::rgba16f(&gpu.device, &gpu.queue, (4, 4), &flat);
    let (va, vb) = (
        ta.create_view(&Default::default()),
        tb.create_view(&Default::default()),
    );
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, size);
    let target_view = target.create_view(&Default::default());
    let mut g = Graph::new();
    let (ia, ib) = (g.import("a", &va), g.import("b", &vb));
    let out = g.add(
        TransitionNode::new(cross_dissolve(), 1.0, Easing::Linear),
        &[ia, ib],
    );
    g.export(out.out(0), &target_view);
    g.compile()
        .expect("compiles")
        .execute(&gpu.renderer, &mut TexturePool::new(), None);
    for px in common::read(&gpu.device, &gpu.queue, &target) {
        assert_eq!(px, flat[0].map(f16));
    }
}

#[test]
fn thumbnails_render_at_the_manifest_progress() {
    let node = TransitionNode::thumbnail(cross_dissolve());
    assert_eq!(node.progress(), 0.5);
}

#[cfg(debug_assertions)]
#[test]
fn an_edited_transition_shader_reloads_and_a_broken_one_is_kept_out() {
    use render::shaders::EMBEDDED;

    const REGISTER: Duration = Duration::from_secs(120);
    let shaders = temp_dir("reload-shaders");
    for (name, source) in EMBEDDED {
        std::fs::write(shaders.join(format!("{name}.wgsl")), source).expect("copy shader");
    }
    let dir = temp_dir("reload");
    let body = |weight: &str| {
        format!(
            "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {{\n    return here_a(in) * {weight};\n}}\n"
        )
    };
    write(
        &dir,
        "fade",
        Some(&manifest("fade", "")),
        Some(&body("1.0")),
    );
    let registry = TransitionRegistry::discover(&dir);
    let library = ShaderLibrary::watching(&shaders);
    assert!(library.wait_until_watching(REGISTER));
    registry.add_shaders(&library);
    let fade = registry.get("fade").expect("fade").clone();
    let wait_for = |what: &str, done: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out: {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    // The watcher registers the transition's folder on its thread: edit until it sees one.
    let edited = body("0.5");
    let path = dir.join("fade/shader.wgsl");
    wait_for("the edit to load", &|| {
        std::fs::write(&path, &edited).expect("edit");
        std::thread::sleep(Duration::from_millis(100));
        library.raw(fade.shader()).as_deref() == Some(edited.as_str())
    });
    assert!(library.poll_changes().iter().any(|n| n == fade.shader()));

    std::fs::write(&path, "@fragment fn fs(").expect("broken edit");
    wait_for("the broken edit to be rejected", &|| {
        library.last_error().is_some()
    });
    assert_eq!(
        library.raw(fade.shader()).as_deref(),
        Some(edited.as_str()),
        "the last good source stays"
    );

    // An edited prelude reports the transitions built on it.
    let prelude = format!(
        "{}\n// edited\n",
        EMBEDDED
            .iter()
            .find(|(n, _)| *n == "transition")
            .expect("prelude")
            .1
    );
    std::fs::write(shaders.join("transition.wgsl"), &prelude).expect("edit prelude");
    wait_for("the prelude edit to load", &|| {
        library.raw("transition").as_deref() == Some(prelude.as_str())
    });
    let changed = library.poll_changes();
    assert!(changed.iter().any(|n| n == "transition"), "{changed:?}");
    assert!(changed.iter().any(|n| n == fade.shader()), "{changed:?}");
    assert!(
        library
            .composed(fade.shader())
            .is_some_and(|s| s.contains("// edited") && s.contains("* 0.5"))
    );

    if let Some((device, queue)) = common::device() {
        let renderer = Renderer::with_shaders(&device, &queue, library);
        let gpu = Gpu {
            device,
            queue,
            renderer,
        };
        let px = [[0.5, 0.25, 1.0, 1.0]];
        let out = run(
            &gpu,
            (1, 1),
            &px,
            &px,
            TransitionNode::new(fade, 0.0, Easing::Linear),
        );
        assert_eq!(out[0], [0.25, 0.125, 0.5, 0.5], "the reloaded shader draws");
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&shaders);
}

#[cfg(debug_assertions)]
#[test]
fn an_edited_pass_reloads_and_one_without_its_entry_point_is_kept_out() {
    use render::shaders::EMBEDDED;

    const REGISTER: Duration = Duration::from_secs(120);
    let shaders = temp_dir("reload-pass-shaders");
    for (name, source) in EMBEDDED {
        std::fs::write(shaders.join(format!("{name}.wgsl")), source).expect("copy shader");
    }
    let dir = temp_dir("reload-pass");
    // The pass weighs A; fs shows the pass's target.
    let body = |pass: &str| {
        format!(
            "@fragment\nfn {pass}(in: VsOut) -> @location(0) vec4<f32> {{\n    return sample_a(in.uv) * 1.0;\n}}\n\n@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {{\n    return sample_pass(0u, in.uv);\n}}\n"
        )
    };
    write(
        &dir,
        "halve",
        Some(&manifest(
            "halve",
            r#", "passes": [{"entry": "weigh", "scale": 0.5}]"#,
        )),
        Some(&body("weigh")),
    );
    let registry = TransitionRegistry::discover(&dir);
    assert!(registry.problems().is_empty(), "{:#?}", registry.problems());
    let library = ShaderLibrary::watching(&shaders);
    assert!(library.wait_until_watching(REGISTER));
    registry.add_shaders(&library);
    let halve = registry.get("halve").expect("halve").clone();
    let wait_for = |what: &str, done: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out: {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    // The pass's entry point is edited: it now halves A.
    let edited = body("weigh").replace("* 1.0", "* 0.5");
    let path = dir.join("halve/shader.wgsl");
    wait_for("the edit to load", &|| {
        std::fs::write(&path, &edited).expect("edit");
        std::thread::sleep(Duration::from_millis(100));
        library.raw(halve.shader()).as_deref() == Some(edited.as_str())
    });
    assert!(library.poll_changes().iter().any(|n| n == halve.shader()));

    // A valid shader that no longer defines the pass's entry point is kept out.
    std::fs::write(&path, body("renamed")).expect("edit without the entry point");
    wait_for("the edit without the entry point to be rejected", &|| {
        library.last_error().is_some()
    });
    let error = library.last_error().unwrap_or_default();
    assert!(error.contains("no `@fragment fn weigh`"), "{error}");
    assert_eq!(
        library.raw(halve.shader()).as_deref(),
        Some(edited.as_str()),
        "the last good source stays"
    );

    if let Some((device, queue)) = common::device() {
        let renderer = Renderer::with_shaders(&device, &queue, library);
        let gpu = Gpu {
            device,
            queue,
            renderer,
        };
        let px = [[0.5, 0.25, 1.0, 1.0]];
        let out = run(
            &gpu,
            (1, 1),
            &px,
            &px,
            TransitionNode::new(halve, 0.0, Easing::Linear),
        );
        assert_eq!(out[0], [0.25, 0.125, 0.5, 0.5], "the reloaded pass draws");
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&shaders);
}

/// GPU time of one 4K cross dissolve pass over noise, 30 frames from the pass
/// timestamps, held to the budget by the shared timing rules (`common::timing`).
#[test]
fn cross_dissolve_at_4k_gpu_time() {
    let _serial = common::timing::serial();
    let registry = TransitionRegistry::bundled();
    let Some(gpu) = gpu(&registry) else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut profiler = GpuProfiler::new(&gpu.device, &gpu.queue, 4);
    if !profiler.enabled() {
        eprintln!("skipped: no timestamp queries");
        return;
    }
    let a = common::noise_4k(&gpu.device, &gpu.queue, 1);
    let b = common::noise_4k(&gpu.device, &gpu.queue, 2);
    let (va, vb) = (
        a.create_view(&Default::default()),
        b.create_view(&Default::default()),
    );
    let target = common::blank(&gpu.device, wgpu::TextureFormat::Rgba16Float, (3840, 2160));
    let target_view = target.create_view(&Default::default());
    let mut pool = TexturePool::new();
    let mut times = Vec::new();
    for frame in 0..32u32 {
        let mut g = Graph::new();
        let (ia, ib) = (g.import("a", &va), g.import("b", &vb));
        let out = g.add(
            TransitionNode::new(cross_dissolve(), f64::from(frame) / 31.0, Easing::EaseInOut),
            &[ia, ib],
        );
        g.export(out.out(0), &target_view);
        g.compile()
            .expect("compiles")
            .execute(&gpu.renderer, &mut pool, Some(&mut profiler));
        let timings = profiler.wait(&gpu.device).expect("timed");
        // The first two frames build the pipeline and warm the caches.
        if frame >= 2 {
            times.extend(timings.passes[0].gpu_ms);
        }
    }
    let times = common::timing::Samples::new(times);
    println!("4K cross dissolve GPU: {times}");
    // A quarter of the 8 ms compositor budget.
    common::timing::assert_gpu_within("4K cross dissolve", &times, 2.0);
}
