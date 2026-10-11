# Video Editor in Rust — Goal Plan

Goal: Glass, a free and open-source native video editor for Windows and macOS (Linux as a bonus) that opens instantly, never stutters during playback or scrubbing, ships with a full library of transitions, titles, looks and sounds, and scales from beginner to pro.

**Who it's for.** Creators making videos for YouTube and social platforms come first. When their needs conflict with a pro's (defaults, what the app opens to, what shows without digging), the creator wins. Pro tools (colour, audio, compositing, multicam) are how the app grows with them, and they never make the beginner path harder.

**Why people pick it.** It feels faster than any editor they've used, it's a small download, and it's free. Speed is the promise: instant start, instant scrub, and a UI that never waits. Size matters, but never at the cost of a feature or a speed optimisation.

**When goals conflict:** speed, then features, then size.

**Success looks like:**

- A first-time user takes raw clips to an exported vertical video with captions and music in under 10 clicks
- A pro cuts, grades, mixes, and delivers a 4K project without leaving the app
- The stress project plays at full rate on the reference hardware and every speed budget holds

**Licence.** Source code under MIT. Release builds are distributed under GPLv3 because they bundle FFmpeg with x264 and x265. No activation, no paid tiers, no telemetry.

**Design.** Every moment of the app has a board in the Paper file "Inspiration 2", made from `DESIGN_HANDOFF.md`. The UI is built from those boards and their tokens, so the whole app looks and behaves as one.

**Brand.** The app is called Glass. Write "Glass" in sentences; the wordmark is always lowercase. The mark, lockups, colours, type, app icon, and their rules live on the Brand page of the Paper file, and every use of the name or mark in the app, installers, and docs follows that page. No splash screen: the app opens too fast to need one.

This file is the single source of truth for the goal. It lives at the repo root as `PLAN.md`. The agent running the goal reads it at the start of every session, works the next unchecked task, and updates it before stopping.

---

## How to run this plan as a goal

**Session loop.** Every session does exactly this:

1. Read `PLAN.md` and the last 20 lines of the Progress Log at the bottom.
2. Run `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings`. If anything fails, fixing it is the first task.
3. Pick the first unchecked `[ ]` task in phase order. Never skip ahead to a later phase while an earlier phase has open tasks, except tasks tagged `(parallel)`.
4. Mark it `[~]`, commit `plan: start <task>`.
5. Implement it. Meet its "Done when" line or the phase's Done when. Add or update a test or benchmark.
6. Run the full check from step 2 plus `cargo run --bin smoke` (the smoke binary, created in Phase 1, opens a reference project, plays 2 seconds, exports 1 second, and exits 0).
7. Mark it `[x]`, append one line to the Progress Log, commit `<area>: <what changed>`.
8. Repeat until out of budget. Never stop with a task at `[~]` and a broken build.

**Concurrency limits (hard, the machine is modest).** Never run more than **10 agents or subagents at once**, including the coordinator. Prefer 3 to 5. Never run more than **one `cargo build` or `cargo test` at a time** across all agents; the rest wait. Cap the compiler with `CARGO_BUILD_JOBS=8` and set `[build] jobs = 8` in `.cargo/config.toml`. Run benchmarks and the stress project serially, alone, with no other agents active. If the machine swaps or a build takes over 3x its usual time, drop to a single agent for the rest of the session. Tasks tagged `(parallel)` are the only ones that may run alongside the current phase task, and at most 2 of them at once.

**Disk space (hard, the disk is small).** About 55 GB is free when the goal starts. Check free space with `df -h /` at the start of every session. Local builds target only this Mac's architecture (arm64): Intel and universal builds, fuzzing, and Linux and Windows builds run in CI only. Build release locally only to run benchmarks or the stress project. Download only the smallest speech and segmentation models locally. Below 15 GB free, run `cargo sweep --time 7` (install with `cargo install cargo-sweep`) to remove build output older than a week, then delete regenerable caches (proxies, thumbnails, waveforms, rendered frames) under the project cache folder. If still below 15 GB, run `cargo clean`. Never delete source, fixture scripts, or anything outside this repo and its cache folder. If space stays below 10 GB after cleanup, tag the current task `(needs-human)` and stop.

**Task size.** One task is at most a few hours of focused work. If a task turns out larger, split it in place into sub-tasks, keep the original line as a heading, and continue. Splitting is a normal, expected action.

**Decisions.** Phase 0 records the defaults. Use them. Change a default only when a benchmark in this repo proves it fails a budget, then record the change in the Decision Log. Never stop to ask about a decision that has a default here.

**Stop and ask only when:** a task needs a paid account, a signing certificate, a licence purchase, or deletes user data. Tag such a task `(needs-human)`, skip it, and continue with the next task. Put a one-line note in the Progress Log.

