#!/usr/bin/env bash
# Generates the reference media used by tests and the smoke binary from FFmpeg's
# synthetic sources, so no media binaries are committed.
#
# Usage: tests/fixtures/generate.sh [--force] [--verify]
#   --force   regenerate files that already exist
#   --verify  probe every file with ffprobe and compare it with the manifest
#
# Output goes to tests/fixtures/generated/ (git-ignored), with MANIFEST.json listing
# what each file is expected to contain.

set -euo pipefail

FORCE=0
VERIFY=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=1 ;;
        --verify) VERIFY=1 ;;
        -h | --help)
            sed -n "2,11p" "$0" | sed "s/^# \{0,1\}//"
            exit 0
            ;;
        *)
            echo "unknown argument: $arg" >&2
            exit 2
            ;;
    esac
done

find_tool() {
    if command -v "$1" >/dev/null 2>&1; then
        command -v "$1"
    elif [ -n "${FFMPEG_DIR:-}" ] && [ -x "$FFMPEG_DIR/bin/$1" ]; then
        echo "$FFMPEG_DIR/bin/$1"
    else
        echo "error: $1 not found. Install FFmpeg (brew install ffmpeg) or set FFMPEG_DIR." >&2
        exit 1
    fi
}
FFMPEG="$(find_tool ffmpeg)"
FFPROBE="$(find_tool ffprobe)"

OUT="$(cd "$(dirname "$0")" && pwd)/generated"
mkdir -p "$OUT"

START_SECONDS=$SECONDS
GENERATED=0
SKIPPED=0
MANIFEST_ENTRIES=()
VERIFY_JOBS=()

json_or_null() {
    if [ "$1" = "null" ]; then echo "null"; else echo "\"$1\""; fi
}

# add_entry file kind codec width height frame_rate duration pix_fmt vfr frames audio_codec audio_rate audio_channels [rotation [timecode [colour [extra]]]]
# rotation is the display matrix's counter-clockwise angle in degrees (default 0), timecode
# the first frame's SMPTE timecode tag (default none), colour the stream's colour tags as
# "primaries,transfer,matrix,range" in ffprobe's names (default: not checked), extra more
# JSON members for the entry (default none).
#
# Kinds: video, audio, image and image_sequence are read through FFmpeg; still, animation
# and sequence are read by media::source (the image crate, or ImageIO on macOS), and their
# width and height are the upright size after EXIF orientation.
add_entry() {
    local file=$1 kind=$2 codec=$3 width=$4 height=$5 rate=$6 duration=$7 pix_fmt=$8 vfr=$9
    local frames=${10} a_codec=${11} a_rate=${12} a_channels=${13}
    local rotation=${14:-0} timecode=${15:-null} colour=${16:-null} extra=${17:-}
    local audio="null"
    local colour_json="null"
    if [ "$colour" != "null" ]; then
        IFS="," read -r c_prim c_trc c_space c_range <<<"$colour"
        colour_json="{ \"primaries\": \"$c_prim\", \"transfer\": \"$c_trc\", \"matrix\": \"$c_space\", \"range\": \"$c_range\" }"
    fi
    if [ "$a_codec" != "null" ]; then
        audio="{ \"codec\": \"$a_codec\", \"sample_rate\": $a_rate, \"channels\": $a_channels }"
    fi
    MANIFEST_ENTRIES+=("    {
      \"file\": \"$file\",
      \"kind\": \"$kind\",
      \"codec\": $(json_or_null "$codec"),
      \"width\": $width,
      \"height\": $height,
      \"frame_rate\": $(json_or_null "$rate"),
      \"duration\": $duration,
      \"pix_fmt\": $(json_or_null "$pix_fmt"),
      \"vfr\": $vfr,
      \"frames\": $frames,
      \"audio\": $audio,
      \"rotation\": $rotation,
      \"timecode\": $(json_or_null "$timecode"),
      \"colour\": $colour_json${extra:+,
      $extra}
    }")
    VERIFY_JOBS+=("$file|$kind|$codec|$width|$height|$rate|$duration|$pix_fmt|$vfr|$frames|$a_codec|$a_rate|$a_channels|$rotation|$timecode|$colour")
}

# make file -- ffmpeg output arguments (inputs included). Writes to a temporary name
# and renames on success, so an interrupted run never leaves a partial file behind.
make() {
    local file=$1
    shift
    local target="$OUT/$file"
    if [ -e "$target" ] && [ "$FORCE" -eq 0 ]; then
        SKIPPED=$((SKIPPED + 1))
        printf "  skip  %s\n" "$file"
        return
    fi
    local tmp="$OUT/.partial-$file"
    rm -f "$tmp"
    "$FFMPEG" -hide_banner -loglevel error -nostdin -y "$@" "$tmp"
    mv -f "$tmp" "$target"
    GENERATED=$((GENERATED + 1))
    printf "  made  %s\n" "$file"
}

TONE="sine=frequency=440:sample_rate=48000"
NOISE="anoisesrc=color=pink:amplitude=0.1:sample_rate=48000"
STEREO_AUDIO=(-f lavfi -i "$TONE" -f lavfi -i "$NOISE"
    -filter_complex "[1:a][2:a]amerge=inputs=2[a]")

echo "Generating reference media in $OUT"

make h264_1080p30.mp4 \
    -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=3" "${STEREO_AUDIO[@]}" \
    -map 0:v -map "[a]" -t 3 \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 30 \
    -c:a aac -b:a 128k -ar 48000 -movflags +faststart
add_entry h264_1080p30.mp4 video h264 1920 1080 30/1 3 yuv420p false null aac 48000 2

make h264_4k30.mp4 \
    -f lavfi -i "testsrc2=size=3840x2160:rate=30:duration=2" \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 30 -movflags +faststart
add_entry h264_4k30.mp4 video h264 3840 2160 30/1 2 yuv420p false null null null null

