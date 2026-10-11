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
//!
//! Shaders outside that folder (transitions, from `assets/transitions/`) are added at
//! run time with [`ShaderLibrary::add_external`]: each is compiled after `common.wgsl`
//! and a prelude from the folder, must define the fragment entry points it was added
//! with, and in debug builds its file is watched too. An edited prelude reports every
//! external shader built on it as changed.

use std::borrow::Cow;
use std::collections::HashMap;
#[cfg(debug_assertions)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Every effect shader by name, embedded at build time.
pub const EMBEDDED: &[(&str, &str)] = &[
    ("common", include_str!("../shaders/common.wgsl")),
    ("composite", include_str!("../shaders/composite.wgsl")),
    ("gamut", include_str!("../shaders/gamut.wgsl")),
    ("lut", include_str!("../shaders/lut.wgsl")),
    ("mask", include_str!("../shaders/mask.wgsl")),
    ("output", include_str!("../shaders/output.wgsl")),
    ("shape", include_str!("../shaders/shape.wgsl")),
    ("slate", include_str!("../shaders/slate.wgsl")),
    ("text", include_str!("../shaders/text.wgsl")),
    ("text_glyph", include_str!("../shaders/text_glyph.wgsl")),
    ("transition", include_str!("../shaders/transition.wgsl")),
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
    parse(name, source).map(|_| ())
}

/// [`validate`], and checks the shader defines every fragment entry point in `entries`
/// (a transition's `fs` and the entry points of its extra passes).
pub fn validate_external(name: &str, source: &str, entries: &[&str]) -> Result<(), ShaderError> {
    let module = parse(name, source)?;
    match entries.iter().find(|entry| {
        !module
            .entry_points
            .iter()
            .any(|e| e.name == **entry && e.stage == naga::ShaderStage::Fragment)
    }) {
        None => Ok(()),
        Some(missing) => Err(ShaderError::Invalid {
            name: name.to_owned(),
            message: format!("there is no `@fragment fn {missing}`"),
        }),
    }
}

fn parse(name: &str, source: &str) -> Result<naga::Module, ShaderError> {
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
    Ok(module)
}

/// `common.wgsl`, a prelude and an external shader's body.
pub fn compose_external(common: &str, prelude: &str, shader: &str) -> String {
    format!("{common}\n{prelude}\n{shader}")
}

/// A shader file outside the shaders folder.
#[derive(Clone, Debug)]
struct External {
    /// The embedded shader compiled between `common.wgsl` and this one.
    prelude: &'static str,
    /// The file, canonical, when it is on disk (watched in debug builds).
    path: Option<PathBuf>,
    /// The fragment entry points every source must define (an edit without one is kept
    /// out like any invalid edit).
    entries: Vec<&'static str>,
}

type Sources = Arc<Mutex<HashMap<String, String>>>;
type Externals = Arc<Mutex<HashMap<String, External>>>;

