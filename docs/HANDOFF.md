# Handoff: resuming the goal

State of the work on 2026-10-11, when the session paused with `goal/phase-2` at the pretty JSON merge (`ac098f7`) and several task branches still in flight, and how to pick it up in a fresh session. `PLAN.md` stays the single source of truth; this file points into it.

## 1. Where things are

| Branch | Contents |
| --- | --- |
| `main` | Phases 0 and 1 merged (local Done-when parts; CI and hardware parts are needs-human). |
| `goal/phase-2` | Phase 2 (no open tasks, only needs-human ones), Phase 4 (complete), most of Phase 5, and tasks pulled forward from Phases 3, 6, 8, 9, 16 and 17 (below). The integration branch: new task branches start here. Project schema v8, autosave `FORMAT_VERSION` 8. |

Phase 2's Done when asks that every fixture decodes to GPU textures and f32 audio through the same API, with hardware decode on both platforms, and that `smoke` plays the reference project. On this Mac every fixture the pinned FFmpeg can generate decodes to textures (`crates/engine/tests/fixture_textures.rs`, 31 picture fixtures), and `smoke` is real (Decision 66): it opens `tests/fixtures/reference.proj`, plays 2 s through `engine::timeline::TimelinePlayback`, exports 1 s through `media::export` (silent audio until the engine mixes audio) and verifies the file. Six fixtures cannot be generated here (PLAN line 144) and Windows hardware decode needs a PC (line 146). Merging `goal/phase-2` into `main` (Decision 12) waits for a review recheck: the last review asked for these docs fixes and for the motion transitions' GPU timing test, which still compared raw medians, to follow Decision 64 (being converted on `task/transition-passes`).

**Done on `goal/phase-2`.**

- Phase 2: safe FFmpeg wrapper, probe, software decode (fixtures needing extra encoders excepted, see section 2), VideoToolbox decode, software fallback, colour space plumbing, VFR normalisation, accurate seek (hardware decoders reopened on every keyframe seek, Decision 57), stills and sequences (macOS part), audio decode and resampling, analysis jobs, thumbnail strips, waveform peaks, proxies, scene cuts, silence, beats and tempo, loudness, import, media pool, relinking, offline slate, metadata panel; texture coverage of every generated fixture and recipe-hash regeneration of stale fixtures in `tests/fixtures/generate.sh`.
- Phase 3: thread layout, lock-free frame queues (the `frame_queue_alloc` flake is fixed, Decision 56), master clock (listening check needs-human), rendered-frame cache, look-ahead decode (`engine::lookahead`, Decision 49), adaptive quality (`engine::quality`, bilinear cap at reduced levels, Decision 52), scrub mode (`engine::scrub`, Decision 53), the timeline playback pipeline (`engine::timeline`: plan, renderer with the output node, `TimelinePlayback`, Decision 60; `engine::playback` is gone), dirty-range invalidation (Decision 67).
- Phase 4: complete. Render graph, linear-light working space and dithered output, blend modes, transform, opacity and crop, shader hot reload, effect parameters and their auto-UI, text, vector shapes within the 4K budget (sparse strips, Decision 46), the mask node with its visual regression cases (Decision 44), readback ring, visual regression suite.
- Phase 5: edit tree, undo and redo, autosave (in the project cache folder once the project has a file, Decision 55), versioned schema with migrations up to v8, sequences, tracks, clips, gaps and markers, clip properties (v4, Decision 45), transitions (v5, Decision 54), project settings (v6, Decision 55), keyframe curves (v7, Decision 63), nested sequences and compound clips (v8, Decision 69), the reference project (Decision 66), the canonical pretty JSON project file (Decision 74). Open: adjustment layers, copy and paste.
- Phase 6: track headers (Decision 68); remappable shortcuts, first two sub-tasks.
- Phase 8: encoder wrapper (AV1, VP9 and MP3 needs-human, Decision 47), hardware encode with VideoToolbox and fallback (NVENC, QSV, AMF needs-human, Decision 51), presets (Decision 58), subtitle export (Decision 65), output verification (Decision 59).
- Phase 9: LUT framework, transition framework (Decision 48), dissolves (Decision 50), wipes (Decision 61), motion transitions (Decision 62).
- Phase 16: UI snapshot tests; timing tests robust under load (Decision 64).
- Phase 17: portable mode, About dialog (with its layout cache).
- Tooling: `scripts/finish-check.sh`, `scripts/check-plan-dups.py`, the `cargo-serial` fingerprint fix (Decision 28), `scripts/dev-env.sh` finding `~/.cargo/bin` in non-login shells, compiler cap of 8 jobs (Decision 43).

