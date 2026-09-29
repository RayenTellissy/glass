# proto-ui generated results

Written by the prototype on each run; hand-written notes live in RESULTS.md.

Timeline stress prototype: 200 clips on 10 tracks, egui/eframe 0.36, release build.
Zoom, scroll and playhead change every frame, continuous repaint. Target: offscreen texture (`--offscreen`), no surface acquire or present.

- Renderer: wgpu Metal on Apple M2 (offscreen)
- Window: 1440 x 748 points at 2 pixels per point
- Display refresh rate: unknown Hz (the 120 Hz budget is 8.33 ms per frame, so the CPU numbers are what matter)
- Off-screen culling: off
- Warm-up frames: 100, measured frames: 3000
- Shapes per frame (median / max): 4833 / 7372

Verdict on UI-thread CPU: **PASS (median < 4 ms and p99 < 8 ms)**

| Metric (ms) | frames | median | p95 | p99 | max | mean |
| --- | --- | --- | --- | --- | --- | --- |
| UI-thread CPU (judged) | 3000 | 0.600 | 0.745 | 0.792 | 0.852 | 0.611 |
| Before submit | 3000 | 0.592 | 0.734 | 0.778 | 0.841 | 0.602 |
| Queue::submit | 3000 | 0.006 | 0.013 | 0.019 | 0.028 | 0.008 |
| Surface acquire | 0 | - | - | - | - | - |
| Present | 0 | - | - | - | - | - |
| UI build only | 3000 | 0.190 | 0.246 | 0.261 | 0.290 | 0.187 |
| Frame interval | 3000 | 0.665 | 0.808 | 0.875 | 0.959 | 0.674 |

- *UI-thread CPU (judged)*: UI build + tessellation + paint encoding + `Queue::submit`, per frame.
- *eframe cpu_usage*: eframe's reported frame time, which leaves out acquire, `Queue::submit` and present.
- *Queue::submit*, *Surface acquire*, *Present*: time inside egui-wgpu's scopes of those names, timed through `profiling`'s tracing backend.
- *UI build only*: time spent inside the app's `ui` callback.
- *Frame interval*: wall-clock time between consecutive frames.