# 60 fps with B-frames (b-pyramid), a 2 s open GOP and no scene cuts, so seeking has to
# decode forward up to 119 frames through reordered pictures.
make h264_bframes_gop120_720p60.mp4 \
    -f lavfi -i "testsrc2=size=1280x720:rate=60:duration=3" \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p \
    -x264-params "keyint=120:min-keyint=120:scenecut=0:bframes=3:b-pyramid=normal:open-gop=1" \
    -movflags +faststart
add_entry h264_bframes_gop120_720p60.mp4 video h264 1280 720 60/1 3 yuv420p false null null null null

make h264_10bit_720p30.mp4 \
    -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=2" \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p10le -g 30 -movflags +faststart
add_entry h264_10bit_720p30.mp4 video h264 1280 720 30/1 2 yuv420p10le false null null null null

make hevc_10bit_1080p25.mp4 \
    -f lavfi -i "smptehdbars=size=1920x1080:rate=25:duration=2" \
    -c:v libx265 -preset fast -pix_fmt yuv420p10le -tag:v hvc1 \
    -x265-params "log-level=error:keyint=25" -movflags +faststart
add_entry hevc_10bit_1080p25.mp4 video hevc 1920 1080 25/1 2 yuv420p10le false null null null null

make prores422_1080p23976.mov \
    -f lavfi -i "testsrc2=size=1920x1080:rate=24000/1001:duration=2" "${STEREO_AUDIO[@]}" \
    -map 0:v -map "[a]" -t 2 \
    -c:v prores_ks -profile:v 2 -pix_fmt yuv422p10le -vendor apl0 \
    -c:a pcm_s24le -ar 48000
add_entry prores422_1080p23976.mov video prores 1920 1080 24000/1001 2 yuv422p10le false null pcm_s24le 48000 2

make vp9_720p30.webm \
    -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=2" "${STEREO_AUDIO[@]}" \
    -map 0:v -map "[a]" -t 2 \
    -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -b:v 2M -pix_fmt yuv420p -g 30 \
    -c:a libopus -b:a 96k
add_entry vp9_720p30.webm video vp9 1280 720 30/1 2 yuv420p false null opus 48000 2

SVT_LOG=1 make av1_1080p30.mp4 \
    -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=2" \
    -c:v libsvtav1 -preset 10 -crf 35 -pix_fmt yuv420p -g 30 \
    -svtav1-params "enable-stat-report=0" -movflags +faststart
add_entry av1_1080p30.mp4 video av1 1920 1080 30/1 2 yuv420p false null null null null

# VFR: drop frames 3 to 5 of every 10 and keep the original timestamps, so frame
# durations alternate between 1/30 s and 4/30 s.
make vfr_h264_720p.mp4 \
    -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=3" \
    -vf "select='not(between(mod(n\,10)\,3\,5))'" -fps_mode vfr \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p -video_track_timescale 30000 \
    -movflags +faststart
add_entry vfr_h264_720p.mp4 video h264 1280 720 30/1 3 yuv420p true null null null null

# Colour: vertical bars of exact Y'CbCr codes (geq), so conversions can be checked on
# known patches. setparams tags the frames before encoding, so FFmpeg does not insert a
# matrix or range conversion to match the output tags. 10-bit HEVC in BT.2020 with PQ (top-left chroma siting, HDR10 metadata)
# and with HLG (untagged siting), bars: black, PQ 203-nit reference white (572), 75%
# (721), 100% (940), BT.2020 75% red (237, 418, 848).
BARS10="lum='if(lt(X/W,0.2),64,if(lt(X/W,0.4),572,if(lt(X/W,0.6),721,if(lt(X/W,0.8),940,237))))'"
BARS10="$BARS10:cb='if(lt(X/W,0.8),512,418)':cr='if(lt(X/W,0.8),512,848)'"
make hevc_pq_10bit_640x360.mp4 \
    -f lavfi -i "color=black:size=640x360:rate=25:duration=1,format=yuv420p10le,geq=$BARS10,setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc" \
    -c:v libx265 -preset fast -pix_fmt yuv420p10le -tag:v hvc1 \
    -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv \
    -x265-params "log-level=error:keyint=25:lossless=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited:chromaloc=2:hdr10=1:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400" \
    -movflags +faststart
add_entry hevc_pq_10bit_640x360.mp4 video hevc 640 360 25/1 1 yuv420p10le false null null null null 0 null bt2020,smpte2084,bt2020nc,tv

make hevc_hlg_10bit_640x360.mp4 \
    -f lavfi -i "color=black:size=640x360:rate=25:duration=1,format=yuv420p10le,geq=$BARS10,setparams=range=tv:color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc" \
    -c:v libx265 -preset fast -pix_fmt yuv420p10le -tag:v hvc1 \
    -color_primaries bt2020 -color_trc arib-std-b67 -colorspace bt2020nc -color_range tv \
    -x265-params "log-level=error:keyint=25:lossless=1:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc:range=limited" \
    -movflags +faststart
add_entry hevc_hlg_10bit_640x360.mp4 video hevc 640 360 25/1 1 yuv420p10le false null null null null 0 null bt2020,arib-std-b67,bt2020nc,tv

# Full range 8-bit BT.709 H.264 (FFmpeg's H.264 decoder reports full range 4:2:0 as
# yuvj420p), bars: black (0), 75% (191), white (255), BT.709 75% red
# (41, 106, 224), 50% (128).
BARS8="lum='if(lt(X/W,0.2),0,if(lt(X/W,0.4),191,if(lt(X/W,0.6),255,if(lt(X/W,0.8),41,128))))'"
BARS8="$BARS8:cb='if(between(X/W,0.6,0.8),106,128)':cr='if(between(X/W,0.6,0.8),224,128)'"
make h264_fullrange_640x360.mp4 \
    -f lavfi -i "color=black:size=640x360:rate=25:duration=1,format=yuv420p,geq=$BARS8,setparams=range=pc:color_primaries=bt709:color_trc=bt709:colorspace=bt709" \
    -c:v libx264 -preset veryfast -qp 2 -pix_fmt yuv420p -g 25 \
    -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range pc \
    -x264-params "colorprim=bt709:transfer=bt709:colormatrix=bt709:fullrange=on" \
    -movflags +faststart
