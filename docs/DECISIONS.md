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
- **Details:** `scripts/cargo-serial` holds an exclusive `flock` on `<git common dir>/glass-cargo.lock` for the whole cargo command, test runs included. `scripts/dev-env.sh` points `CARGO_TARGET_DIR` at `<main checkout>/target` and `FFMPEG_DIR` at `<main checkout>/.glass-cache/ffmpeg/<host>`, so crates.io dependencies and `ffmpeg-sys` build once for all worktrees. (Corrected by Decision 28: workspace crates do not get separate artifacts per worktree, because cargo hashes their path relative to the workspace root.) `scripts/worktree-setup.sh` prepares a new worktree. `PLAN.md` and `docs/DECISIONS.md` use git's union merge driver (`.gitattributes`) so branches that each append log lines merge without conflicts.
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


## 21. Output encoding: inverse BT.1886 for SDR video, straight alpha, triangular dither

- **Date:** 2026-09-30
- **Decision:** `render::nodes::OutputNode` encodes SDR video tagged BT.709 with the inverse of the BT.1886 EOTF (a 1/2.4 power), not the BT.709 camera OETF; the preview and stills use sRGB; PQ is absolute (1.0 = 203 nits) and HLG goes through the inverse BT.2100 OOTF for a 1000-nit display. Outputs carry straight alpha unless a caller asks for premultiplied. 8, 10 and 16-bit integer targets get triangular-PDF dither of one code value from a PCG3D hash of pixel position and a per-frame seed; exact black and white are not dithered.
- **Reason:** players decode BT.709-tagged video with BT.1886, so the inverse EOTF makes display light round-trip with the working space, which is display-referred (the same convention `render::yuv` decodes with); the BT.709 OETF stays available (`OutputTransfer::Bt709`) for scene-referred delivery. Straight alpha is the delivery convention of ProRes 4444, PNG and most encoders. TPDF dither makes the quantisation error independent of the signal, removing banding without bias; varying the seed per frame keeps the pattern from sitting still.
- **Consequences:** Round trips through `media::TransferFn` are exact for sRGB, BT.1886, PQ and HLG. The dithered 8-bit output has no run of equal codes longer than a few pixels on a gradient that bands in 320-pixel steps undithered (`crates/render/tests/output.rs`).

## 22. Autosave in the per-user cache folder until projects have their own

- **Date:** 2026-09-30
- **Decision:** Autosaves go to `<cache>/autosave/<project uid>/`, where `<cache>` is the per-user cache folder (`~/Library/Caches/Glass` on macOS, `%LOCALAPPDATA%\Glass` on Windows, `$XDG_CACHE_HOME/Glass` on Linux, `GLASS_CACHE_DIR` to override) until the Phase 5 project settings task adds the per-project cache folder. The autosave format has its own version (`project::autosave::FORMAT_VERSION`, in every snapshot, log header and marker), separate from the project schema version, and autosaves of another format version are not read or migrated. The recovery prompt shows ages (`12 min ago`) instead of clock times.
- **Reason:** Project files and settings do not exist yet, so there is no project cache folder to write into, and the launch scan needs one known place to look. Autosaves are short-lived (deleted on a clean quit, on Discard, or seven days after the session was last alive), so migrating them is not worth the code; the saved project file is what schema migrations cover. Local clock times need a time zone library, which no task has chosen yet.
- **Consequences:** When project settings land, the app passes the project's cache folder to `engine::autosave` and the launch scan also looks in the recent projects' cache folders. The prompt's `autosave_time` and `saved_time` become local clock times once a date and time crate is added.

## 23. Layer transforms are sampled inside the composite pass

- **Date:** 2026-09-30
- **Decision:** A layer's transform (position, non-uniform scale, rotation, anchor) is applied inside `CompositeNode`'s pass: the shader maps each canvas pixel centre through the inverse matrix into the layer and resamples there (nearest, bilinear, or Catmull-Rom bicubic by default). There is no standalone transform node; opacity and crop are applied in the same pass. A transform that lands every pixel centre on a texel centre samples nearest whatever the setting. The shader has an entry point per filter and each pass uses the one for its most expensive layer filter; a per-pass cap (`CompositeNode::with_max_resample`) lets playback sample transformed layers bilinear, and paused frames and export use bicubic. The per-layer transform is a list of shutter samples (up to 16), of which the compositor renders the middle one until motion blur (Phase 13) averages them.
- **Reason:** Five 4K layers already take about 5.5 ms of the 8 ms Phase 4 budget, bound by memory bandwidth; a separate transform pass would add a full-frame `Rgba16Float` write and read per layer. Sampling in place costs only the extra texture fetches, which mostly hit the cache. Measured on the M2 (standing in for the M1): exact Catmull-Rom from nine bilinear fetches takes about twice the GPU time of bilinear for five full-frame 4K layers (texture-rate bound), a five-fetch approximation still about 1.6 times, and merely compiling the bicubic path into a pass slowed untransformed layers by about half, hence the per-filter entry points and the playback cap.
- **Details:** `render::transform` (matrix maths, `ShutterTransforms`, `Resample`, the CPU resampling reference), `nodes::Layer::reference`, `crates/render/shaders/composite.wgsl`; `crates/render/tests/transform.rs`.
- **Consequences:** A layer whose effects need its transformed pixels as a texture (a blur after scaling, for example) will need a transform node then; the maths is in `render::transform` for it. Strong downscaling samples without mipmaps and can alias.

## 24. Crop in layer pixels with an inward feather

