#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Record one animated plugin scene as a looping GIF for the README gallery.
#
# Every veiland scene animates, so the showcase is GIFs, not stills. This
# captures a REAL locked session (the compositor lets wf-recorder read the
# lock surface on Hyprland), so the password pill shows and it reads as a
# locker, not a demo. The lock grabs the keyboard, so you cannot type once
# locked: the script counts down, you hit your veiland lock keybind at the
# banner, it records for a fixed window and auto-stops, then you unlock
# blind (type your password + Enter). No unlock is ever shown on camera.
#
# Pipeline: wf-recorder (output -> mp4) -> ffmpeg trim -> ffmpeg GIF with a
# two-pass palette (palettegen/paletteuse) for clean colors. Needs the dev
# shell for wf-recorder + ffmpeg:  cd repo && nix develop
#
# Usage:
#   scripts/record-scene.sh <scene> [options]
#
#   <scene>   name under docs/examples/ (e.g. sakura, snow, blobs), OR a
#             path to a .toml. The GIF is written to
#             docs/assets/readme/gallery-<scene>.gif by default.
#
# Options (env or flags):
#   -o OUTPUT     Wayland output to capture      (default: HDMI-A-1)
#   -d SECONDS    record window length           (default: 8)
#   -t SECONDS    trim the GIF to first N seconds (default: 5)
#   -w PIXELS     GIF width, height auto          (default: 600)
#   -f FPS        GIF frame rate                  (default: 15)
#   -c SECONDS    countdown before recording      (default: 8)
#   -n NAME       output basename without .gif    (default: gallery-<scene>)
#   -k           keep the intermediate mp4s in /tmp
#
# Examples:
#   scripts/record-scene.sh sakura
#   scripts/record-scene.sh raymarcher -w 1280 -d 12 -t 10 -n hero
#   scripts/record-scene.sh snow -o DP-1

set -euo pipefail

# --- repo root (script lives in scripts/) ---------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXAMPLES_DIR="$REPO_ROOT/docs/examples"
ASSET_DIR="$REPO_ROOT/docs/assets/readme"

# --- defaults -------------------------------------------------------------
OUTPUT="HDMI-A-1"
REC_SECONDS=8
TRIM_SECONDS=5
WIDTH=600
FPS=15
COUNTDOWN=8
NAME=""
KEEP_MP4=0

# --- parse args -----------------------------------------------------------
if [ $# -lt 1 ]; then
  sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit 1
fi
SCENE="$1"; shift

while getopts ":o:d:t:w:f:c:n:k" opt; do
  case "$opt" in
    o) OUTPUT="$OPTARG" ;;
    d) REC_SECONDS="$OPTARG" ;;
    t) TRIM_SECONDS="$OPTARG" ;;
    w) WIDTH="$OPTARG" ;;
    f) FPS="$OPTARG" ;;
    c) COUNTDOWN="$OPTARG" ;;
    n) NAME="$OPTARG" ;;
    k) KEEP_MP4=1 ;;
    \?) echo "unknown option: -$OPTARG" >&2; exit 1 ;;
    :)  echo "option -$OPTARG needs a value" >&2; exit 1 ;;
  esac
done

# --- resolve the config ---------------------------------------------------
if [ -f "$SCENE" ]; then
  CONFIG="$SCENE"
  SCENE_NAME="$(basename "${SCENE%.toml}")"
elif [ -f "$EXAMPLES_DIR/$SCENE.toml" ]; then
  CONFIG="$EXAMPLES_DIR/$SCENE.toml"
  SCENE_NAME="$SCENE"
else
  echo "no config for '$SCENE' (looked for '$SCENE' and '$EXAMPLES_DIR/$SCENE.toml')" >&2
  exit 1
fi
[ -n "$NAME" ] || NAME="gallery-$SCENE_NAME"

# --- tool + dir checks ----------------------------------------------------
for tool in wf-recorder ffmpeg; do
  command -v "$tool" >/dev/null || {
    echo "'$tool' not on PATH. Enter the dev shell first:  cd $REPO_ROOT && nix develop" >&2
    exit 1
  }
done
mkdir -p "$ASSET_DIR"

MP4_RAW="/tmp/veiland-rec-$SCENE_NAME.mp4"
MP4_CUT="/tmp/veiland-cut-$SCENE_NAME.mp4"
GIF_OUT="$ASSET_DIR/$NAME.gif"

# --- brief the user -------------------------------------------------------
cat <<EOF

  scene    : $SCENE_NAME   ($CONFIG)
  output   : $OUTPUT
  record   : ${REC_SECONDS}s, trimmed to first ${TRIM_SECONDS}s
  gif      : ${WIDTH}px wide @ ${FPS}fps  ->  $GIF_OUT

  Launch the scene as a REAL LOCK yourself (your veiland keybind or:
    VEILAND_CONFIG=$CONFIG veiland )
  when the banner says so. Recording auto-stops; then unlock blind.

EOF

# --- countdown ------------------------------------------------------------
for ((i=COUNTDOWN; i>0; i--)); do
  printf '\r  LOCK THE SCREEN in %2ds ...' "$i"
  sleep 1
done
printf '\r  >>> LOCK NOW  (recording %ss)            \n' "$REC_SECONDS"

# --- record ---------------------------------------------------------------
timeout "$REC_SECONDS" wf-recorder -o "$OUTPUT" -f "$MP4_RAW" || true
echo "  >>> recording stopped. Unlock now (password + Enter)."

if [ ! -s "$MP4_RAW" ]; then
  echo "  ERROR: no video captured. Wrong output? Try:  hyprctl monitors" >&2
  exit 1
fi

# --- trim (fast, no re-encode) --------------------------------------------
ffmpeg -hide_banner -loglevel error -y -ss 0 -to "$TRIM_SECONDS" \
  -i "$MP4_RAW" -c copy "$MP4_CUT"

# --- mp4 -> looping GIF (two-pass palette) --------------------------------
ffmpeg -hide_banner -loglevel error -y -i "$MP4_CUT" \
  -vf "fps=$FPS,scale=$WIDTH:-1:flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=3" \
  -loop 0 "$GIF_OUT"

# --- cleanup + report -----------------------------------------------------
if [ "$KEEP_MP4" -eq 0 ]; then
  rm -f "$MP4_RAW" "$MP4_CUT"
fi

SIZE="$(du -h "$GIF_OUT" | cut -f1)"
echo "  >>> wrote $GIF_OUT  ($SIZE)"
echo "  >>> preview:  mpv $GIF_OUT"
[ "$KEEP_MP4" -eq 1 ] && echo "  >>> kept mp4s: $MP4_RAW $MP4_CUT"
