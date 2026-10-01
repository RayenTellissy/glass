#!/usr/bin/env bash
# Runs a task branch's finish check under a single cargo-serial lock hold, so the
# steps don't queue behind other agents one by one. Continues past a failing step
# and exits 1 if any step failed.
#
# Usage (from the worktree root, after `source scripts/dev-env.sh`):
#   GLASS_CARGO_BIN=$PWD/scripts/finish-check.sh \
#     scripts/cargo-serial -p <crate> -p <dependent crate> ...
# The arguments are passed to `cargo test`.

set -uo pipefail

status=0
run() {
    echo "== cargo $*"
    local start=$SECONDS
    if command cargo "$@"; then
        echo "== ok ($((SECONDS - start)) s): cargo $*"
    else
        status=1
        echo "== FAILED ($((SECONDS - start)) s): cargo $*"
    fi
}

# Workspace crates from different worktrees get the same artifact and fingerprint
# names in the shared target directory, so a crate last built by another worktree
# can look fresh. Forget the workspace packages' fingerprints so this worktree's
# own sources are compiled (dependencies are untouched).
python3 - "${CARGO_TARGET_DIR:-target}" <<'PY'
import json, os, re, shutil, subprocess, sys
target = sys.argv[1]
meta = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"]))
names = {p["name"] for p in meta["packages"]}
pattern = re.compile(r"^(" + "|".join(re.escape(n) for n in names) + r")-[0-9a-f]{16}$")
removed = 0
for profile in ("debug", "release"):
    fp = os.path.join(target, profile, ".fingerprint")
    if not os.path.isdir(fp):
        continue
    for entry in os.listdir(fp):
        if pattern.match(entry):
            shutil.rmtree(os.path.join(fp, entry), ignore_errors=True)
            removed += 1
print(f"== forgot {removed} workspace fingerprints for {len(names)} packages")
PY

run build --workspace
run test "$@"
run clippy --workspace --all-targets -- -D warnings
run run --bin smoke
exit $status
