#!/bin/sh
# Generates the 4K60 H.264 test clip into proto-decode/media/ (git-ignored).
set -eu
cd "$(dirname "$0")/.."
mkdir -p media
FFMPEG="${FFMPEG:-ffmpeg}"
"$FFMPEG" -y -hide_banner -loglevel error \
  -f lavfi -i "testsrc2=size=3840x2160:rate=60:duration=20,noise=alls=12:allf=t+u" \
  -c:v libx264 -profile:v high -preset veryfast -crf 23 -g 120 -keyint_min 120 \
  -bf 2 -pix_fmt yuv420p -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv \
  -movflags +faststart media/clip-4k60.mp4
echo "wrote media/clip-4k60.mp4"
