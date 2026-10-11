#!/usr/bin/env bash
# Builds the pinned FFmpeg (scripts/ffmpeg/versions.env) with x264, x265 and dav1d as
# shared libraries for the host target, into .glass-cache/ffmpeg/<target-triple>/:
#   lib/ include/ bin/   the install prefix (point FFMPEG_DIR here)
#   LICENSES/            licence texts of everything linked into the libraries
#   SOURCES.txt          archive names, URLs and SHA-256 of the sources used
#   BUILDINFO            versions, configure line and the stamp used to skip rebuilds
#
# Usage: scripts/ffmpeg/build.sh [--jobs N] [--force] [--keep-build] [--fetch-only]
#   --jobs N       parallel make jobs (default 4, or CARGO_BUILD_JOBS if set)
#   --force        rebuild even when BUILDINFO matches versions.env and this script
#   --keep-build   keep the intermediate build tree under .glass-cache/ffmpeg/build/
#   --fetch-only   download and verify the source archives, then stop
#
# Targets: aarch64/x86_64-apple-darwin and x86_64-unknown-linux-gnu natively; on
# x86_64-pc-windows-msvc from an MSYS2 shell with the MSVC environment loaded (cl,
# link and cmake on PATH, nasm and pkgconf from MSYS2). Cross builds are not
# supported: CI builds each target on a native runner. python3 (or python) must be on
# PATH: meson, which builds dav1d, runs from its pinned source like the other host tools.
#
# GLASS_FFMPEG_CACHE overrides the cache root (default <repo>/.glass-cache/ffmpeg).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=versions.env
source "$SCRIPT_DIR/versions.env"

JOBS="${CARGO_BUILD_JOBS:-4}"
FORCE=0
KEEP_BUILD=0
FETCH_ONLY=0
usage() {
    sed -n "2,22p" "$0" | sed "s/^# \{0,1\}//"
}

usage_error() {
    echo "build.sh: $*" >&2
    usage >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --jobs)
            [ $# -ge 2 ] && [[ "$2" =~ ^[1-9][0-9]*$ ]] || usage_error "--jobs needs a positive number"
            JOBS="$2"
            shift
            ;;
        --force) FORCE=1 ;;
        --keep-build) KEEP_BUILD=1 ;;
        --fetch-only) FETCH_ONLY=1 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) usage_error "unknown argument: $1" ;;
    esac
    shift
done

die() {
    echo "build.sh: $*" >&2
    exit 1
}

host_triple() {
    if command -v rustc >/dev/null 2>&1; then
        rustc -vV | sed -n "s/^host: //p"
        return
    fi
    case "$(uname -s)-$(uname -m)" in
        Darwin-arm64) echo "aarch64-apple-darwin" ;;
        Darwin-x86_64) echo "x86_64-apple-darwin" ;;
        Linux-x86_64) echo "x86_64-unknown-linux-gnu" ;;
        MINGW64*-x86_64 | MSYS*-x86_64) echo "x86_64-pc-windows-msvc" ;;
        *) die "cannot tell the host target from $(uname -s) $(uname -m)" ;;
    esac
}

TARGET="$(host_triple)"
case "$TARGET" in
    *-apple-darwin) OS=macos ;;
    *-linux-gnu) OS=linux ;;
    *-windows-msvc) OS=windows ;;
    *) die "unsupported target $TARGET" ;;
esac
ARCH="${TARGET%%-*}"

CACHE="${GLASS_FFMPEG_CACHE:-$REPO_ROOT/.glass-cache/ffmpeg}"
SRC_DIR="$CACHE/src"
BUILD_DIR="$CACHE/build/$TARGET"
TOOLS_DIR="$CACHE/tools/$TARGET"
LOG_DIR="$CACHE/logs/$TARGET"
PREFIX="$CACHE/$TARGET"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d " " -f 1
    else
        shasum -a 256 "$1" | cut -d " " -f 1
    fi
}

STAMP="$(cat "$SCRIPT_DIR/versions.env" "$SCRIPT_DIR/build.sh" | {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi
} | cut -d " " -f 1)"

