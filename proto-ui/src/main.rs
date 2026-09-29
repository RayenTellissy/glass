//! Timeline frame-time prototype: 200 fake clips on 10 tracks, repainted
//! continuously with vsync off while zoom, scroll and playhead change every
//! frame, then prints frame-time statistics and writes them to RESULTS.md.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    Vec2, pos2, vec2,
};

const TRACKS: usize = 10;
const CLIPS_PER_TRACK: usize = 20;
const VIDEO_TRACKS: usize = 6;
const WARMUP_FRAMES: usize = 100;
const DEFAULT_MEASURE_FRAMES: usize = 3000;
const TIME_LIMIT: Duration = Duration::from_secs(90);
const HEADER_W: f32 = 180.0;
const RULER_H: f32 = 28.0;
const TRACK_H: f32 = 64.0;
const TRACK_GAP: f32 = 2.0;
const PEAKS_PER_SEC: f32 = 50.0;

struct Clip {
    start: f32,
    len: f32,
    name: String,
    color: Color32,
    peaks: Option<Vec<f32>>,
    thumbs: bool,
}

struct Track {
    name: String,
    clips: Vec<Clip>,
}

struct Rng(u64);

impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

const PALETTE: [Color32; 6] = [
    Color32::from_rgb(74, 122, 214),
    Color32::from_rgb(92, 168, 120),
    Color32::from_rgb(196, 120, 74),
    Color32::from_rgb(160, 96, 200),
    Color32::from_rgb(200, 84, 112),
    Color32::from_rgb(70, 170, 180),
];

fn make_tracks() -> (Vec<Track>, f32) {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut end = 0.0f32;
    let tracks = (0..TRACKS)
        .map(|ti| {
            let audio = ti >= VIDEO_TRACKS;
            let mut t = rng.next_f32() * 2.0;
            let clips = (0..CLIPS_PER_TRACK)
                .map(|ci| {
                    let len = 2.0 + rng.next_f32() * 10.0;
                    let with_media = ci % 2 == 0;
                    let peaks = (audio && with_media).then(|| {
                        let n = (len * PEAKS_PER_SEC) as usize;
                        let mut env = 0.5f32;
                        (0..n)
                            .map(|_| {
                                env = (env + (rng.next_f32() - 0.5) * 0.2).clamp(0.1, 1.0);
                                env * (0.4 + 0.6 * rng.next_f32())
                            })
                            .collect()
                    });
                    let clip = Clip {
                        start: t,
                        len,
                        name: format!(
                            "{}_{:03}.{}",
                            if audio { "VO_take" } else { "A001_C" },
                            ti * CLIPS_PER_TRACK + ci,
                            if audio { "wav" } else { "mov" }
                        ),
                        color: PALETTE[(ti + ci) % PALETTE.len()],
                        peaks,
                        thumbs: !audio && with_media,
                    };
                    t += len + rng.next_f32() * 1.5;
                    clip
                })
                .collect();
            end = end.max(t);
            Track {
                name: if audio {
                    format!("A{}", ti - VIDEO_TRACKS + 1)
                } else {
                    format!("V{}", VIDEO_TRACKS - ti)
                },
                clips,
            }
        })
        .collect();
    (tracks, end)
}

#[derive(Default)]
struct Samples {
    cpu_ms: Vec<f32>,
    build_ms: Vec<f32>,
    interval_ms: Vec<f32>,
    shapes: Vec<usize>,
}

struct Opts {
    cull: bool,
    measure_frames: usize,
    display_hz: String,
}

struct TimelineApp {
    tracks: Vec<Track>,
    duration: f32,
    frame: usize,
    started: Instant,
    last_frame: Option<Instant>,
    samples: Arc<Mutex<Samples>>,
    opts: Arc<Opts>,
    done: bool,
}

