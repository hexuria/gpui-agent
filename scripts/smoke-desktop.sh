#!/usr/bin/env bash
# CRUD against the GPUI Kit 0.6 window. Needs a display (or Xvfb) and a
# Vulkan ICD. On a GPU-less VM, Mesa lavapipe is enough:
#   sudo apt-get install -y mesa-vulkan-drivers
#   export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
#
# Same generic CLI as the headless smoke — todo ids are the demo app's,
# not part of gpui-agent.
#
# Host bind is default-deny: set GPUI_AGENT_TOKEN (same value on CLI).
# Recipe / MCP workflows already required that token; see scripts/smoke.sh
# and docs/TRY_ON_MAC.md.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ADDR="${GPUI_AGENT_ADDR:-127.0.0.1:17421}"
TOKEN="${GPUI_AGENT_TOKEN:-smoke-desktop-token}"
export GPUI_AGENT=1
export GPUI_AGENT_ADDR="$ADDR"
export GPUI_AGENT_TOKEN="$TOKEN"

echo "==> building CLI + GPUI todo"
cargo build -p gpui-agent-cli -p todo

CLI="$ROOT/target/debug/gpui-agent"
APP="$ROOT/target/debug/todo"

cleanup() {
  if [[ -n "${APP_PID:-}" ]] && kill -0 "$APP_PID" 2>/dev/null; then
    "$CLI" --addr "$ADDR" shutdown >/dev/null 2>&1 || true
    wait "$APP_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "==> starting todo (GPUI Kit 0.6) on $ADDR"
"$APP" &
APP_PID=$!
sleep 1
if ! kill -0 "$APP_PID" 2>/dev/null; then
  echo "todo exited before it was ready. Need a display + Vulkan ICD."
  echo "See README: Mesa lavapipe on Xvfb, or a real GPU."
  exit 1
fi

echo "==> wait until ready"
"$CLI" --addr "$ADDR" wait
"$CLI" --addr "$ADDR" set-value todo-input "From the desktop window"
"$CLI" --addr "$ADDR" click todo-add
"$CLI" --addr "$ADDR" assert --id todo-item-1 --name "From the desktop window" --checked false
"$CLI" --addr "$ADDR" click todo-toggle-1
"$CLI" --addr "$ADDR" assert --id todo-item-1 --checked true
"$CLI" --addr "$ADDR" click todo-delete-1
"$CLI" --addr "$ADDR" assert --id todo-item-1 --absent
"$CLI" --addr "$ADDR" shutdown
APP_PID=""

echo
echo "desktop smoke ok: GPUI Kit window driven without CDP"
