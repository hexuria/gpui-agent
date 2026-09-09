#!/usr/bin/env python3
"""Fail unless a recipe receipt JSON has ok=true and session_reused=true.

Used by scripts/ci-recipe.sh. Missing fields fail (do not treat absence as skip).
"""
from __future__ import annotations

import json
import sys


def problems(receipt: object) -> list[str]:
    out: list[str] = []
    if not isinstance(receipt, dict):
        return [f"receipt is {type(receipt).__name__} (want object)"]
    if receipt.get("ok") is not True:
        out.append(f"ok={receipt.get('ok')!r} (want true)")
    if receipt.get("session_reused") is not True:
        out.append(f"session_reused={receipt.get('session_reused')!r} (want true)")
    return out


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: ci_recipe_assert.py RECEIPT.json", file=sys.stderr)
        return 2
    path = argv[1]
    with open(path, encoding="utf-8") as f:
        receipt = json.load(f)
    found = problems(receipt)
    if found:
        json.dump(receipt, sys.stdout, indent=2)
        print()
        print("CI receipt assert failed: " + "; ".join(found), file=sys.stderr)
        return 1
    print("CI receipt assert ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