# Sources each target needs, by their prefix in versions.env.
SHIPPED=(FFMPEG X264 X265 DAV1D)
case "$OS" in
    linux) SHIPPED+=(NVCODEC) ;;
    windows) SHIPPED+=(NVCODEC ZLIB) ;;
esac
TOOLS=(MESON NINJA)
if [ "$OS" != windows ]; then
    TOOLS+=(PKGCONF)
    if [ "$ARCH" = x86_64 ] && ! command -v nasm >/dev/null 2>&1; then
        TOOLS+=(NASM)
    fi
fi

field() {
    local name="$1_$2"
    printf "%s" "${!name}"
}

fetch() {
    local archive url sum path
    archive="$(field "$1" ARCHIVE)"
    url="$(field "$1" URL)"
    sum="$(field "$1" SHA256)"
    path="$SRC_DIR/$archive"
    if [ -f "$path" ] && [ "$(sha256_of "$path")" = "$sum" ]; then
        return
    fi
    echo "fetch  $archive"
    curl -fL --retry 3 --silent --show-error -o "$path.part" "$url"
    local got
    got="$(sha256_of "$path.part")"
    if [ "$got" != "$sum" ]; then
        rm -f "$path.part"
        die "checksum mismatch for $archive: expected $sum, got $got"
    fi
    mv -f "$path.part" "$path"
}

# Unpacks a source archive into the build tree and prints the directory it made.
unpack() {
    local archive dir
    archive="$(field "$1" ARCHIVE)"
    case "$1" in
        FFMPEG) dir="ffmpeg-$FFMPEG_VERSION" ;;
        X264) dir="x264-$X264_COMMIT" ;;
        X265) dir="x265_$X265_VERSION" ;;
        DAV1D) dir="dav1d-$DAV1D_VERSION" ;;
        MESON) dir="meson-$MESON_VERSION" ;;
        NINJA) dir="ninja-$NINJA_VERSION" ;;
        NVCODEC) dir="nv-codec-headers-n$NVCODEC_VERSION" ;;
        ZLIB) dir="zlib-$ZLIB_VERSION" ;;
        PKGCONF) dir="pkgconf-$PKGCONF_VERSION" ;;
        NASM) dir="nasm-$NASM_VERSION" ;;
        *) die "no directory known for $1" ;;
    esac
    rm -rf "${BUILD_DIR:?}/$dir"
    tar -xf "$SRC_DIR/$archive" -C "$BUILD_DIR"
    [ -d "$BUILD_DIR/$dir" ] || die "$archive did not unpack to $dir"
    printf "%s" "$BUILD_DIR/$dir"
}

# run <step> <command...>: logs to LOG_DIR/<step>.log and shows the tail on failure.
run() {
    local step="$1"
    shift
    local log="$LOG_DIR/$step.log"
    if ! "$@" >>"$log" 2>&1; then
        echo "build.sh: step $step failed; last lines of $log:" >&2
        tail -n 40 "$log" >&2
        exit 1
    fi
}

# Native Windows tools (cl, cmake) want Windows paths.
native_path() {
    if [ "$OS" = windows ]; then cygpath -m "$1"; else printf "%s" "$1"; fi
}

mkdir -p "$SRC_DIR"
for name in "${SHIPPED[@]}" "${TOOLS[@]}"; do
    fetch "$name"
done
if [ "$FETCH_ONLY" = 1 ]; then
    echo "sources verified in $SRC_DIR"
    exit 0
fi

if [ "$FORCE" = 0 ] && [ -f "$PREFIX/BUILDINFO" ] && grep -qx "stamp: $STAMP" "$PREFIX/BUILDINFO"; then
    echo "FFmpeg $FFMPEG_VERSION for $TARGET is up to date in $PREFIX"
    exit 0
fi

START=$SECONDS
rm -rf "${BUILD_DIR:?}" "${LOG_DIR:?}" "${PREFIX:?}"
mkdir -p "$BUILD_DIR" "$LOG_DIR" "$PREFIX/lib" "$PREFIX/include" "$TOOLS_DIR"
echo "build  FFmpeg $FFMPEG_VERSION for $TARGET into $PREFIX (logs in $LOG_DIR)"

