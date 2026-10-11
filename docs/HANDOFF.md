# Handoff: resuming the goal

State of the work on 2026-10-11, when the session wrapped up with `goal/phase-2` at the app shell merge (`9af7eca`), and how to pick it up in a fresh session. `PLAN.md` stays the single source of truth; this file points into it. Line numbers below are `PLAN.md` lines on `goal/phase-2` after this refresh.

## 1. Where things are

| Branch | Contents |
| --- | --- |
| `main` | Phases 0, 1 and 2 merged: `416f085` merges `goal/phase-2` as reviewed at `311f0ee` (the reviewer's full workspace test passed 1101 tests). Everything merged into `goal/phase-2` after `311f0ee` is not on `main` yet. |
| `goal/phase-2` | Phase 2 (no open tasks, only needs-human ones), Phase 4 (complete), most of Phases 3 and 5, the first Phase 6 panels and the app shell, most of Phase 8, the first Phase 9 transition families, the Phase 12 mixer core and crossfades, and parts of Phases 16 and 17 (below). The integration branch: new task branches start here. Project schema v9, autosave `FORMAT_VERSION` 9. |

**Nothing is in flight.** No task branches or task worktrees remain; every task branch merged into `goal/phase-2` and was deleted. The detached `../glass-integration` checkout used for full checks on the integration branch was removed at the wrap-up; recreate it with `git worktree add --detach ../glass-integration goal/phase-2` when a full check is due. A final review of `9af7eca` was running when this was written; its findings are not in this file (the snapshot goldens question in section 4 has since been resolved).

Phase 2's Done when asks that every fixture decodes to GPU textures and f32 audio through the same API, with hardware decode on both platforms, and that `smoke` plays the reference project. On this Mac every fixture the pinned FFmpeg can generate decodes to textures (`crates/engine/tests/fixture_textures.rs`, 31 picture fixtures), and `smoke` opens `tests/fixtures/reference.proj`, plays 2 s through `engine::timeline::TimelinePlayback`, exports 1 s with the mixed audio through the export path and verifies the file (Decision 66). Six fixtures cannot be generated here (line 144) and Windows and Linux hardware decode need other machines (lines 146 and 147).

**Done on `goal/phase-2`.**

- Phase 2: safe FFmpeg wrapper, probe, software decode (fixtures needing extra encoders excepted, see section 2), VideoToolbox decode, software fallback, colour space plumbing, VFR normalisation, accurate seek (hardware decoders reopened on every keyframe seek, Decision 57; a seek inside the frame handed out last now returns that frame), stills and sequences (macOS part), audio decode and resampling, analysis jobs, thumbnail strips, waveform peaks, proxies, scene cuts, silence, beats and tempo, loudness, import, media pool, relinking, offline slate, metadata panel; texture coverage of every generated fixture and recipe-hash regeneration of stale fixtures in `tests/fixtures/generate.sh`.
- Phase 3: thread layout, lock-free frame queues (Decision 56), master clock (listening check needs-human), rendered-frame cache, look-ahead decode (Decision 49), dirty-range invalidation (Decision 67), animated and nested rendering (Decision 75), adaptive quality (Decision 52), scrub mode (Decision 53), the timeline playback pipeline (`engine::timeline`, Decision 60).
- Phase 4: complete. Render graph, linear-light working space and dithered output, blend modes, transform, opacity and crop, shader hot reload, effect parameters and their auto-UI, text, vector shapes within the 4K budget (Decision 46), the mask node (Decision 44), readback ring, visual regression suite.
- Phase 5: edit tree, undo and redo, autosave (Decision 55), versioned schema with migrations up to v9, sequences, tracks, clips, gaps and markers, clip properties (v4, Decision 45), transitions (v5, Decision 54), project settings (v6, Decision 55), keyframe curves (v7, Decision 63), nested sequences and compound clips (v8, Decision 69), the media pool and track switches in the project (v9, Decision 77), the reference project (Decision 66), the canonical pretty JSON project file (Decision 74). Open: adjustment layers (line 230), copy and paste (line 233).
- Phase 6: track headers (Decision 68, switches stored since v9), clip rendering within the frame budget (Decision 78), zoom and scroll with the minimap (Decision 72), dockable panels, saveable workspaces and presets (the app shell, Decision 79), remappable shortcuts: the command registry and presets, the keyboard editor, and `keymap.json` loaded, saved and dispatched by the app.
- Phase 8: encoder wrapper (AV1, VP9 and MP3 needs-human, Decision 47), hardware encode with VideoToolbox and fallback (NVENC, QSV, AMF needs-human, Decision 51) and the VideoToolbox B-frame fix, presets (Decision 58), background export queue (Decision 70), subtitle export (Decision 65), output verification (Decision 59).
- Phase 9: LUT framework, transition framework (Decision 48), dissolves (Decision 50), wipes (Decision 61), motion (Decision 62), multi-pass transitions (Decision 71), stylised (Decision 80).
- Phase 12: mixer core and meters, buses and sends, automation lanes, offline render, the mix in the export queue and `smoke` (Decision 73); sample-accurate editing and crossfades (Decision 81).
- Phase 16: UI snapshot tests; timing tests robust under load (Decision 64).
- Phase 17: portable mode, About dialog (with its layout cache).
- Tooling: `scripts/finish-check.sh`, `scripts/check-plan-dups.py`, the `cargo-serial` fingerprint fix (Decision 28), `scripts/dev-env.sh` finding `~/.cargo/bin` in non-login shells, compiler cap of 8 jobs (Decision 43).

### Counting open tasks

Open tasks are lines matching `- [ ]` or `- [~]` (sub-tasks included) that do not contain `needs-human`. For example:

```sh
grep -E '^\s*- \[( |~)\]' PLAN.md | grep -vc needs-human
```

Counts on `goal/phase-2` at `9af7eca` (sub-tasks counted as tasks; needs-human parents "blocked only on the sub-task(s) below" counted as needs-human; the Phase 1 test media line is done with its VP9 and AV1 part tagged, so it counts as done). No line is `[~]`.

| Phase | Done | Open | Needs-human |
| --- | --- | --- | --- |
| 0 Decisions and prototypes | 7 | 0 | 2 |
| 1 Skeleton and CI | 21 | 0 | 7 |
| 2 Media engine | 26 | 0 | 6 |
| 3 Playback engine | 11 | 5 | 2 |
| 4 GPU compositor | 13 | 0 | 0 |
| 5 Project model | 12 | 2 | 0 |
| 6 Timeline UI | 7 | 14 | 0 |
| 7 Core editing | 0 | 9 | 0 |
| 8 Export | 14 | 4 | 4 |
| 9 Built-in library | 7 | 26 | 2 |
| 10 Easy mode and assist | 0 | 15 | 0 |
| 11 Colour | 0 | 10 | 0 |
| 12 Audio | 6 | 11 | 0 |
| 13 Advanced compositing | 0 | 10 | 0 |
| 14 Multicam and workflow | 0 | 8 | 1 |
| 15 Plugins and scripting | 0 | 7 | 0 |
| 16 Performance and quality | 2 | 7 | 0 |
| 17 Distribution | 2 | 6 | 2 (and 1 dropped) |
| 18 Documentation | 0 | 6 | 0 |
| **Total** | **128** | **140** | **26** |

### Last full check

The last full workspace test was the reviewer's on `311f0ee`, before the merge to `main`: 1101 passed. Every merge since (audio mixer, VideoToolbox B-frames, nested contract tests, clip rendering and its frame budget work, animated and nested rendering, schema v9, seek within the current frame, stylised transitions, audio crossfades, app shell) was checked by its branch's scoped finish check. Run the full check on `goal/phase-2` (in `../glass-integration`) before the next merge to `main`.

- Timing tests follow Decision 64: a default-mode failure is a regression, not load. The strict figures (`GLASS_STRICT_TIMING=1`) swing with the GPU clock: a quiet 4K cross dissolve pass takes 1.3 or 2.2 ms.

## 2. Needs a person

**Fonts**
- Add Geist and Geist Mono (OFL): the four TTFs listed in `assets/fonts/README.md` plus `OFL.txt`, into `assets/fonts/`. Then regenerate every UI snapshot golden: `GLASS_UPDATE_SNAPSHOTS=1 scripts/cargo-serial test -p ui --test ui_snapshots`, review the PNGs, commit. Installers must then ship `OFL.txt`.

**GitHub remote and CI**
- Create the GitHub repository and push `main` and `goal/phase-2`.
- Set `repository` in the root `Cargo.toml` (placeholder `https://github.com/OWNER/glass`), run `dist generate`, commit.
- Confirm CI: the check matrix (macOS arm64, macOS x86_64, Windows x86_64, Linux x86_64: build, test, clippy, fmt, deny, i18n and theme checks), the FFmpeg build per target (cached on the `versions.env` hash), the tagged release (DMGs, MSI, MSIX, AppImage, sources attached), the bench job (merge once to `main` to seed the baseline, then open a PR and check the comment), the UI snapshot step on the `macos-14` runner, and the visual regression step (`cargo test -p render --test visual_regression`), which has never run on GitHub.
- The strict timing step (`GLASS_STRICT_TIMING=1`, Decision 64) has never run in CI; its GPU medians swing with the GPU clock, so it may need a warmed GPU or reviewed budgets.
- CI jobs without generated fixtures (Windows) fail `smoke` until they run `tests/fixtures/generate.sh` (Decision 66); the Windows CI fixtures need that step.
- Windows and Linux builds of the hardware-frame types and the NVENC, QSV and AMF encoder paths compile with `cfg!` gates; only CI checks them.

**FFmpeg encoders and fixture tools** (new source downloads)
- AV1, VP9 and MP3 export (line 286): the pinned FFmpeg 9.0.2 build has no AV1, VP9 or MP3 encoder; SVT-AV1 or libaom, libvpx and LAME need new pinned archives in `scripts/ffmpeg/versions.env` and a rebuild. WebM export (and its subtitles) is untestable until then.
- Fixtures (line 144): `vp9_720p30.webm` and `sync_vp9_opus.webm` (libvpx, libopus), `av1_1080p30.mp4` (SVT-AV1), `audio_44k1_mono.mp3` (LAME), `anim_vfr_160x120.webp` and `still_marked_640x360.webp` (libwebp's `cwebp` and `img2webp`, not on this Mac). The pinned FFmpeg has the decoders; `generate.sh` skips these files with a note and their tests skip. Phase 1's test media line (line 111) is done with this part tagged.

**Hardware**
- Windows PC: proto-decode via D3D11VA (line 97), D3D11VA hardware decode (line 146), HEIC/HEIF/AVIF through WIC (line 155).
- NVIDIA GPU: proto-gpu on a GTX 1660 class GPU (line 95: `cargo run --release` in `experiments/proto-gpu/`).
- NVIDIA and Intel or AMD Linux: NVDEC and VAAPI decode (line 147).
- NVIDIA, Intel and AMD GPUs on Windows or Linux: NVENC, QSV and AMF encode (line 291).
- Audio devices and a person listening: the master clock check (line 187): `GLASS_AUDIO_DEVICE_TEST=1 cargo test -p audio --test device` on built-in speakers, USB and Bluetooth outputs, and lip sync by eye and ear now that the app plays through the clock.
- An Apple M1: the M2 and then the M5 here stood in for the M1 reference in every measurement; rerun the benchmarks there.
- An unlocked screen: windowed rerun of `experiments/proto-ui` (offscreen numbers are in the Progress Log).

**Signing, identity, licences**
- macOS notarisation (line 472) and Windows code signing (line 474): certificates and an Apple Developer account; MSIX and a notarised DMG need them.
- Choose the bundle id (placeholder `org.glass.Glass`). dist artifacts are named `app-<target>` after the `app` package; consider renaming the package to `glass`.
- Licence the music (line 342) and SFX (line 343) catalogues.

**Design (Paper)**
- The Paper server timed out for every board these tasks needed, so the track headers, the timeline view (ruler and minimap), clip rendering and the app shell took their sizes from theme tokens. Compare them with the 03 Timeline and 02 Edit workspace boards.

**Every needs-human line in PLAN.md**
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
- Line 153: stills, sequences, GIF, WebP, HEIC (blocked only on line 155)
- Line 155: HEIC, HEIF and AVIF on Windows through WIC
- Line 183: master clock (blocked only on line 187)
- Line 187: listening check on real audio hardware
- Line 281: encoder wrapper (blocked only on line 286)
- Line 286: AV1, VP9 and MP3 encoders
- Line 287: hardware encode (blocked only on line 291)
- Line 291: NVENC, QSV and AMF
- Line 342: music pack
- Line 343: SFX pack
- Line 439: v1.0 checklist (certificates)
- Line 472: macOS notarisation
- Line 474: Windows code signing

## 3. Decisions made so far

Full text in `docs/DECISIONS.md`; one line each here, in number order. `docs/DECISIONS.md` lists its sections in number order, and so does the Decision Log in `PLAN.md` from 43 on (its earlier lines stay in merge order and carry no numbers).

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
70. Background export queue: one job at a time, a render thread beside the encoder, a temporary file renamed on success.
71. Multi-pass transitions: extra entry points drawn into targets at declared scales; whip pan and zoom blurred at a reduced resolution.
72. The timeline view: a float position in frames, exact times out, zoom from a frame to the whole sequence, rulers on timecode values.
73. Audio mixer: one graph for the callback and export, control-point gain ramps, graphs swapped through a ring.
74. Canonical project file: pretty JSON, declared field order, sorted maps, exact floats, no schema bump.
75. Animated and nested rendering: looks per frame at the source time, nested sequences as layers with flattened decode segments, per-frame cache keys from what a frame shows.
77. Schema v9: the media pool in the project, track switches with a lock that refuses edits.
78. Clip rendering: a UI-thread analysis store fed by the job pool, tiles anchored on the clip, levels picked per zoom, blocks when zoomed out.
79. The app shell: one UI-thread `AppState`, `egui_dock` workspaces in the config folder, playback on the audio clock or silent.
80. Stylised transitions: procedural noise from a seed, bounded fields swept past their limits, light under the additive cap.
81. Audio crossfades and sample-accurate edges: transitions as fades on the clip side, nested sequences mixed as sources.

Number 76 is unused (it was reserved for the VideoToolbox B-frame fix, which needed no decision). The next free number is 82.

## 4. Open follow-ups

None of these has its own `PLAN.md` line unless a line is named.

**Open question**
- Resolved at the wrap-up: writing the snapshot goldens rewrote goldens no task changed (UI `about`, `params`, `shortcuts_editor`; render `blend_modes`, `title_over_video`, `transform_crop_opacity`, `shapes`, `mask_*`), each off by 1 or 2 levels. The renderer had not drifted: goldens written on the M2 round differently on the M5. They were rebaselined on the M5, the current reference machine, so a golden rewrite now means a real change.

**App wiring** (the app shell exists, Decision 79; these are not connected to it yet)
- `AppState` builds the plan on every edit and calls `set_plan` on the preview's `TimelinePlayback` (Decision 67), but the app starts no `Scrubber` (Decision 53), so there is no scrub path in the app and nothing to restart after an edit.
- `AnalysisFeed` and atlas loading for clip thumbnails and peaks: an `AtlasLoad` carries no `CacheEntry` for `ThumbnailStrips::load_atlas`, so clips show their placeholders in the app (Decision 78).
- The export queue UI: the export dialog submits `ExportJob`s, drains `ExportEvent`s once per frame and turns `Notification`s into OS notifications (Decision 70). An HDR project exported with an SDR preset is clipped (no tone mapping).
- The update pill, the relink folder picker and its progress events, and the shortcuts editor's import and export (line 256).
- Mixer settings persistence (line 400) and a mixer panel (line 401). The inspector and the transitions browser (line 318) do not offer the audio `crossfade` kind and its `curve` yet (Decision 81).
- Shader hot reload is not wired into the app's `Renderer`; track header actions are not shortcut commands; the first showing of a keyboard editor state costs up to 57 ms (text layout).

**Phase 2 (media)**
- Bug: `media::AudioReader` reading a 48 kHz file at 24 kHz delivers the audio one output sample early, from the start and after a seek (output `n` holds time `(n + 1) / 24000`; a 2x clip plays 42 µs early). The fix belongs in the resampler's delay trimming (Decision 81).
- Hardware seeks reopen the VideoToolbox decoder on every keyframe seek, which costs a 4K seek about 15 to 25 ms (Decision 57). Flushing could be kept for IDR keyframes (closed GOPs) once the GOP index records which keyframes are IDR; that is a GOP index format change. A seek inside the current frame now re-decodes from the keyframe; keeping the last frame would avoid that.
- Five fixtures do not regenerate to the same bytes on every run. Find them (run `tests/fixtures/generate.sh --force` twice and compare file hashes) and pin what varies, as `threads=16` did for x264.
- Image sequences probe at FFmpeg's default 25 fps; anything that probes sequences must honour the sequence or user frame rate (`VideoSource` does; `probe` does not).
- Metadata: HEIC/AVIF EXIF through ImageIO; the created-date time zone offset.
- Media pool: a tag editor, drag to the timeline (Phase 6). `crates/ui/src/panels/media_pool.rs` around line 1490 probably clips centred text the way the inspector did before its fix.

**Phase 3 (playback)**
- VideoToolbox through FFmpeg decodes one frame at a time (4K: 110 fps VT vs 478 fps software on synthetic clips, M2). Consider a direct async `VTDecompressionSession` or several decoders; benchmark with real footage.
- Playback must switch to proxies (proxies are video-only; stills and sequences have none), and callers using proxies scale transform, crop, wipe softness and border values. The proxy/full-res toggle is Phase 14.
- Frame cache: re-uploading a 4K frame from RAM costs about 7 ms of CPU, so the scheduler may prefer re-rendering a cheap frame over a RAM hit.

**Phase 4 (compositor)**
- When `render` joins eframe's wgpu device, request `wgpu::Features::TEXTURE_FORMAT_16BIT_NORM` (10-bit P010 surfaces) on the shared device.
- Sources in other primaries go through `GamutNode` into the working primaries (Decision 15); make sure every source path in the compositor inserts it.
- Budgets: 5 blended layers at 4K take 5.46 ms, a 4K title 7.2 ms (a half-resolution shadow blur would help). Building the 50-shape node takes 1.6 to 1.8 ms in release (Decision 46); cache the strips while shapes are unchanged.
- Masks cost a canvas-sized coverage pass per shape whatever its size (Decision 44). `crates/render/tests/mask.rs` has an ignored timing script (`--ignored --nocapture`) with no assertion; convert it to the Decision 64 helper or delete it.
- No mipmaps for strong downscales; `text_glyph.wgsl` is not hot-reloaded; the text animator should lay out at the final size.
- Effect parameters: samples caption, shutter dial and keyframe tooltip still to do; the transform schema's absolute defaults must map to the clip's centre offsets (Decision 45).
- LUT: an inspector row to pick the `.cube` file, log input spaces (Decision 36); the reference project has no LUT effect because there is no `.cube` fixture.

**Phase 5 (project)**
- Separate schema v9 contract tests were skipped at the wrap-up: v9 (Decision 77) has its task's own tests but no separate contract tests, unlike v4 to v8.
- A roll edit must be one edit that moves both clip edges, or the first trim removes the transition (Decision 54).
- Copy and paste (line 233) must carry a compound with its clip across projects; open-in-place for compounds is UI (Decision 69). Adjustment layers (line 230) need item-relative key times (Decision 63). One schema bump at a time.
- Detect a second running instance.

**Phase 6 (timeline UI)**
- 500 clips with thumbnails and waveforms: strict p99 about 6.2 ms with the test's steady tone and about 7.7 ms with varying peaks, inside 8 ms but over the 5 ms aim; what remains is egui tessellation of the zoomed-out clip shapes.
- High-contrast component differences to implement in component code: status pill `bg/2` fill, selected tab accent ring.
- The snapshot mono font (Courier New, until Geist lands) has no `⌘`, so shortcut keys render as `?I` in the goldens.
- The shared `Paint::floating` glass background shows a band at mid-height in tall dialogs.
- Zoom to selection has no key yet; nested sequences show no thumbnails.

**Phase 8 (export)**
- GIF encodes at about 8.5 fps at 1080p; speed it up before GIF export is offered for long ranges.
- Interlaced broadcast delivery (1080i) needs field weaving and a field-coding ProRes encoder (Decision 58).
- WebM subtitles are untested until an AV1 or VP9 encoder is in the FFmpeg build.

**Phase 9 (library)**
- Light leak looks washed out at its peak with the default intensity of 1.0; consider a lower default.
- Zoom out minifies through the pyramid (Decision 71); whip pan and zoom with 16 ms idle gaps still take 5.5 to 6 ms at 4K (GPU clock).

**Phase 12 (audio)**
- `mix_realtime` runs only four 0.1 s crossfades and two fades, because a debug build decodes the clip side at barely real time; heavier crossfade load is untested.
- Speed ramps are silent in the mix; nested sequences ignore their own fader, pan and sends until mixer settings are stored (line 400).

**Phase 14 / 16 (performance)**
- Proxy pipeline decodes, scales and encodes on one thread; splitting decode/scale and encode across threads could lift ProRes VT 4K from about 130 to about 230 fps.
- The strict timing figures swing with the GPU clock (section 1).
- Integration test binaries could be consolidated per crate to cut link time.

**Phase 17 / release**
- Portable mode: the crash monitor's socket should live under `<data>/run`.
- `repository` placeholder, bundle id, `app-<target>` artifact names, OFL.txt in installers (section 2).

## 5. How to resume

**Working unattended.** The owner is not available for questions while the goal runs. Never stop to ask. Decide with PLAN.md's defaults, the Decision Log, and this file, and record any new decision in both logs. When something truly needs a person (a paid account, a certificate, a licence purchase, hardware not on this Mac, the GitHub remote, deleting user data, a download other than crates and the pinned FFmpeg source archives, or copying files from other projects on this machine), tag the task `(needs-human: <reason>)`, add a Progress Log line, and move on to the next task. End every turn with one line: `Open tasks remaining: N`.

**Session loop.** Follow "Working on a task" in `CONTRIBUTING.md` (and PLAN.md's "How to run this plan as a goal"): read PLAN.md and the last 20 Progress Log lines, `df -h /`, `source scripts/dev-env.sh`, run the full check, pick the next task, and use the three-commit rhythm: `plan: start <task>` (mark `[~]`), `<area>: <what changed>`, `plan: done <task>` (mark `[x]`, Progress Log line `- YYYY-MM-DD — Phase N — <task> — <impl hash> — <key numbers>`).

**Parallel worktrees** ("Working in parallel" in `CONTRIBUTING.md`):
- One worktree per task: `git worktree add ../glass-<name> -b task/<name> goal/phase-2`, then `scripts/worktree-setup.sh` inside it (generates its test media, prints the shared `CARGO_TARGET_DIR` and `FFMPEG_DIR`).
- Every shell: `source scripts/dev-env.sh`; every compiling cargo command through `scripts/cargo-serial` (one at a time machine-wide; `cargo fmt` may run directly). All worktrees share `/Users/ryan/Desktop/Glass/target` and the pinned FFmpeg in `.glass-cache/ffmpeg/aarch64-apple-darwin/`; no `cargo clean` while others work.
- Finish checks: run the whole scoped check in one lock hold with `GLASS_CARGO_BIN=$PWD/scripts/finish-check.sh scripts/cargo-serial -p <crate> -p <dependent crate> ...` (workspace build, the listed crates' tests, clippy `--all-targets -D warnings`, smoke; it forgets the workspace fingerprints first and keeps going past a failing step), then `cargo fmt --all -- --check` directly. Write logs into the worktree's own `.glass-cache/`.
- `PLAN.md` and `docs/DECISIONS.md` use git's union merge driver: after every merge run `scripts/check-plan-dups.py PLAN.md` (exits 1 and prints any task line kept twice, such as a `[~]` and an `[x]` copy) and delete the stale copy. Parallel branches each take the next free Decision number, so the later merge renumbers its Decision (heading in `docs/DECISIONS.md` and every "Decision N" reference) and commits that as a docs change. Keep `docs/DECISIONS.md` sections and the `PLAN.md` Decision Log in number order: a union merge appends in merge order, so move a late entry into place.
- Before reporting, a task branch merges `goal/phase-2` in and reruns its finish check; then `task/<name>` merges into `goal/phase-2` with `--no-ff`, `scripts/check-plan-dups.py PLAN.md` and the finish check run on the result, the worktree is removed and the branch deleted. After each batch of merges, run the full `cargo test --workspace` on `goal/phase-2` (the detached `../glass-integration` checkout is there for that: `git checkout --detach <hash>` and run the full check there). Never commit to `main`; merge a phase branch to `main` only when its locally verifiable Done when passes (Decision 12).
- Shared files (`PLAN.md`, `docs/ARCHITECTURE.md`, `assets/i18n/en.json`, `Cargo.toml`, `Cargo.lock`): keep edits small and local; add new modules rather than reorganising.
- Any change to `Cargo.lock`, including a merge that changes it, needs `scripts/licences/third-party.py` rerun and its `assets/licences/third-party.tsv` committed, or the `assets` licence test fails (`scripts/licences/third-party.py --check` tells you without writing).

**Working conventions not spelled out in CONTRIBUTING.md**
- Design lives in the Paper file "Inspiration 2", file id `01M3N8N2AWRZ7XXZY58Q1RH2BT`. Always pass the file id and an explicit page id (the user may be viewing another file or page; never switch their page). Page ids: `p-1-0` 00 Foundations, `p-C-0` 01 First run, `p-B-0` 02 Edit workspace, `p-A-0` 03 Timeline, `p-9-0` 04 Browsers, `p-8-0` 05 Inspector & keyframes, `p-7-0` 06 Captions & assist, `p-6-0` 07 Colour, `p-5-0` 08 Audio, `p-D-0` Brand, `p-4-0` 09 Export, `p-3-0` 10 System & states, `p-2-0` 11 Platform, theme, RTL. Read values with `get_jsx` and `get_computed_styles`, never from screenshots. Do not edit the file unless a task says so. Never put node ids in docs or reports.
- The Mac's screen is usually locked and eframe skips painting occluded windows, so tests, benchmarks and the frame-time monitor run headless/offscreen (`ui::headless`, `experiments/proto-ui/src/offscreen.rs`). Only a check that truly needs a visible window gets a needs-human sub-task.
- Never copy files from other projects on this machine into the repo. The only downloads are crates through cargo and the pinned, checksummed source archives in `scripts/ffmpeg/versions.env` (FFmpeg, x264, x265, dav1d, pkgconf, nasm, meson, ninja). No fonts, models or asset files. Outside the repo only `cargo install` of tools PLAN.md names (`cargo-sweep` 0.8 is installed) and `rustup` components; no `brew install`.
- Commit messages carry no attribution trailers or footers.
- Measurements say that the M5 stands in for the M1 reference and that the machine was shared with other builds (numbers before 2026-10-11 were taken on an M2). A frame-budget or timing test that fails in its default mode is a regression, not load (Decision 64): do not rerun it to get a pass.
- Split tasks: the parent keeps its checkbox and becomes `[x]` when every sub-task is `[x]` or `[-]`; if only needs-human sub-tasks remain it gets `(needs-human: blocked only on the sub-task(s) below)`. Needs-human: do the local part, tag the line `(needs-human: <reason>)`, add a Progress Log line, commit `plan: needs-human <task>`, move on. A task that needs a CI job adds it to `ci.yml`, verifies locally what it can and tags only the CI part.

**Machine guidance.** This Mac is an Apple M5 with 10 cores and 24 GB RAM; about 810 GiB of disk was free on 2026-10-11 (check `df -h /`). Up to about 7 tasks run in parallel, each in its own worktree, sharing one cargo lock: `scripts/cargo-serial` runs one compiling command at a time machine-wide, and the compiler cap is 8 jobs (Decision 43). A wait for the lock can outlast a 10-minute shell timeout, so run cargo-serial commands in the background and wait for them to finish. `scripts/cargo-serial` forgets the workspace fingerprints whenever the building checkout changes (Decision 28), so with many worktrees nearly every cargo command recompiles the workspace crates; batch a task's cargo steps into one lock hold (`scripts/finish-check.sh`) instead of queueing them one by one, and scope commands (`-p <crate>`, `--test <name>`) while iterating. Task worktrees set `CARGO_INCREMENTAL=0`. Never `cargo clean`, `cargo sweep` or delete anything under `target/` while others work. Never delete source or anything outside the repo.

## 6. Suggested next work, in order

1. **Bring `main` up to date.** Act on the final review of `9af7eca`, run the full check on `goal/phase-2` in `../glass-integration`, then merge `goal/phase-2` into `main` again (Decision 12).
2. **App wiring** (section 4): the analysis feed and atlas loads for clips, the scrubber, the export queue UI, the update pill, the relink folder picker, shortcuts import and export (line 256).
3. **Phase 3 remaining** (lines 194 to 198): JKL shuttle, reverse playback and audio scrubbing; loop range, in and out points and play-around; background render of idle regions; clean feed; benchmarks (scrub latency, frame drops, cache hit rate).
4. **Phase 5 remaining:** adjustment layers (line 230, a schema bump), then copy and paste (line 233, carrying compounds). One schema bump at a time; the mixer settings (line 400) need one too.
5. **Phase 6** (lines 244 to 261): playhead, in and out and markers; snapping; selection; magnetic mode; tools and trims; drag from bin; linked audio and video; storyboard; the rest of the shortcuts sheet; command palette; RTL and full i18n; debug panel.
6. **Phase 8 remaining:** custom preset editor (line 293), smart export (line 295), export ranges (line 296); then the v0.1 checklist (line 302).
7. **Phase 9:** shape and 3D transitions, morph cut, the transition browser, default transition, easing editor (lines 315 to 320), then titles, looks and effects, sound and motion presets (lines 323 to 351).
8. **Phase 12:** mixer settings in the project and the mixer panel (lines 400 and 401), then effects, loudness normalisation and the rest (lines 402 to 410).
9. Then Phase 7 (lines 267 to 275), Phases 10, 11 and 13 to 15, the Phase 16 stress project and benchmarks (lines 457 to 465), and Phases 17 and 18.

**What can run in parallel** (up to 7 parallel tasks, one cargo lock):
- Engine track (`engine`): Phase 3 remaining tasks.
- Project track (`project`): Phase 5 remaining and the mixer settings, strictly one schema bump at a time.
- UI and app track (`ui`, `app`): app wiring and Phase 6 panels; each new panel gets a snapshot case and a frame-budget test.
- Render track (`render`, `assets/transitions/`): Phase 9 transitions.
- Export track (`media::export`, `engine::export`): Phase 8 remaining.
- Audio track (`audio`, `engine::mix`): Phase 12 after the mixer settings land; the `AudioReader` sample offset fix in `media`.
