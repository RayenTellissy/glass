# Theme tokens

The design tokens for the Glass UI, taken from the Paper file "Inspiration 2", page `00 Foundations` (`Sheet / Tokens`). The values were read with Paper's token export (token content hash `61804daf`) and checked against the swatch labels, type specimens and contrast figures on `Sheet / Tokens`. None were read from screenshots.

| File | Contents |
| --- | --- |
| `dark.json` | Default theme: every `--color-*` token |
| `light.json` | Opposite theme: every `--color-light-*` token |
| `high-contrast.json` | High contrast theme, derived from dark (see below) |
| `tokens.json` | Tokens shared by all themes: fonts, type scale, spacing, sizes, radius, icon sizes, stroke, opacity, breakpoints, containers, and a few rules written on the boards |
| `schema.json` | JSON Schema (2020-12) for the three theme files |
| `check.py` | Validator and contrast report, Python stdlib only: `python3 assets/theme/check.py` |

## Dark is the default

The design brief says "Dark by default". `dark.json` is the default, and `light.json` is the opposite theme drawn on page `11 Platform, theme, RTL`. Each theme sets `mode` (`dark` or `light`) so the loader knows which base egui visuals to start from.

## Colour roles

Each theme has the same 31 roles. They are named exactly as in DESIGN_HANDOFF.md section 5, plus the five `glass/*` roles that Paper adds:

- `bg/0`–`bg/3`
- `border/subtle`, `border/strong`
- `text/primary`, `text/secondary`, `text/muted`, `text/inverse`
- `accent`, `accent/soft`
- `state/success|warning|error|info`
- `clip/video|audio|title|adjustment|subtitle|offline`
- `scope/red|green|blue|luma`
- `glass/top|bottom|edge|highlight|shadow`

Each entry has these fields:

- `value`: `#RRGGBB`, or `#RRGGBBAA` with straight alpha, exactly as Paper stores it.
- `paper`: the Paper token name. Paper names are `--color-<role>` for dark and `--color-light-<role>` for light, with `/` written as `-`.
- `alias` and `source` (only when Paper stores a reference instead of a hex). The four light `scope/*` tokens are stored as `var(--color-scope-*)`, because scopes keep their dark plate in the light theme. `value` holds the resolved dark value, `alias` the token it points to, and `source` the original string.

Every Paper colour is stored as hex, so no rgb(), hsl() or oklch() conversion was needed.

## High contrast derivation

Paper has no high-contrast tokens. The board `Screen / Edit workspace — high contrast` on page `11 Platform, theme, RTL` uses the dark tokens. Querying which tokens each node on that board uses, and comparing its computed styles with the same controls on `Screen / Edit workspace — macOS chrome`, gives these changes:

