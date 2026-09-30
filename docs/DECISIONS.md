# Decision Log

Every decision that shapes Glass is recorded here, oldest first. Each entry expands one line of the Decision Log at the bottom of `PLAN.md`.

How to add a decision:

- Append one line to the Decision Log in `PLAN.md`: `- YYYY-MM-DD — <decision>. Reason: <reason>.`
- Append the same decision here as a new entry, with more context where it helps: what it changes, what else was considered, and where the evidence lives.
- Changing a Phase 0 default, dropping a task (`[-]`), or raising a budget always needs an entry in both places. A Phase 0 default changes only when a benchmark in this repo proves it fails a budget.

Entries are never edited after the fact except to fix typos. A decision that reverses an earlier one is a new entry that names the one it replaces.

---

## 1. Adopt the Phase 0 defaults table as the starting stack

- **Date:** 2026-09-29
- **Decision:** Use the defaults table in PLAN.md Phase 0 as the starting stack: `egui`/`eframe` on `winit` for UI, `wgpu` with WGSL for the GPU, `ffmpeg-next` for decode and encode, VideoToolbox, D3D11VA then NVDEC, and VAAPI for hardware decode with a software fallback, `cpal` for audio, `cosmic-text` for text, `im` and `serde_json` for project data, `wasmtime` with the Component Model for plugins, `whisper-rs` for speech-to-text, `cargo-dist` for packaging, and macOS 12 plus Windows 10 as minimum targets.
- **Reason:** Fastest path to a single small binary on both platforms.
- **Alternatives:** Each row names a fallback in PLAN.md: `iced` for UI, `rustybuzz` + `swash` for text shaping, `rpds` for persistent data, hand-written CI for packaging, software decode for hardware decode.
- **Consequences:** A default changes only when a benchmark in this repo proves it fails a budget. The prototypes confirmed the UI toolkit, `wgpu` on Metal, `cosmic-text` and VideoToolbox decode on Apple silicon; see `docs/ARCHITECTURE.md` section 11 for the status of every row.

## 2. Free and open source under MIT; licensing and activation dropped

- **Date:** 2026-09-29
- **Decision:** Glass is free and open source, with source code under MIT. The Phase 17 task "Licensing and activation" is dropped (`[-]`).
- **Reason:** Product decision.
- **Consequences:** No activation, no paid tiers, no telemetry. This later made the GPL FFmpeg build possible (entry 5).

## 3. Priority is speed, then features, then size

- **Date:** 2026-09-29
- **Decision:** When goals conflict, speed wins over features, and features win over size. The 30 MB installer size target gives way to features and speed optimisations, and release builds ship with every feature flag on.
- **Reason:** Product decision.
- **Consequences:** The speed budgets (UI frame under 8 ms, scrub under 50 ms, project open under 1 s, cold start under 1.5 s) are hard. The size budget can be raised, but each raise needs a Decision Log entry. Asset packs and models still download on demand.

## 4. Creators come first; pro tools are how the app grows with them

- **Date:** 2026-09-29
- **Decision:** Creators making videos for YouTube and social platforms are the primary audience. When their needs conflict with a pro's (defaults, what the app opens to, what shows without digging), the creator wins. Pro tools (colour, audio, compositing, multicam) never make the beginner path harder.
- **Reason:** Product decision.
- **Consequences:** The design splits into beginner surfaces (first run, storyboard, guided flow, browsers) and dense pro surfaces (timeline, colour, audio, inspector), as set out in `DESIGN_HANDOFF.md`.

## 5. GPL FFmpeg build with x264 and x265; release builds under GPLv3

- **Date:** 2026-09-29
- **Decision:** FFmpeg switched from an LGPL build to a GPL build with x264 and x265. Release builds are distributed under GPLv3; the source stays MIT.
- **Reason:** Better software H.264 and HEVC encoding, and the LGPL limit only mattered while a commercial licence was possible.
- **Alternatives:** An LGPL FFmpeg build without x264 and x265, which kept a commercial licence open. Entry 2 removed that need.
- **Consequences:** Every release bundles the licence texts and attaches the exact FFmpeg, x264 and x265 source archives used. `cargo-deny` allows only GPLv3-compatible licences. The About dialog shows both the MIT and GPLv3 notices.