**In flight on task branches** when this was written (each in its own worktree `../glass-<name>`; check `git worktree list`, they may have merged since):

| Branch | Task |
| --- | --- |
| `task/timeline-anim` | Phase 3 animated and nested rendering: keyframed values per frame, nested sequences composited as layers |
| `task/audio-mixer` | Phase 12 mixer: buses, sends, faders, meters, automation lanes |
| `task/export-queue` | Phase 8 background export queue |
| `task/transition-passes` | Phase 9 multi-pass transitions (whip pan and zoom blur at reduced resolution) |
| `task/timeline-view` | Phase 6 timeline view |
| `task/vt-bframes`, `task/vt-bframes-tests` | Phase 8 fix: VideoToolbox H.264 and HEVC with B-frames (every web preset) |
| `task/nested-tests` | contract tests for nested sequences |

Decision numbers 70 to 73, 75 and 76 are taken by those branches and appear in `docs/DECISIONS.md` when they merge.

**Cleaned up.** `drafts/phase-4-5-wip` is deleted (every task on it had merged), and the old worktrees `../glass-cargo-serial`, `../glass-fp-demo` and `../glass-vector-shapes` are gone. `../glass-integration` stays: a detached checkout for integration checks.

### Counting open tasks

Open tasks are lines matching `- [ ]` or `- [~]` (sub-tasks included) that do not contain `needs-human`. For example:

```sh
grep -E '^\s*- \[( |~)\]' PLAN.md | grep -vc needs-human
```

Counts on `goal/phase-2` at `ac098f7` plus this file's PLAN.md fixes (sub-tasks counted as tasks; needs-human parents "blocked only on the sub-task(s) below" counted as needs-human; the Phase 1 test media line is done with its VP9 and AV1 part tagged, so it counts as done):

| Phase | Done | Open | Needs-human |
| --- | --- | --- | --- |
| 0 Decisions and prototypes | 7 | 0 | 2 |
| 1 Skeleton and CI | 21 | 0 | 7 |
| 2 Media engine | 25 | 0 | 6 |
| 3 Playback engine | 10 | 5 | 2 |
| 4 GPU compositor | 13 | 0 | 0 |
| 5 Project model | 12 | 2 | 0 |
| 6 Timeline UI | 3 | 17 | 0 |
| 7 Core editing | 0 | 9 | 0 |
| 8 Export | 12 | 5 | 4 |
| 9 Built-in library | 5 | 27 | 2 |
| 10 Easy mode and assist | 0 | 15 | 0 |
| 11 Colour | 0 | 10 | 0 |
| 12 Audio | 0 | 10 | 0 |
| 13 Advanced compositing | 0 | 10 | 0 |
| 14 Multicam and workflow | 0 | 8 | 1 |
| 15 Plugins and scripting | 0 | 7 | 0 |
| 16 Performance and quality | 2 | 7 | 0 |
| 17 Distribution | 2 | 6 | 2 (and 1 dropped) |
| 18 Documentation | 0 | 6 | 0 |
| **Total** | **112** | **144** | **26** |

### Last full check

The last full workspace test the coordinator recorded was on `ea1cf88` (the reference project merge): 990 passed, 0 failed, 6 ignored, with `smoke` playing and exporting the reference project. Merges since then were checked by each branch's scoped finish check. Run the full check on the integration branch before the next merge to `main`.

- Timing tests follow Decision 64: a default-mode failure is a regression, not load. The strict figures (`GLASS_STRICT_TIMING=1`) still swing with the GPU clock: a quiet 4K cross dissolve pass takes 1.3 or 2.2 ms.
- The 4K `dissolves_at_4k_gpu_time` and `cross_dissolve_at_4k_gpu_time` tests failed at 2.2 to 2.4 ms against 2.0 ms on the shared GPU in the reference project and motion transitions finish checks (Progress Log); check them in the next full run.

## 2. Needs a person

**Fonts**
- Add Geist and Geist Mono (OFL): the four TTFs listed in `assets/fonts/README.md` plus `OFL.txt`, into `assets/fonts/`. Then regenerate every UI snapshot golden: `GLASS_UPDATE_SNAPSHOTS=1 scripts/cargo-serial test -p ui --test ui_snapshots`, review the PNGs, commit. Installers must then ship `OFL.txt`.