- **Date:** 2026-09-30
- **Decision:** A layer's crop is four per-edge amounts in layer pixels, applied before the transform, plus a feather width in layer pixels. The feather ramps each edge of the crop rectangle inward with a smoothstep (0 half a pixel outside the edge, 0.5 at half the feather, 1 past it); with no crop it softens the layer's own edges. A hard edge is box-filtered over the canvas pixel's footprint. Opacity scales the layer's premultiplied colour before the blend.
- **Reason:** Cropping before the transform keeps the cut attached to the picture when it moves, turns or scales, as editors do. An inward feather never shows pixels the crop removed, and the smoothstep avoids the visible kink of a linear ramp's ends. Scaling premultiplied colour is how W3C compositing defines opacity and fades every blend mode towards the backdrop.
- **Details:** `render::crop` (CPU reference), `nodes::Layer::{opacity, crop}`, `crates/render/shaders/composite.wgsl`; `crates/render/tests/crop_opacity.rs`.
- **Consequences:** A proxy at a lower resolution than the source needs its crop, feather and transform scaled into proxy pixels by the caller (the same mapping as the transform's anchor).

## 25. Text: coverage atlas, vector outline strokes, graph shadow

- **Date:** 2026-09-30
- **Decision:** `render::text` rasterises glyphs to 8-bit coverage in one `R8Unorm` atlas (shelf packed, doubling up to 4096, then evicting glyphs unused this frame and repacking on the GPU). Outlines are round-join strokes rasterised by swash at twice the outline width, cached per glyph and width in the same atlas, instead of a signed distance field. The drop shadow is a separable Gaussian of the fill-and-outline union, as render-graph passes. cosmic-text 0.19 is built without its `fontconfig` feature.
- **Reason:** Coverage matches the CPU rasteriser exactly (GPU vs CPU max difference 0.0005), strokes stay crisp at any width without an SDF's range limit, and fontdb scans the standard font folders without fontconfig.
- **Consequences:** Each outline width is a separate atlas entry, so animating the outline width re-rasterises per step (quantised to 1/16 px). Per-glyph scale above 1 samples the atlas bilinearly; the text animator should lay out at the final size for large zooms. Colour glyphs (emoji) keep only their alpha and take the fill colour.

## 26. Media pool items are keyed by `MediaId`; colour labels use the clip palette

- **Date:** 2026-09-30
- **Decision:** A media pool item maps one to one onto a source media reference, so the pool keys items by `project::MediaId` (the draft's own `ItemId` is gone) and bins by a new `project::BinId` node id. Until the project root embeds the pool (Phase 5 schema work) the pool hands out ids from its own counter, and `MediaPool::insert_item` accepts ids chosen by the caller. The five colour labels are drawn in the clip roles: amber `clip/title`, blue `clip/video`, green `clip/audio`, red `clip/offline`, violet `clip/adjustment`, and always shown with their names.
- **Reason:** `project` already exports an `ItemId` for timeline items, and a clip's media reference and its pool entry are the same thing, so one id avoids a mapping table. The colour label menu on the `Sheet / Media pool` board uses exactly these clip colours, and the theme has no separate label roles.
- **Consequences:** Embedding the pool into `Project.media` later only swaps the id source. The search index is a derived cache and is never saved.

## 27. Frame queues: rtrb for one producer, crossbeam's ArrayQueue for the decode pool

- **Date:** 2026-09-30
- **Decision:** `engine::frame_queue` uses `rtrb` 0.4 for single-producer queues (render → present, one decoder → render) and `crossbeam-queue`'s bounded `ArrayQueue` for the multi-producer decode → render queue, with `crossbeam-utils`' `Parker` for waiting. Stale frames are flushed through a shared generation counter carried in every frame's stamp rather than by draining the queue under a lock.
- **Reason:** Both are lock-free, bounded and allocation-free per operation, and both pass `cargo deny check` (MIT or Apache-2.0, no new transitive crates; `crossbeam-utils` was already in the tree). `rtrb` is the fastest SPSC ring and can peek, which the present policy needs; it cannot take several producers, so the decode pool uses `ArrayQueue`. `std::sync::mpsc::sync_channel` (crossbeam's channel in std) was not used because a sender blocked on a full channel cannot be woken by a seek, and blocking decoders must return at once when the generation moves on.
- **Consequences:** Order is kept per producer only on the decode queue; render matches frames to requests by stamp. Push and pop fence once (`SeqCst`) to pair with the waiting flags.

## 28. `cargo-serial` forgets workspace fingerprints when the checkout changes

- **Date:** 2026-09-30
- **Decision:** `scripts/cargo-serial` records the top-level path of the checkout that last ran a cargo command in `<git common dir>/glass-cargo-last-checkout`. When a command comes from a different checkout (or the record is missing), after taking the lock and before running cargo it deletes the fingerprint directories of every workspace package (`cargo metadata --no-deps`) under every profile in the target directory (`CARGO_TARGET_DIR`, else `<main checkout>/target`), prints one line, and records the new path. Dependencies' fingerprints stay.
- **Reason:** Every worktree shares the main checkout's target directory (Decision 16), and cargo hashes a workspace member's path relative to the workspace root, so the same crate in two worktrees gets the same `.fingerprint/<pkg>-<hash>` directory and the same `deps/lib<pkg>-<hash>.rlib`. After worktree B builds a crate, worktree A's next build compares A's older source mtimes with B's newer outputs and treats them as fresh, so A compiles and tests against B's code: false "unresolved import" errors, or tests silently running another branch's build. Decision 16 wrongly said workspace crates get separate artifacts per worktree.
- **Consequences:** The first command after switching checkouts recompiles the workspace crates it needs (dependencies and `ffmpeg-sys` are not rebuilt). Only commands run through `cargo-serial` are protected: a plain `cargo` build in a worktree, or a worktree whose branch still has the old script, neither records its checkout nor clears the fingerprints, so every worktree should merge the phase branch to pick up the new script.

## 29. Offline media is runtime state, shown as a cached slate

- **Date:** 2026-09-30
- **Decision:** Whether a source file is available is checked by the engine (`engine::media_mgmt::offline`: missing, unreadable, or content hash changed) and kept in a map beside the project, never in the document. An offline clip renders `render::slate`: the viewer's offline state from the error and warning states sheet, drawn at the clip's size with sizes scaled from the 640 x 360 design frame, colours from the dark theme's roles (whatever the UI theme, since the slate is picture content that exports carry), without the design's Relink button (the preview draws controls over the picture). Each slate is rendered once per size and label and cached; frames import the cached texture.
- **Reason:** Keeping availability out of the tree means loading, editing, undo and saving never depend on the disk, and a relink or a remounted drive changes nothing in the document. One cached texture per size and label costs one layout and a few passes per offline file, then only an import per frame.
- **Details:** `engine::media_mgmt::offline`, `render::slate` (CPU reference `slate::reference`), `crates/render/shaders/slate.wgsl`; `crates/engine/tests/offline_media.rs`, `crates/render/tests/slate.rs`.
- **Consequences:** The design's glow blur (18 px) and backdrop blur are approximated analytically (the glows are already soft; the card's backdrop is the dot screen at its average cover). The file size is not stored on `MediaRef`, so the only content check is the hash (at most 2.5 MiB read per file). The slate uses Geist and Geist Mono when installed and falls back to system faces until they are bundled.

## 30. Loudness through the `ebur128` crate

- **Date:** 2026-09-30
- **Decision:** `media::analysis::loudness` measures with the `ebur128` crate (a Rust port of libebur128, MIT) instead of an in-house BS.1770-4 meter. Fixture references come from FFmpeg's `ebur128` filter and are matched within 0.1 LU integrated, 0.3 dB true peak and 0.5 LU loudness range.
- **Reason:** A maintained, reference-tested implementation of the standard; `cargo deny check` passes (it adds `bitflags` 1 beside 2, a duplicate warning only).
- **Consequences:** Loudness range differs from FFmpeg's by 0.3 to 0.4 LU on the stepped program fixtures (FFmpeg bins short-term values into a histogram); libebur128's values pass EBU Tech 3342, so the fixture tolerance is 0.5 LU rather than 0.1.

## 31. Project files: a versioned envelope, migrations on untyped JSON, newer files refused

- **Date:** 2026-09-30
- **Decision:** A project file is `{"format": "glass.project", "schema_version": N, "project": {...}}`. Loading migrates the `project` value one version at a time with pure `serde_json::Value` functions (`project::schema::migrate_vN_to_vN+1`), then deserialises the current model and validates it; saving always writes the current version. A file with a newer schema version is refused (`project.open_error.newer_version`), never read best-effort. Schema v2 stores `media` as `{"refs": [...]}` instead of a bare list.
- **Reason:** Migrating untyped values keeps only one set of model types (the current one) in the code, and each step stays small and testable. Reading a newer file would silently drop fields the newer build relies on, and saving it would destroy them. An object for `media` lets the media pool add its bins and metadata beside the references with a small, additive migration.
- **Consequences:** Every model change that alters the saved JSON bumps `SCHEMA_VERSION`, adds a migration and a `v<N+1>` fixture in `tests/fixtures/projects/`; old fixtures are never edited. `crates/project/tests/schema.rs` fails when `save` output changes without a fixture update.

## 32. Vector shapes by GPU coverage accumulation

- **Date:** 2026-09-30
- **Decision:** `render::shape` flattens curves and arcs to within 1/64 px and expands strokes into pieces that all wind the same way on the CPU, then rasterises outlines on the GPU by signed-area accumulation: every edge draws a quad from itself to the right edge of its outline's box and adds, per pixel, its height within the row times the fraction of the pixel to its right, with additive blending into an `R32Float` atlas. The sum is the winding number integrated over the pixel; non-zero clamps its magnitude, even-odd folds it. No tessellation crate (`lyon`) is used.
- **Reason:** Exact box-filtered anti-aliasing for straight edges without multisampling, both fill rules from one path, GPU work of about twice each outline's area regardless of its edge count, coverage that the mask node can reuse (`CoverageNode`), and no new dependency. Measured on the M2 (standing in for the M1): coverage within 2e-3 of exact polygon areas for straight edges and within 0.016 for curves; 50 mixed shapes into a 4K layer are reported in the Progress Log.
- **Consequences:** The fill rule applies to each pixel's integral, so a pixel where a twice-wound region meets an empty one (an inner stroke join, a pentagram's centre) is approximate (in the tests up to 0.06 at inner stroke joins and 0.35 at the inner corners of an even-odd pentagram). `R32Float` blending needs `wgpu::Features::FLOAT32_BLENDABLE`, which every Apple GPU has; when `app` sets up the shared device it should request it (otherwise `accumulation_format` falls back to `R16Float`, leaving about 1e-3 of coverage behind edges). Shape pipelines are built through `Renderer::custom_pipeline`, which other nodes with their own vertex stages can use.

## 33. Thread registry with seqlock accounting; decode pool of 2 to 4; thread-id for OS ids

- **Date:** 2026-09-30
- **Decision:** Every long-lived thread starts through `engine::threads::spawn`, which names it from its role and registers it; each entry's busy, idle and blocked totals are written only by the owning thread through a single-writer sequence counter over atomics, and the audio callback gets a pre-registered `AudioCallbackHook` that never allocates or locks. OS thread ids come from the `thread-id` 5 crate. The playback decode pool defaults to logical cores minus 3 (UI, render, audio decode with the callback), clamped to 2..=4. Render re-offers a frame to a full present queue every 2 ms instead of being woken by the presenter.
- **Reason:** The debug panel must read thread activity every frame without stopping threads, and the audio callback may not allocate or lock; a seqlock gives consistent copies with plain atomic stores on the writer side. Engine has no `unsafe` code, and `thread-id` (MIT or Apache-2.0, `libc`/`windows-sys` only) gives the id Instruments shows on macOS (`pthread_threadid_np`) and `GetCurrentThreadId` on Windows without allocating (the 0BSD `gettid` crate is outside `deny.toml`'s licence list). Four 4K software decoders stay near 0.6 GB on 8 GB machines, two are needed for a cut or transition, and the analysis pool pauses during playback. Render must not wait on the UI, so it cannot park on the present queue.
- **Consequences:** `shader-watch` (in `render`, which cannot depend on `engine`), `glass-crash-log` and `glass-file-picker` are named but not registered. `audio` stays free of Glass dependencies: engine hands it the hook as a closure. `thread-id` 5 brings a second `windows-sys` version on Windows (a `cargo deny` duplicate warning, not an error).

## 34. Master clock: callback frame counter, delay-locked loop, slewed output

- **Date:** 2026-09-30
- **Decision:** The master clock is the audio callback's frame counter, mapped to monotonic time by a second-order delay-locked loop (bandwidth 1.5 Hz narrowing to 0.1 Hz over a few seconds) fed with the monotonic time at callback entry plus cpal's reported output latency (playback minus callback timestamp; the buffer length when the host reports none). Readers never see the loop's estimate directly: the published line starts where the previous one is at each update and steers by changing its slope, by at most 0.5% of the rate. Only a gap over 100 ms re-seeds the loop. The presentation time is seconds on the stream's own timeline (frame over rate). Audio I/O uses `cpal` 0.18 (the Phase 0 default) and the callback's feed is an `rtrb` 0.4 ring; `cargo deny check` passes with both.
- **Reason:** Device crystals differ from the monotonic clock by up to a few hundred ppm and callback timing jitters by up to a millisecond, so neither the raw counter nor the last callback's timestamp is smooth enough for picking video frames. A DLL is the standard filter for this (JACK, PortAudio) and costs a few multiplications per callback. The monotonic reading at callback entry needs no mapping between cpal's per-host stream clocks and `Instant`; its cost is a constant lag of the mean wake-up delay.
- **Details:** `audio::drift`, `audio::Clock`, `audio::OutputStream`, `audio::OfflineOutput`; `crates/audio/tests/drift.rs` (0.1% fast and slow, 0 to 1 ms jitter, 256 to 1024-frame buffers), `crates/audio/tests/callback_alloc.rs`.
- **Consequences:** The scheduler owns the mapping from presentation time to timeline time (start offset, pause, shuttle) and subtracts `Clock::silent_frames` after underruns. Pausing the device re-seeds the loop on resume. `Clock::system` is the monotonic stand-in when no device is open; the offline backend covers tests and export. Video is slaved by `audio::video_sync::VideoSync`: the frame on screen at the middle of each refresh, with a 0.15-frame hysteresis band at boundaries, so device-to-display drift costs one repeated or skipped frame at a time and never touches the audio.

## 35. Rendered-frame cache: one LRU across VRAM and RAM, ceilings from the machine

- **Date:** 2026-10-01
- **Decision:** `render::frame_cache::FrameCache` keys frames by segment hash, frame index and a `RenderVariant` (size, quality step, settings hash), keeps one recency order across both tiers, spills VRAM evictions to RAM through a lossless `ReadbackRing` without waiting, stores them bit-exact by default (`SpillFormat::Srgb8` halves `Rgba16Float` frames as an option), and drops a frame whose spill cannot start. Default ceilings: a sixth of Metal's recommended GPU working set for VRAM, an eighth of physical memory for RAM (a sixteenth on unified memory), 1 GiB when the platform reports nothing. Physical memory comes from `objc2-foundation` (`NSProcessInfo`), already in the dependency tree.
- **Reason:** The plan keys by (segment hash, frame index) but adaptive quality renders the same frame at several sizes, which must not collide. Waiting for a spill would stall the render thread; dropping a frame only costs a re-render. Bit-exact spills keep cached and freshly rendered frames identical, which the deterministic-hashing rule relies on. Shares of the machine's memory leave room for decode, the compositor and the rest of the system on an 8 GB machine.
- **Consequences:** Phase 16's memory ceiling sets both ceilings at runtime (`set_vram_ceiling`, `set_ram_ceiling`). An `Srgb8` frame comes back as `Rgba8UnormSrgb`, so callers sample it rather than assume the original format.

## 36. LUTs: declared input space, tetrahedral interpolation in the shader

- **Date:** 2026-10-01
- **Decision:** `render::nodes::LutNode` applies a `.cube` LUT in the working space: alpha divided out, the working space converted into the LUT's declared input space (`render::lut::LutSpace`: Rec.709 display-referred by default, meaning BT.709 primaries and the inverse BT.1886 EOTF, a 2.4 power; sRGB; or linear light in the working primaries), the optional 1D shaper (linear interpolation) and the 3D table (tetrahedral interpolation), back to the working space, then mixed with the input by the intensity in linear light. Inputs outside a table's domain clamp to it, so HDR values above reference white clip in a display-referred LUT. The 3D table is an `Rgba16Float` 3D texture read with four `textureLoad`s and weighted in f32; the shaper is an `Rgba16Float` 2D texture in rows of 4096 entries. The `Renderer` binds an effect input as `texture_3d` when its texture is 3D (`EffectKey::volumes`). The project schema (`project::params::effects::lut`) stores the file as a path in a text parameter until the inspector has a file-pick row.
- **Reason:** Creative LUTs are almost all built for Rec.709 video code values, which is what the output node writes for SDR delivery, so the look matches other editors. Tetrahedral interpolation reads four entries instead of trilinear's eight, is exact on affine tables (identity included) and keeps the grey axis neutral; computing the weights in the shader avoids the 8-bit fixed-point fractions some GPUs use for hardware filtering of 3D textures, so the GPU matches the f64 CPU reference within f16 precision. f16 table entries are within 2^-11 relative, below one 10-bit code, at half the memory and bandwidth of f32.
- **Consequences:** Log input spaces (camera log curves) are added to `LutSpace` when the colour page needs them; their ids go into `project::params::effects::LUT_INPUT_SPACES` too (checked by `engine/tests/lut_ids.rs`). A 256-point table, the format's maximum, takes 134 MB of GPU memory; 33 and 65 points are 0.3 and 2.2 MB.

## 37. Portable mode: a `Glass Data` folder beside the executable

- **Date:** 2026-10-01
- **Decision:** Portable mode is on when a folder named `Glass Data` exists next to the executable; on macOS, when the executable is a bundle's `Contents/MacOS/` binary, next to `Glass.app` instead. In portable mode the config, data and per-user cache folders are `Glass Data/Settings/`, `Glass Data/Data/` and `Glass Data/Cache/`. `app::locations::Locations` is the only place that decides folders: each is the first writable candidate of its environment override (`GLASS_CONFIG_DIR`, `GLASS_DATA_DIR`, `GLASS_CACHE_DIR`), the portable folder, the platform folder (`dirs`), then `Glass` in the temporary folder. Candidates are created and probed with a test file; a read-only one is skipped with a logged warning.
- **Reason:** The folder is both the switch and the place the files go, so making a copy portable is one step and undoing it is deleting the folder; a marker file would need a second folder anyway. Environment overrides stay first because tests and developers rely on them. Falling back instead of failing keeps Glass usable from read-only media and from macOS App Translocation, which runs a quarantined app from a read-only random path.
- **Details:** `crates/app/src/locations.rs` and its tests (both modes, the bundle rule, overrides, read-only portable and platform folders); CONTRIBUTING.md, Portable mode; docs/ARCHITECTURE.md, File locations.
- **Consequences:** Projects keep their own `.glass-cache/` beside the project file in both modes. Logs have no folder yet. The crash monitor's socket may still use `<data>/run`; on a drive that cannot hold sockets it falls through to the next socket candidate. The Windows portable zip must ship the empty `Glass Data` folder.

## 38. Visual regression: a test-side scene registry, the UI comparison shared by path

- **Date:** 2026-10-01
- **Decision:** The compositor's visual regression suite lives in `render`'s integration tests (`crates/render/tests/visual_regression.rs`, registry in `tests/visual/cases.rs`), not in the library. A case is a scene per frame index (layers of procedural sources: NV12 bars through `YuvSourceNode`, uploaded stills, fills, text, vector shapes) rendered at 320 x 180 through `composite_stack` and `OutputNode::preview` with the frame index as the dither seed, compared with RGB golden PNGs. The comparison is `ui::snapshot::compare`, included by `#[path]`, with the compositor's own thresholds: 6 levels on up to 0.05% of the pixels on a GPU, 24 levels on up to 0.5% on a software adapter. Goldens are macOS-only (texts use the system Arial), like the UI snapshots.
- **Reason:** `ui` depends on `render`, so `render`'s tests cannot depend on `ui`, and one comparison is easier to keep right than two. Compositing is float maths without CPU text rasterisation differences, so it can be held tighter than egui: one pixel of movement along a layer edge fails, one dither code does not. Small frames with two or three indices per case keep the goldens near 50 KB each despite the dither noise.
- **Consequences:** Phase 4 masks and Phase 13 keying and tracking add a `Source` variant (or layer setting) and a case. When Geist is bundled, the texts and goldens can move to it and the suite can run on every platform.

## 39. Keymaps: one primary modifier, preset plus overrides, no sequences

- **Date:** 2026-10-01
- **Decision:** A chord is one key with `Cmd` (⌘ on macOS, Ctrl on Windows and Linux), `Ctrl` (the Mac Control key, folded into Ctrl elsewhere), `Alt` and `Shift`; shifted punctuation matches its base key with Shift. A keymap is stored as its preset's name plus per-command overrides (an override replaces that command's keys; an empty list unbinds it), as JSON with a format version, through `Keymap::load`/`save` at a path the app settings task chooses. No multi-key sequences: none of the four presets needs one. Presets give a command only the key the editor's published default list gives it, so commands an editor has no default for start unbound. Final Cut Pro's and Resolve's single trim key runs both ripple and roll trim and is not reported as a clash while neither command is changed. When two commands do share a chord, dispatch runs the one listed first in the registry and the editor flags the clash. The keyboard editor ships the board's window (keyboard map, key detail, changes, system keys, list with recording and clash resolution, Start from preset menu); switching preset keeps the user's changes on top (the board's default choice).
- **Reason:** One preset table then serves every platform, and storing only overrides lets a corrected preset reach users who never touched that command. Sequences would add a pending state to every key press for no preset's benefit.
- **Consequences:** The Switch preset dialog, saved sets, the preset comparison table and toasts on `Sheet / Keyboard`, and the Settings › Shortcuts pane, are follow-up sub-tasks. Sentences name chords in words (`Shift-S`) because the pinned snapshot fonts lack the modifier symbols, which keycaps draw as line icons; keycap labels use ‹ › where the board shows ◂ ▸ for the same reason.

