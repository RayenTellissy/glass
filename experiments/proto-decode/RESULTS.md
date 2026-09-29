# proto-decode results

The prototype writes its numbers to `RESULTS.generated.md` on each run; this file holds the notes and the history.

4K60 H.264 (High, GOP 120, 1200 frames, about 31.6 Mbit/s, `scripts/make-media.sh`) decoded with FFmpeg's VideoToolbox hwaccel. Each frame's NV12 planes are wrapped zero-copy as wgpu textures and converted to `Rgba8Unorm` by a WGSL BT.709 limited-range pass. Release build, headless. Up to 3 conversions are in flight per stream, each with its own output texture; the stream never waits on the GPU unless the slot it is about to reuse is still busy.

Wrapping paths (both zero-copy, no CPU copy of pixel data):

- *CVMetalTextureCache per frame*: `CVMetalTextureCacheCreateTextureFromImage` per plane -> `MTLTexture` -> `texture_from_raw` / `create_texture_from_hal`, new wgpu textures and bind group every frame.
- *IOSurface textures cached*: `newTextureWithDescriptor:iosurface:plane:` per plane of each IOSurface VideoToolbox hands out, wrapped the same way, with the bind group cached by IOSurface ID (VideoToolbox recycles a small pool).

- FFmpeg: `ffmpeg-next` 9 against a shared FFmpeg 9.0.x build (see each run below)
- GPU: Apple M2 (Metal)
- Logical CPU cores: 8
- Decoder output pixel format: `420v`
- Decoder threads: 1
- Paced runs model a 3-frame look-ahead queue: display starts 3 slots after decoding, the decoder starts frame m no earlier than slot m, and frame m is late if it is ready after slot m + 3.

Budget: 60 fps sustained with under 5% CPU per stream, judged as whole-machine utilisation, (process CPU + VTDecoderXPCService CPU) / 8 logical cores, on the best paced single-stream run (see the Decision Log in PLAN.md).

## Rerun after review (2026-09-29): true 3-frame pacing window

The first run's pacing let the decoder start frame m at slot m - 3 but only counted it late after slot m + 3, a 6-slot window. The pacing now gives each frame exactly 3 slots (see above), and an early error return waits for the GPU before releasing in-flight pixel buffers. Built against the pinned FFmpeg 9.0.2 (`.glass-cache/ffmpeg/aarch64-apple-darwin`, libavutil 61.1.102).

Verdict: **PASS**. Judged run: CVMetalTextureCache per frame, paced 60 fps x1 at 2.06% of the machine (8.58% + 7.88% of one core), 60.0 fps, 0 late. Paced x2: 1.81% of the machine per stream (7.43% + 7.01% of one core), 0 late.

| Run | frames | fps per stream (slowest) | late frames | decode ms median / p99 / max | wrap + submit ms median / p99 / max | decode + wrap + submit ms median / p99 / max | waits for a busy slot | process CPU % of one core, per stream | VTDecoderXPCService CPU % of one core, per stream | whole machine %, per stream | IOSurfaces seen |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| decode only (no GPU work), paced 60 fps x1 | 1200 | 60.0 | 0 | 9.24 / 11.17 / 33.83 | 0.00 / 0.00 / 0.01 | 9.24 / 11.17 / 33.83 | 0 | 4.95 | 7.64 | 1.57 | - |
| CVMetalTextureCache per frame, unpaced x1 | 1200 | 110.3 | 0 | 9.11 / 11.70 / 33.21 | 0.31 / 0.60 / 1.82 | 9.43 / 12.00 / 33.46 | 0 | 13.74 | 12.65 | 3.30 | - |
| CVMetalTextureCache per frame, paced 60 fps x1 | 1200 | 60.0 | 0 | 9.33 / 11.54 / 41.44 | 0.34 / 0.65 / 3.05 | 9.70 / 11.99 / 41.60 | 0 | 8.58 | 7.88 | 2.06 | - |
| IOSurface textures cached, unpaced x1 | 1200 | 108.7 | 0 | 9.19 / 11.26 / 35.80 | 0.23 / 0.36 / 0.75 | 9.41 / 11.61 / 35.95 | 0 | 13.23 | 12.84 | 3.26 | 7 |
| IOSurface textures cached, paced 60 fps x1 | 1200 | 60.0 | 0 | 9.60 / 11.85 / 35.14 | 0.26 / 0.53 / 0.90 | 9.87 / 12.21 / 35.29 | 0 | 8.91 | 8.74 | 2.21 | 7 |
| IOSurface textures cached, unpaced x2 | 2400 | 127.4 | 0 | 7.63 / 10.90 / 61.37 | 0.20 / 0.37 / 1.26 | 7.83 / 11.15 / 61.50 | 0 | 13.03 | 12.01 | 3.13 | 7 |
| IOSurface textures cached, paced 60 fps x2 | 2400 | 60.0 | 0 | 9.21 / 14.48 / 42.73 | 0.22 / 0.34 / 0.99 | 9.44 / 14.72 / 42.90 | 0 | 7.43 | 7.01 | 1.81 | 7 |

