#!/usr/bin/env python3
"""A3 escalation endpoint (PRD §5.1): Instinct abstains, Haiku picks off-menu.

Reads one JSON object on stdin: {"goal": str, "menu": [{"id": str, "op": str,
"target": str|null, "description": str}, ...]}. Prints exactly one line:
either a menu `id` to execute or the literal `abstain`. Anything else is a
protocol violation and the caller must treat it as abstain.
"""

import json
import sys

import anthropic

MODEL = "claude-haiku-5-5"


def main() -> int:
    req = json.load(sys.stdin)
    menu = req["menu"]
    if not menu:
        print("abstain")
        return 0
    listing = "\n".join(
        f"{m['id']}: {m['op']}" + (f" -> {m['target']}" if m.get("target") else "") +
        (f"  ({m['description']})" if m.get("description") else "")
        for m in menu
    )
    client = anthropic.Anthropic()
    resp = client.messages.create(
        model=MODEL,
        max_tokens=256,
        system=(
            "You pick exactly one UI action from a closed menu, or abstain. "
            "Reply with the action id and nothing else, or the word 'abstain'."
        ),
        messages=[
            {
                "role": "user",
                "content": f"Goal: {req['goal']}\n\nMenu:\n{listing}",
            }
        ],
    )
    text = "".join(b.text for b in resp.content if b.type == "text").strip()
    ids = {m["id"] for m in menu}
    print(text if text in ids else "abstain")
    return 0


if __name__ == "__main__":
    sys.exit(main())
