//! Decode prototype: 4K H.264 through FFmpeg's VideoToolbox hwaccel, NV12
//! planes wrapped zero-copy as wgpu textures, converted to RGBA by a WGSL pass.
//! Measures throughput, per-frame latency and CPU, unpaced and paced at 60 fps.

mod ffi;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ffmpeg_next as ffmpeg;

const W: u32 = 3840;
const H: u32 = 2160;
const VERIFY_FRAME: usize = 300;
const PACE_FPS: f64 = 60.0;
const SURFACE_CACHE_MAX: usize = 64;
/// Frames the paced run may decode ahead of display, like a player's look-ahead queue.
const PACE_LOOKAHEAD: u32 = 3;

enum Wrapper {
    None,
    /// `CVMetalTextureCacheCreateTextureFromImage` per plane, every frame.
    CvCache(ffi::TextureCache),
    /// Metal textures made directly from each IOSurface, cached by surface ID.
    Surfaces {
        cache: HashMap<u32, wgpu::BindGroup>,
        misses: usize,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum WrapMode {
    /// Reference point: decode only, frames are dropped without touching the GPU.
    DecodeOnly,
    CvCache,
    Surfaces,
}

const NV12_WGSL: &str = r#"
@group(0) @binding(0) var luma: texture_2d<f32>;
@group(0) @binding(1) var chroma: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VsOut;
    o.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    return o;
}

// BT.709, limited range.
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let y = (textureLoad(luma, vec2<i32>(in.pos.xy), 0).r - 16.0 / 255.0) * (255.0 / 219.0);
    let c = (textureSample(chroma, samp, in.uv).rg - vec2<f32>(128.0 / 255.0)) * (255.0 / 224.0);
    let rgb = vec3<f32>(
        y + 1.5748 * c.y,
        y - 0.1873 * c.x - 0.4681 * c.y,
        y + 1.8556 * c.x,
    );
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    adapter_name: String,
}

impl Gpu {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12"),
            source: wgpu::ShaderSource::Wgsl(NV12_WGSL.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("nv12 to rgba"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            device,
            queue,
            pipeline,
            sampler,
            adapter_name: format!("{} ({:?})", info.name, info.backend),
        })
    }

    fn output(&self) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rgba out"),
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    fn bind_group(&self, y: &wgpu::Texture, uv: &wgpu::Texture) -> wgpu::BindGroup {
        let yv = y.create_view(&Default::default());
        let uvv = uv.create_view(&Default::default());
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&yv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uvv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Wraps the frame's planes, converts to `out`, and waits for the GPU.
    fn convert(
        &self,
        wrap: &mut Wrapper,
        frame: &ffmpeg::frame::Video,
        out: &wgpu::TextureView,
    ) -> Result<(), String> {
        let pb = ffi::pixel_buffer(frame).ok_or("frame is not a VideoToolbox frame")?;
        if matches!(wrap, Wrapper::None) {
            return Ok(());
        }
        // Keeps per-frame CoreVideo textures alive until the GPU has finished.
        let mut _planes = None;
        let bg = match wrap {
            Wrapper::None => return Ok(()),
            Wrapper::CvCache(cache) => {
                let y = ffi::wrap_plane(&self.device, cache, &pb, 0)?;
                let uv = ffi::wrap_plane(&self.device, cache, &pb, 1)?;
                let bg = self.bind_group(&y.texture, &uv.texture);
                _planes = Some((y, uv));
                bg
            }
            Wrapper::Surfaces { cache, misses } => {
                let id = ffi::surface_id(&pb).ok_or("pixel buffer has no IOSurface")?;
                if !cache.contains_key(&id) {
                    *misses += 1;
                    if cache.len() >= SURFACE_CACHE_MAX {
                        cache.clear();
                    }
                    let (y, uv) = ffi::wrap_surface(&self.device, &pb)?;
                    let bg = self.bind_group(&y, &uv);
                    cache.insert(id, bg);
                }
                cache.get(&id).cloned().ok_or("surface cache miss")?
            }
        };
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nv12"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: out,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        let idx = self.queue.submit([enc.finish()]);
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(idx),
                timeout: None,
            })
            .map_err(|e| e.to_string())?;
        if let Wrapper::CvCache(cache) = wrap {
            cache.flush();
        }
        Ok(())
    }

    fn read_rgba(&self, tex: &wgpu::Texture) -> Result<Vec<u8>, String> {
        let bytes_per_row = W * 4;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bytes_per_row * H) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(H),
                },
            },
            tex.size(),
        );
        self.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| e.to_string())?;
        let data = buf
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?
            .to_vec();
        buf.unmap();
        Ok(data)
    }
}

