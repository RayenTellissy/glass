# Experiments

Phase 0 prototypes. They are not part of the workspace: each is a standalone crate with its own `Cargo.lock` and `[workspace]` table, kept as working reference code for the real crates. Full numbers are in each `RESULTS.md`; the summary and the stack they confirmed are in [`docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md).

All measurements were taken on an Apple M2, which stands in for the Apple M1 reference machine.

| Prototype | What it proved | Run |
| --- | --- | --- |
| [`proto-ui`](proto-ui/RESULTS.md) | egui 0.36 on wgpu 30 (Metal) draws a 200-clip, 10-track timeline with zoom, scroll and playhead changing every frame. UI-thread CPU (UI build + tessellation + paint + `Queue::submit`) median 0.37 ms, p99 0.74 ms offscreen; the original windowed run measured eframe's `cpu_usage` (which leaves out submit) at median 1.37 ms, p99 3.00 ms. Budget: median under 4 ms, p99 under 8 ms. | `cargo run --release -- --display-hz 60` (flags: `--offscreen`, `--no-cull`, `--frames N`) |
| [`proto-gpu`](proto-gpu/RESULTS.md) | A 4K WGSL dissolve plus a glyphon/cosmic-text title and timecode costs 0.77 ms GPU time at p99 on Metal, measured with wgpu timestamp queries. Budget: under 2 ms. The GTX 1660 class run still needs that hardware. | `cargo run --release` (flags: `--no-noise`, `--format rgba16f`, `--frames N`) |
| [`proto-decode`](proto-decode/RESULTS.md) | `ffmpeg-next` 9 with the VideoToolbox hwaccel decodes 4K60 H.264 and hands each frame's IOSurface to wgpu with no CPU copy, converting NV12 to RGB in WGSL. 60 fps, 0 late frames with a 3-frame look-ahead, 2.06% of the machine per stream. Budget: 60 fps, under 5% CPU per stream. All `unsafe` is in `src/ffi.rs`. macOS only; the Windows D3D11VA port still needs a Windows PC. | `scripts/make-media.sh`, then `FFMPEG_DIR=<ffmpeg prefix> cargo run --release` |

Run each command from inside the prototype's folder, one at a time, with `CARGO_BUILD_JOBS=4`. `proto-ui` and `proto-decode` write their numbers to `RESULTS.generated*.md` when they finish, leaving the hand-written `RESULTS.md` alone; `proto-gpu` prints its report to stdout. `proto-decode` needs a shared FFmpeg 9.0.x (for example Homebrew's, with `FFMPEG_DIR=/opt/homebrew/opt/ffmpeg`) and the `ffmpeg` CLI on `PATH` for the media script and the correctness check; its generated clip and frames go in `proto-decode/media/`, which git ignores.

The `target/` folders are regenerable; delete them after a run to save disk.
