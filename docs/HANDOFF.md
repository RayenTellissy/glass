# Handoff: resuming the goal

State of the work on 2026-10-11, when the session wrapped up with `goal/phase-2` at the app shell merge (`9af7eca`), and how to pick it up in a fresh session. `PLAN.md` stays the single source of truth; this file points into it. Line numbers below are `PLAN.md` lines on `goal/phase-2` after this refresh, which follows the 1.0 scope cut (Decision 82): `PLAN.md`'s "Road to 1.0" (line 67) is now the finish line, a simple editor for macOS on Apple silicon and Windows 10 and 11, and nothing on it needs a person except the GitHub repository (section 2).

## 1. Where things are

| Branch | Contents |
| --- | --- |
| `main` | Phases 0, 1 and 2 merged: `416f085` merges `goal/phase-2` as reviewed at `311f0ee` (the reviewer's full workspace test passed 1101 tests). Everything merged into `goal/phase-2` after `311f0ee` is not on `main` yet. |
| `goal/phase-2` | Phase 2 (open for 1.0: the WebP still fixture, the `AudioReader` fix, media pool text clipping, relinking in the app), Phase 4 (complete), most of Phases 3 and 5, the first Phase 6 panels and the app shell, most of Phase 8, the first Phase 9 transition families, the Phase 12 mixer core and crossfades, and parts of Phases 16 and 17 (below). The integration branch: new task branches start here. Project schema v9, autosave `FORMAT_VERSION` 9. |

**Nothing is in flight.** No task branches or task worktrees remain; every task branch merged into `goal/phase-2` and was deleted. The detached `../glass-integration` checkout used for full checks on the integration branch was removed at the wrap-up; recreate it with `git worktree add --detach ../glass-integration goal/phase-2` when a full check is due. The final review of `0715c95` passed everything except the strict timing step (see section 4); `main` holds that merge.

Phase 2's Done when asks that every fixture decodes to GPU textures and f32 audio through the same API, with hardware decode on both platforms, and that `smoke` plays the reference project. On this Mac every fixture the pinned FFmpeg can generate decodes to textures (`crates/engine/tests/fixture_textures.rs`, 31 picture fixtures), and `smoke` opens `tests/fixtures/reference.proj`, plays 2 s through `engine::timeline::TimelinePlayback`, exports 1 s with the mixed audio through the export path and verifies the file (Decision 66). For 1.0 the still WebP fixture comes from the `image` crate (line 174, R1), the five other fixtures that need extra encoders are dropped, and Windows decodes in software, checked by the CI Windows job once the repository exists (Decision 82).

**Done on `goal/phase-2`.**

- Phase 2: safe FFmpeg wrapper, probe, software decode (fixtures needing extra encoders excepted, see section 2), VideoToolbox decode, software fallback, colour space plumbing, VFR normalisation, accurate seek (hardware decoders reopened on every keyframe seek, Decision 57; a seek inside the frame handed out last now returns that frame), stills and sequences (macOS part), audio decode and resampling, analysis jobs, thumbnail strips, waveform peaks, proxies, scene cuts, silence, beats and tempo, loudness, import, media pool, relinking, offline slate, metadata panel; texture coverage of every generated fixture and recipe-hash regeneration of stale fixtures in `tests/fixtures/generate.sh`.
- Phase 3: thread layout, lock-free frame queues (Decision 56), master clock (a device check without a listener open for 1.0, line 220), rendered-frame cache, look-ahead decode (Decision 49), dirty-range invalidation (Decision 67), animated and nested rendering (Decision 75), adaptive quality (Decision 52), scrub mode (Decision 53), the timeline playback pipeline (`engine::timeline`, Decision 60).
- Phase 4: complete. Render graph, linear-light working space and dithered output, blend modes, transform, opacity and crop, shader hot reload, effect parameters and their auto-UI, text, vector shapes within the 4K budget (Decision 46), the mask node (Decision 44), readback ring, visual regression suite.
- Phase 5: edit tree, undo and redo, autosave (Decision 55), versioned schema with migrations up to v9, sequences, tracks, clips, gaps and markers, clip properties (v4, Decision 45), transitions (v5, Decision 54), project settings (v6, Decision 55), keyframe curves (v7, Decision 63), nested sequences and compound clips (v8, Decision 69), the media pool and track switches in the project (v9, Decision 77), the reference project (Decision 66), the canonical pretty JSON project file (Decision 74). Open for 1.0: copy and paste (line 267, R3); adjustment layers are dropped for 1.0.
- Phase 6: track headers (Decision 68, switches stored since v9), clip rendering within the frame budget (Decision 78), zoom and scroll with the minimap (Decision 72), dockable panels, saveable workspaces and presets (the app shell, Decision 79), remappable shortcuts: the command registry and presets, the keyboard editor, and `keymap.json` loaded, saved and dispatched by the app.
- Phase 8: encoder wrapper (AV1, VP9 and MP3 dropped for 1.0, Decisions 47 and 82), hardware encode with VideoToolbox and fallback (NVENC, QSV and AMF wired behind `cfg!` but dropped for 1.0, Decisions 51 and 82) and the VideoToolbox B-frame fix, presets (Decision 58), background export queue (Decision 70), subtitle export (Decision 65), output verification (Decision 59).
- Phase 9: LUT framework, transition framework (Decision 48), dissolves (Decision 50), wipes (Decision 61), motion (Decision 62), multi-pass transitions (Decision 71), stylised (Decision 80).
- Phase 12: mixer core and meters, buses and sends, automation lanes, offline render, the mix in the export queue and `smoke` (Decision 73); sample-accurate editing and crossfades (Decision 81).
- Phase 16: UI snapshot tests; timing tests robust under load (Decision 64).
- Phase 17: portable mode, About dialog (with its layout cache).
- Tooling: `scripts/finish-check.sh`, `scripts/check-plan-dups.py`, the `cargo-serial` fingerprint fix (Decision 28), `scripts/dev-env.sh` finding `~/.cargo/bin` in non-login shells, compiler cap of 8 jobs (Decision 43).

### Counting open tasks

Open tasks are lines matching `- [ ]` or `- [~]` (sub-tasks included) that do not contain `needs-human`; dropped lines are `- [-]`. Every open line belongs to 1.0 and ends with its milestone tag `(R1)` to `(R7)`. For example:

```sh
grep -E '^\s*- \[( |~)\]' PLAN.md | grep -vc needs-human   # open
grep -cE '^\s*- \[ \].*\(R3\)' PLAN.md                    # open in R3
```

Counts on `goal/phase-2` after the scope cut, with the counts at `9af7eca` before it in brackets (sub-tasks counted as tasks; parents whose sub-tasks are all `[x]` or `[-]` count as done). No line is `[~]`.

| Phase | Done | Open | Needs-human | Dropped |
| --- | --- | --- | --- | --- |
| 0 Decisions and prototypes | 7 (7) | 0 (0) | 0 (2) | 2 (0) |
| 1 Skeleton and CI | 22 (21) | 7 (0) | 1 (7) | 1 (0) |
| 2 Media engine | 27 (26) | 5 (0) | 0 (6) | 3 (0) |
| 3 Playback engine | 11 (11) | 6 (5) | 0 (2) | 2 (0) |
| 4 GPU compositor | 13 (13) | 0 (0) | 0 (0) | 0 (0) |
| 5 Project model | 12 (12) | 1 (2) | 0 (0) | 1 (0) |
| 6 Timeline UI | 7 (7) | 11 (14) | 0 (0) | 4 (0) |
| 7 Core editing | 0 (0) | 7 (9) | 0 (0) | 3 (0) |
| 8 Export | 16 (14) | 4 (4) | 0 (4) | 4 (0) |
| 9 Built-in library | 7 (7) | 14 (26) | 0 (2) | 14 (0) |
| 10 Easy mode and assist | 0 (0) | 1 (15) | 0 (0) | 14 (0) |
| 11 Colour | 0 (0) | 3 (10) | 0 (0) | 7 (0) |
| 12 Audio | 6 (6) | 6 (11) | 0 (0) | 5 (0) |
| 13 Advanced compositing | 0 (0) | 0 (10) | 0 (0) | 10 (0) |
| 14 Multicam and workflow | 0 (0) | 1 (8) | 0 (1) | 8 (0) |
| 15 Plugins and scripting | 0 (0) | 0 (7) | 0 (0) | 7 (0) |
| 16 Performance and quality | 2 (2) | 3 (7) | 0 (0) | 5 (0) |
| 17 Distribution | 2 (2) | 2 (6) | 0 (2) | 7 (1) |
| 18 Documentation | 0 (0) | 6 (6) | 0 (0) | 0 (0) |
| **Total** | **132 (128)** | **77 (140)** | **1 (26)** | **97 (1)** |

By milestone: R1 8, R2 9, R3 14, R4 21, R5 4, R6 10, R7 11 open plus the owner's line. Six of the open lines wait on the owner's repository line (140, 142, 146, 147, 150, 512); the 1.0 checklist (line 477) needs them too. Twelve open lines are new: the owner's line and eleven follow-ups from section 4 or the scope cut (lines 162, 163, 187, 207, 208, 227, 277, 302, 328, 332, 502).

### Last full check

The last full workspace test was the final review's on `0715c95`, before the merge to `main`: 1241 passed, 0 failed, 7 ignored; clippy, fmt, snapshots (354 UI images, 118 render frames), `ui_source`, smoke, licences and `cargo deny` all passed. Strict timing fails on this M5 for three 4K GPU medians (cross dissolve, film dissolve, linear wipe: about 2.5 ms against 2.0 ms), exactly as at `311f0ee`; it needs a look before the CI strict step is switched on. Run the full check on `goal/phase-2` (in a recreated `../glass-integration`, see above) before the next merge to `main`.

- Timing tests follow Decision 64: a default-mode failure is a regression, not load. The strict figures (`GLASS_STRICT_TIMING=1`) swing with the GPU clock: a quiet 4K cross dissolve pass takes 1.3 or 2.2 ms.

## 2. Needs a person

**The one step on the road to 1.0** (PLAN.md line 139, the only `needs-human` line): the owner says yes to a public GitHub repository named `glass` under their account, holding this code. Public because Glass is free and open source (Decision 2) and public repositories get GitHub's build machines without a monthly limit; a private one would run out of free macOS and Windows minutes quickly. This Mac's `gh` tool is already signed in to the owner's account, so after the yes the person running the plan (or the owner, in Terminal) runs:

```sh
cd ~/Desktop/Glass
gh repo create glass --public --source . --remote origin
git push -u origin main goal/phase-2
```

What it unlocks, with nothing more from the owner:
- The Windows build, the Windows tests and the Windows smoke run, on GitHub's Windows machines (`windows-2022` in `.github/workflows/ci.yml`, line 140), and the same checks on a clean Mac.
- The pinned FFmpeg built for Windows (line 146).
- The tagged release with the macOS DMG and the unsigned Windows MSI and portable zip (lines 147, 150 and 512); then set `repository` in `Cargo.toml` (placeholder `https://github.com/OWNER/glass`) and run `dist generate` (line 150).

Until then Windows is unverified: everything Windows-specific is written so it can be checked by that job, and a local `cargo check --target x86_64-pc-windows-msvc` is tried (line 163) only if crates and a rustup target are all it needs.

**Optional later, needs a person** (all dropped for 1.0 in `PLAN.md`, Decision 82; none blocks 1.0):
- Hardware: an NVIDIA GPU for `proto-gpu` (line 122) and NVENC; a Windows PC for D3D11VA proto-decode and decode (lines 124 and 176) and HEIC, HEIF and AVIF through WIC (line 185); NVIDIA and Intel or AMD on Linux for NVDEC and VAAPI (line 177); NVIDIA, Intel and AMD GPUs for NVENC, QSV and AMF, already wired behind `cfg!` (line 327); USB and Bluetooth outputs and a person listening for lip sync (line 220 keeps the automated part); an Apple M1 to rerun the benchmarks on the reference machine; an unlocked screen for a windowed `experiments/proto-ui` run.
- Accounts and certificates: macOS notarisation with an Apple Developer account (line 511), Windows code signing (line 513), an auto-update server (line 515).
- Licences and downloads: music and SFX catalogues (lines 380 and 381), speech and segmentation models (Phase 10), Geist fonts (`assets/fonts/README.md`; adding them means regenerating every UI snapshot golden), FFmpeg encoders for AV1, VP9 and MP3 and libwebp's tools (lines 174 and 322: new pinned archives in `scripts/ffmpeg/versions.env` and a rebuild).
- Design: a Paper pass over the track headers, timeline view, clip rendering and app shell (the Paper server timed out for every board; sizes came from theme tokens). Not a gate for 1.0.
- CI extras once the repository exists: the macOS x86_64 and Linux rows, the bench job posting PR comments (line 155), fuzzing (line 499).

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
82. Scope cut for 1.0: macOS and Windows, no owner-supplied resources except the GitHub repository.

Number 76 is unused (it was reserved for the VideoToolbox B-frame fix, which needed no decision). The next free number is 83.

## 4. Open follow-ups

None of these has its own `PLAN.md` line unless a line is named. The ones 1.0 needs have lines tagged with their milestone since Decision 82; the rest wait until after 1.0.

**Open question**
- Resolved at the wrap-up: writing the snapshot goldens rewrote goldens no task changed (UI `about`, `params`, `shortcuts_editor`; render `blend_modes`, `title_over_video`, `transform_crop_opacity`, `shapes`, `mask_*`), each off by 1 or 2 levels. Most were written on the M2 and round differently on the M5; the `mask_*` and `shapes` goldens had also not been refreshed after the sparse-strips change (Decision 46). All were rebaselined on the M5, the machine the goldens are now written on, so a golden rewrite now means a real change.

**App wiring** (the app shell exists, Decision 79; these are not connected to it yet)
- `AppState` builds the plan on every edit and calls `set_plan` on the preview's `TimelinePlayback` (Decision 67), but the app starts no `Scrubber` (Decision 53), so there is no scrub path in the app and nothing to restart after an edit. Line 227 (R2).
- `AnalysisFeed` and atlas loading for clip thumbnails and peaks: an `AtlasLoad` carries no `CacheEntry` for `ThumbnailStrips::load_atlas`, so clips show their placeholders in the app (Decision 78). Line 277 (R2).
- The export queue UI: the export dialog submits `ExportJob`s, drains `ExportEvent`s once per frame and turns `Notification`s into OS notifications (Decision 70); line 332 (R5) uses in-app toasts instead. An HDR project exported with an SDR preset is clipped (no tone mapping); for 1.0, HDR sources in SDR projects are tone-mapped (line 424, R4) and HDR projects stay as built.
- The relink folder picker and its progress events (line 208, R2). The update pill and the shortcuts editor's import and export are dropped for 1.0 (no auto-updater; line 291).
- Mixer settings persistence (line 438) and a mixer panel (line 439), both R2. The inspector (line 302) and the transitions browser (line 356) do not offer the audio `crossfade` kind and its `curve` yet (Decision 81).
- Shader hot reload is not wired into the app's `Renderer`; track header actions are not shortcut commands; the first showing of a keyboard editor state costs up to 57 ms (text layout).

**Phase 2 (media)**
- Bug: `media::AudioReader` reading a 48 kHz file at 24 kHz delivers the audio one output sample early, from the start and after a seek (output `n` holds time `(n + 1) / 24000`; a 2x clip plays 42 µs early). The fix belongs in the resampler's delay trimming (Decision 81). Line 187 (R1).
- Hardware seeks reopen the VideoToolbox decoder on every keyframe seek, which costs a 4K seek about 15 to 25 ms (Decision 57). Flushing could be kept for IDR keyframes (closed GOPs) once the GOP index records which keyframes are IDR; that is a GOP index format change. A seek inside the current frame now re-decodes from the keyframe; keeping the last frame would avoid that.
- Five fixtures do not regenerate to the same bytes on every run. Find them (run `tests/fixtures/generate.sh --force` twice and compare file hashes) and pin what varies, as `threads=16` did for x264.
- Image sequences probe at FFmpeg's default 25 fps; anything that probes sequences must honour the sequence or user frame rate (`VideoSource` does; `probe` does not).
- Metadata: HEIC/AVIF EXIF through ImageIO; the created-date time zone offset.
- Media pool: a tag editor (after 1.0), drag to the timeline (line 285, R3). `crates/ui/src/panels/media_pool.rs` around line 1490 probably clips centred text the way the inspector did before its fix (line 207, R1).

**Phase 3 (playback)**
- VideoToolbox through FFmpeg decodes one frame at a time (4K: 110 fps VT vs 478 fps software on synthetic clips, M2). Consider a direct async `VTDecompressionSession` or several decoders; benchmark with real footage.
- Playback must switch to proxies (proxies are video-only; stills and sequences have none), and callers using proxies scale transform, crop, wipe softness and border values. The proxy/full-res toggle is dropped for 1.0.
- Frame cache: re-uploading a 4K frame from RAM costs about 7 ms of CPU, so the scheduler may prefer re-rendering a cheap frame over a RAM hit.

**Phase 4 (compositor)**
- When `render` joins eframe's wgpu device, request `wgpu::Features::TEXTURE_FORMAT_16BIT_NORM` (10-bit P010 surfaces) on the shared device.
- Sources in other primaries go through `GamutNode` into the working primaries (Decision 15); make sure every source path in the compositor inserts it.
- Budgets: 5 blended layers at 4K take 5.46 ms, a 4K title 7.2 ms (a half-resolution shadow blur would help). Building the 50-shape node takes 1.6 to 1.8 ms in release (Decision 46); cache the strips while shapes are unchanged.
- Masks cost a canvas-sized coverage pass per shape whatever its size (Decision 44). `crates/render/tests/mask.rs` has an ignored timing script (`--ignored --nocapture`) with no assertion; convert it to the Decision 64 helper or delete it.
- No mipmaps for strong downscales; `text_glyph.wgsl` is not hot-reloaded; the text animator should lay out at the final size.
- Effect parameters: samples caption, shutter dial and keyframe tooltip still to do; the transform schema's absolute defaults must map to the clip's centre offsets (Decision 45).
- LUT: an inspector row to pick the `.cube` file (line 423, R4), log input spaces (Decision 36); the reference project has no LUT effect because there is no `.cube` fixture (the built-in looks script, line 372, makes some).

**Phase 5 (project)**
- Separate schema v9 contract tests were skipped at the wrap-up: v9 (Decision 77) has its task's own tests but no separate contract tests, unlike v4 to v8.
- A roll edit must be one edit that moves both clip edges, or the first trim removes the transition (Decision 54).
- Copy and paste (line 267, R3) must carry a compound with its clip; open-in-place for compounds is UI (Decision 69) and dropped for 1.0. Adjustment layers (dropped for 1.0) need item-relative key times (Decision 63). One schema bump at a time: mixer settings (line 438), linked audio and video (line 286), text and background clips (lines 306 and 366).
- Detect a second running instance.

**Phase 6 (timeline UI)**
- 500 clips with thumbnails and waveforms: strict p99 about 6.2 ms with the test's steady tone and about 7.7 ms with varying peaks, inside 8 ms but over the 5 ms aim; what remains is egui tessellation of the zoomed-out clip shapes.
- High-contrast component differences to implement in component code: status pill `bg/2` fill, selected tab accent ring.
- The snapshot mono font (Courier New; Geist is not coming for 1.0, Decision 82) has no `⌘`, so shortcut keys render as `?I` in the goldens; the cheat sheet line (line 528) picks a system font that has them.
- The shared `Paint::floating` glass background shows a band at mid-height in tall dialogs.
- Zoom to selection has no key yet; nested sequences show no thumbnails.

**Phase 8 (export)**
- GIF encodes at about 8.5 fps at 1080p; speed it up before GIF export is offered for long ranges.
- Interlaced broadcast delivery (1080i) needs field weaving and a field-coding ProRes encoder (Decision 58).
- WebM subtitles stay untested (no AV1 or VP9 encoder in the pinned build; dropped for 1.0).

**Phase 9 (library)**
- Light leak looks washed out at its peak with the default intensity of 1.0; consider a lower default.
- Zoom out minifies through the pyramid (Decision 71); whip pan and zoom with 16 ms idle gaps still take 5.5 to 6 ms at 4K (GPU clock).

**Phase 12 (audio)**
- `mix_realtime` runs only four 0.1 s crossfades and two fades, because a debug build decodes the clip side at barely real time; heavier crossfade load is untested.
- Speed ramps are silent in the mix (ramps are dropped for 1.0); nested sequences ignore their own fader, pan and sends until mixer settings are stored (line 438).

**Phase 14 / 16 (performance)**
- Proxy pipeline decodes, scales and encodes on one thread; splitting decode/scale and encode across threads could lift ProRes VT 4K from about 130 to about 230 fps.
- The strict timing figures swing with the GPU clock (section 1); line 502 (R1) sets the strict 4K budget from this Mac's measurements.
- Integration test binaries could be consolidated per crate to cut link time.

**Phase 17 / release**
- Portable mode: the crash monitor's socket should live under `<data>/run`.
- `repository` placeholder (set by line 150 after the owner's step). Decided for 1.0 (Decision 82): the bundle id stays `org.glass.Glass`, the `app-<target>` artifact names stay, and there is no `OFL.txt` because no fonts are bundled.

## 5. How to resume

**Working unattended.** The owner is not available for questions while the goal runs. Never stop to ask. Decide with PLAN.md's defaults, the Decision Log, and this file, and record any new decision in both logs. Since Decision 82, a task that would need a person (a paid account, a certificate, a licence purchase, hardware not on this Mac, a download other than crates and the pinned FFmpeg source archives, or copying files from other projects on this machine) is simplified to what this Mac can do or dropped (`[-]`, "dropped for 1.0: <reason>"), recorded in a Progress Log line, and the work moves on; the only `needs-human` line is the owner's repository (section 2), and lines that wait on it say so. Deleting user data still needs a person. End every turn with one line: `Open tasks remaining: N`.

**Session loop.** Follow "Working on a task" in `CONTRIBUTING.md` (and PLAN.md's "How to run this plan as a goal"): read PLAN.md and the last 20 Progress Log lines, `df -h /`, `source scripts/dev-env.sh`, run the full check, pick the next task, and use the three-commit rhythm: `plan: start <task>` (mark `[~]`), `<area>: <what changed>`, `plan: done <task>` (mark `[x]`, Progress Log line `- YYYY-MM-DD — Phase N — <task> — <impl hash> — <key numbers>`).

**Parallel worktrees** ("Working in parallel" in `CONTRIBUTING.md`):
- One worktree per task: `git worktree add ../glass-<name> -b task/<name> goal/phase-2`, then `scripts/worktree-setup.sh` inside it (generates its test media, prints the shared `CARGO_TARGET_DIR` and `FFMPEG_DIR`).
- Every shell: `source scripts/dev-env.sh`; every compiling cargo command through `scripts/cargo-serial` (one at a time machine-wide; `cargo fmt` may run directly). All worktrees share `/Users/ryan/Desktop/Glass/target` and the pinned FFmpeg in `.glass-cache/ffmpeg/aarch64-apple-darwin/`; no `cargo clean` while others work.
- Finish checks: run the whole scoped check in one lock hold with `GLASS_CARGO_BIN=$PWD/scripts/finish-check.sh scripts/cargo-serial -p <crate> -p <dependent crate> ...` (workspace build, the listed crates' tests, clippy `--all-targets -D warnings`, smoke; it forgets the workspace fingerprints first and keeps going past a failing step), then `cargo fmt --all -- --check` directly. Write logs into the worktree's own `.glass-cache/`.
- `PLAN.md` and `docs/DECISIONS.md` use git's union merge driver: after every merge run `scripts/check-plan-dups.py PLAN.md` (exits 1 and prints any task line kept twice, such as a `[~]` and an `[x]` copy) and delete the stale copy. Parallel branches each take the next free Decision number, so the later merge renumbers its Decision (heading in `docs/DECISIONS.md` and every "Decision N" reference) and commits that as a docs change. Keep `docs/DECISIONS.md` sections and the `PLAN.md` Decision Log in number order: a union merge appends in merge order, so move a late entry into place.
- Before reporting, a task branch merges `goal/phase-2` in and reruns its finish check; then `task/<name>` merges into `goal/phase-2` with `--no-ff`, `scripts/check-plan-dups.py PLAN.md` and the finish check run on the result, the worktree is removed and the branch deleted. After each batch of merges, run the full `cargo test --workspace` on `goal/phase-2` (recreate the detached `../glass-integration` checkout as in section 1, run the full check there, and remove it afterwards). Never commit to `main`; merge a phase branch to `main` only when its locally verifiable Done when passes (Decision 12).
- Shared files (`PLAN.md`, `docs/ARCHITECTURE.md`, `assets/i18n/en.json`, `Cargo.toml`, `Cargo.lock`): keep edits small and local; add new modules rather than reorganising.
- Any change to `Cargo.lock`, including a merge that changes it, needs `scripts/licences/third-party.py` rerun and its `assets/licences/third-party.tsv` committed, or the `assets` licence test fails (`scripts/licences/third-party.py --check` tells you without writing).

**Working conventions not spelled out in CONTRIBUTING.md**
- Design lives in the Paper file "Inspiration 2", file id `01M3N8N2AWRZ7XXZY58Q1RH2BT`. Always pass the file id and an explicit page id (the user may be viewing another file or page; never switch their page). Page ids: `p-1-0` 00 Foundations, `p-C-0` 01 First run, `p-B-0` 02 Edit workspace, `p-A-0` 03 Timeline, `p-9-0` 04 Browsers, `p-8-0` 05 Inspector & keyframes, `p-7-0` 06 Captions & assist, `p-6-0` 07 Colour, `p-5-0` 08 Audio, `p-D-0` Brand, `p-4-0` 09 Export, `p-3-0` 10 System & states, `p-2-0` 11 Platform, theme, RTL. Read values with `get_jsx` and `get_computed_styles`, never from screenshots. Do not edit the file unless a task says so. Never put node ids in docs or reports. Since Decision 82 the boards are a reference, not a gate: theme tokens and the snapshot goldens decide, and a timed-out Paper server is not a reason to stop.
- The Mac's screen is usually locked and eframe skips painting occluded windows, so tests, benchmarks and the frame-time monitor run headless/offscreen (`ui::headless`, `experiments/proto-ui/src/offscreen.rs`). A check that truly needs a visible window is dropped for 1.0 (Decision 82).
- Never copy files from other projects on this machine into the repo. The only downloads are crates through cargo and the pinned, checksummed source archives in `scripts/ffmpeg/versions.env` (FFmpeg, x264, x265, dav1d, pkgconf, nasm, meson, ninja). No fonts, models or asset files. Outside the repo only `cargo install` of tools PLAN.md names (`cargo-sweep` 0.8 is installed) and `rustup` components; no `brew install`.
- Commit messages carry no attribution trailers or footers.
- Measurements say that the M5 stands in for the M1 reference and that the machine was shared with other builds (numbers before 2026-10-11 were taken on an M2). A frame-budget or timing test that fails in its default mode is a regression, not load (Decision 64): do not rerun it to get a pass.
- Split tasks: the parent keeps its checkbox and becomes `[x]` when every sub-task is `[x]` or `[-]`; a parent whose open sub-task waits on something says "open until the sub-task below is done". Something that would need a person: do the local part, simplify or drop the rest (`[-]`, "dropped for 1.0: <reason>"), add a Progress Log line, commit `plan: drop <task>`, move on (Decision 82). A task that needs a CI job adds it to `ci.yml`, verifies locally what it can and notes that the CI part waits on the owner's repository line (line 139); it is not tagged needs-human.

**Machine guidance.** This Mac is an Apple M5 with 10 cores and 24 GB RAM; about 810 GiB of disk was free on 2026-10-11 (check `df -h /`). Up to about 7 tasks run in parallel, each in its own worktree, sharing one cargo lock: `scripts/cargo-serial` runs one compiling command at a time machine-wide, and the compiler cap is 8 jobs (Decision 43). A wait for the lock can outlast a 10-minute shell timeout, so run cargo-serial commands in the background and wait for them to finish. `scripts/cargo-serial` forgets the workspace fingerprints whenever the building checkout changes (Decision 28), so with many worktrees nearly every cargo command recompiles the workspace crates; batch a task's cargo steps into one lock hold (`scripts/finish-check.sh`) instead of queueing them one by one, and scope commands (`-p <crate>`, `--test <name>`) while iterating. Task worktrees set `CARGO_INCREMENTAL=0`. Never `cargo clean`, `cargo sweep` or delete anything under `target/` while others work. Never delete source or anything outside the repo.

## 6. Suggested next work, in order

Follow `PLAN.md`'s "Road to 1.0" (line 67); `grep -n '(R1)' PLAN.md` and so on list each milestone's lines.

1. **Ask the owner for the one yes** (section 2) at the first chance, and push. It only unlocks Windows and release checks, so nothing waits for it.
2. **R1 Gate and fixes** first: `scripts/full-check.sh` (line 162), the Windows compile check (line 163), the strict 4K GPU budget (line 502), the `AudioReader` fix (line 187), media pool text clipping (line 207), the WebP still fixture (line 174), software encoding by default off macOS (line 328). All independent, so they run in parallel.
3. **Then in parallel tracks** (up to 7 tasks, one cargo lock):
   - App and UI wiring, R2: thumbnails and waveforms (line 277), scrubbing (line 227), the inspector (line 302), relinking (line 208), the device check (line 220).
   - Project, one schema bump at a time: mixer settings (line 438, R2), then linked audio and video (line 286, R3), then text and background clips (lines 306 and 366, R4); copy and paste (line 267) needs no bump.
   - Timeline UI, R3: playhead, in and out and markers (line 279), snapping, selection, ripple, tools, trim edge frame, drag from the pool (lines 280 to 285), transport keys and loop (lines 228 and 229, engine), speed, freeze frame and transform handles (lines 303 to 305).
   - Render and assets, R4: looks, colour adjustment, effects, HDR tone map, title presets and animations (lines 361 to 376, 418 to 424), then the browsers (lines 356 and 376).
   - Export, R5: the export dialog and queue (line 332), then settings, ranges and audio-only presets (lines 330, 334, 448).
   - Audio, R2 and R4: the mixer panel (line 439) after the settings, fades and volume line, loudness (lines 307, 442, 444).
   - First run, R6: lines 410, 503 and 526 to 531, and the keyboard sheet rest (line 291).
4. **R7 Release** last: the scripted integration test (line 311), local benchmarks (line 232), the stress project (line 495), the DMG (line 510), the CI and release lines once the repository exists (lines 140 to 150, 512), and the Glass 1.0 checklist (line 477).
5. Before each merge to `main`, run the full check on `goal/phase-2` in a recreated `../glass-integration` (section 1, Decision 12).
