//! Crash reports are opt-in and local: `glass` is made to crash on purpose in a child
//! process (`GLASS_DEBUG_CRASH`, honoured by debug builds only), with its config and data folders pointed at a fresh
//! temporary folder, and the test checks what lands in `crashes/`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("crash-reports-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test folder");
    dir
}

/// Runs `glass` with its folders in `dir`, crashing as `crash` asks. `reports` sets the
/// environment override, or leaves it unset.
fn run_glass(dir: &Path, crash: &str, reports: Option<&str>) -> ExitStatus {
    let mut command = Command::new(env!("CARGO_BIN_EXE_glass"));
    command
        .env("GLASS_CONFIG_DIR", dir)
        .env("GLASS_DATA_DIR", dir)
        .env("GLASS_DEBUG_CRASH", crash)
        .env("GLASS_LOG", "info")
        .env_remove("GLASS_CRASH_REPORTS");
    if let Some(value) = reports {
        command.env("GLASS_CRASH_REPORTS", value);
    }
    command.output().expect("run glass").status
}

fn files_with(dir: &Path, extension: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir.join("crashes"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == extension))
                .collect()
        })
        .unwrap_or_default()
}

/// Waits for the monitor process (which outlives the crashed app briefly) to finish.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "GLASS_DEBUG_CRASH works in debug builds only"
)]
fn native_crash_with_reports_on_writes_a_minidump_and_a_report() {
    let dir = fresh_dir("native-on");
    let status = run_glass(&dir, "segfault", Some("1"));
    assert!(!status.success(), "glass should have crashed: {status:?}");

    wait_for("the minidump and its report", || {
        !files_with(&dir, "dmp").is_empty() && !files_with(&dir, "txt").is_empty()
    });
    let dump = &files_with(&dir, "dmp")[0];
    let bytes = std::fs::read(dump).expect("read minidump");
    assert_eq!(&bytes[..4], b"MDMP", "{dump:?} is not a minidump");
    assert!(bytes.len() > 1024, "minidump is only {} bytes", bytes.len());

    let report = std::fs::read_to_string(&files_with(&dir, "txt")[0]).expect("read report");
    for part in [
        "kind: native crash",
        "version: ",
        "os: ",
        "gpu: ",
        "minidump: crash-",
        // Sent before crash reporting finishes starting; later lines can be up to a
        // second behind a native crash (docs/ARCHITECTURE.md, Crash reports).
        "crash reports: on",
    ] {
        assert!(
            report.contains(part),
            "{part:?} missing from report:\n{report}"
        );
    }
}

#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "GLASS_DEBUG_CRASH works in debug builds only"
)]
fn native_crash_with_reports_off_writes_nothing() {
    let dir = fresh_dir("native-off");
    let status = run_glass(&dir, "segfault", Some("0"));
    assert!(!status.success(), "glass should have crashed: {status:?}");
    // Nothing is started when reports are off, so there is nothing to wait for; give a
    // stray monitor time to show itself anyway.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !dir.join("crashes").exists(),
        "crashes/ was created with reports off"
    );
}

#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "GLASS_DEBUG_CRASH works in debug builds only"
)]
fn reports_are_off_by_default() {
    let dir = fresh_dir("default");
    let status = run_glass(&dir, "panic", None);
    assert!(!status.success(), "glass should have panicked: {status:?}");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!dir.join("crashes").exists(), "crashes/ exists by default");
}

#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "GLASS_DEBUG_CRASH works in debug builds only"
)]
fn panic_with_the_setting_on_writes_a_text_report_only() {
    let dir = fresh_dir("panic-settings");
    std::fs::write(dir.join("settings.json"), r#"{ "crash_reports": true }"#)
        .expect("write settings");
    let status = run_glass(&dir, "panic", None);
    assert!(!status.success(), "glass should have panicked: {status:?}");

    let reports = files_with(&dir, "txt");
    assert_eq!(reports.len(), 1, "{reports:?}");
    let name = reports[0].file_name().unwrap_or_default().to_string_lossy();
    assert!(name.starts_with("panic-"), "{name}");
    let report = std::fs::read_to_string(&reports[0]).expect("read report");
    for part in [
        "kind: panic (no minidump)",
        "deliberate panic requested by GLASS_DEBUG_CRASH",
        "backtrace:",
    ] {
        assert!(
            report.contains(part),
            "{part:?} missing from report:\n{report}"
        );
    }
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        files_with(&dir, "dmp").is_empty(),
        "a panic wrote a minidump"
    );
}