add_entry h264_fullrange_640x360.mp4 video h264 640 360 25/1 1 yuvj420p false null null null null 0 null bt709,bt709,bt709,pc

# VFR without B-frames: presentation order equals decode order, so the MP4 index gives
# every presentation time without a packet scan.
make vfr_h264_nob_640x360.mp4 \
    -f lavfi -i "testsrc2=size=640x360:rate=30:duration=2" \
    -vf "select='not(between(mod(n\,10)\,3\,5))'" -fps_mode vfr \
    -c:v libx264 -preset veryfast -bf 0 -pix_fmt yuv420p -video_track_timescale 30000 \
    -movflags +faststart
add_entry vfr_h264_nob_640x360.mp4 video h264 640 360 30/1 2 yuv420p true null null null null

make still_1080p.png \
    -f lavfi -i "smptehdbars=size=1920x1080" -frames:v 1 -pix_fmt rgb24
add_entry still_1080p.png image png 1920 1080 null null rgb24 false 1 null null null

make still_1080p.jpg \
    -f lavfi -i "testsrc2=size=1920x1080" -frames:v 1 -pix_fmt yuvj420p -q:v 3
add_entry still_1080p.jpg image mjpeg 1920 1080 null null yuvj420p false 1 null null null

# Rotation and timecode: a portrait phone clip stores landscape pixels plus a display
# matrix, and camera files carry a start timecode (here a QuickTime tmcd track). Written
# in two passes because -display_rotation applies to an input and is kept by stream copy.
ROT_FILE="rotated_timecode_h264_640x360_25.mov"
if [ -e "$OUT/$ROT_FILE" ] && [ "$FORCE" -eq 0 ]; then
    SKIPPED=$((SKIPPED + 1))
    printf "  skip  %s\n" "$ROT_FILE"
else
    ROT_TMP="$OUT/.partial-unrotated-$ROT_FILE"
    "$FFMPEG" -hide_banner -loglevel error -nostdin -y \
        -f lavfi -i "testsrc2=size=640x360:rate=25:duration=1" \
        -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 25 -f mov "$ROT_TMP"
    "$FFMPEG" -hide_banner -loglevel error -nostdin -y \
        -display_rotation 90 -i "$ROT_TMP" -c copy -timecode 01:02:03:04 \
        -movflags +faststart -f mov "$OUT/.partial-$ROT_FILE"
    rm -f "$ROT_TMP"
    mv -f "$OUT/.partial-$ROT_FILE" "$OUT/$ROT_FILE"
    GENERATED=$((GENERATED + 1))
    printf "  made  %s\n" "$ROT_FILE"
fi
add_entry "$ROT_FILE" video h264 640 360 25/1 1 yuv420p false null null null null 90 01:02:03:04

SEQ_DIR="sequence_640x360_24"
if [ -d "$OUT/$SEQ_DIR" ] && [ "$FORCE" -eq 0 ]; then
    SKIPPED=$((SKIPPED + 1))
    printf "  skip  %s/\n" "$SEQ_DIR"
else
    rm -rf "${OUT:?}/.partial-$SEQ_DIR"
    mkdir -p "$OUT/.partial-$SEQ_DIR"
    "$FFMPEG" -hide_banner -loglevel error -nostdin -y \
        -f lavfi -i "testsrc2=size=640x360:rate=24:duration=2" \
        -pix_fmt rgb24 -start_number 1 "$OUT/.partial-$SEQ_DIR/frame_%04d.png"
    rm -rf "${OUT:?}/$SEQ_DIR"
    mv "$OUT/.partial-$SEQ_DIR" "$OUT/$SEQ_DIR"
    GENERATED=$((GENERATED + 1))
    printf "  made  %s/\n" "$SEQ_DIR"
fi
add_entry "$SEQ_DIR/frame_%04d.png" image_sequence png 640 360 24/1 2 rgb24 false 48 null null null

# produce file function [args...]: runs `function tmp_path args...` to write the file (or,
# when the name ends in "/", a directory) under a temporary name and renames it on success.
produce() {
    local file=$1 fn=$2
    shift 2
    local name=${file%/}
    local target="$OUT/$name"
    if [ -e "$target" ] && [ "$FORCE" -eq 0 ]; then
        SKIPPED=$((SKIPPED + 1))
        printf "  skip  %s\n" "$file"
        return
    fi
    local tmp="$OUT/.partial-$name"
    rm -rf "$tmp"
    "$fn" "$tmp" "$@"
    rm -rf "$target"
    mv -f "$tmp" "$target"
    GENERATED=$((GENERATED + 1))
    printf "  made  %s\n" "$file"
}

ffmpeg_to() {
    local out=$1
    shift
    "$FFMPEG" -hide_banner -loglevel error -nostdin -y "$@" "$out"
}

# Stills, animations and sequences for media::source. Solid frames of known colour make
# pixel spot checks exact; a red top-left block shows orientation.
MARKED="color=c=blue:s=640x360,format=rgb24,drawbox=x=0:y=0:w=160:h=90:c=red:t=fill"

produce still_marked_640x360.tiff ffmpeg_to -f lavfi -i "$MARKED" -frames:v 1 -c:v tiff -f image2
add_entry still_marked_640x360.tiff still tiff 640 360 null null rgb24 false 1 null null null 0 null null \
    '"marker": "top_left"'

produce still_rgba64_320x180.png ffmpeg_to \
    -f lavfi -i "color=c=0x336699:s=320x180,format=rgba64le,colorchannelmixer=aa=0.5" -frames:v 1 -c:v png -pix_fmt rgba64be -f image2
add_entry still_rgba64_320x180.png still png 320 180 null null rgba64be false 1 null null null 0 null null \
    '"bit_depth": 16, "alpha": true'

