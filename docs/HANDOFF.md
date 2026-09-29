# Handoff: resuming the goal

State of the work when the session paused on 2026-09-29, and how to pick it up in a fresh session. `PLAN.md` stays the single source of truth; this file points into it.

## 1. Where things are

| Branch | Contents |
| --- | --- |
| `main` | Phases 0 and 1 merged (local Done-when parts; CI and hardware parts are needs-human). |
| `goal/phase-2` | Phase 2 work so far, plus tasks pulled forward (below). The integration branch: new task branches start here. |
| `drafts/phase-4-5-wip` | Uncompiled, untested drafts for the next tasks, in `drafts/` (see below). Not part of any build. |

No task worktrees or `task/*` branches are left; everything finished is merged into `goal/phase-2`.

**Done on `goal/phase-2`.** Phase 2: safe FFmpeg wrapper, probe, software decode, VideoToolbox decode, software fallback, colour space plumbing, VFR normalisation, accurate seek (plus the seek landing fix), stills and sequences (macOS part), audio decode and resampling, analysis job system, thumbnail strips, waveform peaks, proxy generation, scene cut detection, import. Pulled forward:

- Phase 4: render graph (`render::graph`, `Renderer`, `GpuProfiler`, `YuvSourceNode`, `GamutNode`)
- Phase 5: immutable edit tree (`imbl`), undo and redo (`project::History`)
- Phase 16: UI snapshot tests (`ui::snapshot`, `crates/ui/tests/ui_snapshots.rs`), with cases for the import panels

**Drafts on `drafts/phase-4-5-wip`** (read `git show drafts/phase-4-5-wip:drafts/README.md`). To use one: move the file into its crate, compile, test, then delete it from `drafts/`.

| Draft folder | Covers | Known issues |
| --- | --- | --- |
| `drafts/analysis/` | Phase 2 silence (`silence.rs`), beat and tempo (`beats.rs`), loudness (`loudness.rs`), tests for each, a fixture patch for `tests/fixtures/generate.sh` | `ebur128` crate not added or checked with `cargo deny`; beat latency constant uncalibrated (0.0); FLAC fixtures from the patch decode as s32 and fail `crates/media/tests/decode.rs`'s native-format check (add `-sample_fmt s16` or extend the mapping); see `NOTES.txt` |
| `drafts/media-pool/` | Phase 2 media pool model (`mm_pool_model.rs`) and search index (`mm_search.rs`) | Draft's `ItemId` clashes with `project::ItemId`: rename (e.g. `MediaItemId`) or reuse `project::MediaId` |
| `drafts/compositor/` | Phase 4: output node with dither and HDR transfer (`output.*`), blend modes with a CPU reference (`blend.rs`, `composite*`), transform, crop and opacity (`transform.rs`, `composite_full.wgsl`), shader hot reload (`shaders.rs`), effect parameter system and egui auto-UI (`render-src/params.rs`, `ui-params/`) | Hot reload needs `notify` and a direct `naga` dependency (check with `cargo deny`); `ui-params/` will need snapshot cases (`crates/ui/src/params/` is covered by the registry check) |
| `drafts/project-autosave/` | Phase 5 autosave: root diffs (`diff.rs`), generation snapshots with an append-only synced log and unclean-exit marker (`autosave.rs`), validation, recovery, recovery dialog (Paper `Dialog / Recovery`) | Drafted for the `task/project-model` layout; `project` must not start threads (see section 6); the dialog needs a snapshot case |

### Counting open tasks

Open tasks are lines matching `- [ ]` or `- [~]` (sub-tasks included) that do not contain `needs-human`. For example:

```sh
grep -E '^\s*- \[( |~)\]' PLAN.md | grep -vc needs-human
```

Counts on `goal/phase-2` at the time of writing (sub-tasks counted as tasks; needs-human parents "blocked only on the sub-task(s) below" counted as needs-human):

| Phase | Done | Open | Needs-human |
| --- | --- | --- | --- |
| 0 Decisions and prototypes | 7 | 0 | 2 |
| 1 Skeleton and CI | 21 | 0 | 7 |
| 2 Media engine | 16 | 7 | 4 |
| 3 Playback engine | 0 | 13 | 0 |
| 4 GPU compositor | 1 | 11 | 0 |
| 5 Project model | 2 | 12 | 0 |
| 6 Timeline UI | 0 | 17 | 0 |
| 7 Core editing | 0 | 9 | 0 |
| 8 Export | 0 | 10 | 0 |
| 9 Built-in library | 0 | 32 | 2 |
| 10 Easy mode and AI | 0 | 15 | 0 |
| 11 Colour | 0 | 10 | 0 |
| 12 Audio | 0 | 10 | 0 |
| 13 Advanced compositing | 0 | 10 | 0 |
| 14 Multicam and workflow | 0 | 8 | 1 |
| 15 Plugins and scripting | 0 | 7 | 0 |
| 16 Performance and quality | 1 | 7 | 0 |
| 17 Distribution | 0 | 8 | 2 (and 1 dropped) |
| 18 Documentation | 0 | 6 | 0 |
| **Total** | **48** | **192** | **18** |

