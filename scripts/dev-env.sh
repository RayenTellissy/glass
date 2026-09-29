# Development environment for Glass. Source it, do not run it:
#   source scripts/dev-env.sh
# Works in zsh and bash.

export CARGO_BUILD_JOBS=4

if [ -z "${FFMPEG_DIR:-}" ] && command -v brew >/dev/null 2>&1; then
    if _glass_ffmpeg_prefix="$(brew --prefix ffmpeg 2>/dev/null)" && [ -d "$_glass_ffmpeg_prefix" ]; then
        export FFMPEG_DIR="$_glass_ffmpeg_prefix"
    fi
    unset _glass_ffmpeg_prefix
fi