produce still_marked_640x360.bmp ffmpeg_to -f lavfi -i "$MARKED" -frames:v 1 -c:v bmp -f image2
add_entry still_marked_640x360.bmp still bmp 640 360 null null bgr24 false 1 null null null 0 null null \
    '"marker": "top_left"'

# A marked clip stored landscape with a 90-degree display rotation: the red block is the
# stored top-left corner, so thumbnails can check that the rotation was applied (upright,
# 360x640, block at the bottom left).
rotated_marked() {
    local out=$1 plain="$1.plain.mov"
    ffmpeg_to "$plain" -f lavfi -i "$MARKED,format=yuv420p" -r 25 -t 1 -c:v libx264 \
        -preset veryfast -g 25 -f mov
    ffmpeg_to "$out" -display_rotation 90 -i "$plain" -c copy -f mov
    rm -f "$plain"
}
produce rotated_marked_h264_640x360.mov rotated_marked
add_entry rotated_marked_h264_640x360.mov video h264 640 360 25/1 1 yuv420p false 25 null null null 90 null null \
    '"marker": "top_left"'

# Display P3 tagged with a PNG cICP chunk (FFmpeg writes cICP and cHRM from the tags).
produce still_p3_cicp_640x360.png ffmpeg_to \
    -f lavfi -i "$MARKED,setparams=color_primaries=smpte432:color_trc=iec61966-2-1" -frames:v 1 -c:v png -f image2
add_entry still_p3_cicp_640x360.png still png 640 360 null null rgb24 false 1 null null null 0 null null \
    '"primaries": "smpte432", "marker": "top_left"'

# EXIF orientation 6 (rotate 90 degrees clockwise to display): a 640x360 JPEG whose
# upright picture is 360x640 with the red block at the top right. The APP1 segment is
# spliced in after SOI.
exif_jpeg() {
    local out=$1 plain="$1.plain.jpg"
    ffmpeg_to "$plain" -f lavfi -i "$MARKED" -frames:v 1 -pix_fmt yuvj444p -q:v 2 -f image2
    {
        head -c 2 "$plain"
        printf "\xff\xe1\x00\x22Exif\x00\x00MM\x00\x2a\x00\x00\x00\x08\x00\x01\x01\x12\x00\x03\x00\x00\x00\x01\x00\x06\x00\x00\x00\x00\x00\x00"
        tail -c +3 "$plain"
    } >"$out"
    rm -f "$plain"
}
produce still_exif6_640x360.jpg exif_jpeg
add_entry still_exif6_640x360.jpg still mjpeg 360 640 null null yuvj444p false 1 null null null 90 null null \
    '"orientation": 6, "marker": "top_right"'

# Animated GIF, variable frame delays: source frame N is solid (N*16, 255-N*16, 128);
# frames 3 to 5 of every 10 are dropped, so delays are 100 ms except 400 ms after
# frames 2 and 12.
produce anim_vfr_160x120.gif ffmpeg_to \
    -f lavfi -i "color=c=black:s=160x120:r=10:d=2,format=rgb24,geq=r='N*16':g='255-N*16':b=128" \
    -vf "select='not(between(mod(n\,10)\,3\,5))',split[a][b];[a]palettegen=reserve_transparent=0:stats_mode=full[p];[b][p]paletteuse=dither=none" \
    -fps_mode vfr -loop 0 -f gif
add_entry anim_vfr_160x120.gif animation gif 160 120 null 2 null false 14 null null null 0 null null \
    '"delays_ms": [100, 100, 400, 100, 100, 100, 100, 100, 100, 400, 100, 100, 100, 100], "source_frames": [0, 1, 2, 6, 7, 8, 9, 10, 11, 12, 16, 17, 18, 19]'

# Animated and still WebP (lossless) from solid frames (N*40, 255-N*40, 128), when the
# libwebp tools are installed.
if command -v img2webp >/dev/null 2>&1 && command -v cwebp >/dev/null 2>&1; then
    webp_frames() {
        mkdir -p "$1"
        ffmpeg_to "$1/f_%02d.png" -f lavfi \
            -i "color=c=black:s=160x120:r=10:d=0.6,format=rgb24,geq=r='N*40':g='255-N*40':b=128" -start_number 0
    }
    anim_webp() {
        local out=$1 frames="$1.frames"
        webp_frames "$frames"
        img2webp -loop 0 -lossless -d 100 "$frames/f_00.png" -d 250 "$frames/f_01.png" \
            -d 100 "$frames/f_02.png" -d 400 "$frames/f_03.png" -d 100 "$frames/f_04.png" \
            -d 50 "$frames/f_05.png" -o "$out" >/dev/null 2>&1
        rm -rf "$frames"
    }
    still_webp() {
        local out=$1
        ffmpeg_to "$out.png" -f lavfi -i "$MARKED" -frames:v 1 -f image2
        cwebp -quiet -lossless "$out.png" -o "$out"
        rm -f "$out.png"
    }
    produce anim_vfr_160x120.webp anim_webp
    add_entry anim_vfr_160x120.webp animation webp_anim 160 120 null 1 null false 6 null null null 0 null null \
        '"delays_ms": [100, 250, 100, 400, 100, 50], "source_frames": [0, 1, 2, 3, 4, 5], "colour_step": 40'
    produce still_marked_640x360.webp still_webp
    add_entry still_marked_640x360.webp still webp 640 360 null null null false 1 null null null 0 null null \
        '"marker": "top_left"'
else
    echo "  note  img2webp or cwebp not found; WebP fixtures skipped"
fi

