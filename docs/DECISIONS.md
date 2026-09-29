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
- **Reason:** The plan does not define the unit; this is how system-wide decode cost is usually stated. The per-core numbers are recorded in `proto-decode/RESULTS.md` so the result can be re-judged on the reference M1.
- **Evidence:** On the M2 (8 logical cores), the judged run (CVMetalTextureCache per frame, paced 60 fps, one stream) used 2.18% of the machine: 9.20% of one core in the process plus 8.26% of one core in VTDecoderXPCService, at 60.1 fps with 0 late frames.
- **Alternatives:** Judging against a single core. Measured that way, the same run is above 5% (9.20% in the process alone), so the unit decides the verdict; the per-core figures are kept for that reason.
- **Consequences:** Later "under X% CPU" budgets are judged the same way, with per-core numbers recorded alongside.

## 8. FFmpeg pinned to 9.0.x

- **Date:** 2026-09-29
- **Decision:** FFmpeg is pinned to 9.0.x instead of 7.x, still a GPL shared build with x264 and x265, used through `ffmpeg-next` 9.
- **Reason:** The maintained `ffmpeg-next` 9 bindings build against it, and local development and release builds should use the same FFmpeg.
- **Evidence:** proto-decode built the published `ffmpeg-next` 9 and `ffmpeg-sys-next` 9 crates against the Homebrew FFmpeg 9.0.1 shared libraries (libavutil 61.1.101) with `FFMPEG_DIR` set at build time, so no FFmpeg 7.1 build was needed.
- **Alternatives:** Building FFmpeg 7.x for older bindings.
- **Consequences:** The Phase 0 defaults table and the Phase 1 FFmpeg build script task name 9.0.x.