## 2. Needs a person

**Fonts**
- Add Geist and Geist Mono (OFL): the four TTFs listed in `assets/fonts/README.md` plus `OFL.txt`, into `assets/fonts/`. Then regenerate every UI snapshot golden: `GLASS_UPDATE_SNAPSHOTS=1 scripts/cargo-serial test -p ui --test ui_snapshots`, review the PNGs, commit. Installers must then ship `OFL.txt`.

**GitHub remote and CI**
- Create the GitHub repository and push `main`, `goal/phase-2` and `drafts/phase-4-5-wip`.
- Set `repository` in the root `Cargo.toml` (placeholder `https://github.com/OWNER/glass`), run `dist generate`, commit.
- Confirm CI: the check matrix (macOS arm64, macOS x86_64, Windows x86_64, Linux x86_64: build, test, clippy, fmt, deny, i18n and theme checks), the FFmpeg build per target (cached on the `versions.env` hash), the tagged release (DMGs, MSI, MSIX, AppImage, sources attached), the bench job (merge once to `main` to seed the baseline, then open a PR and check the comment), and the UI snapshot step on the `macos-14` runner.
- Windows and Linux builds of the hardware-frame types compile with an empty handle enum; only CI checks them.

**Hardware**
- Windows PC: proto-decode via D3D11VA (PLAN line 97), D3D11VA hardware decode (line 144), HEIC/HEIF/AVIF through WIC (line 152).
- NVIDIA GPU: proto-gpu on a GTX 1660 class GPU (line 95: `cargo run --release` in `experiments/proto-gpu/`).
- NVIDIA and Intel or AMD Linux: NVDEC and VAAPI decode (line 145).
- An Apple M1: the M2 here stood in for the M1 reference in every measurement; rerun the benchmarks there.
- An unlocked screen: windowed rerun of `experiments/proto-ui` (offscreen numbers are in the Progress Log).

**Signing, identity, licences**
- macOS notarisation (line 436) and Windows code signing (line 438): certificates and an Apple Developer account; MSIX and a notarised DMG need them.
- Choose the bundle id (placeholder `org.glass.Glass`). dist artifacts are named `app-<target>` after the `app` package; consider renaming the package to `glass`.
- Licence the music (line 314) and SFX (line 315) catalogues.

**Every needs-human line in PLAN.md**
- Line 95: `proto-gpu` on a GTX 1660 class GPU
- Line 97: proto-decode on Windows via D3D11VA
- Line 112: GitHub Actions matrix
- Line 114: FFmpeg build script (blocked only on line 118)
- Line 118: CI job building FFmpeg per target and attaching sources
- Line 119: `cargo-dist` config (blocked only on line 122)
- Line 122: tagged release in CI producing every installer
- Line 125: benchmark harness (blocked only on line 127)
- Line 127: `bench` job posting a PR comment
- Line 144: hardware decode D3D11VA
- Line 145: hardware decode NVDEC and VAAPI
- Line 150: stills, sequences, GIF, WebP, HEIC (blocked only on line 152)
- Line 152: HEIC, HEIF and AVIF on Windows through WIC
- Line 314: music pack
- Line 315: SFX pack
- Line 404: v1.0 checklist (certificates)
- Line 436: macOS notarisation
- Line 438: Windows code signing

## 3. Decisions made so far

Full text in `docs/DECISIONS.md`; one line each here.

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

## 4. Open follow-ups

**Phase 2**
- Image sequences probe at FFmpeg's default 25 fps; anything that probes sequences must honour the sequence or user frame rate (`VideoSource` does; `probe` does not).
- Analysis drafts: see the `drafts/analysis/` row above (ebur128 unchecked, beat latency uncalibrated, FLAC fixture sample format).
- Media pool draft: `ItemId` clash (see above); the metadata panel will be a new `ui::panels` module and needs a snapshot case.
- `TimelineCache` and `GopIndex` already use `CacheDir`/`ContentHash`; new analysis results (silence, beats, loudness) should reuse the same scheme.