# HEIC (Display P3, from the cICP PNG) and AVIF through macOS sips.
if command -v sips >/dev/null 2>&1 && sips --formats 2>/dev/null | grep -q "heic.*Writable"; then
    sips_to() {
        local out=$1 format=$2 src="$1.src.png"
        ffmpeg_to "$src" -f lavfi -i "$MARKED,setparams=color_primaries=smpte432:color_trc=iec61966-2-1" \
            -frames:v 1 -c:v png -f image2
        sips -s format "$format" "$src" --out "$out.$format" >/dev/null
        mv -f "$out.$format" "$out"
        rm -f "$src"
    }
    produce still_p3_640x360.heic sips_to heic
    add_entry still_p3_640x360.heic still hevc 640 360 null null null false 1 null null null 0 null null \
        '"primaries": "smpte432", "marker": "top_left", "platform": "macos"'
    if sips --formats 2>/dev/null | grep -q "avif.*Writable"; then
        produce still_p3_640x360.avif sips_to avif
        add_entry still_p3_640x360.avif still av1 640 360 null null null false 1 null null null 0 null null \
            '"primaries": "smpte432", "marker": "top_left", "platform": "macos"'
    fi
else
    echo "  note  sips with HEIC output not found (macOS only); HEIC and AVIF fixtures skipped"
fi

# A numbered sequence with gaps: shot_0001.png to shot_0012.png without 5, 6 and 10;
# number k is solid ((k-1)*20, 0, 255-(k-1)*20).
gap_sequence() {
    mkdir -p "$1"
    ffmpeg_to "$1/shot_%04d.png" -f lavfi \
        -i "color=c=black:s=320x180:r=24:d=0.5,format=rgb24,geq=r='N*20':g=0:b='255-N*20'" -start_number 1
    rm -f "$1/shot_0005.png" "$1/shot_0006.png" "$1/shot_0010.png"
}
produce sequence_gaps_320x180/ gap_sequence
add_entry "sequence_gaps_320x180/shot_%04d.png" sequence png 320 180 null null rgb24 false 9 null null null 0 null null \
    '"first": 1, "last": 12, "missing": [5, 6, 10]'

# A/V sync markers: a white frame at 1.000 s (frame 30 at 30 fps) and, at the same time, a
# 5 ms Hann-windowed 2 kHz burst in both audio channels. Encoded with the priming each
# codec brings (AAC 1024 samples, Opus 312-sample pre-skip) and as PCM for reference.
SYNC_VIDEO="color=black:s=320x240:r=30:d=3,format=yuv420p,drawbox=x=0:y=0:w=iw:h=ih:c=white:t=fill:enable='eq(n\,30)'"
sync_audio() {
    local rate=$1
    local burst="0.8*sin(2*PI*2000*(t-1))*if(between(t\,1\,1.005)\,pow(sin(PI*(t-1)/0.005)\,2)\,0)"
    echo "aevalsrc='$burst|$burst':c=stereo:s=$rate:d=3"
}
SYNC_EXTRA='"sync_marker_s": 1.0'

make sync_h264_aac_48k.mp4 \
    -f lavfi -i "$SYNC_VIDEO" -f lavfi -i "$(sync_audio 48000)" \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 30 \
    -c:a aac -b:a 192k -movflags +faststart
add_entry sync_h264_aac_48k.mp4 video h264 320 240 30/1 3 yuv420p false null aac 48000 2 0 null null "$SYNC_EXTRA"

make sync_h264_aac_44k1.mp4 \
    -f lavfi -i "$SYNC_VIDEO" -f lavfi -i "$(sync_audio 44100)" \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 30 \
    -c:a aac -b:a 192k -movflags +faststart
add_entry sync_h264_aac_44k1.mp4 video h264 320 240 30/1 3 yuv420p false null aac 44100 2 0 null null "$SYNC_EXTRA"

make sync_vp9_opus.webm \
    -f lavfi -i "$SYNC_VIDEO" -f lavfi -i "$(sync_audio 48000)" \
    -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -b:v 500k -pix_fmt yuv420p -g 30 \
    -c:a libopus -b:a 128k
add_entry sync_vp9_opus.webm video vp9 320 240 30/1 3 yuv420p false null opus 48000 2 0 null null "$SYNC_EXTRA"

make sync_h264_pcm.mov \
    -f lavfi -i "$SYNC_VIDEO" -f lavfi -i "$(sync_audio 48000)" \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 30 \
    -c:a pcm_s16le
add_entry sync_h264_pcm.mov video h264 320 240 30/1 3 yuv420p false null pcm_s16le 48000 2 0 null null "$SYNC_EXTRA"

# 5.1 (side) with a constant level per channel (FL 0.1, FR 0.2, FC 0.3, LFE 0.4, SL 0.5,
# SR 0.6), for the downmix rules.
make audio_51_48k.wav \
    -f lavfi -i "aevalsrc='0.1|0.2|0.3|0.4|0.5|0.6':c=5.1(side):s=48000:d=1" -c:a pcm_s16le
add_entry audio_51_48k.wav audio null null null null 1 null false null pcm_s16le 48000 6 0 null null \
    '"levels": [0.1, 0.2, 0.3, 0.4, 0.5, 0.6]'

# Mono 1 kHz sine (amplitude 1/8) at 44.1 kHz as MP3, whose gapless header FFmpeg honours.
make audio_44k1_mono.mp3 \
    -f lavfi -i "sine=frequency=1000:sample_rate=44100:duration=2" -c:a libmp3lame -b:a 192k
add_entry audio_44k1_mono.mp3 audio null null null null 2 null false null mp3 44100 1 0 null null \
    '"sine_hz": 1000, "sine_amplitude": 0.125'

make audio_48k_stereo.wav \
    -f lavfi -i "$TONE" -f lavfi -i "$NOISE" \
    -filter_complex "[0:a][1:a]amerge=inputs=2[a]" -map "[a]" -t 3 -c:a pcm_s16le -ar 48000
add_entry audio_48k_stereo.wav audio null null null null 3 null false null pcm_s16le 48000 2 0 null null \
    '"sine_hz": 440, "sine_amplitude": 0.125, "sine_channel": 0'