## 40. Implicit gaps, per-kind tracks, markers in label colours

- **Date:** 2026-10-01
- **Decision:** A track stores only its items; gaps are the empty space between them, derived on demand (`Track::gaps`, `Track::item_at`), never gap items. Each track kind accepts one item kind: video and audio tracks hold clips, subtitle tracks hold subtitle cues, adjustment tracks hold adjustment items (`TrackKind::accepts`), and items on audio tracks sit on the sample grid, all others on the frame grid. Markers live on sequences (sequence time, frame grid) and on clips (source time, no grid), carry a start, a duration (zero for a point), a name, a note, a chapter flag, a to-do state and one of the five label colours (`media_pool::ColourLabel`, drawn in the clip theme roles), and are ordered by start then id.
- **Reason:** Stored gaps would carry no information the item ranges lack, yet every insert, remove, move and trim would have to create, split, merge or delete them and keep them in step with their neighbours, which is a whole class of invariant to get wrong. Source-time clip markers stay on the frame they mark when the clip is trimmed or moved. The design's marker popover offers exactly the label colours, and theme roles keep files free of RGB.
- **Details:** `project::model`, `project::marker`, schema v3 (`migrate_v2_to_v3`), autosave format 3 (`diff::Node::Marker`); `crates/project/tests/sequences.rs`, `crates/project/tests/schema.rs`.
- **Consequences:** Ripple and magnetic editing (Phase 6) compute gaps from item ranges. A clip marker outside the clip's visible source range is kept and not shown. A clip marker has no grid check because the project does not know source frame rates; the timeline snaps it when placing it.

