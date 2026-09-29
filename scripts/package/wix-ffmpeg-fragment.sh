#!/usr/bin/env bash
# Writes a WiX v3 fragment that installs the FFmpeg DLLs next to glass.exe and the
# licence texts plus the GPL notice under LICENSES\, as the component group
# `FFmpegLibraries` that crates/app/wix/main.wxs references. dist compiles every
# .wxs file in crates/app/wix/, so release.yml runs this before `dist build` on
# Windows (from .github/build-setup.yml). The output is generated, not committed.
#
# Usage: scripts/package/wix-ffmpeg-fragment.sh FFMPEG_PREFIX OUT.wxs

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

[ $# -eq 2 ] || die "usage: $0 FFMPEG_PREFIX OUT.wxs"
FFMPEG="$1"
OUT="$2"
[ -d "$FFMPEG/bin" ] || die "no FFmpeg build at $FFMPEG"

# Stage the licence texts so every file has a stable source path.
STAGE="$REPO_ROOT/target/package/wix-licences"
rm -rf "$STAGE"
copy_licences "$FFMPEG" "$STAGE"

win_path() {
    if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf "%s" "$1"; fi
}

components=()
{
    echo "<?xml version='1.0' encoding='utf-8'?>"
    echo "<Wix xmlns='http://schemas.microsoft.com/wix/2006/wi'>"
    echo "    <Fragment>"
    echo "        <DirectoryRef Id='Bin'>"
    n=0
    for dll in "$FFMPEG"/bin/*.dll; do
        [ -f "$dll" ] || die "no DLLs in $FFMPEG/bin"
        echo "            <Component Id='ffmpeg$n' Guid='*'>"
        echo "                <File Id='ffmpeg$n' Name='$(basename "$dll")' DiskId='1' Source='$(win_path "$dll")' KeyPath='yes'/>"
        echo "            </Component>"
        components+=("ffmpeg$n")
        n=$((n + 1))
    done
    echo "        </DirectoryRef>"
    echo "        <DirectoryRef Id='APPLICATIONFOLDER'>"
    echo "            <Component Id='notice' Guid='*'>"
    echo "                <File Id='notice' Name='NOTICE.txt' DiskId='1' Source='$(win_path "$STAGE/NOTICE.txt")' KeyPath='yes'/>"
    echo "            </Component>"
    components+=("notice")
    echo "            <Directory Id='LicensesDir' Name='LICENSES'>"
    n=0
    for file in "$STAGE"/LICENSES/*; do
        echo "                <Component Id='licence$n' Guid='*'>"
        echo "                    <File Id='licence$n' Name='$(basename "$file")' DiskId='1' Source='$(win_path "$file")' KeyPath='yes'/>"
        echo "                </Component>"
        components+=("licence$n")
        n=$((n + 1))
    done
    echo "            </Directory>"
    echo "        </DirectoryRef>"
    echo "        <ComponentGroup Id='FFmpegLibraries'>"
    for c in "${components[@]}"; do
        echo "            <ComponentRef Id='$c'/>"
    done
    echo "        </ComponentGroup>"
    echo "    </Fragment>"
    echo "</Wix>"
} >"$OUT"

echo "wrote $OUT (${#components[@]} components)"
