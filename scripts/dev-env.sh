# Development environment for Glass. Source it, do not run it:
#   source scripts/dev-env.sh
# Works in zsh and bash.

export CARGO_BUILD_JOBS=4

# FFMPEG_DIR: an existing value wins, then the pinned build from scripts/ffmpeg/build.sh
# for the host target, then Homebrew's FFmpeg.
if [ -z "${FFMPEG_DIR:-}" ]; then
    if [ -n "${BASH_SOURCE:-}" ]; then
        _glass_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    elif [ -n "${ZSH_VERSION:-}" ]; then
        # shellcheck disable=SC2296
        _glass_root="$(cd "$(dirname "${(%):-%x}")/.." && pwd)"
    else
        _glass_root="$(pwd)"
    fi
    _glass_host="$(rustc -vV 2>/dev/null | sed -n "s/^host: //p")"
    _glass_ffmpeg="${GLASS_FFMPEG_CACHE:-$_glass_root/.glass-cache/ffmpeg}/$_glass_host"
    if [ -n "$_glass_host" ] && [ -f "$_glass_ffmpeg/BUILDINFO" ]; then
        export FFMPEG_DIR="$_glass_ffmpeg"
    elif command -v brew >/dev/null 2>&1; then
        if _glass_ffmpeg="$(brew --prefix ffmpeg 2>/dev/null)" && [ -d "$_glass_ffmpeg" ]; then
            export FFMPEG_DIR="$_glass_ffmpeg"
        fi
    fi
    unset _glass_root _glass_host _glass_ffmpeg
fi
