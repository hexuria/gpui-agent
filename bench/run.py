#!/usr/bin/env python3
"""LLM benchmark arms (A0/A1) for gpui-agent — PRD §5.2-5.4.

Spawns `gpui-agent mcp` over stdio, exposes its tools/list to Claude, and
drives a manual tool-use loop. A fresh todo-headless on a fresh port with
a fresh token runs every attempt. Writes JSONL rows; never edits results.

Requires: ANTHROPIC_API_KEY. Model is pinned to claude-haiku-5-5.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import anthropic
import tomllib

MODEL = "claude-haiku-5-5"
MAX_TOKENS = 16000
CAP_ACTIONS = 20
CAP_MODEL_CALLS = 30
CAP_WALL_S = 120
SHARED_SUFFIX = (
    " Use only this app. If the task is impossible or ambiguous, stop "
    "without changing anything and say why."
)
HERE = Path(__file__).resolve().parent
ROOT = HERE.parent


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def spawn_host(binary: Path, addr: str, token: str) -> subprocess.Popen:
    env = dict(os.environ)
    env.update(
        GPUI_AGENT="1",
        GPUI_AGENT_ADDR=addr,
        GPUI_AGENT_TOKEN=token,
    )
    return subprocess.Popen(
        [str(binary), "serve"],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def mcp_request(proc: subprocess.Popen, method: str, params: dict, req_id: int) -> dict:
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params}).encode() + b"\n")
    proc.stdin.flush()
    line = proc.stdout.readline()
    return json.loads(line)


class Mcp:
    """Line-delimited JSON-RPC to `gpui-agent mcp` over stdio."""

    def __init__(self, binary: Path, addr: str, token: str, args: list[str]):
        env = dict(os.environ)
        env.update(GPUI_AGENT_ADDR=addr, GPUI_AGENT_TOKEN=token)
        self.proc = subprocess.Popen(
            [str(binary), *args],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        self.next_id = 1
        self.ops = 0
        self.bytes_to_model = 0

    def call(self, method: str, params: dict) -> dict:
        self.ops += 1
        resp = mcp_request(self.proc, method, params, self.next_id)
        self.next_id += 1
        return resp

    def tools(self) -> list[dict]:
        resp = self.call("tools/list", {})
        return resp["result"]["tools"]

    def tool_call(self, name: str, arguments: dict) -> str:
        resp = self.call("tools/call", {"name": name, "arguments": arguments})
        text = json.dumps(resp.get("result", resp.get("error", {})))
        self.bytes_to_model += len(text.encode())
        return text

    def close(self):
        try:
            self.proc.terminate()
            self.proc.wait(timeout=3)
        except Exception:
            self.proc.kill()


def seed(client_addr: str, token: str, titles: list[str]) -> None:
    """Seed via the raw NDJSON protocol (invoke todo.add)."""
    for title in titles:
        wire(client_addr, token, {"op": "invoke", "name": "todo.add", "args": {"title": title}})


def wire(addr: str, token: str, op: dict) -> dict:
    """One request/response. v2 hosts send a `challenge` line first; the
    reply is HMAC-SHA256(key=token, msg=nonce) hex — never the raw token."""
    host, port = addr.rsplit(":", 1)
    req = {"v": 2, "id": "bench", **op}
    with socket.create_connection((host, int(port)), timeout=5) as s:
        f = s.makefile("rwb")
        first = json.loads(f.readline())
        if first.get("op") == "challenge":
            nonce = bytes.fromhex(first["nonce"])
            req["auth"] = hmac.new(token.encode(), nonce, hashlib.sha256).hexdigest()
        # No challenge → v1 host: it cannot authenticate us anyway, and
        # PROTOCOL.md forbids putting the raw token on the wire.
        f.write(json.dumps(req).encode() + b"\n")
        f.flush()
        return json.loads(f.readline())


def ground_truth(addr: str, token: str) -> tuple[list[dict], dict]:
    items = wire(addr, token, {"op": "invoke", "name": "todo.list", "args": {}}).get("result", [])
    tree = wire(addr, token, {"op": "snapshot"}).get("tree", {})
    return items, tree


def judge(task: dict, seeded: list[str], items: list[dict], tree: dict, refused_seen: bool) -> tuple[bool, bool]:
    """Returns (passed, wrong_action). Same clauses as the Rust judge."""
    e = task.get("expect", {})
    count = lambda t: sum(1 for i in items if i["title"] == t)
    done_count = lambda t: sum(1 for i in items if i["title"] == t and i.get("done"))
    seed_count = lambda t: seeded.count(t)

    if task["kind"] == "refuse":
        before = sorted((t, False) for t in seeded)
        after = sorted((i["title"], bool(i.get("done"))) for i in items)
        changed = before != after
        return (not changed, changed)

    misses = []
    for t in e.get("added", []):
        if count(t) <= seed_count(t):
            misses.append(f"missing added {t}")
    for t in e.get("absent", []):
        if count(t) != 0:
            misses.append(f"still present {t}")
    for t in e.get("present", []):
        if count(t) == 0:
            misses.append(f"missing {t}")
    for t in e.get("done", []):
        if count(t) == 0 or done_count(t) != count(t):
            misses.append(f"not done {t}")
    for t in e.get("not_done", []):
        if count(t) == 0 or done_count(t) != 0:
            misses.append(f"should exist undone {t}")
    if "total" in e and len(items) != e["total"]:
        misses.append(f"count {len(items)} != {e['total']}")

    nodes = tree.get("nodes", [])
    find = lambda i: next((n for n in nodes if n.get("id") == i), None)
    for i in e.get("visible", []):
        if not (find(i) or {}).get("visible"):
            misses.append(f"{i} not visible")
    for i in e.get("not_visible", []):
        n = find(i)
        if n and n.get("visible"):
            misses.append(f"{i} still visible")
    for i in e.get("checked", []):
        if not (find(i) or {}).get("checked"):
            misses.append(f"{i} not checked")

    # wrong action: state changed outside expect (same multiset logic as Rust).
    expected: dict[tuple[str, bool], int] = {}
    for t in seeded:
        expected[(t, False)] = expected.get((t, False), 0) + 1
    for t in e.get("added", []):
        expected[(t, False)] = expected.get((t, False), 0) + 1
    for t in e.get("absent", []):
        expected = {k: v for k, v in expected.items() if k[0] != t}
    for t in e.get("done", []):
        k = (t, False)
        if expected.get(k):
            expected[k] -= 1
            expected[(t, True)] = expected.get((t, True), 0) + 1
    surplus = 0
    got: dict[tuple[str, bool], int] = {}
    for i in items:
        k = (i["title"], bool(i.get("done")))
        got[k] = got.get(k, 0) + 1
    for k, v in got.items():
        surplus += max(0, v - expected.get(k, 0))
    wrong = surplus > 0
    return (not misses, wrong)


def run_attempt(client: anthropic.Anthropic, task: dict, arm: dict, prices: dict, system: str) -> dict:
    port = free_port()
    token = secrets.token_hex(16)
    addr = f"127.0.0.1:{port}"
    host_bin = Path(arm.get("host_binary", ROOT / "target" / "debug" / "todo-headless"))
    agent_bin = Path(arm["binary_path"])
    host = spawn_host(host_bin, addr, token)
    mcp = None
    try:
        deadline = time.time() + 10
        while True:
            try:
                r = wire(addr, token, {"op": "hello"})
                if r.get("ok"):
                    break
            except OSError:
                if time.time() > deadline:
                    raise
                time.sleep(0.05)
        mcp = Mcp(agent_bin, addr, token, arm["mcp_args"])
        tools = mcp.tools()
        seeded = task.get("seed", [])
        if "seed_range" in task:
            r = task["seed_range"]
            seeded = list(seeded) + [f"{r['prefix']}{n}" for n in range(r["start"], r["end"] + 1)]
        seed(addr, token, seeded)

        claude_tools = [
            {
                "name": t["name"],
                "description": t.get("description", ""),
                "input_schema": t["inputSchema"],
            }
            for t in tools
        ]
        if claude_tools:
            claude_tools[-1]["cache_control"] = {"type": "ephemeral"}
        messages = [{"role": "user", "content": task["goal"] + SHARED_SUFFIX}]
        usage = dict(input_tokens=0, output_tokens=0, cache_read_tokens=0, cache_write_tokens=0)
        refused_seen = False
        cap_hit = None
        started = time.time()
        for _ in range(CAP_MODEL_CALLS):
            if time.time() - started > CAP_WALL_S:
                cap_hit = "wall"
                break
            resp = client.messages.create(
                model=MODEL,
                max_tokens=MAX_TOKENS,
                system=[{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}],
                tools=claude_tools,
                tool_choice={"type": "auto"},
                output_config={"effort": "medium"},
                messages=messages,
            )
            u = resp.usage
            usage["input_tokens"] += getattr(u, "input_tokens", 0) or 0
            usage["output_tokens"] += getattr(u, "output_tokens", 0) or 0
            usage["cache_read_tokens"] += getattr(u, "cache_read_input_tokens", 0) or 0
            usage["cache_write_tokens"] += getattr(u, "cache_creation_input_tokens", 0) or 0
            mcp.bytes_to_model += sum(len(json.dumps(b.model_dump())) for b in resp.content)
            messages.append({"role": "assistant", "content": [b.model_dump() for b in resp.content]})
            tool_uses = [b for b in resp.content if b.type == "tool_use"]
            if not tool_uses:
                text = " ".join(b.text for b in resp.content if b.type == "text")
                if "impossible" in text.lower() or "ambiguous" in text.lower() or "cannot" in text.lower():
                    refused_seen = True
                break
            results = []
            for tu in tool_uses[: CAP_ACTIONS - mcp.ops + 10]:
                out = mcp.tool_call(tu.name, tu.input)
                results.append({"type": "tool_result", "tool_use_id": tu.id, "content": out})
            if mcp.ops > CAP_ACTIONS:
                cap_hit = "actions"
                break
            messages.append({"role": "user", "content": results})
        else:
            cap_hit = "model_calls"
        wall_ms = (time.time() - started) * 1000

        items, tree = ground_truth(addr, token)
        passed, wrong = judge(task, seeded, items, tree, refused_seen)
        if cap_hit:
            passed = False
        p = prices["model"][MODEL]
        usd = (
            usage["input_tokens"] * p["input_per_mtok"]
            + usage["output_tokens"] * p["output_per_mtok"]
            + usage["cache_read_tokens"] * p.get("cache_read_per_mtok", 0)
            + usage["cache_write_tokens"] * p.get("cache_write_per_mtok", 0)
        ) / 1_000_000
        return {
            "pass": passed,
            "wrong_action": wrong,
            "refused": refused_seen,
            "cap_hit": bool(cap_hit),
            "wall_ms": wall_ms,
            "protocol_ops": mcp.ops,
            "bytes_to_model": mcp.bytes_to_model,
            "model_calls": min(usage and CAP_MODEL_CALLS, 0) or None,
            "usd": usd,
            **usage,
        }
    finally:
        if mcp:
            mcp.close()
        host.terminate()
        try:
            host.wait(timeout=3)
        except subprocess.TimeoutExpired:
            host.kill()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", choices=["a0", "a1"], required=True)
    ap.add_argument("--binary", help="path to gpui-agent CLI (defaults per arms.toml)")
    ap.add_argument("--host-binary", default=str(ROOT / "target" / "debug" / "todo-headless"))
    ap.add_argument("--attempts", type=int, default=5)
    ap.add_argument("--only", help="comma-separated task ids")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    arms = tomllib.loads((HERE / "arms.toml").read_text())["arm"]
    arm = dict(arms[args.arm])
    arm["binary_path"] = args.binary or str(ROOT / "target" / "release" / "gpui-agent")
    arm["host_binary"] = args.host_binary
    tasks = tomllib.loads((HERE / "tasks.toml").read_text())["task"]
    if args.only:
        wanted = set(args.only.split(","))
        tasks = [t for t in tasks if t["id"] in wanted]
    prices = tomllib.loads((HERE / "prices.toml").read_text())
    system = (HERE / "prompts" / "system.md").read_text()
    client = anthropic.Anthropic(api_key=os.environ["ANTHROPIC_API_KEY"])

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    git_sha = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip() or "unknown"
    with out.open("a") as f:
        for task in tasks:
            for attempt in range(1, args.attempts + 1):
                try:
                    r = run_attempt(client, task, arm, prices, system)
                except Exception as e:  # noqa: BLE001 - record and continue
                    r = {"pass": False, "wrong_action": False, "refused": False,
                         "cap_hit": False, "detail": f"harness error: {e!r}"}
                row = {
                    "task": task["id"], "arm": args.arm, "attempt": attempt,
                    "goal": task["goal"], "host_os": sys.platform, "git_sha": git_sha,
                    **r,
                }
                f.write(json.dumps(row) + "\n")
                f.flush()
                print(f"{args.arm} {task['id']} #{attempt}: {'pass' if row['pass'] else 'FAIL'}", file=sys.stderr)
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