# Scene cuts: six 2 s shots at 30 fps with small motion, joined with hard cuts at frames
# 60, 120, 180 and 300: testsrc2 (moving), a Mandelbrot zoom with a two-frame white flash
# at frames 90 and 91, fast animated gradients, RGB bars fading out to black over their
# last 15 frames, testsrc fading in from black over its first 15 (a fade through black
# around frame 240) and a pan across HD bars.
SHOT="format=yuv420p,setsar=1"
make scenes_cuts_640x360_30.mp4 \
    -f lavfi -i "testsrc2=size=640x360:rate=30:duration=2,$SHOT" \
    -f lavfi -i "mandelbrot=size=640x360:rate=30,trim=end_frame=60,drawbox=c=white@0.8:t=fill:enable='between(n,30,31)',$SHOT" \
    -f lavfi -i "gradients=size=640x360:rate=30:duration=2:speed=0.03,$SHOT" \
    -f lavfi -i "rgbtestsrc=size=640x360:rate=30:duration=2,fade=t=out:start_frame=45:nb_frames=15,$SHOT" \
    -f lavfi -i "testsrc=size=640x360:rate=30:duration=2,fade=t=in:start_frame=0:nb_frames=15,$SHOT" \
    -f lavfi -i "smptehdbars=size=1280x720:rate=30:duration=2,crop=640:360:x='n*4':y=180,$SHOT" \
    -filter_complex "[0:v][1:v][2:v][3:v][4:v][5:v]concat=n=6:v=1:a=0[v]" -map "[v]" \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p -g 30 -movflags +faststart
add_entry scenes_cuts_640x360_30.mp4 video h264 640 360 30/1 12 yuv420p false 360 null null null 0 null null \
    '"hard_cuts": [60, 120, 180, 300], "fade_through_black": 240, "flash_frames": [90, 91]'

# Silence: 10 s of stereo over a uniform noise floor of +-0.0015 (-61 dBFS RMS) per
# channel, with 1 kHz tone at 0.25 (-15 dBFS RMS) in both channels at 0.7-1.5 s and
# 5.0-5.02 s, in the left channel only at 2.9-3.5 s, a white noise burst (-15 dBFS RMS)
# at 2.3-2.6 s and a quiet tone at -30 dBFS RMS at 6.0-7.2 s. Silences at -40 dBFS and
# 500 ms: 0-0.7, 1.5-2.3, 3.5-5.0, 5.02-6.0 and 7.2-10 s; 2.6-2.9 s is too short.
on() { echo "gte(t\\,$1)*lt(t\\,$2)"; }
SIL_TONE="sin(2*PI*1000*t)"
SIL_COMMON="0.0447*$SIL_TONE*$(on 6 7.2)"
SIL_L="0.003*(random(0)-0.5)+0.25*$SIL_TONE*($(on 0.7 1.5)+$(on 2.9 3.5)+$(on 5 5.02))+$SIL_COMMON+0.6*(random(1)-0.5)*$(on 2.3 2.6)"
SIL_R="0.003*(random(2)-0.5)+0.25*$SIL_TONE*($(on 0.7 1.5)+$(on 5 5.02))+$SIL_COMMON+0.6*(random(3)-0.5)*$(on 2.3 2.6)"
make silence_48k_stereo.wav \
    -f lavfi -i "aevalsrc='$SIL_L|$SIL_R':c=stereo:s=48000:d=10" -c:a pcm_s16le
add_entry silence_48k_stereo.wav audio null null null null 10 null false null pcm_s16le 48000 2 0 null null \
    '"silences": [[0.0, 0.7], [1.5, 2.3], [3.5, 5.0], [5.02, 6.0], [7.2, 10.0]]'
# The same program as AAC in MP4 (lossy, with encoder priming that the decoder removes).
make silence_48k_stereo.m4a -i "$OUT/silence_48k_stereo.wav" -c:a aac -b:a 160k -movflags +faststart
add_entry silence_48k_stereo.m4a audio null null null null 10 null false null aac 48000 2 0 null null \
    '"silences": [[0.0, 0.7], [1.5, 2.3], [3.5, 5.0], [5.02, 6.0], [7.2, 10.0]]'

# Beats: mono 48 kHz 16-bit FLAC drum patterns (lossless: AAC codes the noise hats with
# perceptual noise substitution, which a seek does not repeat) whose first beat is at 0.1 s. ld(0) is the time
# since 0.1 s, ld(1) the time since the last beat, ld(2) the beat number.
# kick: a 55 Hz thump with a 2 kHz attack; hat: a white noise tick; click: 1.5 kHz.
beat_vars() { echo "st(0\\,t-0.1);st(1\\,mod(ld(0)\\,$1));st(2\\,floor(ld(0)/$1));gte(ld(0)\\,0)*"; }
KICK="(0.8*exp(-ld(1)*25)*sin(2*PI*55*ld(1))+0.3*exp(-ld(1)*300)*sin(2*PI*2000*ld(1)))"
hat() { echo "$1*(random(4)-0.5)*exp(-(ld(1)-$2)*80)*gte(ld(1)\\,$2)"; }
beat_track() {
    local file=$1 expr=$2 extra=$3
    make "$file" -f lavfi -i "aevalsrc='$expr':c=mono:s=48000:d=20" -c:a flac -sample_fmt s16
    add_entry "$file" audio null null null null 20 null false null flac 48000 1 0 null null "$extra"
}
# 90 BPM: a kick on every beat, soft hats on the eighths.
P=0.666666667
beat_track beats_090_kick_hats.flac "$(beat_vars $P)($KICK+$(hat 0.2 0)+$(hat 0.2 $(echo "$P/2" | bc -l)))" \
    '"bpm": 90, "first_beat_s": 0.1, "style": "kick on beats, soft eighth-note hats"'
# 120 BPM: a metronome, accented every fourth click.
P=0.5
beat_track beats_120_clicks.flac \
    "$(beat_vars $P)0.5*(1+eq(mod(ld(2)\\,4)\\,0))*exp(-ld(1)*200)*sin(2*PI*1500*ld(1))" \
    '"bpm": 120, "first_beat_s": 0.1, "style": "accented metronome"'
# 128 BPM: four on the floor with an off-beat open hat.
P=0.46875
beat_track beats_128_house.flac "$(beat_vars $P)($KICK+$(hat 0.4 0.234375))" \
    '"bpm": 128, "first_beat_s": 0.1, "style": "four on the floor, off-beat hats"'