if [ "$OS" = macos ]; then
    # Matches the macOS 12 minimum target in docs/ARCHITECTURE.md.
    export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
fi

# Host tools.
export PATH="$TOOLS_DIR/bin:$PATH"
PYTHON="$(command -v python3 || command -v python || true)"
[ -n "$PYTHON" ] || die "python3 not found (meson needs it)"
EXE=""
[ "$OS" = windows ] && EXE=".exe"
if [ "$("$TOOLS_DIR/bin/ninja$EXE" --version 2>/dev/null)" != "$NINJA_VERSION" ]; then
    echo "tool   ninja $NINJA_VERSION"
    dir="$(unpack NINJA)"
    (cd "$dir" && run ninja "$PYTHON" configure.py --bootstrap)
    mkdir -p "$TOOLS_DIR/bin"
    cp "$dir/ninja$EXE" "$TOOLS_DIR/bin/ninja$EXE"
fi
MESON_DIR="$TOOLS_DIR/meson-$MESON_VERSION"
if [ ! -f "$MESON_DIR/meson.py" ]; then
    echo "tool   meson $MESON_VERSION"
    tar -xf "$SRC_DIR/$MESON_ARCHIVE" -C "$TOOLS_DIR"
    [ -f "$MESON_DIR/meson.py" ] || die "$MESON_ARCHIVE did not unpack to $MESON_DIR"
fi
MESON=("$PYTHON" "$(native_path "$MESON_DIR/meson.py")")
for name in "${TOOLS[@]}"; do
    case "$name" in MESON | NINJA) continue ;; esac
    tool="$(printf "%s" "$name" | tr "[:upper:]" "[:lower:]")"
    if [ -x "$TOOLS_DIR/bin/$tool" ]; then
        continue
    fi
    echo "tool   $tool"
    dir="$(unpack "$name")"
    extra=()
    [ "$name" = PKGCONF ] && extra=(--disable-shared)
    (cd "$dir" && run "$tool" ./configure --prefix="$TOOLS_DIR" ${extra[@]+"${extra[@]}"} &&
        run "$tool" make -j"$JOBS" && run "$tool" make install)
done
if [ "$OS" = windows ]; then
    PKG_CONFIG="$(command -v pkgconf || command -v pkg-config || true)"
else
    PKG_CONFIG="$TOOLS_DIR/bin/pkgconf"
fi
[ -n "$PKG_CONFIG" ] || die "pkgconf not found"
export PKG_CONFIG
PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
if [ "$OS" = linux ]; then
    # System libraries FFmpeg links on Linux (libva, zlib).
    multiarch="$(gcc -dumpmachine 2>/dev/null || echo "$ARCH-linux-gnu")"
    PKG_CONFIG_PATH="$PKG_CONFIG_PATH:/usr/lib/$multiarch/pkgconfig:/usr/lib/pkgconfig:/usr/share/pkgconfig"
fi
export PKG_CONFIG_PATH

if [ "$OS" = windows ]; then
    echo "zlib   $ZLIB_VERSION (static)"
    dir="$(unpack ZLIB)"
    run zlib cmake -S "$(native_path "$dir")" -B "$(native_path "$dir/build")" \
        -DCMAKE_INSTALL_PREFIX="$(native_path "$PREFIX")" -DZLIB_BUILD_SHARED=OFF -DZLIB_BUILD_TESTING=OFF
    run zlib cmake --build "$(native_path "$dir/build")" --config Release --parallel "$JOBS"
    run zlib cmake --install "$(native_path "$dir/build")" --config Release
    # FFmpeg's MSVC toolchain turns -lz into z.lib.
    for lib in zs.lib zlibstatic.lib zlib.lib; do
        if [ -f "$PREFIX/lib/$lib" ]; then
            cp "$PREFIX/lib/$lib" "$PREFIX/lib/z.lib"
            break
        fi
    done
    [ -f "$PREFIX/lib/z.lib" ] || die "static zlib library not found after install"
fi

