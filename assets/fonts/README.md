# Fonts

The Glass UI is set in **Geist** and **Geist Mono** (Vercel, SIL Open Font License 1.1). The font files are not in the repository yet. Until they are, the app and the headless tests read a system UI font at run time (never bundled, never copied here) and log which one they used.

## Files to add

Take them from the official release of [vercel/geist-font](https://github.com/vercel/geist-font/releases) (the release zip, TTF folder), and put exactly these five files in this folder:

| File | Where it is in the release | Used for |
| --- | --- | --- |
| `Geist-Regular.ttf` | `fonts/Geist/ttf/` | UI text, 400 |
| `Geist-Medium.ttf` | `fonts/Geist/ttf/` | titles, display, emphasis, 500 |
| `GeistMono-Regular.ttf` | `fonts/GeistMono/ttf/` | timecode and numbers, 400 |
| `GeistMono-Medium.ttf` | `fonts/GeistMono/ttf/` | emphasised numbers, 500 |
| `OFL.txt` | repository root of the release | the licence, shipped with the fonts |

Use the static TTF files, not the variable fonts: egui renders a font's default instance only, so a variable file would always draw at its default weight. Record the release version in this README when you add them.

`cargo deny` allows `OFL-1.1` for font files shipped as data (see `deny.toml`). The installers must carry `OFL.txt` next to the fonts.

## How they are loaded

`crates/assets/build.rs` embeds each of the four TTF files that exists here at build time, so a release binary carries its fonts. At run time `assets::fonts::load` uses, per face:

1. the embedded file;
2. for Medium faces, the embedded Regular face of the same family;
3. a system font from the standard OS locations (San Francisco, Geneva or Arial on macOS; Segoe UI or Arial on Windows; DejaVu, Noto or Liberation on Linux), mono faces falling back to the proportional one.

It logs which source it used (`fonts: bundled Geist` or `fonts: system fallback ...`). `ui::fonts::install` turns the result into egui font families: `Proportional` (Geist 400), `Monospace` (Geist Mono 400), and the named families `medium` (Geist 500) and `mono-medium` (Geist Mono 500).
