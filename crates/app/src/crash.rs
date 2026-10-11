//! Opt-in crash reports, written to the user data folder's `crashes/` and never sent
//! anywhere. There is no network code here: the app and its monitor talk over a local
//! socket (and a Mach port on macOS).
//!
//! When the user has turned crash reports on (see [`app::settings`]):
//!
//! - `glass` starts a copy of itself as a monitor process (`glass --crash-monitor
//!   <socket> <folder>`) and attaches a native crash handler. On a crash the handler asks
//!   the monitor to write a minidump of the crashed process (`crash-<time>-<pid>.dmp`)
//!   and the monitor writes a text report beside it (`.txt`: version, OS, GPU adapter
//!   if known, the last log lines). Out of process, because a crashed process cannot be
//!   trusted to write its own dump.
//! - A panic hook writes a text report (`panic-<time>-<pid>-<n>.txt`, with the panic
//!   message, location and backtrace) and no minidump: a panic is not a memory fault.
//!
//! When crash reports are off, none of this is installed and nothing is written.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use engine::logging::{LogEvent, LogFilter};
use minidumper::{Client, LoopAction, MinidumpBinary, Server, ServerHandler, SocketName};
use serde::{Deserialize, Serialize};

/// First argument that turns `glass` into the crash monitor.
pub const MONITOR_ARG: &str = "--crash-monitor";
/// For testing crash reports: `segfault`, `abort` or `panic` makes a debug build of
/// `glass` crash that way right after startup. Release builds ignore it.
pub const DEBUG_CRASH_ENV: &str = "GLASS_DEBUG_CRASH";

/// Longest socket path, in bytes: `sockaddr_un.sun_path` holds 104 bytes on macOS and
/// 108 on Linux, including the terminating NUL.
const MAX_SOCKET_PATH: usize = 103;

/// Log lines kept in a report.
const LOG_LINES: usize = 200;
/// How often the app sends new log lines to the monitor.
const LOG_INTERVAL: Duration = Duration::from_secs(1);
/// The monitor exits if the app has not connected by then.
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
/// The monitor drops an app connection silent for this long (the app pings every
/// [`LOG_INTERVAL`]).
const STALE_TIMEOUT: Duration = Duration::from_secs(60);

const MSG_CONTEXT: u32 = 1;
const MSG_LOG: u32 = 2;

/// Facts about the running app for a report.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Context {
    version: String,
    os: String,
    gpu: Option<String>,
}

impl Context {
    fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            os: os_description(),
            gpu: GPU.lock().ok().and_then(|g| g.clone()),
        }
    }
}

static GPU: Mutex<Option<String>> = Mutex::new(None);
static INFO_CLIENT: Mutex<Option<Arc<Client>>> = Mutex::new(None);

/// Records the GPU adapter for crash reports, once the renderer knows it.
pub fn set_gpu_adapter(name: impl Into<String>) {
    if let Ok(mut gpu) = GPU.lock() {
        *gpu = Some(name.into());
    }
    if let Some(client) = INFO_CLIENT.lock().ok().and_then(|c| c.clone()) {
        send_context(&client);
    }
}

fn send_context(client: &Client) {
    if let Ok(json) = serde_json::to_vec(&Context::current()) {
        let _ = client.send_message(MSG_CONTEXT, json);
    }
}

/// Crash reporting while it is alive; dropping it detaches the handler and lets the
/// monitor exit.
pub struct CrashReporting {
    // Field order is drop order: stop the log thread, detach the handler, disconnect.
    stop: Option<mpsc::Sender<()>>,
    log_thread: Option<JoinHandle<()>>,
    handler: Option<crash_handler::CrashHandler>,
    _monitor: Option<Child>,
}

impl Drop for CrashReporting {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.log_thread.take() {
            let _ = thread.join();
        }
        self.handler.take();
        if let Ok(mut client) = INFO_CLIENT.lock() {
            client.take();
        }
    }
}