## 41. Relinking: content hash decides, then weighted name and probe facts; one edit per batch

- **Date:** 2026-10-01
- **Decision:** `engine::media_mgmt::relink` walks the picked folders with the import scan and scores each file of the source's kind: an equal content hash is an exact match (100%, `Confidence::Exact`) whatever the name or folder; otherwise name similarity (case-insensitive stem, edit distance, containment) 0.35, duration 0.35 (a match within half a frame at the source's rate, at least 50 ms, scored by `1 / (1 + (Δ / 0.1 s)²)` so the nearer of two near-duplicates ranks first), resolution, frame rate and codec 0.1 each, over the parts both sides know, scaled by `0.6 + 0.4 × known weight`, times 0.9 when the hash is known to differ, capped at 95%. Tiers: Exact, Likely (at least 70%, picked for the user), Possible (40 to 70%, shown only). Every exact match's folder move (the paths without their common trailing components) is tried for the other offline sources. At most 2,000 files are hashed per search, the most likely first, and 8 are probed per source. A file shorter than the source range the clips use (less 1 ms) is listed but cannot be relinked, and `project::Project::relink_media` refuses it too. A batch is one `relink_media` edit (`EditLabel::Relink`) that replaces only `MediaRef` nodes, including the content hash.
- **Reason:** The content hash recorded at import is the one reliable identity (Decision 14), so it must outrank any name; a re-encode or proxy under the same name must never look identical. Names and lengths are what people recognise when the bytes changed. Repeating a move turns one found clip into a whole relinked card or drive. Replacing the hash keeps the next availability check from flagging a deliberately chosen re-export as changed.
- **Details:** `engine::media_mgmt::relink`, `project::relink`, `ui::dialogs::relink`; `crates/engine/tests/relink.rs`, `crates/project/tests/relink.rs`, `crates/ui/tests/relink_dialog.rs`.
- **Consequences:** Hashing reads up to 2.5 MiB per file, so a search of a large drive with no name or size hints may stop hashing before it reaches a renamed file; picking a narrower folder finds it. Relinking to a different file (hash changed) changes the analysis cache key, so its thumbnails and waveforms are recomputed.

