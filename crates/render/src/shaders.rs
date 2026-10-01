//! The WGSL sources of the compositor's effect shaders, from `crates/render/shaders/`.
//! Each effect shader is compiled with `common.wgsl` prepended (the full-screen vertex
//! shader and shared helpers).
//!
//! Release builds use the sources embedded with `include_str!` and never watch anything.
//! Debug builds read them from disk and start a `shader-watch` thread on the folder: an
//! edited shader is validated with naga on that thread, and only a valid source is
//! published; [`ShaderLibrary::poll_changes`] then reports it and the
//! [`crate::renderer::Renderer`] rebuilds its pipelines on the next frame. A source that
//! fails to parse or validate is logged with naga's error, and the last good source and
//! pipelines stay in use.

use std::borrow::Cow;
use std::collections::HashMap;
#[cfg(debug_assertions)]
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Every effect shader by name, embedded at build time.
pub const EMBEDDED: &[(&str, &str)] = &[
    ("common", include_str!("../shaders/common.wgsl")),
    ("composite", include_str!("../shaders/composite.wgsl")),
    ("gamut", include_str!("../shaders/gamut.wgsl")),
    ("output", include_str!("../shaders/output.wgsl")),
    ("shape", include_str!("../shaders/shape.wgsl")),
    ("slate", include_str!("../shaders/slate.wgsl")),
    ("text", include_str!("../shaders/text.wgsl")),
    ("text_glyph", include_str!("../shaders/text_glyph.wgsl")),
];

/// The embedded source of one shader file, without `common.wgsl`.
pub fn embedded(name: &str) -> Option<&'static str> {
    EMBEDDED.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// `common.wgsl` followed by the named shader.
pub fn compose(common: &str, shader: &str) -> Cow<'static, str> {
    Cow::Owned(format!("{common}\n{shader}"))
}

/// The shaders folder in the source tree (what debug builds watch).
#[cfg(debug_assertions)]
pub fn source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders")
}

/// Why a shader source was rejected.
#[derive(Debug, thiserror::Error)]
pub enum ShaderError {
    #[error("{name}.wgsl could not be read: {error}")]
    Read { name: String, error: std::io::Error },
    #[error("{name}.wgsl does not compile:\n{message}")]
    Invalid { name: String, message: String },
}

/// Parses and validates a composed effect shader with naga, the compiler wgpu uses.
pub fn validate(name: &str, source: &str) -> Result<(), ShaderError> {
    let path = format!("{name}.wgsl");
    let invalid = |message: String| ShaderError::Invalid {
        name: name.to_owned(),
        message,
    };
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|e| invalid(e.emit_to_string_with_path(source, &path)))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|e| invalid(e.emit_to_string_with_path(source, &path)))?;
    Ok(())
}

type Sources = Arc<Mutex<HashMap<String, String>>>;

/// The current source of every effect shader, owned by the [`crate::renderer::Renderer`].
pub struct ShaderLibrary {
    sources: Sources,
    #[cfg(debug_assertions)]
    watch: Option<watch::Watch>,
}

impl std::fmt::Debug for ShaderLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShaderLibrary")
            .field("watching", &self.is_watching())
            .finish_non_exhaustive()
    }
}

impl Default for ShaderLibrary {
    /// Watches [`source_dir`] in debug builds; the embedded sources in release builds.
    fn default() -> Self {
        #[cfg(debug_assertions)]
        {
            Self::watching(&source_dir())
        }
        #[cfg(not(debug_assertions))]
        {
            Self::embedded_only()
        }
    }
}

impl ShaderLibrary {
    /// The sources compiled into the binary; never changes.
    pub fn embedded_only() -> Self {
        Self {
            sources: Arc::new(Mutex::new(
                EMBEDDED
                    .iter()
                    .map(|(n, s)| ((*n).to_owned(), (*s).to_owned()))
                    .collect(),
            )),
            #[cfg(debug_assertions)]
            watch: None,
        }
    }

    pub fn is_watching(&self) -> bool {
        #[cfg(debug_assertions)]
        {
            self.watch.is_some()
        }
        #[cfg(not(debug_assertions))]
        {
            false
        }
    }

    /// The raw source of `name` (without `common.wgsl`).
    pub fn raw(&self, name: &str) -> Option<String> {
        lock(&self.sources).get(name).cloned()
    }

    /// `common.wgsl` followed by `name`.
    pub fn composed(&self, name: &str) -> Option<String> {
        let sources = lock(&self.sources);
        let common = sources.get("common").map_or("", String::as_str);
        sources.get(name).map(|s| compose(common, s).into_owned())
    }

