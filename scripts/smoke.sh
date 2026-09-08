#!/usr/bin/env bash
# End-to-end CRUD against the headless host (no GPU / display required).
#
# This script drives the *sample todo app* with generic protocol ops
# (set-value / click / assert / invoke). There is no `gpui-agent todo`
# command — app-specific verbs are host `invoke` names or click targets.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ADDR="${GPUI_AGENT_ADDR:-127.0.0.1:17421}"
export GPUI_AGENT=1
export GPUI_AGENT_ADDR="$ADDR"

echo "==> building CLI + headless host"
cargo build -p gpui-agent-cli -p todo-headless

CLI="$ROOT/target/debug/gpui-agent"
HOST="$ROOT/target/debug/todo-headless"

cleanup() {
  if [[ -n "${HOST_PID:-}" ]] && kill -0 "$HOST_PID" 2>/dev/null; then
    "$CLI" --addr "$ADDR" shutdown >/dev/null 2>&1 || true
    wait "$HOST_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "==> starting todo-headless on $ADDR"
"$HOST" &
HOST_PID=$!

echo "==> wait until ready"
"$CLI" --addr "$ADDR" wait
"$CLI" --addr "$ADDR" hello

echo "==> snapshot (empty)"
"$CLI" --addr "$ADDR" snapshot --pretty

echo "==> create via widgets (todo demo ids)"
"$CLI" --addr "$ADDR" set-value todo-input "Buy milk"
"$CLI" --addr "$ADDR" click todo-add
"$CLI" --addr "$ADDR" assert --id todo-item-1 --name "Buy milk" --checked false

echo "==> create via invoke (host-registered name, still generic CLI)"
"$CLI" --addr "$ADDR" invoke todo.add --arg title="Write docs"
"$CLI" --addr "$ADDR" assert --id todo-item-2 --name "Write docs"

echo "==> toggle"
"$CLI" --addr "$ADDR" click todo-toggle-1
"$CLI" --addr "$ADDR" assert --id todo-item-1 --checked true
"$CLI" --addr "$ADDR" invoke todo.toggle --arg id=1
"$CLI" --addr "$ADDR" assert --id todo-item-1 --checked false

echo "==> delete"
"$CLI" --addr "$ADDR" click todo-delete-2
"$CLI" --addr "$ADDR" assert --id todo-item-2 --absent
"$CLI" --addr "$ADDR" invoke todo.delete --arg id=1
"$CLI" --addr "$ADDR" assert --id todo-item-1 --absent

echo "==> final list via invoke"
"$CLI" --addr "$ADDR" invoke todo.list

echo "==> shutdown"
"$CLI" --addr "$ADDR" shutdown
HOST_PID=""

echo
echo "smoke ok: create / toggle / delete / assert via generic protocol ops"
