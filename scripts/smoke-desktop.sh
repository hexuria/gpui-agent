#!/usr/bin/env bash
# Desktop smoke per ADR-001: todo-headless is the source of truth (SoT).
# Default `todo` is a GPUI daemon *client* and does not listen. CLI CRUD
# talks to the headless daemon, not to a GUI port. The window is optional
# (needs a display / Vulkan ICD). Headless CRUD still passes if the
# window is skipped. Do not use `--features embedded-host` as the only
# path — that flag is widget E2E / Mac screenshot, not product SoT.
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

echo "==> building CLI + todo-headless (SoT)"
cargo build -p gpui-agent-cli -p todo-headless

TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
CLI="$TARGET/debug/gpui-agent"
HOST="$TARGET/debug/todo-headless"
APP="$TARGET/debug/todo"

cleanup() {
  if [[ -n "${APP_PID:-}" ]] && kill -0 "$APP_PID" 2>/dev/null; then
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
  fi
  if [[ -n "${HOST_PID:-}" ]] && kill -0 "$HOST_PID" 2>/dev/null; then
    "$CLI" --addr "$ADDR" shutdown >/dev/null 2>&1 || true
    wait "$HOST_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "==> starting todo-headless on $ADDR (ADR-001 daemon SoT; token required)"
"$HOST" &
HOST_PID=$!

echo "==> wait until the daemon is ready"
"$CLI" --addr "$ADDR" wait
"$CLI" --addr "$ADDR" hello

echo "==> CLI CRUD against todo-headless (not the GUI)"
"$CLI" --addr "$ADDR" set-value todo-input "From the desktop smoke"
"$CLI" --addr "$ADDR" click todo-add
"$CLI" --addr "$ADDR" assert --id todo-item-1 --name "From the desktop smoke" --checked false
"$CLI" --addr "$ADDR" click todo-toggle-1
"$CLI" --addr "$ADDR" assert --id todo-item-1 --checked true
"$CLI" --addr "$ADDR" click todo-delete-1
"$CLI" --addr "$ADDR" assert --id todo-item-1 --absent

echo "==> building todo GUI client (default features, not embedded-host)"
if cargo build -p todo; then
  if [[ "$(uname -s)" == "Darwin" ]] || [[ -n "${DISPLAY:-}" || -n "${WAYLAND_DISPLAY:-}" ]]; then
    echo "==> starting todo GUI as daemon client (does not listen; ADR-001)"
    "$APP" &
    APP_PID=$!
    sleep 1
    if ! kill -0 "$APP_PID" 2>/dev/null; then
      echo "todo GUI exited before it stayed up (need a display + Vulkan ICD)."
      echo "Headless CRUD already passed. Window skipped."
      APP_PID=""
    fi
  else
    echo "==> skipping todo GUI (no DISPLAY/WAYLAND_DISPLAY on this host)"
  fi
else
  echo "todo GUI binary did not link. Headless CRUD already passed. Window skipped."
fi

"$CLI" --addr "$ADDR" shutdown
wait "$HOST_PID" 2>/dev/null || true
HOST_PID=""

echo
echo "desktop smoke ok: ADR-001 daemon SoT driven without CDP"