## 6. Three commits per task

- **Date:** 2026-09-29
- **Decision:** Each task uses three commits: `plan: start <task>`, the implementation commit or commits, and `plan: done <task>`, whose Progress Log line cites the implementation commit hash.
- **Reason:** The log line can then point at the commit that did the work.

## 7. proto-decode CPU budget judged as whole-machine utilisation

- **Date:** 2026-09-29
- **Decision:** The proto-decode budget "under 5% CPU per stream" is judged as whole-machine utilisation: the app process plus the system decoder service (VTDecoderXPCService on macOS), divided by the number of logical cores, at 60 fps paced.
- **Reason:** The plan does not define the unit; this is how system-wide decode cost is usually stated. The per-core numbers are recorded in `experiments/proto-decode/RESULTS.md` so the result can be re-judged on the reference M1.
- **Evidence:** On the M2 (8 logical cores), the judged run (CVMetalTextureCache per frame, paced 60 fps, one stream) used 2.18% of the machine: 9.20% of one core in the process plus 8.26% of one core in VTDecoderXPCService, at 60.1 fps with 0 late frames. Rerun after review with a true 3-frame pacing window (the first run allowed 6 slots) against the pinned FFmpeg 9.0.2: 2.06% of the machine (8.58% + 7.88% of one core), 60 fps, 0 late; two paced streams 1.81% each, 0 late. The verdict is unchanged.
- **Alternatives:** Judging against a single core. Measured that way, the same run is above 5% (9.20% in the process alone), so the unit decides the verdict; the per-core figures are kept for that reason.
- **Consequences:** Later "under X% CPU" budgets are judged the same way, with per-core numbers recorded alongside.

## 8. FFmpeg pinned to 9.0.x

- **Date:** 2026-09-29
- **Decision:** FFmpeg is pinned to 9.0.x instead of 7.x, still a GPL shared build with x264 and x265, used through `ffmpeg-next` 9.
- **Reason:** The maintained `ffmpeg-next` 9 bindings build against it, and local development and release builds should use the same FFmpeg.
- **Evidence:** proto-decode built the published `ffmpeg-next` 9 and `ffmpeg-sys-next` 9 crates against the Homebrew FFmpeg 9.0.1 shared libraries (libavutil 61.1.101) with `FFMPEG_DIR` set at build time, so no FFmpeg 7.1 build was needed.
- **Alternatives:** Building FFmpeg 7.x for older bindings.
- **Consequences:** The Phase 0 defaults table and the Phase 1 FFmpeg build script task name 9.0.x.

## 9. Phase 0 closed with hardware checks pending

- **Date:** 2026-09-29
- **Decision:** Phase 0 is closed. The UI toolkit, GPU (Metal), decode through `ffmpeg-next`, and VideoToolbox hardware decode are confirmed by the `experiments/` prototypes. Defaults without a Phase 0 prototype (`cpal`, `im` and `serde_json`, `wasmtime`, `whisper-rs`, `cargo-dist`, Vulkan, NVDEC and VAAPI, min targets) are adopted and confirmed by the tests and benchmarks of the phase that builds them.
- **Reason:** Phase 0 only defines prototypes for UI, GPU, and decode, and the missing Windows and NVIDIA hardware should not block the engine work that follows.
- **Consequences:** The GTX 1660 run of `experiments/proto-gpu` and the Windows D3D11VA port of `experiments/proto-decode` stay open as needs-human tasks.

## 10. App icon small-size rule

- **Date:** 2026-09-29
- **Decision:** The mark's slice count follows its own drawn size: 4 slices at 32 pt and up, 3 below. Glow is a tile rule: simplified at 128 px, none at 64 px and below.
- **Reason:** The Brand page text said a slice drops "at 32 px" on one board and "below 32 px" on another; the drawn size ramps on both boards agree once the rule is read as the mark's size, not the icon's.
- **Consequences:** The Windows `.ico` uses the 3-slice mark at 16 and 24 px only. Details per output size are in `assets/icons/README.md`.

## 11. egui without its default fonts