if [ "$OS" != macos ]; then
    echo "nvcodec headers $NVCODEC_VERSION"
    dir="$(unpack NVCODEC)"
    run nvcodec make -C "$dir" PREFIX="$PREFIX" install
fi

echo "x264   $X264_COMMIT"
dir="$(unpack X264)"
x264_env=()
[ "$OS" = windows ] && x264_env=(CC=cl)
(cd "$dir" && run x264 env ${x264_env[@]+"${x264_env[@]}"} ./configure --prefix="$PREFIX" \
    --enable-shared --enable-pic --disable-cli &&
    run x264 make -j"$JOBS" && run x264 make install)

# x265: one shared library holding the 8-bit encoder plus a linked-in 10-bit encoder
# (Main10 for HDR and 10-bit export); 12-bit is left out to halve build time.
echo "x265   $X265_VERSION (8-bit + 10-bit)"
dir="$(unpack X265)"
x265_common=(-DCMAKE_BUILD_TYPE=Release -DENABLE_CLI=OFF -DCMAKE_POLICY_VERSION_MINIMUM=3.5)
run x265 cmake -S "$(native_path "$dir/source")" -B "$(native_path "$dir/build-10")" "${x265_common[@]}" \
    -DHIGH_BIT_DEPTH=ON -DEXPORT_C_API=OFF -DENABLE_SHARED=OFF
run x265 cmake --build "$(native_path "$dir/build-10")" --config Release --parallel "$JOBS"
mkdir -p "$dir/build-8"
if [ "$OS" = windows ]; then
    cp "$dir/build-10/Release/x265-static.lib" "$dir/build-8/x265_main10.lib"
    main10="x265_main10.lib"
    main10_flags="/LIBPATH:$(native_path "$dir/build-8")"
else
    cp "$dir/build-10/libx265.a" "$dir/build-8/libx265_main10.a"
    main10="x265_main10"
    main10_flags="-L$dir/build-8"
fi
x265_main=(-DCMAKE_INSTALL_PREFIX="$(native_path "$PREFIX")" -DENABLE_SHARED=ON -DLINKED_10BIT=ON
    -DEXTRA_LIB="$main10" -DEXTRA_LINK_FLAGS="$main10_flags")
if [ "$OS" = macos ]; then
    # Absolute install names so dev builds and tests load it without DYLD paths;
    # packaging rewrites them to @rpath when bundling.
    x265_main+=(-DCMAKE_INSTALL_NAME_DIR="$PREFIX/lib")
fi
run x265 cmake -S "$(native_path "$dir/source")" -B "$(native_path "$dir/build-8")" "${x265_common[@]}" "${x265_main[@]}"
run x265 cmake --build "$(native_path "$dir/build-8")" --config Release --parallel "$JOBS"
run x265 cmake --install "$(native_path "$dir/build-8")" --config Release

echo "dav1d  $DAV1D_VERSION"
dir="$(unpack DAV1D)"
run dav1d "${MESON[@]}" setup "$(native_path "$dir/build")" "$(native_path "$dir")" \
    --prefix="$(native_path "$PREFIX")" --libdir=lib --buildtype=release --default-library=shared \
    -Denable_tools=false -Denable_tests=false -Denable_examples=false
run dav1d ninja -C "$dir/build" -j "$JOBS"
run dav1d ninja -C "$dir/build" install
if [ "$OS" = macos ]; then
    # meson installs with an @rpath id; use an absolute one like the other libraries.
    for lib in "$PREFIX"/lib/libdav1d.*.dylib; do
        [ -L "$lib" ] || install_name_tool -id "$lib" "$lib"
    done
fi

