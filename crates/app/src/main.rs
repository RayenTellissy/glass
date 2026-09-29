//! The `glass` binary: creates the window, the GPU device, the threads and the engine,
//! then hands control to `ui`. Owns command-line flags, safe mode, portable mode and
//! crash minidumps.

fn main() {
    if let Err(error) = engine::logging::init() {
        eprintln!("glass: logging disabled: {error}");
    }
}
