# proto-decode results

4K60 H.264 (High, GOP 120, 1200 frames, about 31.6 Mbit/s, `scripts/make-media.sh`) decoded with FFmpeg's VideoToolbox hwaccel. Each frame's NV12 planes are wrapped zero-copy as wgpu textures and converted to `Rgba8Unorm` by a WGSL BT.709 limited-range pass. Release build, headless, one frame in flight per stream (the GPU conversion is waited on before the next frame).

Wrapping paths (both zero-copy, no CPU copy of pixel data):

- *CVMetalTextureCache per frame*: `CVMetalTextureCacheCreateTextureFromImage` per plane -> `MTLTexture` -> `texture_from_raw` / `create_texture_from_hal`, new wgpu textures and bind group every frame.
- *IOSurface textures cached*: `newTextureWithDescriptor:iosurface:plane:` per plane of each IOSurface VideoToolbox hands out, wrapped the same way, with the bind group cached by IOSurface ID (VideoToolbox recycles a small pool).

- FFmpeg: libavutil 61.1.101 via `ffmpeg-next` 9 against the Homebrew shared FFmpeg 9.0.1 build
- GPU: Apple M2 (Metal)
- Decoder output pixel format: `420v`
- Decoder threads: 1
- Paced runs let the decoder run up to 3 frames ahead of display (a player's look-ahead queue); a frame is late if it is ready after its display slot.

Verdict: **FAIL** (budget: 60 fps sustained with under 5% CPU per stream, judged on the best paced single-stream run using this process's CPU: 8.29% CPU, 60.1 fps, 0 late)

| Run | frames | fps per stream (slowest) | late frames | decode ms median / p99 / max | wrap + convert + GPU wait ms median / p99 / max | decode + upload ms median / p99 / max | process CPU % of one core, per stream | VTDecoderXPCService CPU %, per stream | IOSurfaces seen |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| decode only (no GPU work), paced 60 fps x1 | 1200 | 60.1 | 0 | 9.22 / 11.86 / 34.13 | 0.00 / 0.00 / 0.03 | 9.22 / 11.86 / 34.13 | 4.80 | 7.46 | - |
| CVMetalTextureCache per frame, unpaced x1 | 1200 | 92.7 | 0 | 9.17 / 11.80 / 34.45 | 1.80 / 2.49 / 14.13 | 10.98 / 14.18 / 37.65 | 12.50 | 11.26 | - |
| CVMetalTextureCache per frame, paced 60 fps x1 | 1200 | 60.1 | 0 | 9.66 / 12.56 / 36.55 | 1.90 / 2.73 / 12.41 | 11.60 / 14.79 / 39.29 | 9.61 | 8.56 | - |
| IOSurface textures cached, unpaced x1 | 1200 | 90.1 | 0 | 9.34 / 11.67 / 35.94 | 1.78 / 2.15 / 3.48 | 11.13 / 13.70 / 38.50 | 12.95 | 12.07 | 5 |
| IOSurface textures cached, paced 60 fps x1 | 1200 | 60.1 | 0 | 9.40 / 12.07 / 35.26 | 1.78 / 2.21 / 5.40 | 11.20 / 13.97 / 38.06 | 8.29 | 7.81 | 5 |
| IOSurface textures cached, unpaced x2 | 2400 | 115.5 | 0 | 7.20 / 9.38 / 75.39 | 1.36 / 1.72 / 3.85 | 8.59 / 10.89 / 77.43 | 12.17 | 12.20 | 5 |
| IOSurface textures cached, paced 60 fps x2 | 2400 | 60.1 | 0 | 8.60 / 13.82 / 38.23 | 1.53 / 1.89 / 11.06 | 10.17 / 15.59 / 40.10 | 7.29 | 6.51 | 5 |

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

## Notes

- Path taken: the published `ffmpeg-next` 9 / `ffmpeg-sys-next` 9 crates build against the Homebrew FFmpeg 9.0.1 shared libraries (`FFMPEG_DIR=/opt/homebrew/opt/ffmpeg` in the environment at build time), so no FFmpeg 7.1 build was needed. The Phase 0 defaults table still says FFmpeg 7.x.
- The numbers above come from the zero-copy path; there is no CPU-copy path in this prototype.
- Why it fails: decoding alone (no GPU work at all) already costs about 5% of one core in this process at 60 fps (demuxing, FFmpeg's H.264 slice parsing for the hwaccel, and the synchronous VideoToolbox round trip per frame). The wgpu wrap + conversion + per-frame submit and wait adds about 3.5%. Caching the IOSurface textures and bind groups instead of calling the CoreVideo texture cache every frame saves only a little, so the cost is mostly per-frame submission and wake-ups, not texture wrapping.
- Budget interpretation: the percentages are of one CPU core. This M2 has 8 cores, so the best paced single stream (about 8.3% of one core) is about 1% of the whole machine. The VTDecoderXPCService figure is CPU time spent by the system decoder service on our behalf, sampled with `ps` before and after each run (any other process decoding at the same time would also be counted).
- Two streams decode faster per stream than one (115 vs 90 fps unpaced), because a single stream waits on each frame in turn while two streams overlap their waits.
- Likely ways to get under 5% of one core, not tried here: drive `VTDecompressionSession` directly with asynchronous output instead of FFmpeg's synchronous hwaccel, keep several frames in flight instead of waiting for the GPU after every frame, and batch the NV12 conversion into the compositor's own frame instead of a separate submission.

Run: `FFMPEG_DIR=/opt/homebrew/opt/ffmpeg cargo run --release` after `scripts/make-media.sh` (the verification step also needs the `ffmpeg` CLI on `PATH`, or `FFMPEG=/path/to/ffmpeg`). Flag: `--threads N` sets the decoder thread count (default 1).
