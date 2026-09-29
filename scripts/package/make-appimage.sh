#!/usr/bin/env bash
# Builds a Glass AppImage for x86_64 Linux from an already built `glass` binary and
# the pinned FFmpeg build. Runs in CI (release.yml's package job); there is no local
# Linux build.
#
# Usage: scripts/package/make-appimage.sh --binary PATH [--ffmpeg DIR] [--out DIR]
#                                         [--version X.Y.Z]
#
# Needs `patchelf` and `appimagetool` (APPIMAGETOOL=/path overrides the one on PATH).
# The FFmpeg shared libraries go to usr/lib/ with RUNPATH $ORIGIN, the binary gets a
# RUNPATH of $ORIGIN/../lib, and the licences and the GPL notice go to
# usr/share/glass/. Icons come from assets/icons/linux/ when present.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

TARGET="x86_64-unknown-linux-gnu"
BINARY=""
FFMPEG="${FFMPEG_DIR:-}"
OUT=""
VERSION=""
APPIMAGETOOL="${APPIMAGETOOL:-appimagetool}"

while [ $# -gt 0 ]; do
    case "$1" in
        --binary) BINARY="$2"; shift ;;
        --ffmpeg) FFMPEG="$2"; shift ;;
        --out) OUT="$2"; shift ;;
        --version) VERSION="$2"; shift ;;
        -h | --help)
            sed -n "2,12p" "$0" | sed "s/^# \{0,1\}//"
            exit 0
            ;;
        *) die "unknown argument: $1" ;;
    esac
    shift
done

[ "$(uname -s)" = Linux ] || die "runs on Linux only"
[ -f "$BINARY" ] || die "--binary PATH is required"
[ -n "$FFMPEG" ] || FFMPEG="$REPO_ROOT/.glass-cache/ffmpeg/$TARGET"
[ -d "$FFMPEG/lib" ] || die "no FFmpeg build at $FFMPEG (run scripts/ffmpeg/build.sh)"
[ -n "$OUT" ] || OUT="$REPO_ROOT/target/package/$TARGET"
[ -n "$VERSION" ] || VERSION="$(workspace_version)"
command -v patchelf >/dev/null || die "patchelf not found"

APPDIR="$OUT/Glass.AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" "$APPDIR/usr/share/glass"

cp "$BINARY" "$APPDIR/usr/bin/glass"
chmod 755 "$APPDIR/usr/bin/glass"
patchelf --set-rpath "\$ORIGIN/../lib" "$APPDIR/usr/bin/glass"

# Shared libraries with their SONAME symlinks.
cp -a "$FFMPEG"/lib/*.so* "$APPDIR/usr/lib/"
for lib in "$APPDIR"/usr/lib/*.so*; do
    [ -L "$lib" ] && continue
    patchelf --set-rpath "\$ORIGIN" "$lib"
done

copy_licences "$FFMPEG" "$APPDIR/usr/share/glass"

cat >"$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/glass" "$@"
EOF
chmod 755 "$APPDIR/AppRun"

cat >"$APPDIR/glass.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Glass
Comment=Video editor
Exec=glass %F
Icon=glass
Categories=AudioVideo;Video;AudioVideoEditing;
Terminal=false
X-AppImage-Version=$VERSION
EOF

ICONS="$REPO_ROOT/assets/icons/linux"
if [ -f "$ICONS/glass-256.png" ]; then
    cp "$ICONS/glass-256.png" "$APPDIR/glass.png"
    for png in "$ICONS"/glass-*.png; do
        size="$(basename "$png" .png)"
        size="${size#glass-}"
        mkdir -p "$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps"
        cp "$png" "$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps/glass.png"
    done
else
    placeholder_png "$APPDIR/glass.png"
fi
ln -sf glass.png "$APPDIR/.DirIcon"

IMAGE="$OUT/glass-$VERSION-$TARGET.AppImage"
rm -f "$IMAGE"
# Extract-and-run avoids needing FUSE on the build machine.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$APPIMAGETOOL" --no-appstream "$APPDIR" "$IMAGE"

echo "appimage  $IMAGE ($(du -k "$IMAGE" | cut -f 1) KB)"
