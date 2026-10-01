#!/usr/bin/env python3
# Lists PLAN.md task lines kept twice by a union merge (e.g. one "[ ]" and one "[x]" copy).
# Run after every merge into a task or integration branch; exits 1 if any pair is found.
import re, collections, sys
lines = open(sys.argv[1] if len(sys.argv) > 1 else "PLAN.md").read().splitlines()
seen = collections.defaultdict(list)
for i, l in enumerate(lines, 1):
    m = re.match(r"^(\s*)- \[(.)\] (.*)$", l)
    if m:
        key = (m.group(1), re.sub(r"\s*\(needs-human[^)]*\)\s*$", "", m.group(3)))
        seen[key].append((i, m.group(2)))
bad = [(v, k[1][:90]) for k, v in seen.items() if len(v) > 1]
for b in bad:
    print(*b)
sys.exit(1 if bad else 0)
