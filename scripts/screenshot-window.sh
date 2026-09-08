#!/usr/bin/env bash
# Observe-only one-shot PNG of one macOS window (not the full desktop).
# Does not move the mouse, type, or raise privileges beyond Screen Recording.
#
# Use this when the host returns screenshot_unavailable (no GPUI export yet)
# and you still need a mid-run visual for an agent. Prefer the protocol
# `screenshot` op / `recipe run --screenshot-dir` when the host can write
# the app surface itself.
#
# Usage (repo root, desktop `todo` already running):
#   ./scripts/screenshot-window.sh --out artifacts/steps/manual.png --title "Agent Todo"
set -euo pipefail

OUT=""
TITLE="Agent Todo"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="${2:-}"; shift 2 ;;
    --title) TITLE="${2:-}"; shift 2 ;;
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
  echo "usage: $0 --out FILE.png [--title \"Agent Todo\"]" >&2
  exit 2
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "screenshot-window.sh is macOS-only (screencapture -l)." >&2
  echo "On Linux CI: gpui-agent recipe run … --screenshot-dir DIR" >&2
  echo "Headless hosts return screenshot_unavailable (no fake PNG)." >&2
  echo "See docs/RECORDING.md" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUT")"

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

# -l: that window only. -x: no shutter sound. -o: no shadow. No click / type.
screencapture -l "$WID" -x -o "$OUT"
echo "wrote $OUT (window id $WID)"