# 174 BPM: kick on odd beats, a noise snare on even beats.
P=0.344827586
beat_track beats_174_kick_snare.flac \
    "$(beat_vars $P)(eq(mod(ld(2)\\,2)\\,0)*$KICK+eq(mod(ld(2)\\,2)\\,1)*1.2*(random(5)-0.5)*exp(-ld(1)*30))" \
    '"bpm": 174, "first_beat_s": 0.1, "style": "kick and snare alternating"'
# 100 BPM swing: kick on beats, hats on the beat and on the swung eighth (2/3 of a beat).
P=0.6
beat_track beats_100_swing.flac "$(beat_vars $P)($KICK+$(hat 0.25 0)+$(hat 0.25 0.4))" \
    '"bpm": 100, "first_beat_s": 0.1, "style": "swung eighth-note hats (2/3)"'
# Tempo drift: accented clicks speeding up linearly from 116 to 124 BPM over 20 s; beat k
# is where the phase (116 u + 0.2 u^2) / 60 = k, u = t - 0.1.
DRIFT="st(0\\,t-0.1);st(3\\,(116*ld(0)+0.2*ld(0)*ld(0))/60);st(1\\,(ld(3)-floor(ld(3)))*60/(116+0.4*ld(0)));gte(ld(0)\\,0)*0.5*exp(-ld(1)*200)*sin(2*PI*1500*ld(1))"
beat_track beats_drift_116_124_clicks.flac "$DRIFT" \
    '"bpm_start": 116, "bpm_end": 124, "first_beat_s": 0.1, "style": "clicks, linear tempo drift over 20 s (phase (116 u + 0.2 u^2) / 60)"'

# Loudness: FFmpeg's ebur128 filter measures each file when the manifest is written
# (integrated LUFS, loudness range LU, true peak dBTP), for media::analysis::loudness to
# match within 0.1 LU.
ebur128_ref() {
    "$FFMPEG" -hide_banner -nostats -nostdin -i "$OUT/$1" -af ebur128=peak=true -f null - 2>&1 |
        awk '/Summary:/ { s = 1 } s && $1 == "I:" { i = $2 } s && $1 == "LRA:" { l = $2 } s && $1 == "Peak:" { p = $2 }
             END { printf "\"ebur128\": { \"integrated\": %s, \"lra\": %s, \"true_peak\": %s }", i, l, p }'
}
# 30 s of stereo noise (pink left, brown right) at four levels, and a 1 kHz tone in the
# left channel at 12-14 s; also resampled to 44.1 kHz FLAC (measured at its own rate;
# FFmpeg's filter measures at 48 kHz).
LOUD_PROGRAM=(-f lavfi -i "anoisesrc=c=pink:seed=11:a=0.5:r=48000:d=30"
    -f lavfi -i "anoisesrc=c=brown:seed=12:a=0.5:r=48000:d=30"
    -f lavfi -i "sine=f=1000:r=48000:d=30"
    -filter_complex "[0:a][1:a]amerge=inputs=2,volume='if(lt(t,8),0.4,if(lt(t,16),0.08,if(lt(t,24),0.2,0.6)))':eval=frame[n];[2:a]volume='0.3*between(t,12,14)':eval=frame,pan=stereo|c0=c0|c1=0*c0[s];[n][s]amix=inputs=2:normalize=0[a]"
    -map "[a]")
make loudness_program_48k_stereo.wav "${LOUD_PROGRAM[@]}" -c:a pcm_s16le
add_entry loudness_program_48k_stereo.wav audio null null null null 30 null false null pcm_s16le 48000 2 0 null null \
    "$(ebur128_ref loudness_program_48k_stereo.wav)"
make loudness_program_44k1_stereo.flac "${LOUD_PROGRAM[@]}" -ar 44100 -c:a flac -sample_fmt s16
add_entry loudness_program_44k1_stereo.flac audio null null null null 30 null false null flac 44100 2 0 null null \
    "$(ebur128_ref loudness_program_44k1_stereo.flac)"
# 12 s of 5.1 (side) pink noise, a different seed and level per channel (the LFE loud, so
# leaving it out matters).
make loudness_51_48k.wav \
    -f lavfi -i "anoisesrc=c=pink:seed=21:a=0.3:r=48000:d=13" -f lavfi -i "anoisesrc=c=pink:seed=22:a=0.2:r=48000:d=13" \
    -f lavfi -i "anoisesrc=c=pink:seed=23:a=0.25:r=48000:d=13" -f lavfi -i "anoisesrc=c=brown:seed=24:a=0.8:r=48000:d=13" \
    -f lavfi -i "anoisesrc=c=pink:seed=25:a=0.15:r=48000:d=13" -f lavfi -i "anoisesrc=c=pink:seed=26:a=0.1:r=48000:d=13" \
    -filter_complex "[0:a][1:a][2:a][3:a][4:a][5:a]join=inputs=6:channel_layout=5.1(side)[a]" -map "[a]" -t 12 -c:a pcm_s16le
add_entry loudness_51_48k.wav audio null null null null 12 null false null pcm_s16le 48000 6 0 null null \
    "$(ebur128_ref loudness_51_48k.wav)"