**Phase 3 (playback)**
- VideoToolbox through FFmpeg decodes one frame at a time (4K: 110 fps VT vs 478 fps software on synthetic clips). For look-ahead, consider a direct async `VTDecompressionSession` or several decoders; benchmark with real footage.
- Seek after the landing fix (M2, median/p95 ms): software 1080p 13.6/22.5, 4K 39.5/74.8; VideoToolbox 1080p 51.5/110.8, 4K 121.8/279.3. Scrub should decode exact frames in software and use VideoToolbox for playback. Other ideas: a separate low-latency software decoder for seeks (slice threads, or `AVDISCARD_NONREF` before the target), cache neighbouring frames, proxies for 4K.
- `HwPreference` defaults to `Off`; playback and `media_check` use `Auto`.
- Playback must switch to proxies (proxies are video-only; stills and sequences have none). The proxy/full-res toggle is Phase 14.

**Phase 4 (compositor)**
- When `render` joins eframe's wgpu device, request `wgpu::Features::TEXTURE_FORMAT_16BIT_NORM` (10-bit P010 surfaces) on the shared device.
- Sources in other primaries go through `GamutNode` into the working primaries (Decision 15); make sure every source path in the compositor inserts it.

**Phase 6 (panels)**
- High-contrast component differences to implement in component code: status pill `bg/2` fill, selected tab accent ring.
- The snapshot mono font (Courier New, until Geist lands) has no `⌘`, so the import drop area's shortcut key renders as `?I` in the goldens. It goes away with Geist Mono; if Geist stays out long, add a symbol fallback font.

**Phase 14 / 16 (performance)**
- Proxy pipeline decodes, scales and encodes on one thread; splitting decode/scale and encode across threads could lift ProRes VT 4K from about 130 to about 230 fps.
- The frame-budget test `every_theme_renders_within_the_frame_budget` can fail under parallel load or swap; rerun it alone before treating it as a failure.

**Release (Phase 1/17, with the needs-human items)**
- `repository` placeholder, bundle id, `app-<target>` artifact names, OFL.txt in installers (section 2).

## 5. How to resume

