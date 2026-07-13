#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Record the two-monitor shinkai flagship scene as one side-by-side GIF.
#
# This is the "it's a real multi-head setup" proof shot: the config runs a
# different wallpaper on each output (HDMI-A-1 = wallpaper + sakura, DP-1 =
# wallpaper-2 + particles). Wayland has no atomic two-output capture, so we
# run TWO wf-recorders in parallel from a SINGLE lock session (so both
# halves share the same clock time and coherent motion), then stack them
# side by side with ffmpeg.
#
# Both outputs are 1920x1080 here, so no per-output rescale is needed; the
# script still probes dimensions and scales to a common height defensively.
# Stack order matches the physical layout: HDMI-A-1 (left) | DP-1 (right).
#
# Needs the dev shell for wf-recorder + ffmpeg:  cd repo && nix develop
# Launch the scene as a real lock yourself when the banner says so, e.g.:
#   VEILAND_CONFIG=./docs/examples/shinkai.toml ./target/debug/veiland
#
# Usage:
#   scripts/record/shinkai.sh [options]
#
# Options:
#   -L OUTPUT    left output      (default: HDMI-A-1)
#   -R OUTPUT    right output     (default: DP-1)
#   -d SECONDS   record window    (default: 8)
#   -t SECONDS   trim to first N  (default: 5)
#   -w PIXELS    final GIF width  (default: 1200)
#   -f FPS       GIF frame rate   (default: 15)
#   -c SECONDS   countdown        (default: 8)
#   -k           keep the intermediate mp4s

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CONFIG="$REPO_ROOT/docs/examples/shinkai.toml"
ASSET_DIR="$REPO_ROOT/docs/assets/readme"

LEFT="HDMI-A-1"
RIGHT="DP-1"
REC_SECONDS=8
TRIM_SECONDS=5
WIDTH=1200
FPS=15
COUNTDOWN=8
KEEP_MP4=0

while getopts ":L:R:d:t:w:f:c:k" opt; do
  case "$opt" in
    L) LEFT="$OPTARG" ;;
    R) RIGHT="$OPTARG" ;;
    d) REC_SECONDS="$OPTARG" ;;
    t) TRIM_SECONDS="$OPTARG" ;;
    w) WIDTH="$OPTARG" ;;
    f) FPS="$OPTARG" ;;
    c) COUNTDOWN="$OPTARG" ;;
    k) KEEP_MP4=1 ;;
    \?) echo "unknown option: -$OPTARG" >&2; exit 1 ;;
    :)  echo "option -$OPTARG needs a value" >&2; exit 1 ;;
  esac
done

for tool in wf-recorder ffmpeg ffprobe; do
  command -v "$tool" >/dev/null || {
    echo "'$tool' not on PATH. Enter the dev shell:  cd $REPO_ROOT && nix develop" >&2
    exit 1
  }
done
[ -f "$CONFIG" ] || { echo "missing $CONFIG" >&2; exit 1; }
mkdir -p "$ASSET_DIR"

MP4_L="/tmp/veiland-shinkai-left.mp4"
MP4_R="/tmp/veiland-shinkai-right.mp4"
MP4_MERGED="/tmp/veiland-shinkai-merged.mp4"
GIF_OUT="$ASSET_DIR/gallery-shinkai.gif"

cat <<EOF

  scene    : shinkai (two-monitor)   ($CONFIG)
  outputs  : $LEFT (left) | $RIGHT (right)
  record   : ${REC_SECONDS}s, trimmed to first ${TRIM_SECONDS}s
  gif      : ${WIDTH}px wide @ ${FPS}fps  ->  $GIF_OUT

  Launch the scene as a REAL LOCK when the banner says so:
    VEILAND_CONFIG=$CONFIG ./target/debug/veiland
  Both recorders auto-stop; then unlock blind (password + Enter).

EOF

for ((i=COUNTDOWN; i>0; i--)); do
  printf '\r  LOCK THE SCREEN in %2ds ...' "$i"
  sleep 1
done
printf '\r  >>> LOCK NOW  (recording %ss on both outputs)   \n' "$REC_SECONDS"

# Two recorders in parallel, from the one lock session. Start as close
# together as possible so the halves stay in sync.
timeout "$REC_SECONDS" wf-recorder -o "$LEFT"  -f "$MP4_L" &
PID_L=$!
timeout "$REC_SECONDS" wf-recorder -o "$RIGHT" -f "$MP4_R" &
PID_R=$!
wait "$PID_L" || true
wait "$PID_R" || true
echo "  >>> recording stopped. Unlock now (password + Enter)."

for f in "$MP4_L" "$MP4_R"; do
  [ -s "$f" ] || { echo "  ERROR: $f is empty. Wrong output name?" >&2; exit 1; }
done

# Probe heights; pick the smaller as the common target so neither upscales.
h_l=$(ffprobe -v error -select_streams v:0 -show_entries stream=height -of csv=p=0 "$MP4_L")
h_r=$(ffprobe -v error -select_streams v:0 -show_entries stream=height -of csv=p=0 "$MP4_R")
H=$(( h_l < h_r ? h_l : h_r ))

# Trim both to the same window, scale to common height (even width via -2),
# then hstack left|right. Done in one ffmpeg graph on the trimmed inputs.
ffmpeg -hide_banner -loglevel error -y \
  -t "$TRIM_SECONDS" -i "$MP4_L" \
  -t "$TRIM_SECONDS" -i "$MP4_R" \
  -filter_complex \
    "[0:v]scale=-2:$H[l];[1:v]scale=-2:$H[r];[l][r]hstack=inputs=2[v]" \
  -map "[v]" "$MP4_MERGED"

# Merged mp4 -> looping GIF (two-pass palette), scaled to final width.
ffmpeg -hide_banner -loglevel error -y -i "$MP4_MERGED" \
  -vf "fps=$FPS,scale=$WIDTH:-1:flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=3" \
  -loop 0 "$GIF_OUT"

if [ "$KEEP_MP4" -eq 0 ]; then
  rm -f "$MP4_L" "$MP4_R" "$MP4_MERGED"
fi

SIZE="$(du -h "$GIF_OUT" | cut -f1)"
echo "  >>> wrote $GIF_OUT  ($SIZE)"
echo "  >>> preview:  mpv $GIF_OUT"
[ "$KEEP_MP4" -eq 1 ] && echo "  >>> kept mp4s in /tmp"
