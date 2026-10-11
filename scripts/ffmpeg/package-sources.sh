#!/usr/bin/env bash
# Gathers what a release must carry for GPL compliance of the bundled FFmpeg: the exact
# source archives used by scripts/ffmpeg/build.sh, their checksums, the licence texts
# and the build scripts, into one folder ready to attach to a release.
#
# Usage: scripts/ffmpeg/package-sources.sh [--out DIR] [--archive]
#   --out DIR    output folder (default .glass-cache/ffmpeg/release-sources); it is
#                replaced on every run, so an existing non-empty DIR must be one this
#                script wrote (it holds a .glass-release-sources marker)
#   --archive    also write DIR.tar.gz next to the folder
#
# Archives are taken from the source cache when present (and fetched otherwise), and
# every one is checked against versions.env. All targets' sources are included, so
# one bundle covers every installer of a release. The build tools (pkgconf, nasm, meson,
# ninja) are not shipped and are left out.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=versions.env
source "$SCRIPT_DIR/versions.env"

CACHE="${GLASS_FFMPEG_CACHE:-$REPO_ROOT/.glass-cache/ffmpeg}"
DEFAULT_OUT="$CACHE/release-sources"
OUT="$DEFAULT_OUT"
ARCHIVE=0
MARKER=".glass-release-sources"

usage() {
    sed -n "2,16p" "$0" | sed "s/^# \{0,1\}//"
}

usage_error() {
    echo "package-sources.sh: $*" >&2
    usage >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --out)
            [ $# -ge 2 ] && [ -n "$2" ] && [ "${2#-}" = "$2" ] || usage_error "--out needs a folder"
            OUT="$2"
            shift
            ;;
        --archive) ARCHIVE=1 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) usage_error "unknown argument: $1" ;;
    esac
    shift
done

die() {
    echo "package-sources.sh: $*" >&2
    exit 1
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d " " -f 1
    else
        shasum -a 256 "$1" | cut -d " " -f 1
    fi
}

field() {
    local name="$1_$2"
    printf "%s" "${!name}"
}

SHIPPED=(FFMPEG X264 X265 DAV1D NVCODEC ZLIB)
SRC_DIR="$CACHE/src"
mkdir -p "$SRC_DIR"

# OUT is replaced wholesale, so only ever delete a folder this script made.
if [ -e "$OUT" ] || [ -L "$OUT" ]; then
    [ -d "$OUT" ] && [ ! -L "$OUT" ] || die "$OUT exists and is not a folder; refusing to replace it"
    # The default folder predates the marker and is always this script's own.
    if [ "$OUT" != "$DEFAULT_OUT" ] && [ -n "$(ls -A "$OUT")" ] && [ ! -f "$OUT/$MARKER" ]; then
        die "$OUT is not empty and was not written by this script (no $MARKER); refusing to delete it"
    fi
fi
rm -rf "${OUT:?}"
mkdir -p "$OUT/sources" "$OUT/LICENSES" "$OUT/build-scripts"
echo "Written by scripts/ffmpeg/package-sources.sh; the folder is replaced on every run." >"$OUT/$MARKER"

for name in "${SHIPPED[@]}"; do
    archive="$(field "$name" ARCHIVE)"
    sum="$(field "$name" SHA256)"
    src="$SRC_DIR/$archive"
    if [ ! -f "$src" ] || [ "$(sha256_of "$src")" != "$sum" ]; then
        echo "fetch  $archive"
        curl -fL --retry 3 --silent --show-error -o "$src.part" "$(field "$name" URL)"
        got="$(sha256_of "$src.part")"
        if [ "$got" != "$sum" ]; then
            rm -f "$src.part"
            die "checksum mismatch for $archive: expected $sum, got $got"
        fi
        mv -f "$src.part" "$src"
    fi
    cp "$src" "$OUT/sources/$archive"
done

# Licence texts straight from the archives, so they match the shipped versions.
extract() {
    local archive="$1" member="$2" dest="$3"
    tar -xOf "$OUT/sources/$archive" "$member" >"$OUT/LICENSES/$dest"
}
extract "$FFMPEG_ARCHIVE" "ffmpeg-$FFMPEG_VERSION/COPYING.GPLv3" FFmpeg-COPYING.GPLv3
extract "$FFMPEG_ARCHIVE" "ffmpeg-$FFMPEG_VERSION/LICENSE.md" FFmpeg-LICENSE.md
extract "$X264_ARCHIVE" "x264-$X264_COMMIT/COPYING" x264-COPYING
extract "$X265_ARCHIVE" "x265_$X265_VERSION/COPYING" x265-COPYING
extract "$DAV1D_ARCHIVE" "dav1d-$DAV1D_VERSION/COPYING" dav1d-COPYING
extract "$ZLIB_ARCHIVE" "zlib-$ZLIB_VERSION/LICENSE" zlib-LICENSE
tar -xOf "$OUT/sources/$NVCODEC_ARCHIVE" \
    "nv-codec-headers-n$NVCODEC_VERSION/include/ffnvcodec/nvEncodeAPI.h" |
    sed -n "1,/\*\//p" >"$OUT/LICENSES/nv-codec-headers-LICENSE"

cp "$SCRIPT_DIR/build.sh" "$SCRIPT_DIR/versions.env" "$SCRIPT_DIR/package-sources.sh" "$OUT/build-scripts/"

{
    echo "Glass bundles FFmpeg $FFMPEG_VERSION built with x264, x265 and dav1d as shared"
    echo "libraries, licensed under the GNU General Public License version 3 (see LICENSES/);"
    echo "dav1d itself is under the BSD 2-clause licence."
    echo
    echo "sources/ holds the exact, unmodified archives the libraries were built from."
    echo "build-scripts/ holds the script that built them on each target (build.sh, which"
    echo "lists every configure flag) and the pinned versions (versions.env)."
    echo "zlib is linked into the Windows build only; nv-codec-headers into Windows and Linux."
    echo
    echo "archive  sha256  url"
    for name in "${SHIPPED[@]}"; do
        printf "%s  %s  %s\n" "$(field "$name" ARCHIVE)" "$(field "$name" SHA256)" "$(field "$name" URL)"
    done
} >"$OUT/README.txt"

(cd "$OUT/sources" && for f in *; do printf "%s  %s\n" "$(sha256_of "$f")" "$f"; done) >"$OUT/SHA256SUMS"

if [ "$ARCHIVE" = 1 ]; then
    tar -czf "$OUT.tar.gz" -C "$(dirname "$OUT")" "$(basename "$OUT")"
    echo "wrote  $OUT.tar.gz ($(du -h "$OUT.tar.gz" | cut -f 1))"
fi
echo "wrote  $OUT ($(du -sh "$OUT" | cut -f 1))"