- **Date:** 2026-09-29
- **Decision:** `egui` is used with its `default_fonts` feature off (workspace dependency `egui = { version = "0.36", default-features = false }`; `eframe` likewise). Glass ships its own fonts, Geist and Geist Mono (OFL), from `assets/fonts/` and installs them into the egui context.
- **Reason:** `default_fonts` bundles the Ubuntu font, whose licence has no FSF compatibility ruling, so `deny.toml` does not allow it. The design uses Geist and Geist Mono anyway (section 8 of `docs/ARCHITECTURE.md`).
- **Evidence:** Without any font, egui 0.36 lays out text with zero width and draws nothing, so the fonts are required, not optional. `cargo tree -e features -i epaint` shows no `default_fonts` feature enabled.
- **Consequences:** Until the Geist files are added to `assets/fonts/` (400, 500, 600 and Mono 400, 500, from the official vercel/geist-font release, with the OFL text), the headless test harness in `ui::headless` loads a system font at test time (never bundled) and draws shapes only when none exists.


## 12. Phases merge when their local Done-when parts pass

- **Date:** 2026-09-29
- **Decision:** Phase 1 is merged to main with its local Done-when parts verified (smoke exits 0, the full local check is green, the arm64 DMG builds within the size budget). CI green and tagged installers for both platforms stay open as needs-human tasks. From here on, a phase merges when every part of its Done when that can run on this Mac passes, and the rest is tagged needs-human.
- **Reason:** There is no GitHub remote and no Windows or NVIDIA hardware; holding main back would not change what can be verified.
- **Consequences:** Needs-human tasks remain open in PLAN.md and are resolved when a person pushes the repo, runs CI, and tests on the missing hardware.

## 13. dav1d in the pinned FFmpeg build for AV1 software decode

- **Date:** 2026-09-29
- **Decision:** The pinned FFmpeg build adds dav1d for AV1 software decode, with meson and ninja built from pinned sources as host tools.
- **Reason:** AV1 is a supported format (the `codec-av1` flag, the fixtures, export) and FFmpeg's built-in AV1 decoder only drives hardware decoders, which the M1/M2 lack (AV1 hardware decode arrived with the M3).
- **Details:** `scripts/ffmpeg/versions.env` pins dav1d 1.5.4 (BSD 2-clause), meson 1.12.1 and ninja 1.13.2 by URL and SHA-256. `build.sh` bootstraps ninja with its `configure.py`, runs meson from its unpacked source with `python3`, builds dav1d as a shared library into the same prefix, and configures FFmpeg with `--enable-libdav1d`. meson and ninja are build tools only and are not shipped; dav1d's `COPYING` joins `LICENSES/` and its archive joins the release source bundle.
- **Consequences:** The local prefix grows from 32 MB to 32.8 MB (libdav1d is 0.9 MB). `cargo deny` is unaffected (a C library). python3 must be on `PATH` where the FFmpeg build runs; the CI runners have it.

## 14. Content hash without the modification time

- **Date:** 2026-09-29
- **Decision:** The content hash that keys the project cache folder's per-file analysis is BLAKE3 over a version tag, the file size, the first and last 1 MiB and eight evenly spaced 64 KiB blocks. It does not include the modification time.
- **Reason:** The cache must survive relinking, renaming, moving and copying media (ARCHITECTURE section 6), and a copy gets a new modification time. Sampling keeps hashing at 2.5 MiB per file whatever its size.
- **Details:** `media::cache::ContentHash::of_file`; files up to 2 MiB plus one block are hashed whole. The accepted risk is two same-size files that agree in every sampled block; compressed media differs throughout after any change, and headers and MP4 indexes are always sampled. A version tag in the hash lets the sampling change later without stale matches.
- **Consequences:** Editing a file in place without changing its size or any sampled block would reuse stale analysis; tools that do that (metadata editors that rewrite a fixed-size field in the middle of a file) are rare for video. A user-facing "rebuild analysis" action covers it.


## 15. Working primaries follow the project colour space

- **Date:** 2026-09-29
- **Decision:** The render working space is linear light in the project primaries: BT.709 by default, BT.2020 when the project colour space is HDR (PQ or HLG). Sources are gamut-converted into it by a render-graph node (Phase 4).
- **Reason:** Most creator footage and delivery targets are BT.709, and BT.2020 keeps HDR sources unclipped; the project colour space setting (Phase 5) already chooses between them.
- **Consequences:** `media::ColourSpace` carries source primaries; Phase 4 adds the gamut conversion node; Phase 11 colour management may add ACES on top.


