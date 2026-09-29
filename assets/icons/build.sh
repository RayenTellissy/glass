#!/bin/sh
# Rebuilds macos/, windows/ and linux/ from source/ (files exported from the
# Paper Brand page). Needs macOS with Xcode 26 or later (sips, iconutil, actool)
# and python3. See README.md for what each output uses.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
src="$here/source"
tools="$here/tools"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Pads a tile PNG with transparent pixels to a square canvas.
pad() {
    sips -p "$2" "$2" "$1" --out "$3" >/dev/null
}

# macOS: AppIcon.icns (graphite, every size up to 1024).
# The tile covers 824/1024 of the canvas, Apple's macOS icon grid.
rm -rf "$here/macos"
mkdir -p "$here/macos"
set_dir="$work/AppIcon.iconset"
mkdir -p "$set_dir"
pad "$src/tile/graphite-16pt-13.png" 16 "$set_dir/icon_16x16.png"
pad "$src/tile/graphite-16pt-26.png" 32 "$set_dir/icon_16x16@2x.png"
pad "$src/tile/graphite-32pt-26.png" 32 "$set_dir/icon_32x32.png"
pad "$src/tile/graphite-32pt-52.png" 64 "$set_dir/icon_32x32@2x.png"
pad "$src/tile/graphite-128pt-103.png" 128 "$set_dir/icon_128x128.png"
pad "$src/tile/graphite-128pt-206.png" 256 "$set_dir/icon_128x128@2x.png"
pad "$src/tile/graphite-206.png" 256 "$set_dir/icon_256x256.png"
pad "$src/tile/graphite-412.png" 512 "$set_dir/icon_256x256@2x.png"
pad "$src/tile/graphite-412.png" 512 "$set_dir/icon_512x512.png"
pad "$src/tile/graphite-824.png" 1024 "$set_dir/icon_512x512@2x.png"
iconutil -c icns "$set_dir" -o "$here/macos/AppIcon.icns"

# macOS 26: Icon Composer bundle with light, dark and tinted/clear appearances,
# compiled into Assets.car.
bundle="$here/macos/AppIcon.icon"
mkdir -p "$bundle/Assets"
python3 "$tools/svg.py" "$src/mark-ink.svg" "$bundle/Assets/mark-ink.svg" --size 642
python3 "$tools/svg.py" "$src/mark.svg" "$bundle/Assets/mark.svg" --size 642 --flatten 0.09
python3 "$tools/svg.py" "$src/mark-clear.svg" "$bundle/Assets/mark-clear.svg" --size 642
cp "$tools/glow.svg" "$bundle/Assets/glow.svg"
cat > "$bundle/icon.json" <<'EOF'
{
  "fill" : {
    "linear-gradient" : [ "extended-gray:1.00000,1.00000", "extended-gray:0.89430,1.00000" ],
    "orientation" : { "start" : { "x" : 0.5, "y" : 0 }, "stop" : { "x" : 0.5, "y" : 1 } }
  },
  "fill-specializations" : [
    {
      "appearance" : "dark",
      "value" : {
        "linear-gradient" : [ "extended-gray:0.16860,1.00000", "extended-gray:0.05500,1.00000" ],
        "orientation" : { "start" : { "x" : 0.5, "y" : 0 }, "stop" : { "x" : 0.5, "y" : 1 } }
      }
    }
  ],
  "groups" : [
    {
      "layers" : [
        {
          "name" : "mark-ink",
          "image-name" : "mark-ink.svg",
          "glass" : false,
          "hidden-specializations" : [
            { "appearance" : "dark", "value" : true },
            { "appearance" : "tinted", "value" : true }
          ]
        },
        {
          "name" : "mark",
          "image-name" : "mark.svg",
          "glass" : false,
          "hidden-specializations" : [
            { "value" : true },
            { "appearance" : "dark", "value" : false }
          ]
        },
        {
          "name" : "mark-clear",
          "image-name" : "mark-clear.svg",
          "glass" : false,
          "hidden-specializations" : [
            { "value" : true },
            { "appearance" : "tinted", "value" : false }
          ]
        }
      ],
      "shadow" : { "kind" : "none", "opacity" : 0.5 },
      "translucency" : { "enabled" : false, "value" : 0.5 }
    },
    {
      "layers" : [
        {
          "name" : "glow",
          "image-name" : "glow.svg",
          "glass" : false,
          "hidden-specializations" : [
            { "value" : true },
            { "appearance" : "dark", "value" : false }
          ]
        }
      ],
      "shadow" : { "kind" : "none", "opacity" : 0.5 },
      "specular" : false,
      "translucency" : { "enabled" : false, "value" : 0.5 }
    }
  ],
  "supported-platforms" : { "squares" : [ "macOS" ] }
}
EOF
mkdir -p "$work/car"
xcrun actool "$bundle" --compile "$work/car" --platform macosx \
    --minimum-deployment-target 11.0 --app-icon AppIcon \
    --output-partial-info-plist "$work/car/partial.plist" >/dev/null
cp "$work/car/Assets.car" "$here/macos/Assets.car"

# Windows: bare mark, 3-slice variant below 32 px.
rm -rf "$here/windows"
mkdir -p "$here/windows"
python3 "$tools/ico.py" write "$here/windows/glass.ico" \
    "$src/mark/mark-small-16.png" "$src/mark/mark-small-24.png" \
    "$src/mark/mark-32.png" "$src/mark/mark-48.png" "$src/mark/mark-64.png" \
    "$src/mark/mark-128.png" "$src/mark/mark-256.png"

# Linux: bare mark PNGs for the hicolor theme, plus the scalable SVG.
rm -rf "$here/linux"
mkdir -p "$here/linux"
cp "$src/mark/mark-small-16.png" "$here/linux/glass-16.png"
for size in 32 48 64 128 256 512; do
    cp "$src/mark/mark-$size.png" "$here/linux/glass-$size.png"
done
python3 "$tools/svg.py" "$src/mark.svg" "$here/linux/glass.svg"

echo "Built macos/, windows/ and linux/ in $here"
