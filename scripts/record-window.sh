#!/usr/bin/env bash
# Observe-only capture of one macOS window (not the full desktop).
# Does not move the mouse, type, or raise privileges beyond Screen Recording.
#
# Usage (repo root, desktop `todo` already running):
#   ./scripts/record-window.sh --out artifacts/recipe-run --title "Agent Todo"
#   # other terminal: gpui-agent recipe run … --record artifacts/recipe-run
#
# Stops when OUT/recording.flag disappears (the CLI removes it) or after
# --seconds. Optional ffmpeg mux to OUT/recipe-run.mp4.
set -euo pipefail

OUT=""
TITLE="Agent Todo"
SECONDS_MAX=90
INTERVAL="0.4"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="${2:-}"; shift 2 ;;
    --title) TITLE="${2:-}"; shift 2 ;;
    --seconds) SECONDS_MAX="${2:-}"; shift 2 ;;
    --interval) INTERVAL="${2:-}"; shift 2 ;;
    -h|--help)
      sed -n '2,14p' "$0"
      exit 0
      ;;
    *)
      echo "unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$OUT" ]]; then
  echo "usage: $0 --out DIR [--title \"Agent Todo\"]" >&2
  exit 2
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "record-window.sh is macOS-only (screencapture -l)." >&2
  echo "On Linux CI use: gpui-agent recipe run … --record DIR  (semantic frames)." >&2
  echo "See docs/RECORDING.md" >&2
  exit 1
fi

mkdir -p "$OUT"
FLAG="$OUT/recording.flag"
if [[ ! -f "$FLAG" ]]; then
  echo "1" > "$FLAG"
fi

# CGWindowID for the first on-screen window whose owner or title matches.
WID="$(osascript - "$TITLE" <<'APPLESCRIPT'
on run argv
  set wanted to item 1 of argv
  tell application "System Events"
    repeat with p in (application processes whose background only is false)
      repeat with w in windows of p
        set nm to name of w
        set owner to name of p
        if nm contains wanted or owner contains wanted or owner contains "todo" then
          try
            return value of attribute "AXWindowNumber" of w as string
          end try
        end if
      end repeat
    end repeat
  end tell
  return ""
end run
APPLESCRIPT
)"

if [[ -z "$WID" ]]; then
  echo "could not find a window matching title/process '$TITLE'." >&2
  echo "Start ./target/debug/todo with GPUI_AGENT=1 first (needs a display)." >&2
  exit 1
fi

echo "capturing window id $WID ($TITLE) → $OUT (observe-only, no HID)" >&2
i=0
start="$(date +%s)"
while [[ -f "$FLAG" ]]; do
  now="$(date +%s)"
  if (( now - start > SECONDS_MAX )); then
    echo "stopping after $SECONDS_MAX seconds" >&2
    break
  fi
  printf -v name "%04d-window.png" "$i"
  # -l: that window only. -x: no shutter sound. -o: no shadow.
  if ! screencapture -l "$WID" -x -o "$OUT/$name" 2>/dev/null; then
    echo "screencapture failed (Screen Recording permission? window closed?)" >&2
    break
  fi
  i=$((i + 1))
  sleep "$INTERVAL"
done

rm -f "$FLAG"
echo "wrote $i PNGs under $OUT" >&2

if command -v ffmpeg >/dev/null 2>&1 && [[ "$i" -gt 0 ]]; then
  ffmpeg -y -loglevel error -framerate 5 -i "$OUT/%04d-window.png" \
    -pix_fmt yuv420p "$OUT/recipe-run.mp4"
  echo "muxed $OUT/recipe-run.mp4" >&2
else
  echo "install ffmpeg to mux PNGs → $OUT/recipe-run.mp4" >&2
  echo "  ffmpeg -y -framerate 5 -i $OUT/%04d-window.png -pix_fmt yuv420p $OUT/recipe-run.mp4" >&2
fi