## 42. About dialog: shipped third-party list, fixed brand colours for the app tile

- **Date:** 2026-09-30
- **Decision:** `scripts/licences/third-party.py` generates `assets/licences/third-party.tsv` from `cargo metadata` with `app/release-full` for every target platform plus the native libraries pinned in `scripts/ffmpeg/versions.env`. A crate counts as shipped when `app` reaches it through normal dependencies without passing a proc-macro; proc-macros and build and dev dependencies are listed as `dev` lines (name and version only) so the file names every registry package in `Cargo.lock`, and a test in `assets` fails when the two differ. Installers gather the shipped crates' licence files from their published sources (`LICENSES/third-party-crates.txt`, identical texts once) in `copy_licences`; no third-party text is committed. The source archives link is `<repository>/releases/tag/v<version>` from Cargo metadata. The About dialog follows `Dialog / About` on page 10 with the name and graphite app tile from the Brand page's About window; the tile is drawn from `assets/icons/source/mark-small.svg` (the mark is 31 pt in the 48 pt tile, below 32, so 3 slices; no glow at 64 pt and smaller) with fixed colours in `ui::theme::brand`. The update status pill is left out until the auto-updater exists, and the licence notices ("Read") use the third-party view's frame because page 10 has no board for them.
- **Reason:** Only code that ships needs a notice, and a committed list keeps the dialog offline, fast and testable while the test stops it going stale. The icon is artwork, the same in every theme ("the mark only ever changes size"), so it takes the Brand page's own values instead of UI roles.
- **Consequences:** Any dependency change needs `scripts/licences/third-party.py` rerun (the `assets` test and `copy_licences` say so). Native library descriptions are translated strings; crate descriptions are the crates' own English text. The dialog is not wired into a window yet (the app shell does not exist); `glass --version` prints the same details.

## 43. Compiler cap raised to 8 jobs

- **Date:** 2026-10-11
- **Decision:** `.cargo/config.toml` sets `[build] jobs = 8` and `scripts/dev-env.sh` exports `CARGO_BUILD_JOBS=8`, up from 4. `scripts/ffmpeg/build.sh` follows `CARGO_BUILD_JOBS`, so it also uses 8 make jobs after `dev-env.sh` is sourced.
- **Reason:** Development moved from an 8 GB Apple M2 to a 10-core Apple M5 with 24 GB RAM. Four jobs left half the cores idle, and `scripts/cargo-serial` still allows only one cargo command at a time, so the cap bounds one build, not the sum across agents.
- **Consequences:** Peak memory of a build roughly doubles. On a machine with less RAM, export a lower `CARGO_BUILD_JOBS` after sourcing `dev-env.sh` (the env var wins over the config file).

## 44. Masks: shape coverage unioned, Gaussian feather, combined in list order before the blend