impl TimelineApp {
    fn draw(&self, ui: &mut egui::Ui) -> usize {
        let full = ui.max_rect();
        let (_, painter) = ui.allocate_painter(full.size(), Sense::hover());
        let t = self.frame as f32;

        let lane_w = (full.width() - HEADER_W).max(1.0);
        let fit_pps = lane_w / self.duration;
        // Zoom sweeps from "whole timeline" to 6x in, so every frame re-lays out.
        let zoom = 1.0 + 5.0 * (0.5 + 0.5 * (t * 0.011).sin());
        let pps = fit_pps * zoom;
        let visible_secs = lane_w / pps;
        let max_scroll = (self.duration - visible_secs).max(0.0);
        let scroll = max_scroll * (0.5 + 0.5 * (t * 0.007).sin());
        let playhead = (t / 60.0) % self.duration;

        let lanes = Rect::from_min_max(pos2(full.left() + HEADER_W, full.top()), full.max);
        let x_of = |secs: f32| lanes.left() + (secs - scroll) * pps;

        painter.rect_filled(full, CornerRadius::ZERO, Color32::from_gray(22));

        // Ruler.
        let ruler = Rect::from_min_size(lanes.min, vec2(lane_w, RULER_H));
        painter.rect_filled(ruler, CornerRadius::ZERO, Color32::from_gray(32));
        let step = [0.5f32, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
            .into_iter()
            .find(|s| s * pps >= 80.0)
            .unwrap_or(60.0);
        let mut s = (scroll / step).floor() * step;
        while s <= scroll + visible_secs {
            let x = x_of(s);
            painter.line_segment(
                [pos2(x, ruler.bottom() - 8.0), pos2(x, ruler.bottom())],
                Stroke::new(1.0, Color32::from_gray(120)),
            );
            let secs = s as u32;
            painter.text(
                pos2(x + 3.0, ruler.top() + 4.0),
                Align2::LEFT_TOP,
                format!(
                    "{:02}:{:02}:{:02}",
                    secs / 60,
                    secs % 60,
                    ((s.fract()) * 30.0) as u32
                ),
                FontId::monospace(10.0),
                Color32::from_gray(170),
            );
            s += step;
        }

        let font = FontId::proportional(11.0);
        for (ti, track) in self.tracks.iter().enumerate() {
            let top = lanes.top() + RULER_H + ti as f32 * (TRACK_H + TRACK_GAP);
            let lane = Rect::from_min_size(pos2(lanes.left(), top), vec2(lane_w, TRACK_H));
            painter.rect_filled(
                lane,
                CornerRadius::ZERO,
                Color32::from_gray(if ti % 2 == 0 { 28 } else { 26 }),
            );
            let clip_painter = painter.with_clip_rect(lane);

            for clip in &track.clips {
                let x0 = x_of(clip.start);
                let x1 = x_of(clip.start + clip.len);
                if self.opts.cull && (x1 < lane.left() || x0 > lane.right()) {
                    continue;
                }
                let body = Rect::from_min_max(
                    pos2(x0, top + 1.0),
                    pos2((x1 - 1.0).max(x0 + 1.0), top + TRACK_H - 1.0),
                );
                clip_painter.rect_filled(
                    body,
                    CornerRadius::same(4),
                    clip.color.gamma_multiply(0.55),
                );
                let media = Rect::from_min_max(pos2(body.left(), body.top() + 16.0), body.max);

                if clip.thumbs {
                    let tw = media.height() * 16.0 / 9.0;
                    let vis_l = body.left().max(lane.left());
                    let vis_r = body.right().min(lane.right());
                    let first = if self.opts.cull {
                        ((vis_l - body.left()) / tw).floor().max(0.0) as usize
                    } else {
                        0
                    };
                    let mut i = first;
                    loop {
                        let tx = body.left() + i as f32 * tw;
                        if tx >= body.right() || (self.opts.cull && tx > vis_r) {
                            break;
                        }
                        let r = Rect::from_min_max(
                            pos2(tx, media.top()),
                            pos2((tx + tw).min(body.right()), media.bottom()),
                        );
                        let shade = ((i * 37 + track.clips.len()) % 60) as u8;
                        clip_painter.rect_filled(
                            r,
                            CornerRadius::ZERO,
                            Color32::from_rgb(40 + shade, 50 + shade / 2, 70),
                        );
                        clip_painter.rect_filled(
                            Rect::from_min_max(pos2(r.left(), r.center().y), r.max),
                            CornerRadius::ZERO,
                            Color32::from_rgb(60, 70 + shade, 50),
                        );
                        clip_painter.line_segment(
                            [r.left_top(), r.left_bottom()],
                            Stroke::new(1.0, Color32::from_gray(15)),
                        );
                        i += 1;
                    }
                }

                if let Some(peaks) = &clip.peaks {
                    let mid = media.center().y;
                    let half = media.height() * 0.45;
                    let (pl, pr) = if self.opts.cull {
                        (body.left().max(lane.left()), body.right().min(lane.right()))
                    } else {
                        (body.left(), body.right())
                    };
                    let stroke = Stroke::new(1.0, clip.color.gamma_multiply(1.3));
                    let mut x = pl;
                    let mut pts: Vec<Pos2> = Vec::new();
                    let mut lower: Vec<Pos2> = Vec::new();
                    while x < pr {
                        let idx = (((x - body.left()) / pps) * PEAKS_PER_SEC) as usize;
                        let p = peaks[idx.min(peaks.len() - 1)];
                        pts.push(pos2(x, mid - p * half));
                        lower.push(pos2(x, mid + p * half));
                        x += 2.0;
                    }
                    if self.opts.cull {
                        pts.extend(lower.into_iter().rev());
                        if pts.len() > 2 {
                            clip_painter.add(Shape::closed_line(pts, stroke));
                        }
                    } else {
                        for (a, b) in pts.iter().zip(&lower) {
                            clip_painter.line_segment([*a, *b], stroke);
                        }
                    }
                }

                clip_painter.rect_stroke(
                    body,
                    CornerRadius::same(4),
                    Stroke::new(1.0, clip.color),
                    StrokeKind::Inside,
                );
                let name_clip = body.shrink(3.0).intersect(lane);
                painter.with_clip_rect(name_clip).text(
                    pos2(body.left().max(lane.left()) + 5.0, body.top() + 3.0),
                    Align2::LEFT_TOP,
                    &clip.name,
                    font.clone(),
                    Color32::from_gray(235),
                );
            }

            // Header drawn after the lane so it sits on top.
            let header = Rect::from_min_size(pos2(full.left(), top), vec2(HEADER_W, TRACK_H));
            painter.rect_filled(header, CornerRadius::ZERO, Color32::from_gray(38));
            painter.text(
                header.left_top() + vec2(10.0, 8.0),
                Align2::LEFT_TOP,
                &track.name,
                FontId::proportional(13.0),
                Color32::from_gray(220),
            );
            for (bi, label) in ["M", "S", "L", "V"].iter().enumerate() {
                let r = Rect::from_min_size(
                    header.left_top() + vec2(10.0 + bi as f32 * 24.0, 34.0),
                    Vec2::splat(20.0),
                );
                ui.put(r, egui::Button::new(*label).small());
            }
        }

        let px = x_of(playhead);
        if px >= lanes.left() && px <= lanes.right() {
            painter.line_segment(
                [pos2(px, lanes.top()), pos2(px, lanes.bottom())],
                Stroke::new(2.0, Color32::from_rgb(255, 80, 60)),
            );
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(px - 6.0, lanes.top()),
                    pos2(px + 6.0, lanes.top()),
                    pos2(px, lanes.top() + 10.0),
                ],
                Color32::from_rgb(255, 80, 60),
                Stroke::NONE,
            ));
        }

        let layer = painter.layer_id();
        ui.ctx()
            .graphics(|g| g.get(layer).map(|l| l.all_entries().len()))
            .unwrap_or(0)
    }
}

