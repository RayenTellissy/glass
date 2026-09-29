"""Rewrite a mark SVG exported from Paper into a portable form.

Usage:
    svg.py <in.svg> <out.svg> [--size N] [--flatten GREY]

- Drops the inline style Paper adds and sets width/height to N (default 200).
- Rewrites #RRGGBBAA fills as fill + fill-opacity, which every renderer
  understands (CoreSVG and older librsvg ignore the 8-digit form).
- --flatten GREY composites each translucent fill over an opaque grey
  (0..1, sRGB) and writes opaque colours. The macOS dark icon rendition
  brightens translucent layers to full white, so the dark mark needs its
  slice tiers baked in.
"""

import re
import sys


def main() -> None:
    args = sys.argv[1:]
    src, dst = args[0], args[1]
    size = 200
    flatten = None
    if "--size" in args:
        size = int(args[args.index("--size") + 1])
    if "--flatten" in args:
        flatten = float(args[args.index("--flatten") + 1])

    text = open(src, encoding="utf-8").read()
    text = re.sub(r"<svg([^>]*?) style=\"[^\"]*\"", r"<svg\1", text, count=1)
    text = re.sub(
        r"<svg([^>]*?)width=\"[^\"]*\" height=\"[^\"]*\"",
        lambda m: f"<svg{m.group(1)}width=\"{size}\" height=\"{size}\"",
        text,
        count=1,
    )

    def fill(match: re.Match) -> str:
        rgb = match.group(1)
        alpha = int(match.group(2), 16) / 255 if match.group(2) else 1.0
        if flatten is None:
            if match.group(2) is None:
                return f"fill=\"#{rgb}\""
            return f"fill=\"#{rgb}\" fill-opacity=\"{alpha:.3f}\""
        out = []
        for i in range(0, 6, 2):
            c = int(rgb[i : i + 2], 16) / 255
            out.append(round(255 * (alpha * c + (1 - alpha) * flatten)))
        return "fill=\"#{:02X}{:02X}{:02X}\"".format(*out)

    text = re.sub(r"fill=\"#([0-9A-Fa-f]{6})([0-9A-Fa-f]{2})?\"", fill, text)
    with open(dst, "w", encoding="utf-8") as f:
        f.write(text)


if __name__ == "__main__":
    main()