**GitHub remote and CI**
- Create the GitHub repository and push `main` and `goal/phase-2`.
- Set `repository` in the root `Cargo.toml` (placeholder `https://github.com/OWNER/glass`), run `dist generate`, commit.
- Confirm CI: the check matrix (macOS arm64, macOS x86_64, Windows x86_64, Linux x86_64: build, test, clippy, fmt, deny, i18n and theme checks), the FFmpeg build per target (cached on the `versions.env` hash), the tagged release (DMGs, MSI, MSIX, AppImage, sources attached), the bench job (merge once to `main` to seed the baseline, then open a PR and check the comment), the UI snapshot step on the `macos-14` runner, and the visual regression step (`cargo test -p render --test visual_regression`), which has never run on GitHub.
- The strict timing step (`GLASS_STRICT_TIMING=1`, Decision 64) has never run in CI; its GPU medians swing with the GPU clock, so it may need a warmed GPU or reviewed budgets.
- CI jobs without generated fixtures (Windows) fail `smoke` until they run `tests/fixtures/generate.sh` (Decision 66).
- Windows and Linux builds of the hardware-frame types and the NVENC, QSV and AMF encoder paths compile with `cfg!` gates; only CI checks them.

**FFmpeg encoders and fixture tools** (new source downloads)
- AV1, VP9 and MP3 export (line 283): the pinned FFmpeg 9.0.2 build has no AV1, VP9 or MP3 encoder; SVT-AV1 or libaom, libvpx and LAME need new pinned archives in `scripts/ffmpeg/versions.env` and a rebuild. WebM export (and its subtitles) is untestable until then.
- Fixtures (line 144): `vp9_720p30.webm` and `sync_vp9_opus.webm` (libvpx, libopus), `av1_1080p30.mp4` (SVT-AV1), `audio_44k1_mono.mp3` (LAME), `anim_vfr_160x120.webp` and `still_marked_640x360.webp` (libwebp's `cwebp` and `img2webp`, not on this Mac). The pinned FFmpeg has the decoders; `generate.sh` skips these files with a note and their tests skip. Phase 1's test media line (line 111) is done with this part tagged.

**Hardware**
- Windows PC: proto-decode via D3D11VA (line 97), D3D11VA hardware decode (line 146), HEIC/HEIF/AVIF through WIC (line 154).
- NVIDIA GPU: proto-gpu on a GTX 1660 class GPU (line 95: `cargo run --release` in `experiments/proto-gpu/`).
- NVIDIA and Intel or AMD Linux: NVDEC and VAAPI decode (line 147).
- NVIDIA, Intel and AMD GPUs on Windows or Linux: NVENC, QSV and AMF encode (line 287).
- Audio devices and a person listening: the master clock check (line 186): `GLASS_AUDIO_DEVICE_TEST=1 cargo test -p audio --test device` on built-in speakers, USB and Bluetooth outputs, and lip sync by eye and ear once the engine plays through the clock.
- An Apple M1: the M2 and then the M5 here stood in for the M1 reference in every measurement; rerun the benchmarks there.
- An unlocked screen: windowed rerun of `experiments/proto-ui` (offscreen numbers are in the Progress Log).

**Signing, identity, licences**
- macOS notarisation (line 460) and Windows code signing (line 462): certificates and an Apple Developer account; MSIX and a notarised DMG need them.
- Choose the bundle id (placeholder `org.glass.Glass`). dist artifacts are named `app-<target>` after the `app` package; consider renaming the package to `glass`.
- Licence the music (line 337) and SFX (line 338) catalogues.

**Design**
- Track headers took their sizes from theme tokens because the Paper board could not be read (the server timed out); compare them with 03 Timeline.

**Every needs-human line in PLAN.md** (line numbers on this branch, after the fixtures tag)
- Line 95: `proto-gpu` on a GTX 1660 class GPU
- Line 97: proto-decode on Windows via D3D11VA
- Line 111: reference media (done; the VP9 and AV1 fixtures tagged)
- Line 112: GitHub Actions matrix
- Line 114: FFmpeg build script (blocked only on line 118)
- Line 118: CI job building FFmpeg per target and attaching sources
- Line 119: `cargo-dist` config (blocked only on line 122)
- Line 122: tagged release in CI producing every installer
- Line 125: benchmark harness (blocked only on line 127)
- Line 127: `bench` job posting a PR comment
- Line 142: software decode tested against every fixture (blocked only on line 144)
- Line 144: the six fixtures needing extra encoders and libwebp tools
- Line 146: hardware decode D3D11VA
- Line 147: hardware decode NVDEC and VAAPI
- Line 152: stills, sequences, GIF, WebP, HEIC (blocked only on line 154)
- Line 154: HEIC, HEIF and AVIF on Windows through WIC
- Line 182: master clock (blocked only on line 186)
- Line 186: listening check on real audio hardware
- Line 278: encoder wrapper (blocked only on line 283)
- Line 283: AV1, VP9 and MP3 encoders
- Line 284: hardware encode (blocked only on line 287)
- Line 287: NVENC, QSV and AMF
- Line 337: music pack
- Line 338: SFX pack
- Line 427: v1.0 checklist (certificates)
- Line 460: macOS notarisation
- Line 462: Windows code signing

## 3. Decisions made so far

Full text in `docs/DECISIONS.md`; one line each here, in number order (the file itself is in merge order).

1. Phase 0 defaults table adopted as the starting stack.
2. MIT source, free; licensing and activation dropped.
3. Priority: speed, then features, then size; release builds have every feature flag on.
4. Creators first; pro tools grow with them.
5. GPL FFmpeg with x264 and x265; release builds under GPLv3.
6. Three commits per task (start, implementation, done citing its hash).
7. "Under X% CPU" judged as whole-machine utilisation, per-core numbers recorded.
8. FFmpeg pinned to 9.0.x through `ffmpeg-next` 9.
9. Phase 0 closed with the hardware checks left needs-human.
10. App icon small-size rule (slices by drawn size; glow as a tile rule).
11. egui without `default_fonts`; Geist ships from `assets/fonts/`.
12. A phase merges when its locally verifiable Done-when parts pass.
13. dav1d in the pinned FFmpeg build for AV1 software decode.
14. Content hash (BLAKE3 sampled) without the modification time.
15. Working primaries follow the project colour space (BT.709, BT.2020 for HDR).
16. Parallel tasks in separate worktrees, cargo serialised.
17. `imbl` instead of `im` for the edit tree.
18. Scoped checks on task branches, the full test on the integration branch.
19. Shader hot reload dependencies (`notify`, `naga`) are ordinary, not profile-gated.
20. Blend modes on premultiplied linear light, HDR-safe.
21. Output: inverse BT.1886 for SDR video, straight alpha, triangular dither.
22. Autosave in the per-user cache folder until projects have their own; its own format version, never migrated.
23. Layer transforms sampled inside the composite pass.
24. Crop in layer pixels before the transform, with an inward feather.
25. Text: coverage atlas, vector outline strokes, shadow as a graph pass.
26. Media pool items keyed by `MediaId`; colour labels use the clip palette.
27. Frame queues: `rtrb` for one producer, crossbeam's `ArrayQueue` for the decode pool.
28. `cargo-serial` forgets workspace fingerprints when the checkout changes.
29. Offline media is runtime state beside the project, shown as a cached slate.
30. Loudness through the `ebur128` crate.
31. Project files: versioned envelope, migrations on untyped JSON, newer files refused.
32. Vector shapes by GPU coverage accumulation.
33. Thread registry with seqlock accounting; decode pool of 2 to 4.
34. Master clock: callback frame counter, delay-locked loop, slewed output.
35. Rendered-frame cache: one LRU across VRAM and RAM, ceilings from the machine.
36. LUTs: declared input space, tetrahedral interpolation in the shader.
37. Portable mode: a `Glass Data` folder beside the executable.
38. Visual regression: a test-side scene registry, sharing the UI snapshot comparison.
39. Keymaps: one primary modifier, preset plus overrides, no key sequences.
40. Implicit gaps, per-kind tracks, markers in label colours.
41. Relinking: content hash decides, then weighted name and probe facts; one edit per batch.
42. About dialog: shipped third-party list, fixed brand colours for the app tile.
43. Compiler cap raised to 8 jobs (the M5 with 24 GB).
44. Masks: shape coverage unioned, Gaussian feather, combined in list order before the blend.
45. Clip properties: derived source range, a speed map of keys, centre-relative transform.
46. Vector shapes: sparse strips over the coverage accumulation.
47. Export encoder wrapper in `media::export`; native ProRes on macOS; GIF through the `gif` crate; AV1, VP9 and MP3 wait for the FFmpeg build.
48. Transitions: a folder per transition, a shared WGSL prelude, easing on the CPU.
49. Look-ahead decode: one worker and one lane per segment.
50. Dissolve family: dips by a midpoint, film in a 2.4 power curve, additive capped at max(white, brighter input).
51. Hardware encode: an encoder preference with fallback on open and on the first frames.
52. Adaptive quality: a pure governor in render, resolution first, drops only at a quarter.
53. Scrub mode: nearest cached frame at once, exact frame in software after a 40 ms pause.
54. Transitions in the project: nodes on a track's cuts, refused edits, fitted neighbours, head handles only.
55. Project settings: project defaults plus a full format per sequence, rate changes refused off the grid, the cache folder (and autosave) beside the project file.
56. Frame queues park on their own mutex-and-condvar parker.
57. Hardware decoders are reopened on every keyframe seek.
58. Export presets: Rust data in `media::export::presets`, resolved against the timeline.
59. Output verification: findings as i18n keys, hard bounds fail, soft bounds warn.
60. Timeline playback: segments from the sequence, one graph per frame, deadlines from the clock.
61. Wipes: coverage of a moving edge in output pixels, filtered by one kernel, with margins for softness and border.
62. Motion transitions: moved frames over transparent black, blur by taps in one pass.
63. Keyframes: curves in source time, After Effects-style bezier handles, one solver copied with a cross-check.
64. Timing tests: serial sections, thread CPU time, load-robust statistics by default, the strict ones under `GLASS_STRICT_TIMING=1`.
65. Subtitle export: sidecars from the model, `mov_text` and WebVTT streams, burn-in from the frame size.
66. The reference project: generated in code, media relative to the file, `smoke` reaches the other crates through the engine.
67. Dirty ranges from the project diff, cache keys by content, no eviction on edit, plans swapped between frames.
68. Track header switches and heights are UI state until a schema version stores them.
69. Nested sequences and compound clips: a clip source of media or a sequence, a compound flag, refused cycles.
74. Canonical project file: pretty JSON, declared field order, sorted maps, exact floats, no schema bump.

70 to 73, 75 and 76 belong to the branches in flight (section 1).

## 4. Open follow-ups

**Phase 2 (media)**
- Image sequences probe at FFmpeg's default 25 fps; anything that probes sequences must honour the sequence or user frame rate (`VideoSource` does; `probe` does not).
- Hardware seeks reopen the VideoToolbox decoder on every keyframe seek, which costs a 4K seek about 15 to 25 ms (Decision 57). Flushing could be kept for IDR keyframes (closed GOPs) once the GOP index records which keyframes are IDR; that is a GOP index format change.
- Five fixtures were reported at the stale-fixture task's merge as not regenerating to the same bytes on every run; this is not in PLAN.md yet. Find them (run `tests/fixtures/generate.sh --force` twice and compare file hashes) and pin what varies, as `threads=16` did for x264.
- Metadata: HEIC/AVIF EXIF through ImageIO; the created-date time zone offset; check the metadata panel follows the media pool selection.
- Media pool: `ImportedClip` → `MediaItem` conversion in `engine`/`app`, a tag editor, drag to the timeline (Phase 6). In a debug build, the first frame after a new search result took 73 to 120 ms.
- Relink is not wired into an app shell yet: the folder picker and progress events reach no window.

**Phase 3 (playback)**
- In progress on `task/timeline-anim`: the engine renders keyframed values per frame and nested sequences as layers. Until it merges, nested clips draw nothing (Decision 69) and the engine reads constant clip values; animated clips need their evaluated values in the look hash (Decision 67).
- App shell wiring: the app creates a `TimelinePlayback` per sequence, shows `PresentedFrame`s by their due time and starts a `Scrubber` (Decision 60); on each edit it builds the new plan and dirty frames, calls `set_plan`, and restarts a scrubber started before the edit (Decision 67); a new canvas or frame rate needs a new `TimelinePlayback`.
- VideoToolbox through FFmpeg decodes one frame at a time (4K: 110 fps VT vs 478 fps software on synthetic clips, M2). Consider a direct async `VTDecompressionSession` or several decoders; benchmark with real footage.
- `HwPreference` defaults to `Off`; playback and `media_check` use `Auto`.
- Playback must switch to proxies (proxies are video-only; stills and sequences have none), and callers using proxies scale transform, crop, wipe softness and border values. The proxy/full-res toggle is Phase 14.
- Frame cache: re-uploading a 4K frame from RAM costs about 7 ms of CPU, so the scheduler may prefer re-rendering a cheap frame over a RAM hit.

**Phase 4 (compositor)**
- When `render` joins eframe's wgpu device, request `wgpu::Features::TEXTURE_FORMAT_16BIT_NORM` (10-bit P010 surfaces) on the shared device.
- Sources in other primaries go through `GamutNode` into the working primaries (Decision 15); make sure every source path in the compositor inserts it.
- Shader hot reload is not wired into the app yet (wire it when the app creates its `Renderer`).
- Budgets: 5 blended layers at 4K take 5.46 ms, a 4K title 7.2 ms (a half-resolution shadow blur would help). Sparse strips moved shape work to the CPU: building the 50-shape node takes 1.6 to 1.8 ms in release (Decision 46); cache the strips while shapes are unchanged.
- Masks cost a canvas-sized coverage pass per shape whatever its size; limiting a mask to its shapes' bounds is a later optimisation (Decision 44).
- No mipmaps for strong downscales; `text_glyph.wgsl` is not hot-reloaded; the text animator should lay out at the final size.
- Effect parameters: samples caption, shutter dial and keyframe tooltip still to do; check the schema defaults against the nodes' own defaults; the transform schema's absolute defaults must map to the clip's centre offsets (Decision 45).
- LUT: an inspector row to pick the `.cube` file, log input spaces (Decision 36 has display-referred Rec.709, sRGB and linear); the reference project has no LUT effect because there is no `.cube` fixture.

**Phase 5 (project)**
- The app's project open does not resolve media paths relative to the project file yet; only `smoke` does (Decision 66).
- The recovery dialog is not shown yet (no app window); the launch scan reads only the per-user cache folder until a recent-projects list exists (Decision 55); detect a second running instance.
- A roll edit must be one edit that moves both clip edges, or the first trim removes the transition (Decision 54).
- Copy and paste must carry a compound with its clip across projects; open-in-place for compounds is UI (Decision 69).
- Track header switches and heights are lost on close until a schema version stores them (Decision 68).

**Phase 6 (panels)**
- High-contrast component differences to implement in component code: status pill `bg/2` fill, selected tab accent ring.
- The snapshot mono font (Courier New, until Geist lands) has no `⌘`, so shortcut keys render as `?I` in the goldens. It goes away with Geist Mono; if Geist stays out long, add a symbol fallback font.
- Shortcuts are not wired into an app shell: no `keymap.json` load or save and no dispatch from a window yet (the open sub-task). Track header actions are not shortcut commands yet. The first showing of a keyboard editor state costs up to 57 ms (text layout); warm it or lay out lazily.
- The shared `Paint::floating` glass background shows a band at mid-height in tall dialogs.
- The debug panel reads thread activity through `threads::global().snapshot_into`.

**Phase 8 (export)**
- Export audio is silence of the right length: the engine has no sequence audio mix yet, so `engine::export` pushes zeros and skips the loudness target (Decision 70). Replace it with the mix once Phase 12 builds one, and check the preset's loudness again. WebM subtitles are untested until an AV1 or VP9 encoder is in the FFmpeg build.
- The export queue is not wired into an app window yet: the export dialog submits `ExportJob`s, drains `ExportEvent`s once per frame and turns `Notification`s into OS notifications. An HDR project exported with an SDR preset is clipped (no tone mapping).
- In progress on `task/vt-bframes`: VideoToolbox H.264 with B-frames failed in `ExportSession` on macOS with `Auto`, which every web preset uses.
- GIF encodes at about 8.5 fps at 1080p; speed it up before GIF export is offered for long ranges.
- Interlaced broadcast delivery (1080i) needs field weaving and a field-coding ProRes encoder (Decision 58).

**Phase 9 (library)**
- Blurred whip pan and zoom take 5.4 and 7.1 ms at 4K, over the 3 ms aim and held only to the 8 ms frame budget (Decision 62); a reduced-resolution multi-pass blur is in progress on `task/transition-passes`. Zoom out minifies without mipmaps.

**Phase 12 (audio)**
- In progress on `task/audio-mixer`: the mixer. The engine does not mix audio yet, so `smoke` exports silence; once it mixes, `smoke` should push the mixed A1 track and verify loudness (Decision 66).

**Phase 14 / 16 (performance)**
- Proxy pipeline decodes, scales and encodes on one thread; splitting decode/scale and encode across threads could lift ProRes VT 4K from about 130 to about 230 fps.
- The strict timing figures swing with the GPU clock (section 2).
- Integration test binaries could be consolidated per crate to cut link time.

**Phase 17 / release**
- The About dialog and the update pill are not opened from any window until the app shell exists.
- Portable mode: the crash monitor's socket should live under `<data>/run`.
- `repository` placeholder, bundle id, `app-<target>` artifact names, OFL.txt in installers (section 2).

**Documents**
- The `PLAN.md` Decision Log lists entries in merge order, which differs from the numbering in `docs/DECISIONS.md` (whose sections are also in merge order). The Progress Log has the frame cache line (2026-10-01) among the 2026-09-30 lines.
- `CONTRIBUTING.md` ("Machine limits": an M2 with 8 GB) and the top of `docs/ARCHITECTURE.md` (measurements from an M2) still describe the old machine.

## 5. How to resume

**Working unattended.** The owner is not available for questions while the goal runs. Never stop to ask. Decide with PLAN.md's defaults, the Decision Log, and this file, and record any new decision in both logs. When something truly needs a person (a paid account, a certificate, a licence purchase, hardware not on this Mac, the GitHub remote, deleting user data, a download other than crates and the pinned FFmpeg source archives, or copying files from other projects on this machine), tag the task `(needs-human: <reason>)`, add a Progress Log line, and move on to the next task. End every turn with one line: `Open tasks remaining: N`.

**Session loop.** Follow "Working on a task" in `CONTRIBUTING.md` (and PLAN.md's "How to run this plan as a goal"): read PLAN.md and the last 20 Progress Log lines, `df -h /`, `source scripts/dev-env.sh`, run the full check, pick the next task, and use the three-commit rhythm: `plan: start <task>` (mark `[~]`), `<area>: <what changed>`, `plan: done <task>` (mark `[x]`, Progress Log line `- YYYY-MM-DD — Phase N — <task> — <impl hash> — <key numbers>`).

**Parallel worktrees** ("Working in parallel" in `CONTRIBUTING.md`):
- One worktree per task: `git worktree add ../glass-<name> -b task/<name> goal/phase-2`, then `scripts/worktree-setup.sh` inside it (generates its test media, prints the shared `CARGO_TARGET_DIR` and `FFMPEG_DIR`).
- Every shell: `source scripts/dev-env.sh`; every compiling cargo command through `scripts/cargo-serial` (one at a time machine-wide; `cargo fmt` may run directly). All worktrees share `/Users/ryan/Desktop/Glass/target` and the pinned FFmpeg in `.glass-cache/ffmpeg/aarch64-apple-darwin/`; no `cargo clean` while others work.
- Finish checks: run the whole scoped check in one lock hold with `GLASS_CARGO_BIN=$PWD/scripts/finish-check.sh scripts/cargo-serial -p <crate> -p <dependent crate> ...` (workspace build, the listed crates' tests, clippy `--all-targets -D warnings`, smoke; it forgets the workspace fingerprints first and keeps going past a failing step), then `cargo fmt --all -- --check` directly. Write logs into the worktree's own `.glass-cache/`.
- `PLAN.md` and `docs/DECISIONS.md` use git's union merge driver: after every merge run `scripts/check-plan-dups.py PLAN.md` (exits 1 and prints any task line kept twice, such as a `[~]` and an `[x]` copy) and delete the stale copy. Parallel branches each take the next free Decision number, so the later merge renumbers its Decision (heading in `docs/DECISIONS.md` and every "Decision N" reference) and commits that as a docs change.
- Before reporting, a task branch merges `goal/phase-2` in and reruns its finish check; then `task/<name>` merges into `goal/phase-2` with `--no-ff`, `scripts/check-plan-dups.py PLAN.md` and the finish check run on the result, the worktree is removed and the branch deleted. After each batch of merges, run the full `cargo test --workspace` on `goal/phase-2` (the detached `../glass-integration` checkout is there for that: `git checkout --detach <hash>` and run the full check there). Never commit to `main`; merge a phase branch to `main` only when its locally verifiable Done when passes (Decision 12).
- Shared files (`PLAN.md`, `docs/ARCHITECTURE.md`, `assets/i18n/en.json`, `Cargo.toml`, `Cargo.lock`): keep edits small and local; add new modules rather than reorganising.
- Any change to `Cargo.lock`, including a merge that changes it, needs `scripts/licences/third-party.py` rerun and its `assets/licences/third-party.tsv` committed, or the `assets` licence test fails (`scripts/licences/third-party.py --check` tells you without writing).

**Working conventions not spelled out in CONTRIBUTING.md**
- Design lives in the Paper file "Inspiration 2", file id `01M3N8N2AWRZ7XXZY58Q1RH2BT`. Always pass the file id and an explicit page id (the user may be viewing another file or page; never switch their page). Page ids: `p-1-0` 00 Foundations, `p-C-0` 01 First run, `p-B-0` 02 Edit workspace, `p-A-0` 03 Timeline, `p-9-0` 04 Browsers, `p-8-0` 05 Inspector & keyframes, `p-7-0` 06 Captions & assist, `p-6-0` 07 Colour, `p-5-0` 08 Audio, `p-D-0` Brand, `p-4-0` 09 Export, `p-3-0` 10 System & states, `p-2-0` 11 Platform, theme, RTL. Read values with `get_jsx` and `get_computed_styles`, never from screenshots. Do not edit the file unless a task says so. Never put node ids in docs or reports.
- The Mac's screen is usually locked and eframe skips painting occluded windows, so tests, benchmarks and the frame-time monitor run headless/offscreen (`ui::headless`, `experiments/proto-ui/src/offscreen.rs`). Only a check that truly needs a visible window gets a needs-human sub-task.
- Never copy files from other projects on this machine into the repo. The only downloads are crates through cargo and the pinned, checksummed source archives in `scripts/ffmpeg/versions.env` (FFmpeg, x264, x265, dav1d, pkgconf, nasm, meson, ninja). No fonts, models or asset files. Outside the repo only `cargo install` of tools PLAN.md names (`cargo-sweep` 0.8 is installed) and `rustup` components; no `brew install`.
- Commit messages carry no attribution trailers or footers.
- Measurements say that the M5 stands in for the M1 reference and that the machine was shared with other agents (numbers before 2026-10-11 were taken on an M2). A frame-budget or timing test that fails in its default mode is a regression, not load (Decision 64): do not rerun it to get a pass.
- Split tasks: the parent keeps its checkbox and becomes `[x]` when every sub-task is `[x]` or `[-]`; if only needs-human sub-tasks remain it gets `(needs-human: blocked only on the sub-task(s) below)`. Needs-human: do the local part, tag the line `(needs-human: <reason>)`, add a Progress Log line, commit `plan: needs-human <task>`, move on. A task that needs a CI job adds it to `ci.yml`, verifies locally what it can and tags only the CI part.

**Machine guidance.** This Mac is an Apple M5 with 10 cores and 24 GB RAM; about 810 GiB of disk was free on 2026-10-11 (check `df -h /`). Up to about 7 agents work in parallel, each in its own worktree, sharing one cargo lock: `scripts/cargo-serial` runs one compiling command at a time machine-wide, and the compiler cap is 8 jobs (Decision 43). A wait for the lock can outlast a 10-minute shell timeout, so run cargo-serial commands in the background and wait for them to finish. `scripts/cargo-serial` forgets the workspace fingerprints whenever the building checkout changes (Decision 28), so with many worktrees nearly every cargo command recompiles the workspace crates; batch a task's cargo steps into one lock hold (`scripts/finish-check.sh`) instead of queueing them one by one, and scope commands (`-p <crate>`, `--test <name>`) while iterating. Task worktrees set `CARGO_INCREMENTAL=0`. Never `cargo clean`, `cargo sweep` or delete anything under `target/` while others work. Never delete source or anything outside the repo.

## 6. Suggested next work, in order

1. **Close Phase 2 on `main`.** Land the motion transitions timing conversion (`task/transition-passes`), run the full check on `goal/phase-2`, get the review recheck, then merge to `main` (Decision 12).
2. **Merge the branches in flight** (section 1), one at a time, each with `scripts/check-plan-dups.py PLAN.md` and its finish check; renumber Decision collisions.
3. **Phase 3 remaining:** JKL shuttle, reverse playback and audio scrubbing; loop range, in and out points and play-around; background render of idle regions; clean feed; benchmarks (scrub latency, frame drops, cache hit rate).
4. **Phase 5 remaining:** adjustment layers (a schema bump; item-relative key times, Decision 63), then copy and paste (carrying compounds, Decision 69). One schema bump at a time.
5. **Phase 6** (timeline UI): clip rendering, zoom and scroll, playhead and markers, snapping, selection, tools and trims, then the shortcuts sub-task and debug panel once an app shell exists; the app shell wiring from section 4 (playback, set_plan per edit, relative media on open, recovery dialog) belongs with it.
6. **Phase 8 remaining:** custom preset editor, smart export, export ranges; then the v0.1 checklist.
7. **Phase 9:** stylised, shape and 3D transitions, the transition browser, default transition, easing editor, titles.

**What can run in parallel** (about 7 agents, one cargo lock):
- Engine track (`engine`): Phase 3 remaining tasks, after `task/timeline-anim` merges.
- Project track (`project`): Phase 5 remaining, strictly one schema bump at a time.
- UI track (`ui`): Phase 6 panels; each new panel gets a snapshot case and a frame-budget test.
- Render track (`render`, `assets/transitions/`): Phase 9 transitions after `task/transition-passes` merges (it changes the transition framework).
- Export track (`media::export`): Phase 8 remaining after `task/export-queue` and `task/vt-bframes` merge.
- Audio track (`audio`): Phase 12 after the mixer merges.