impl eframe::App for TimelineApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let now = Instant::now();
        let interval = self.last_frame.map(|l| now - l);
        self.last_frame = Some(now);

        // cpu_usage covers the previous frame: UI build, tessellation and paint
        // submission, excluding time spent waiting on surface acquire/present.
        if self.frame > WARMUP_FRAMES
            && let (Some(cpu), Some(interval)) = (frame.info().cpu_usage, interval)
            && let Ok(mut s) = self.samples.lock()
        {
            s.cpu_ms.push(cpu * 1000.0);
            s.interval_ms.push(interval.as_secs_f32() * 1000.0);
        }

        let build_start = Instant::now();
        let shapes = self.draw(ui);
        let build = build_start.elapsed();
        if self.frame >= WARMUP_FRAMES
            && let Ok(mut s) = self.samples.lock()
        {
            s.build_ms.push(build.as_secs_f32() * 1000.0);
            s.shapes.push(shapes);
        }

        self.frame += 1;
        let enough = self
            .samples
            .lock()
            .map(|s| s.cpu_ms.len() >= self.opts.measure_frames)
            .unwrap_or(true);
        if !self.done && (enough || self.started.elapsed() > TIME_LIMIT) {
            self.done = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint();
    }
}

struct Stats {
    median: f32,
    p95: f32,
    p99: f32,
    max: f32,
    mean: f32,
}

fn stats(v: &[f32]) -> Option<Stats> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    let pct = |p: f32| s[(((p / 100.0) * s.len() as f32).ceil() as usize).clamp(1, s.len()) - 1];
    Some(Stats {
        median: pct(50.0),
        p95: pct(95.0),
        p99: pct(99.0),
        max: s[s.len() - 1],
        mean: s.iter().sum::<f32>() / s.len() as f32,
    })
}

fn row(name: &str, v: &[f32]) -> String {
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

fn parse_opts() -> Opts {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };
    Opts {
        cull: !args.iter().any(|a| a == "--no-cull"),
        measure_frames: value("--frames")
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MEASURE_FRAMES),
        display_hz: value("--display-hz").unwrap_or_else(|| "unknown".into()),
    }
}

