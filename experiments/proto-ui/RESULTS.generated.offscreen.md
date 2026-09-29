# proto-ui generated results

Written by the prototype on each run; hand-written notes live in RESULTS.md.

Timeline stress prototype: 200 clips on 10 tracks, egui/eframe 0.36, release build.
Zoom, scroll and playhead change every frame, continuous repaint. Target: offscreen texture (`--offscreen`), no surface acquire or present.

- Renderer: wgpu Metal on Apple M2 (offscreen)
- Window: 1440 x 748 points at 2 pixels per point
- Display refresh rate: unknown Hz (the 120 Hz budget is 8.33 ms per frame, so the CPU numbers are what matter)
- Off-screen culling: on
- Warm-up frames: 100, measured frames: 3000
- Shapes per frame (median / max): 479 / 978

Verdict on UI-thread CPU: **PASS (median < 4 ms and p99 < 8 ms)**

| Metric (ms) | frames | median | p95 | p99 | max | mean |
| --- | --- | --- | --- | --- | --- | --- |
| UI-thread CPU (judged) | 3000 | 0.367 | 0.699 | 0.743 | 0.865 | 0.427 |
| Before submit | 3000 | 0.360 | 0.687 | 0.731 | 0.850 | 0.420 |
| Queue::submit | 3000 | 0.006 | 0.013 | 0.017 | 0.049 | 0.007 |
| Surface acquire | 0 | - | - | - | - | - |
| Present | 0 | - | - | - | - | - |
| UI build only | 3000 | 0.079 | 0.117 | 0.128 | 0.150 | 0.084 |
| Frame interval | 3000 | 0.533 | 0.722 | 0.760 | 0.882 | 0.558 |

- *UI-thread CPU (judged)*: UI build + tessellation + paint encoding + `Queue::submit`, per frame.
- *eframe cpu_usage*: eframe's reported frame time, which leaves out acquire, `Queue::submit` and present.
- *Queue::submit*, *Surface acquire*, *Present*: time inside egui-wgpu's scopes of those names, timed through `profiling`'s tracing backend.
- *UI build only*: time spent inside the app's `ui` callback.
- *Frame interval*: wall-clock time between consecutive frames.
