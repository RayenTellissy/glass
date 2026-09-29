# proto-decode generated results

Written by the prototype on each run; hand-written notes live in RESULTS.md.

4K60 H.264 (High, GOP 120, 1200 frames, about 31.6 Mbit/s, `scripts/make-media.sh`) decoded with FFmpeg's VideoToolbox hwaccel. Each frame's NV12 planes are wrapped zero-copy as wgpu textures and converted to `Rgba8Unorm` by a WGSL BT.709 limited-range pass. Release build, headless. Up to 3 conversions are in flight per stream, each with its own output texture; the stream never waits on the GPU unless the slot it is about to reuse is still busy.

Wrapping paths (both zero-copy, no CPU copy of pixel data):

- *CVMetalTextureCache per frame*: `CVMetalTextureCacheCreateTextureFromImage` per plane -> `MTLTexture` -> `texture_from_raw` / `create_texture_from_hal`, new wgpu textures and bind group every frame.
- *IOSurface textures cached*: `newTextureWithDescriptor:iosurface:plane:` per plane of each IOSurface VideoToolbox hands out, wrapped the same way, with the bind group cached by IOSurface ID (VideoToolbox recycles a small pool).

- FFmpeg: libavutil 61.1.102 via `ffmpeg-next` 9 against the shared FFmpeg that `FFMPEG_DIR` pointed at when building
- GPU: Apple M2 (Metal)
- Logical CPU cores: 8
- Decoder output pixel format: `420v`
- Decoder threads: 1
- Paced runs model a 3-frame look-ahead queue: display starts 3 slots after decoding, the decoder starts frame m no earlier than slot m, and frame m is late if it is ready after slot m + 3.

Verdict: **PASS**. Budget: 60 fps sustained with under 5% CPU per stream, judged as whole-machine utilisation, (process CPU + VTDecoderXPCService CPU) / 8 logical cores, on the best paced single-stream run (see the Decision Log in PLAN.md). Judged run: CVMetalTextureCache per frame, paced 60 fps x1 at 2.06% of the machine (8.58% + 7.88% of one core), 60.0 fps, 0 late

| Run | frames | fps per stream (slowest) | late frames | decode ms median / p99 / max | wrap + submit ms median / p99 / max | decode + wrap + submit ms median / p99 / max | waits for a busy slot | process CPU % of one core, per stream | VTDecoderXPCService CPU % of one core, per stream | whole machine %, per stream | IOSurfaces seen |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| decode only (no GPU work), paced 60 fps x1 | 1200 | 60.0 | 0 | 9.24 / 11.17 / 33.83 | 0.00 / 0.00 / 0.01 | 9.24 / 11.17 / 33.83 | 0 | 4.95 | 7.64 | 1.57 | - |
| CVMetalTextureCache per frame, unpaced x1 | 1200 | 110.3 | 0 | 9.11 / 11.70 / 33.21 | 0.31 / 0.60 / 1.82 | 9.43 / 12.00 / 33.46 | 0 | 13.74 | 12.65 | 3.30 | - |
| CVMetalTextureCache per frame, paced 60 fps x1 | 1200 | 60.0 | 0 | 9.33 / 11.54 / 41.44 | 0.34 / 0.65 / 3.05 | 9.70 / 11.99 / 41.60 | 0 | 8.58 | 7.88 | 2.06 | - |
| IOSurface textures cached, unpaced x1 | 1200 | 108.7 | 0 | 9.19 / 11.26 / 35.80 | 0.23 / 0.36 / 0.75 | 9.41 / 11.61 / 35.95 | 0 | 13.23 | 12.84 | 3.26 | 7 |
| IOSurface textures cached, paced 60 fps x1 | 1200 | 60.0 | 0 | 9.60 / 11.85 / 35.14 | 0.26 / 0.53 / 0.90 | 9.87 / 12.21 / 35.29 | 0 | 8.91 | 8.74 | 2.21 | 7 |
| IOSurface textures cached, unpaced x2 | 2400 | 127.4 | 0 | 7.63 / 10.90 / 61.37 | 0.20 / 0.37 / 1.26 | 7.83 / 11.15 / 61.50 | 0 | 13.03 | 12.01 | 3.13 | 7 |
| IOSurface textures cached, paced 60 fps x2 | 2400 | 60.0 | 0 | 9.21 / 14.48 / 42.73 | 0.22 / 0.34 / 0.99 | 9.44 / 14.72 / 42.90 | 0 | 7.43 | 7.01 | 1.81 | 7 |

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