**Never do:** commit to `main` directly (work on `goal/<phase>` branches and merge when a phase's Done when passes), add telemetry, download models or assets into the repo (use on-demand download paths), hardcode UI strings or colours, or block the UI thread.

**Quality gates that apply to every task:**

- Builds and tests pass on macOS and Windows in CI
- No UI-thread stall over 8 ms in the frame-time monitor while the feature is used
- Size check in CI still passes, or the budget was raised with a Decision Log entry
- Every new user-facing string is in `assets/i18n/en.json`
- UI matches its board in the Paper file and uses only its tokens, with exact values read from the board, not from screenshots. A moment with no board gets one, following `DESIGN_HANDOFF.md`, before it's built
- If data flow changed, `docs/ARCHITECTURE.md` is updated in the same commit

Legend: `[ ]` todo · `[~]` in progress · `[x]` done · `[-]` dropped (say why in the Decision Log) · `(parallel)` may be done out of phase order · `(needs-human)` blocked on a person

Milestones: **M0** decisions → **M1** engine → **M2** editable timeline → **M3** export (v0.1) → **M4** built-in library (v0.5) → **M5** easy mode → **M6** pro tools (v1.0) → **M7** ecosystem.

---

## Phase 0 — Decisions and prototypes (M0)

Done when: every default below is either confirmed by a prototype benchmark or replaced, and `docs/ARCHITECTURE.md` exists.

Defaults (use these unless a benchmark here proves otherwise):

| Concern | Default | Fallback |
| --- | --- | --- |
| UI toolkit | `egui` with `eframe` on `winit` | `iced` |
| GPU | `wgpu`, WGSL shaders, Metal on macOS, DX12 on Windows, Vulkan on Linux | none |
| Decode/encode | `ffmpeg-next` against a GPL shared FFmpeg 9.0.x build with x264, x265 and dav1d | none |
| HW decode | VideoToolbox (macOS), D3D11VA then NVDEC (Windows), VAAPI (Linux), software fallback | software |
| Audio I/O | `cpal` | none |
| Text shaping | `cosmic-text` | `rustybuzz` + `swash` |
| Persistent data | `im` crate for the edit tree, `serde_json` for files | `rpds` |
| Plugins | `wasmtime`, Component Model ABI | none |
| Speech-to-text | `whisper-rs`, models downloaded on demand | none |
| Packaging | `cargo-dist` | hand-written CI |
| Min targets | macOS 12 arm64 + x86_64, Windows 10 x86_64 | Linux best effort |

Budgets (enforced by benchmarks from Phase 1 on): UI frame under 8 ms at 120 Hz · scrub latency under 50 ms · project open under 1 s · cold start under 1.5 s. Measured on the reference hardware: an Apple M1 Mac and a Windows PC with a GTX 1660 class GPU. Slower supported machines stay smooth by lowering preview resolution, not by dropping frames.

Size target: installers around 30 MB, not counting asset packs and models, which download on demand. Unlike the speed budgets, this one gives way: raise it with a Decision Log entry when a feature or a speed optimisation needs the space.

- [x] Create the new repo with this file as `PLAN.md`, `DESIGN_HANDOFF.md`, and an MIT `LICENSE`
- [x] Prototype `proto-ui`: 200 fake clips on 10 tracks in `egui`, measure frame time at 120 Hz. Done when: median under 4 ms, p99 under 8 ms, or switch default
- Prototype `proto-gpu`: two 4K textures, one dissolve WGSL shader, one text overlay via `wgpu`. Done when: under 2 ms GPU time on Apple M1 and a GTX 1660 class GPU (split; resolved by its sub-tasks)
  - [x] `proto-gpu` on Apple silicon
  - [ ] `proto-gpu` on a GTX 1660 class GPU (needs-human: no NVIDIA hardware available locally)
- [x] Prototype `proto-decode`: FFmpeg hardware decode of 4K H.264 to a `wgpu` texture on macOS. Done when: 60 fps sustained with under 5% CPU per stream
- [ ] Same prototype on Windows via D3D11VA. Done when: same budget (needs-human: no Windows PC available locally; port proto-decode to D3D11VA on one)
- [x] Write `docs/ARCHITECTURE.md`: threading model, data flow diagram, crate layout, cache design, the defaults table above
- [x] Write `docs/DECISIONS.md` (Decision Log) with the entries created so far
- [x] Delete the prototypes or move them to `experiments/`

## Phase 1 — Project skeleton and CI

Done when: a tagged push produces signed-or-unsigned installers for both platforms, CI is green, and `cargo run --bin smoke` exits 0.

- [x] `.cargo/config.toml` with `[build] jobs = 4`, and `CARGO_BUILD_JOBS=4` documented in `CONTRIBUTING.md`
- [x] Lean dev builds to save disk: `[profile.dev] debug = "line-tables-only"` and `[profile.dev.package."*"] debug = false` in the workspace `Cargo.toml`
- [x] Cargo workspace with crates: `app`, `ui`, `engine`, `media`, `render`, `project`, `audio`, `plugins`, `assets`, `smoke`
- [x] Feature flags: `codec-av1`, `codec-prores-sw`, `ai-whisper`, `ai-segmentation`, off by default, all on in release builds
- [x] `smoke` binary: opens `tests/fixtures/reference.proj`, plays 2 s headless, exports 1 s, exits 0. Starts as a stub that exits 0 and grows with each phase
- [x] Reference media in `tests/fixtures/` generated by a script (FFmpeg synthetic sources), not committed binaries: H.264, HEVC, ProRes, VP9, AV1, VFR, 10-bit, stills, WAV
- [ ] GitHub Actions matrix: macOS arm64, macOS x86_64, Windows x86_64, Linux x86_64, running build, test, clippy, fmt (needs-human: no GitHub remote yet; push the repo and confirm the workflow runs green)
- [x] `cargo-deny` for licences and advisories, allowing only licences compatible with GPLv3
- [ ] FFmpeg build script: pinned FFmpeg 9.0.x with x264 and x265, built as shared libraries for every target and cached in CI. Every release bundles the licence texts and attaches the exact source archives used (needs-human: blocked only on the CI sub-task below)
  - [x] `scripts/ffmpeg/build.sh` and pinned `scripts/ffmpeg/versions.env`: download and checksum the source archives, build pkgconf (and nasm on x86) as host tools, then x264, x265 and FFmpeg as shared libraries into `.glass-cache/ffmpeg/<target>/` with `lib/`, `include/`, `LICENSES/` and `SOURCES.txt`
  - [x] Build locally for `aarch64-apple-darwin`, check the workspace and `experiments/proto-decode` against it, and make `scripts/dev-env.sh` prefer it over Homebrew
  - [x] `scripts/ffmpeg/package-sources.sh` gathering the exact source archives and licence texts for a release, plus GPL compliance notes in `CONTRIBUTING.md`
  - [ ] CI job building FFmpeg per target, cached on the `versions.env` hash, and release attachment of the sources (needs-human: no GitHub remote yet, and the Windows and Linux builds can only run in CI; push, confirm all four targets build and a `v*` tag attaches the sources)
- [ ] `cargo-dist` config producing DMG, MSI or MSIX, and AppImage on tags (needs-human: blocked only on the sub-task below)
  - [x] `dist-workspace.toml` and the generated `release.yml` (cargo-dist 0.32: archives and a WiX MSI with the FFmpeg DLLs for the `glass` binary with `release-full`, four targets), plus `scripts/package/` for DMG, MSIX and AppImage, run by `package.yml` as a dist local-artifacts job
  - [x] Build `Glass.app` and the DMG locally on `aarch64-apple-darwin` and check linkage, signature and launch from the mounted image
  - [ ] Tagged release in CI producing the DMGs, MSI, MSIX and AppImage (needs-human: no GitHub remote yet; Windows MSI/MSIX and Linux AppImage can only be built in CI. Set `repository` in `Cargo.toml`, run `dist generate`, push a `v*` tag and check every installer; MSIX and notarised DMG also need signing certificates)
- [x] App icons exported from the Brand page: macOS icon with the graphite, light, and clear appearances, Windows `.ico` and Linux PNGs using the bare mark, with the small-size variants the page defines (glow drops out at 64 px, a slice at 32 px)
- [x] Size check in CI: fails when an installer grows past the stored budget (starting at 30 MB). Raising the budget is allowed with a Decision Log entry
- [ ] Benchmark harness with `criterion` and a `bench` CI job that posts results as a PR comment (needs-human: blocked only on the sub-task below)
  - [x] `criterion` harness: workspace dev-dependency, `benches/` targets, `cargo bench --workspace --bench "*"` with short settings and baselines documented in `CONTRIBUTING.md`, and `scripts/bench-report.py` formatting a baseline comparison as Markdown
  - [ ] `bench` job in `ci.yml`: saves a `main` baseline on pushes to main, compares pull requests against it and posts the report as a PR comment (needs-human: no GitHub remote yet; push, merge once to main to seed the baseline, then open a PR and confirm the comment appears)
- [x] Logging with `tracing`, per-crate filters, in-memory ring buffer exposed to the UI later
- [x] Frame-time monitor: records UI frame durations, exposes p50 and p99, fails tests that exceed budget
- [x] i18n: `assets/i18n/en.json`, a `t!()` macro, and a CI check that every key used exists and no literal strings appear in UI code
- [x] Theme: tokens in `assets/theme/` (colour, type, spacing, radius, for the default, opposite, and high-contrast themes) loaded into the egui style, and a CI check that no literal colours appear in UI code. Values come from the `00 Foundations` page of the Paper file
- [x] Opt-in crash minidumps written locally, never uploaded automatically
- [x] `CONTRIBUTING.md` with the session loop from this file

## Phase 2 — Media engine (M1)

Done when: every fixture file decodes to GPU textures and f32 audio through the same API, with hardware decode active on both platforms, and `smoke` plays the reference project.

### Decode
- [x] Safe wrapper over `ffmpeg-next`: `Demuxer`, `Decoder`, `Frame`, `Packet`, with error types and no `unsafe` outside `media::ffi`
- [x] Probe: streams, codecs, duration, frame rate, colour metadata, rotation, timecode
- [x] Software decode path for video and audio, tested against every fixture
- [x] Hardware decode: VideoToolbox. Done when: zero-copy `IOSurface` → Metal texture
- [ ] Hardware decode: D3D11VA. Done when: shared handle → DX12 texture without CPU copy (needs-human: needs a Windows PC with a D3D11 GPU to implement and verify)
- [ ] Hardware decode: NVDEC and VAAPI (parallel) (needs-human: needs NVIDIA and Intel or AMD Linux hardware to implement and verify)
- [x] Automatic fallback to software per file with a reason string surfaced in the UI
- [x] Colour space plumbing: BT.709, BT.2020, sRGB, limited and full range, 8/10/12-bit, PQ and HLG flags carried on every frame
- [x] VFR normalisation to the sequence frame rate with a per-clip timestamp map
- [x] Accurate seek: nearest keyframe then decode forward, GOP index cached per file on disk
- [ ] Still images, image sequences, GIF, WebP, HEIC via `image` and platform decoders (needs-human: blocked only on the Windows sub-task below)
  - [x] `media::source::VideoSource` for video files, stills (PNG, JPEG, TIFF, BMP, WebP, GIF with `image`; HEIC, HEIF and AVIF with ImageIO on macOS), animated GIF and WebP (variable-rate timelines from frame delays) and numbered sequences (caller's rate, gaps reported); colour from `cICP` and ICC with Display P3 flagged; EXIF orientation applied; fixtures and tests
  - [ ] HEIC, HEIF and AVIF stills on Windows through WIC with the HEIF and AV1 Video Extensions, replacing the `media.unsupported.heif`/`avif` error there (needs-human: needs a Windows PC to implement and verify)
- [x] Audio decode to f32 planar at the project rate using a high-quality resampler (`rubato`)

### Analysis (background, cancellable, results cached under the project's cache folder)
- [x] Analysis job system: priority queue, cancellation, progress, disk cache keyed by content hash
- [x] Thumbnail strips at 3 zoom levels
- [x] Waveform peaks at 5 zoom levels
- [x] Proxy generation: ProRes Proxy on macOS, H.264 720p on Windows, user-selectable, resumable
- [x] Scene cut detection
- [x] Silence detection with threshold and minimum length
- [x] Beat and tempo detection
- [x] Loudness analysis (EBU R128 integrated and short-term)

### Media management
- [x] Import from drag-drop, file dialog, and folders, recursive, with camera card structures recognised (AVCHD, XDCAM, Sony, Canon, DJI)
- [x] Media pool with bins, colour labels, tags, and a search index
  - [x] Pool model and search index: `project::media_pool::MediaPool` (imbl, nested bins, colour labels, tags, items keyed by `MediaId`) and `SearchIndex` (name, tags, labels, metadata; prefix and typo matches), tests and a 10,000-item search bench
  - [x] Media pool panel `ui::panels::media_pool`: list and grid views, bins tree, labels, tags, search, metadata columns, proxy, offline and analysis badges; snapshot cases and a frame-budget test
- [x] Relinking: fuzzy match by name, duration, and content hash, batch relink dialog
- [x] Offline media placeholder that keeps edits intact and renders a labelled slate
- [x] Metadata panel: camera, lens, timecode, GPS, created date

## Phase 3 — Playback engine (M1)

Done when: the stress project (Phase 16) plays at full rate on both platforms, scrub latency benchmark passes, and no UI frame exceeds 8 ms during playback.

- [x] Thread layout implemented: UI, scheduler, decode pool, render, audio callback, analysis pool, all named, all visible in the debug panel
- [x] Lock-free frame queues between decode, render, and present (`crossbeam` or `rtrb`)
- [ ] Master clock from the audio device callback, video slaved to it, drift correction (needs-human: blocked only on the sub-task below)
  - [x] Audio output through `cpal` with an `rtrb` feed ring and an offline backend for tests and export; the callback allocates and locks nothing (counting-allocator test)
  - [x] Master clock: callback frame counter plus output latency, drift-corrected by a delay-locked loop, with a monotonic stand-in when no device is open
  - [x] Video slaved to the clock: one frame per display refresh with hysteresis, drift corrected by a single repeated or skipped frame
  - [ ] Listening check on real hardware: run `GLASS_AUDIO_DEVICE_TEST=1 cargo test -p audio --test device` on built-in speakers, USB and Bluetooth outputs, and confirm lip sync by eye and ear once the engine plays through the clock (needs-human: judging audible sync and other output devices needs a person at the machine)
- [ ] Look-ahead decode across cuts and transitions, configurable depth
- [x] Rendered-frame cache keyed by (segment hash, frame index), LRU in VRAM with RAM spill and a configurable ceiling
- [ ] Dirty-range invalidation: any edit invalidates only affected segments, verified by a test that counts re-renders
- [ ] Adaptive quality: drop preview resolution before dropping frames, restore when idle
- [ ] Scrub mode: nearest cached frame immediately, exact frame when the pointer pauses for 40 ms
- [ ] JKL shuttle, reverse playback, audio scrubbing
- [ ] Loop range, in and out points, play-around with configurable pre-roll
- [ ] Background render of idle regions when CPU and GPU are quiet, pauses on user input
- [ ] Clean feed to a second display, full screen
- [ ] Benchmarks: scrub latency, playback frame drops, cache hit rate, added to the bench job

## Phase 4 — GPU compositor (M1)

Done when: a sequence with 5 video layers, blend modes, transforms, text, shapes, and masks renders at 4K within 8 ms GPU time, and readback for export does not stall the UI.

- [x] Render graph: per-frame DAG compiled into `wgpu` passes with resource aliasing
- [x] Linear-light working space, `Rgba16Float` intermediates, dithered 8-bit output
- [x] Layer compositing with blend modes: normal, multiply, screen, overlay, add, subtract, difference, soft light, hard light, colour dodge, colour burn
- [x] Transform node: position, scale, rotation, anchor, bicubic resampling, motion-blur ready
- [x] Opacity, crop with feather
- [x] Shader hot reload in debug builds
- [x] Effect parameter system: typed params, ranges, defaults, units, keyframeable flag, UI auto-generated from the schema
- [x] Text rendering via `cosmic-text`: glyph atlas, outlines, shadows, per-glyph transform hooks
- [x] Vector shapes: rect, ellipse, polygon, path, stroke, fill, gradients
- [x] Vector shapes within the 4K budget: 50 mixed shapes into a 4K layer (`fifty_shapes_4k_gpu_time`, ignored until then) at a median under 8 ms GPU; 9.3 ms on the M2 now, with 29.9 M blended accumulate fragments (each edge's quad runs to its outline box's right edge) and 12.8 M paint fragments
- [x] Mask node: shape masks, feather, invert, intersection and union
- [x] Async GPU readback ring for export and scopes
- [x] Visual regression tests: render reference sequences, compare against golden PNGs with a tolerance

## Phase 5 — Project model (M2)

Done when: a 1000-clip project round-trips through save and load byte-identically, undo depth is unlimited, and crash recovery restores the last autosave.

- [x] Immutable edit tree using `imbl` (the maintained fork of `im`, see Decision Log) with structural sharing
- [x] Undo and redo as a list of tree roots, grouped per gesture, unlimited
- [x] Autosave as diffs every 10 s to the cache folder, recovery prompt on next launch
- [x] Versioned schema with a migration function per version, tested with fixtures from every prior version
- [x] Sequences, tracks (video, audio, subtitle, adjustment), clips, gaps, markers
- [x] Clip properties: source range, timeline range, speed map, effect stack, transform, audio gain, enabled flag
- [ ] Transitions as first-class items between two clips with alignment (centre, start, end)
- [ ] Nested sequences and compound clips
- [ ] Adjustment layers
- [ ] Keyframe curves per parameter: linear, bezier, hold, with time and value scaling
- [ ] Project settings: resolution, frame rate, colour space, audio rate, cache folder
- [ ] Copy and paste of clips, attributes, and effects, across projects via the clipboard
- [ ] Pretty JSON project file with stable key order for clean git diffs
- [ ] `tests/fixtures/reference.proj` filled with a real reference sequence used by `smoke`

## Phase 6 — Timeline UI (M2)

Done when: every tool below works with keyboard and mouse, the 200-clip benchmark stays under budget, and the UI passes the i18n check with RTL enabled.

- [ ] Track headers: name, mute, solo, lock, visibility, height presets
- [ ] Clip rendering: thumbnails, waveforms, names, effect badges, speed indicator, all from the analysis cache
- [ ] Zoom and scroll with snapping, minimap, zoom to fit, zoom to selection
- [ ] Playhead, in and out, markers with colours and notes
- [ ] Snapping to clip edges, playhead, markers, beats, toggle with a key
- [ ] Selection: click, shift, marquee, track targeting
- [ ] Magnetic mode (ripple by default) and free mode
- [ ] Tools: select, razor, ripple, roll, slip, slide, rate stretch, hand, zoom
- [ ] Trim with numeric entry and a trim monitor showing both sides of the cut
- [ ] Drag from bin: insert vs overwrite, three-point edit
- [ ] Linked audio and video with unlink and relink
- [ ] Storyboard view: cards, drag to reorder, one transition per gap
- [ ] Remappable shortcuts with presets for Premiere, Final Cut, and DaVinci
  - [x] Command registry, keymap model (preset plus overrides, clash checks, JSON load and save from a path), Glass, Premiere Pro, Final Cut Pro and DaVinci Resolve presets, dispatch of egui key events (pulled forward: needs no timeline model)
  - [x] Keyboard editor window from `Sheet / Keyboard`: keyboard map with modifier layers, key detail, your changes, system keys, command list searchable by name or key with filters, recording, clash Swap / Take it, Start from preset menu, undo
  - [ ] `Sheet / Keyboard` rest: Switch preset dialog (keep my changes or start clean), saved sets, import and export from the preset menu, preset comparison table, toasts with Undo; the Settings › Shortcuts pane from `Dialog / Settings — Shortcuts`; the app loads and saves `keymap.json` in the config folder and dispatches through it once the app shell exists
- [ ] Command palette: effects, clips, commands, fuzzy search
- [ ] Right-to-left layout and full i18n
- [ ] Dockable panels, saveable workspaces, presets for Edit, Colour, Audio, Effects
- [ ] Debug panel: thread activity, cache stats, frame-time graph, log ring buffer

## Phase 7 — Core editing features (M2)

Done when: the reference project can be recreated from raw fixtures using only the UI, recorded as an integration test script.

- [ ] Speed change and speed ramps with keyframes, frame blending, optical flow behind a feature flag
- [ ] Freeze frame and reverse
- [ ] Preview transform handles: position, scale, rotation, crop
- [ ] Text tool directly in the preview with basic titles
- [ ] Clip audio gain, fades, pan
- [ ] Marker export to CSV and YouTube chapter format
- [ ] Batch rename and colour labels
- [ ] Find and replace media, consolidate project, trim unused media
- [ ] Integration test: scripted UI run that rebuilds the reference project

## Phase 8 — Export (M3, v0.1)

Done when: the v0.1 checklist passes on both platforms and a tagged build ships installers.

- [ ] Encoder wrapper: H.264, HEVC, AV1, ProRes (native on macOS, software elsewhere behind a flag), VP9, PNG and TIFF sequences, GIF, WAV, AAC, MP3, FLAC (needs-human: blocked only on the AV1, VP9 and MP3 sub-task below)
  - [x] Export session and muxer: settings structs (codec, CRF or bit rate, pixel format, BT.709/BT.2020 colour tags, frame rate, audio rate and channels), RGBA and YUV picture input through swscale, MP4, MOV and Matroska, H.264 (x264) and HEVC (x265)
  - [x] ProRes: VideoToolbox on macOS, `prores_ks` behind `codec-prores-sw` elsewhere
  - [x] PNG and TIFF sequences, GIF
  - [x] Audio: WAV, AAC and FLAC, alone and in video containers
  - [ ] AV1, VP9 and MP3 encoders: in the API, reported unavailable by this build (needs-human: the pinned FFmpeg 9.0.2 build has no AV1, VP9 or MP3 encoder; adding SVT-AV1 or libaom, libvpx and LAME needs new source downloads in `scripts/ffmpeg/versions.env` and a rebuild, Decision 47)
- [ ] Hardware encode: VideoToolbox, NVENC, QSV, AMF, with software fallback (x264 for H.264, x265 for HEVC)
- [ ] Presets: YouTube, TikTok, Instagram, X, Vimeo, broadcast, archival
- [ ] Custom preset editor: bitrate, CRF, two-pass, colour tags, audio settings
- [ ] Background export queue with progress, pause, cancel, notifications
- [ ] Smart export: reuse cached rendered segments, stream-copy untouched ranges when the codec matches
- [ ] Export in and out range, selected clips, stills
- [ ] Subtitle export: burned in, sidecar SRT and VTT, embedded
- [ ] Output verification: duration, frame count, loudness, then reveal in Finder or Explorer
- [ ] **v0.1 checklist:** import, play, cut, trim, 10 transitions, basic titles, export, both platforms, installers on tag

## Phase 9 — Built-in library (M4, v0.5)

Done when: the transition browser has 150+ items, the title browser 60+, the LUT browser 50+, all with hover preview, and the app never shows an empty panel on first launch.

### Transitions (all GPU, all with editable duration, easing, direction, hover preview)
- [ ] Transition framework: a transition is a shader plus a param schema plus a thumbnail, discovered from `assets/transitions/`
- [ ] Dissolve family: cross, dip to black, dip to white, dip to colour, film, additive, non-additive
- [ ] Wipes: linear in 8 directions, radial, clock, iris shapes, barn door, venetian, checker
- [ ] Motion: push, slide, whip pan with blur, zoom in and out, spin, swirl, roll
- [ ] Stylised: glitch, RGB split, pixelate, VHS, light leak, film burn, ink bleed, paper tear, liquid
- [ ] Shape and mask: circle, heart, star, custom SVG, text-shaped
- [ ] 3D: cube, flip, page turn, door
- [ ] Morph cut for jump cuts in talking-head footage (feature flag, optical flow)
- [ ] Transition browser with live hover preview, favourites, search
- [ ] Default transition and duration, apply to all selected cuts
- [ ] Easing curves editor shared by transitions and keyframes

### Titles and graphics
- [ ] Template framework: a title is a small sequence with exposed fields, discovered from `assets/titles/`
- [ ] 60+ title templates: lower thirds, centred, kinetic, cinematic, social, minimal, bold
- [ ] Text animator: per-character and per-word reveal, typewriter, bounce, blur in, tracking
- [ ] Shapes, arrows, callouts, progress bars, counters, timers
- [ ] Sticker and emoji browser
- [ ] Animated backgrounds and gradients
- [ ] Bundled open-licence fonts plus system font access
- [ ] Save any graphic as a reusable template with editable fields

### Looks and effects
- [x] LUT framework: `.cube` loader, GPU 3D LUT node, intensity slider
- [ ] 50+ LUTs grouped by mood, thumbnails rendered from the current frame
- [ ] Film grain, halation, bloom, vignette, chromatic aberration, lens distortion
- [ ] Blur (gaussian, directional, radial, zoom), sharpen, glow, mosaic, posterise
- [ ] Mirror, kaleidoscope, tile, RGB shift, VHS, old film
- [ ] Effect browser with categories, search, drag to clip, hover preview

### Sound
- [ ] Asset pack system: packs downloaded on demand into the user data folder, versioned, licence file included
- [ ] Music pack: mood, tempo, duration filters, stems where available (needs-human: licence the catalogue)
- [ ] SFX pack: whooshes, impacts, clicks, ambience, foley (needs-human: licence the catalogue)
- [ ] Auto-add a matching whoosh when a transition is applied, toggle
- [ ] Auto-duck music under speech

### Motion presets
- [ ] Clip animations: fade, slide, zoom, shake, bounce, pop, rotate
- [ ] Speed ramp presets: slow-mo hit, speed up, time freeze
- [ ] Ken Burns presets for stills
- [ ] **v0.5 checklist:** all browsers populated, hover previews under 100 ms, installers on tag

## Phase 10 — Easy mode and AI assist (M5)

Done when: the guided first-run flow takes raw clips to an exported vertical video with captions and music in under 10 clicks.

All on-device. Models download on demand so the installer stays small.

- [ ] Model manager: download, verify, cache, delete models in the user data folder
- [ ] Speech-to-text captions with `whisper-rs`, word timing, speaker labels
- [ ] Caption styles: 30+ animated presets, karaoke highlight, emoji injection, in-place editing
- [ ] Auto-cut silences and filler words with sensitivity slider and a review list
- [ ] Auto-reframe 16:9 to 9:16 and 1:1 with subject tracking
- [ ] Scene detection into clips on import
- [ ] Beat markers from music and snap-to-beat
- [ ] Smart colour match between selected clips
- [ ] One-click stabilisation
- [ ] One-click audio cleanup: noise, reverb, hum, plosives
- [ ] Background removal without green screen via a segmentation model
- [ ] Auto-highlight suggestions from long recordings
- [ ] Text-based editing: edit the transcript, the timeline follows
- [ ] Guided first-run flow: pick format, drop clips, pick a look, add music, export
- [ ] Templates gallery: intro, vlog, product demo, tutorial, recap, slideshow

## Phase 11 — Colour page (M6)

Done when: a graded reference shot matches a golden render within tolerance and all scopes update at playback rate.

- [ ] Node-based colour graph per clip and per sequence
- [ ] Primary wheels: lift, gamma, gain, offset, temperature, tint
- [ ] Curves: RGB, hue vs hue, hue vs sat, hue vs lum, lum vs sat, sat vs sat
- [ ] HSL qualifier with softness and mask overlay
- [ ] Power windows with tracking
- [ ] GPU scopes: waveform, RGB parade, vectorscope, histogram, false colour
- [ ] LUT import (`.cube`, `.3dl`) and export
- [ ] Colour management: per-clip input transform, ACES and BT.2020 pipelines, HDR grading, tone mapping to SDR
- [ ] Shot matching, stills gallery, wipe compare
- [ ] Grade copy, paste, and groups

## Phase 12 — Audio (M6)

Done when: a 16-track mix with effects and automation plays without dropouts at a 256-sample buffer on both platforms.

- [ ] Mixer: buses, sends, faders, meters, automation lanes
- [ ] Effects: EQ, compressor, limiter, gate, de-esser, reverb, delay, pitch shift
- [ ] Spectral noise removal and de-hum
- [ ] Loudness normalisation to platform targets
- [ ] VST3 and AU hosting in a sandboxed helper process
- [ ] Audio clip effects and keyframes on the timeline
- [ ] Sample-accurate editing and crossfades
- [ ] Voice-over recording to the timeline with punch and roll
- [ ] Stereo and surround panning, downmix rules
- [ ] Audio-only and stem export

## Phase 13 — Advanced compositing (M6)

Done when: a tracked, keyed, masked composite renders correctly in the visual regression suite.

- [ ] Graph editor for keyframes: bezier handles, ease presets, copy curves
- [ ] Expressions for parameter linking, small embedded language with a test suite
- [ ] Masks: pen tool, feather, expansion, tracking, roto brush with AI edge refinement
- [ ] Planar and point tracking, attach layers to tracks
- [ ] Chroma key: spill suppression, edge refine, light wrap, garbage mattes
- [ ] Luma key and difference key
- [ ] Motion blur for animated layers
- [ ] 3D layer transforms with camera and depth of field
- [ ] Time remapping with optical flow
- [ ] Stabilisation with crop, warp, and rolling shutter fix

## Phase 14 — Multicam, collaboration and workflow (M6, v1.0)

Done when: the v1.0 checklist passes.

- [ ] Multicam sync by waveform, timecode, or markers
- [ ] Real-time angle switching with cut recording
- [ ] Nested sequences and compound clips with editable inner timelines
- [ ] Proxy and full-res toggle, per clip and global
- [ ] Project archive: collect media, trim unused, zip
- [ ] Interchange: FCPXML, OTIO, EDL, Premiere XML import and export, best effort with a compatibility matrix in docs
- [ ] Project locking and change history for shared drives
- [ ] Review export with burnt-in timecode and watermark
- [ ] **v1.0 checklist:** Phases 9 to 14 complete, stress project passes, both platforms signed (needs-human: certificates)

## Phase 15 — Plugins and scripting (M7)

Done when: a third-party effect, transition, and title each load from a plugin bundle and run inside the sandbox.

- [ ] WASM effect ABI: params, textures, time, project info, via the Component Model
- [ ] Plugin manager: install, update, enable, disable, resource limits
- [ ] Transition and title plugins on the same ABI
- [ ] Lua scripting API for batch operations and automation
- [ ] Optional Python bridge (feature flag)
- [ ] Plugin SDK: template repo, docs, hot reload
- [ ] Community asset packs installable as bundles

## Phase 16 — Performance and quality (parallel, ongoing from Phase 3)

Done when: every benchmark and budget is enforced in CI and the stress project passes.

- [ ] Stress project generator: 1000 clips, 20 tracks, 4K 10-bit, transitions on every cut. Clips reuse about 20 generated source files with different in and out points, so the media stays under 5 GB (parallel)
- [ ] Benchmarks on every PR: open, scrub, playback, export on reference projects (parallel)
- [ ] Memory ceiling: configurable cache size, never swaps (parallel)
- [ ] Battery mode: lower preview resolution, pause background analysis (parallel)
- [ ] Fuzzing of parsers: project files, LUTs, subtitles, with `cargo-fuzz`, run in CI only (parallel)
- [x] UI snapshot tests for every panel in light and dark (parallel)
- [ ] Accessibility: keyboard for everything, screen reader labels, high contrast, scalable UI (parallel)
- [ ] Full UI localisation and dictionary check in CI (parallel)

## Phase 17 — Distribution

Done when: a tagged build installs cleanly on a fresh macOS and Windows machine without warnings.

- [ ] macOS: Universal binary, hardened runtime, DMG
- [ ] macOS: notarisation (needs-human: Apple Developer account and certificate)
- [ ] Windows: MSIX and portable EXE (the portable zip ships an empty `Glass Data` folder beside `glass.exe` so it starts in portable mode; CONTRIBUTING.md, Portable mode)
- [ ] Windows: code signing (needs-human: certificate)
- [ ] Linux: AppImage and Flatpak
- [ ] Auto-updater with delta updates and rollback
- [ ] Asset packs downloaded on demand so the installer stays small
- [x] Portable mode with settings next to the executable
- [ ] Safe mode: disable plugins and GPU features on repeated crash
- [-] Licensing and activation (dropped: free and open source, see Decision Log)
- [x] About dialog: version, MIT and GPLv3 notices, third-party licences, link to the release's source archives, matching the About window on the Brand page

## Phase 18 — Documentation and onboarding

Done when: a new user can complete the first-run flow without external help and a contributor can build from the docs alone.

- [ ] In-app interactive tutorial on first launch
- [ ] Tooltips with short video loops for every tool
- [ ] Searchable help and shortcut cheat sheet
- [ ] Bundled sample project so the app is never empty
- [ ] Public roadmap and changelog generated from this file and git tags
- [ ] Developer docs: architecture, plugin SDK, contributing guide

---

## Risk register

| Risk | Mitigation |
| --- | --- |
| UI toolkit too immature for a pro timeline | Phase 0 benchmark decides, timeline widget kept self-contained so it can be swapped |
| Hardware decode edge cases per GPU vendor | Software fallback always available, per-file override, local failure log |
| Shipping a GPL build without meeting its terms | Every release attaches the FFmpeg, x264, and x265 source it was built from, and `cargo-deny` blocks dependencies incompatible with GPLv3 |
| AI models bloat the binary | On-demand download, feature flags, quantised models |
| Stale frames from cache invalidation bugs | Deterministic segment hashing, re-render counting test, visual regression suite |
| Scope creep before v0.1 | Nothing from Phase 9 onward starts until the v0.1 checklist is green |
| Too many parallel agents overload the machine | Hard cap of 10 agents, one build at a time, 4 compiler jobs, serial benchmarks, drop to one agent when the machine slows |
| Long-running agent drifts from the plan | Session loop above, one task at a time, every session starts by reading this file |

## Decision Log

Append one line per decision: date, decision, reason. Add the same entry to `docs/DECISIONS.md`, which may carry more context.

- 2026-09-29 — Adopted the defaults table in Phase 0 as the starting stack. Reason: fastest path to a single small binary on both platforms.
- 2026-09-29 — Free and open source under MIT. Licensing and activation dropped. Reason: product decision.
- 2026-09-29 — Priority is speed, then features, then size. The 30 MB size target gives way to features and speed optimisations, and release builds ship with every feature flag on. Reason: product decision.
- 2026-09-29 — Creators come first, and pro tools are how the app grows with them. Reason: product decision.
- 2026-09-29 — FFmpeg switched from an LGPL build to a GPL build with x264 and x265. Release builds are distributed under GPLv3, the source stays MIT. Reason: better software H.264 and HEVC encoding, and the LGPL limit only mattered while a commercial licence was possible.
- 2026-09-29 — Each task uses three commits: plan: start, the implementation commit(s), and plan: done, whose Progress Log line cites the implementation commit hash. Reason: the log line can then point at the commit that did the work.
- 2026-09-29 — proto-decode CPU budget ("under 5% CPU per stream") is judged as whole-machine utilisation, counting the app process plus the system decoder service, at 60 fps paced. Measured on M2: 2.18% of machine (9.20% + 8.26% of one core). Reason: the plan does not define the unit; this is how system-wide decode cost is usually stated, and the per-core numbers are recorded in experiments/proto-decode/RESULTS.md for re-judging on the reference M1. Later "under X% CPU" budgets are judged the same way, with per-core numbers recorded alongside.
- 2026-09-29 — FFmpeg pinned to 9.0.x instead of 7.x, still a GPL shared build with x264 and x265, used through ffmpeg-next 9. Reason: the maintained ffmpeg-next 9 bindings build against it, and local development and release builds should use the same FFmpeg.
- 2026-09-29 — Phase 0 is closed with the UI, GPU, and macOS decode defaults confirmed by prototypes. Defaults without a Phase 0 prototype (cpal, im and serde_json, wasmtime, whisper-rs, cargo-dist, Vulkan, NVDEC and VAAPI, min targets) are adopted and confirmed by the tests and benchmarks of the phase that builds them. The GTX 1660 and Windows D3D11VA checks stay open as needs-human. Reason: Phase 0 only defines prototypes for UI, GPU, and decode, and the missing hardware should not block the engine work that follows.
- 2026-09-29 — App icon small-size rule: the slice count follows the mark's own drawn size (4 slices at 32 pt and up, 3 below), and glow is a tile rule (simplified at 128, none at 64 and below), following the drawn size ramps on the Brand page. Reason: the Brand page text said both "at 32 px" and "below 32 px"; the drawn ramps agree once read this way.
- 2026-09-29 — egui and eframe are used with the `default_fonts` feature off; Glass ships Geist and Geist Mono (OFL) from `assets/fonts/`. Reason: `default_fonts` bundles the Ubuntu font, which `deny.toml` rejects, and the design uses Geist.
- 2026-09-29 — Phase 1 is merged with its local Done-when parts verified: smoke exits 0, the full local check is green, and the arm64 DMG builds within the size budget. CI green and tagged installers for both platforms stay open as needs-human tasks. From here on, a phase merges to main when every part of its Done when that can run on this Mac passes, and the rest is tagged needs-human. Reason: there is no GitHub remote and no Windows or NVIDIA hardware, and holding main back would not change what can be verified.
- 2026-09-29 — The pinned FFmpeg build adds dav1d for AV1 software decode, with meson and ninja built from pinned sources as host tools. Reason: AV1 is a supported format (codec-av1 flag, fixtures, export) and FFmpeg's built-in AV1 decoder needs hardware the M1/M2 lack.
- 2026-09-29 — The content hash keying per-file analysis is BLAKE3 of the size, the first and last 1 MiB and eight 64 KiB blocks, without the modification time. Reason: the cache must survive moving and copying media, and a copy gets a new modification time.
- 2026-09-29 — The render working space is linear light in the project primaries: BT.709 by default, BT.2020 when the project colour space is HDR (PQ or HLG). Sources are gamut-converted into it by a render-graph node (Phase 4). Reason: most creator footage and delivery targets are BT.709, and BT.2020 keeps HDR sources unclipped; the project colour space setting (Phase 5) already chooses between them.
- 2026-09-29 — Independent tasks now run in parallel in separate git worktrees, including tasks from the next phases whose inputs are already built, with cargo commands serialised by scripts/cargo-serial and one shared target directory. The hard limits stay: at most 10 agents, one cargo command at a time, 4 compiler jobs, serial benchmarks. Reason: the user asked for faster progress, and most agent time is spent outside the compiler.
- 2026-09-29 — The edit tree uses `imbl` 7 instead of `im` 15 (Phase 0 default). Reason: `cargo deny` rejects `im`: it is unmaintained (RUSTSEC-2026-0248) and has an unsoundness advisory with no fix (RUSTSEC-2023-0126); `imbl` is its maintained fork with the same API and licence (MPL-2.0).
- 2026-09-30 — Autosave lives in the per-user cache folder (`<cache>/autosave/<project uid>/`) until project settings add the per-project cache folder; the autosave format has its own version, separate from the project schema, and is never migrated; the recovery prompt shows ages instead of clock times until a date and time crate is chosen. Reason: no project files or settings exist yet, autosaves are short-lived, and local times need a time zone library.
- 2026-09-30 — Shader hot reload uses `notify` 8 (macOS FSEvents only) and a direct `naga` 30 (the version wgpu 30 uses) as ordinary `render` dependencies, with the watcher code compiled only under `debug_assertions`. Reason: Cargo cannot select dependencies by build profile (it does not support `debug_assertions` in target dependency tables); naga is already built for wgpu, and release binaries contain no watcher. notify adds a second `windows-sys` version (a `cargo deny` duplicate warning, not an error).
- 2026-09-30 — With up to 10 agents sharing one cargo lock, a task branch's finish check runs the workspace build, the tests of the crates it changed and of the crates that depend on them, clippy, fmt and smoke in one lock hold; the full `cargo test --workspace` runs on the integration branch after each batch of merges, and a failure there is fixed before the next batch. Reason: one full check takes about 30 minutes of cargo time (65 minutes under load), so running it on every branch left most agents waiting on the lock.
- 2026-09-30 — Loudness is measured with the `ebur128` crate (Rust port of libebur128, MIT; `cargo deny check` passes) rather than an in-house BS.1770-4 meter, and checked against FFmpeg's `ebur128` filter within 0.1 LU integrated, 0.3 dB true peak and 0.5 LU loudness range (FFmpeg's histogram percentiles read 0.3 to 0.4 LU higher on stepped programs; libebur128 passes EBU Tech 3341 and 3342). Reason: a maintained, reference-tested implementation of the standard, and the two meters agree where the standard is exact.
- 2026-09-30 — Text draws from an 8-bit coverage glyph atlas; outlines are vector strokes rasterised by swash (a round-join stroke twice the outline width, cached per glyph and width in the same atlas), not a signed distance field, and the drop shadow is a separable Gaussian of the fill-and-outline union in the render graph. cosmic-text 0.19 is built without its fontconfig feature. Reason: coverage matches the CPU rasteriser exactly (GPU vs CPU max 0.0005), strokes stay crisp at any width without SDF range limits, and fontdb scans the standard font folders itself; per-glyph scale above 1 is bilinear from the atlas, so the text animator should re-lay out at the final size for large zooms.
- 2026-09-30 — Blend modes run on premultiplied linear light in the working space (W3C Compositing Level 1 source-over with a separable blend function), and HDR backdrops above 1 keep their value where the W3C formulas would clip them: Screen takes max(Cb, Cs) once either is above 1, Colour dodge caps at max(1, Cb) instead of 1, Colour burn returns Cb for Cb >= 1. Reason: the working space is linear and can hold HDR values; a neutral screen, dodge or burn layer should not dim highlights, and for 0..1 values the results equal the W3C functions.
- 2026-09-30 — Media pool items are keyed by `project::MediaId` (bins by a new `project::BinId`), and colour labels draw in the clip roles (amber `clip/title`, blue `clip/video`, green `clip/audio`, red `clip/offline`, violet `clip/adjustment`) with their names always shown. Reason: pool items map one to one onto media references and `project` already has an `ItemId`; the board's label menu uses these clip colours.
- 2026-09-30 — Layer transforms, opacity and crop are sampled inside the composite pass (inverse matrix per canvas pixel, bicubic by default, nearest when the transform maps pixel centres onto texel centres, one shader entry point per filter, and a per-pass cap so playback can sample transformed layers bilinear while paused frames and export use bicubic); no standalone transform node; the per-layer transform holds shutter samples for motion blur, of which the middle one is rendered until Phase 13. Reason: five 4K layers already use about 5.5 ms of the 8 ms budget, bandwidth-bound, and a separate transform pass would add a full-frame intermediate per layer; exact bicubic costs about twice bilinear there.
- 2026-09-30 — A layer's crop is four per-edge amounts in layer pixels applied before the transform, with a feather that ramps each crop edge inward by a smoothstep (the layer's own edges when uncropped); opacity scales the premultiplied layer before the blend. Reason: the cut stays attached to the picture as it moves, an inward feather never reveals cropped pixels, and premultiplied scaling fades every blend mode towards the backdrop.
- 2026-09-30 — The output node encodes SDR video with the inverse BT.1886 EOTF (sRGB for the preview and stills, PQ absolute, HLG through the inverse 1000-nit OOTF), writes straight alpha by default, and dithers 8, 10 and 16-bit targets with one code of triangular noise seeded per frame. Reason: players decode BT.709 video with BT.1886, so display light round-trips with the display-referred working space; TPDF dither removes banding without bias.
- 2026-09-30 — Frame queues use `rtrb` 0.4 (single producer: render → present) and `crossbeam-queue`'s `ArrayQueue` (decode pool → render), with `crossbeam-utils`' `Parker` for waiting and a shared generation counter in every frame stamp for flushing. Reason: both are lock-free, bounded and allocation-free per operation and pass `cargo deny`; `rtrb` is the faster ring and can peek for the present policy; `sync_channel` senders blocked on a full queue cannot be woken by a seek.
- 2026-09-30 — `scripts/cargo-serial` deletes the workspace packages' fingerprints (dependencies kept) when the previous command ran in another checkout, recorded in `<git common dir>/glass-cargo-last-checkout`. Reason: every worktree shares one target directory and cargo names a workspace crate's fingerprint and artifacts by its path relative to the workspace root, so a crate built by another worktree looked fresh and a checkout could compile against another branch's code; Decision 16 wrongly said workspace crates get separate artifacts per worktree.
- 2026-09-30 — Offline media availability is engine runtime state beside the project (missing, unreadable, content hash changed), never written into the document; offline clips render a cached `render::slate` (dark theme roles, 640 x 360 design scaled, no Relink button). Reason: load, edit, undo and save never depend on the disk, and one render per size and label keeps frames to an import.
- 2026-09-30 — Project files are a versioned envelope (`format`, `schema_version`, `project`); loading runs one pure migration per version on the untyped JSON, then deserialises and validates the current model; saving writes the current version; newer files are refused with a clear message; schema v2 stores `media` as `{"refs": [...]}`. Reason: one set of model types in the code, small testable steps, no silent data loss from reading a newer file, and room for the media pool beside the references.
- 2026-09-30 — Vector shapes are rasterised on the GPU by signed-area coverage accumulation (each edge adds its exact box-filtered area contribution through additive blending into an `R32Float` atlas, the fill rule applied to the sum), with curves flattened and strokes expanded into same-winding pieces on the CPU, and no tessellation crate (`lyon` not added). Reason: exact anti-aliasing without MSAA, both fill rules, GPU cost of about twice each outline's area whatever its edge count, the same coverage feeds the mask node, and no new dependency; the price is approximate coverage in pixels where a twice-wound region meets an empty one, and `R32Float` blending needs `wgpu::Features::FLOAT32_BLENDABLE` on the shared device (else `R16Float`, about 1e-3 of residue).
- 2026-09-30 — Every long-lived thread starts through `engine::threads::spawn` and is listed in a registry with seqlock busy/idle/blocked accounting readable without locking the thread; the audio callback uses a pre-registered allocation-free hook; OS thread ids come from `thread-id` 5; the playback decode pool defaults to logical cores minus 3, clamped to 2..=4; render re-offers a frame to a full present queue every 2 ms. Reason: the debug panel reads activity every frame, the callback may not allocate or lock, engine has no unsafe code, four 4K software decoders fit 8 GB, and render must not wait on the UI.
- 2026-09-30 — The master clock is the audio callback's frame counter mapped to monotonic time by a delay-locked loop (1.5 Hz narrowing to 0.1 Hz) whose published line steers by slope, at most 0.5%, so it never jumps; audio I/O uses `cpal` 0.18 and an `rtrb` 0.4 feed ring. Reason: device crystals drift by hundreds of ppm and callback timing jitters by up to 1 ms; a DLL filters both for a few multiplications per callback, and `cargo deny check` passes with both crates.
- 2026-10-01 — Rendered-frame cache: one recency order across VRAM and RAM, key adds a render variant (size, quality, settings), lossless non-blocking spill through the readback ring, bit-exact by default, default ceilings 1/6 of the GPU working set and 1/8 of RAM (1/16 on unified memory). Reason: adaptive quality renders one frame at several sizes, and the render thread never waits on a spill.
- 2026-10-01 — LUTs (`.cube`) apply in the working space through a declared input space, Rec.709 display-referred (BT.709 primaries, inverse BT.1886 2.4 power) by default, with sRGB and linear working space as the other choices; values outside the table's domain clamp, and the intensity mixes the result with the input in linear light. 3D tables are stored as an `Rgba16Float` 3D texture and interpolated tetrahedrally from four `textureLoad`s in f32, not by hardware trilinear filtering. Reason: most creative LUTs are built for Rec.709 video code values; tetrahedral needs half the texel reads of trilinear, is exact on affine tables and keeps greys neutral, and shader weights avoid the filtering hardware's fixed-point fractions so the GPU matches the f64 reference; f16 table entries sit below a 10-bit code.
- 2026-10-01 — Portable mode is on when a folder named `Glass Data` exists next to the executable (next to `Glass.app` on macOS) and holds `Settings/`, `Data/` and `Cache/`; one resolver, `app::locations`, picks each folder from environment override, portable folder, platform folder, temporary folder, skipping any that cannot be written with a logged warning. Reason: a folder is the one thing a user creates anyway to keep the files beside the app, and falling back keeps Glass starting from read-only media.
- 2026-10-01 — The compositor's visual regression suite is a test-side scene registry in `render` (procedural sources through the render graph at 320 x 180, frame index as dither seed) compared with RGB golden PNGs by the UI snapshots' comparison, shared by path, at 6 levels on 0.05% of pixels (GPU) and 24 levels on 0.5% (software); goldens are macOS-only like the UI snapshots. Reason: `render`'s tests cannot depend on `ui`, one comparison is easier to keep right, and the compositor's float maths can be held tighter than egui.
- 2026-10-01 — Keymaps use one primary modifier (`Cmd`: ⌘ on macOS, Ctrl elsewhere; Mac Control folds into Ctrl off macOS), are stored as a preset name plus per-command overrides in versioned JSON at a path the app settings choose, and have no key sequences; presets bind only each editor's published default keys, Final Cut Pro's and Resolve's shared trim key is not a clash, and on a real clash dispatch runs the first command in the registry while the editor flags it. Reason: one preset table serves every platform, stored overrides let corrected presets reach users, and no preset needs sequences.
- 2026-10-01 — Gaps on a track are implicit (the empty space between items, derived by `Track::gaps`), never stored as gap items; markers use the five label colours (`ColourLabel`, clip theme roles) and sit in sequence time on sequences and in source time on clips; each track kind accepts one item kind (video and audio: clips, subtitle: subtitle cues, adjustment: adjustment items). Reason: stored gaps would have to be created, split and merged by every edit and kept in step with their neighbours for no information the item ranges do not already give; source-time clip markers stay on their frame through trims; the design's marker popover offers the same five colours as labels.
- 2026-10-01 — Relinking: an equal content hash is an exact match whatever the name; otherwise name 0.35, duration 0.35 (half a frame, at least 50 ms), resolution, frame rate and codec 0.1 each over what both sides know, times 0.9 when the hash differs, capped at 95%; tiers Exact, Likely (70%+, picked for the user) and Possible (40%+); every exact match's folder move is tried for the other offline sources; files shorter than the used source range are refused; a batch is one `relink_media` edit that replaces only references and their content hashes. Reason: the import hash is the one reliable identity, a same-name re-encode must never look identical, and one found clip should bring back a moved card or drive.
- 2026-09-30 — The About dialog lists what release builds ship: the bundled native libraries and the crates `app` reaches through normal dependencies (proc-macros, build and dev dependencies left out), generated into `assets/licences/third-party.tsv` and checked against `Cargo.lock` by a test; installers gather the crates' licence texts at packaging time. The app tile is drawn from the mark's SVG with the Brand page's fixed graphite colours (`ui::theme::brand`), not theme roles. Reason: only shipped code needs notices, a committed list keeps the dialog offline and testable, and the icon is artwork that looks the same in every theme (see docs/DECISIONS.md 42).
- 2026-10-11 — The compiler cap is 8 parallel jobs (`[build] jobs = 8` in `.cargo/config.toml`, `CARGO_BUILD_JOBS=8` in `scripts/dev-env.sh`), up from 4. Reason: development moved to a 10-core Mac with 24 GB RAM, where 4 jobs left half the cores idle; builds still run one at a time through `scripts/cargo-serial`. On a smaller machine, export `CARGO_BUILD_JOBS` lower after sourcing `dev-env.sh`.
- 2026-10-11 — The Phase 8 encoder wrapper is `media::export` (no new crate): RGB pictures become YUV inside the session with the colour tags' matrix, H.264 and HEVC through x264 and x265, ProRes through VideoToolbox on macOS with `allow_sw` and `prores_ks` only behind `codec-prores-sw`, FFmpeg's `aac` and `flac`, PNG and TIFF sequences written one file per frame by the session, GIF through the `gif` crate with a palette per frame; AV1, VP9 and MP3 report unavailable until the pinned FFmpeg build adds an encoder for them (needs-human). Reason: `media` already owns encoding and the `unsafe` boundary, Apple's software ProRes keeps the M1 (no ProRes engine) exporting, and the pinned build has no AV1, VP9 or MP3 encoder and adding one needs a download.
- 2026-10-11 — Clip properties (schema v4): the timeline range is stored and the source range derived as source start plus timeline duration times speed, exact; source-to-timeline durations round to the nearest grid step (halves up, at least one); the speed map is a list of (source time, speed) keys, one at 0 in v4 (1/100 to 100, reduced); transform position and anchor are offsets from the canvas and layer centres with per-axis scale, crop in layer pixels; effects are nodes holding a schema id, an enabled flag and values at the schema defaults; edits clamp numbers and refuse non-finite ones; autosave format 4. Reason: one source of truth on the grid, ramps later without a data migration, and a centred default without knowing canvas or source sizes (see docs/DECISIONS.md 45).
- 2026-10-11 — Vector shapes bin each outline into sparse strips (bands of 4 canvas rows, the columns edges cross merged into intervals, the winding each row carries right of them summed on the CPU): edges accumulate only to the end of their interval, paint reads the accumulation only in intervals and paints the gaps between with the carry, skipping empty ones; extends Decision 32. Reason: right of every edge crossing a row the integral is constant, so the old quads to the box's right edge were mostly wasted: 29.9 M accumulate and 12.8 M paint fragments become 0.4 M and 5.3 M on the 50-shape 4K frame, with coverage and goldens unchanged; the price is about 1 ms of CPU binning per frame for 25,749 edges in release.
- 2026-10-11 — Layer masks: each shape's interior from `shape::CoverageNode`, unioned per mask; feather is a Gaussian of sigma = feather / 3 truncated at the feather; invert after the feather; masks combine in list order by union (max) or intersection (min); the result multiplies the layer's premultiplied colour before its transform and blend. Reason: one rasteriser, an edge that stays put when feathered, and the crop's and opacity's conventions.