**Session loop.** Follow "Working on a task" in `CONTRIBUTING.md` (and PLAN.md's "How to run this plan as a goal"): read PLAN.md and the last 20 Progress Log lines, `df -h /`, `source scripts/dev-env.sh`, run the full check, pick the next task, and use the three-commit rhythm: `plan: start <task>` (mark `[~]`), `<area>: <what changed>`, `plan: done <task>` (mark `[x]`, Progress Log line `- YYYY-MM-DD — Phase N — <task> — <impl hash> — <key numbers>`).

**Parallel worktrees** ("Working in parallel" in `CONTRIBUTING.md`):
- One worktree per task: `git worktree add ../glass-<name> -b task/<name> goal/phase-2`, then `scripts/worktree-setup.sh` inside it (generates its test media, prints the shared `CARGO_TARGET_DIR` and `FFMPEG_DIR`).
- Every shell: `source scripts/dev-env.sh`; every compiling cargo command through `scripts/cargo-serial` (one at a time machine-wide; `cargo fmt` may run directly). All worktrees share `/Users/ryantellissy/Glass/target` and the pinned FFmpeg in `.glass-cache/ffmpeg/aarch64-apple-darwin/`; no `cargo clean` while others work.
- `PLAN.md` and `docs/DECISIONS.md` use git's union merge driver: after every merge run `sort PLAN.md | uniq -d` and look for a task line kept twice (`[~]` and `[x]`), then delete the stale copy.
- Before reporting, a task branch merges the phase branch in and reruns the full check; the coordinator then merges `task/<name>` into `goal/phase-<N>` with `--no-ff`, deletes the branch and removes the worktree. Never commit to `main`; merge a phase branch to `main` only when its locally verifiable Done when passes (Decision 12).
- Shared files (`PLAN.md`, `docs/ARCHITECTURE.md`, `assets/i18n/en.json`, `Cargo.toml`, `Cargo.lock`): keep edits small and local; add new modules rather than reorganising.

**Working conventions not spelled out in CONTRIBUTING.md**
- Design lives in the Paper file "Inspiration 2", file id `01M3N8N2AWRZ7XXZY58Q1RH2BT`. Always pass the file id and an explicit page id (the user may be viewing another file or page; never switch their page). Page ids: `p-1-0` 00 Foundations, `p-C-0` 01 First run, `p-B-0` 02 Edit workspace, `p-A-0` 03 Timeline, `p-9-0` 04 Browsers, `p-8-0` 05 Inspector & keyframes, `p-7-0` 06 Captions & assist, `p-6-0` 07 Colour, `p-5-0` 08 Audio, `p-D-0` Brand, `p-4-0` 09 Export, `p-3-0` 10 System & states, `p-2-0` 11 Platform, theme, RTL. Read values with `get_jsx` and `get_computed_styles`, never from screenshots. Do not edit the file unless a task says so. Never put node ids in docs or reports.
- The Mac's screen is usually locked and eframe skips painting occluded windows, so tests, benchmarks and the frame-time monitor run headless/offscreen (`ui::headless`, `experiments/proto-ui/src/offscreen.rs`). Only a check that truly needs a visible window gets a needs-human sub-task.
- Never copy files from other projects on this machine into the repo. The only downloads are crates through cargo and the pinned, checksummed source archives in `scripts/ffmpeg/versions.env` (FFmpeg, x264, x265, dav1d, pkgconf, nasm, meson, ninja). No fonts, models or asset files. Outside the repo only `cargo install` of tools PLAN.md names and `rustup` components; no `brew install`.
- Commit messages carry no attribution trailers or footers.
- Measurements say that the M2 stands in for the M1 reference. Rerun a frame-budget or timing test once if it fails under load and say so.
- Split tasks: the parent keeps its checkbox and becomes `[x]` when every sub-task is `[x]` or `[-]`; if only needs-human sub-tasks remain it gets `(needs-human: blocked only on the sub-task(s) below)`. Needs-human: do the local part, tag the line `(needs-human: <reason>)`, add a Progress Log line, commit `plan: needs-human <task>`, move on. A task that needs a CI job adds it to `ci.yml`, verifies locally what it can and tags only the CI part.

**Machine guidance.** This Mac (Apple M2, 8 GB RAM) swapped heavily with five parallel tracks plus other apps open. Up to 10 agents may run in parallel (PLAN.md's hard cap, confirmed by the owner); cargo commands stay serialised through `scripts/cargo-serial`, so extra agents add queueing rather than memory. If the Mac starts swapping, export `CARGO_BUILD_JOBS=2` after sourcing `dev-env.sh` (which sets 4) and close other heavy apps. Disk free at the time of writing: 24 GB (`df -h /`). Below 15 GB: `cargo sweep --time 7`, then regenerable caches under the project cache folder, then `cargo clean` (only with no other worktree building). Never delete source or anything outside the repo.

## 6. Suggested next work, in order

1. **Finish Phase 2.**
   - Silence detection, beat and tempo, loudness: from `drafts/analysis/` (add `ebur128`, run `cargo deny check`, fix the FLAC fixture format, calibrate the beat latency).
   - Media pool with bins, labels, tags and search: from `drafts/media-pool/` (resolve the `ItemId` clash).
   - Relinking (fuzzy match by name, duration, content hash; batch dialog, Paper `Dialog / Relink media` on `10 System & states`), after the media pool.
   - Offline media placeholder with a labelled slate.
   - Metadata panel (camera, lens, timecode, GPS, created date), a `ui::panels` module with a snapshot case.
2. **Phase 4 tasks 2 to 7** from `drafts/compositor/`: linear-light working space and dithered output, blend modes, transform, opacity and crop, shader hot reload, effect parameter system with its auto-UI.
3. **Phase 5 tasks 3 to 7**:
   - Autosave from `drafts/project-autosave/`. `project` must not start threads: the 10 s autosave thread and the launch recovery prompt belong in `engine`/`app`, with `project` providing the diffs, snapshots and validation.
   - The schema version moves v1 → v4 across tasks 4 to 7 (versioned schema with migrations, sequences/tracks/markers, clip properties, transitions); each bump adds a migration and a fixture from the prior version. Run these one after another, not in parallel.
   - Embed the media pool (`MediaPool`) into `Project.media` once both exist.
4. **Phase 3 playback** once the project model (sequences, clips, transitions) and the compositor (through the effect parameter system) are far enough along; it builds on the Phase 3 follow-ups above.

**What can run in parallel** (at most three tracks on this machine):
- Track A, `media` analysis: silence, beats, loudness (one after another).
- Track B, media management: media pool, then relink, offline slate, metadata panel.
- Track C, `render` compositor: Phase 4 tasks 2 to 7.
- Phase 5 touches `project` only and can be a fourth track when memory allows, or follows Track A. Embedding `MediaPool` into `Project.media` waits for both Track B's pool and Phase 5's schema work. Phase 3 starts after Tracks C and Phase 5.
