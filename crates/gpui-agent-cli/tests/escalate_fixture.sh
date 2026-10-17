#!/bin/sh
# M7 test endpoint for `--escalate-cmd`. Drains the JSON request into
# $ESC_LOG (call tracking), sleeps $ESC_SLEEP seconds if set (timeout
# tests), then replies with the file for the request kind:
# $ESC_REPLY_CHOOSE for "choose", $ESC_REPLY_TEXT for "text".
req="$(cat)"
if [ -n "$ESC_LOG" ]; then
    printf '%s\n' "$req" >> "$ESC_LOG"
fi
if [ -n "$ESC_SLEEP" ]; then
    sleep "$ESC_SLEEP"
fi
case "$req" in
*'"kind":"text"'*)
    cat "$ESC_REPLY_TEXT"
    ;;
*)
    cat "$ESC_REPLY_CHOOSE"
    ;;
esac
