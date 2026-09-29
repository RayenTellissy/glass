//! The `app` crate's only unsafe code (docs/ARCHITECTURE.md, section 2): attaching the
//! native crash handler, and the deliberate crash used to test it.
#![allow(unsafe_code)]

use std::sync::Arc;

use crash_handler::{CrashContext, CrashEventResult, CrashHandler};
use minidumper::Client;

/// Attaches the crash handler: on a crash it asks the monitor process to write a
/// minidump of this process.
pub fn attach_crash_handler(client: Arc<Client>) -> Result<CrashHandler, crash_handler::Error> {
    // SAFETY: the closure runs in a crash context (a signal handler on Linux, the
    // exception thread on macOS, the exception filter on Windows). It only sends one
    // pre-connected IPC request and allocates nothing, as `CrashEvent` requires.
    let event = unsafe {
        crash_handler::make_crash_event(move |context: &CrashContext| {
            CrashEventResult::Handled(client.request_dump(context).is_ok())
        })
    };
    CrashHandler::attach(event)
}

/// Crashes the process with an invalid memory read, for testing crash reports. Debug
/// builds only.
#[cfg(debug_assertions)]
pub fn crash_with_invalid_read() -> ! {
    // A non-null address in the first page, which is never mapped on any supported OS.
    let address = 0x10usize as *const u32;
    // SAFETY: not safe at all, on purpose: this read faults and the process dies.
    let value = unsafe { address.read_volatile() };
    // Unreachable in practice; keeps the read from being optimised away.
    std::process::exit(value as i32)
}