A first rerun a few minutes earlier, while macOS storage services were using about 1.5 cores in the background, gave the same picture (paced x1: 1.99% CVMetalTextureCache, 1.97% IOSurface, 0 late) except paced x2, which had 10 late frames out of 2400 around one 36 ms stall in wrap + submit (1.63% of the machine per stream). With the tighter window a single stall of that size makes frames late; the engine's deeper look-ahead and asynchronous decode (next steps below) are the answer, not a different default.

## Original run (2026-09-29, before review)

Homebrew FFmpeg 9.0.1 (libavutil 61.1.101), with the 6-slot pacing window. Judged run: CVMetalTextureCache per frame, paced 60 fps x1 at 2.18% of the machine (9.20% + 8.26% of one core), 60.1 fps, 0 late.

| Run | frames | fps per stream (slowest) | late frames | decode ms median / p99 / max | wrap + submit ms median / p99 / max | decode + wrap + submit ms median / p99 / max | waits for a busy slot | process CPU % of one core, per stream | VTDecoderXPCService CPU % of one core, per stream | whole machine %, per stream | IOSurfaces seen |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| decode only (no GPU work), paced 60 fps x1 | 1200 | 60.1 | 0 | 9.33 / 12.85 / 41.47 | 0.00 / 0.00 / 0.02 | 9.33 / 12.85 / 41.47 | 0 | 5.10 | 7.76 | 1.61 | - |
| CVMetalTextureCache per frame, unpaced x1 | 1200 | 105.6 | 0 | 9.30 / 12.18 / 33.24 | 0.31 / 0.58 / 1.16 | 9.61 / 12.50 / 33.51 | 0 | 13.92 | 12.90 | 3.35 | - |
| CVMetalTextureCache per frame, paced 60 fps x1 | 1200 | 60.1 | 0 | 9.44 / 12.36 / 36.03 | 0.35 / 0.59 / 0.67 | 9.82 / 12.72 / 36.18 | 0 | 9.20 | 8.26 | 2.18 | - |
| IOSurface textures cached, unpaced x1 | 1200 | 105.4 | 0 | 9.37 / 12.33 / 33.76 | 0.26 / 0.53 / 6.08 | 9.62 / 12.63 / 33.93 | 0 | 14.13 | 13.66 | 3.47 | 7 |
| IOSurface textures cached, paced 60 fps x1 | 1200 | 60.1 | 0 | 9.59 / 12.44 / 46.06 | 0.27 / 0.46 / 1.12 | 9.86 / 12.73 / 46.21 | 0 | 8.95 | 8.86 | 2.23 | 7 |
| IOSurface textures cached, unpaced x2 | 2400 | 126.6 | 0 | 7.66 / 11.04 / 56.81 | 0.20 / 0.34 / 1.45 | 7.87 / 11.24 / 57.03 | 0 | 13.42 | 12.36 | 3.22 | 7 |
| IOSurface textures cached, paced 60 fps x2 | 2400 | 60.1 | 0 | 9.22 / 14.59 / 42.09 | 0.22 / 0.41 / 4.62 | 9.46 / 14.92 / 42.20 | 0 | 7.84 | 7.25 | 1.89 | 7 |

## Correctness

