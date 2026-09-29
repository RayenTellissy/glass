#!/usr/bin/env bash
# Prepares a linked worktree for building and testing. Run it from inside the new
# worktree; running it again is harmless.
#
#   git worktree add ../glass-<name> -b task/<name> goal/phase-<N>
#   cd ../glass-<name> && scripts/worktree-setup.sh
#
# Test media is git-ignored, so every worktree generates its own (about 25 MB).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
# shellcheck source=dev-env.sh
source scripts/dev-env.sh

tests/fixtures/generate.sh

echo "worktree:         $(pwd -P)"
echo "main checkout:    $GLASS_MAIN_CHECKOUT"
echo "CARGO_TARGET_DIR: $CARGO_TARGET_DIR"
echo "FFMPEG_DIR:       ${FFMPEG_DIR:-<not found>}"
echo "In each shell:    source scripts/dev-env.sh, then run cargo through scripts/cargo-serial"
