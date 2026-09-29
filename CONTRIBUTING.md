# Contributing to Glass

`PLAN.md` is the source of truth for what gets built and in which order. `docs/ARCHITECTURE.md` describes how the crates fit together, and `docs/DECISIONS.md` records why.

## Building

The development machine is modest, so the compiler is capped at four parallel jobs. `.cargo/config.toml` sets `[build] jobs = 4` for every build in this repo; also export `CARGO_BUILD_JOBS=4` in your shell so tools that ignore the config file follow the same limit.

The easiest way to set up a shell is to source the development environment script from the repo root:

```sh
source scripts/dev-env.sh
```

It exports `CARGO_BUILD_JOBS=4` and, when `FFMPEG_DIR` is not already set and Homebrew is installed, points `FFMPEG_DIR` at Homebrew's FFmpeg (`brew install ffmpeg`), which the FFmpeg bindings build against. No machine-specific path is committed to the repo.

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

## Licences and advisories

Glass is distributed linked against a GPL build of FFmpeg, so every dependency must use a licence compatible with GPLv3. `deny.toml` holds the allow-list (with the reasoning for borderline entries), checks the RustSec advisory database, warns on duplicate crate versions and only accepts crates from crates.io. Run it before adding a dependency:

```sh
cargo install cargo-deny --locked
cargo deny check
```

CI runs the same check. If a new dependency is rejected, pick another crate; extend the allow-list only for a licence that is GPLv3-compatible, with a comment saying why.