/// Turns crash reporting on: the panic hook always, the minidump monitor when it starts.
/// Reports go to `dir`; the monitor's socket goes in a private folder under the
/// runtime folder or `data` (see [`socket_path`]).
pub fn install(dir: &Path, data: &Path) -> CrashReporting {
    let mut reporting = CrashReporting {
        stop: None,
        log_thread: None,
        handler: None,
        _monitor: None,
    };
    if let Err(error) = std::fs::create_dir_all(dir) {
        tracing::warn!("crash reports: cannot create {}: {error}", dir.display());
        return reporting;
    }
    install_panic_hook(dir.to_path_buf());

    let (monitor, crash_client, info_client) = match start_monitor(dir, data) {
        Ok(started) => started,
        Err(error) => {
            tracing::warn!(
                "crash reports: minidumps unavailable ({error}); panics are still reported"
            );
            return reporting;
        }
    };
    match crate::ffi::attach_crash_handler(crash_client) {
        Ok(handler) => reporting.handler = Some(handler),
        Err(error) => {
            tracing::warn!("crash reports: cannot attach the crash handler: {error}");
        }
    }
    tracing::info!("crash reports: on, written to {}", dir.display());
    // Context and the log so far go out before install returns, so even a crash right
    // after startup has them in its report.
    send_context(&info_client);
    let mut forwarder = LogForwarder::new();
    let _ = forwarder.tick(&info_client);
    if let Ok(mut client) = INFO_CLIENT.lock() {
        *client = Some(info_client.clone());
    }
    let (stop, stopped) = mpsc::channel();
    reporting.log_thread = std::thread::Builder::new()
        .name("glass-crash-log".into())
        .spawn(move || forwarder.run(&info_client, &stopped))
        .ok();
    reporting.stop = Some(stop);
    reporting._monitor = Some(monitor);
    reporting
}

/// Where the app and its monitor meet: `crash-<pid>.sock` in a folder only this user can
/// open (created `0700` on Unix), never in a shared `/tmp`. The first candidate whose
/// path fits in [`MAX_SOCKET_PATH`] wins:
///
/// - Unix: `$XDG_RUNTIME_DIR/glass` when that variable is set, then `<data>/run`, then
///   on macOS only the per-user temporary folder (`$TMPDIR`, under `/var/folders`).
/// - Windows: the per-user temporary folder (`%TEMP%`).
fn socket_path(
    pid: u32,
    runtime: Option<OsString>,
    data: &Path,
    temp: &Path,
) -> Result<PathBuf, String> {
    let name = format!("crash-{pid}.sock");
    let mut candidates = Vec::new();
    if cfg!(unix) {
        if let Some(runtime) = runtime.filter(|r| Path::new(r).is_absolute()) {
            candidates.push(PathBuf::from(runtime).join("glass"));
        }
        candidates.push(data.join("run"));
        if cfg!(target_os = "macos") {
            candidates.push(temp.join("glass"));
        }
    } else {
        candidates.push(temp.to_path_buf());
    }
    let mut problems = Vec::new();
    for folder in candidates {
        let socket = folder.join(&name);
        let len = socket.as_os_str().len();
        if len > MAX_SOCKET_PATH {
            problems.push(format!(
                "{} is {len} bytes, over the {MAX_SOCKET_PATH}-byte socket path limit",
                socket.display()
            ));
            continue;
        }
        match private_dir(&folder) {
            Ok(()) => return Ok(socket),
            Err(error) => problems.push(format!("{}: {error}", folder.display())),
        }
    }
    Err(format!(
        "no folder for the socket ({})",
        problems.join("; ")
    ))
}

/// Creates `dir` if needed and, on Unix, makes it a real folder readable only by us.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.is_dir() {
            return Err(std::io::Error::other("not a folder"));
        }
        if meta.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

