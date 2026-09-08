#!/usr/bin/env bash
# Demo-only helpers for apps/todo. Not part of the gpui-agent CLI.
#
# These wrap generic `invoke` calls the sample host registered
# (`todo.add`, `todo.toggle`, …). Other apps should register their own
# names — or skip invoke and drive the UI with click / set-value / assert.
#
# Usage (with the headless host already running):
#   examples/todo.sh add "Buy milk"
#   examples/todo.sh toggle 1
#   examples/todo.sh delete 1
#   examples/todo.sh list
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CLI="${GPUI_AGENT_BIN:-$ROOT/target/debug/gpui-agent}"
if [[ ! -x "$CLI" ]]; then
  CLI="${GPUI_AGENT_BIN:-gpui-agent}"
fi

ADDR="${GPUI_AGENT_ADDR:-127.0.0.1:17421}"
cmd="${1:-}"
shift || true

case "$cmd" in
  add)
    title="${1:-}"
    if [[ -z "$title" ]]; then
      echo "usage: $0 add TITLE" >&2
      exit 2
    fi
    exec "$CLI" --addr "$ADDR" invoke todo.add --arg "title=$title"
    ;;
  toggle)
    id="${1:-}"
    if [[ -z "$id" ]]; then
      echo "usage: $0 toggle ID" >&2
      exit 2
    fi
    exec "$CLI" --addr "$ADDR" invoke todo.toggle --arg "id=$id"
    ;;
  delete)
    id="${1:-}"
    if [[ -z "$id" ]]; then
      echo "usage: $0 delete ID" >&2
      exit 2
    fi
    exec "$CLI" --addr "$ADDR" invoke todo.delete --arg "id=$id"
    ;;
  list)
    exec "$CLI" --addr "$ADDR" invoke todo.list
    ;;
  *)
    echo "usage: $0 {add TITLE|toggle ID|delete ID|list}" >&2
    echo "These helpers are for the sample todo app only." >&2
    exit 2
    ;;
esac
