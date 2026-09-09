#!/usr/bin/env bash
# End-to-end CRUD against the headless host (no GPU / display required).
#
# This script drives the *sample todo app* with generic protocol ops
# (set-value / click / assert / invoke). There is no `gpui-agent todo`
# command — app-specific verbs are host `invoke` names or click targets.
#
# P2: one-off click/snapshot stay untokened. `recipe run` / `mcp` require
# a token; the recipe phase below exports the same value on host and CLI.
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

echo "==> recipe run / mcp without token fail fast"
if out=$(env -u GPUI_AGENT_TOKEN "$CLI" --addr "$ADDR" recipe run examples/recipes/todo-crud.json --set title="x" 2>&1); then
  echo "expected recipe run without token to fail, got: $out" >&2
  exit 1
fi
echo "$out" | grep -E -i 'GPUI_AGENT_TOKEN|token' >/dev/null

if out=$(env -u GPUI_AGENT_TOKEN "$CLI" --addr "$ADDR" mcp </dev/null 2>&1); then
  echo "expected mcp without token to fail, got: $out" >&2
  exit 1
fi
echo "$out" | grep -E -i 'GPUI_AGENT_TOKEN|token' >/dev/null

echo "==> starting todo-headless on $ADDR (no token; one-off click/snapshot)"
"$HOST" &
HOST_PID=$!

echo "==> wait until ready"
"$CLI" --addr "$ADDR" wait
"$CLI" --addr "$ADDR" hello

echo "==> snapshot (empty)"
"$CLI" --addr "$ADDR" snapshot --pretty

echo "==> virtual delivery is unavailable on headless (honest error)"
if "$CLI" --addr "$ADDR" click --delivery virtual todo-add; then
  echo "expected virtual_unavailable from headless" >&2
  exit 1
fi

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

echo "==> recipe run with matching host + client token"
export GPUI_AGENT_TOKEN=smoke-p2-token
"$HOST" &
HOST_PID=$!
receipt="$("$CLI" --addr "$ADDR" recipe run examples/recipes/todo-crud.json --set title="Buy milk")"
echo "$receipt"
echo "$receipt" | grep -F '"ok": true' >/dev/null
echo "$receipt" | grep -F '"session_reused": true' >/dev/null
"$CLI" --addr "$ADDR" shutdown
HOST_PID=""

echo
echo "smoke ok: create / toggle / delete / assert via generic protocol ops"
echo "smoke ok: recipe run with matching token (ok + session_reused)"
