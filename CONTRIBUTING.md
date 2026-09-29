# Contributing to Glass

`PLAN.md` is the source of truth for what gets built and in which order. `docs/ARCHITECTURE.md` describes how the crates fit together, and `docs/DECISIONS.md` records why.

## Working on a task

Glass is built one `PLAN.md` task at a time, in phase order, and every task follows the same loop. This section adapts PLAN.md's "How to run this plan as a goal" for people; the plan's version is the one to follow when the two differ.

### The session loop

1. **Read the plan.** Open `PLAN.md` and read the last 20 lines of the Progress Log at the bottom, the current phase's "Done when" line and the Decision Log.
2. **Check the machine.** Run `df -h /` (see [Disk space](#disk-space)), then `source scripts/dev-env.sh` in the shell you build from.
3. **Check the tree.** Run the [full check](#the-full-check). If anything fails on a clean checkout, fixing it is your first task.
4. **Pick a task.** Take the first unchecked `[ ]` task in phase order. Do not start a later phase while an earlier one has open tasks; only tasks tagged `(parallel)` may be done out of order, at most two at a time.
5. **Start it.** Mark it `[~]` in `PLAN.md` and commit `plan: start <short task name>`.
6. **Build it.** Meet the task's own "Done when", or its phase's. Add or update a test or a benchmark that proves it. If the change alters how data moves between crates or threads, update `docs/ARCHITECTURE.md` in the same commit. Commit the work as `<area>: <what changed>`, where the area is a crate or folder (`ui`, `media`, `docs`, `scripts`); several commits are fine.
7. **Check it.** Run the full check, the smoke binary, and the extra checks the change calls for (see [Before you mark a task done](#before-you-mark-a-task-done)).
8. **Finish it.** Mark it `[x]`, append one Progress Log line, `- YYYY-MM-DD — Phase N — <short task name> — <short hash of the main implementation commit> — <key numbers>`, and commit `plan: done <short task name>`. Never leave a task at `[~]` with a broken build.

That is the three-commit rhythm: `plan: start`, the implementation commit(s), `plan: done`. The done commit comes last so its log line can cite the implementation commit's hash.

Work happens on `goal/phase-<N>` branches, never on `main`; a branch is merged when its phase's "Done when" passes.

### The full check

```sh
source scripts/dev-env.sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run --bin smoke
```

`cargo test --workspace` includes the source rules for UI code (i18n keys, no literal text, no literal colours) and the headless frame-budget tests. CI's check job runs the same commands, plus `python3 assets/theme/check.py` and the source rules as their own step.

### Before you mark a task done

Every task passes the full check. Depending on what it touched, also run:

| If the change touches | Run | See |
| --- | --- | --- |
| A dependency | `cargo deny check` | [Licences and advisories](#licences-and-advisories) |
| UI text | `cargo test -p app --test ui_source -- --nocapture` (fails on missing keys, warns on unused ones) | [User-facing strings](#user-facing-strings) |
| Theme tokens or colours | `python3 assets/theme/check.py` and the same `ui_source` test | [Colours and theme](#colours-and-theme) |
| A UI screen | its headless frame-budget test (p99 under 8 ms) | [UI frame budget](#ui-frame-budget) |
| Performance-sensitive code | `cargo bench -p <crate> --bench "*"` against a saved baseline, alone on the machine | [Benchmarks](#benchmarks) |
| Media decoding or tests that read media | `tests/fixtures/generate.sh --verify` | [Test media](#test-media) |
| `scripts/ffmpeg/` or `versions.env` | `scripts/ffmpeg/build.sh`, then the full check against the new build | [FFmpeg](#ffmpeg) |
| Packaging, bundled files or anything that grows the app | `scripts/package/make-dmg.sh` and `scripts/size-check.sh` | [Packaging](#packaging) |

The quality gates that apply to every task, from PLAN.md:

- builds and tests pass on macOS and Windows in CI;
- no UI-thread stall over 8 ms in the frame-time monitor while the feature is used;
- the size check still passes, or the budget was raised with a Decision Log entry;
- every new user-facing string is in `assets/i18n/en.json`;
- UI matches its board in the Paper file and uses only its tokens, with exact values read from the board, not from screenshots (a moment with no board gets one first, following `DESIGN_HANDOFF.md`);
- if data flow changed, `docs/ARCHITECTURE.md` changed in the same commit.

### Decisions, splits and blocked tasks

- **Defaults.** Phase 0 records the defaults. Use them. Change one only when a benchmark in this repo proves it fails a budget, and record the change in the Decision Log in `PLAN.md` (`- YYYY-MM-DD — <decision>. Reason: <reason>.`) and in `docs/DECISIONS.md`. The same goes for dropping a task (`[-]`) or raising a budget.
- **Splitting.** A task is at most a few hours of focused work. If it turns out larger, split it in place into indented sub-tasks under the original line, which stays as a heading with its checkbox until every sub-task is `[x]` or `[-]`. Splitting is normal.
- **Blocked.** A task that needs a paid account, a signing certificate, a licence purchase, hardware you do not have, or a person's decision about deleting user data gets `(needs-human: <one-line reason>)` on its line and a Progress Log line, committed as `plan: needs-human <short task name>`. Do what can be done locally first, then move on to the next task. When only needs-human sub-tasks are left, the parent line says `(needs-human: blocked only on the sub-task(s) below)`.
- **Never:** commit to `main`, add telemetry, download models or large assets into the repo (use on-demand download paths and ignored folders), hardcode UI strings or colours, or block the UI thread.

### Machine limits

The reference development machine is an Apple M2 with 8 GB of RAM and a small disk, so these limits hold for everyone sharing it, people and scripts alike.

#### Concurrency

- **One `cargo build` or `cargo test` at a time** on the machine. Anything else that compiles waits.
- The compiler is capped at four jobs: `.cargo/config.toml` sets `[build] jobs = 4`, and `scripts/dev-env.sh` exports `CARGO_BUILD_JOBS=4` for tools that ignore the config.
- At most 10 concurrent workers (terminals running tasks, automated sessions), coordinator included; 3 to 5 is better. Only `(parallel)` tasks run beside the current phase task, at most two of them.
- **Benchmarks and the stress project run alone:** serially, with no build, test or other heavy app running. Timing taken on a busy machine is not a result.
- If the machine swaps, or a build takes more than three times its usual time, drop to a single worker for the rest of the session.

#### Disk space

- Check `df -h /` at the start of every session.
- Build only for this Mac's architecture (arm64). Intel, universal, Windows and Linux builds, and fuzzing, run in CI only.
- Build release only to run benchmarks, the stress project or a local package.
- Download only the smallest speech and segmentation models locally, into the user data folder, never into the repo.
- **Below 15 GB free:** run `cargo sweep --time 7` (`cargo install cargo-sweep`) to drop build output older than a week; then delete the regenerable caches under the project cache folder (rendered frames, proxies, thumbnails, waveforms); if still below 15 GB, run `cargo clean`.
- **Below 10 GB after that cleanup:** stop, tag the current task `(needs-human)` and say why in the Progress Log.
- Never delete source, fixture scripts, autosave data, or anything outside the repo and its cache folder.

## Building

The development machine is modest, so the compiler is capped at four parallel jobs. `.cargo/config.toml` sets `[build] jobs = 4` for every build in this repo; also export `CARGO_BUILD_JOBS=4` in your shell so tools that ignore the config file follow the same limit.

The easiest way to set up a shell is to source the development environment script from the repo root:

```sh
source scripts/dev-env.sh
```

It exports `CARGO_BUILD_JOBS=4` and, when `FFMPEG_DIR` is not already set, points it at the FFmpeg the bindings build against: the pinned build in `.glass-cache/ffmpeg/<host-triple>/` when `scripts/ffmpeg/build.sh` has produced one (see [FFmpeg](#ffmpeg)), otherwise Homebrew's FFmpeg (`brew install ffmpeg`). No machine-specific path is committed to the repo.

### Logging

Binaries log with `tracing` to stderr. `GLASS_LOG` sets the filter in `EnvFilter` syntax, per crate or module: `GLASS_LOG=debug`, `GLASS_LOG=engine=trace,info`, `GLASS_LOG=warn`. Unset, it is `info` with wgpu, naga and winit at `warn`. Code on the audio callback must not log.

### Crash reports

Crash reports are off unless turned on. Until the Settings dialog exists, turn them on with `{"crash_reports": true}` in `settings.json` in the user config folder, or with `GLASS_CRASH_REPORTS=1` (`0` forces them off). Reports go to `crashes/` in the user data folder: a minidump (`.dmp`) and a text report (`.txt`) for a native crash, a text report only for a panic. Nothing is ever uploaded. To try it:

```sh
GLASS_CRASH_REPORTS=1 GLASS_DATA_DIR=/tmp/glass-data GLASS_DEBUG_CRASH=segfault cargo run --bin glass
ls /tmp/glass-data/crashes
```

`GLASS_DEBUG_CRASH` also takes `abort` and `panic`, and works in debug builds only (release builds ignore it); `GLASS_CONFIG_DIR` and `GLASS_DATA_DIR` move the two folders. Read a minidump with `minidump-stackwalk` (`cargo install minidump-stackwalk`) and the matching build's debug symbols.

### Feature flags

`codec-av1`, `codec-prores-sw`, `ai-whisper` and `ai-segmentation` are off by default so local builds stay fast. Release builds turn all of them on with the `app` feature `release-full`, because Cargo features cannot follow the build profile:

```sh
cargo build --release --bin glass --features release-full
```

To check that every flag still resolves: `cargo test -p app --features release-full`.

## Test media

Reference media is generated, never committed. With the FFmpeg command-line tools installed (`brew install ffmpeg`), run from the repo root:

```sh
tests/fixtures/generate.sh --verify
```

It writes H.264 (8-bit 1080p, 10-bit 720p, 4K), 10-bit HEVC, ProRes 422, VP9, AV1, a VFR clip, PNG and JPEG stills, a PNG image sequence and a 48 kHz stereo WAV into `tests/fixtures/generated/` (git-ignored, about 25 MB), plus `MANIFEST.json` describing each file's codec, size, frame rate, duration, pixel format and whether it is VFR. Existing files are kept unless you pass `--force`; `--verify` checks every file with `ffprobe` against the manifest. Tests that need the media skip when the manifest is missing.

## Benchmarks

Micro-benchmarks use `criterion` and live in each crate's `benches/` folder (a `[[bench]]` target with `harness = false`, plus `[lib] bench = false` so the library's test harness does not receive criterion's flags). Benchmarks are timing measurements, so run them serially and alone: no other build, test or benchmark on the machine at the same time, and no other agent or heavy app running. They build with the release profile.

```sh
source scripts/dev-env.sh
cargo bench --workspace --bench "*"                      # every benchmark, full settings
cargo bench -p engine --bench "*" -- --quick             # short local run, a few seconds
cargo bench --workspace --bench "*" -- --save-baseline before
# ... change something ...
cargo bench --workspace --bench "*" -- --baseline before # compare with the saved run
scripts/bench-report.py --baseline before                # the same comparison as Markdown
```

`--bench "*"` selects only the benchmark targets; a plain `cargo bench --workspace` would also pass criterion's flags to the test harnesses of the libraries and binaries, which reject them. Each benchmark group sets short warm-up and measurement times so the full suite stays quick; `--quick` shortens it further for a sanity check. Results go to `target/criterion/`.

In CI the `bench` job in `ci.yml` saves a `main` baseline on every push to `main` and, on pull requests, compares against it with `--baseline-lenient main` and posts the `scripts/bench-report.py` table as a PR comment (updated in place on later pushes). Shared runners are noisy, so the report treats changes within 10% as noise; budgets that matter (UI frame time, scrub latency) are enforced by tests, not by this comparison.

## UI frame budget

UI code is checked headless: `ui::headless::HeadlessUi` renders egui into an offscreen wgpu texture (a locked screen or a CI runner has no visible window) and records each frame into a `ui::frame_time::FrameTimeMonitor`. A UI test runs its screen for a few hundred frames and calls `headless.assert_p99_within_budget(what, &monitor.stats())`, which fails with p50, p95, p99, max and the over-budget count when the p99 is not under 8 ms; `crates/ui/tests/frame_budget.rs` is the example to copy. Keep each such test under a few seconds. Without a GPU the test prints `SKIPPED ...` and returns. On a software rasteriser (lavapipe, WARP; wgpu's `DeviceType::Cpu`) the frames still render but the budget is not judged, and the test prints `BUDGET NOT CHECKED ...`. Set `GLASS_REQUIRE_GPU=1` where a GPU is expected so a missing GPU or a software one fails instead; CI sets it on the macOS arm64 job and installs lavapipe on Linux.

## User-facing strings

Every string the UI shows is a key in `assets/i18n/en.json`, used through `t!("key")` or `t!("key", name = value)`. Add the key in the same change as the code that shows it, and write it in the product's voice (see [`assets/i18n/README.md`](assets/i18n/README.md)). The source check runs in CI and locally:

```sh
cargo test -p app --test ui_source -- --nocapture
```

It fails on a missing key, a wrong placeholder, or literal text passed to an egui text API (`label`, `button`, `checkbox`, `ComboBox::new`, `on_hover_text` and the rest listed in `crates/app/tests/support/source_scan.rs`), including text chosen inside the call by an `if` or `match`, and prints keys nothing uses yet as warnings. A debug-only string can carry `// allow-literal-text: <reason>`.

Known limit: the check reads each call's arguments only. Text built into a variable first (`let label = "Play"; ui.button(label)`), returned from a helper function, or passed to a text API the list does not know gets through, so review those by hand and keep text in `t!()` at the point it is created.

## Colours and theme

UI code never writes a colour. Widgets get theirs from the egui style, which `ui::theme` builds from the tokens in `assets/theme/`; custom painting asks for a role: `ui::theme::current(ctx).color(Role::ClipVideo)`. The same `ui_source` test fails on `Color32::from_rgb` and the other constructors (`from_gray`, `from_black_alpha`, `from_white_alpha`, `from_additive_luminance`, ...), `Color32::RED`-style constants, egui's own palettes (`Visuals::dark()`, `Visuals::light()`, `Widgets::dark()`, ...), and hex colour strings outside `crates/ui/src/theme.rs`; a debug-only colour can carry `// allow-literal-colour: <reason>`. After editing a token file, run its validator and contrast report:

```sh
python3 assets/theme/check.py
```

Fonts: see [`assets/fonts/README.md`](assets/fonts/README.md). Until the Geist files are added there, builds and tests use a system font and log which one.

## Licences and advisories

Glass is distributed linked against a GPL build of FFmpeg, so every dependency must use a licence compatible with GPLv3. `deny.toml` holds the allow-list (with the reasoning for borderline entries), checks the RustSec advisory database, warns on duplicate crate versions and only accepts crates from crates.io. Run it before adding a dependency:

```sh
cargo install cargo-deny --locked
cargo deny check
```

CI runs the same check. If a new dependency is rejected, pick another crate; extend the allow-list only for a licence that is GPLv3-compatible, with a comment saying why.

## FFmpeg

Glass links FFmpeg as shared libraries. For day-to-day work Homebrew's FFmpeg is enough; the pinned build that releases ship comes from `scripts/ffmpeg/build.sh`:

```sh
scripts/ffmpeg/build.sh          # about 3 minutes on an M2, once per version bump
source scripts/dev-env.sh        # now prefers .glass-cache/ffmpeg/<host-triple>
```

`scripts/ffmpeg/versions.env` pins FFmpeg 9.0.x, x264 and x265 (and, per target, nv-codec-headers and zlib) by URL and SHA-256. The script downloads the archives into `.glass-cache/ffmpeg/src/`, verifies them, builds its own `pkgconf` (and `nasm` on x86_64 when missing) as host tools, and installs into `.glass-cache/ffmpeg/<target-triple>/` with `lib/`, `include/`, `bin/`, `LICENSES/`, `SOURCES.txt` and `BUILDINFO`. It rebuilds only when `versions.env` or the script changes (`--force` to override). It builds for the host only; CI builds each target on its own runner.

### GPL compliance

That FFmpeg build is configured with `--enable-gpl --enable-version3` and links x264 and x265, so every Glass release is distributed under GPLv3 terms for those libraries. Each release must:

- bundle the licence texts from `.glass-cache/ffmpeg/<target>/LICENSES/` inside every installer,
- attach the exact source archives used, produced by `scripts/ffmpeg/package-sources.sh --archive` (archives, `SHA256SUMS`, licence texts and the build scripts, in `.glass-cache/ffmpeg/release-sources.tar.gz`),
- keep `versions.env` unchanged between building the installers and packaging the sources.

Never patch the FFmpeg, x264 or x265 sources without adding the patches to the source bundle.

## Packaging

Releases are cut by pushing a `v*` tag. `.github/workflows/release.yml` is generated by `cargo-dist` from `dist-workspace.toml`; do not edit it by hand. After changing `dist-workspace.toml`, `.github/build-setup.yml` or the `[package.metadata.dist]` / `[package.metadata.wix]` tables, run `dist generate` (install with `cargo install cargo-dist --locked`) and commit the result. `crates/app/wix/main.wxs` is edited by hand (`allow-dirty = ["msi"]`).

The formats dist does not make come from `scripts/package/`, run by `.github/workflows/package.yml`. The macOS one can be tried locally:

```sh
source scripts/dev-env.sh
cargo build --profile dist --features release-full -p app
scripts/package/make-dmg.sh --binary target/dist/glass
```

It writes `target/package/<target>/Glass.app` and `glass-<version>-<target>.dmg`, signed ad hoc unless `GLASS_CODESIGN_IDENTITY` names a Developer ID identity, and fails if any bundled binary still references a library outside the bundle or the system. Icons are taken from `assets/icons/` when present.

### Size budget

Every installer has a budget in bytes in `size-budget.toml` (30 MiB each for the DMG, MSI, MSIX and AppImage to start). `package.yml` runs the check after building each installer, and it can be run on any local build:

```sh
scripts/size-check.sh target/package/aarch64-apple-darwin/*.dmg
```

It prints a table of size, budget and headroom per installer and exits 1 when any is over. Raising a budget is allowed only with a Decision Log line in `PLAN.md` and the same entry in `docs/DECISIONS.md`, in the same commit as the new number.
