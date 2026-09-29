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
