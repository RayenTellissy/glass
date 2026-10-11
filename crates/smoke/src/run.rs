//! Playing and exporting the reference sequence headless, through the same engine paths
//! the app uses: `TimelinePlayback` for play, the look-ahead and `TimelineRenderer` for
//! export frames, `media::export` to encode them and `media::export::verify` to read the
//! file back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::audio::Clock;
use engine::lookahead::{
    GopIndexes, Lookahead, LookaheadConfig, MediaDecoder, MediaDecoderOptions,
};
use engine::media::analysis::Unattended;
use engine::media::export::presets::{Preset, TimelineFormat};
use engine::media::export::verify::{self, Expected};
use engine::media::export::{EncoderPreference, ExportSession, Picture};
use engine::quality::QualityLevel;
use engine::render::readback::{ReadbackConfig, ReadbackRing};
use engine::render::wgpu;
use engine::timeline::{
    RendererConfig, TimelineConfig, TimelinePlan, TimelinePlayback, TimelineRenderer,
};

/// The longest wait for any one frame.
const WAIT: Duration = Duration::from_secs(20);
/// The export preset: H.264 in MP4, encoded in software.
const PRESET: &str = "youtube_1080p";

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// A device with 16-bit normalised textures (10-bit sources upload as R16 planes), or
/// `None` without a suitable adapter.
pub fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    if !adapter
        .features()
        .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
    {
        return None;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
        ..Default::default()
    }))
    .ok()?;
    Some(Gpu { device, queue })
}

/// The sequence's frames per second.
fn fps(plan: &TimelinePlan) -> f64 {
    plan.rate().to_f64().unwrap_or(30.0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Plays `frames` timeline frames from the start, paced by the system clock and held
/// until due like a display would. Fails on a dropped frame, a decode error or a frame
/// rendered without its picture; late frames are reported.
pub fn play(gpu: &Gpu, plan: &Arc<TimelinePlan>, frames: i64) -> Result<(), String> {
    let started = Instant::now();
    let clock = Clock::system(48_000);
    let (playback, mut presented) = TimelinePlayback::start(
        plan.clone(),
        &gpu.device,
        &gpu.queue,
        clock.clone(),
        TimelineConfig::new(plan.canvas()),
    )
    .map_err(|e| format!("playback did not start: {e}"))?;
    let pop = |presented: &mut engine::timeline::Presented| {
        presented
            .pop_timeout(WAIT)
            .map(|s| s.frame)
            .map_err(|e| format!("no frame from playback: {e:?}"))
    };
    let first = pop(&mut presented)?;
    let ready = started.elapsed();
    playback.play();
    let playing = Instant::now();
    let mut last = first.frame;
    let mut shown = 0;
    while last < frames {
        let frame = pop(&mut presented)?;
        shown += 1;
        if let Some(due) = frame.due {
            let ahead = due - clock.now();
            if ahead > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(ahead));
            }
        }
        last = frame.frame;
    }
    let played = playing.elapsed();
    let stats = playback.stats();
    let quality = playback.quality();
    let lookahead = playback.lookahead_stats();
    let (render, _) = playback.render_stats();
    playback.shutdown();
    println!(
        "smoke: played frames 0..={last} ({:.2} s of timeline) in {:.2} s: {shown} shown, \
         {} late, {} dropped, level {:?}, render mean {:.1} ms max {:.1} ms, first frame \
         after {:.0} ms",
        last as f64 / fps(plan),
        played.as_secs_f64(),
        stats.late,
        stats.dropped,
        quality.level,
        ms(stats.render_total) / stats.presented.max(1) as f64,
        ms(stats.render_max),
        ms(ready),
    );
    if stats.dropped > 0 {
        return Err(format!("playback dropped {} frames", stats.dropped));
    }
    if lookahead.decode_errors > 0 {
        return Err(format!(
            "{} decode errors in playback",
            lookahead.decode_errors
        ));
    }
    if render.missing_pictures > 0 || render.upload_errors > 0 || render.unknown_transitions > 0 {
        return Err(format!("playback rendered incomplete frames: {render:?}"));
    }
    Ok(())
}

