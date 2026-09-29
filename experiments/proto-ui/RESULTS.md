# proto-ui results

Timeline stress prototype: 200 clips on 10 tracks, egui/eframe 0.36, release build.
Zoom, scroll and playhead change every frame, continuous repaint, vsync off (`PresentMode::AutoNoVsync`).

- Renderer: wgpu Metal on Apple M2
- Window: 1440 x 748 points at 2 pixels per point
- Display refresh rate: 60 Hz (the 120 Hz budget is 8.33 ms per frame, so the CPU numbers are what matter)
- Off-screen culling: on
- Warm-up frames: 100, measured frames: 3001
- Shapes per frame (median / max): 479 / 978

Verdict: **PASS (median < 4 ms and p99 < 8 ms)**

| Metric (ms) | frames | median | p95 | p99 | max | mean |
| --- | --- | --- | --- | --- | --- | --- |
| Full frame CPU | 3001 | 1.372 | 2.552 | 2.998 | 3.518 | 1.398 |
| UI build only | 3002 | 0.217 | 0.383 | 0.467 | 2.191 | 0.215 |
| Frame interval | 3001 | 3.581 | 6.474 | 13.089 | 21.669 | 4.090 |
| Acquire/present wait | 3001 | 2.397 | 5.170 | 11.349 | 20.297 | 2.692 |

- *Full frame CPU*: eframe's per-frame CPU time (UI build + tessellation + paint submission), excluding surface acquire/present waits.
- *UI build only*: time spent inside the app's `ui` callback.
- *Frame interval*: wall-clock time between consecutive frames.
- *Acquire/present wait*: frame interval minus full frame CPU, a cheap proxy for the GPU/present side.

## Comparison run without culling (`--no-cull`)

Same scene with every clip, thumbnail and waveform segment submitted even when off screen (shapes per frame median / max: 4835 / 7372).

| Metric (ms) | frames | median | p95 | p99 | max | mean |
| --- | --- | --- | --- | --- | --- | --- |
| Full frame CPU | 3001 | 1.091 | 2.661 | 3.129 | 6.289 | 1.406 |
| UI build only | 3002 | 0.247 | 0.708 | 0.795 | 1.074 | 0.329 |
| Frame interval | 3001 | 3.674 | 6.497 | 13.738 | 15.223 | 4.155 |
| Acquire/present wait | 3001 | 2.426 | 5.461 | 11.172 | 13.964 | 2.749 |

Both runs pass without optimisation. The frame interval p99 (about 13 ms) is time spent waiting on surface acquire/present at 2880 x 1496 pixels, not CPU work; revisit it in the frame-time monitor once the compositor draws real frames.

Run: `cargo run --release -- --display-hz 60` (flags: `--no-cull`, `--frames N`). The run stops by itself after the measured frames or 90 s.