    /// Shaders whose new source validated and was stored since the last call, `common`
    /// included (every pipeline depends on it). Never blocks; call once per frame.
    pub fn poll_changes(&self) -> Vec<String> {
        #[cfg(debug_assertions)]
        if let Some(watch) = &self.watch {
            return watch.poll();
        }
        Vec::new()
    }

    /// Naga's message for the most recent rejected edit, until a later edit validates.
    pub fn last_error(&self) -> Option<String> {
        #[cfg(debug_assertions)]
        if let Some(watch) = &self.watch {
            return watch.last_error();
        }
        None
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(debug_assertions)]
impl ShaderLibrary {
    /// Starts from the files in `dir` (keeping the embedded source for a file that is
    /// missing or invalid) and watches the folder on a `shader-watch` thread. Never
    /// blocks on the watcher: registering with the OS happens on that thread (macOS
    /// FSEvents registration can take seconds on a busy machine), and the thread rereads
    /// every shader once registered, so an edit made meanwhile is not lost. When watching
    /// fails, the thread logs it and the library keeps its current sources.
    pub fn watching(dir: &Path) -> Self {
        let mut library = Self::embedded_only();
        for (name, _) in EMBEDDED {
            match watch::read(&library.sources, dir, name) {
                Ok(source) => {
                    lock(&library.sources).insert((*name).to_owned(), source);
                }
                Err(e) => tracing::warn!("{e}; using the built-in {name}.wgsl"),
            }
        }
        match watch::Watch::start(dir, library.sources.clone()) {
            Ok(w) => library.watch = Some(w),
            Err(e) => tracing::warn!("shader hot reload is off: {e}"),
        }
        library
    }

    /// Waits up to `timeout` for the watch thread to register with the OS. False when
    /// not watching, when registration failed, or on timeout.
    pub fn wait_until_watching(&self, timeout: std::time::Duration) -> bool {
        self.watch
            .as_ref()
            .is_some_and(|w| w.wait_until_watching(timeout))
    }
}

#[cfg(debug_assertions)]
mod watch {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use super::{EMBEDDED, ShaderError, Sources, compose, embedded, lock, validate};

    /// How long the watch thread waits for more events after the first, so an editor's
    /// truncate-then-write or write-then-rename is read once, complete.
    const SETTLE: Duration = Duration::from_millis(50);

    enum Message {
        Changed(PathBuf),
        Stop,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Starting,
        Watching,
        Failed,
    }

    type Shared<T> = Arc<(Mutex<T>, Condvar)>;

    /// The `shader-watch` thread and what it publishes.
    pub(super) struct Watch {
        messages: Sender<Message>,
        changed: Mutex<Receiver<String>>,
        error: Arc<Mutex<Option<String>>>,
        state: Shared<State>,
    }

    impl Watch {
        pub(super) fn start(dir: &Path, sources: Sources) -> Result<Self, String> {
            let (messages, inbox) = mpsc::channel();
            let (publish, changed) = mpsc::channel();
            let error = Arc::new(Mutex::new(None));
            let state: Shared<State> = Arc::new((Mutex::new(State::Starting), Condvar::new()));
            let events = messages.clone();
            let thread_error = error.clone();
            let thread_state = state.clone();
            let dir = dir.to_owned();
            std::thread::Builder::new()
                .name("shader-watch".into())
                .spawn(move || {
                    let watcher = register(&dir, events);
                    let (lock_state, ready) = &*thread_state;
                    *lock(lock_state) = if watcher.is_ok() {
                        State::Watching
                    } else {
                        State::Failed
                    };
                    ready.notify_all();
                    let _watcher = match watcher {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::warn!("shader hot reload is off: {e}");
                            return;
                        }
                    };
                    tracing::info!("watching {} for shader edits", dir.display());
                    let all = EMBEDDED.iter().map(|(n, _)| (*n).to_owned()).collect();
                    reload(&dir, &sources, all, &publish, &thread_error);
                    run(&dir, &sources, &inbox, &publish, &thread_error);
                })
                .map_err(|e| e.to_string())?;
            Ok(Self {
                messages,
                changed: Mutex::new(changed),
                error,
                state,
            })
        }

        pub(super) fn wait_until_watching(&self, timeout: Duration) -> bool {
            let (state, ready) = &*self.state;
            let guard = lock(state);
            let (guard, _) = ready
                .wait_timeout_while(guard, timeout, |s| *s == State::Starting)
                .unwrap_or_else(|e| e.into_inner());
            *guard == State::Watching
        }

