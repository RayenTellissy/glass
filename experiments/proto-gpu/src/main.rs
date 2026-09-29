//! GPU compositor prototype: two 4K source textures, a WGSL dissolve pass and a
//! text overlay pass rendered offscreen, timed per pass with timestamp queries.

use std::time::Instant;

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};

const W: u32 = 3840;
const H: u32 = 2160;
const WARMUP_FRAMES: usize = 100;
const DEFAULT_MEASURE_FRAMES: usize = 2000;
const SOURCE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const QUERY_COUNT: u32 = 4;

const GEN_WGSL: &str = r#"
struct Params { seed: f32, noise: f32, _pad1: f32, _pad2: f32 }
@group(0) @binding(0) var<uniform> p: Params;

fn hash(v: vec2<u32>) -> f32 {
    var h = v.x * 1664525u + v.y * 1013904223u;
    h = (h ^ (h >> 16u)) * 2246822519u;
    h = h ^ (h >> 13u);
    return f32(h & 0xffffu) / 65535.0;
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy / vec2<f32>(3840.0, 2160.0);
    let s = p.seed;
    let grad = vec3<f32>(uv.x, uv.y * (0.5 + 0.5 * s), 1.0 - uv.x * s);
    let c = step(0.5, fract(pos.x / (64.0 + 64.0 * s))) != step(0.5, fract(pos.y / (64.0 + 64.0 * s)));
    let d = length(uv - vec2<f32>(0.3 + 0.4 * s, 0.5)) * 8.0;
    let ring = 0.5 + 0.5 * sin(d * 6.2831 + s * 3.0);
    let col = mix(grad, vec3<f32>(ring), 0.35) * select(0.8, 1.0, c);
    let n = (hash(vec2<u32>(pos.xy) + vec2<u32>(u32(s * 7919.0))) - 0.5) * p.noise;
    return vec4<f32>(col + vec3<f32>(n), 1.0);
}
"#;

const DISSOLVE_WGSL: &str = r#"
struct Params { amount: f32, _pad0: f32, _pad1: f32, _pad2: f32 }
@group(0) @binding(0) var src_a: texture_2d<f32>;
@group(0) @binding(1) var src_b: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> p: Params;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VsOut;
    o.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let a = textureSample(src_a, samp, in.uv);
    let b = textureSample(src_b, samp, in.uv);
    return mix(a, b, p.amount);
}
"#;

struct Opts {
    noise: f32,
    target_format: wgpu::TextureFormat,
    measure_frames: usize,
}

fn parse_opts() -> Opts {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };
    Opts {
        noise: if args.iter().any(|a| a == "--no-noise") {
            0.0
        } else {
            0.25
        },
        target_format: match value("--format").as_deref() {
            Some("rgba16f") => wgpu::TextureFormat::Rgba16Float,
            _ => wgpu::TextureFormat::Rgba8UnormSrgb,
        },
        measure_frames: value("--frames")
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MEASURE_FRAMES),
    }
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn uniform(device: &wgpu::Device, queue: &wgpu::Queue, value: f32) -> wgpu::Buffer {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("params"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buf, 0, &params_bytes(value));
    buf
}

fn params_bytes(value: f32) -> [u8; 16] {
    params2_bytes(value, 0.0)
}

fn params2_bytes(a: f32, b: f32) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&a.to_le_bytes());
    out[4..8].copy_from_slice(&b.to_le_bytes());
    out
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    wgsl: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(format.into())],
        }),
        multiview_mask: None,
        cache: None,
    })
}

struct Stats {
    median: f64,
    p95: f64,
    p99: f64,
    max: f64,
    mean: f64,
}

fn stats(v: &[f64]) -> Option<Stats> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let pct = |p: f64| s[(((p / 100.0) * s.len() as f64).ceil() as usize).clamp(1, s.len()) - 1];
    Some(Stats {
        median: pct(50.0),
        p95: pct(95.0),
        p99: pct(99.0),
        max: s[s.len() - 1],
        mean: s.iter().sum::<f64>() / s.len() as f64,
    })
}

