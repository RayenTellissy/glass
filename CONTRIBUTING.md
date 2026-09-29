# Contributing to Glass

`PLAN.md` is the source of truth for what gets built and in which order. `docs/ARCHITECTURE.md` describes how the crates fit together, and `docs/DECISIONS.md` records why.

## Building

The development machine is modest, so the compiler is capped at four parallel jobs. `.cargo/config.toml` sets `[build] jobs = 4` for every build in this repo; also export `CARGO_BUILD_JOBS=4` in your shell so tools that ignore the config file follow the same limit.

The easiest way to set up a shell is to source the development environment script from the repo root:

```sh
source scripts/dev-env.sh
```

It exports `CARGO_BUILD_JOBS=4` and, when `FFMPEG_DIR` is not already set, points it at the FFmpeg the bindings build against: the pinned build in `.glass-cache/ffmpeg/<host-triple>/` when `scripts/ffmpeg/build.sh` has produced one (see [FFmpeg](#ffmpeg)), otherwise Homebrew's FFmpeg (`brew install ffmpeg`). No machine-specific path is committed to the repo.

### Logging

Binaries log with `tracing` to stderr. `GLASS_LOG` sets the filter in `EnvFilter` syntax, per crate or module: `GLASS_LOG=debug`, `GLASS_LOG=engine=trace,info`, `GLASS_LOG=warn`. Unset, it is `info` with wgpu, naga and winit at `warn`. Code on the audio callback must not log.

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

UI code is checked headless: `ui::headless::HeadlessUi` renders egui into an offscreen wgpu texture (a locked screen or a CI runner has no visible window) and records each frame into a `ui::frame_time::FrameTimeMonitor`. A UI test runs its screen for a few hundred frames and calls `monitor.stats().assert_p99_within_budget()`, which fails with p50, p95, p99, max and the over-budget count when the p99 is not under 8 ms; `crates/ui/tests/frame_budget.rs` is the example to copy. Keep each such test under a few seconds. Without a GPU the test prints `SKIPPED ...` and returns; set `GLASS_REQUIRE_GPU=1` where a GPU is expected so a missing one fails instead.

## User-facing strings

Every string the UI shows is a key in `assets/i18n/en.json`, used through `t!("key")` or `t!("key", name = value)`. Add the key in the same change as the code that shows it, and write it in the product's voice (see [`assets/i18n/README.md`](assets/i18n/README.md)). The source check runs in CI and locally:

```sh
cargo test -p app --test ui_source -- --nocapture
```

It fails on a missing key, a wrong placeholder, or literal text passed to an egui text API, and prints keys nothing uses yet as warnings. A debug-only string can carry `// allow-literal-text: <reason>`.

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