## 16. Parallel tasks in separate worktrees

- **Date:** 2026-09-29
- **Decision:** Independent tasks now run in parallel in separate git worktrees, including tasks from the next phases whose inputs are already built. Cargo commands are serialised by `scripts/cargo-serial` and every worktree shares the main checkout's target directory. The hard limits stay: at most 10 agents, one cargo command at a time, 4 compiler jobs, serial benchmarks.
- **Reason:** The user asked for faster progress, and most agent time is spent outside the compiler (reading, writing, reviewing), so only the compiler needs to wait its turn.
- **Details:** `scripts/cargo-serial` holds an exclusive `flock` on `<git common dir>/glass-cargo.lock` for the whole cargo command, test runs included. `scripts/dev-env.sh` points `CARGO_TARGET_DIR` at `<main checkout>/target` and `FFMPEG_DIR` at `<main checkout>/.glass-cache/ffmpeg/<host>`, so crates.io dependencies and `ffmpeg-sys` build once for all worktrees; workspace crates get separate artifacts per worktree because their paths differ. `scripts/worktree-setup.sh` prepares a new worktree. `PLAN.md` and `docs/DECISIONS.md` use git's union merge driver (`.gitattributes`) so branches that each append log lines merge without conflicts.
- **Consequences:** Replaces the earlier rule that only `(parallel)` tasks run beside the current phase task. With union merges, a line changed differently on two branches (the same checkbox marked `[~]` on one and `[x]` on the other) ends up in the file twice, so check `PLAN.md` after each merge. Killing `cargo-serial` with `SIGKILL` releases the lock while its cargo child may still run; stop it with Ctrl-C or `SIGTERM` instead.


## 17. `imbl` instead of `im` for the edit tree

- **Date:** 2026-09-29
- **Decision:** The project edit tree uses `imbl` 7 instead of `im` 15, the Phase 0 default.
- **Reason:** `cargo deny check` rejects `im`: it is unmaintained (RUSTSEC-2026-0248) and has an unsoundness advisory in `OrdSet` insertion with no fixed release (RUSTSEC-2023-0126). `imbl` is the maintained fork, with the same persistent `Vector` and `OrdMap`, the same structural sharing and the same licence (MPL-2.0, allowed by `deny.toml`).
- **Consequences:** Only `project` depends on it. The architecture doc's defaults table records the change.

## 18. Scoped checks on task branches, full test on the integration branch

- **Date:** 2026-09-30
- **Decision:** A task branch's finish check runs `cargo build --workspace`, `cargo test` for the crates it changed and every workspace crate that depends on them, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` and the smoke binary, holding the cargo lock once for the whole sequence. The full `cargo test --workspace` runs on the integration branch after each batch of merges; a failure there is fixed before the next batch merges.
- **Reason:** One full check takes about 30 minutes of cargo time on the M2 (the workspace test alone about 22 minutes) and about 65 minutes end to end with other agents queued, so running it on every task branch kept most agents waiting on the lock.
- **Consequences:** A breakage that only shows in an unrelated crate's tests is caught one batch later, on the integration branch, instead of on the task branch. A phase still merges to `main` only after a full check passes on its branch.

## 19. Shader hot reload dependencies are not profile-gated

- **Date:** 2026-09-30
- **Decision:** `render` depends on `notify` 8 (default features off, macOS FSEvents backend) and directly on `naga` 30 (the version wgpu 30 uses, `wgsl-in`) in every profile; the watcher (`shader-watch` thread, `ShaderLibrary::watching`) is compiled only under `debug_assertions`, and release builds use the embedded shaders.
- **Reason:** Cargo cannot choose dependencies by build profile: it does not support `debug_assertions` in `[target.'cfg(...)'.dependencies]` tables. naga is already compiled for wgpu, so a direct dependency costs nothing, and naga validation stays usable in tests of every profile.
- **Consequences:** Release binaries contain no watcher code. notify brings a second `windows-sys` version, which `cargo deny check` reports as a duplicate warning; `cargo deny check` passes.

