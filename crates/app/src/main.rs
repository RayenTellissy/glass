//! The `glass` binary: creates the window, the GPU device, the threads and the engine,
//! then hands control to `ui`. Owns command-line flags, safe mode, portable mode and
//! crash minidumps.

mod crash;
mod ffi;
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
    let paths = settings::Paths::from_env();
    let settings = settings::Settings::load(&paths);
    let _crash_reporting = settings
        .crash_reports
        .then(|| crash::install(&paths.crashes()));
    crash::trigger_debug_crash();
    ExitCode::SUCCESS
}