fn row(name: &str, v: &[f64]) -> String {
    match stats(v) {
        Some(s) => format!(
            "| {name} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |",
            v.len(),
            s.median,
            s.p95,
            s.p99,
            s.max,
            s.mean
        ),
        None => format!("| {name} | 0 | - | - | - | - | - |"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = parse_opts();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))?;
    let info = adapter.get_info();
    let supported = adapter.features();
    let has_timestamps = supported.contains(wgpu::Features::TIMESTAMP_QUERY);
    let wanted = wgpu::Features::TIMESTAMP_QUERY
        | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
        | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("proto-gpu"),
        required_features: supported & wanted,
        ..Default::default()
    }))?;

    let (_tex_a, view_a) = texture(&device, "source a", SOURCE_FORMAT);
    let (_tex_b, view_b) = texture(&device, "source b", SOURCE_FORMAT);
    let (_tex_out, view_out) = texture(&device, "output", opts.target_format);

    // Procedural sources, rendered once.
    let gen_pipeline = fullscreen_pipeline(&device, GEN_WGSL, SOURCE_FORMAT);
    let mut enc = device.create_command_encoder(&Default::default());
    for (seed, view) in [(0.0f32, &view_a), (1.0, &view_b)] {
        let buf = uniform(&device, &queue, seed);
        queue.write_buffer(&buf, 0, &params2_bytes(seed, opts.noise));
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &gen_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("generate"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&gen_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit([enc.finish()]);

    let dissolve = fullscreen_pipeline(&device, DISSOLVE_WGSL, opts.target_format);
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let params = uniform(&device, &queue, 0.0);
    let dissolve_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &dissolve.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view_a),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view_b),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params.as_entire_binding(),
            },
        ],
    });

    // Text overlay.
    let mut font_system = FontSystem::new();
    let mut swash_cache = SwashCache::new();
    let cache = Cache::new(&device);
    let mut viewport = Viewport::new(&device, &cache);
    viewport.update(
        &queue,
        Resolution {
            width: W,
            height: H,
        },
    );
    let mut atlas = TextAtlas::new(&device, &queue, &cache, opts.target_format);
    let mut text_renderer =
        TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
    let mut title = Buffer::new(&mut font_system, Metrics::new(180.0, 216.0));
    title.set_size(Some(W as f32 - 400.0), Some(600.0));
    title.set_text(
        "Summer in the Alps\nChapter one",
        &Attrs::new().family(Family::SansSerif).weight(Weight::BOLD),
        Shaping::Advanced,
        None,
    );
    title.shape_until_scroll(&mut font_system, false);
    let mut timecode = Buffer::new(&mut font_system, Metrics::new(64.0, 80.0));
    timecode.set_size(Some(800.0), Some(100.0));

    let query_set = has_timestamps.then(|| {
        device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: QUERY_COUNT,
        })
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("resolve"),
        size: 8 * QUERY_COUNT as u64,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: 8 * QUERY_COUNT as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let period_ns = queue.get_timestamp_period() as f64;

    let mut dissolve_ms = Vec::new();
    let mut text_ms = Vec::new();
    let mut total_ms = Vec::new();
    let mut wall_ms = Vec::new();

    let frames = WARMUP_FRAMES + opts.measure_frames;
    for frame in 0..frames {
        let amount = 0.5 + 0.5 * (frame as f32 * 0.05).sin();
        queue.write_buffer(&params, 0, &params_bytes(amount));

        let secs = frame as u32 / 30;
        timecode.set_text(
            &format!(
                "00:{:02}:{:02}:{:02}",
                secs / 60,
                secs % 60,
                frame as u32 % 30
            ),
            &Attrs::new().family(Family::Monospace),
            Shaping::Basic,
            None,
        );
        timecode.shape_until_scroll(&mut font_system, false);
        text_renderer.prepare(
            &device,
            &queue,
            &mut font_system,
            &mut atlas,
            &viewport,
            [
                TextArea {
                    buffer: &title,
                    left: 200.0,
                    top: 1400.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: W as i32,
                        bottom: H as i32,
                    },
                    default_color: Color::rgb(255, 255, 255),
                    custom_glyphs: &[],
                },
                TextArea {
                    buffer: &timecode,
                    left: 200.0,
                    top: 120.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: W as i32,
                        bottom: H as i32,
                    },
                    default_color: Color::rgb(255, 220, 80),
                    custom_glyphs: &[],
                },
            ],
            &mut swash_cache,
        )?;

        let wall_start = Instant::now();
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dissolve"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view_out,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: query_set
                    .as_ref()
                    .map(|qs| wgpu::RenderPassTimestampWrites {
                        query_set: qs,
                        beginning_of_pass_write_index: Some(0),
                        end_of_pass_write_index: Some(1),
                    }),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&dissolve);
            pass.set_bind_group(0, &dissolve_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("text"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view_out,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: query_set
                    .as_ref()
                    .map(|qs| wgpu::RenderPassTimestampWrites {
                        query_set: qs,
                        beginning_of_pass_write_index: Some(2),
                        end_of_pass_write_index: Some(3),
                    }),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            text_renderer.render(&atlas, &viewport, &mut pass)?;
        }
        if let Some(qs) = &query_set {
            enc.resolve_query_set(qs, 0..QUERY_COUNT, &resolve, 0);
            enc.copy_buffer_to_buffer(&resolve, 0, &readback, 0, 8 * QUERY_COUNT as u64);
        }
        queue.submit([enc.finish()]);
        device.poll(wgpu::PollType::wait_indefinitely())?;
        let wall = wall_start.elapsed().as_secs_f64() * 1000.0;

        if query_set.is_some() {
            readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely())?;
            let ts: Vec<u64> = {
                let data = readback.slice(..).get_mapped_range()?;
                data.as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| u64::from_le_bytes(*c))
                    .collect()
            };
            readback.unmap();
            if frame >= WARMUP_FRAMES {
                let ms = |a: u64, b: u64| b.saturating_sub(a) as f64 * period_ns / 1e6;
                dissolve_ms.push(ms(ts[0], ts[1]));
                text_ms.push(ms(ts[2], ts[3]));
                total_ms.push(ms(ts[0], ts[3]));
            }
        }
        if frame >= WARMUP_FRAMES {
            wall_ms.push(wall);
        }
    }

    let total = stats(&total_ms);
    let budget_basis = if has_timestamps {
        total.as_ref().map(|s| s.p99)
    } else {
        stats(&wall_ms).map(|s| s.p99)
    };
    let verdict = match budget_basis {
        Some(v) if v < 2.0 => "PASS (under 2 ms GPU time, judged on p99)",
        Some(_) => "FAIL (budget is under 2 ms GPU time)",
        None => "NO DATA",
    };
    let report = format!(
        "# proto-gpu results\n\n\
         Two {W}x{H} procedural source textures (`{SOURCE_FORMAT:?}`), one WGSL dissolve pass (single fullscreen triangle, uniform mix amount changing every frame) into a {W}x{H} offscreen target, then a glyphon/cosmic-text overlay pass (two-line 180 px title plus a timecode re-shaped every frame). Release build, headless.\n\n\
         - Adapter: {} ({:?}, {:?})\n\
         - Output format: `{:?}`\n\
         - Per-pixel noise in the sources: {} (defeats the GPU's lossless framebuffer compression, closer to real footage)\n\
         - Timestamp queries: {}\n\
         - Warm-up frames: {WARMUP_FRAMES}, measured frames: {}\n\n\
         Verdict: **{verdict}**\n\n\
         | GPU time (ms) | frames | median | p95 | p99 | max | mean |\n\
         | --- | --- | --- | --- | --- | --- | --- |\n\
         {}\n{}\n{}\n{}\n\n\
         - *Dissolve pass* and *Text pass*: begin/end-of-pass timestamps.\n\
         - *Total*: dissolve begin to text end, including any gap between the passes.\n\
         - *Wall*: CPU wall time from encoding through `queue.submit` and `device.poll(Wait)`, for reference; it includes submission and wake-up latency.\n",
        info.name,
        info.backend,
        info.device_type,
        opts.target_format,
        if opts.noise > 0.0 { "on" } else { "off" },
        if has_timestamps {
            format!(
                "supported (period {period_ns} ns; inside encoders: {}, inside passes: {})",
                device
                    .features()
                    .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
                device
                    .features()
                    .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES)
            )
        } else {
            "not supported, verdict uses wall time".into()
        },
        wall_ms.len(),
        row("Dissolve pass", &dissolve_ms),
        row("Text pass", &text_ms),
        row("Total", &total_ms),
        row("Wall (submit + wait)", &wall_ms),
    );
    println!("{report}");
    Ok(())
}