#[derive(Default)]
struct StreamResult {
    frames: usize,
    wall: Duration,
    decode_ms: Vec<f64>,
    upload_ms: Vec<f64>,
    total_ms: Vec<f64>,
    late: usize,
    fourcc: String,
    surface_misses: usize,
}

enum Stop {
    AllFrames,
    AtFrame(usize),
}

/// Decodes the whole clip (or up to a frame) and converts every frame on the GPU.
/// Returns the stats and, when stopping at a frame, leaves it in `out`.
fn run_stream(
    path: &Path,
    gpu: &Gpu,
    out: &wgpu::Texture,
    paced: bool,
    threads: i32,
    mode: WrapMode,
    stop: Stop,
) -> Result<StreamResult, String> {
    let hw = ffi::HwDevice::videotoolbox()?;
    let mut wrap = match mode {
        WrapMode::DecodeOnly => Wrapper::None,
        WrapMode::CvCache => Wrapper::CvCache(ffi::TextureCache::new(&gpu.device)?),
        WrapMode::Surfaces => Wrapper::Surfaces {
            cache: HashMap::new(),
            misses: 0,
        },
    };
    let mut ictx = ffmpeg::format::input(path).map_err(|e| e.to_string())?;
    let stream = ictx
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or("no video stream")?;
    let index = stream.index();
    let mut ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?;
    ffi::enable_hwaccel(&mut ctx, &hw, threads)?;
    let mut decoder = ctx.decoder().video().map_err(|e| e.to_string())?;
    let view = out.create_view(&Default::default());

    let mut res = StreamResult::default();
    let mut frame = ffmpeg::frame::Video::empty();
    let start = Instant::now();
    let mut decode_acc = Duration::ZERO;
    let slot = Duration::from_secs_f64(1.0 / PACE_FPS);

    let mut handle = |frame: &ffmpeg::frame::Video,
                      decode: Duration,
                      res: &mut StreamResult|
     -> Result<bool, String> {
        if res.fourcc.is_empty() {
            res.fourcc = ffi::pixel_buffer(frame)
                .map(|pb| ffi::fourcc(&pb))
                .unwrap_or_else(|| "software frame".into());
        }
        let t0 = Instant::now();
        gpu.convert(&mut wrap, frame, &view)?;
        let upload = t0.elapsed();
        let n = res.frames;
        res.frames += 1;
        res.decode_ms.push(decode.as_secs_f64() * 1e3);
        res.upload_ms.push(upload.as_secs_f64() * 1e3);
        res.total_ms.push((decode + upload).as_secs_f64() * 1e3);
        if paced {
            let now = Instant::now();
            if now > start + slot * (n as u32 + PACE_LOOKAHEAD) {
                res.late += 1;
            }
            let next = start + slot * (n as u32 + 1).saturating_sub(PACE_LOOKAHEAD);
            if next > now {
                thread::sleep(next - now);
            }
        }
        if let Wrapper::Surfaces { misses, .. } = &wrap {
            res.surface_misses = *misses;
        }
        Ok(matches!(stop, Stop::AtFrame(f) if n == f))
    };

    let mut t = Instant::now();
    'outer: for (s, packet) in ictx.packets() {
        if s.index() != index {
            continue;
        }
        decoder.send_packet(&packet).map_err(|e| e.to_string())?;
        while decoder.receive_frame(&mut frame).is_ok() {
            decode_acc += t.elapsed();
            if handle(&frame, decode_acc, &mut res)? {
                break 'outer;
            }
            decode_acc = Duration::ZERO;
            t = Instant::now();
        }
        decode_acc += t.elapsed();
        t = Instant::now();
    }
    if matches!(stop, Stop::AllFrames) {
        decoder.send_eof().map_err(|e| e.to_string())?;
        t = Instant::now();
        while decoder.receive_frame(&mut frame).is_ok() {
            decode_acc += t.elapsed();
            handle(&frame, decode_acc, &mut res)?;
            decode_acc = Duration::ZERO;
            t = Instant::now();
        }
    }
    res.wall = start.elapsed();
    Ok(res)
}