fn start_monitor(dir: &Path, data: &Path) -> Result<(Child, Arc<Client>, Arc<Client>), String> {
    let socket = socket_path(
        std::process::id(),
        std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()),
        data,
        &std::env::temp_dir(),
    )?;
    let socket_str = socket
        .to_str()
        .ok_or("the socket path is not UTF-8")?
        .to_owned();
    let _ = std::fs::remove_file(&socket);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut monitor = Command::new(exe)
        .arg(MONITOR_ARG)
        .arg(&socket)
        .arg(dir)
        .arg(std::process::id().to_string())
        .env_remove(DEBUG_CRASH_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start the monitor: {e}"))?;

    let connect = || Client::with_name(SocketName::path(&socket_str));
    let deadline = Instant::now() + Duration::from_secs(5);
    let crash_client = loop {
        match connect() {
            Ok(client) => break client,
            Err(error) if Instant::now() >= deadline => {
                let _ = monitor.kill();
                return Err(format!("cannot reach the monitor: {error}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // A second connection for messages, so a crash request never interleaves with a
    // log message on the same socket.
    let info_client = connect().map_err(|e| format!("cannot reach the monitor: {e}"))?;
    Ok((monitor, Arc::new(crash_client), Arc::new(info_client)))
}

/// Sends new log lines from the ring to the monitor, keeping the last [`LOG_LINES`].
struct LogForwarder {
    lines: VecDeque<String>,
    next: u64,
}

impl LogForwarder {
    fn new() -> Self {
        Self {
            lines: VecDeque::with_capacity(LOG_LINES),
            next: 0,
        }
    }

    /// Sends the lines when new ones arrived, otherwise pings so the monitor knows the
    /// app is alive. Fails when the monitor is gone.
    fn tick(&mut self, client: &Client) -> Result<(), minidumper::Error> {
        let mut changed = false;
        if let Some(ring) = engine::logging::ring() {
            let batch = ring.since(self.next, &LogFilter::default());
            self.next = batch.next;
            for event in batch.events {
                if self.lines.len() == LOG_LINES {
                    self.lines.pop_front();
                }
                self.lines.push_back(format_event(&event));
                changed = true;
            }
        }
        if changed {
            let text: Vec<&str> = self.lines.iter().map(String::as_str).collect();
            client.send_message(MSG_LOG, text.join("\n"))
        } else {
            client.ping()
        }
    }

    fn run(mut self, client: &Client, stopped: &mpsc::Receiver<()>) {
        while self.tick(client).is_ok() {
            match stopped.recv_timeout(LOG_INTERVAL) {
                Err(RecvTimeoutError::Timeout) => {}
                _ => return,
            }
        }
    }
}

fn format_event(event: &LogEvent) -> String {
    let mut line = format!(
        "{} {:>5} {}: {}",
        utc_timestamp(event.time),
        event.level,
        event.target,
        event.message
    );
    if !event.fields.is_empty() {
        let _ = write!(line, " {}", event.fields);
    }
    line
}

fn recent_log_lines() -> Vec<String> {
    engine::logging::ring()
        .map(|ring| {
            let events = ring.snapshot(&LogFilter::default());
            let skip = events.len().saturating_sub(LOG_LINES);
            events[skip..].iter().map(format_event).collect()
        })
        .unwrap_or_default()
}

fn install_panic_hook(dir: PathBuf) {
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let context_os = os_description();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_owned());
        let location = info
            .location()
            .map_or_else(String::new, |l| format!(" at {}:{}", l.file(), l.line()));
        let thread = std::thread::current();
        let mut context = Context::current();
        context.os = context_os.clone();
        let detail = format!(
            "panic on thread {}{location}: {payload}\n\nbacktrace:\n{}",
            thread.name().unwrap_or("unnamed"),
            std::backtrace::Backtrace::force_capture()
        );
        let path = dir.join(format!("panic-{}-{n}.txt", report_stem(std::process::id())));
        let report = report_text(
            "panic (no minidump)",
            &context,
            &detail,
            &recent_log_lines(),
        );
        if std::fs::write(&path, report).is_ok() {
            eprintln!("glass: crash report written to {}", path.display());
        }
        previous(info);
    }));
}

fn report_text(kind: &str, context: &Context, detail: &str, log: &[String]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Glass crash report");
    let _ = writeln!(
        out,
        "This report stays on this computer. Glass never sends it anywhere."
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "kind: {kind}");
    let _ = writeln!(out, "time: {}", utc_timestamp(SystemTime::now()));
    let _ = writeln!(out, "version: {}", context.version);
    let _ = writeln!(out, "os: {}", context.os);
    let _ = writeln!(out, "gpu: {}", context.gpu.as_deref().unwrap_or("unknown"));
    if !detail.is_empty() {
        let _ = writeln!(out, "\n{detail}");
    }
    let _ = writeln!(out, "\nlast {} log lines:", log.len());
    for line in log {
        let _ = writeln!(out, "{line}");
    }
    out
}

/// `<UTC time>-<pid>`, safe in file names on every OS.
fn report_stem(pid: u32) -> String {
    format!(
        "{}-{pid}",
        utc_timestamp(SystemTime::now()).replace(':', "")
    )
}

/// `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_timestamp(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

fn os_description() -> String {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let version = if cfg!(target_os = "macos") {
        std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
            .ok()
            .and_then(|plist| {
                let after = plist.split("<key>ProductVersion</key>").nth(1)?;
                let start = after.find("<string>")? + "<string>".len();
                let end = after[start..].find("</string>")?;
                Some(after[start..start + end].trim().to_owned())
            })
    } else if cfg!(target_os = "linux") {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|release| {
                release.lines().find_map(|line| {
                    line.strip_prefix("PRETTY_NAME=")
                        .map(|v| v.trim_matches('"').to_owned())
                })
            })
    } else {
        None
    };
    match version {
        Some(version) => format!("{os} {version} ({arch})"),
        None => format!("{os} ({arch})"),
    }
}