echo "ffmpeg $FFMPEG_VERSION"
dir="$(unpack FFMPEG)"
ffmpeg_args=(
    --prefix="$(native_path "$PREFIX")"
    --enable-shared --disable-static
    --enable-gpl --enable-version3
    --disable-autodetect
    --enable-libx264 --enable-libx265 --enable-libdav1d
    --enable-zlib
    --disable-doc --disable-ffplay
    --disable-network
    --pkg-config="$PKG_CONFIG"
)
case "$OS" in
    macos)
        ffmpeg_args+=(--cc=clang --enable-videotoolbox --enable-audiotoolbox)
        ;;
    linux)
        ffmpeg_args+=(--enable-rpath --enable-vaapi --enable-ffnvcodec --enable-nvdec --enable-cuvid)
        ;;
    windows)
        ffmpeg_args+=(--toolchain=msvc --enable-d3d11va --enable-dxva2
            --enable-ffnvcodec --enable-nvdec --enable-cuvid
            --extra-cflags="-I$(native_path "$PREFIX/include")"
            --extra-ldflags="-LIBPATH:$(native_path "$PREFIX/lib")")
        ;;
esac
(cd "$dir" && run ffmpeg ./configure "${ffmpeg_args[@]}" &&
    run ffmpeg make -j"$JOBS" && run ffmpeg make install)

# Shared libraries only; import libraries (.lib) stay on Windows.
if [ "$OS" != windows ]; then
    rm -f "$PREFIX"/lib/*.a
fi
rm -rf "$PREFIX/share/ffmpeg/examples"

mkdir -p "$PREFIX/LICENSES"
cp "$BUILD_DIR/ffmpeg-$FFMPEG_VERSION/COPYING.GPLv3" "$PREFIX/LICENSES/FFmpeg-COPYING.GPLv3"
cp "$BUILD_DIR/ffmpeg-$FFMPEG_VERSION/LICENSE.md" "$PREFIX/LICENSES/FFmpeg-LICENSE.md"
cp "$BUILD_DIR/x264-$X264_COMMIT/COPYING" "$PREFIX/LICENSES/x264-COPYING"
cp "$BUILD_DIR/x265_$X265_VERSION/COPYING" "$PREFIX/LICENSES/x265-COPYING"
cp "$BUILD_DIR/dav1d-$DAV1D_VERSION/COPYING" "$PREFIX/LICENSES/dav1d-COPYING"
if [ "$OS" != macos ]; then
    # nv-codec-headers carries its MIT licence in each header.
    sed -n "1,/\*\//p" "$BUILD_DIR/nv-codec-headers-n$NVCODEC_VERSION/include/ffnvcodec/nvEncodeAPI.h" \
        >"$PREFIX/LICENSES/nv-codec-headers-LICENSE"
fi
if [ "$OS" = windows ]; then
    cp "$BUILD_DIR/zlib-$ZLIB_VERSION/LICENSE" "$PREFIX/LICENSES/zlib-LICENSE"
fi

{
    echo "# Source archives of the libraries in this FFmpeg build ($TARGET)."
    echo "# Format: archive  sha256  url"
    for name in "${SHIPPED[@]}"; do
        printf "%s  %s  %s\n" "$(field "$name" ARCHIVE)" "$(field "$name" SHA256)" "$(field "$name" URL)"
    done
    if [ ${#TOOLS[@]} -gt 0 ]; then
        echo "# Build tools used on the build machine, not shipped:"
        for name in "${TOOLS[@]}"; do
            printf "# %s  %s  %s\n" "$(field "$name" ARCHIVE)" "$(field "$name" SHA256)" "$(field "$name" URL)"
        done
    fi
} >"$PREFIX/SOURCES.txt"

ELAPSED=$((SECONDS - START))
{
    echo "target: $TARGET"
    echo "ffmpeg: $FFMPEG_VERSION"
    echo "x264: $X264_COMMIT"
    echo "x265: $X265_VERSION"
    echo "dav1d: $DAV1D_VERSION (meson $MESON_VERSION, ninja $NINJA_VERSION)"
    echo "built: $(date -u +%Y-%m-%dT%H:%M:%SZ) in ${ELAPSED}s with $JOBS jobs"
    echo "configure: ${ffmpeg_args[*]}"
    echo "stamp: $STAMP"
} >"$PREFIX/BUILDINFO"

if [ "$KEEP_BUILD" = 0 ]; then
    rm -rf "${BUILD_DIR:?}"
fi

echo "done   in ${ELAPSED}s; $(du -sh "$PREFIX" | cut -f 1) in $PREFIX"
echo "       export FFMPEG_DIR=\"$PREFIX\""