{
    echo "{"
    echo "  \"version\": 1,"
    echo "  \"generator\": \"tests/fixtures/generate.sh\","
    echo "  \"files\": ["
    last=$((${#MANIFEST_ENTRIES[@]} - 1))
    for i in "${!MANIFEST_ENTRIES[@]}"; do
        if [ "$i" -lt "$last" ]; then
            echo "${MANIFEST_ENTRIES[$i]},"
        else
            echo "${MANIFEST_ENTRIES[$i]}"
        fi
    done
    echo "  ]"
    echo "}"
} >"$OUT/MANIFEST.json"

ELAPSED=$((SECONDS - START_SECONDS))
TOTAL_KB=$(du -sk "$OUT" | cut -f1)
echo "Summary: $GENERATED generated, $SKIPPED skipped, ${#MANIFEST_ENTRIES[@]} manifest entries," \
    "$((TOTAL_KB / 1024)) MB total, ${ELAPSED}s"

[ "$VERIFY" -eq 1 ] || exit 0

# probe path stream entries: prints key=value lines for the first matching stream.
probe() {
    "$FFPROBE" -v error -select_streams "$2" -show_entries "$3" -of default=nw=1 "${@:4}" "$1" |
        head -n 20
}

field() {
    printf "%s\n" "$1" | sed -n "s/^$2=//p" | head -n 1
}

FAILURES=0
fail() {
    echo "  FAIL  $1: $2"
    FAILURES=$((FAILURES + 1))
}

check_eq() {
    if [ "$3" != "$4" ]; then fail "$1" "$2 is '$3', expected '$4'"; fi
}

# Frame durations in stream ticks; VFR when the longest is over 1.5x the shortest.
is_vfr() {
    "$FFPROBE" -v error -select_streams v:0 -show_entries packet=pts -of csv=p=0 "$1" |
        grep -v "N/A" | sort -n |
        awk 'NR > 1 { d = $1 - prev; if (min == "" || d < min) min = d; if (d > max) max = d }
             { prev = $1 }
             END { if (min > 0 && max > 1.5 * min) print "true"; else print "false" }'
}

echo "Verifying against MANIFEST.json"
for job in "${VERIFY_JOBS[@]}"; do
    IFS="|" read -r file kind codec width height rate duration pix_fmt vfr frames a_codec a_rate a_channels rotation timecode colour <<<"$job"
    before=$FAILURES
    path="$OUT/$file"
    input_args=()
    if [ "$kind" = "image_sequence" ]; then
        input_args=(-framerate "${rate%/1}" -start_number 1)
    elif [ "$kind" = "sequence" ]; then
        # Gaps stop FFmpeg's image2 demuxer, so count the files and probe the first one.
        present=$(find "$(dirname "$path")" -name "*.png" | wc -l | tr -d " ")
        check_eq "$file" frames "$present" "$frames"
        path=$(find "$(dirname "$path")" -name "*.png" | sort | head -n 1)
    elif [ ! -e "$path" ]; then
        fail "$file" "missing"
        continue
    fi

    if [ "$kind" != "audio" ]; then
        v=$(probe "$path" v:0 "stream=codec_name,width,height,pix_fmt,r_frame_rate" ${input_args[@]+"${input_args[@]}"})
        check_eq "$file" codec "$(field "$v" codec_name)" "$codec"
        # Stills list their upright size; the stored picture of a 90 or 270 degree EXIF
        # orientation is the other way round.
        stored_w=$width stored_h=$height
        if [ "$kind" = "still" ] && { [ "$rotation" = "90" ] || [ "$rotation" = "270" ]; }; then
            stored_w=$height stored_h=$width
        fi
        check_eq "$file" width "$(field "$v" width)" "$stored_w"
        check_eq "$file" height "$(field "$v" height)" "$stored_h"
        if [ "$pix_fmt" != "null" ]; then
            check_eq "$file" pix_fmt "$(field "$v" pix_fmt)" "$pix_fmt"
        fi
        if [ "$kind" = "video" ] && [ "$vfr" = "false" ]; then
            check_eq "$file" frame_rate "$(field "$v" r_frame_rate)" "$rate"
        fi
    fi

    if [ "$kind" = "video" ]; then
        check_eq "$file" vfr "$(is_vfr "$path")" "$vfr"
        r=$(probe "$path" v:0 "stream_side_data=rotation")
        check_eq "$file" rotation "$(field "$r" rotation)" "$([ "$rotation" = "0" ] || echo "$rotation")"
        t=$(probe "$path" v:0 "stream_tags=timecode")
        check_eq "$file" timecode "$(field "$t" "TAG:timecode")" "$([ "$timecode" = "null" ] || echo "$timecode")"
        if [ "$colour" != "null" ]; then
            c=$(probe "$path" v:0 "stream=color_primaries,color_transfer,color_space,color_range")
            check_eq "$file" colour \
                "$(field "$c" color_primaries),$(field "$c" color_transfer),$(field "$c" color_space),$(field "$c" color_range)" \
                "$colour"
        fi
    fi

    if [ "$kind" = "animation" ]; then
        n=$(probe "$path" v:0 "stream=nb_read_frames" -count_frames)
        check_eq "$file" frames "$(field "$n" nb_read_frames)" "$frames"
    fi

    if [ "$kind" = "image_sequence" ]; then
        n=$(probe "$path" v:0 "stream=nb_read_frames" -count_frames ${input_args[@]+"${input_args[@]}"})
        check_eq "$file" frames "$(field "$n" nb_read_frames)" "$frames"
    fi

    if [ "$duration" != "null" ] && { [ "$kind" = "video" ] || [ "$kind" = "audio" ]; }; then
        d=$("$FFPROBE" -v error -show_entries format=duration -of csv=p=0 "$path")
        if ! awk -v d="$d" -v e="$duration" 'BEGIN { x = d - e; if (x < 0) x = -x; exit !(x <= 0.1) }'; then
            fail "$file" "duration is $d s, expected $duration s"
        fi
    fi

    if [ "$a_codec" != "null" ]; then
        a=$(probe "$path" a:0 "stream=codec_name,sample_rate,channels")
        check_eq "$file" audio_codec "$(field "$a" codec_name)" "$a_codec"
        check_eq "$file" sample_rate "$(field "$a" sample_rate)" "$a_rate"
        check_eq "$file" channels "$(field "$a" channels)" "$a_channels"
    fi

    if [ "$FAILURES" -eq "$before" ]; then printf "  ok    %s\n" "$file"; fi
done

if [ "$FAILURES" -gt 0 ]; then
    echo "Verification failed: $FAILURES problem(s)"
    exit 1
fi
echo "Verification passed: ${#VERIFY_JOBS[@]} entries"
