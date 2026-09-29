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

# add_entry file kind codec width height frame_rate duration pix_fmt vfr frames audio_codec audio_rate audio_channels [rotation [timecode]]
# rotation is the display matrix's counter-clockwise angle in degrees (default 0), timecode
# the first frame's SMPTE timecode tag (default none).
add_entry() {
    local file=$1 kind=$2 codec=$3 width=$4 height=$5 rate=$6 duration=$7 pix_fmt=$8 vfr=$9
    local frames=${10} a_codec=${11} a_rate=${12} a_channels=${13}
    local rotation=${14:-0} timecode=${15:-null}
    local audio="null"
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
      \"timecode\": $(json_or_null "$timecode")
    }")
    VERIFY_JOBS+=("$file|$kind|$codec|$width|$height|$rate|$duration|$pix_fmt|$vfr|$frames|$a_codec|$a_rate|$a_channels|$rotation|$timecode")
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

make audio_48k_stereo.wav \
    -f lavfi -i "$TONE" -f lavfi -i "$NOISE" \
    -filter_complex "[0:a][1:a]amerge=inputs=2[a]" -map "[a]" -t 3 -c:a pcm_s16le -ar 48000
add_entry audio_48k_stereo.wav audio null null null null 3 null false null pcm_s16le 48000 2

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
    IFS="|" read -r file kind codec width height rate duration pix_fmt vfr frames a_codec a_rate a_channels rotation timecode <<<"$job"
    before=$FAILURES
    path="$OUT/$file"
    input_args=()
    if [ "$kind" = "image_sequence" ]; then
        input_args=(-framerate "${rate%/1}" -start_number 1)
    elif [ ! -e "$path" ]; then
        fail "$file" "missing"
        continue
    fi

    if [ "$kind" != "audio" ]; then
        v=$(probe "$path" v:0 "stream=codec_name,width,height,pix_fmt,r_frame_rate" ${input_args[@]+"${input_args[@]}"})
        check_eq "$file" codec "$(field "$v" codec_name)" "$codec"
        check_eq "$file" width "$(field "$v" width)" "$width"
        check_eq "$file" height "$(field "$v" height)" "$height"
        check_eq "$file" pix_fmt "$(field "$v" pix_fmt)" "$pix_fmt"
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
    fi

    if [ "$kind" = "image_sequence" ]; then
        n=$(probe "$path" v:0 "stream=nb_read_frames" -count_frames ${input_args[@]+"${input_args[@]}"})
        check_eq "$file" frames "$(field "$n" nb_read_frames)" "$frames"
    fi

    if [ "$duration" != "null" ] && [ "$kind" != "image_sequence" ]; then
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
