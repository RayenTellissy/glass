# proto-ui results

Timeline stress prototype: 200 clips on 10 tracks, egui/eframe 0.36 on wgpu 30 (Metal), release build, Apple M2 (stands in for the M1 reference). Zoom, scroll and playhead change every frame with continuous repaint.

Budget: UI-thread CPU per frame median under 4 ms, p99 under 8 ms, where UI-thread CPU is UI build + tessellation + paint encoding + `Queue::submit`.

The prototype writes its numbers to `RESULTS.generated[.offscreen][.no-cull].md` on each run; this file holds the notes.

## What eframe's `cpu_usage` leaves out

egui-wgpu 0.36.2 (`winit.rs`, `paint_and_update_textures`) adds the time spent in `get_current_texture`, `Queue::submit` and `present` to the `vsync_sec` it returns, and eframe 0.36.2 (`native/wgpu_integration.rs`) reports `frame time - vsync_sec` as `cpu_usage`. So `cpu_usage` excludes `Queue::submit`, which is CPU work on the UI thread. The prototype now times egui-wgpu's `Queue::submit`, `get_current_texture` and `present` scopes itself (through `profiling`'s tracing backend and a small subscriber in `src/spans.rs`) and judges `cpu_usage + submit`.

## Rerun after review (2026-09-29): offscreen

The windowed rerun could not be measured: the session's screen was locked, so the window was occluded and eframe skipped tessellation and paint entirely (the windowed mode now reports NO DATA in that case instead of a false pass). The rerun therefore uses `--offscreen`, which drives the same `egui::Context` and `egui_wgpu::Renderer` path into a 2880 x 1496 texture (same 1440 x 748 points at 2x as the window) and times UI build, tessellation, texture and buffer upload, render pass encoding and `Queue::submit` on the calling thread, with at most two frames in flight. It has no surface acquire or present and none of eframe's winit event handling.

| Run (ms, 3000 frames) | median | p95 | p99 | max |
| --- | --- | --- | --- | --- |
| UI-thread CPU, culling on | 0.367 | 0.699 | 0.743 | 0.865 |
| of which `Queue::submit` | 0.006 | 0.013 | 0.017 | 0.049 |
| UI-thread CPU, culling off (4833 / 7372 shapes) | 0.600 | 0.745 | 0.792 | 0.852 |
| of which `Queue::submit` | 0.006 | 0.013 | 0.019 | 0.028 |

Verdict: **PASS**. `Queue::submit` costs about 6 µs per frame on Metal because wgpu encodes the Metal commands while the render pass is recorded, before submit. Adding it back does not change the original verdict.

Still to do: rerun the windowed mode with the screen unlocked (`cargo run --release -- --display-hz 60`) to get `cpu_usage + submit` on the real eframe path; the offscreen numbers are lower than the original windowed `cpu_usage` (below), which also includes eframe's per-frame winit and platform-output work.

## Original run (2026-09-29, before review): window

Window 1440 x 748 points at 2x on a 60 Hz display, vsync off (`PresentMode::AutoNoVsync`), 3001 measured frames after 100 warm-up frames, 479 / 978 shapes per frame (median / max).

| Metric (ms) | median | p95 | p99 | max |
| --- | --- | --- | --- | --- |
| eframe `cpu_usage` (UI build + tessellation + paint encoding; excludes `Queue::submit`) | 1.372 | 2.552 | 2.998 | 3.518 |
| UI build only | 0.217 | 0.383 | 0.467 | 2.191 |
| Frame interval | 3.581 | 6.474 | 13.089 | 21.669 |
| Frame interval minus `cpu_usage` (acquire + submit + present) | 2.397 | 5.170 | 11.349 | 20.297 |

Without culling (4835 / 7372 shapes): `cpu_usage` median 1.091, p99 3.129 ms. The frame interval p99 (about 13 ms) is surface acquire/present at 2880 x 1496 pixels; revisit it in the frame-time monitor once the compositor draws real frames.

## Run

- Window: `cargo run --release -- --display-hz 60` (the screen must be unlocked and the window visible)
- Offscreen: `cargo run --release -- --offscreen`
- Flags: `--no-cull`, `--frames N`. A windowed run stops by itself after the measured frames or 90 s.
