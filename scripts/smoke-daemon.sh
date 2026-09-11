#!/usr/bin/env bash
# Daemon-only smoke: serve → status → snapshot ping → shutdown.
# No GPUI window. Bind policy stays with from_env (loopback default).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ADDR="${GPUI_AGENT_ADDR:-127.0.0.1:18421}"
TOKEN="${GPUI_AGENT_TOKEN:-smoke-daemon-token}"
export GPUI_AGENT=1
export GPUI_AGENT_ADDR="$ADDR"
export GPUI_AGENT_TOKEN="$TOKEN"

echo "==> building CLI + daemon"
cargo build -p gpui-agent-cli -p todo-headless

TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
CLI="$TARGET/debug/gpui-agent"
HOST="$TARGET/debug/todo-headless"

cleanup() {
  if [[ -n "${HOST_PID:-}" ]] && kill -0 "$HOST_PID" 2>/dev/null; then
    "$HOST" shutdown >/dev/null 2>&1 || "$CLI" --addr "$ADDR" shutdown >/dev/null 2>&1 || true
    wait "$HOST_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "==> serve"
"$HOST" serve &
HOST_PID=$!

echo "==> status (daemon hello)"
for _ in $(seq 1 50); do
  if "$HOST" status >/tmp/todo-headless-status.json 2>/dev/null; then
    break
  fi
  sleep 0.1
done
grep -F '"ok":true' /tmp/todo-headless-status.json >/dev/null
grep -F '"app":"todo"' /tmp/todo-headless-status.json >/dev/null

echo "==> snapshot ping via gpui-agent CLI"
"$CLI" --addr "$ADDR" wait
"$CLI" --addr "$ADDR" snapshot --pretty >/dev/null

echo "==> shutdown via daemon verb"
"$HOST" shutdown
wait "$HOST_PID" 2>/dev/null || true
HOST_PID=""

echo "smoke-daemon ok: serve / status / snapshot / shutdown"