| Role | HC value | Evidence on the HC board |
| --- | --- | --- |
| `text/secondary` | `text/primary` (#EDEDED) | Every text node is `text/primary`. The only exceptions are `text/inverse` on the accent button and `state/error` on the offline count and the Relink action. No node uses `text/secondary` or `text/muted`. |
| `text/muted` | `text/primary` (#EDEDED) | Same as above. |
| `border/subtle` | `border/strong` (#707070) | All 61 dividers, panel edges, lane lines and control borders use `border/strong`. No node uses `border/subtle`. |
| `glass/edge` | `border/strong` (#707070) | Pills, buttons and tabs that have a `glass/edge` border on the default board have a `border/strong` border here. |
| `glass/top`, `glass/bottom` | `#00000000` | No glass gradient anywhere. Controls are flat `bg/2` or `bg/3`. |
| `glass/highlight` | `#00000000` | No 1 px inner top highlight. |

Every other role, including `bg/0`–`bg/3`, keeps its dark value. The board still uses all four surface steps, `accent`, `accent/soft` and the clip, state and scope colours. `glass/shadow` keeps its dark value because the board has no floating layer to show a change.

Derived entries carry `derived_from` and a `note`. Cleared entries have `paper: null`. The loader can treat `high-contrast.json` like any other theme.

Two differences on the HC board cannot be expressed as tokens and belong in component code:

- Status pills get a `bg/2` fill. On the default board they are glass only.
- Selected tabs get a 1 px `accent` inset ring in place of the `glass/edge` ring.

## Contrast

`check.py` computes WCAG 2 contrast for these pairs:

- `text/primary`, `text/secondary` and `text/muted` on `bg/0`–`bg/3`, with a 4.5:1 minimum.
- `text/inverse` on `accent`, with a 4.5:1 minimum.
- `accent` on `bg/1`, with a 3:1 minimum.
- `border/strong` on `bg/1`–`bg/3`, with a 3:1 minimum.

Alpha colours are skipped.

The script exits non-zero on any structural error or failing pair. The one exception is `text/muted`: a muted pair under 4.5:1 is printed as a note but does not fail the check, because muted text may trade contrast for hierarchy. As drawn, no pair fails in any theme, muted included.

- The design brief promises 4.5:1 for text up to `bg/3` and 3:1 for controls with `border/strong`, and the values meet both.
- The lowest pairs are:
  - light `text/muted` on `bg/0`: 4.60:1
  - dark `border/strong` on `bg/3`: 3.13:1
- The ratios match the figures printed on `Sheet / Tokens`. For example, text/primary is 15.7 and 16.7, border/strong is 3.7 and 3.6, and text/inverse on accent is 17.5.

## Shared tokens (`tokens.json`)

| Group | Values | Paper tokens |
| --- | --- | --- |
| Fonts | Geist (UI, 400 and 500), Geist Mono (400) | `--font-ui`, `--font-mono` |
| Weights | regular 400, medium 500 (the heaviest in the app) | `--font-weight-*` |
| Type | display 24/32 500 −0.02em · title 18/24 500 −0.01em · body 14/20 400 · label 12/16 400 · caption 11/14 400 (caps +0.06em) · mono 12/16 Geist Mono 400 | `--text-*`, `--leading-*`, `--tracking-*` |
| Spacing | 2, 4, 8, 12, 16, 24, 32, and 48, 64 (`board_only`: sheet margins, never in app UI) | `--spacing-<n>` |
| Sizes | control 24 (Pro), 32 (Beginner) · row 28 (Pro), 40 (Beginner) | `--spacing-control-*`, `--spacing-row-*` |
| Radius | panel 0, control 4, floating 8, pill 999 | `--radius-*` |
| Icons | 16, 20 | `--spacing-icon-sm`, `--spacing-icon-md` |
| Stroke | 1.5 px icon stroke, 1.5 px selected-clip outline | none: `source: "board text"` |
| Opacity | disabled 40%, glass 72% | `--opacity-*` |
| Breakpoints | min 1280, default 1920 | `--breakpoint-*` |
| Containers | inspector 320, sheet 1440, screen 1920 | `--container-*` |

Type weights come from the `Sheet / Tokens` type specimens. Body and label are listed there as "400 and 500": 400 is the default, and 500 is used for emphasis.

`rules` records values that are written on the boards but are not tokens:

- floating shadow `0 16 48`
- focus ring 2 px with a 2 px gap, in `accent`
- 24 px backdrop blur
- danger button fill: `state/error` at 14%

Every entry in `rules` has `source: "board text"`.

## Differences from DESIGN_HANDOFF.md section 5

- **Font:** Geist and Geist Mono, not Inter. The Feel extraction chose Geist because it is rounder.
- **Radius:** floating is 8, not 6.
- **Glass:** Paper adds five `glass/*` colour roles (the sheen on controls and floating layers), plus `--opacity-glass` 72%.
- **Spacing:** Paper adds 48 and 64, for board margins only.
- **Sizes:** Paper adds control and row heights, opacity, breakpoints and containers.
- **Stroke:** 1.5 px is written on the boards, not stored as a token.
- **Floating shadow:** the translation table writes it as 50% black, but the stored `glass/shadow` token is 60% black in dark and 15% in light. Use the token.
