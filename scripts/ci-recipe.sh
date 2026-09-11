#!/usr/bin/env bash
# CI gate for P4: headless todo-headless + `gpui-agent recipe run`, then assert
# the receipt JSON has ok=true and session_reused=true (missing fields fail).
#
# No display, no Vulkan, no screenshot files. Uses the same loopback + token
# policy as P2 (set GPUI_AGENT_TOKEN in the workflow; do not disable it).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ADDR="${GPUI_AGENT_ADDR:-127.0.0.1:17421}"
TOKEN="${GPUI_AGENT_TOKEN:-}"
export GPUI_AGENT=1
export GPUI_AGENT_ADDR="$ADDR"
export GPUI_AGENT_TOKEN="$TOKEN"

if [[ -z "$TOKEN" ]]; then
  echo "ci-recipe.sh requires a non-empty GPUI_AGENT_TOKEN (P2)" >&2
  exit 1
fi

# Workflow and this default are IPv4 loopback. The CLI also refuses
# non-loopback; fail closed here so a mistyped CI env cannot skip that.
if [[ "$ADDR" != 127.* ]]; then
  echo "refusing non-loopback GPUI_AGENT_ADDR=$ADDR" >&2
  exit 1
fi

echo "==> building CLI + headless host"
cargo build -p gpui-agent-cli -p todo-headless

TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
CLI="$TARGET/debug/gpui-agent"
HOST="$TARGET/debug/todo-headless"
RECEIPT="${RECEIPT_OUT:-$TARGET/ci-recipe-receipt.json}"
mkdir -p "$(dirname "$RECEIPT")"

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

echo "==> recipe run examples/recipes/todo-crud.json"
"$CLI" --addr "$ADDR" recipe run examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json \
  --set title="Buy milk" \
  --receipt-out "$RECEIPT"

echo "==> assert receipt"
python3 "$ROOT/scripts/ci_recipe_assert.py" "$RECEIPT"

"$CLI" --addr "$ADDR" shutdown
wait "$HOST_PID" 2>/dev/null || true
HOST_PID=""

echo "ci-recipe ok"
