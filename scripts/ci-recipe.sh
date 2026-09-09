#!/usr/bin/env bash
# CI gate for P4: headless todo-headless + `gpui-agent recipe run`, then assert
# the receipt JSON is ok (and session_reused when that field is present).
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

CLI="$ROOT/target/debug/gpui-agent"
HOST="$ROOT/target/debug/todo-headless"
RECEIPT="${RECEIPT_OUT:-$ROOT/target/ci-recipe-receipt.json}"
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
  --set title="Buy milk" \
  --receipt-out "$RECEIPT"

echo "==> assert receipt"
python3 - "$RECEIPT" <<'PY'
import json, sys

path = sys.argv[1]
with open(path, encoding="utf-8") as f:
    receipt = json.load(f)

problems = []
if receipt.get("ok") is not True:
    problems.append(f"ok={receipt.get('ok')!r} (want true)")
if "session_reused" in receipt and receipt.get("session_reused") is not True:
    problems.append(f"session_reused={receipt.get('session_reused')!r} (want true)")

if problems:
    json.dump(receipt, sys.stdout, indent=2)
    print()
    print("CI receipt assert failed: " + "; ".join(problems), file=sys.stderr)
    sys.exit(1)

print("CI receipt assert ok")
PY

"$CLI" --addr "$ADDR" shutdown
wait "$HOST_PID" 2>/dev/null || true
HOST_PID=""

echo "ci-recipe ok"
