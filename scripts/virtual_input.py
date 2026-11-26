#!/usr/bin/env python3
"""Virtual delivery checks against the painted todo app (embedded-host).

Runs the `gpui-agent` CLI against `todo --features embedded-host` on a real
window (GPUI_AGENT_ADDR / GPUI_AGENT_TOKEN, or --addr / --token passed
through) and checks that:

- `hello` lists `virtual` among the deliveries and requires a token;
- a request with the wrong token is refused and changes nothing;
- the recorded bounds are the widget's own: the first checkbox lies inside
  its row, and the `flex_1` text input is wider than the Add button;
- a virtual click on a checkbox toggles it;
- a virtual click on the sidebar toggle hides the sidebar, and the host
  still answers afterwards (it used to panic re-entering the app entity);
- a virtual `type` into the text input changes its value.

Exit code 0 when every check passed; 1 otherwise.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time


class Run:
    def __init__(self, cli, passthrough):
        self.base = [cli] + passthrough
        self.failed = 0

    def cli(self, *args, base=None):
        proc = subprocess.run(
            (base or self.base) + list(args),
            capture_output=True,
            encoding="utf-8",
            errors="replace",
        )
        return proc.returncode, proc.stdout, proc.stderr.strip()

    def json(self, *args):
        code, out, err = self.cli(*args)
        if code != 0:
            return None, err or out.strip()
        try:
            return json.loads(out), ""
        except ValueError as exc:
            return None, f"not JSON: {exc}"

    def check(self, ok, what, detail=""):
        print(("ok    " if ok else "FAIL  ") + what + (f": {detail}" if detail and not ok else ""))
        if not ok:
            self.failed += 1
        return ok

    def nodes(self):
        reply, err = self.json("snapshot")
        if reply is None:
            raise SystemExit(f"snapshot failed: {err}")
        found = {}

        def walk(node):
            found[node["id"]] = node
            for child in node.get("children", []):
                walk(child)

        for node in reply["tree"]["nodes"]:
            walk(node)
        return found

    def wait_for(self, what, predicate, timeout_s=5.0):
        deadline = time.monotonic() + timeout_s
        while True:
            nodes = self.nodes()
            if predicate(nodes):
                return nodes
            if time.monotonic() > deadline:
                print(f"timed out waiting for {what}", file=sys.stderr)
                return nodes
            time.sleep(0.1)


def area(node):
    b = node.get("bounds") or {}
    return b.get("w", 0) * b.get("h", 0)


def inside(inner, outer):
    a, b = inner["bounds"], outer["bounds"]
    return (
        a["x"] >= b["x"]
        and a["y"] >= b["y"]
        and a["x"] + a["w"] <= b["x"] + b["w"]
        and a["y"] + a["h"] <= b["y"] + b["h"]
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--cli", default="gpui-agent", help="gpui-agent binary")
    args, passthrough = parser.parse_known_args()
    run = Run(args.cli, passthrough)

    hello, err = run.json("hello")
    if not run.check(hello is not None, "hello answers", err):
        return 1
    info = hello.get("hello", {})
    run.check("virtual" in info.get("deliveries", []), "hello lists virtual delivery", str(info))
    run.check(info.get("auth") == "required", "hello requires a token", str(info.get("auth")))

    toggle, row = "todo-toggle-1", "todo-item-1"
    nodes = run.wait_for(
        "painted bounds",
        lambda n: toggle in n and area(n[toggle]) > 0 and area(n.get("todo-input", {})) > 0,
    )
    if not run.check(toggle in nodes and area(nodes[toggle]) > 0, f"{toggle} has painted bounds"):
        return 1
    run.check(
        inside(nodes[toggle], nodes[row]),
        f"{toggle} bounds lie inside {row}",
        f"{nodes[toggle]['bounds']} vs {nodes[row]['bounds']}",
    )
    run.check(
        nodes["todo-input"]["bounds"]["w"] > nodes["todo-add"]["bounds"]["w"],
        "todo-input keeps its flex width",
        f"input {nodes['todo-input']['bounds']} add {nodes['todo-add']['bounds']}",
    )
    checked = bool(nodes[toggle].get("checked"))

    # Mailbox + token: a virtual click with the wrong token must not land.
    wrong = []
    skip = False
    for arg in run.base:
        if skip:
            skip = False
        elif arg == "--token":
            skip = True
        elif not arg.startswith("--token="):
            wrong.append(arg)
    code, _, _ = run.cli(
        "--token", "wrong-token", "click", toggle, "--delivery", "virtual", base=wrong
    )
    run.check(code != 0, "virtual click with a wrong token is refused")
    run.check(
        bool(run.nodes()[toggle].get("checked")) == checked,
        "refused click changed nothing",
    )

    reply, err = run.json("click", toggle, "--delivery", "virtual")
    run.check(reply is not None and reply.get("ok"), f"virtual click {toggle} replies ok", err)
    nodes = run.wait_for(
        f"{toggle} to toggle", lambda n: bool(n[toggle].get("checked")) != checked
    )
    run.check(
        bool(nodes[toggle].get("checked")) != checked,
        f"virtual click toggles {toggle}",
        f"checked stayed {checked}",
    )

    nav_visible = nodes.get("todo-nav", {}).get("visible", True)
    reply, err = run.json("click", "nav-toggle-sidebar", "--delivery", "virtual")
    run.check(reply is not None and reply.get("ok"), "virtual click nav-toggle-sidebar replies ok", err)
    hello, err = run.json("hello")
    run.check(hello is not None, "host still answers after the sidebar click", err)
    if hello is None:
        return 1
    nodes = run.wait_for(
        "the sidebar to toggle",
        lambda n: n.get("todo-nav", {}).get("visible", True) != nav_visible,
    )
    run.check(
        nodes.get("todo-nav", {}).get("visible", True) != nav_visible,
        "virtual click toggles the sidebar",
    )

    before = nodes["todo-input"].get("value", "")
    reply, err = run.json("type", "todo-input", "virtual", "--delivery", "virtual")
    run.check(reply is not None and reply.get("ok"), "virtual type replies ok", err)
    nodes = run.wait_for(
        "the input value", lambda n: n["todo-input"].get("value", "") == before + "virtual"
    )
    run.check(
        nodes["todo-input"].get("value", "") == before + "virtual",
        "virtual type changes todo-input",
        repr(nodes["todo-input"].get("value")),
    )

    # A paced virtual type (one key per request, `key_delay_ms`): every key lands after the one before, because the
    # input that already has the focus is not clicked again for each key.
    before = nodes["todo-input"].get("value", "")
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as recipe:
        json.dump({"name": "paced-virtual", "steps": [{"id": "keys", "op": "type", "target": "todo-input",
                                                       "text": "paced 12.5", "delivery": "virtual",
                                                       "key_delay_ms": 20}]}, recipe)
    reply, err = run.json("recipe", "run", recipe.name)
    os.unlink(recipe.name)
    run.check(reply is not None and reply.get("ok"), "paced virtual type replies ok", err)
    nodes = run.wait_for(
        "the paced value", lambda n: n["todo-input"].get("value", "") == before + "paced 12.5"
    )
    run.check(
        nodes["todo-input"].get("value", "") == before + "paced 12.5",
        "paced virtual type keeps every key after the one before",
        repr(nodes["todo-input"].get("value", "")),
    )

    print("virtual input: " + ("all checks passed" if not run.failed else f"{run.failed} failed"))
    return 1 if run.failed else 0


if __name__ == "__main__":
    sys.exit(main())