/// Exports `range` of the timeline with the [`PRESET`] in software H.264 (audio is
/// silence: the engine has no audio mix yet), then verifies the file.
pub fn export(
    gpu: &Gpu,
    plan: &Arc<TimelinePlan>,
    range: std::ops::Range<i64>,
) -> Result<(), String> {
    let started = Instant::now();
    let preset = Preset::built_in(PRESET).ok_or_else(|| format!("no preset {PRESET}"))?;
    let rate = plan.rate();
    let (width, height) = plan.canvas();
    let mut settings = preset
        .resolve(&TimelineFormat {
            width,
            height,
            frame_rate: engine::media::Rational::new(rate.num, rate.den),
            audio_channels: 2,
        })
        .map_err(|e| e.to_string())?;
    if let Some(video) = &mut settings.video {
        video.encoder_preference = EncoderPreference::Software;
    }
    let (sample_rate, channels) = settings
        .audio
        .as_ref()
        .map_or((48_000, 2), |a| (a.sample_rate, a.channels));
    let path = std::env::temp_dir().join(format!(
        "glass-smoke-{}.{}",
        std::process::id(),
        settings.container.extension()
    ));
    let result = encode(
        gpu,
        plan,
        range.clone(),
        &settings,
        &path,
        sample_rate,
        channels,
    )
    .and_then(|(summary, timings)| {
        let encoded = started.elapsed();
        let frames = (range.end - range.start) as u64;
        let duration = frames as f64 / fps(plan);
        let report = verify::verify(
            &path,
            &Expected::new(&settings, frames, duration).with_preset(preset),
            &Unattended,
        )
        .map_err(|e| format!("verify: {e}"))?;
        println!(
            "smoke: exported frames {}..{} ({duration:.2} s) with {PRESET} through {} \
                 and {} in {:.0} ms (decode wait {:.0} ms, render {:.0} ms, readback {:.0} ms, \
                 encode {:.0} ms), {} bytes; audio is silence (no audio mix in the engine yet)",
            range.start,
            range.end,
            summary.video_encoder.as_deref().unwrap_or("?"),
            summary.audio_encoder.as_deref().unwrap_or("?"),
            ms(encoded),
            ms(timings.decode),
            ms(timings.render),
            ms(timings.readback),
            ms(timings.encode),
            summary.bytes,
        );
        println!(
            "smoke: verified in {:.0} ms: {} frames, video {:.3} s, audio {:.3} s, \
                 {} checks, {} failures, {} warnings{}",
            ms(started.elapsed() - encoded),
            report.measured.frames.unwrap_or(0),
            report.measured.video_duration.unwrap_or(0.0),
            report.measured.audio_duration.unwrap_or(0.0),
            report.checked.len(),
            report.failures().count(),
            report.warnings().count(),
            if report.warnings().count() > 0 {
                format!(
                    " ({})",
                    report
                        .warnings()
                        .map(|w| w.key)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                String::new()
            },
        );
        if !report.passed() {
            let failures: Vec<String> = report
                .failures()
                .map(|f| format!("{} {:?}", f.key, f.args))
                .collect();
            return Err(format!(
                "the export failed verification: {}",
                failures.join("; ")
            ));
        }
        Ok(())
    });
    let _ = std::fs::remove_file(&path);
    result
}

#[derive(Default)]
struct Timings {
    decode: Duration,
    render: Duration,
    readback: Duration,
    encode: Duration,
}

fn encode(
    gpu: &Gpu,
    plan: &Arc<TimelinePlan>,
    range: std::ops::Range<i64>,
    settings: &engine::media::export::ExportSettings,
    path: &std::path::Path,
    sample_rate: u32,
    channels: usize,
) -> Result<(engine::media::export::ExportSummary, Timings), String> {
    let mut session = ExportSession::create(path, settings).map_err(|e| e.to_string())?;
    let rate = plan.rate();
    let options = MediaDecoderOptions {
        project_rate: engine::media::Rational::new(rate.num, rate.den),
        ..MediaDecoderOptions::default()
    };
    let gops = GopIndexes::new();
    let (lookahead, mut frames) = Lookahead::start(
        plan.segments().clone(),
        LookaheadConfig {
            frame_duration: plan.frame_duration(),
            ..LookaheadConfig::default()
        },
        |_| MediaDecoder::new(options, gops.clone()),
    )
    .map_err(|e| format!("look-ahead did not start: {e}"))?;
    let generation = lookahead.seek(range.start);
    let mut renderer =
        TimelineRenderer::new(&gpu.device, &gpu.queue, RendererConfig::new(plan.canvas()));
    let mut ring = ReadbackRing::new(ReadbackConfig::EXPORT);
    let mut layers = Vec::new();
    let mut timings = Timings::default();
    let per_frame = (f64::from(sample_rate) / fps(plan)).round() as usize;
    let silence = vec![0.0f32; per_frame];
    let planes: Vec<&[f32]> = (0..channels).map(|_| silence.as_slice()).collect();
    let mut t = range.start;
    let mut waited = Instant::now();
    while t < range.end {
        let stamp = frames
            .next_into(&mut layers)
            .map_err(|e| format!("no frame {t} from the look-ahead: {e:?}"))?;
        if stamp.generation != generation || stamp.index != t {
            continue;
        }
        timings.decode += waited.elapsed();
        let rendering = Instant::now();
        let rendered = renderer.render(plan, t, &layers, QualityLevel::Full);
        timings.render += rendering.elapsed();
        let reading = Instant::now();
        ring.submit(&gpu.device, &gpu.queue, &rendered.texture, t as u64)
            .map_err(|e| e.to_string())?;
        let frame = ring
            .wait(&gpu.device)
            .map_err(|e| e.to_string())?
            .ok_or("the readback ring lost a frame")?;
        timings.readback += reading.elapsed();
        let encoding = Instant::now();
        session
            .push_picture(&Picture::rgba8(
                frame.width,
                frame.height,
                &frame.data,
                frame.width as usize * 4,
            ))
            .map_err(|e| e.to_string())?;
        session.push_audio(&planes).map_err(|e| e.to_string())?;
        timings.encode += encoding.elapsed();
        ring.recycle(frame);
        t += 1;
        waited = Instant::now();
    }
    let stats = frames.stats();
    let render = renderer.stats();
    lookahead.shutdown();
    if stats.decode_errors > 0 {
        return Err(format!("{} decode errors in export", stats.decode_errors));
    }
    if render.missing_pictures > 0 || render.upload_errors > 0 || render.unknown_transitions > 0 {
        return Err(format!("export rendered incomplete frames: {render:?}"));
    }
    let encoding = Instant::now();
    let summary = session.finish().map_err(|e| e.to_string())?;
    timings.encode += encoding.elapsed();
    Ok((summary, timings))
}