## Progress Log

Append one line per completed task: date, phase, task, commit hash. Newest last.

- 2026-09-29 — Plan created.
- 2026-09-29 — Phase 0 — create repository — ee8e233
- 2026-09-29 — Phase 0 — proto-ui — ae8be32 — egui on wgpu/Metal, M2, 200 clips: full-frame CPU median 1.37 ms, p99 3.00 ms (no culling: 1.09 / 3.13); default kept
- 2026-09-29 — Phase 0 — proto-gpu on Apple silicon — f8adadb — M2 (stands in for M1), Metal, 4K Rgba8UnormSrgb: total GPU median 0.72 ms, p99 0.77 ms
- 2026-09-29 — Phase 0 — proto-gpu on a GTX 1660 class GPU — needs-human: no NVIDIA hardware available locally; run `cargo run --release` in `experiments/proto-gpu/` on one
- 2026-09-29 — Phase 0 — proto-decode on macOS — e94e902 — VideoToolbox zero-copy to wgpu, 4K60 H.264 paced 60 fps, 0 late: 2.18% of machine (9.20% process + 8.26% decoder service, % of one core)
- 2026-09-29 — Phase 0 — proto-decode on Windows via D3D11VA — needs-human: no Windows PC available locally
- 2026-09-29 — Phase 0 — architecture doc — 17b6897
- 2026-09-29 — Phase 0 — decision log doc — 374a82f
- 2026-09-29 — Phase 0 — move prototypes to experiments — f21aeaf
- 2026-09-29 — Phase 1 — cargo build jobs config — 5b96ed1
- 2026-09-29 — Phase 1 — cargo workspace — f07f944
- 2026-09-29 — Phase 1 — lean dev builds — 2db67d4
- 2026-09-29 — Phase 1 — feature flags — 43f67d8
- 2026-09-29 — Phase 1 — smoke binary stub — 4542e56 — stub only; real open, play and export arrive with later phases
- 2026-09-29 — Phase 1 — reference media generator — 8074022 — 12 entries, 23 MB, 4 s on M2 (stands in for M1), all verified with ffprobe
- 2026-09-29 — Phase 1 — GitHub Actions matrix — needs-human: no GitHub remote yet; `.github/workflows/ci.yml` added in 8392c3c, push the repo and confirm the workflow runs green
- 2026-09-29 — Phase 1 — cargo-deny — a036b3f — `cargo deny check` passes locally (advisories, bans, licences, sources); CI job waits on the GitHub remote
- 2026-09-29 — Phase 1 — FFmpeg build script — 749514b
- 2026-09-29 — Phase 1 — local FFmpeg build — 1a04c31 — aarch64-apple-darwin on M2 (stands in for M1): 177 s with 4 jobs, prefix 32 MB (sources 17 MB); workspace checks and proto-decode build link against it, VideoToolbox decode and x265 Main10 work
- 2026-09-29 — Phase 1 — FFmpeg source packaging — 69df98b — 5 archives plus licences and build scripts, 16 MB
- 2026-09-29 — Phase 1 — FFmpeg CI job — needs-human: no GitHub remote yet; job added to `.github/workflows/ci.yml` in 8da1053, Windows (MSYS2 + MSVC) and Linux paths are untested until CI runs
- 2026-09-29 — Phase 0 — proto-ui rerun after review (submit time included) — 4b1620a — offscreen (screen locked, window occluded): UI-thread CPU median 0.37 ms, p99 0.74 ms (no culling 0.60 / 0.79), Queue::submit about 6 µs; windowed rerun still to do on an unlocked screen
- 2026-09-29 — Phase 0 — proto-decode rerun after review (true 3-frame pacing window) — 4b1620a — pinned FFmpeg 9.0.2, paced x1 2.06% of machine (8.58% + 7.88% of one core), 0 late; paced x2 1.81% per stream, 0 late
- 2026-09-29 — Phase 1 — cargo-dist packaging — 53a888f — cargo-dist 0.32 (installers: shell, powershell, npm, homebrew, msi; no DMG, MSIX or AppImage, so scripts/package/ adds them); local arm64 Glass.app 30.6 MB, DMG 10.5 MB (12.3 MB with the app icons), only @rpath and system references, starts and exits 0 from the mounted DMG, bundled FFmpeg dylibs load from it
- 2026-09-29 — Phase 1 — cargo-dist tagged release in CI — needs-human: no GitHub remote yet; Windows MSI/MSIX and Linux AppImage can only be built in CI (scripts in 1b919d9, workflows in 53a888f)
- 2026-09-29 — Phase 1 — app icons from the Brand page — 93de610 — icns, Icon Composer bundle and Assets.car (light, dark graphite, clear, tinted), Windows ico, Linux PNGs
- 2026-09-29 — Phase 1 — size check — a054f90 — `scripts/size-check.sh` against `size-budget.toml` (30 MiB per DMG, MSI, MSIX, AppImage): local arm64 DMG with icons 11.99 MiB passes, a 30 MiB + 1 byte fake fails with exit 1; the CI run is covered by the needs-human tagged-release task, and the MSI is not checked yet because dist builds it with no step after the build
- 2026-09-29 — Phase 1 — benchmark harness — 6577e1f — criterion 0.8.2 (no plots, no rayon); `engine` bench `enabled_features` 0.73 ns on M2 (stands in for M1) with `--quick`, baseline save and compare checked locally, report script output checked; real engine and UI benchmarks join as the code lands
- 2026-09-29 — Phase 1 — bench CI job with PR comment — needs-human: no GitHub remote yet; job added to `.github/workflows/ci.yml` in 6577e1f
- 2026-09-29 — Phase 1 — logging — d131061 — tracing 0.1.44, tracing-subscriber 0.3.23 in `engine::logging`; ring of 10,000 events: record 0.14 µs, full snapshot 0.77 ms on M2 (stands in for M1); tests for wraparound, level and target filters, 8 concurrent writers with a reader
- 2026-09-29 — Phase 1 — frame-time monitor — 78499a3 — egui, egui-wgpu 0.36.2 (wgpu 30.0.1), eframe 0.36 declared, `default_fonts` off; headless editor frame (200 clips, 2880 x 1496, Metal, M2 standing in for M1, debug build): p50 2.31 ms, p99 2.63 ms, 0 of 240 over 8 ms, test 0.73 s; record 3.9 ns, stats 5.1 µs; Geist fonts not bundled yet, the harness reads a system font at test time
- 2026-09-29 — Phase 1 — i18n — 3341ba9 — flat dotted keys in `assets/i18n/en.json` (embedded), `assets::t!` with named placeholders, `⟦key⟧` marker plus one warning for missing keys, `direction` flag per locale; `crates/app/tests/ui_source.rs` checks keys, placeholders and literal text in egui calls (5 fixture tests), run as its own step in ci.yml's check job
- 2026-09-29 — Phase 1 — theme — 3d6874a — `assets::theme` parses the embedded token files, `ui::theme` maps them onto egui `Visuals`, `Spacing` and text styles with run-time dark, light and high-contrast switching and `current(ctx).color(Role)`; literal-colour check in `ui_source` plus `check.py` in ci.yml; headless themed editor frame on M2 (stands in for M1), debug build, 240 frames: dark p50 2.49 / p99 2.91 ms, light 2.48 / 2.94 ms, high contrast 2.47 / 2.64 ms, 0 over 8 ms; Geist not in the repo yet (`assets/fonts/README.md` lists the files), so tests used the system fallback SFNS.ttf and SFNSMono.ttf
- 2026-09-29 — Phase 1 — crash minidumps — dd729fa — crash-handler 0.8.1 and minidumper 0.11.0 (out-of-process monitor, local socket and Mach port, no network code), dirs 7.0.0; off by default, on through `settings.json` or `GLASS_CRASH_REPORTS`; native crash writes `.dmp` (about 45 KB on M2) plus `.txt`, a panic writes `.txt` only, into the data folder's `crashes/`; `crates/app/tests/crash_reports.rs` crashes `glass` in a child process (on: dump and report appear, off and unset: nothing written), 4 tests in 1.9 s; unsafe confined to `app::ffi`; `cargo deny check` passes
- 2026-09-29 — Phase 1 — contributing session loop — 7c6763d — "Working on a task" in `CONTRIBUTING.md`: the session loop and three-commit rhythm for people, the full check, a per-change check table (deny, i18n and theme checks, frame budget, benches, fixtures, FFmpeg build, packaging and size check), decisions, splits and needs-human, concurrency and disk limits
- 2026-09-29 — Phase 1 — review fixes — 62a3608..420d50a — CI runners match dist plan (macos-15-intel), UI source check scans if/match blocks and more egui label APIs and palettes, frame budgets judged only on hardware GPUs (lavapipe on Linux, GPU required on macOS arm64), log ring cursor stops at unfilled slots, i18n placeholder warnings deduped and mismatched translations fall back to English, crash socket in a private 0700 folder, GLASS_DEBUG_CRASH debug-only, package-sources guards --out and scripts reject options without values
- 2026-09-29 — Phase 2 — hardware decode D3D11VA — needs-human: no Windows PC
- 2026-09-29 — Phase 2 — hardware decode NVDEC and VAAPI — needs-human: no NVIDIA or Linux GPU hardware
- 2026-09-29 — Phase 2 — safe ffmpeg wrapper — abd7bda — ffmpeg-next 9.0.0 and thiserror 2 in `media`; `Demuxer`, `Packet`, `Decoder` (send/receive, flush, EOF), `VideoFrame`/`AudioFrame` in native formats, `MediaError` with path, stream and FFmpeg code, `Rational`/`Timestamp`, FFmpeg log into `tracing` target `ffmpeg`; all unsafe in `media::ffi`; 11 unit and 6 fixture tests, `cargo deny check` passes
- 2026-09-29 — Phase 2 — probe — ed2e176 — `media::probe` → `MediaInfo`; VFR when the longest frame interval exceeds 1.5x the shortest over up to 2000 index timestamps (MOV/MP4) or 300 scanned packets, no decoding; every MANIFEST entry matches (codec, size, pixel format, rate, duration ±0.1 s, VFR, audio, rotation, timecode), new rotated 90° + `01:02:03:04` tmcd fixture checked by `--verify`; probe bench on M2 (stands in for M1): ProRes MOV 0.16 ms, H.264 1080p 3.7 ms, 4K 13.2 ms, VP9 WebM 4.0 ms, PNG 10.3 ms (FFmpeg's stream-info pass decodes a few H.264 and image frames); image-sequence frame rate deferred to the stills task
- 2026-09-29 — Phase 2 — software decode path — 3989227 — `media::FrameReader` (best video/audio or index, iterate, seek to start), native CPU planes and native audio sample formats, swscale `to_rgba8` for tests and thumbnails (frame 10 of the 1080p H.264 fixture identical to the FFmpeg CLI, mean and max difference 0); every fixture decodes with exact CFR frame counts, first pts 0 and exact PCM sample counts, stills and the PNG sequence included, except AV1: the pinned FFmpeg build has no libdav1d or libaom, so AV1 returns `NoDecoder` and its test skips; decode bench on M2 (stands in for M1), synthetic fixtures: H.264 4K 445 fps with a thread per core, 97 fps on one thread, H.264 1080p 1701 fps, HEVC 10-bit 1080p 1287 fps, ProRes 422 1080p 891 fps; smoke decodes 60 frames and 2 s of audio in 85 ms (debug)
- 2026-09-29 — Phase 2 — dav1d in the pinned FFmpeg build — 537f8fa — dav1d 1.5.4 (meson 1.12.1, ninja 1.13.2 as host tools), full FFmpeg rebuild 191 s with 4 jobs on M2 (stands in for M1), prefix 32 MB to 32.8 MB (libdav1d 0.9 MB); the AV1 fixture decodes (60 frames) and its test no longer skips
- 2026-09-29 — Phase 2 — hardware decode VideoToolbox — 491ad6e, 7543cda — `DecoderOptions::hw`, `HwFrame` (retained CVPixelBuffer/IOSurface, NV12/P010 and 4:2:2/4:4:4 siblings), `render::interop::SurfaceImporter` (IOSurface planes as Metal textures via wgpu-hal, cached per surface) and `render::yuv::YuvConverter` (Rgba16Float or Rgba8Unorm, encoded R'G'B'); on M2 (stands in for M1): frame 10 of 4K H.264 and 1080p 10-bit HEVC match the swscale software decode (mean 0.000 to 0.004, max 1 level); 4K H.264 VideoToolbox decode 110 fps, with zero-copy import and conversion 99 fps (7 surface wraps for 240 frames), software decode 478 fps with a thread per core (synthetic clip)
- 2026-09-29 — Phase 2 — automatic software fallback — 5ff42c9 — `HwPreference::Auto` falls back per file at open (VP9 and AV1 on M2: no VideoToolbox decoder), on the first frame (RGB-coded H.264, which FFmpeg never offers to VideoToolbox) and mid-stream (injected failure after 20 frames: software resumes after 0:00.63, all 90 frames once and in order); `Require` errors instead; `FallbackReason::message()` from `decode.fallback.*` in `assets/i18n/en.json` is logged and exposed through `FrameReader::decode_path()` and `engine::media_check` for the UI, which shows it once a clip panel exists; on M2 (stands in for M1) 10-bit and 4:2:2 H.264 decode in hardware
- 2026-09-29 — Phase 2 — colour space plumbing — 56aebc5 — `media::ColourSpace` on every CPU and hardware frame and in probe (untagged: HD BT.709 limited left-sited, SD BT.601 by line count, RGB/PNG sRGB full, JPEG BT.601 full sRGB centred; PQ/HLG/BT.2020 tags pull the matrix to BT.2020); `YuvConverter` honours chroma siting and writes linear light (BT.1886, sRGB, 2.2/2.8, linear, PQ absolute, HLG with the 1000-nit OOTF, 1.0 = 203 nits) or encoded R'G'B', 8 to 16-bit planar and biplanar, plus `render::upload` for software frames; GPU vs swscale with matching siting on M2 (stands in for M1): mean 0.000 to 0.008, max 1 level over 7 fixtures (centred sampling instead: mean up to 0.63, max 129); PQ/HLG 10-bit HEVC and full-range H.264 bar fixtures: PQ 58% 0.99, PQ 100% 49.25, HLG 75% 1.00, HLG 100% 4.93, full-range 75% 0.500 in software and VideoToolbox; primaries conversion deferred to the render graph (Phase 4)
- 2026-09-29 — Phase 2 — VFR normalisation — 3c94351 — `media::timemap`: `SourceTimeline` per file (closed form for constant rate; VFR presentation times from the MP4 index when frames are not reordered, else a packet scan, no decode; serde for the disk cache), `TimestampMap` on the sequence rate with the nearest-earlier-frame rule (`len = ceil(duration x rate)`), inverse lookups, in-memory `TimelineCache`; exact indices on the VFR fixtures (packet scan and index paths, 5 sequence rates, scanned times equal decoded times) and on 23.976 in 25, 30 in 24, 60 in 30 and 25 in 25; proptest: maps total, monotonic and nearest-earlier; new fixtures: 60 fps B-frame 120-frame open GOP H.264, VFR H.264 without B-frames; proptest 1 added as a dev-dependency (`cargo deny check` passes)
- 2026-09-29 — Phase 2 — accurate seek — 3ad7cbd — `FrameReader::seek_exact` / `seek_to_sequence_frame` (last keyframe at or before the target, decode forward, nearest-earlier frame, forward seeks inside a GOP skip the seek; a drained VideoToolbox decoder is reopened because FFmpeg's VT HEVC path repeats the last drained frame after a flush); `GopIndex` from the MP4 index or a packet scan, cached as `<cache>/analysis/<content hash>/gop-*.json`; `ContentHash` = BLAKE3 of size + first/last 1 MiB + 8 x 64 KiB (no mtime, Decision Log) and `CacheDir`; 50 random seeks per video fixture match a linear decode's plane checksums (14 fixtures software, 12 VideoToolbox), plus after EOF, backwards, between frames, before start and via a timestamp map; seek bench on M2 (stands in for M1), 200 random seeks, median / p95: software 1080p H.264 24.7 / 36.6 ms, 720p60 120-frame open GOP 15.9 / 41.3 ms, 4K 58.3 / 130 ms; VideoToolbox 1080p 116 / 206 ms, 720p60 129 / 409 ms, 4K 209 / 544 ms (FFmpeg's synchronous hwaccel, about 4.5 ms per 1080p frame), so scrubbing should decode in software until the Phase 3 decoder work
- 2026-09-29 — Phase 2 — still images, sequences, GIF, WebP, HEIC — 2534b88 — `media::source::VideoSource`: one API for video, stills, animations and sequences, files recognised by content; stills (`image` 0.25 with png, jpeg, gif, webp, tiff, bmp only; HEIC/HEIF/AVIF via ImageIO on macOS, SDR primary image, gain maps ignored) have a `FrameTimes::Still` timeline (length from the project); animated GIF and WebP (`gif`, `image-webp` directly: `Send`) use millisecond VFR timelines from frame headers, delays of 10 ms or less play as 100 ms; sequences run at the caller's rate (project rate by default) with gaps held and reported; frames are planar RGB `gbrp`/`gbrap` (16-bit when deeper), PNG `cICP` and ICC (`cicp` tag or colorants) recognise Display P3 and BT.2020, others flagged; EXIF orientation applied to pixels; `render` reads planar RGB (GPU vs swscale max 1 level on 6 still fixtures); 11 new fixtures (TIFF, 16-bit alpha PNG, BMP, cICP P3 PNG, EXIF-6 JPEG, VFR GIF, animated and still WebP, P3 HEIC and AVIF via sips, gap sequence); Windows HEIC via WIC is needs-human; `cargo deny check` passes
- 2026-09-29 — Phase 2 — audio decode and resampling — af2df25 — `media::AudioReader`: planar f32 at the requested rate and layout (mono, stereo, 5.1; BS.775 downmix, mono to both channels) into caller buffers, 0 heap allocations per read after warm-up (counting allocator); FFmpeg decodes only, rubato 5 `Async::new_sinc` resamples (256 taps, BlackmanHarris2, auto cutoff, oversampling 256, cubic): on M2 (stands in for M1) SNR 135 dB (48↔44.1 kHz) and 145 dB (96→48 kHz), level within 0.0001 dB, 30 kHz alias -139.5 dB, latency 117/139/64 output frames trimmed; sample n at n/rate on the file timeline (priming handled through pts, WebM pts bias removed); counts exact (±1) at 48 and 44.1 kHz on all 11 audio fixtures; seeks match a linear read (PCM exact, AAC 4.4e-4, Opus 1.4e-4); A/V sync markers (AAC 48 and 44.1 kHz, Opus, PCM) offset 0.000 ms, worst -0.021 ms (44.1→48 kHz); throughput one thread: AAC 1,630x realtime passthrough, 133x resampled, MP3 44.1→48 kHz 124x; smoke decodes 2 s at 48 kHz f32 through it; `cargo deny check` passes
- 2026-09-29 — Phase 2 — analysis job system — d1ad1a7 — `engine::analysis`: `AnalysisJob` trait (kind, version, params, `run(ctx)`, `Artifact` output), `glass-analysis-<n>` pool sized cores minus 5 reserved, 1 to 3 workers (3 on M2, standing in for M1; 8 GB cap), priority queue (visible, selected, background, FIFO within) with re-prioritisation, dedup by key, cooperative per-job cancellation, pause and resume (running jobs park at checkpoints), progress on a non-blocking mpsc channel (queued, started, 1% steps, partial, finished), disk cache `<cache>/analysis/<hash>/<kind>-<params hash>-v<version>.<ext>` via `media::CacheEntry` (hit does no work, version bump misses and deletes old files); `TimelineCache` now keyed by content hash with disk backing, shared with `TimelineJob`; 9 pool tests (stress: 8 threads x 200 submits with cancels, moves and pauses) and 1 fixture job test
- 2026-09-29 — Phase 2 — thumbnail strips — 2a7386c — `media::analysis::thumbnails` + `engine::analysis::ThumbnailJob`: levels one thumbnail per 1, 5 and 30 s (`max(1, ceil(duration / interval))`, one for stills), 108 px tall (54 pt at 2x), width by display aspect after SAR and rotation; keyframe shown when within `min(interval/2, 1 s)` before the target (`VideoSource::seek_keyframe`), else `seek_exact`; coarse level first (`Partial(2, 1, 0)`), shared thumbnails decoded once; CPU swscale straight to thumbnail size (`to_rgba8_scaled`), 2 decode threads, JPEG q80 atlases of 64 (PNG with alpha), about 4 KB per thumbnail on the fixtures, 17 to 35 MB per hour for all levels; bench on M2 (stands in for M1), release: 1080p H.264 all levels 67 ms (coarse level after 37 ms, then 15 ms per 1 s thumbnail, 54 s per hour of footage), 4K 146 ms (coarse after 104 ms, then 42 ms per thumbnail, 152 s per hour); found: `FrameReader::seek_exact` seeks by the keyframe dts, which lands one GOP early in B-frame MP4s (index times are pts-like), so every exact seek there decodes an extra GOP (not changed here); new rotated marked fixture; sequences hash by folder (`ContentHash::of_folder`)
- 2026-09-29 — Phase 2 — waveform peaks — 0292c70 — `media::analysis::peaks` + `engine::analysis::PeaksJob`: min, max and RMS per channel at 256, 1024, 4096, 16384 and 65536 samples per peak (48 kHz project rate by default, source layout mono, stereo or 5.1), a factor-4 mipmap built in one pass from `AudioReader` (coarse min and max exactly those of the finer peaks, RMS from summed squares), i16 quantisation (to -90 dBFS) in one `peaks-<params>-v1.bin` with header and level table, `peaks(level, sample_range)` and `level_for(samples_per_pixel)`; sine and click file: crest 16384 ±1, click 0.9 at the right peak on every level, RMS within 2 LSB; bench on M2 (stands in for M1), release, one thread: 48 kHz s16 stereo WAV 3.8 s per hour of audio (937x realtime), 44.1 kHz resampled 30 s per hour (120x), AAC 13 s per hour (274x, 3 s file); 10.8 MB per hour of stereo
- 2026-09-29 — Phase 2 — seek landing fix (accurate seek follow-up) — 2d10ad5 — `FrameReader::seek_exact`/`seek_keyframe` seek the demuxer by the keyframe's presentation time (FFmpeg's MP4 demuxer converts pts to its decode-time index itself; seeking by dts landed one GOP early on B-frame MP4s), drop packets before the keyframe, re-seek by dts then from the start if a demuxer lands late; `seek_keyframe` uses the GOP index too; 50-random-seek checksum tests still pass on every fixture (software and VideoToolbox); seek bench on M2 (stands in for M1), 200 random seeks, median / p95 before → after: software 1080p 25.6 / 36.8 → 13.6 / 22.5 ms, 720p60 open GOP 15.4 / 41.6 → 10.9 / 24.8 ms, 4K 58.3 / 127.1 → 39.5 / 74.8 ms; VideoToolbox 1080p 119.5 / 219.7 → 51.5 / 110.8 ms, 720p60 132.6 / 362.9 → 89.5 / 219.7 ms, 4K 205.5 / 535.0 → 121.8 / 279.3 ms
- 2026-09-29 — Phase 2 — scene cut detection — b153148 — `media::analysis::scenes` + `engine::analysis::SceneJob` (`scenes-<p>-v1.json`): every frame decoded in software, a 96 px copy via `to_rgba8_scaled`, difference = luma histogram + chroma-weighted hue histogram + 8x8 block means, cut when above `max(floor, median + k·1.4826·MAD)` over ±1 s and 2.5x its neighbours (sensitivity 0 to 1), flashes dropped when frames within 0.15 s either side match, a fade through black is one `Fade` cut mid-run (other fades and dissolves: none), 500 ms minimum scene; new 12 s fixture `scenes_cuts_640x360_30.mp4` (cuts at 60, 120, 180, 300, 2-frame flash at 90, fade through black at 240): all cuts exact at sensitivity 0, 0.5 and 1, no cut on the flash, fade at 240, no cuts on four continuous-motion clips; bench on M2 (stands in for M1), release, run while other agents compiled: 1080p H.264 426 fps (14.2x realtime) with the job's 2 decoder threads, 817 fps (27x) with all cores; 4K 95 fps (3.2x) / 123 fps (4.1x); 640x360 1689 fps (56x)
- 2026-09-29 — Phase 4 — render graph — acde2f0 — `render::graph` (`Graph` of `Node`s wired by `Port`s, imports and exports, Kahn topological compile taking the earliest-added ready node, cycle and unconnected-input errors, culling, `TextureDesc` derivation, aliasing of `Rgba16Float` intermediates into pooled slots by lifetime; `TexturePool` reuses textures across frames), `render::renderer::Renderer` (lazy effect pipelines from `crates/render/shaders/*.wgsl`), `render::profiler::GpuProfiler` (per-pass begin/end timestamps, 3-buffer readback ring, non-blocking `poll`), `YuvSourceNode` (Y'CbCr to linear with the gamut matrix folded in) and `GamutNode` (`render::colour::gamut_matrix`: BT.709, BT.601 625/525, BT.2020, DCI-P3 with Bradford, Display P3; BT.709 to BT.2020 within 6e-4 of BT.2087); five 4K layers unfused: 18 intermediates, 1194 MB, aliased into 3 textures, 199 MB (995 MB saved); M2 stands in for M1
- 2026-09-29 — Phase 5 — immutable edit tree — 38232a7 — `imbl` 7 (not `im`: RUSTSEC unmaintained + unsound), exact rational `Time`, u64 ids per project, pure validated edits (insert, remove, move, trim, tracks, sequences, media); after moving one of 200 clips, the other 199 items, the other tracks and sequences and the media map are the same allocations (`Arc::ptr_eq`)
- 2026-09-29 — Phase 5 — undo and redo — 67c58fc — `project::History`: roots list, gestures (begin/update/end/cancel), coalescing by key, `EditLabel` with `edit.label.*` strings; 100,000 steps on a 1000-clip project on M2 (stands in for M1): 3.9 KB live heap per step (a fresh tree is 92 KB), 391 MB in all, 10 s debug; undo and redo to either end allocate nothing
- 2026-09-29 — Phase 2 — proxy generation — 6cd6900 — `media::Encoder`/`FrameScaler`/`Muxer` (minimal safe encode and mux, extension points for Phase 8 in ARCHITECTURE), `media::analysis::proxy` + `engine::analysis::ProxyJob` (cache area `proxies/<hash>/`): ProRes 422 Proxy by default on macOS (`prores_videotoolbox`, else `prores_ks` profile 0), H.264 elsewhere (`h264_videotoolbox` on macOS, else x264 veryfast CRF 23; 1 s GOP, no B-frames), `.mov` with PCM audio (16-bit, 24-bit for deeper sources), timecode, display matrix and colour tags (resolved) kept; size: ProRes halves above 720 lines capped at 1280 wide (UHD → 1280x720, 1080p → 960x540), H.264 fits 1280x720; resumable ~10 s keyframe-aligned segments with a manifest, concatenated by stream copy into one file; tests: 1080p/4K size, codec, profile, frame count, pts, duration, frame-accurate (64x36 mean difference under 4 levels and closer than neighbouring frames), PCM sample count, timecode 01:02:03:04, rotation 90, PQ tags, H.264 software and VideoToolbox, cancel after 2 segments then resume encodes only the rest with the same length; bench on M2 (stands in for M1), release, 2 s 4K H.264 → 1280x720: ProRes VideoToolbox 130 fps (4.3x realtime), prores_ks 107 fps (3.6x), H.264 VideoToolbox 197 fps (6.6x), x264 235 fps (7.8x); 1080p → proxy: 295, 226, 274, 515 fps
- 2026-09-29 — Phase 2 — import — 530f759 — `engine::media_mgmt`: scan on a `glass-import` thread (recursive walk, hidden files, sidecars and camera indexes skipped, numbered stills of 8+ files grouped as sequences, Photoshop/raw/documents reported), camera cards read as cameras (AVCHD `PRIVATE/AVCHD/BDMV/STREAM` with FAT32-split files joined, XDCAM `XDROOT/Clip` with `_01` continuations, Sony XAVC `PRIVATE/M4ROOT/CLIP` + NonRealTimeMeta XML, Canon `DCIM/100CANON` and Cinema `CONTENTS/CLIPS001` + XML, DJI `DCIM/DJI_*`/`100MEDIA` + SRT telemetry), `Importer` probes, hashes (`ContentHash`, duplicates skipped by path and hash), reads sidecars into `project::media_pool::Metadata`, streams progress and cancels; `AfterImport` hook is the scene-split extension point (no scene detection on goal/phase-2 yet); `ui::panels` drop area (empty, drag over, unreadable), folder import dialog and progress strip from `Sheet / Import`, asynchronous `rfd` 0.17 picker (`cargo deny check` passes); tests: cards synthesised from fixtures in a temp dir (13 clips, spans, sidecars, dedup, cancel); headless import surfaces on M2 (stands in for M1), debug build: p50 0.89 / p99 1.20 ms, 0 of 240 over 8 ms
- 2026-09-29 — Phase 16 — UI snapshot tests — 0e8d803 — `ui::snapshot` registry (explicit list in `crates/ui/src/snapshot/cases.rs`) and `crates/ui/tests/ui_snapshots.rs`: every case rendered headless (`HeadlessUi::capture`, Metal) in dark, light and high contrast at 1x and 2x against `crates/ui/tests/snapshots/<case>/<theme>@<scale>x.png`; tolerance 16 levels per channel on at most 0.002% of pixels on GPUs (one changed letter moves 15 px at 1x, 56 at 2x, and fails), 48 levels and 0.1% on software adapters; `.actual.png`/`.diff.png` on failure, `GLASS_UPDATE_SNAPSHOTS=1` rewrites; fonts pinned to Arial and Courier New (macOS-only goldens until Geist is bundled, fingerprint in `FONTS.txt`); 2 cases (theme showcase, sample timeline), 12 goldens, 372 KB; CI step on macOS arm64; later panels, dialogs and params add their cases through the registry check, which fails for any module under `panels/`, `dialogs/` or `params/` without one
- 2026-09-29 — Session paused — see docs/HANDOFF.md for state, needs-human items, follow-ups, and how to resume
- 2026-09-30 — Phase 2 — silence detection — 6e47138 — `media::analysis::silence` + `engine::analysis::SilenceJob` (`silence-<p>-v1.json`): 10 ms RMS sliding by 1 ms, every channel below threshold (-40 dBFS), 3 dB hysteresis, 500 ms minimum, optional padding, ranges in samples on the file timeline; fixture edges within 3 ms in WAV and 10 ms in AAC (measured 2.0 ms both), cache hit does no work; release bench on M2 (stands in for M1, machine shared): AAC 1,518x realtime, WAV 216x (open cost dominates a 10 s file); fixture patch adds silence WAV/M4A, six 16-bit FLAC beat tracks and three loudness files with ebur128 references
- 2026-09-30 — Phase 2 — loudness analysis — fa88524 — `media::analysis::loudness` + `engine::analysis::LoudnessJob` (`loudness-<p>-v1.json`) on the `ebur128` crate: integrated, LRA, true and sample peak, short-term and momentary curves every 100 ms, source rate and layout; vs FFmpeg's ebur128 filter: I -26.08/-26.17/-20.84 LUFS vs -26.1/-26.2/-20.8 (48 kHz WAV, 44.1 kHz FLAC, 5.1), TP within 0.1 dB, LRA 0.3 to 0.4 LU below FFmpeg's (tolerance 0.5); Tech 3341 cases 1-5 within 0.1 LU, Tech 3342 cases 1-4 within 1 LU; release bench on M2 (stands in for M1, machine shared): stereo WAV 191x realtime, 44.1 kHz FLAC 225x, 5.1 WAV 60x (true-peak oversampling dominates); `AudioReader::open_for_source` gives silence and loudness one open per file: silence WAV 46 → 25 ms (216x → 394x), AAC 6.6 → 6.3 ms (1,599x)
- 2026-09-30 — Phase 2 — beat and tempo detection — f4d071d — `media::analysis::beats` + `engine::analysis::BeatsJob` (`beats-<p>-v1.json`): three-band normalised spectral flux (so kicks outweigh noise hats and match snares), autocorrelation tempo with the half-period rule, confidence (autocovariance at the period: 0.75 to 1.00 steady, 0.29 on the drift, under 0.2 for noise), Ellis DP beats; latency calibrated on the click fixtures' 76 known clicks: flux peaks 2.99 ms (0.140 window) early, residual mean under 0.1 ms, sd 0.5 ms; all six fixtures within 1 BPM (drift 119.93 vs mean 119.90) and every beat 1-19 s within 10 ms (worst 3.1 ms, 174 BPM snares), no extras; release bench on M2 (stands in for M1, machine shared): 20 s FLAC 1,125-1,204x realtime, stereo WAV 343x, AAC 891x; `realfft` 3.5 added (`cargo deny check` passes)
- 2026-09-30 — Phase 4 — shader hot reload — e3385a9 — debug builds: `render::shaders::ShaderLibrary` reads `crates/render/shaders/`, a `shader-watch` thread (notify 8, FSEvents) validates edits with naga 30 and publishes them, `Renderer::begin_frame` (from `CompiledGraph::record`) drops the reloaded shader's pipelines (all for `common`); broken edits keep the last good source and pipeline with naga's error logged; release builds embed and never watch; FSEvents registration took 13 s on the loaded M2, so it runs on the watch thread and never blocks `Renderer::new`; cargo deny ok (new duplicate-version warning: windows-sys)
- 2026-09-30 — Phase 4 — blend modes — b0ccd57 — `render::blend` (11 modes, W3C Compositing Level 1 source-over on premultiplied linear light, HDR-safe screen, dodge and burn; f64 CPU reference) and `nodes::CompositeNode` (up to 8 layers over an optional background per pass, `composite_stack` chains more); GPU vs CPU over 1024 backdrop/layer combinations (alpha 0, 0.25, 0.5, 1, levels to 1.5): max 0.00098 per mode, add 0.00195 (one f16 step above 2), tolerance 2e-3 x max(1, |v|); transparent layers, no background and a smaller layer exact; 12 layers in 2 passes max relative error 0.0019; five 4K noise layers in one pass: GPU median 5.46 ms, p99 6.23 ms (about 85 GB/s, near the M2 bandwidth), machine shared with other agents, M2 stands in for M1
- 2026-09-30 — Phase 4 — transform node — 1c9d7dc — `render::transform` (`Affine2`, `Transform2D` position/non-uniform scale/rotation/anchor, `ShutterTransforms` up to 16 sub-frame samples, middle one rendered until Phase 13; `Resample` nearest/bilinear/Catmull-Rom bicubic with an f64 CPU reference) sampled inside `CompositeNode`'s pass (no transform node, no intermediate per layer), nearest whenever pixel centres map onto texel centres, one entry point per filter, per-pass cap `with_max_resample`; identity, whole-pixel moves, quarter turns and flips exact, free transforms vs CPU max 0.0027 bicubic / 0.0021 bilinear; five 4K layers in one pass, GPU median / p99: untransformed 5.05 / 5.08 ms (blend-modes baseline 5.46), transformed bilinear 5.03 / 5.10 ms, transformed bicubic 10.26 / 10.31 ms (texture-rate bound, for paused frames and export); machine shared with other agents, M2 stands in for M1
- 2026-09-30 — Phase 4 — opacity and crop — aad5d14 — `nodes::Layer::{opacity, crop}` and `render::crop` (per-edge crop in layer pixels before the transform, feather ramping each edge inward by a smoothstep, hard edges box-filtered over the pixel footprint; f64 CPU reference) in the composite pass; whole-pixel crops exact, feather ramp within 1e-3 of `crop::edge`, cropped rotated scaled feathered layers at 80% vs CPU max 0.0027, opacity 0 and 1 bit-exact and 0.25/0.5/0.8 within 5e-3 for all 11 blend modes; five transformed, cropped (90 to 330 px per edge, 40 px feather, 85% opacity) 4K layers in one pass, GPU median / p99: bilinear 4.82 / 6.47 ms, bicubic 9.36 / 9.56 ms, vs untransformed 5.32 / 6.72 ms in the same run (blend-modes baseline 5.46); machine shared with other agents, M2 stands in for M1
- 2026-09-30 — Phase 2 — metadata panel — d41b589, e1bb032 — `engine::media_mgmt::tags` maps container tags into `Metadata` on import (QuickTime `com.apple.quicktime.make/model/camera.lens_model/location.ISO6709/creationdate`, MP4 `location` and `date`, Android make/model, `creation_time`, timecode track), `media::exif` reads still EXIF (JPEG, PNG, WebP, TIFF via `image`: make, model, lens, date with offset, focal length, f-number, ISO, exposure, GPS); order sidecars > container > EXIF, dates normalised to ISO 8601 (epoch placeholders dropped), timecode to `HH:MM:SS:FF`/`;FF`; ISO 6709 in degree, DDMM and DDMMSS forms; no `Metadata` fields added; `ui::panels::metadata` from `Sheet / Media pool` (three columns, muted mono dash for missing values, `Edit…` action), 2 snapshot cases (12 goldens); 2 tagged fixtures (QuickTime mdta MOV, MP4 location) with tag checks in `--verify` (50 entries pass); headless metadata panel on M2 (stands in for M1), debug, shared machine: p50 0.68 / p99 0.75 ms, 0 of 240 over 8 ms
- 2026-09-30 — Phase 4 — linear-light output — bbea099 — `render::nodes::OutputNode` (gamut into the output primaries, straight alpha by default, `OutputTransfer` sRGB / inverse BT.1886 / BT.709 OETF / PQ / HLG / linear, TPDF dither of one code for 8, 10 and 16-bit targets, per-frame seed): GPU vs CPU reference within 5e-4 (f16) and 0.501 codes (8 and 10-bit); PQ 100/203/1000/10,000 nits and HLG 75%/100% spot values within f16 precision; gradient banding in 321 px steps undithered, longest dithered run 38 px (10-bit 37), 16x16-blur RMS 0.268 to 0.033 codes, mean error 0.0001 codes; YUV, planar RGB still (P3) and BT.2020 uploads gamut-converted into the working space within 1e-3 relative; 4K output pass GPU median 2.44 ms (8-bit sRGB), 2.86 ms (10-bit PQ), float copy 1.09 ms, machine shared with other agents, M2 stands in for M1
- 2026-09-30 — Phase 3 — frame queues — 248ae1f — `engine::frame_queue`: `spsc` (`rtrb` 0.4, render → present, peek and `pop_latest_due` present policy: latest due frame shown, earlier due frames dropped as late, hold the current frame when none is due) and `mpsc` (crossbeam `ArrayQueue`, one producer clone per decoder, order per producer) carrying `Stamped { FrameStamp { generation, index, pts }, frame }` (`DecodedFrame::{Cpu, Hw}`, rendered `wgpu::Texture`); shared `Generation::advance` flushes stale frames with no lock on push or pop (producers get `Stale`, consumers drop and count) and releases producers blocked on a full queue; decode blocks (parks) when full, render only `try_push`es to present; waiting parks (crossbeam `Parker`, `SeqCst` flag and recheck), never spins; 0 allocations per push, pop, wait, wake and advance after warm-up (counting allocator, single thread and cross-thread); 12 tests incl. 200k-frame SPSC and 4-producer x 50k MPSC stress with 40 seeks (loom skipped: the rings are not loom-instrumented); release bench on M2 (stands in for M1), machine shared with other agents: push+pop same thread spsc 10.9 ns, mpsc 18.7 ns; cross-thread with parking, 4-slot queue: spsc 1.08 µs per frame (0.92 M frames/s), mpsc 2 producers 0.35 µs per frame (2.9 M frames/s); `cargo deny check` passes
- 2026-09-30 — Phase 5 — autosave — 6fcc66f — `project::autosave` (node-id diffs, generation snapshots every 30 records and on save, append-only `sync_data` log, `session.json` unclean-exit marker, own format version, recovery skips a torn tail and falls back a generation), `engine::autosave` `glass-io` thread (10 s, only when the tree changed), `app::recovery` launch scan, `ui::dialogs::recovery`; simulated crash restores the last autosave; 1000 clips (M2 standing in for M1, debug, machine shared with other agents): snapshot 94.8 KB, one-move diff 196 B (0.21%), first snapshot 72 to 108 ms, tick with one change median 34 ms max 61 ms on `glass-io`, submit from the UI at most 2.6 µs, recover 20 records 40 ms; recovery dialog p99 3.3 to 4.1 ms
- 2026-09-30 — Phase 4 — readback ring — b1941a0 — `render::readback::ReadbackRing`: N staging buffers (`EXPORT` 4 lossless, `SCOPES` 3 drop-oldest), copy + `map_buffer_on_submit` in the caller's encoder, `PollType::Poll` delivery in submission order with id, timestamp and submit instant, 256-byte row padding, `Rgba16Float`/`Rgba8Unorm(Srgb)`/`Bgra8Unorm(Srgb)`/`Rgb10a2Unorm`/`Rgba16Unorm` byte-exact round trips, `to_rgba8`/`to_rgba16` packing; lossless refuses when full (60 of 60 frames delivered under a slow consumer, 109 refusals, 0 dropped), drop-oldest keeps the newest 3 of 10; 4K `Rgba16Float` record + submit 0.14 ms median, 0.45 ms worst vs 3.2 ms blocking; 4K latency 2 frames median (p95 3); throughput copied out `Rgba16Float` 190 to 300 fps, `Rgba8Unorm` 460 fps, in place 700 to 1100 fps (debug build, machine shared with other agents, M2 stands in for M1)
- 2026-09-30 — Phase 1 — cargo-serial fingerprint fix — d3e73af — `scripts/cargo-serial` records the last checkout in `<git common dir>/glass-cargo-last-checkout` and, when the checkout changes, deletes the workspace packages' fingerprints under every profile of the shared target directory before cargo runs (dependencies kept); a two-worktree test of the `assets` crate with a different public function in each ran the other worktree's build before the fix and recompiled the right sources after it; CONTRIBUTING and Decision 16 corrected, Decision 28
- 2026-09-30 — Phase 2 — integration fix (check #2) — b3f276f — `recovery_restoring` goldens (autosave merge) failed on goal/phase-2 after the metadata-panel merge added snapshot cases: the snapshot test reuses one egui context whose input time grew 1/60 s per frame across every earlier case, and the recovery spinner's angle comes from `i.time`; `HeadlessUi::set_time` pins the clock, the snapshot test pins it at 0, and only the six recovery_restoring goldens changed
- 2026-09-30 — Phase 4 — text rendering — aa207c7 — `render::text`: cosmic-text 0.19 layout (bidi, fallback, system fonts; tests pin Arial), `R8Unorm` coverage glyph atlas (shelf packing, 512 to 4096 doubling, evict-unused-this-frame and GPU repack), swash vector outline strokes, separable Gaussian drop shadow, per-glyph offset/scale/rotation/opacity hook; GPU vs CPU coverage max 0.00049 (tolerance 1/255 + 1/2048), mean 0.000007; paragraph layout ~4,700 glyphs/ms, warm atlas prepare ~19,600 glyphs/ms (346 glyphs, release); full-screen 4K title with outline and blur-8 shadow ~7.2 ms GPU (shadow blur most of it); machine shared with other agents, M2 stands in for M1
- 2026-09-30 — Phase 2 — media pool — 301a5de, a97bf9a, 9345a1e — `project::media_pool::MediaPool` (imbl `Arc` nodes keyed by `MediaId`, nested `BinId` bins, labels, tags; labelling 1 of 10,000 shares the other 9,999 items) and `SearchIndex` (prefix, exact, typo, `type:`/`label:`/`tag:`/`is:offline`, incremental `refresh` by persistent-map diff); bench on M2 (stands in for M1), release, machine shared with other agents: 10,000 items build 62 ms, search 0.1 to 0.7 ms, refresh after one label edit 46 µs, set label 0.5 µs; `ui::panels::media_pool` grid and list with bins tree, smart bins, labels, tag chips, proxy/offline/analysing badges, metadata strip, label menu, 3 snapshot cases; 3,000 clips scrolling, debug: grid p99 5.96 ms, list p99 7.09 ms (a new result set's frame 73 to 117 ms debug, release max 2.4 ms)
- 2026-10-01 — Phase 3 — frame cache — 3145ed9 — `render::frame_cache::FrameCache`: (segment hash, frame, render variant) key, one LRU order across VRAM and RAM, byte ceilings settable at runtime (defaults 1/6 GPU working set, 1/8 RAM, 1/16 on unified memory), non-blocking lossless spill through the readback ring, bit-exact `Native` and half-size `Srgb8` spill, re-upload on RAM hit, segment invalidation incl. spills in flight, hit/miss/eviction stats; 7 tests (eviction order, 300-step random ceiling fuzz, round trips); insert 1.9 µs, VRAM lookup 1.3 µs (2000 frames), 4K spill record + submit 0.22 ms, pack 1.8 ms, re-upload CPU 7.3 ms (Srgb8 4.1 ms); debug build, machine shared with other agents, M2 stands in for M1
- 2026-09-30 — Phase 4 — effect parameter system — b00efe0 — `project::params` (ParamSchema/ParamDef/ParamKind float, int, bool, choice, colour, point, angle, text; hard and soft ranges, defaults, units, keyframeable flag, header switch, slider hint; ParamValues serde map, clamped on read and write) with built-in transform, compositing, crop and motion blur schemas; `ui::params::Section` generates the inspector rows from `Sheet / Inspector video` and `Sheet / Controls` (scrub, typed maths, stepper, switch, dropdown, slider, keyframe lane, reset) and returns events; 33 parameters headless on M2 (stands in for M1, machine shared with other agents), debug build: p50 1.89 / p99 2.02 ms, 0 of 240 over 8 ms; 4 snapshot cases, 24 goldens; blend choice ids checked against `render::blend`
- 2026-09-30 — Phase 2 — offline media slate — 27d5124 — `engine::media_mgmt::offline`: `check_project` marks each `MediaRef` offline as missing, unreadable (system message) or changed (content hash), `Verify::Quick` stat and open only, a `MediaAvailability` map beside the project (tree untouched), `offline_clips` with timeline ranges, `slate_label` from `media.offline.*`; `render::slate`: the offline slate from the error and warning states sheet (glows through a 5 px dot screen, glass card with shadow, highlight and edge, wifi-off icon, title, mono file name shortened in the middle, detail) at any size, dark theme roles in linear light, `SlateCache` renders once per size and label and frames import it; GPU vs CPU reference max 0.00049 at 640x360, 1920x1080 and 1080x1920, texts in their boxes at their colours (brightest 0.847/0.456/0.275 vs text/primary, secondary, muted), 30 imported frames no re-render; project with offline media round-trips byte-identically through load, move, trim, delete, undo and redo; M2 stands in for M1
- 2026-10-01 — Phase 5 — versioned schema — 53a0e2c — `project::schema`: file envelope (`format`, `schema_version`, `project`), `save` writes v2 (pretty JSON, stable key order), `load` refuses newer and older-than-supported versions (i18n `project.open_error.*`), runs one pure `serde_json::Value` migration per step (`migrate_v1_to_v2` wraps `media` in `{"refs": [...]}`), then deserialises and validates; fixtures `tests/fixtures/projects/v1-basic.json` and `v2-basic.json`, 8 schema tests (every prior version migrates to the current project, current fixture equals `save` output, save-load identity, newer file refused, damaged and non-project files, migrated tree validated) plus 2 unit tests
- 2026-10-01 — Phase 17 — about dialog — 76bd431 — `ui::dialogs::about` after `Dialog / About` with the Brand page's graphite app tile drawn from the mark SVG: version, build date and commit (`app/build.rs`), this copy, MIT and GPLv3 notices one level in, filterable third-party list (6 native libraries plus the crates `app` ships, `assets/licences/third-party.tsv` from `scripts/licences/third-party.py`, checked against Cargo.lock), source archives link `<repository>/releases/tag/v<version>`, Copy details, `glass --version`; installers gather crate licence texts into `LICENSES/third-party-crates.txt`; 3 snapshot cases, 18 goldens; every view headless on M2 (stands in for M1, machine shared with other agents), debug build: p50 0.60 / p99 6.4 ms, 0 of 240 over 8 ms
- 2026-10-01 — Phase 4 — vector shapes — 72c31d4 — `render::shape`: rectangle (corner radius), ellipse, polygon and `Path` (lines, quadratic and cubic Béziers) under an `Affine`, fill (non-zero, even-odd) and stroke (width, miter/round/bevel joins, butt/square/round caps, dashes), solid, linear and radial gradient paint; curves flattened to 1/64 px and strokes expanded on the CPU, GPU signed-area coverage accumulation into an `R32Float` atlas (`ShapeNode`, `CoverageNode` for masks), `Renderer::custom_pipeline`; GPU vs exact polygon areas: straight edges max 0.00049, curves max 0.0156, gradients max 0.00097; 50 mixed shapes into a 4K layer (64 outlines, 25,749 edges, 29.9 M accumulate and 12.8 M paint fragments): GPU median 3.41 ms, p99 9.40 ms when rerun alone (9.33 ms median in the first run under load), CPU build 4.7 ms (debug); machine shared with other agents, M2 stands in for M1
- 2026-10-01 — Phase 4 — integration fix (check #3) — 037141c, f1f2390 — workspace clippy on Rust 1.98: `media::analysis::beats` searches the previous beats over a slice of the score (`needless_range_loop`), `render::shape::stroke` uses `is_empty` (`len_zero`); `fifty_shapes_4k_gpu_time` measured GPU median 9.33, 3.41, 9.33 and 9.33 ms in four runs (p99 9.4 ms in all), so the vector shapes Progress Log figure of 3.41 ms was the outlier: the test is ignored and the 8 ms budget is a new Phase 4 task
- 2026-10-01 — Phase 3 — thread layout — 6d9ea89 — `engine::threads` registry: every long-lived thread (`main` UI, `glass-sched`, `glass-decode-<n>`, `glass-render`, `glass-audio` callback via a pre-registered hook, `glass-analysis-<n>`, `glass-import`, `glass-io`) named from its `Role` and listed with OS thread id, state and seqlock busy/idle/blocked totals, wakeups and last activity, read without locking the threads; audio callback hook 0 allocations over 20,000 callbacks; `engine::playback` skeleton (scheduler, decode pool of logical cores minus 3 clamped to 2..=4, so 4 on the 8-core M2 standing in for M1, render) delivers frames in order across seeks and joins every thread on shutdown, 20 start/stop rounds leak none; decode-bound run shows decoders over 50% busy and scheduler under 50%; the debug panel that reads `threads::global().snapshot_into` is the Phase 6 debug panel task (machine shared with other agents)
- 2026-10-01 — Phase 3 — master clock — 39535dd — `audio::OutputStream` (cpal 0.18 f32 stream, default device) and `audio::OfflineOutput` share one callback fed by an `rtrb` ring through `audio::Feeder`; 0 allocations in 2000 callbacks with underruns, channel mapping, drift correction and clock reads (counting allocator); `audio::drift::DriftEstimator` DLL (1.5 Hz narrowing to 0.1 Hz, slope slewed at most 0.5%) against simulated devices 0.1% fast/slow, 300 ppm, 0 to 1 ms jitter, 256 to 1024-frame buffers: max position error after 10 s 0.56 to 0.59 ms (mean wake-up lag 0.5 ms), rate error 6 to 14 ppm, after 3 s 0.59 ms; `Clock::system` monotonic stand-in; `audio::video_sync::VideoSync` (mid-refresh frame, 0.15-frame hysteresis): 30 at 60 Hz even 2-2 cadence and 24 at 60 Hz 3:2 under jitter, 60 at 60 Hz 0.1% drift over 60 s corrected by 3 skips or 4 repeats, never adjacent; real-device listening check tagged needs-human
- 2026-10-01 — Phase 9 (pulled forward: self-contained in `render` and `project::params`) — LUT framework — 34ad698 — `render::lut` `.cube` parser (TITLE, LUT_1D/3D_SIZE, DOMAIN_MIN/MAX, LUT_1D/3D_INPUT_RANGE, Resolve shaper + cube, comments, errors with line numbers) and writer, f64 tetrahedral reference; `nodes::GpuLut` (Rgba16Float 3D texture) and `LutNode` (declared `LutSpace`, Rec.709 display-referred default, intensity mix in linear light); `project::params::effects::lut` (file as a text row until a file-pick row exists, input space, intensity slider); identity 2/17/33/65 exact within one f16 step, GPU vs CPU max 0.00098; 4K GPU median on M2 (stands in for M1, machine shared with other agents): picture 1.15 ms (33 and 65-point), 1D 1.05 ms, copy 0.40 ms; per-pixel noise worst case 3.77 / 7.87 ms
- 2026-10-01 — Phase 17 — portable mode — 008425d — `app::locations::Locations` is the one resolver for the config, data and per-user cache folders (env override, then `Glass Data/{Settings,Data,Cache}` when a `Glass Data` folder sits next to the executable or next to `Glass.app`, then the platform folder, then temp); every candidate is created and probed, read-only ones are skipped with a logged warning; settings, crash reports and the recovery scan go through it; 6 tests (both modes, bundle rule, overrides, read-only portable and platform folders); logs stay on stderr and the ring, no folder
- 2026-10-01 — Phase 4 — visual regression tests — e170709, a905713 — `crates/render/tests/visual_regression.rs` + `tests/visual/` registry (`Case`: name, frame indices, scene per frame; `Source` bars via `YuvSourceNode`, stills, fills, text, shapes) rendered at 320 x 180 through `composite_stack` and `OutputNode::preview` seeded per frame; 4 cases, 11 golden frames, 568 KB; GPU tolerance 6 levels on 0.05% of pixels, software 24 on 0.5%; deterministic on M2 (0 pixels over threshold), a 1-pixel shift fails with 9.7% of pixels; `GLASS_UPDATE_SNAPSHOTS=1`, `.actual.png`/`.diff.png`, CI step on macOS arm64
- 2026-10-01 — Phase 5 — integration fix (check #4) — e232d11 — schema v2 changed how a snapshot's project serialises (`media` as `{"refs": [...]}`), so `project::autosave::FORMAT_VERSION` is 2 and recovery reads a snapshot's format version before its body: an autosave from a format 1 build is refused as `AutosaveError::Format(1)` (logged by the launch scan as not readable) instead of failing as damaged JSON; test `a_format_1_autosave_from_before_schema_v2_is_refused_by_version`
- 2026-10-01 — Phase 5 — sequences, tracks, clips, gaps, markers — 48e5946 — `TrackKind` video/audio/subtitle/adjustment with `accepts` (clips on video and audio, `Subtitle` cues, `Adjustment` items), `set_track_kind` only when every item fits kind and grid; gaps implicit (`Track::gaps`, `item_at`, Decision 40); `project::marker` sequence markers (sequence time, frame grid) and clip markers (source time) with name, note, duration, label colour, chapter, to-do, `add/update/remove_marker`; undo labels `edit.label.change_marker`, `delete_marker`, `change_track_kind`; autosave diffs carry sequence markers as `Node::Marker` (autosave format 3); schema v3 with `migrate_v2_to_v3`, fixtures `v3-basic.json` and `v3-tracks.json` (v1/v2 kept); 12 new sequence tests incl. 2000 random edits checked against the invariants after each, sharing (`Arc::ptr_eq` on untouched tracks, items and markers), diff apply and rebuild; finish check ok (`inspector_column_p99_is_within_budget` failed once under load at p99 57 ms, passed on rerun)
- 2026-10-01 — Phase 6 — remappable shortcuts: model, presets, dispatch, keyboard editor (pulled forward, needs no timeline model; Switch preset dialog, saved sets, compare table and Settings pane left as a sub-task) — 54bae9a — 68 commands in 5 groups, 4 presets with no clashes or system keys on macOS, Windows and Linux, dispatch tests per preset and a clash test; headless editor cycling 8 states on M2 (stands in for M1, machine shared with other agents), debug, 1x: p50 3.04 / p99 5.99 ms dark, 6.12 light, 4.82 high contrast, max 14.4 ms (2 of 240 frames over 8 ms, state switches), timed after one warm-up pass of every state (a state's first showing lays out new text, up to 57 ms); 2 snapshot cases, 12 goldens, 1.8 MB
- 2026-10-01 — Phase 2 — relinking — 86e451f, 41fc563, 0238672 — `engine::media_mgmt::relink`: `RelinkSearch` on a `glass-relink` thread walks picked folders with the import scan, hashes up to 2,000 likeliest files (equal content hash = exact, 100%), probes 8 per source and scores name 0.35, duration 0.35 (half a frame, min 50 ms, smooth falloff), resolution/fps/codec 0.1 each, x0.9 on a known hash mismatch, capped at 95%; tiers Exact/Likely (70%+, auto-picked)/Possible (40%+); each exact match's `FolderMove` is repeated for the other offline sources; `project::Project::relink_media` (`EditLabel::Relink`, one undo step) replaces only `MediaRef` nodes and refuses files shorter than the used source range (1 ms tolerance); `ui::dialogs::relink` from `Dialog / Relink media` (3 snapshot cases, 18 goldens); fixture tests: moved folder, renamed file found by hash, same-name re-encode never exact, near-duplicate durations ranked, too-short file refused, move inference finds an unsearched card; headless dialog with 12 clips scrolling on M2 (stands in for M1, debug, machine shared with other agents): p99 4.3 / 4.9 / 4.7 ms dark / light / high contrast
- 2026-10-11 — Phase 8 — encoder wrapper: AV1, VP9 and MP3 (needs-human) — 4ee26dd — the pinned FFmpeg build has no AV1, VP9 or MP3 encoder (only dav1d decode); `media::export` resolves them to `libsvtav1`/`libaom-av1`/`librav1e`, `libvpx-vp9` and `libmp3lame` and returns `ExportError::Unavailable`; enabling them needs new pinned source archives and an FFmpeg rebuild
- 2026-10-11 — Phase 5 — clip properties — 8d9e147 — `project::clip`: speed map (keys of source time and speed; one key at 0 in v4, 1/100 to 100, reduced), enabled flag, gain (-96 to +24 dB), transform (centre offsets, per-axis scale, degrees), crop and feather, opacity, blend mode, effect stack (`Effect` nodes with schema id, enabled flag, values at schema defaults); source range derived exactly as source start plus timeline duration times speed, source-to-timeline durations rounded to the nearest grid step (halves up, at least one); 15 edits (`set_clip_speed` keeping source or timeline, `set_clip_source_range`, `slip_clip`, enabled, gain, transform, crop, opacity, blend, add/remove/move/enable effect, `set_effect_param`) plus a speed-aware `trim_item`, 7 new undo labels; schema v4 with `migrate_v3_to_v4`, fixtures `v4-basic`, `v4-tracks`, `v4-props` (v3 kept); autosave format 4; 19 edit tests in `tests/clip_props.rs`, 4 new schema tests, 2 new autosave tests, 7 unit tests; finish check ok (`every_theme_renders_within_the_frame_budget` failed once under load, passed on rerun)
- 2026-10-11 — Phase 4 — vector shapes within the 4K budget — 17c5dd8 — sparse strips (`render::shape::strips`, Decision 46): bands of 4 canvas rows, edges accumulate only to the end of their interval, paint reads the atlas only in intervals and paints covered gaps with the carried winding; 50 mixed shapes at 4K (64 outlines, 25,749 edges): accumulate fragments 29.9 M to 0.4 M, paint 12.8 M to 5.3 M (23,215 paint pieces); GPU median on the M5 (stands in for M1, machine shared with other agents) release 1.72 to 0.75 ms, debug test 1.7–2.6 ms (p99 up to 8.2) to 3.45–3.47 ms (p99 3.5–6.9; the debug figure tracks the slower CPU loop); CPU build release 0.4–0.6 to 1.6–1.8 ms, debug 3.0 to 18.8 ms; coverage vs reference unchanged (straight 0.00049, curves 0.0156), visual regression goldens unchanged; `fifty_shapes_4k_gpu_time` no longer ignored
- 2026-10-11 — Phase 4 — mask node — d66c59c, 71f80e8 — `render::mask` (`Mask`: shapes in layer pixels unioned, feather, invert, `MaskMode` union/intersection in list order; f64 reference, Decision 44) and `nodes::MaskNode` (`mask.wgsl`) after one `shape::CoverageNode` per shape, multiplying the layer's premultiplied colour before its transform and blend (`mask::add_to_graph`); Gaussian feather sigma = r/3 truncated at r, at half resolution from 12 px and a quarter from 32 px; GPU vs reference max 0.0009 hard, 0.0028 feathered, 0.0070 at lower resolution (0.024 at the layer border); 5 visual cases (`mask_hard`, `mask_feathered`, `mask_inverted`, `mask_union`, `mask_intersection`, 1 frame each, goldens 808 KB in all); 3-shape mask feathered 20 px GPU median 0.58 ms at 1080p, 2.31 ms at 4K (6.6 ms before the low-resolution blur), 8 px 0.96 / 3.85 ms, hard 0.39 / 1.56 ms (M5 standing in for the M1, release, machine shared with other agents); finish check ok
- 2026-10-11 — Phase 8 — encoder wrapper (AV1, VP9, MP3 needs-human) — 4ee26dd — `media::export::ExportSession` (settings: container, codec, CRF or bit rate, pixel format, BT.709/BT.2020 PQ/HLG/sRGB tags, frame rate, audio rate and channels; RGBA8, BGRA8, RGBA16, RGB10A2 or YUV pictures with any stride, converted by a checked swscale context with the tags' matrix, limited range and left siting: red 63/102/240 BT.709, 74/97/240 BT.2020); 14 tests re-probe and decode 1 s at 640x360 30 fps plus a 48 kHz tone: H.264 and HEVC (`hvc1`) in MP4, HEVC PQ 10-bit in MOV, H.264 HLG 10-bit in Matroska, ProRes HQ and 4444 (VideoToolbox; `prores_ks` with `codec-prores-sw`), PNG (16-bit RGBA) and TIFF sequences, GIF, WAV, FLAC and AAC (WAV and FLAC sample-exact), with codec, size, frame count, duration and colour tags checked; H.264 CRF 23 RGB PSNR mean 37.7 dB, worst frame 37.5 dB; 1080p encode from RGBA8 on the M5 (stands in for M1, release, machine shared with other agents): x264 medium 127 fps, x265 medium 66 fps, ProRes 422 HQ VideoToolbox 251 fps, 4444 193 fps, `prores_ks` HQ 116 fps, PNG 376 fps, TIFF 436 fps, GIF 8.5 fps; AAC 36x and FLAC 24-bit 923x realtime; Decision 47
- 2026-10-11 — Tooling — dev setup fixes — ee739d1, 31f71db — `scripts/dev-env.sh` adds `~/.cargo/bin` to `PATH` when `cargo` is missing (non-login shells), so the host triple and pinned `FFMPEG_DIR` resolve; `tests/fixtures/generate.sh` prefers `$FFMPEG_DIR/bin/ffmpeg` and skips fixtures whose encoder that FFmpeg lacks (the pinned build has no libvpx, libopus, libsvtav1, libmp3lame: `vp9_720p30.webm`, `sync_vp9_opus.webm`, `av1_1080p30.mp4`, `audio_44k1_mono.mp3`), leaving them out of the manifest so their tests skip; `scripts/worktree-setup.sh` exits 0 from `env -i` zsh, 44 fixtures (45 MB) in 5 s, `--verify` passes
