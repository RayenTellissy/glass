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

**Concurrency limits (hard, the machine is modest).** Never run more than **10 agents or subagents at once**, including the coordinator. Prefer 3 to 5. Never run more than **one `cargo build` or `cargo test` at a time** across all agents; the rest wait. Cap the compiler with `CARGO_BUILD_JOBS=4` and set `[build] jobs = 4` in `.cargo/config.toml`. Run benchmarks and the stress project serially, alone, with no other agents active. If the machine swaps or a build takes over 3x its usual time, drop to a single agent for the rest of the session. Tasks tagged `(parallel)` are the only ones that may run alongside the current phase task, and at most 2 of them at once.

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
- [~] Audio decode to f32 planar at the project rate using a high-quality resampler (`rubato`)

### Analysis (background, cancellable, results cached under the project's cache folder)
- [ ] Analysis job system: priority queue, cancellation, progress, disk cache keyed by content hash
- [ ] Thumbnail strips at 3 zoom levels
- [ ] Waveform peaks at 5 zoom levels
- [ ] Proxy generation: ProRes Proxy on macOS, H.264 720p on Windows, user-selectable, resumable
- [ ] Scene cut detection
- [ ] Silence detection with threshold and minimum length
- [ ] Beat and tempo detection
- [ ] Loudness analysis (EBU R128 integrated and short-term)

### Media management
- [ ] Import from drag-drop, file dialog, and folders, recursive, with camera card structures recognised (AVCHD, XDCAM, Sony, Canon, DJI)
- [ ] Media pool with bins, colour labels, tags, and a search index
- [ ] Relinking: fuzzy match by name, duration, and content hash, batch relink dialog
- [ ] Offline media placeholder that keeps edits intact and renders a labelled slate
- [ ] Metadata panel: camera, lens, timecode, GPS, created date

## Phase 3 — Playback engine (M1)

Done when: the stress project (Phase 16) plays at full rate on both platforms, scrub latency benchmark passes, and no UI frame exceeds 8 ms during playback.

- [ ] Thread layout implemented: UI, scheduler, decode pool, render, audio callback, analysis pool, all named, all visible in the debug panel
- [ ] Lock-free frame queues between decode, render, and present (`crossbeam` or `rtrb`)
- [ ] Master clock from the audio device callback, video slaved to it, drift correction
- [ ] Look-ahead decode across cuts and transitions, configurable depth
- [ ] Rendered-frame cache keyed by (segment hash, frame index), LRU in VRAM with RAM spill and a configurable ceiling
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

- [ ] Render graph: per-frame DAG compiled into `wgpu` passes with resource aliasing
- [ ] Linear-light working space, `Rgba16Float` intermediates, dithered 8-bit output
- [ ] Layer compositing with blend modes: normal, multiply, screen, overlay, add, subtract, difference, soft light, hard light, colour dodge, colour burn
- [ ] Transform node: position, scale, rotation, anchor, bicubic resampling, motion-blur ready
- [ ] Opacity, crop with feather
- [ ] Shader hot reload in debug builds
- [ ] Effect parameter system: typed params, ranges, defaults, units, keyframeable flag, UI auto-generated from the schema
- [ ] Text rendering via `cosmic-text`: glyph atlas, outlines, shadows, per-glyph transform hooks
- [ ] Vector shapes: rect, ellipse, polygon, path, stroke, fill, gradients
- [ ] Mask node: shape masks, feather, invert, intersection and union
- [ ] Async GPU readback ring for export and scopes
- [ ] Visual regression tests: render reference sequences, compare against golden PNGs with a tolerance

## Phase 5 — Project model (M2)

Done when: a 1000-clip project round-trips through save and load byte-identically, undo depth is unlimited, and crash recovery restores the last autosave.

- [ ] Immutable edit tree using `im` with structural sharing
- [ ] Undo and redo as a list of tree roots, grouped per gesture, unlimited
- [ ] Autosave as diffs every 10 s to the cache folder, recovery prompt on next launch
- [ ] Versioned schema with a migration function per version, tested with fixtures from every prior version
- [ ] Sequences, tracks (video, audio, subtitle, adjustment), clips, gaps, markers
- [ ] Clip properties: source range, timeline range, speed map, effect stack, transform, audio gain, enabled flag
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

- [ ] Encoder wrapper: H.264, HEVC, AV1, ProRes (native on macOS, software elsewhere behind a flag), VP9, PNG and TIFF sequences, GIF, WAV, AAC, MP3, FLAC
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
- [ ] LUT framework: `.cube` loader, GPU 3D LUT node, intensity slider
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
- [ ] UI snapshot tests for every panel in light and dark (parallel)
- [ ] Accessibility: keyboard for everything, screen reader labels, high contrast, scalable UI (parallel)
- [ ] Full UI localisation and dictionary check in CI (parallel)

## Phase 17 — Distribution

Done when: a tagged build installs cleanly on a fresh macOS and Windows machine without warnings.

- [ ] macOS: Universal binary, hardened runtime, DMG
- [ ] macOS: notarisation (needs-human: Apple Developer account and certificate)
- [ ] Windows: MSIX and portable EXE
- [ ] Windows: code signing (needs-human: certificate)
- [ ] Linux: AppImage and Flatpak
- [ ] Auto-updater with delta updates and rollback
- [ ] Asset packs downloaded on demand so the installer stays small
- [ ] Portable mode with settings next to the executable
- [ ] Safe mode: disable plugins and GPU features on repeated crash
- [-] Licensing and activation (dropped: free and open source, see Decision Log)
- [ ] About dialog: version, MIT and GPLv3 notices, third-party licences, link to the release's source archives, matching the About window on the Brand page

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