## 20. Blend modes on linear light, HDR-safe

- **Date:** 2026-09-30
- **Decision:** Layer blend modes are evaluated on premultiplied linear light in the working space (W3C Compositing and Blending Level 1, source-over with a separable blend function B, plus Add and Subtract). Where a W3C formula would clip or dim an HDR backdrop above 1, Glass keeps it: Screen takes max(Cb, Cs) once either is above 1, Colour dodge caps at max(1, Cb) instead of 1, Colour burn returns Cb for Cb >= 1.
- **Reason:** The working space is linear light (Decision 15) and holds values above 1; a neutral screen, dodge or burn layer should leave highlights alone. For 0..1 values every mode equals the W3C function.
- **Details:** `render::blend` (CPU reference, f64) and `crates/render/shaders/composite.wgsl`; `crates/render/tests/composite.rs` compares them for every mode.
- **Consequences:** Overlay and Soft light look somewhat different from editors that blend encoded sRGB values. Soft light and the other modes extrapolate their 0..1 formulas above 1.

## 21. Layer transforms are sampled inside the composite pass

- **Date:** 2026-09-30
- **Decision:** A layer's transform (position, non-uniform scale, rotation, anchor) is applied inside `CompositeNode`'s pass: the shader maps each canvas pixel centre through the inverse matrix into the layer and resamples there (nearest, bilinear, or Catmull-Rom bicubic by default). There is no standalone transform node; opacity and crop are applied in the same pass. A transform that lands every pixel centre on a texel centre samples nearest whatever the setting. The shader has an entry point per filter and each pass uses the one for its most expensive layer filter; a per-pass cap (`CompositeNode::with_max_resample`) lets playback sample transformed layers bilinear, and paused frames and export use bicubic. The per-layer transform is a list of shutter samples (up to 16), of which the compositor renders the middle one until motion blur (Phase 13) averages them.
- **Reason:** Five 4K layers already take about 5.5 ms of the 8 ms Phase 4 budget, bound by memory bandwidth; a separate transform pass would add a full-frame `Rgba16Float` write and read per layer. Sampling in place costs only the extra texture fetches, which mostly hit the cache. Measured on the M2 (standing in for the M1): exact Catmull-Rom from nine bilinear fetches takes about twice the GPU time of bilinear for five full-frame 4K layers (texture-rate bound), a five-fetch approximation still about 1.6 times, and merely compiling the bicubic path into a pass slowed untransformed layers by about half, hence the per-filter entry points and the playback cap.
- **Details:** `render::transform` (matrix maths, `ShutterTransforms`, `Resample`, the CPU resampling reference), `nodes::Layer::reference`, `crates/render/shaders/composite.wgsl`; `crates/render/tests/transform.rs`.
- **Consequences:** A layer whose effects need its transformed pixels as a texture (a blur after scaling, for example) will need a transform node then; the maths is in `render::transform` for it. Strong downscaling samples without mipmaps and can alias.

## 22. Crop in layer pixels with an inward feather

- **Date:** 2026-09-30
- **Decision:** A layer's crop is four per-edge amounts in layer pixels, applied before the transform, plus a feather width in layer pixels. The feather ramps each edge of the crop rectangle inward with a smoothstep (0 half a pixel outside the edge, 0.5 at half the feather, 1 past it); with no crop it softens the layer's own edges. A hard edge is box-filtered over the canvas pixel's footprint. Opacity scales the layer's premultiplied colour before the blend.
- **Reason:** Cropping before the transform keeps the cut attached to the picture when it moves, turns or scales, as editors do. An inward feather never shows pixels the crop removed, and the smoothstep avoids the visible kink of a linear ramp's ends. Scaling premultiplied colour is how W3C compositing defines opacity and fades every blend mode towards the backdrop.
- **Details:** `render::crop` (CPU reference), `nodes::Layer::{opacity, crop}`, `crates/render/shaders/composite.wgsl`; `crates/render/tests/crop_opacity.rs`.
- **Consequences:** A proxy at a lower resolution than the source needs its crop, feather and transform scaled into proxy pixels by the caller (the same mapping as the transform's anchor).