/// The current source of every effect shader, owned by the [`crate::renderer::Renderer`].
pub struct ShaderLibrary {
    sources: Sources,
    externals: Externals,
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
    /// Either way with the shaders of [`crate::transition::TransitionRegistry::standard`].
    fn default() -> Self {
        #[cfg(debug_assertions)]
        let library = Self::watching(&source_dir());
        #[cfg(not(debug_assertions))]
        let library = Self::embedded_only();
        crate::transition::TransitionRegistry::standard().add_shaders(&library);
        library
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
            externals: Arc::default(),
            #[cfg(debug_assertions)]
            watch: None,
        }
    }

    /// Adds (or replaces) the shader `name`, compiled as `common.wgsl`, the embedded
    /// shader `prelude`, then `source`, which must define the fragment entry points
    /// `entries`. With a `path`, a watching library rereads that file when it changes.
    /// Rejects a source that does not validate, keeping what was there.
    pub fn add_external(
        &self,
        name: &str,
        prelude: &'static str,
        source: String,
        path: Option<PathBuf>,
        entries: &[&'static str],
    ) -> Result<(), ShaderError> {
        {
            let mut sources = lock(&self.sources);
            let part = |n: &str| sources.get(n).map_or("", String::as_str);
            validate_external(
                name,
                &compose_external(part("common"), part(prelude), &source),
                entries,
            )?;
            sources.insert(name.to_owned(), source);
        }
        let path = path.map(|p| p.canonicalize().unwrap_or(p));
        lock(&self.externals).insert(
            name.to_owned(),
            External {
                prelude,
                path: path.clone(),
                entries: entries.to_vec(),
            },
        );
        #[cfg(debug_assertions)]
        if let (Some(watch), Some(path)) = (&self.watch, path) {
            watch.add(name, path);
        }
        Ok(())
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

    /// `common.wgsl` followed by `name` (and an external shader's prelude before it).
    pub fn composed(&self, name: &str) -> Option<String> {
        let sources = lock(&self.sources);
        let part = |n: &str| sources.get(n).map_or("", String::as_str);
        let body = sources.get(name)?;
        Some(match lock(&self.externals).get(name) {
            Some(external) => compose_external(part("common"), part(external.prelude), body),
            None => compose(part("common"), body).into_owned(),
        })
    }

    /// Shaders whose new source validated and was stored since the last call, `common`
    /// included (every pipeline depends on it); an edited prelude brings the external
    /// shaders built on it. Never blocks; call once per frame.
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
            match watch::read(&library.sources, &library.externals, dir, name) {
                Ok(source) => {
                    lock(&library.sources).insert((*name).to_owned(), source);
                }
                Err(e) => tracing::warn!("{e}; using the built-in {name}.wgsl"),
            }
        }
        match watch::Watch::start(dir, library.sources.clone(), library.externals.clone()) {
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
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use super::{
        EMBEDDED, Externals, ShaderError, Sources, compose, compose_external, embedded, lock,
        validate, validate_external,
    };

    /// How long the watch thread waits for more events after the first, so an editor's
    /// truncate-then-write or write-then-rename is read once, complete.
    const SETTLE: Duration = Duration::from_millis(50);

    enum Message {
        Changed(PathBuf),
        /// Watch an external shader's file (its folder) and reread it once.
        Add(String, PathBuf),
        Stop,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Starting,
        Watching,
        Failed,
    }

    type Shared<T> = Arc<(Mutex<T>, Condvar)>;

    /// What the watch thread reads and publishes into.
    struct Shaders {
        dir: PathBuf,
        sources: Sources,
        externals: Externals,
        publish: Sender<String>,
        error: Arc<Mutex<Option<String>>>,
    }

    /// The `shader-watch` thread and what it publishes.
    pub(super) struct Watch {
        messages: Sender<Message>,
        changed: Mutex<Receiver<String>>,
        error: Arc<Mutex<Option<String>>>,
        state: Shared<State>,
    }

    impl Watch {
        pub(super) fn start(
            dir: &Path,
            sources: Sources,
            externals: Externals,
        ) -> Result<Self, String> {
            let (messages, inbox) = mpsc::channel();
            let (publish, changed) = mpsc::channel();
            let error = Arc::new(Mutex::new(None));
            let state: Shared<State> = Arc::new((Mutex::new(State::Starting), Condvar::new()));
            let events = messages.clone();
            let thread_state = state.clone();
            let shaders = Shaders {
                dir: dir.to_owned(),
                sources,
                externals,
                publish,
                error: error.clone(),
            };
            std::thread::Builder::new()
                .name("shader-watch".into())
                .spawn(move || {
                    let watcher = register(&shaders.dir, events);
                    let (lock_state, ready) = &*thread_state;
                    *lock(lock_state) = if watcher.is_ok() {
                        State::Watching
                    } else {
                        State::Failed
                    };
                    ready.notify_all();
                    let mut watcher = match watcher {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::warn!("shader hot reload is off: {e}");
                            return;
                        }
                    };
                    tracing::info!("watching {} for shader edits", shaders.dir.display());
                    let all = EMBEDDED.iter().map(|(n, _)| (*n).to_owned()).collect();
                    shaders.reload(all);
                    run(&shaders, &mut watcher, &inbox);
                })
                .map_err(|e| e.to_string())?;
            Ok(Self {
                messages,
                changed: Mutex::new(changed),
                error,
                state,
            })
        }

        pub(super) fn add(&self, name: &str, path: PathBuf) {
            let _ = self.messages.send(Message::Add(name.to_owned(), path));
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

    /// `path` with its folder resolved (the file itself may be gone mid-save).
    fn canonical(path: &Path) -> Option<PathBuf> {
        let parent = path.parent()?.canonicalize().ok()?;
        Some(parent.join(path.file_name()?))
    }

    fn run(shaders: &Shaders, watcher: &mut notify::RecommendedWatcher, inbox: &Receiver<Message>) {
        use notify::Watcher;
        let mut folders: HashSet<PathBuf> = HashSet::new();
        let Ok(mut message) = inbox.recv() else {
            return;
        };
        loop {
            let mut pending = Vec::new();
            loop {
                match message {
                    Message::Changed(path) => pending.extend(shaders.name_of(&path)),
                    Message::Add(name, path) => {
                        if let Some(folder) = path.parent()
                            && folders.insert(folder.to_owned())
                            && let Err(e) =
                                watcher.watch(folder, notify::RecursiveMode::NonRecursive)
                        {
                            tracing::warn!("not watching {}: {e}", folder.display());
                        }
                        pending.push(name);
                    }
                    Message::Stop => return,
                }
                match inbox.recv_timeout(SETTLE) {
                    Ok(next) => message = next,
                    Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => break,
                }
            }
            shaders.reload(pending);
            match inbox.recv() {
                Ok(next) => message = next,
                Err(_) => return,
            }
        }
    }

    impl Shaders {
        /// The shader a changed file holds: an embedded shader's file in the shaders
        /// folder, or an external shader's file.
        fn name_of(&self, path: &Path) -> Option<String> {
            if path.extension()? != "wgsl" {
                return None;
            }
            let canonical = canonical(path);
            if let Some((name, _)) = lock(&self.externals)
                .iter()
                .find(|(_, e)| e.path.is_some() && e.path == canonical)
            {
                return Some(name.clone());
            }
            if canonical.as_deref().and_then(Path::parent)
                != self.dir.canonicalize().ok().as_deref()
            {
                return None;
            }
            let name = path.file_stem()?.to_str()?;
            embedded(name).map(|_| name.to_owned())
        }

        /// Rereads `names`, storing and publishing each one that validates and differs
        /// from its current source, with the external shaders built on a changed prelude.
        fn reload(&self, mut names: Vec<String>) {
            names.sort();
            names.dedup();
            for name in names {
                match self.read(&name) {
                    Ok(source) => {
                        *lock(&self.error) = None;
                        let mut current = lock(&self.sources);
                        if current.get(&name) != Some(&source) {
                            tracing::info!("reloaded {name}.wgsl");
                            current.insert(name.clone(), source);
                            // Published under the lock: whoever sees the new source also
                            // finds its name waiting in `poll_changes`.
                            for dependent in self.dependents(&name) {
                                let _ = self.publish.send(dependent);
                            }
                            let _ = self.publish.send(name);
                        }
                    }
                    Err(e) => {
                        tracing::error!("keeping the last good {name}.wgsl: {e}");
                        *lock(&self.error) = Some(e.to_string());
                    }
                }
            }
        }

        /// The external shaders compiled with `prelude`.
        fn dependents(&self, prelude: &str) -> Vec<String> {
            lock(&self.externals)
                .iter()
                .filter(|(_, e)| e.prelude == prelude)
                .map(|(n, _)| n.clone())
                .collect()
        }

        fn read(&self, name: &str) -> Result<String, ShaderError> {
            read(&self.sources, &self.externals, &self.dir, name)
        }
    }

    /// Reads `name` from its file (in `dir`, or an external shader's own) and validates
    /// it composed with the current sources (or, for `common` or a prelude, with every
    /// shader that includes it).
    pub(super) fn read(
        sources: &Sources,
        externals: &Externals,
        dir: &Path,
        name: &str,
    ) -> Result<String, ShaderError> {
        let external = lock(externals).get(name).cloned();
        let path = match &external {
            Some(e) => e.path.clone().ok_or_else(|| ShaderError::Read {
                name: name.to_owned(),
                error: std::io::Error::from(std::io::ErrorKind::NotFound),
            })?,
            None => dir.join(format!("{name}.wgsl")),
        };
        let source = std::fs::read_to_string(path).map_err(|error| ShaderError::Read {
            name: name.to_owned(),
            error,
        })?;
        let mut current = lock(sources).clone();
        let externals = lock(externals).clone();
        current.insert(name.to_owned(), source.clone());
        let part = |n: &str| current.get(n).map_or("", String::as_str);
        let check = |other: &str| match externals.get(other) {
            Some(e) => validate_external(
                other,
                &compose_external(part("common"), part(e.prelude), part(other)),
                &e.entries,
            ),
            None => validate(other, &compose(part("common"), part(other))),
        };
        if name == "common" {
            for other in EMBEDDED
                .iter()
                .map(|(n, _)| *n)
                .filter(|n| *n != "common")
                .chain(externals.keys().map(String::as_str))
            {
                check(other)?;
            }
        } else {
            check(name)?;
            for other in externals.iter().filter(|(_, e)| e.prelude == name) {
                check(other.0)?;
            }
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
