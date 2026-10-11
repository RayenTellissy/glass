#!/usr/bin/env bash
# Builds Glass.app and a compressed DMG for one macOS target from an already built
# `glass` binary and the pinned FFmpeg build.
#
# Usage: scripts/package/make-dmg.sh --binary PATH [--target TRIPLE] [--ffmpeg DIR]
#                                    [--out DIR] [--version X.Y.Z]
#   --binary PATH   the release `glass` executable (cargo build --profile dist
#                   --features release-full -p app)
#   --target TRIPLE aarch64-apple-darwin or x86_64-apple-darwin (default: host)
#   --ffmpeg DIR    FFmpeg prefix (default $FFMPEG_DIR, then .glass-cache/ffmpeg/<target>)
#   --out DIR       output folder (default target/package/<target>)
#   --version V     bundle version (default: the workspace version in Cargo.toml)
#
# Environment:
#   GLASS_BUNDLE_ID          CFBundleIdentifier (default below; final ID still to be chosen)
#   GLASS_CODESIGN_IDENTITY  a Developer ID identity; without it everything is signed ad hoc
#
# Layout: Glass.app/Contents/{MacOS/glass, Frameworks/<FFmpeg dylibs>, Resources/,
# Info.plist}. Every FFmpeg, x264, x265 and dav1d dylib is copied under the name its install
# id uses, its id becomes @rpath/<name>, references between them become @rpath/<name>,
# and the executable gets an rpath of @executable_path/../Frameworks. The script fails
# if any bundled Mach-O still references a path outside @rpath, /usr/lib or /System.
# Icons come from assets/icons/macos/ (AppIcon.icns, Assets.car) when present.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

BINARY=""
TARGET=""
FFMPEG="${FFMPEG_DIR:-}"
OUT=""
VERSION=""
BUNDLE_ID="${GLASS_BUNDLE_ID:-org.glass.Glass}"
IDENTITY="${GLASS_CODESIGN_IDENTITY:--}"

HELP_LINES="2,24"
while [ $# -gt 0 ]; do
    case "$1" in
        --binary) need_value "$@"; BINARY="$2"; shift ;;
        --target) need_value "$@"; TARGET="$2"; shift ;;
        --ffmpeg) need_value "$@"; FFMPEG="$2"; shift ;;
        --out) need_value "$@"; OUT="$2"; shift ;;
        --version) need_value "$@"; VERSION="$2"; shift ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) usage_error "unknown argument: $1" ;;
    esac
    shift
done

[ "$(uname -s)" = Darwin ] || die "runs on macOS only"
[ -n "$BINARY" ] || die "--binary is required"
[ -f "$BINARY" ] || die "binary not found: $BINARY"
[ -n "$TARGET" ] || TARGET="$(rustc -vV | sed -n "s/^host: //p")"
case "$TARGET" in
    aarch64-apple-darwin | x86_64-apple-darwin) ;;
    *) die "not a macOS target: $TARGET" ;;
esac
[ -n "$FFMPEG" ] || FFMPEG="$REPO_ROOT/.glass-cache/ffmpeg/$TARGET"
[ -d "$FFMPEG/lib" ] || die "no FFmpeg build at $FFMPEG (run scripts/ffmpeg/build.sh)"
[ -n "$OUT" ] || OUT="$REPO_ROOT/target/package/$TARGET"
[ -n "$VERSION" ] || VERSION="$(workspace_version)"

APP="$OUT/Glass.app"
CONTENTS="$APP/Contents"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Frameworks" "$CONTENTS/Resources"

cp "$BINARY" "$CONTENTS/MacOS/glass"
chmod 755 "$CONTENTS/MacOS/glass"