        pub(super) fn poll(&self) -> Vec<String> {
            let mut names: Vec<String> = lock(&self.changed).try_iter().collect();
            names.sort();
            names.dedup();
            names
        }

        pub(super) fn last_error(&self) -> Option<String> {
            lock(&self.error).clone()
        }
    }

    impl Drop for Watch {
        /// Asks the thread to stop without joining it: it may still be registering.
        fn drop(&mut self) {
            let _ = self.messages.send(Message::Stop);
        }
    }

    fn register(dir: &Path, events: Sender<Message>) -> notify::Result<notify::RecommendedWatcher> {
        use notify::Watcher;
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if let Ok(event) = event
                    && !event.kind.is_access()
                {
                    for path in event.paths {
                        let _ = events.send(Message::Changed(path));
                    }
                }
            })?;
        watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
        Ok(watcher)
    }

    fn shader_name(path: &Path) -> Option<String> {
        if path.extension()? != "wgsl" {
            return None;
        }
        let name = path.file_stem()?.to_str()?;
        embedded(name).map(|_| name.to_owned())
    }

    fn run(
        dir: &Path,
        sources: &Sources,
        inbox: &Receiver<Message>,
        publish: &Sender<String>,
        error: &Mutex<Option<String>>,
    ) {
        while let Ok(Message::Changed(path)) = inbox.recv() {
            let mut pending: Vec<String> = shader_name(&path).into_iter().collect();
            loop {
                match inbox.recv_timeout(SETTLE) {
                    Ok(Message::Changed(path)) => pending.extend(shader_name(&path)),
                    Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => break,
                }
            }
            reload(dir, sources, pending, publish, error);
        }
    }

    /// Rereads `names`, storing and publishing each one that validates and differs from
    /// its current source.
    fn reload(
        dir: &Path,
        sources: &Sources,
        mut names: Vec<String>,
        publish: &Sender<String>,
        error: &Mutex<Option<String>>,
    ) {
        names.sort();
        names.dedup();
        for name in names {
            match read(sources, dir, &name) {
                Ok(source) => {
                    *lock(error) = None;
                    let mut current = lock(sources);
                    if current.get(&name) != Some(&source) {
                        tracing::info!("reloaded {name}.wgsl");
                        current.insert(name.clone(), source);
                        // Published under the lock: whoever sees the new source also
                        // finds its name waiting in `poll_changes`.
                        let _ = publish.send(name);
                    }
                }
                Err(e) => {
                    tracing::error!("keeping the last good {name}.wgsl: {e}");
                    *lock(error) = Some(e.to_string());
                }
            }
        }
    }

    /// Reads `name` from `dir` and validates it composed with the current common source
    /// (or, for `common` itself, with every shader that includes it).
    pub(super) fn read(sources: &Sources, dir: &Path, name: &str) -> Result<String, ShaderError> {
        let source =
            std::fs::read_to_string(dir.join(format!("{name}.wgsl"))).map_err(|error| {
                ShaderError::Read {
                    name: name.to_owned(),
                    error,
                }
            })?;
        let current = lock(sources).clone();
        if name == "common" {
            for (other, _) in EMBEDDED.iter().filter(|(n, _)| *n != "common") {
                let body = current.get(*other).map_or("", String::as_str);
                validate(other, &compose(&source, body))?;
            }
        } else {
            let common = current.get("common").map_or("", String::as_str);
            validate(name, &compose(common, &source))?;
        }
        Ok(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_shader_validates() {
        let common = embedded("common").expect("common");
        for (name, source) in EMBEDDED.iter().filter(|(n, _)| *n != "common") {
            validate(name, &compose(common, source)).unwrap_or_else(|e| panic!("{e}"));
        }
    }

    #[test]
    fn errors_name_the_file() {
        let err = validate("broken", "fn f() -> f32 { return 1u; }").expect_err("invalid");
        let text = err.to_string();
        assert!(text.contains("broken.wgsl"), "{text}");
    }

    #[test]
    fn embedded_library_never_changes() {
        let library = ShaderLibrary::embedded_only();
        assert!(!library.is_watching());
        assert!(library.poll_changes().is_empty());
        assert!(library.last_error().is_none());
        for (name, source) in EMBEDDED {
            assert_eq!(library.raw(name).as_deref(), Some(*source));
        }
    }

    #[test]
    fn default_watches_only_in_debug_builds() {
        assert_eq!(
            ShaderLibrary::default().is_watching(),
            cfg!(debug_assertions)
        );
    }
}