fn main() -> eframe::Result {
    let opts = Arc::new(parse_opts());
    let samples = Arc::new(Mutex::new(Samples::default()));
    let (tracks, duration) = make_tracks();
    let clip_count: usize = tracks.iter().map(|t| t.clips.len()).sum();

    let mut native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 900.0])
            .with_title("proto-ui"),
        ..Default::default()
    };
    native.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;

    let backend = Arc::new(Mutex::new(String::from("unknown")));
    let window_px = Arc::new(Mutex::new((0.0f32, 0.0f32, 0.0f32)));
    {
        let samples = samples.clone();
        let opts = opts.clone();
        let backend = backend.clone();
        let window_px = window_px.clone();
        eframe::run_native(
            "proto-ui",
            native,
            Box::new(move |cc| {
                if let (Some(rs), Ok(mut b)) = (cc.wgpu_render_state.as_ref(), backend.lock()) {
                    let info = rs.adapter.get_info();
                    *b = format!("wgpu {:?} on {}", info.backend, info.name);
                }
                let ctx = cc.egui_ctx.clone();
                let window_px = window_px.clone();
                ctx.clone().on_end_pass(
                    "size",
                    Arc::new(move |_| {
                        let r = ctx.content_rect();
                        let ppp = ctx.pixels_per_point();
                        if let Ok(mut w) = window_px.lock() {
                            *w = (r.width(), r.height(), ppp);
                        }
                    }),
                );
                Ok(Box::new(TimelineApp {
                    tracks,
                    duration,
                    frame: 0,
                    started: Instant::now(),
                    last_frame: None,
                    samples,
                    opts,
                    done: false,
                }))
            }),
        )?;
    }

    let s = samples
        .lock()
        .map_err(|_| eframe::Error::AppCreation("poisoned".into()))?;
    let backend = backend.lock().map(|b| b.clone()).unwrap_or_default();
    let (w, h, ppp) = window_px.lock().map(|w| *w).unwrap_or_default();
    let shape_stats = stats(&s.shapes.iter().map(|&n| n as f32).collect::<Vec<_>>());
    let present_ms: Vec<f32> = s
        .interval_ms
        .iter()
        .zip(&s.cpu_ms)
        .map(|(i, c)| (i - c).max(0.0))
        .collect();
    let cpu = stats(&s.cpu_ms);
    let verdict = match &cpu {
        Some(c) if c.median < 4.0 && c.p99 < 8.0 => "PASS (median < 4 ms and p99 < 8 ms)",
        Some(_) => "FAIL (budget is median < 4 ms and p99 < 8 ms)",
        None => "NO DATA",
    };

    let report = format!(
        "# proto-ui results\n\n\
         Timeline stress prototype: {clip_count} clips on {TRACKS} tracks, egui/eframe 0.36, release build.\n\
         Zoom, scroll and playhead change every frame, continuous repaint, vsync off (`PresentMode::AutoNoVsync`).\n\n\
         - Renderer: {backend}\n\
         - Window: {w:.0} x {h:.0} points at {ppp} pixels per point\n\
         - Display refresh rate: {} Hz (the 120 Hz budget is 8.33 ms per frame, so the CPU numbers are what matter)\n\
         - Off-screen culling: {}\n\
         - Warm-up frames: {WARMUP_FRAMES}, measured frames: {}\n\
         - Shapes per frame (median / max): {:.0} / {:.0}\n\n\
         Verdict: **{verdict}**\n\n\
         | Metric (ms) | frames | median | p95 | p99 | max | mean |\n\
         | --- | --- | --- | --- | --- | --- | --- |\n\
         {}\n{}\n{}\n{}\n\n\
         - *Full frame CPU*: eframe's per-frame CPU time (UI build + tessellation + paint submission), excluding surface acquire/present waits.\n\
         - *UI build only*: time spent inside the app's `ui` callback.\n\
         - *Frame interval*: wall-clock time between consecutive frames.\n\
         - *Acquire/present wait*: frame interval minus full frame CPU, a cheap proxy for the GPU/present side.\n",
        opts.display_hz,
        if opts.cull { "on" } else { "off" },
        s.cpu_ms.len(),
        shape_stats.as_ref().map(|s| s.median).unwrap_or(0.0),
        shape_stats.as_ref().map(|s| s.max).unwrap_or(0.0),
        row("Full frame CPU", &s.cpu_ms),
        row("UI build only", &s.build_ms),
        row("Frame interval", &s.interval_ms),
        row("Acquire/present wait", &present_ms),
    );
    println!("{report}");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("RESULTS.md");
    if let Err(err) = std::fs::write(&path, &report) {
        eprintln!("could not write {}: {err}", path.display());
    }
    Ok(())
}