- **Date:** 2026-10-11
- **Decision:** A layer's masks (`render::mask::Mask`) are shapes in layer pixels, rasterised by `shape::CoverageNode` (each shape's interior with its own fill rule, strokes ignored) and unioned per mask (per-pixel max). A mask's feather is a separable Gaussian of standard deviation feather / 3, truncated at the feather (rounded up to whole pixels) and renormalised, clamping to the layer's edge, capped at 250 px; from 12 px the GPU blurs at half resolution and from 32 px at a quarter (2 x 2 or 4 x 4 averages, a Gaussian narrowed by the variance that averaging and the bilinear read back add), within about 1% of the full kernel away from the layer's border. Invert (1 - c) follows the feather. Masks combine in list order: the first starts the result, each later one by its mode, union (max) or intersection (min). `nodes::MaskNode` multiplies the layer's premultiplied linear-light colour by the result before the layer is transformed and blended; `mask::add_to_graph` adds the coverage nodes and the mask node between a layer's source and its `CompositeNode`. Mask textures are `R16Float`.
- **Reason:** Reusing `CoverageNode` keeps one rasteriser with exact box-filtered edges (Decision 32). A symmetric Gaussian keeps a straight edge at 0.5 where it was, so feathering does not grow or shrink the mask, and truncating at three sigma confines it to the stated radius; the separable form costs about one bilinear fetch per pixel of feather per pass (taps paired in one fetch), which at 4K made a 20 px feather on three shapes take 6.6 ms of GPU time at full resolution against 2.3 ms at half (M5, standing in for the M1). Max and min are the usual union and intersection of soft masks and keep 0..1 without clamping. Masking before the transform keeps the mask attached to the picture, like the crop (Decision 24), and multiplying premultiplied colour is how opacity already works.
- **Consequences:** Every shape costs a canvas-sized coverage pass and every feathered mask two blur passes, whatever the shape's size; a mask limited to the shapes' bounds is an optimisation for later. Subtract and difference modes, mask opacity and expansion (grow or shrink) can be added as further `MaskMode`s and fields. Parameters for masks in `project::params` come with the inspector's mask UI.

## 45. Clip properties: derived source range, a speed map of keys, centre-relative transform

- **Date:** 2026-10-11
- **Decision:** Schema v4 gives every clip a speed map, an enabled flag, gain in dB, a transform, a crop, opacity, a blend mode and an effect stack (`project::clip`). The timeline range (the item's `range`, on the track's grid) is stored; the source range is derived as `source_start` plus the timeline duration times the speed, exactly in rational time. An edit that derives a timeline duration from a source duration (`set_clip_source_range`, `set_clip_speed` keeping the source range) rounds it to the nearest whole grid step, halves away from zero, at least one step, and the source duration follows the rounded value; `trim_item` moves the source start by the head trim times the speed. The speed map is a list of keys (source time, speed) of which v4 holds exactly one at source time 0, a reduced positive rational from 1/100 to 100; reverse and freeze stay out until Phase 7. The transform stores position as the anchor's offset from the canvas centre in canvas pixels and the anchor as an offset from the layer centre in layer pixels, with per-axis scale (negative flips, up to 100 either way) and clockwise degrees, so the default (all zero, 100%) is `render::transform::Transform2D::centred` for any canvas and layer size; crop and feather are layer pixels as in Decision 24. Blend modes serialise as the `params::builtin::BLEND_MODES` ids. Each effect in the stack is a node with its own id from the project counter, the id of the `ParamSchema` its values follow, an enabled flag and `ParamValues` filled with the schema's defaults when added, and the stack applies first to last before crop, transform, opacity and blend. Every clip carries every property, whatever its track. Edit operations clamp numbers to their ranges and refuse non-finite ones; `Project::validate` refuses out-of-range values in a loaded tree. Autosave format 4 began with schema v4.
- **Reason:** Storing both ranges would let them disagree; the timeline range is the one that must sit on the grid, so it is primary and the other is exact arithmetic. A list of keys holds keyframed ramps later with the same JSON shape for constant speeds, so Phase 7 needs no data migration. The project knows neither the canvas nor the source picture size, so absolute pixel positions could not express "centred" for a new clip; centre offsets also survive a change of project resolution. Storing defaults in each effect keeps a saved look stable if a schema's default changes later. Keeping transform fields on audio clips and gain on video clips costs a few bytes and spares every edit a track-kind check and every track kind change a conversion.
- **Details:** `project::clip`, `project::model::Clip`, `project::schema::migrate_v3_to_v4`, `project::time::Time::round_to_multiple`; `crates/project/tests/clip_props.rs`, `crates/project/tests/schema.rs`, `tests/fixtures/projects/v4-*.json`.
- **Consequences:** The `params::builtin::transform` schema's position default (960, 540) and anchor (0, 0) are absolute pixels and uniform scale; the inspector's transform section must map to the clip's centre offsets and per-axis scale when it is wired to clips. Phase 7 ramps extend `SpeedKey` (an interpolation field with a serde default) and relax the one-key check, with a schema bump and an identity migration so older builds refuse those files as newer. Keyframe curves (Phase 5) animate `transform/position` and the other built-in section ids on top of these static values.
## 46. Vector shapes: sparse strips over the coverage accumulation

- **Date:** 2026-10-11
- **Decision:** Extends Decision 32. The signed-area accumulation stays exact and per pixel, but each edge's quad no longer runs to its outline box's right edge. `render::shape::strips` bins every outline on the CPU into bands of 4 canvas rows; in a band, the columns each edge crosses (`floor(min x)` to `ceil(max x)`, at least one) are merged into intervals, and the height of every edge in each row is added up left to right in f64 (the carry). An edge's accumulate quad covers its rows in the band from its first column to the end of its interval; paint pieces cover each interval (accumulation plus the carry of its row, then the fill rule) and each gap between intervals that the fill rule gives coverage (the carry alone, no atlas read); gaps left empty are not drawn. Pieces with the same columns and data that continue the band above are merged. `CoverageNode` draws the same paint pieces into its coverage texture instead of a full-canvas resolve. Carries within 1e-5 of a whole number are snapped to it.
- **Reason:** Right of every edge crossing a pixel row the row's winding integral is constant, so the accumulate quads only needed to reach the next edge. On the 50-shape 4K frame the old quads shaded 29.9 M blended accumulate fragments and 12.8 M paint fragments (9.3 ms GPU median on the M2, standing in for the M1); sparse strips shade 0.4 M and 5.3 M. On the M5 (machine shared with other agents), release: GPU median 1.72 ms before, 0.75 ms after; debug test profile: 1.7 to 2.6 ms median with a p99 up to 8.2 ms before, 3.45 ms median and p99 at most 7 ms after (the debug figure follows the slower CPU loop more than the GPU work). Coverage against the CPU reference and the visual regression goldens are unchanged. A tile-based winding scheme (per-tile backdrops, as in GPU path renderers) would need a compute pass and a second data path for masks; the strips keep the render-pass graph and the one fragment formula.
- **Consequences:** Binning moves work to the CPU: building the 50-shape node takes 1.6 to 1.8 ms in release (0.4 to 0.6 ms before), about 1 ms of it binning 25,749 edges, and 18.6 ms in the unoptimised test build (3 ms before). The accumulation atlas is written only inside intervals, so it means nothing without the paint pieces; any new reader (a mask node that wants raw winding) goes through `CoverageNode`. Paint is now one instance per piece (23,215 on the test frame) instead of one per outline.

