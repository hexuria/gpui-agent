#!/usr/bin/env python3
"""A3 escalation endpoint (PRD §5.1 / M7.3): Instinct abstains, Haiku
picks strictly from the offered menu — or abstains.

Wire (one JSON object on stdin, one JSON object on stdout):

  choose: {"v":1,"kind":"choose","goal","clause",
           "actions":[{"id","op","role","name"}],
           "history":[{"op","id","effect"}],
           "abstain":{"reason","ranked":[…]}}
  text:   {"v":1,"kind":"text","goal","clause","field":{"id","name"}}

  reply:  {"choice":{"id","op"}} | {"text":"…"} | {"abstain":"reason"}

Off-menu ids, mismatched ops, and extra keys are protocol violations
the caller maps to a hard error; this script therefore only ever emits
a validated menu pick, a field-length text, or `abstain`. Usage is
appended to $GPUI_AGENT_BENCH_DIR/escalate_usage.jsonl when set.
"""

import json
import os
import sys
import time

import anthropic

MODEL = "claude-haiku-5-5"
TEXT_CAP = 256


def emit(reply: dict) -> None:
    print(json.dumps(reply))


def choose(client: anthropic.Anthropic, req: dict) -> dict:
    actions = req.get("actions") or []
    if not actions:
        return {"abstain": "empty menu"}
    listing = "\n".join(
        f"{a['id']}: {a['op']}"
        + (f"  ({a['name']})" if a.get("name") else "")
        for a in actions
    )
    history = "\n".join(
        f"- {h.get('op')} {h.get('id')} → {h.get('effect')}"
        for h in req.get("history") or []
    ) or "(none)"
    abstain = req.get("abstain") or {}
    user = (
        f"Goal: {req.get('goal', '')}\n"
        f"Clause: {req.get('clause', '')}\n\n"
        f"Menu (pick exactly one id, or abstain):\n{listing}\n\n"
        f"Steps already taken:\n{history}\n\n"
        f"The local policy abstained: {abstain.get('reason', 'n/a')}\n"
        f"Reply with the action id and nothing else, or the word 'abstain'."
    )
    resp = client.messages.create(
        model=MODEL,
        max_tokens=256,
        system=(
            "You pick exactly one UI action from a closed menu, or abstain. "
            "Reply with the action id and nothing else, or the word 'abstain'."
        ),
        messages=[{"role": "user", "content": user}],
    )
    log_usage(resp)
    text = "".join(b.text for b in resp.content if b.type == "text").strip()
    by_id = {a["id"]: a for a in actions}
    if text in by_id:
        return {"choice": {"id": text, "op": by_id[text]["op"]}}
    return {"abstain": f"off-menu reply: {text[:80]}"}


def text_for(client: anthropic.Anthropic, req: dict) -> dict:
    field = req.get("field") or {}
    resp = client.messages.create(
        model=MODEL,
        max_tokens=256,
        system=(
            "You supply the text to type into one named UI field for the "
            "stated goal. Reply with only the text to type — a single line, "
            "no quotes, no explanation — or the word 'abstain'."
        ),
        messages=[
            {
                "role": "user",
                "content": (
                    f"Goal: {req.get('goal', '')}\n"
                    f"Clause: {req.get('clause', '')}\n"
                    f"Field: {field.get('name', '')} ({field.get('id', '')})\n"
                    f"Reply with only the text to type, or 'abstain'."
                ),
            }
        ],
    )
    log_usage(resp)
    text = "".join(b.text for b in resp.content if b.type == "text").strip()
    if text and text != "abstain" and "\n" not in text and len(text) <= TEXT_CAP:
        return {"text": text}
    return {"abstain": "no valid text"}


def log_usage(resp) -> None:
    bench_dir = os.environ.get("GPUI_AGENT_BENCH_DIR")
    if not bench_dir:
        return
    usage = getattr(resp, "usage", None)
    try:
        entry = {
            "ts": time.time(),
            "model": MODEL,
            "input_tokens": getattr(usage, "input_tokens", None),
            "output_tokens": getattr(usage, "output_tokens", None),
        }
        with open(os.path.join(bench_dir, "escalate_usage.jsonl"), "a") as fh:
            fh.write(json.dumps(entry) + "\n")
    except OSError:
        pass


def main() -> int:
    req = json.load(sys.stdin)
    client = anthropic.Anthropic()
    kind = req.get("kind")
    if kind == "choose":
        emit(choose(client, req))
    elif kind == "text":
        emit(text_for(client, req))
    else:
        emit({"abstain": f"unknown kind {kind!r}"})
    return 0


if __name__ == "__main__":
    sys.exit(main())
