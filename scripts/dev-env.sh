# Development environment for Glass. Source it, do not run it:
#   source scripts/dev-env.sh
# Works in zsh and bash, in the main checkout and in linked worktrees.

export CARGO_BUILD_JOBS=4

if [ -n "${BASH_SOURCE:-}" ]; then
    _glass_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
elif [ -n "${ZSH_VERSION:-}" ]; then
    # shellcheck disable=SC2296
    _glass_root="$(cd "$(dirname "${(%):-%x}")/.." && pwd -P)"
else
    _glass_root="$(pwd -P)"
fi

# The main checkout owns the shared target directory and the FFmpeg cache, so every
# worktree resolves to the same absolute paths and nothing rebuilds when switching.
if _glass_common="$(git -C "$_glass_root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" &&
    _glass_main="$(cd "$_glass_common/.." 2>/dev/null && pwd -P)"; then
    :
else
    _glass_main="$_glass_root"
fi
export GLASS_MAIN_CHECKOUT="$_glass_main"
export CARGO_TARGET_DIR="$_glass_main/target"

# FFMPEG_DIR: an existing value wins, then the pinned build from scripts/ffmpeg/build.sh
# for the host target, then Homebrew's FFmpeg.
if [ -z "${FFMPEG_DIR:-}" ]; then
    _glass_host="$(rustc -vV 2>/dev/null | sed -n "s/^host: //p")"
    _glass_ffmpeg="${GLASS_FFMPEG_CACHE:-$_glass_main/.glass-cache/ffmpeg}/$_glass_host"
    if [ -n "$_glass_host" ] && [ -f "$_glass_ffmpeg/BUILDINFO" ]; then
        export FFMPEG_DIR="$_glass_ffmpeg"
    elif command -v brew >/dev/null 2>&1; then
        if _glass_ffmpeg="$(brew --prefix ffmpeg 2>/dev/null)" && [ -d "$_glass_ffmpeg" ]; then
            export FFMPEG_DIR="$_glass_ffmpeg"
        fi
    fi
fi
unset _glass_root _glass_common _glass_main _glass_host _glass_ffmpeg
