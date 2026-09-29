//! Headless check binary: opens the reference project, plays a short range and exports
//! a short range without a window, then exits 0. It starts as a stub and grows with
//! each phase.

use std::process::ExitCode;

use engine::logging::{self, LogFilter};

const STEPS: &[&str] = &[
    "open tests/fixtures/reference.proj",
    "play 2 s headless",
    "export 1 s",
];

fn main() -> ExitCode {
    let ring = match logging::init() {
        Ok(ring) => ring,
        Err(error) => {
            eprintln!("smoke: logging: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!("smoke started");
    // GLASS_LOG may silence info; the ring is only checked when the event passed the filter.
    if tracing::enabled!(tracing::Level::INFO) && ring.snapshot(&LogFilter::default()).is_empty() {
        eprintln!("smoke: the log ring did not record the start event");
        return ExitCode::FAILURE;
    }

    println!("smoke: stub, no checks run yet. Once implemented it will:");
    for step in STEPS {
        println!("  - {step}");
    }
    println!("smoke: ok (stub)");
    ExitCode::SUCCESS
}
