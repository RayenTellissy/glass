# proto-gpu results

Two 3840x2160 procedural source textures (`Rgba8UnormSrgb`), one WGSL dissolve pass (single fullscreen triangle, uniform mix amount changing every frame) into a 3840x2160 offscreen target, then a glyphon/cosmic-text overlay pass (two-line 180 px title plus a timecode re-shaped every frame). Release build, headless.

- Adapter: Apple M2 (Metal, IntegratedGpu)
- Output format: `Rgba8UnormSrgb`
- Per-pixel noise in the sources: on (defeats the GPU's lossless framebuffer compression, closer to real footage)
- Timestamp queries: supported (period 1 ns; inside encoders: false, inside passes: false)
- Warm-up frames: 100, measured frames: 2000

Verdict: **PASS (under 2 ms GPU time, judged on p99)**

| GPU time (ms) | frames | median | p95 | p99 | max | mean |
| --- | --- | --- | --- | --- | --- | --- |
| Dissolve pass | 2000 | 0.689 | 0.718 | 0.733 | 0.792 | 0.691 |
| Text pass | 2000 | 0.609 | 0.665 | 0.701 | 0.733 | 0.613 |
| Total | 2000 | 0.721 | 0.751 | 0.771 | 0.820 | 0.722 |
| Wall (submit + wait) | 2000 | 1.038 | 1.081 | 1.119 | 1.163 | 1.032 |

- *Dissolve pass* and *Text pass*: begin/end-of-pass timestamps.
- *Total*: dissolve begin to text end, including any gap between the passes.
- *Wall*: CPU wall time from encoding through `queue.submit` and `device.poll(Wait)`, for reference; it includes submission and wake-up latency.


## Variants

Total GPU time (ms) for the other configurations:

| Variant | median | p95 | p99 | max |
| --- | --- | --- | --- | --- |
| Rgba8UnormSrgb output, noise on (default) | 0.721 | 0.751 | 0.771 | 0.820 |
| Rgba8UnormSrgb output, noise off (`--no-noise`) | 0.343 | 0.353 | 0.359 | 0.417 |
| Rgba16Float output, noise on (`--format rgba16f`) | 0.879 | 0.905 | 0.917 | 0.985 |

## Notes

- Hardware: this machine is an Apple M2 (10-core GPU). It stands in for the Apple M1 reference, which is somewhat slower (8-core GPU on the base M1, lower memory bandwidth), so expect the M1 number to be higher. The margin to 2 ms is wide enough that it should still pass; confirm on an M1 when one is available.
- No GTX 1660 class GPU is available locally; that half of the task is tagged needs-human in PLAN.md.
- Apple GPUs are tile-based and overlap consecutive passes, so the per-pass begin/end timestamps overlap and *Total* is less than the sum of the two passes. *Total* is the number to judge on. The wall-time row is an upper bound that includes submission and wake-up latency, and it also passes.
- Without per-pixel noise the smooth procedural sources compress well under the GPU's lossless framebuffer compression and the frame costs half as much. The default run keeps the noise because real footage behaves more like it.
- The text pass loads and stores the whole 4K target, which is most of its cost; folding the overlay into the dissolve pass is the first optimisation if this ever gets tight.
- The Metal adapter supports `TIMESTAMP_QUERY` but not the inside-encoders or inside-passes variants, so timing uses pass-level timestamp writes.

Run: `cargo run --release` (flags: `--no-noise`, `--format rgba16f`, `--frames N`).
