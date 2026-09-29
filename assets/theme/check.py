#!/usr/bin/env python3
"""Validate the theme files in assets/theme and report WCAG contrast.

Exits non-zero on a structural error or on a text/control pair under its
threshold. text/muted pairs under 4.5:1 are reported but do not fail, because
muted text is allowed to trade contrast for hierarchy (see README.md).
"""

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
THEMES = ["dark.json", "light.json", "high-contrast.json"]

REQUIRED_ROLES = [
    "bg/0", "bg/1", "bg/2", "bg/3",
    "border/subtle", "border/strong",
    "text/primary", "text/secondary", "text/muted", "text/inverse",
    "accent", "accent/soft",
    "state/success", "state/warning", "state/error", "state/info",
    "clip/video", "clip/audio", "clip/title", "clip/adjustment", "clip/subtitle", "clip/offline",
    "scope/red", "scope/green", "scope/blue", "scope/luma",
]
GLASS_ROLES = ["glass/top", "glass/bottom", "glass/edge", "glass/highlight", "glass/shadow"]

COLOR_RE = re.compile(r"^#([0-9A-F]{6}|[0-9A-F]{8})$")
PAPER_RE = re.compile(r"^--color-[a-z0-9-]+$")
ENTRY_KEYS = {"value", "paper", "alias", "source", "derived_from", "note"}
THEME_KEYS = {"$schema", "name", "mode", "description", "colors"}
TOKEN_SECTIONS = [
    "fonts", "font_weights", "type", "spacing_px", "sizes_px", "radius_px",
    "icon_px", "stroke_px", "opacity", "breakpoints_px", "containers_px",
]
TYPE_STYLES = ["display", "title", "body", "label", "caption", "mono"]

TEXT_MIN = 4.5
CONTROL_MIN = 3.0


def parse_color(text):
    if not isinstance(text, str) or not COLOR_RE.match(text):
        raise ValueError(f"not #RRGGBB or #RRGGBBAA: {text!r}")
    rgb = tuple(int(text[i:i + 2], 16) for i in (1, 3, 5))
    alpha = int(text[7:9], 16) if len(text) == 9 else 255
    return rgb, alpha


def luminance(rgb):
    def channel(c):
        c = c / 255
        return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4

    r, g, b = (channel(c) for c in rgb)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(fg, bg):
    a, b = luminance(fg), luminance(bg)
    hi, lo = max(a, b), min(a, b)
    return (hi + 0.05) / (lo + 0.05)


def check_theme(name, data, errors):
    for key in data:
        if key not in THEME_KEYS:
            errors.append(f"{name}: unknown top-level key {key!r}")
    for key in ("name", "mode", "colors"):
        if key not in data:
            errors.append(f"{name}: missing {key!r}")
    if data.get("mode") not in ("dark", "light"):
        errors.append(f"{name}: mode must be dark or light")
    colors = data.get("colors", {})
    for role in REQUIRED_ROLES + GLASS_ROLES:
        if role not in colors:
            errors.append(f"{name}: missing role {role}")
    parsed = {}
    for role, entry in colors.items():
        if not isinstance(entry, dict):
            errors.append(f"{name}: {role} is not an object")
            continue
        extra = set(entry) - ENTRY_KEYS
        if extra:
            errors.append(f"{name}: {role} has unknown keys {sorted(extra)}")
        paper = entry.get("paper", "missing")
        if paper == "missing":
            errors.append(f"{name}: {role} has no paper field")
        elif paper is not None and not PAPER_RE.match(paper):
            errors.append(f"{name}: {role} paper name {paper!r} is malformed")
        try:
            parsed[role] = parse_color(entry.get("value"))
        except ValueError as err:
            errors.append(f"{name}: {role}: {err}")
    return parsed


def check_tokens(errors):
    path = HERE / "tokens.json"
    try:
        tokens = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as err:
        errors.append(f"tokens.json: {err}")
        return
    for section in TOKEN_SECTIONS:
        if section not in tokens:
            errors.append(f"tokens.json: missing section {section!r}")
    for style in TYPE_STYLES:
        spec = tokens.get("type", {}).get(style)
        if spec is None:
            errors.append(f"tokens.json: missing type style {style!r}")
            continue
        for field in ("font", "weight", "size_px", "line_height_px", "letter_spacing_em"):
            if field not in spec:
                errors.append(f"tokens.json: type.{style} missing {field!r}")
        if spec.get("font") not in tokens.get("fonts", {}):
            errors.append(f"tokens.json: type.{style} font {spec.get('font')!r} is not in fonts")


def pairs():
    for fg in ("text/primary", "text/secondary", "text/muted"):
        for bg in ("bg/0", "bg/1", "bg/2", "bg/3"):
            yield fg, bg, "text", TEXT_MIN
    yield "text/inverse", "accent", "text", TEXT_MIN
    yield "accent", "bg/1", "control", CONTROL_MIN
    for bg in ("bg/1", "bg/2", "bg/3"):
        yield "border/strong", bg, "control", CONTROL_MIN


def main():
    errors = []
    themes = {}
    for file in THEMES:
        try:
            data = json.loads((HERE / file).read_text())
        except (OSError, json.JSONDecodeError) as err:
            errors.append(f"{file}: {err}")
            continue
        themes[file] = (data, check_theme(file, data, errors))

    key_sets = {file: set(data.get("colors", {})) for file, (data, _) in themes.items()}
    if key_sets:
        union = set().union(*key_sets.values())
        for file, keys in key_sets.items():
            missing = union - keys
            if missing:
                errors.append(f"{file}: roles present elsewhere but missing here: {sorted(missing)}")

    check_tokens(errors)

    failures = []
    notes = []
    print(f"{'theme':<20} {'foreground':<15} {'background':<10} {'kind':<8} {'ratio':>7}  min   result")
    for file, (data, parsed) in themes.items():
        for fg, bg, kind, minimum in pairs():
            if fg not in parsed or bg not in parsed:
                continue
            (fg_rgb, fg_a), (bg_rgb, bg_a) = parsed[fg], parsed[bg]
            if fg_a != 255 or bg_a != 255:
                continue
            ratio = contrast(fg_rgb, bg_rgb)
            ok = ratio >= minimum
            result = "ok"
            if not ok:
                if fg == "text/muted":
                    result = "LOW (muted, reported only)"
                    notes.append(f"{file}: {fg} on {bg} is {ratio:.2f}:1")
                else:
                    result = "FAIL"
                    failures.append(f"{file}: {fg} on {bg} is {ratio:.2f}:1, needs {minimum}:1")
            print(f"{data.get('name', file):<20} {fg:<15} {bg:<10} {kind:<8} {ratio:>6.2f}:1 {minimum:>4}  {result}")

    print()
    for file, keys in key_sets.items():
        print(f"{file}: {len(keys)} colour roles")
    for note in notes:
        print(f"note: {note}")
    for err in errors:
        print(f"error: {err}")
    for failure in failures:
        print(f"contrast: {failure}")
    if errors or failures:
        print(f"\n{len(errors)} error(s), {len(failures)} contrast failure(s)")
        return 1
    print("\nall theme checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
