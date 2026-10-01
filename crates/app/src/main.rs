//! The `glass` binary: creates the window, the GPU device, the threads and the engine,
//! then hands control to `ui`. Owns command-line flags, safe mode, portable mode and
//! crash minidumps.

mod crash;
mod ffi;
mod locations;
mod recovery;
mod settings;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    // The crash monitor is this same binary started with a flag; it does nothing else.
    if args.get(1).is_some_and(|a| a == crash::MONITOR_ARG) {
        return crash::run_monitor(&args[2..]);
    }

    if let Err(error) = engine::logging::init() {
        eprintln!("glass: logging disabled: {error}");
    }
    let locations = locations::Locations::from_env();
    let settings = settings::Settings::load(&locations);
    let _crash_reporting = settings
        .crash_reports
        .then(|| crash::install(&locations.crashes(), &locations.data));
    crash::trigger_debug_crash();
    // The window shell shows these one at a time over the welcome screen once it exists.
    let pending = recovery::LaunchRecovery::scan(&locations.cache, engine::autosave::now_ms());
    for launch in &pending {
        tracing::info!(
            "autosave of {} with {} unsaved edits waits for the recovery prompt",
            launch.prompt.project_name,
            launch.prompt.edits
        );
    }
    ExitCode::SUCCESS
}