## 47. Export encoder wrapper in `media::export`; native ProRes on macOS; GIF through the `gif` crate; AV1, VP9 and MP3 wait for the FFmpeg build

- **Date:** 2026-10-11
- **Decision:** The Phase 8 encoder wrapper is a new public module, `media::export` (`ExportSession`, `ExportSettings`, `VideoSettings`, `AudioSettings`, `Picture`), not a new crate: ARCHITECTURE section 2 already gives `media` encoding and muxing and `engine` the export pipeline, and `media` is the only crate allowed `ffmpeg-next`. RGB pictures from the readback ring become YUV inside the session with swscale (`media::ffi::Sws`, plane sizes checked before swscale reads caller memory) using the colour tags' matrix, limited range and left chroma siting; the same tags go into the stream. Encoders: `libx264` and `libx265` (CRF 23 and 28 by default, `hvc1` for HEVC in MP4 and MOV); ProRes through `prores_videotoolbox` on macOS with `allow_sw` on, and through `prores_ks` only with the `codec-prores-sw` feature (on other platforms, or when named explicitly); FFmpeg's own `aac` (128 kbit/s per channel by default) and `flac` (24-bit from `s32`); PCM in WAV, MOV and Matroska. PNG and TIFF sequences are written one file per frame by the session from the `png` and `tiff` encoders' packets, not through FFmpeg's `image2` muxer, because `ffmpeg-next` opens the output path before the muxer runs and would leave an empty file named after the pattern. GIF goes through the `gif` crate already in the tree (a NeuQuant palette per frame, transparent where alpha is 0, delays rounded on the running total, looping, at most 50 fps), not FFmpeg's `gif` encoder, which needs `libavfilter`'s palette filters for usable colour. AV1, VP9 and MP3 are in the API and resolve to `libsvtav1`/`libaom-av1`/`librav1e`, `libvpx-vp9` and `libmp3lame`, but report `ExportError::Unavailable` because the pinned FFmpeg 9.0.2 build has none of them; adding them means new pinned source archives in `scripts/ffmpeg/versions.env` (a download) and a rebuild, so that sub-task is needs-human.
- **Reason:** One module keeps the dependency rules and the `unsafe` boundary as they are. `allow_sw` keeps ProRes export working on the M1, which has no ProRes engine, with Apple's own encoder. FFmpeg's `aac` is the same on every platform (AudioToolbox's `aac_at` is macOS-only). The `gif` crate gives a per-frame palette without enabling another FFmpeg library.
- **Consequences:** The hardware encode task plugs `h264_videotoolbox`, `hevc_videotoolbox` and the others in through `VideoSettings::encoder` (any encoder of the same codec is accepted, except `prores_ks` without its flag). HDR exports carry colour tags only; mastering display and content light level metadata are left to the preset task. The fixture generator's VP9 clips also need `libvpx`, which the pinned build lacks.

## 49. Hardware encode: an encoder preference with fallback on open and on the first frames

- **Date:** 2026-10-11
- **Decision:** `media::export::VideoSettings` gains `encoder_preference: EncoderPreference::{Auto, Hardware, Software}`, `Auto` by default, the same three-way choice as `ProxyEncoder`. `Auto` tries the platform's hardware encoders for H.264 and HEVC (`*_videotoolbox` on macOS; `*_nvenc`, `*_qsv`, `*_amf` in that order on Windows and Linux; ProRes keeps `prores_videotoolbox`) and then `libx264` or `libx265`; `Hardware` never falls back; `Software` skips them; codecs without hardware encoders ignore it; `VideoSettings::encoder` still names one encoder with no fallback. Hardware encoders get NV12, or P010 for 10-bit settings (HEVC Main 10; hardware H.264 is skipped for anything over 8 bits), and a constant quality mapped from the CRF (the software default, 23 or 28, for `RateControl::Default`): VideoToolbox `-q:v` = 100 - 1.75 x CRF (CRF 23 gives 60, near x264 CRF 23 in PSNR and size), NVENC `rc vbr` with `cq`, Quick Sync ICQ `global_quality`, AMF `cqp` with `qp_i/p/b`; the preset goes to software encoders only. An encoder that fails to open is replaced at `ExportSession::create`. One that opens stays on trial until its first packet (at most 8 frames): the session keeps the frames it sent and adds no muxer streams yet (audio packets wait), and an error from sending or receiving replaces it with the next encoder, the frames converted with `FrameScaler` when the pixel format changes and replayed, so the file holds only the replacement's packets. `ExportSummary::{video_encoder, fallbacks}` and `ExportSession::fallbacks` report what ran and what was given up on, with FFmpeg's reason. The NVENC, Quick Sync and AMF names, formats and rate options are selected with `cfg!` so they compile on every platform, but they are not verified on hardware (needs-human).
- **Reason:** Speed comes first (Decision Log, 2026-09-29), and VideoToolbox encodes several times faster than x264 and x265 at a similar quality, so `Auto` is the default. A hardware encoder can fail where software never does (picture sizes beyond its limits, a busy or missing GPU, a driver without the codec), and most failures show at open or on the first frame, so those two points are covered without buffering more than a few frames. Constant quality keeps hardware exports from FFmpeg's 200 kbit/s default bit rate and lets one CRF setting drive both kinds of encoder.
- **Consequences:** The same settings can produce different bitstreams on different machines; tests that need a fixed encoder set `Software` or name it. A hardware encoder that fails after its first packet fails the export, as a software one does. VideoToolbox takes colour tags from the frames, which the session's converter sets. Verifying NVENC, Quick Sync and AMF (encode, fallback, tags, quality mapping) needs Windows or Linux machines with those GPUs.

## 48. Transitions: a folder per transition, a shared WGSL prelude, easing on the CPU

- **Date:** 2026-10-11
- **Decision:** A transition is a folder `assets/transitions/<id>/` holding `transition.json` (JSON, since `serde_json` is already a dependency and the project file is JSON: id equal to the folder name, i18n name key, category, offered directions from eight compass directions with the first as default, thumbnail progress, at most 8 parameters written as `project::params::ParamDef` JSON, text parameters refused) and `shader.wgsl`. `render::transition::TransitionRegistry` discovers them at startup (the source tree's folder in debug builds, copies embedded by the `assets` build script in release builds); a folder whose manifest or shader fails is reported and skipped. Every transition shader is compiled as `common.wgsl` + `transition.wgsl` + its own file and defines `@fragment fn fs`; the prelude fixes the bindings (uniform, sampler, A, B) and one uniform layout for all: eased progress, linear progress, direction index, direction unit vector, output size, then eight `vec4` parameter slots in manifest order (numbers in x, angles in radians, booleans 0 or 1, choices as the option index, points in xy, colours as straight linear RGBA). Easing is applied on the CPU before the uniform: linear, CSS ease-in `(0.42, 0, 1, 1)`, ease-out `(0, 0, 0.58, 1)`, ease-in-out `(0.42, 0, 0.58, 1)`, or any `cubic-bezier` (control x clamped to 0..1, y free to overshoot); every curve is exactly 0 at the start and 1 at the end. The shader name is `transition/<id>`; the shader library treats it as an external shader with a prelude, watched in its own folder in debug builds (Decision 19), and reloads every transition when the prelude changes. Thumbnails are not stored: the browser renders the transition at its thumbnail progress (`TransitionNode::thumbnail`).
- **Reason:** One fixed uniform and bindings make a new transition a single WGSL file with no Rust, which is what the 150-item library needs; 8 slots cover the plan's transitions (the widest, iris and shape wipes, need a shape, a centre, a softness and a colour). Easing on the CPU keeps it out of every shader and lets the timeline and the keyframe curve editor share one implementation. `render` cannot depend on `project`, so it keeps the definitions as JSON and reads only what it packs; the engine builds the `ParamSchema`, and a test there validates the bundled ones. Rendered thumbnails follow the current frame like the LUT browser's and add no binary files.
- **Details:** `crates/render/src/transition/`, `crates/render/shaders/transition.wgsl`, `crates/render/src/shaders.rs` (`add_external`), `crates/assets/src/transitions.rs`, `assets/transitions/README.md`; tests `crates/render/tests/transition.rs`, `crates/engine/tests/transition_schemas.rs`, visual case `cross_dissolve`.
- **Consequences:** A transition with more than 8 parameters, or a text one, needs the prelude to grow (a uniform layout change for every shader). Discovery happens once per process, so a new folder needs a restart; only shader edits hot reload. The timeline's transition items (Phase 5) store the id, linear duration, `Easing`, direction id and parameter values, and hand them to `TransitionNode`. Transitions from asset packs are discovered by calling `TransitionRegistry::discover` on the pack folder.
## 49. Look-ahead decode: one worker and one lane per segment

- **Date:** 2026-10-11
- **Decision:** `engine::lookahead` decodes ahead of the playhead across cuts and transitions as its own pipeline beside `engine::playback`'s skeleton. A `DecodePlan` (engine-local; `SegmentList` until a sequence-backed plan exists) lists `PlanSegment`s: source, timeline frame range, source time at the first frame, layer and the frame cache's `SegmentHash`. `glass-sched` hands each segment in timeline order to one idle `glass-decode-<n>` worker together with a new `spsc` lane of `depth` frames, whose consumer goes to `LookaheadFrames` (render's end) with a horizon message saying how far the plan has been handed out. The worker opens and seeks the source (pre-roll) and fills the lane with one frame per timeline frame (the last source frame at or before that instant), parking while it is full. At most `decode_workers + 1` lanes are open; at most `decode_workers` segments may overlap, and a segment that would exceed that is dropped with a warning. Depth defaults to as many 4:2:0 frames as fit 64 MiB (`LANE_BUDGET`), clamped to 2..=8: 8 at 1080p, 5 at 4K 8-bit, 2 at 4K 10-bit. `MediaDecoder` seeks with the playback decoder (`HwPreference::Auto`, `PreRoll::Steady`); `PreRoll::Software` (exact seek in software, hand-off to the hardware decoder's `seek_keyframe` at the next keyframe) stays available.
- **Reason:** Real decoders decode sequentially and seek expensively, so the skeleton's frame `n` to decoder `n % N` round robin would seek on every frame; one worker per segment decodes it in order, and a free worker pre-rolls the next segment while the current one plays. A lane per segment bounds decoded memory per stream and lets both sides of a transition decode at once. Measured on the M5 (shared with other agents; it stands in for the M1 reference) with `tests/lookahead.rs`: three cuts across two 1080p files (H.264 30 fps and HEVC 10-bit 25 fps), all mid-GOP, played against the system master clock with 60 Hz `VideoSync`: 0 late frames of 114 in all three set-ups, every cut ready 111 to 116 ms before its refresh (render pulls three frames ahead), pre-roll mean 42 ms with VideoToolbox, 38 ms in software, 51 ms with the software hand-off; 4K cuts inside one file: 145, 147 and 176 ms, 0 late. VideoToolbox was not slower to pre-roll than software here (the M2 handoff numbers were 51.5 vs 13.6 ms at 1080p), and the hand-off pays a second open and a GOP scan while short segments never reach hardware decode.
- **Details:** `engine::lookahead` (`mod.rs`, `plan.rs`, `media_decoder.rs`); `crates/engine/tests/lookahead.rs`.
- **Consequences:** The playback task that wires render replaces the skeleton's request rings with `LookaheadFrames` and builds the plan from sequences and clips (clip properties, speed). A run of segments each shorter than a pre-roll, more of them than there are workers, can still arrive late; the frame cache can skip decoding cached ranges later. On a machine whose hardware seeks are slow, switch the default to `PreRoll::Software` after measuring with the same test.

## 50. Dissolve family: dips by a midpoint, film in a 2.4 power curve, additive capped at max(white, brighter input)

- **Date:** 2026-10-11
- **Decision:** All dissolves work on premultiplied linear light in the working space and are exactly A at eased progress 0 and B at 1. Dip to black, white (1.0, diffuse white) and colour: A goes to the premultiplied colour by the `midpoint` parameter (5% to 95%, default 50%), then the colour goes to B; each half is a weighted sum with its weight clamped to 0..1, so an overshooting ease holds rather than extrapolates. Film dissolve: each straight (unpremultiplied) channel is encoded as sign(x)·|x|^(1/2.4) (the inverse of the BT.1886 power, odd so negative wide-gamut values and HDR values above 1 follow the same curve), the premultiplied encoded pictures are mixed by progress, and the result decoded with the 2.4 power; a pixel with zero alpha is treated as opaque. Additive dissolve: A weighted min(1, 2(1 − t)), B weighted min(1, 2t), summed, alpha capped at 1 and each colour channel capped at max(alpha, A·wa, B·wb). Non-additive dissolve: the per-channel (alpha included) maximum of the same weighted inputs. Film and non-additive return A and B themselves at 0 and 1.
- **Reason:** A display-like power is what makes a dissolve read as a film optical (the middle passes through the darker picture: black to white is 0.19 in linear light half way, not 0.5), and BT.1886's 2.4 matches the output encoding (Decision 21). Capping the additive sum at white keeps SDR pictures in range, and keeping the brighter input's value above white follows Decision 20 (highlights are neither clipped nor pushed further). An encode-decode round trip and a maximum against zero are not exact in floating point, so those two branch at the endpoints.
- **Details:** `assets/transitions/{dip-to-black,dip-to-white,dip-to-colour,film-dissolve,additive-dissolve,non-additive-dissolve}/`; tests `crates/render/tests/dissolves.rs` (f64 references), visual cases `dip_to_black` to `non_additive_dissolve`.
- **Consequences:** The film dissolve's curve is fixed (no gamma parameter). The dips share their WGSL function by copy, since a transition is one file over the shared prelude.