/// Cumulative CPU seconds per VTDecoderXPCService process.
fn vt_service_cpu() -> HashMap<u32, f64> {
    let Ok(out) = Command::new("ps")
        .args(["-axo", "pid=,time=,comm="])
        .output()
    else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("VTDecoderXPCService"))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let time = it.next()?;
            let secs = time.split(':').try_fold(0.0f64, |acc, part| {
                part.parse::<f64>().ok().map(|v| acc * 60.0 + v)
            })?;
            Some((pid, secs))
        })
        .collect()
}

fn vt_delta(before: &HashMap<u32, f64>, after: &HashMap<u32, f64>) -> f64 {
    after
        .iter()
        .map(|(pid, t)| t - before.get(pid).copied().unwrap_or(0.0))
        .sum()
}

struct Stats {
    median: f64,
    p99: f64,
    max: f64,
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
        p99: pct(99.0),
        max: s[s.len() - 1],
    })
}

fn fmt_stats(v: &[f64]) -> String {
    stats(v)
        .map(|s| format!("{:.2} / {:.2} / {:.2}", s.median, s.p99, s.max))
        .unwrap_or_else(|| "-".into())
}

struct RunReport {
    label: String,
    mode: WrapMode,
    streams: usize,
    paced: bool,
    fps_per_stream: f64,
    frames: usize,
    late: usize,
    decode: String,
    upload: String,
    total: String,
    process_cpu_pct: f64,
    vt_cpu_pct: f64,
    fourcc: String,
    surfaces: usize,
}

impl WrapMode {
    fn name(self) -> &'static str {
        match self {
            WrapMode::DecodeOnly => "decode only (no GPU work)",
            WrapMode::CvCache => "CVMetalTextureCache per frame",
            WrapMode::Surfaces => "IOSurface textures cached",
        }
    }
}

