#!/usr/bin/env bash
# Builds a Glass MSIX package for x86_64 Windows from an already built glass.exe and
# the pinned FFmpeg build. Runs in CI (release.yml's package job, Git Bash on a
# Windows runner); there is no local Windows build.
#
# Usage: scripts/package/make-msix.sh --binary PATH [--ffmpeg DIR] [--out DIR]
#                                     [--version X.Y.Z]
#
# Needs makeappx.exe from the Windows SDK (MAKEAPPX=/path overrides the search).
# The package is unsigned unless GLASS_MSIX_PFX (a .pfx path) is set, with its
# password in GLASS_MSIX_PFX_PASSWORD; Windows installs only signed MSIX packages,
# and the manifest's Publisher must equal the certificate subject (GLASS_MSIX_PUBLISHER).
# Logos come from assets/icons/linux/ PNGs when present.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

TARGET="x86_64-pc-windows-msvc"
BINARY=""
FFMPEG="${FFMPEG_DIR:-}"
OUT=""
VERSION=""
PUBLISHER="${GLASS_MSIX_PUBLISHER:-CN=Glass contributors}"

while [ $# -gt 0 ]; do
    case "$1" in
        --binary) BINARY="$2"; shift ;;
        --ffmpeg) FFMPEG="$2"; shift ;;
        --out) OUT="$2"; shift ;;
        --version) VERSION="$2"; shift ;;
        -h | --help)
            sed -n "2,14p" "$0" | sed "s/^# \{0,1\}//"
            exit 0
            ;;
        *) die "unknown argument: $1" ;;
    esac
    shift
done

[ -f "$BINARY" ] || die "--binary PATH is required"
[ -n "$FFMPEG" ] || FFMPEG="$REPO_ROOT/.glass-cache/ffmpeg/$TARGET"
[ -d "$FFMPEG/bin" ] || die "no FFmpeg build at $FFMPEG (run scripts/ffmpeg/build.sh)"
[ -n "$OUT" ] || OUT="$REPO_ROOT/target/package/$TARGET"
[ -n "$VERSION" ] || VERSION="$(workspace_version)"

find_sdk_tool() {
    local name="$1" found
    found="$(ls -d "/c/Program Files (x86)/Windows Kits/10/bin/"10.*/x64/"$name" 2>/dev/null | sort -V | tail -n 1)"
    [ -n "$found" ] || die "$name not found in the Windows SDK"
    printf "%s" "$found"
}
MAKEAPPX="${MAKEAPPX:-$(find_sdk_tool makeappx.exe)}"

ROOT="$OUT/msix"
rm -rf "$ROOT"
mkdir -p "$ROOT/Assets"
cp "$BINARY" "$ROOT/glass.exe"
cp "$FFMPEG"/bin/*.dll "$ROOT/"
copy_licences "$FFMPEG" "$ROOT"

ICONS="$REPO_ROOT/assets/icons/linux"
logo() {
    local src="$ICONS/glass-$1.png" dst="$ROOT/Assets/$2"
    if [ -f "$src" ]; then cp "$src" "$dst"; else placeholder_png "$dst"; fi
}
logo 256 Square150x150Logo.png
logo 48 Square44x44Logo.png
logo 64 StoreLogo.png

# MSIX versions have four parts.
cat >"$ROOT/AppxManifest.xml" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
         xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
         xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
         IgnorableNamespaces="uap rescap">
  <Identity Name="Glass" Publisher="$PUBLISHER" Version="$VERSION.0" ProcessorArchitecture="x64"/>
  <Properties>
    <DisplayName>Glass</DisplayName>
    <PublisherDisplayName>Glass contributors</PublisherDisplayName>
    <Logo>Assets\\StoreLogo.png</Logo>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.17763.0" MaxVersionTested="10.0.26100.0"/>
  </Dependencies>
  <Resources>
    <Resource Language="en-us"/>
  </Resources>
  <Applications>
    <Application Id="Glass" Executable="glass.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements DisplayName="Glass" Description="Video editor" BackgroundColor="transparent"
                          Square150x150Logo="Assets\\Square150x150Logo.png" Square44x44Logo="Assets\\Square44x44Logo.png"/>
    </Application>
  </Applications>
  <Capabilities>
    <rescap:Capability Name="runFullTrust"/>
  </Capabilities>
</Package>
EOF

PACKAGE="$OUT/glass-$VERSION-$TARGET.msix"
rm -f "$PACKAGE"
win_path() {
    if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf "%s" "$1"; fi
}
"$MAKEAPPX" pack -o -d "$(win_path "$ROOT")" -p "$(win_path "$PACKAGE")"

if [ -n "${GLASS_MSIX_PFX:-}" ]; then
    SIGNTOOL="${SIGNTOOL:-$(find_sdk_tool signtool.exe)}"
    "$SIGNTOOL" sign -fd SHA256 -f "$(win_path "$GLASS_MSIX_PFX")" -p "${GLASS_MSIX_PFX_PASSWORD:-}" \
        -tr http://timestamp.digicert.com -td SHA256 "$(win_path "$PACKAGE")"
else
    echo "note: $PACKAGE is unsigned (set GLASS_MSIX_PFX to sign it)"
fi

echo "msix  $PACKAGE ($(du -k "$PACKAGE" | cut -f 1) KB)"