/// Makes a debug build of `glass` crash on purpose when [`DEBUG_CRASH_ENV`] asks for it.
/// Release builds compile this to nothing, so a shipped `glass` ignores the variable.
#[cfg(not(debug_assertions))]
pub fn trigger_debug_crash() {}

/// Makes a debug build of `glass` crash on purpose when [`DEBUG_CRASH_ENV`] asks for it.
#[cfg(debug_assertions)]
pub fn trigger_debug_crash() {
    let Ok(kind) = std::env::var(DEBUG_CRASH_ENV) else {
        return;
    };
    tracing::warn!("{DEBUG_CRASH_ENV}={kind}: crashing on purpose");
    match kind.as_str() {
        "segfault" => crate::ffi::crash_with_invalid_read(),
        "abort" => std::process::abort(),
        "panic" => panic!("deliberate panic requested by {DEBUG_CRASH_ENV}"),
        other => tracing::warn!("{DEBUG_CRASH_ENV}: unknown value {other:?}"),
    }
}

/// The monitor process: `glass --crash-monitor <socket> <folder> <app pid>`.
pub fn run_monitor(args: &[OsString]) -> ExitCode {
    let [socket, dir, pid] = args else {
        eprintln!("glass: usage: {MONITOR_ARG} <socket> <folder> <app pid>");
        return ExitCode::FAILURE;
    };
    let app_pid = pid.to_str().and_then(|p| p.parse().ok()).unwrap_or(0);
    let Some(socket_str) = socket.to_str() else {
        eprintln!("glass crash monitor: socket path is not UTF-8");
        return ExitCode::FAILURE;
    };
    let _ = std::fs::remove_file(socket);
    let mut server = match Server::with_name(SocketName::path(socket_str)) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("glass crash monitor: {error}");
            return ExitCode::FAILURE;
        }
    };

    static SHUTDOWN: AtomicBool = AtomicBool::new(false);
    static CONNECTED: AtomicBool = AtomicBool::new(false);
    let _ = std::thread::Builder::new()
        .name("glass-crash-deadline".into())
        .spawn(|| {
            std::thread::sleep(CONNECT_DEADLINE);
            if !CONNECTED.load(Ordering::Acquire) {
                SHUTDOWN.store(true, Ordering::Release);
            }
        });

    let monitor = Arc::new(Monitor {
        dir: PathBuf::from(dir),
        app_pid,
        state: Mutex::new(MonitorState::default()),
        connected: &CONNECTED,
    });
    let result = server.run(
        Box::new(MonitorHandle(monitor.clone())),
        &SHUTDOWN,
        Some(STALE_TIMEOUT),
    );
    // If the app's connections went stale instead of closing, the report is still owed.
    monitor.write_report();
    let _ = std::fs::remove_file(socket);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("glass crash monitor: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Default)]
struct MonitorState {
    context: Context,
    log: Vec<String>,
    dump: Option<PathBuf>,
    /// What happened to the dump, until the text report is written.
    pending: Option<String>,
}

struct Monitor {
    dir: PathBuf,
    app_pid: u32,
    state: Mutex<MonitorState>,
    connected: &'static AtomicBool,
}

impl Monitor {
    /// Writes the text report for a dump, once. Called after the app's connections
    /// close, so every message it sent before crashing has been read (on macOS the dump
    /// request travels on a Mach port and can overtake them).
    fn write_report(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(detail) = state.pending.take() else {
            return;
        };
        let path = state.dump.as_ref().map_or_else(
            || {
                self.dir
                    .join(format!("crash-{}.txt", report_stem(self.app_pid)))
            },
            |p| p.with_extension("txt"),
        );
        let report = report_text("native crash", &state.context, &detail, &state.log);
        let _ = std::fs::write(path, report);
    }
}

struct MonitorHandle(Arc<Monitor>);

impl ServerHandler for MonitorHandle {
    fn create_minidump_file(&self) -> Result<(File, PathBuf), std::io::Error> {
        let monitor = &self.0;
        std::fs::create_dir_all(&monitor.dir)?;
        let path = monitor
            .dir
            .join(format!("crash-{}.dmp", report_stem(monitor.app_pid)));
        let file = File::create(&path)?;
        if let Ok(mut state) = monitor.state.lock() {
            state.dump = Some(path.clone());
        }
        Ok((file, path))
    }

    fn on_minidump_created(&self, result: Result<MinidumpBinary, minidumper::Error>) -> LoopAction {
        let detail = match result {
            Ok(mut dump) => {
                use std::io::Write as _;
                let _ = dump.file.flush();
                let name = dump
                    .path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                format!("minidump: {name}")
            }
            Err(error) => format!("minidump could not be written: {error}"),
        };
        if let Ok(mut state) = self.0.state.lock() {
            state.pending = Some(detail);
        }
        // Keep reading until the crashed app's connections close.
        LoopAction::Continue
    }

    fn on_message(&self, kind: u32, buffer: Vec<u8>) {
        let Ok(mut state) = self.0.state.lock() else {
            return;
        };
        match kind {
            MSG_CONTEXT => {
                if let Ok(context) = serde_json::from_slice(&buffer) {
                    state.context = context;
                }
            }
            MSG_LOG => {
                state.log = String::from_utf8_lossy(&buffer)
                    .lines()
                    .map(str::to_owned)
                    .collect();
            }
            _ => {}
        }
    }

    fn on_client_connected(&self, _num_clients: usize) -> LoopAction {
        self.0.connected.store(true, Ordering::Release);
        LoopAction::Continue
    }

    fn on_client_disconnected(&self, num_clients: usize) -> LoopAction {
        if num_clients > 0 {
            return LoopAction::Continue;
        }
        self.0.write_report();
        LoopAction::Exit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gst-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test folder");
        dir
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn socket_goes_in_the_runtime_folder_when_there_is_one() {
        let base = fresh("runtime");
        let (runtime, data) = (base.join("x"), base.join("data"));
        std::fs::create_dir_all(&runtime).expect("runtime");
        let socket = socket_path(42, Some(runtime.clone().into()), &data, &base).expect("path");
        assert_eq!(socket, runtime.join("glass").join("crash-42.sock"));
        assert_eq!(mode(&runtime.join("glass")), 0o700);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn socket_falls_back_to_a_private_folder_in_the_data_folder() {
        let base = fresh("data");
        let data = base.join("data");
        // A relative runtime folder is ignored, as the XDG spec asks.
        let socket = socket_path(7, Some("relative".into()), &data, &base).expect("path");
        assert_eq!(socket, data.join("run").join("crash-7.sock"));
        assert_eq!(mode(&data.join("run")), 0o700);

        // A folder left with looser permissions is tightened.
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(data.join("run"), std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        socket_path(7, None, &data, &base).expect("path");
        assert_eq!(mode(&data.join("run")), 0o700);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn socket_paths_stay_under_the_length_limit() {
        let base = fresh("long");
        let data = base.join("d".repeat(120));
        let temp = std::env::temp_dir();
        let result = socket_path(u32::MAX, None, &data, &temp);
        if cfg!(target_os = "macos") {
            // The per-user temporary folder takes over.
            let socket = result.expect("temporary folder fallback");
            assert_eq!(
                socket,
                temp.join("glass").join(format!("crash-{}.sock", u32::MAX))
            );
            assert!(socket.as_os_str().len() <= MAX_SOCKET_PATH);
        } else {
            // Linux never falls back to the shared /tmp.
            let error = result.expect_err("no private folder short enough");
            assert!(error.contains("socket path limit"), "{error}");
        }
        assert!(
            !data.exists(),
            "a folder too deep for the socket is not created"
        );

        // The platform default stays well within the limit for a typical home folder.
        let home = Path::new("/Users/someone-with-a-long-name/Library/Application Support/Glass");
        let socket = home.join("run").join(format!("crash-{}.sock", u32::MAX));
        assert!(
            socket.as_os_str().len() <= MAX_SOCKET_PATH,
            "{}",
            socket.display()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn timestamps_are_utc_civil_dates() {
        assert_eq!(utc_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        assert_eq!(utc_timestamp(t), "2026-09-21T14:13:20Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(utc_timestamp(leap), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn report_has_the_facts_and_the_log() {
        let context = Context {
            version: "0.1.0".into(),
            os: "macos 26.0 (aarch64)".into(),
            gpu: None,
        };
        let text = report_text("panic", &context, "detail", &["a".into(), "b".into()]);
        for part in [
            "version: 0.1.0",
            "os: macos",
            "gpu: unknown",
            "detail",
            "last 2 log lines:\na\nb",
        ] {
            assert!(text.contains(part), "{part} missing from:\n{text}");
        }
    }
}