fn run_config(
    path: &Path,
    gpu: &Arc<Gpu>,
    streams: usize,
    paced: bool,
    threads: i32,
    mode: WrapMode,
) -> Result<RunReport, String> {
    let outs: Vec<wgpu::Texture> = (0..streams).map(|_| gpu.output()).collect();
    let vt_before = vt_service_cpu();
    let cpu_before = ffi::process_cpu_seconds();
    let wall_start = Instant::now();
    let results: Vec<Result<StreamResult, String>> = thread::scope(|scope| {
        let handles: Vec<_> = outs
            .iter()
            .map(|out| {
                let gpu = gpu.clone();
                scope.spawn(move || {
                    run_stream(path, &gpu, out, paced, threads, mode, Stop::AllFrames)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("stream thread panicked".into()))
            })
            .collect()
    });
    let wall = wall_start.elapsed().as_secs_f64();
    let cpu = ffi::process_cpu_seconds() - cpu_before;
    let vt = vt_delta(&vt_before, &vt_service_cpu());
    let results: Vec<StreamResult> = results.into_iter().collect::<Result<_, _>>()?;

    let all = |f: fn(&StreamResult) -> &Vec<f64>| -> Vec<f64> {
        results.iter().flat_map(|r| f(r).iter().copied()).collect()
    };
    Ok(RunReport {
        label: format!(
            "{}, {} x{streams}",
            mode.name(),
            if paced { "paced 60 fps" } else { "unpaced" }
        ),
        mode,
        streams,
        paced,
        fps_per_stream: results
            .iter()
            .map(|r| r.frames as f64 / r.wall.as_secs_f64())
            .fold(f64::INFINITY, f64::min),
        frames: results.iter().map(|r| r.frames).sum(),
        late: results.iter().map(|r| r.late).sum(),
        decode: fmt_stats(&all(|r| &r.decode_ms)),
        upload: fmt_stats(&all(|r| &r.upload_ms)),
        total: fmt_stats(&all(|r| &r.total_ms)),
        process_cpu_pct: cpu / wall * 100.0 / streams as f64,
        vt_cpu_pct: vt / wall * 100.0 / streams as f64,
        fourcc: results
            .first()
            .map(|r| r.fourcc.clone())
            .unwrap_or_default(),
        surfaces: results.iter().map(|r| r.surface_misses).max().unwrap_or(0),
    })
}

struct Verify {
    mode: WrapMode,
    png: String,
    mean_abs: f64,
    max_abs: u8,
    samples: Vec<(u32, u32, [u8; 3], [u8; 3])>,
}

fn reference_frame(path: &Path) -> Result<Vec<u8>, String> {
    let ffmpeg_bin = std::env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let filter = format!(
        "select=eq(n\\,{VERIFY_FRAME}),scale=in_color_matrix=bt709:in_range=tv:out_range=pc:flags=bilinear+accurate_rnd+full_chroma_int,format=rgb24"
    );
    let reference = Command::new(ffmpeg_bin)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vf", &filter, "-frames:v", "1", "-f", "rawvideo", "-"])
        .output()
        .map_err(|e| format!("running ffmpeg CLI: {e}"))?
        .stdout;
    if reference.len() != (W * H * 3) as usize {
        return Err(format!("reference frame has {} bytes", reference.len()));
    }
    Ok(reference)
}

fn verify(
    path: &Path,
    gpu: &Gpu,
    media: &Path,
    mode: WrapMode,
    reference: &[u8],
) -> Result<Verify, String> {
    let out = gpu.output();
    run_stream(path, gpu, &out, false, 1, mode, Stop::AtFrame(VERIFY_FRAME))?;
    let rgba = gpu.read_rgba(&out)?;

    let png = format!(
        "frame-{VERIFY_FRAME}-{}.png",
        match mode {
            WrapMode::DecodeOnly => "none",
            WrapMode::CvCache => "cvcache",
            WrapMode::Surfaces => "iosurface",
        }
    );
    let file = std::fs::File::create(media.join(&png)).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(&rgba))
        .map_err(|e| e.to_string())?;

    let mut sum = 0u64;
    let mut max = 0u8;
    for (px, rf) in rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(reference.as_chunks::<3>().0)
    {
        for c in 0..3 {
            let d = px[c].abs_diff(rf[c]);
            sum += d as u64;
            max = max.max(d);
        }
    }
    let samples = [(100, 100), (1920, 1080), (3000, 1800), (500, 2000)]
        .into_iter()
        .map(|(x, y)| {
            let i = (y * W + x) as usize;
            let g = [rgba[i * 4], rgba[i * 4 + 1], rgba[i * 4 + 2]];
            let r = [reference[i * 3], reference[i * 3 + 1], reference[i * 3 + 2]];
            (x, y, g, r)
        })
        .collect();
    Ok(Verify {
        mode,
        png,
        mean_abs: sum as f64 / (W * H * 3) as f64,
        max_abs: max,
        samples,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let media = root.join("media");
    let clip = media.join("clip-4k60.mp4");
    if !clip.exists() {
        return Err(format!("{} missing; run scripts/make-media.sh", clip.display()).into());
    }
    let args: Vec<String> = std::env::args().collect();
    let threads: i32 = args
        .iter()
        .position(|a| a == "--threads")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    ffmpeg::init()?;
    ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
    let gpu = Arc::new(Gpu::new()?);

    let reference = reference_frame(&clip)?;
    let mut checks = Vec::new();
    for mode in [WrapMode::CvCache, WrapMode::Surfaces] {
        let v = verify(&clip, &gpu, &media, mode, &reference)?;
        println!(
            "verify {}: frame {VERIFY_FRAME}, mean abs diff {:.3}, max {}",
            mode.name(),
            v.mean_abs,
            v.max_abs
        );
        checks.push(v);
    }

    let configs: &[(WrapMode, usize, bool)] = &[
        (WrapMode::DecodeOnly, 1, true),
        (WrapMode::CvCache, 1, false),
        (WrapMode::CvCache, 1, true),
        (WrapMode::Surfaces, 1, false),
        (WrapMode::Surfaces, 1, true),
        (WrapMode::Surfaces, 2, false),
        (WrapMode::Surfaces, 2, true),
    ];
    let mut reports = Vec::new();
    for &(mode, streams, paced) in configs {
        let r = run_config(&clip, &gpu, streams, paced, threads, mode)?;
        println!(
            "{}: {:.1} fps/stream, process CPU {:.2}% per stream, VT service CPU {:.2}% per stream, late {}",
            r.label, r.fps_per_stream, r.process_cpu_pct, r.vt_cpu_pct, r.late
        );
        reports.push(r);
    }

    let judged = reports
        .iter()
        .filter(|r| r.streams == 1 && r.paced && r.mode != WrapMode::DecodeOnly)
        .min_by(|a, b| a.process_cpu_pct.total_cmp(&b.process_cpu_pct));
    let pass =
        judged.is_some_and(|r| r.late == 0 && r.process_cpu_pct < 5.0 && r.fps_per_stream >= 59.5);
    let mut md = format!(
        "# proto-decode results\n\n\
         4K60 H.264 (High, GOP 120, 1200 frames, about 31.6 Mbit/s, `scripts/make-media.sh`) decoded with FFmpeg's VideoToolbox hwaccel. Each frame's NV12 planes are wrapped zero-copy as wgpu textures and converted to `Rgba8Unorm` by a WGSL BT.709 limited-range pass. Release build, headless, one frame in flight per stream (the GPU conversion is waited on before the next frame).\n\n\
         Wrapping paths (both zero-copy, no CPU copy of pixel data):\n\n\
         - *CVMetalTextureCache per frame*: `CVMetalTextureCacheCreateTextureFromImage` per plane -> `MTLTexture` -> `texture_from_raw` / `create_texture_from_hal`, new wgpu textures and bind group every frame.\n\
         - *IOSurface textures cached*: `newTextureWithDescriptor:iosurface:plane:` per plane of each IOSurface VideoToolbox hands out, wrapped the same way, with the bind group cached by IOSurface ID (VideoToolbox recycles a small pool).\n\n\
         - FFmpeg: libavutil {} via `ffmpeg-next` 9 against the Homebrew shared FFmpeg 9.0.1 build\n\
         - GPU: {}\n\
         - Decoder output pixel format: `{}`\n\
         - Decoder threads: {threads}\n\
         - Paced runs let the decoder run up to {PACE_LOOKAHEAD} frames ahead of display (a player's look-ahead queue); a frame is late if it is ready after its display slot.\n\n\
         Verdict: **{}** (budget: 60 fps sustained with under 5% CPU per stream, judged on the best paced single-stream run using this process's CPU: {})\n\n\
         | Run | frames | fps per stream (slowest) | late frames | decode ms median / p99 / max | wrap + convert + GPU wait ms median / p99 / max | decode + upload ms median / p99 / max | process CPU % of one core, per stream | VTDecoderXPCService CPU %, per stream | IOSurfaces seen |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
        {
            let v = ffmpeg::util::version();
            format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
        },
        gpu.adapter_name,
        reports.first().map(|r| r.fourcc.as_str()).unwrap_or(""),
        if pass { "PASS" } else { "FAIL" },
        judged
            .map(|r| format!(
                "{:.2}% CPU, {:.1} fps, {} late",
                r.process_cpu_pct, r.fps_per_stream, r.late
            ))
            .unwrap_or_default(),
    );
    for r in &reports {
        md.push_str(&format!(
            "| {} | {} | {:.1} | {} | {} | {} | {} | {:.2} | {:.2} | {} |\n",
            r.label,
            r.frames,
            r.fps_per_stream,
            r.late,
            r.decode,
            r.upload,
            r.total,
            r.process_cpu_pct,
            r.vt_cpu_pct,
            if r.mode == WrapMode::Surfaces {
                r.surfaces.to_string()
            } else {
                "-".into()
            }
        ));
    }
    md.push_str(&format!(
        "\n## Correctness\n\nFrame {VERIFY_FRAME} converted on the GPU, read back, saved to `media/` and compared with the same frame decoded by the FFmpeg CLI (swscale, BT.709 limited to full range, rgb24).\n\n"
    ));
    for v in &checks {
        md.push_str(&format!(
            "- {} (`media/{}`): mean absolute difference {:.3} per channel, max {}\n",
            v.mode.name(),
            v.png,
            v.mean_abs,
            v.max_abs
        ));
    }
    md.push_str("\n| path | pixel | GPU RGB | FFmpeg CLI RGB |\n| --- | --- | --- | --- |\n");
    for v in &checks {
        for (x, y, g, r) in &v.samples {
            md.push_str(&format!(
                "| {} | ({x}, {y}) | {g:?} | {r:?} |\n",
                v.mode.name()
            ));
        }
    }
    std::fs::write(root.join("RESULTS.md"), &md)?;
    println!("{md}");
    Ok(())
}
