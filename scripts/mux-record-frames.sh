#!/usr/bin/env bash
# Mux semantic PPM frames (or macOS window PNGs) to mp4. ffmpeg is optional.
set -euo pipefail

DIR="${1:-}"
OUT="${2:-}"
if [[ -z "$DIR" ]]; then
  echo "usage: $0 FRAMES_DIR [out.mp4]" >&2
  exit 2
fi
OUT="${OUT:-$DIR/recipe-run.mp4}"

if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "ffmpeg not found. Semantic frames are already in $DIR (*.svg / *.ppm)." >&2
  exit 1
fi

if compgen -G "$DIR/*-window.png" > /dev/null; then
  ffmpeg -y -loglevel error -framerate 5 -pattern_type glob -i "$DIR/*-window.png" \
    -pix_fmt yuv420p "$OUT"
elif compgen -G "$DIR/*.ppm" > /dev/null; then
  ffmpeg -y -loglevel error -framerate 2 -pattern_type glob -i "$DIR/*.ppm" \
    -pix_fmt yuv420p "$OUT"
else
  echo "no *.ppm or *-window.png in $DIR" >&2
  exit 1
fi
echo "wrote $OUT"
