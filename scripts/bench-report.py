#!/usr/bin/env python3
"""Formats criterion results as a Markdown table, comparing each benchmark's latest run
with a saved baseline.

Usage: scripts/bench-report.py [--criterion DIR] [--baseline NAME] [--threshold PCT]

Reads <DIR>/**/new/estimates.json (the latest run) and <DIR>/**/<NAME>/estimates.json
(the baseline saved with `--save-baseline NAME`). Prints Markdown on stdout. Changes
beyond the threshold (default 10%) are flagged; shared CI runners are noisy, so smaller
changes are not called out.
"""

import argparse
import json
import pathlib
import sys


def mean_ns(path):
    with open(path) as f:
        return json.load(f)["mean"]["point_estimate"]


def human(ns):
    for unit, scale in (("s", 1e9), ("ms", 1e6), ("µs", 1e3), ("ns", 1.0)):
        if ns >= scale:
            return f"{ns / scale:.3g} {unit}"
    return f"{ns * 1e3:.3g} ps"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--criterion", default="target/criterion")
    parser.add_argument("--baseline", default="main")
    parser.add_argument("--threshold", type=float, default=10.0)
    args = parser.parse_args()

    root = pathlib.Path(args.criterion)
    rows = []
    for bench in sorted(root.glob("**/new/benchmark.json")):
        with open(bench) as f:
            name = json.load(f)["full_id"]
        new = mean_ns(bench.parent / "estimates.json")
        base_file = bench.parent.parent / args.baseline / "estimates.json"
        if base_file.exists():
            base = mean_ns(base_file)
            change = (new - base) / base * 100.0
            if change > args.threshold:
                verdict = "slower"
            elif change < -args.threshold:
                verdict = "faster"
            else:
                verdict = "same"
            rows.append((name, human(base), human(new), f"{change:+.1f}%", verdict))
        else:
            rows.append((name, "none", human(new), "", "new"))

    print("### Benchmarks")
    print()
    if not rows:
        print("No criterion results found.")
        return 0
    print(f"Mean time per iteration against the `{args.baseline}` baseline. Changes within "
          f"±{args.threshold:g}% are treated as noise.")
    print()
    print("| Benchmark | Baseline | This run | Change | Verdict |")
    print("| --- | ---: | ---: | ---: | --- |")
    for row in rows:
        print("| " + " | ".join(row) + " |")
    slower = sum(1 for row in rows if row[4] == "slower")
    if slower:
        print()
        print(f"**{slower} benchmark(s) slower than the baseline by more than {args.threshold:g}%.**")
    return 0


if __name__ == "__main__":
    sys.exit(main())