Frame 300 converted on the GPU, read back, saved to `media/` and compared with the same frame decoded by the FFmpeg CLI (swscale, BT.709 limited to full range, rgb24).

- CVMetalTextureCache per frame (`media/frame-300-cvcache.png`): mean absolute difference 0.001 per channel, max 1
- IOSurface textures cached (`media/frame-300-iosurface.png`): mean absolute difference 0.001 per channel, max 1

| path | pixel | GPU RGB | FFmpeg CLI RGB |
| --- | --- | --- | --- |
| CVMetalTextureCache per frame | (100, 100) | [250, 2, 2] | [250, 2, 2] |
| CVMetalTextureCache per frame | (1920, 1080) | [9, 10, 133] | [9, 10, 133] |
| CVMetalTextureCache per frame | (3000, 1800) | [251, 9, 251] | [251, 9, 251] |
| CVMetalTextureCache per frame | (500, 2000) | [249, 1, 1] | [249, 1, 1] |
| IOSurface textures cached | (100, 100) | [250, 2, 2] | [250, 2, 2] |
| IOSurface textures cached | (1920, 1080) | [9, 10, 133] | [9, 10, 133] |
| IOSurface textures cached | (3000, 1800) | [251, 9, 251] | [251, 9, 251] |
| IOSurface textures cached | (500, 2000) | [249, 1, 1] | [249, 1, 1] |

## Before and after removing the per-frame GPU wait

The first version waited for the GPU (`device.poll(Wait)`) after every frame. The current version keeps up to 3 conversions in flight, polls without blocking, and recycles an output slot only after its submission has completed (`queue.on_submitted_work_done`). IOSurface path, % of one core per stream:

| Run | per-frame wait | 3 in flight |
| --- | --- | --- |
| paced 60 fps x1 | 8.29 / 7.81 | 8.95 / 8.86 |
| paced 60 fps x2 | 7.29 / 6.51 | 7.84 / 7.25 |
| unpaced x1 fps | 90.1 | 105.4 |
| unpaced x2 fps per stream | 115.5 | 126.6 |

Removing the wait raises unpaced throughput but does not measurably lower CPU at 60 fps (run-to-run noise is about ±0.7 points), so the cost is in per-frame work (decode round trip, texture wrap, encode and submit), not in blocking.

## Notes

- Path taken: the published `ffmpeg-next` 9 / `ffmpeg-sys-next` 9 crates build against the Homebrew FFmpeg 9.0.1 shared libraries (`FFMPEG_DIR` set in the environment at build time), so no FFmpeg 7.1 build was needed. PLAN.md now pins FFmpeg 9.0.x.
- The numbers come from the zero-copy paths; there is no CPU-copy path in this prototype.
- Decoding alone (no GPU work) costs about 5% of one core in this process at 60 fps: demuxing, FFmpeg's H.264 slice parsing for the hwaccel, and the synchronous VideoToolbox round trip per frame. Wrapping and converting adds about 3.5 to 4 points.
- Per-core versus whole machine: the budget is judged as whole-machine utilisation (Decision Log, 2026-09-29), counting this process plus VTDecoderXPCService, divided by the logical core count. The per-core figures are kept in the table so the result can be re-judged on the reference M1.
- The VTDecoderXPCService figure is the change in its CPU time sampled with `ps` before and after each run; any other process decoding at the same time would also be counted.
- Two streams decode faster per stream than one because their waits on the decoder overlap.

## Next steps for the engine

- Drive a `VTDecompressionSession` directly with asynchronous output instead of FFmpeg's synchronous hwaccel, keeping FFmpeg for demuxing and software fallback.
- Keep several decoded frames in flight end to end (decode, wrap, convert), sized by the look-ahead queue.
- Fold the NV12 to RGB conversion into the compositor's frame instead of a separate submission per decoded frame, and keep the IOSurface texture cache.

Run: `FFMPEG_DIR=<ffmpeg prefix> cargo run --release` after `scripts/make-media.sh` (the verification step also needs the `ffmpeg` CLI on `PATH`, or `FFMPEG=/path/to/ffmpeg`). Flag: `--threads N` sets the decoder thread count (default 1).