# Copy each real dylib under the name its install id uses (libavcodec.63.dylib, ...),
# which is the name every dependent references.
bundled=()
for lib in "$FFMPEG"/lib/*.dylib; do
    [ -L "$lib" ] && continue
    id="$(otool -D "$lib" | sed -n 2p)"
    [ -n "$id" ] || die "no install id in $lib"
    name="$(basename "$id")"
    cp "$lib" "$CONTENTS/Frameworks/$name"
    chmod 644 "$CONTENTS/Frameworks/$name"
    bundled+=("$name")
done
[ ${#bundled[@]} -gt 0 ] || die "no dylibs in $FFMPEG/lib"

is_bundled() {
    local n
    for n in "${bundled[@]}"; do
        [ "$n" = "$1" ] && return 0
    done
    return 1
}

# Point every reference to a bundled library at @rpath/<name>.
relink() {
    local file="$1" dep base
    while read -r dep; do
        base="$(basename "$dep")"
        if [ "$dep" != "@rpath/$base" ] && is_bundled "$base"; then
            install_name_tool -change "$dep" "@rpath/$base" "$file" 2>/dev/null
        fi
    done < <(otool -L "$file" | tail -n +2 | awk "{print \$1}")
}

for name in "${bundled[@]}"; do
    lib="$CONTENTS/Frameworks/$name"
    install_name_tool -id "@rpath/$name" "$lib" 2>/dev/null
    relink "$lib"
    if ! otool -l "$lib" | grep -q "path @loader_path "; then
        install_name_tool -add_rpath "@loader_path" "$lib" 2>/dev/null
    fi
done

EXE="$CONTENTS/MacOS/glass"
relink "$EXE"
# Drop rpaths into the build machine (for example the FFmpeg prefix), keep system ones.
while read -r rp; do
    case "$rp" in
        @*) ;;
        *) install_name_tool -delete_rpath "$rp" "$EXE" 2>/dev/null ;;
    esac
done < <(otool -l "$EXE" | awk "/cmd LC_RPATH/{getline; getline; print \$2}")
if ! otool -l "$EXE" | grep -q "path @executable_path/../Frameworks "; then
    install_name_tool -add_rpath "@executable_path/../Frameworks" "$EXE" 2>/dev/null
fi

copy_licences "$FFMPEG" "$CONTENTS/Resources"

# Icons, when the app icon task has produced them.
ICONS="$REPO_ROOT/assets/icons/macos"
if [ -f "$ICONS/AppIcon.icns" ]; then
    cp "$ICONS/AppIcon.icns" "$CONTENTS/Resources/AppIcon.icns"
else
    echo "note: $ICONS/AppIcon.icns not found, bundle has no icon"
fi
[ -f "$ICONS/Assets.car" ] && cp "$ICONS/Assets.car" "$CONTENTS/Resources/Assets.car"

cat >"$CONTENTS/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>Glass</string>
    <key>CFBundleExecutable</key>
    <string>glass</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIconName</key>
    <string>AppIcon</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Glass</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.video</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
EOF
plutil -lint "$CONTENTS/Info.plist" >/dev/null

# Nothing may point outside the bundle or the system.
bad=0
for file in "$EXE" "$CONTENTS"/Frameworks/*.dylib; do
    while read -r dep; do
        case "$dep" in
            @rpath/* | /usr/lib/* | /System/Library/*) ;;
            *)
                echo "unbundled reference in $(basename "$file"): $dep" >&2
                bad=1
                ;;
        esac
    done < <(otool -L "$file" | tail -n +2 | awk "{print \$1}")
done
for dep in $(otool -L "$EXE" | tail -n +2 | awk "{print \$1}" | sed -n "s|^@rpath/||p"); do
    [ -f "$CONTENTS/Frameworks/$dep" ] || { echo "missing from Frameworks: $dep" >&2; bad=1; }
done
[ "$bad" = 0 ] || die "bundle links outside itself"

# install_name_tool invalidates signatures; sign inside out.
sign_args=(--force --sign "$IDENTITY")
[ "$IDENTITY" != "-" ] && sign_args+=(--options runtime --timestamp)
sign() {
    local out
    out="$(codesign "${sign_args[@]}" "$1" 2>&1)" || die "codesign failed for $1: $out"
}
for lib in "$CONTENTS"/Frameworks/*.dylib; do
    sign "$lib"
done
sign "$APP"
codesign --verify --strict "$APP"

DMG="$OUT/glass-$VERSION-$TARGET.dmg"
STAGE="$OUT/dmg-stage"
rm -rf "$STAGE" "$DMG"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname Glass -srcfolder "$STAGE" -fs HFS+ -format ULMO "$DMG"
rm -rf "$STAGE"
[ "$IDENTITY" != "-" ] && codesign --force --sign "$IDENTITY" --timestamp "$DMG"

echo "app  $APP ($(du -sk "$APP" | cut -f 1) KB)"
echo "dmg  $DMG ($(du -k "$DMG" | cut -f 1) KB)"
