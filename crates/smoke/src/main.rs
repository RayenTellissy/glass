//! Headless check binary: opens the reference project (`tests/fixtures/reference.proj`),
//! mixes 2 s of its audio offline and checks the tone's level, plays 2 s of it and exports
//! 1 s (with the mixed audio) without a window, verifies the export, then exits 0.
//!
//! `smoke --write-reference [path]` writes the reference project instead (to
//! `tests/fixtures/reference.proj` by default), from the generator in `reference.rs`.
//!
//! Without a GPU adapter play and export are skipped with a message, unless
//! `GLASS_REQUIRE_GPU=1`; missing fixtures always fail.

mod reference;
mod run;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use engine::logging::{self, LogFilter};
use engine::timeline::TimelinePlan;

/// Timeline frames played: 2 s at the reference's 30 fps.
const PLAY_FRAMES: i64 = 2 * reference::FPS;
/// Seconds of the sequence mixed offline and checked against the A1 tone.
const MIX_SECONDS: i64 = 2;

fn main() -> ExitCode {
    let ring = match logging::init() {
        Ok(ring) => ring,
        Err(error) => {
            eprintln!("smoke: logging: {error}");
            return ExitCode::FAILURE;
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--write-reference") {
        let path = args
            .get(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| reference::repo_root().join(reference::FILE));
        return match reference::write(&path) {
            Ok(()) => {
                println!("smoke: wrote {}", path.display());
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("smoke: {message}");
                ExitCode::FAILURE
            }
        };
    }
    if let Some(arg) = args.first() {
        eprintln!("smoke: unknown argument {arg}; usage: smoke [--write-reference [path]]");
        return ExitCode::FAILURE;
    }

    tracing::info!("smoke started");
    // GLASS_LOG may silence info; the ring is only checked when the event passed the filter.
    if tracing::enabled!(tracing::Level::INFO) && ring.snapshot(&LogFilter::default()).is_empty() {
        eprintln!("smoke: the log ring did not record the start event");
        return ExitCode::FAILURE;
    }

    match check() {
        Ok(()) => {
            println!("smoke: ok");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("smoke: FAILED: {message}");
            ExitCode::FAILURE
        }
    }
}

fn check() -> Result<(), String> {
    let started = Instant::now();
    let path = reference::repo_root().join(reference::FILE);
    let opened = reference::open(&path)?;
    let plan = TimelinePlan::build(&opened.project, opened.sequence).map_err(|e| e.to_string())?;
    let sequence = opened
        .project
        .sequence(opened.sequence)
        .ok_or("the sequence vanished")?;
    let clips = sequence.tracks.iter().map(|t| t.items.len()).sum::<usize>();
    let transitions = sequence
        .tracks
        .iter()
        .map(|t| t.transitions.len())
        .sum::<usize>();
    let (w, h) = plan.canvas();
    println!(
        "smoke: opened {} in {:.0} ms: {w}x{h} at {} fps, {} frames, {} tracks, {clips} clips, \
         {transitions} transitions, {} markers, {} decode segments",
        reference::FILE,
        started.elapsed().as_secs_f64() * 1000.0,
        plan.rate().to_f64().unwrap_or(0.0),
        plan.end(),
        sequence.tracks.len(),
        sequence.markers.len(),
        plan.segments().segments().len(),
    );
    if plan.missing_media() > 0 || plan.skipped_effects() > 0 {
        return Err(format!(
            "the plan left out {} clips without media and {} effects",
            plan.missing_media(),
            plan.skipped_effects()
        ));
    }

    run::mix_check(&opened.project, opened.sequence, MIX_SECONDS)?;

    let Some(gpu) = run::gpu() else {
        if std::env::var_os("GLASS_REQUIRE_GPU").is_some_and(|v| v == "1") {
            return Err("no GPU adapter with 16-bit normalised textures".to_owned());
        }
        println!("smoke: SKIPPED play and export: no GPU adapter with 16-bit normalised textures");
        return Ok(());
    };
    let plan = Arc::new(plan);
    run::play(&gpu, &plan, PLAY_FRAMES)?;
    run::export(
        &gpu,
        &plan,
        &opened.project,
        opened.sequence,
        reference::EXPORT,
    )?;
    println!("smoke: total {:.2} s", started.elapsed().as_secs_f64());
    Ok(())
}
