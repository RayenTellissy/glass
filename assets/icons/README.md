# App icons

Exported from the Paper file "Inspiration 2", page "Brand", boards
`Brand / Identity` and `Brand / App icon & in use`. Everything in `macos/`,
`windows/` and `linux/` is generated from `source/` by `./build.sh` (macOS with
Xcode 26 or later, plus `python3`; no other tools).

## Sources (`source/`)

Exported with Paper's export tool and committed unchanged. They are the only inputs.

| File | Paper node | What it is |
| --- | --- | --- |
| `mark.svg` | Identity, hero "Mark" | The mark, 4 slices, white, 200×200 viewBox |
| `mark-small.svg` | App icon ramp, "Mark small" | The small mark, 3 slices, corner radius 14 instead of 11 |
| `mark-ink.svg` | App icon, "Mark ink" | The mark in ink (#141414) for the Light tile |
| `mark-clear.svg` | App icon, "Mark white" | The mark for the Clear tile (slices 30 to 85% white) |
| `mark/mark-{32..512}.png` | hero "Mark" | Bare mark, 4 slices, exported at each pixel size |
| `mark/mark-small-{16,24}.png` | "Mark small" | Bare mark, 3 slices, exported at each pixel size |
| `tile/graphite-{1024,824,412,206}.png` | "App icon / Graphite" (300 px) | Full graphite tile with caustic, frame glow and sheen |
| `tile/graphite-128pt-{103,206}.png` | Ramp "Icon / 128" | 128 tile: simplified glow, 4 slices |
| `tile/graphite-32pt-{26,52}.png` | Ramp "Icon / 32" | 32 tile: no glow, 3 slices |
| `tile/graphite-16pt-{13,26}.png` | Ramp "Icon / 16" | 16 tile: no glow, 3 slices, edge line only |
| `tile/light-1024.png`, `tile/clear-1024.png` | "App icon / Light", "App icon / Clear" | Reference renders of the other two appearances |

Each PNG was exported at its final pixel width, so Paper rasterises the vector
at that size. Nothing is downscaled from 1024. The ramp's "Icon / 64" tile
(no glow, 4 slices) is not exported, because no `.icns` slot is 64 points.

## Small-size ruling (for the Decision Log)

The boards disagree about exactly 32 px. The App icon board's caption says "a
slice drops out at 32 px", and its drawn ramp puts the 3-slice mark in the 32
tile. The Identity board's caption says "below 32 px the mark drops a slice",
and its drawn ramp labels a 32 px bare mark "4 slices".

Ruling: the drawn ramps are the spec, and both are right once the rule is read
as applying to the mark's own drawn size in points, not the icon's size. The mark keeps
4 slices when it is drawn at 32 px or larger, and drops to 3 slices below 32 px.
The App icon ramp agrees: the mark inside the 32 tile is 21 px, and inside the
16 tile it is 12 px, so both use 3 slices. The mark in the 64 tile is 40 px, so
it keeps 4. Glow is a tile rule: the 128 tile has it, and the 64 tile and
smaller do not.

What each output uses:

| Output | Size (px) | Mark drawn at | Slices | Glow |
| --- | --- | --- | --- | --- |
| `.icns` 16 pt @1x/@2x | 16 / 32 | 10 / 20 px (10 pt) | 3 (16 tile) | no |
| `.icns` 32 pt @1x/@2x | 32 / 64 | 17 / 34 px (17 pt) | 3 (32 tile) | no |
| `.icns` 128 pt @1x/@2x | 128 / 256 | 64 / 129 px (64 pt) | 4 (128 tile) | simplified (128 tile) |
| `.icns` 256 pt, 512 pt @1x/@2x | 256 to 1024 | 129 to 516 px | 4 (hero tile) | full (hero tile) |
| `glass.ico`, Linux PNGs | 16, 24 | 16, 24 | 3 (`mark-small`) | none (bare mark) |
| `glass.ico`, Linux PNGs | 32 to 512 | 32 to 512 | 4 (`mark`) | none (bare mark) |

The `.icns` slots follow the point size. The 32 pt @2x image (64 px) uses the
32 tile drawn at 2x, not the 64 tile, because both rules follow the size on
screen in points. Its mark is 34 px but 17 pt, so it keeps 3 slices.

## Outputs

### `macos/AppIcon.icns`

The graphite (default) appearance, all ten slots from 16 to 1024, built with
`iconutil`. The tile fills 824 of the 1024 canvas, with transparent padding,
on Apple's macOS icon grid. The board's "22% corner radius" matches that grid's
rounded square. The board's drop shadow is not added (Paper exports clip at the
tile edge). Use it as `CFBundleIconFile` for macOS before 26.

### `macos/AppIcon.icon` and `macos/Assets.car`

An Icon Composer bundle, compiled by `actool` (Xcode 26.6) into `Assets.car`.
Use it with `CFBundleIconName = AppIcon`. `assetutil --info` lists renditions
for `NSAppearanceNameAqua`, `NSAppearanceNameDarkAqua` and
`ISAppearanceTintable`.

| System appearance | What shows |
| --- | --- |
| Light (default) | Light tile (white to #E4E4E4) with the ink mark |
| Dark | Graphite tile (oklab 28.9% to 16.4%) with the white mark and the caustic and frame glow |
| Clear (light and dark) | Rendered by the system from the white Clear mark: a translucent tile with a white mark, as on the board |
| Tinted | Rendered by the system from the same mark |

Notes:
- The layers have Liquid Glass, shadow and translucency turned off. The Misuse
  board says "don't add glows, shadows or outlines" to the mark.
- In the dark rendition, macOS brightens translucent layers to full white. So
  the dark mark's slice alphas are pre-composited over the tile's mid grey
  (`tools/svg.py --flatten 0.09`).
- Icon Composer's renderer ignores `#RRGGBBAA` fills. `tools/svg.py` rewrites
  them as `fill-opacity`.
- The graphite glow is redrawn as SVG radial gradients (`tools/glow.svg`),
  because CSS blur has no SVG equivalent that Icon Composer draws. The board's
  top sheen is left to the system's specular highlight.
- Gradient stops are the board's oklab values converted to sRGB grey. Each
  tile uses two stops. The board's middle stop is within 0.02 of the straight
  line between them.
- To preview a rendition:
  `"/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool" macos/AppIcon.icon --export-image --output-file out.png --platform macOS --rendition Dark --width 512 --height 512 --scale 1`
  (the renditions are Default, Dark, ClearLight, ClearDark, TintedLight and TintedDark).

### `windows/glass.ico`

The bare mark (no tile, per the ramp's "Windows and Linux · bare mark") at 16,
24, 32, 48, 64, 128 and 256. It is PNG-compressed and written by `tools/ico.py`,
which also lists entries: `python3 tools/ico.py list windows/glass.ico`.

### `linux/`

`glass-{16,32,48,64,128,256,512}.png`: the bare mark, for
`share/icons/hicolor/<n>x<n>/apps/glass.png`. `glass.svg` is the 4-slice mark
for `hicolor/scalable/apps`, with fills rewritten as `fill-opacity` for older
librsvg. Desktop environments prefer the fixed-size PNG when one exists, so the
16 px menu size still gets the 3-slice mark.

The bare mark is white, as the board specifies. On a light panel it is faint.
That is the board's choice, noted here in case a later platform task wants an
ink variant.
