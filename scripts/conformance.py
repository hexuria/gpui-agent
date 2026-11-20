#!/usr/bin/env python3
"""Conformance checks every gpui-agent host must pass, whatever its platform.

Runs the `gpui-agent` CLI against one host (GPUI_AGENT_ADDR / GPUI_AGENT_TOKEN,
or --addr / --token passed through) and checks that:

- `hello` states the protocol, a platform and a feature list (a missing
  `framework`, which the protocol makes optional, is a warning);
- `snapshot` returns a non-empty tree with unique node ids;
- every capability-gated op the host does NOT list is refused by the host
  with `unsupported_op`. The probes are sent with `--as-written`, so the
  CLI does not refuse them itself; they are skipped on a 0.1 host, whose
  dispatcher does not know the ops;
- the listed ops that are safe to try on any screen work: `scroll` and
  `swipe` on the first usable scroller (then back by what actually moved),
  `hover` on the first usable button, `viewport` to the current size, and
  an inline screenshot that is a complete PNG. A host that does not advertise
  `screenshot_inline` gets a warning and skips screenshot checks. A host that
  declines with `screenshot_unavailable` also gets a warning. Either case is
  a failure with --require-screenshot.

`press` is only tried with --press <node id>, since it changes the app.
Exit code 0 when every check passed; 1 otherwise.
"""

import argparse
import json
import os
import struct
import subprocess
import sys
import tempfile

# Feature -> a probe for it. Each is refused before the host looks at its
# arguments, so none can act on the app.
GATED = {
    "press": ["press", "--x", "1", "--y", "1"],
    "scroll": ["scroll", "--x", "1", "--y", "1", "--dy", "1"],
    "hover": ["hover", "--x", "1", "--y", "1"],
    "drag": ["drag", "--x", "1", "--y", "1", "--dx", "1"],
    "swipe": ["swipe", "up"],
    "long_press": ["long-press", "--x", "1", "--y", "1"],
    "pinch": ["pinch", "--scale", "1.1"],
    "viewport": ["viewport", "--width", "800", "--height", "600"],
    "paste": ["paste", "conformance-no-such-node", "x"],
    "system.back": ["system", "back"],
    "system.home": ["system", "home"],
    "system.rotate": ["system", "rotate", "landscape"],
    "system.keyboard_show": ["system", "keyboard-show"],
    "system.keyboard_hide": ["system", "keyboard-hide"],
    "system.background": ["system", "background"],
    "system.foreground": ["system", "foreground"],
    "system.open_url": ["system", "open-url", "about:blank"],
}


class Run:
    def __init__(self, cli, passthrough):
        self.base = [cli] + passthrough
        self.failed = 0

    def cli(self, *args):
        # The CLI writes UTF-8 whatever the locale of this process.
        proc = subprocess.run(
            self.base + list(args),
            capture_output=True,
            encoding="utf-8",
            errors="replace",
        )
        return proc.returncode, proc.stdout, proc.stderr.strip()

    def json(self, *args):
        code, out, err = self.cli(*args)
        if code != 0:
            return None, err
        try:
            return json.loads(out), ""
        except ValueError as exc:
            return None, f"not JSON: {exc}"

    def nodes(self):
        reply, err = self.json("snapshot")
        return ((reply or {}).get("tree") or {}).get("nodes") or [], err

    def check(self, name, ok, detail=""):
        print(f"{'ok  ' if ok else 'FAIL'}  {name}{': ' + detail if detail else ''}")
        if not ok:
            self.failed += 1
        return ok

    def warn(self, text):
        print(f"warn  {text}")

    def skip(self, name, why):
        print(f"skip  {name}: {why}")


def walk(nodes):
    for node in nodes or []:
        yield node
        yield from walk(node.get("children") or [])


def find(nodes, node_id):
    return next((node for node in walk(nodes) if node["id"] == node_id), None)


def usable(node, size):
    """Enabled, visible, and, where the host gives geometry, on screen."""
    if not node.get("enabled", False) or not node.get("visible", True):
        return False
    bounds = node.get("bounds") or {}
    width, height = bounds.get("w", 0), bounds.get("h", 0)
    if width <= 0 or height <= 0:
        return False
    if size:
        x = bounds.get("x", 0) + width / 2
        y = bounds.get("y", 0) + height / 2
        return 0 <= x <= size[0] and 0 <= y <= size[1]
    return True


def first(nodes, role, size):
    return next(
        (node for node in walk(nodes) if node.get("role") == role and usable(node, size)),
        None,
    )


def marker(scroller):
    """A descendant whose position tells how far the scroller moved."""
    return next(
        (
            node
            for node in walk(scroller.get("children", []))
            if (node.get("bounds") or {}).get("h", 0) > 0
        ),
        None,
    )


def top(nodes, node):
    found = find(nodes, node["id"]) if node else None
    return (found.get("bounds") or {}).get("y") if found else None


def valid_png(path):
    """The signature, a first IHDR chunk with a size, and a final IEND."""
    try:
        with open(path, "rb") as file:
            data = file.read()
    except OSError:
        return False
    if len(data) < 45 or data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        return False
    width, height = struct.unpack(">II", data[16:24])
    return width > 0 and height > 0 and data[-8:-4] == b"IEND"


def number(value):
    """A size as the host stated it: 399.75 stays 399.75."""
    return repr(float(value))


def scroll_probes(run, features, nodes, size):
    scroller = first(nodes, "scroll", size)
    probes = (
        ("scroll", lambda amount: ["scroll", scroller["id"], "--dy", number(amount)]),
        (
            "swipe",
            lambda amount: [
                "swipe",
                "up" if amount > 0 else "down",
                "--target",
                scroller["id"],
                "--distance",
                number(abs(amount)),
            ],
        ),
    )
    for feature, command in probes:
        if feature not in features:
            continue
        if scroller is None:
            run.skip(feature, "no usable scroller on this screen")
            continue
        mark = marker(scroller)
        before = top(nodes, mark)
        code, _, err = run.cli(*command(40))
        run.check(f"{feature} on {scroller['id']}", code == 0, err)
        if code != 0:
            continue
        # Put the content back by what it moved, which at an edge of the
        # list is less than what was asked for, and may be nothing.
        after = top(run.nodes()[0], mark)
        if before is None or after is None:
            run.warn(f"{feature}: could not measure the move; the list may have shifted")
            continue
        moved = before - after
        if abs(moved) < 0.5:
            continue
        restore = ["scroll", scroller["id"], "--dy", number(-moved)]
        if "scroll" not in features:
            restore = command(-moved)
        code, _, err = run.cli(*restore)
        run.check(f"{feature} back on {scroller['id']}", code == 0, err)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--cli", default=os.environ.get("GPUI_AGENT_CLI", "gpui-agent"))
    parser.add_argument("--press", metavar="NODE", help="also press this node")
    parser.add_argument(
        "--require-screenshot",
        action="store_true",
        help="fail when the host declines to capture, headless included",
    )
    args, passthrough = parser.parse_known_args()
    run = Run(args.cli, passthrough)

    reply, err = run.json("hello")
    hello = (reply or {}).get("hello")
    if not run.check("hello answers", hello is not None, err):
        return 1
    run.check("hello.protocol is 2", hello.get("protocol") == 2, str(hello.get("protocol")))
    run.check("hello.platform is stated", bool(hello.get("platform")))
    features = hello.get("features") or []
    current = "screenshot_inline" in features
    if current:
        run.check("hello.features lists screenshot_inline", True, ", ".join(features))
    else:
        run.warn("hello.features does not list screenshot_inline")
    # Optional in the protocol, so its absence is reported and not failed.
    if hello.get("framework"):
        run.check("hello.framework is stated", True, hello["framework"])
    else:
        run.warn("hello.framework is not stated (optional)")
    device = hello.get("device") or {}
    size = None
    if device.get("width") and device.get("height"):
        size = (device["width"], device["height"])

    nodes, err = run.nodes()
    ids = [node["id"] for node in walk(nodes)]
    run.check("snapshot has nodes", bool(ids), err or f"{len(ids)} nodes")
    run.check("node ids are unique", len(ids) == len(set(ids)))

    if current:
        for feature, command in GATED.items():
            if feature in features:
                continue
            code, _, err = run.cli("--as-written", *command)
            run.check(
                f"the host refuses {feature} (not listed)",
                code != 0 and "unsupported_op" in err,
                "the host acted on it" if code == 0 else ("" if "unsupported_op" in err else err),
            )
    else:
        run.skip("refusal of unlisted ops", "a 0.1 host does not know them")

    scroll_probes(run, features, nodes, size)
    if "hover" in features:
        button = first(nodes, "button", size)
        if button is None:
            run.skip("hover", "no usable button on this screen")
        else:
            code, _, err = run.cli("hover", button["id"])
            run.check(f"hover {button['id']}", code == 0, err)
    if "viewport" in features:
        if size:
            code, _, err = run.cli(
                "viewport", "--width", number(size[0]), "--height", number(size[1])
            )
            run.check("viewport to the current size", code == 0, err)
        else:
            run.skip("viewport", "hello.device states no size")
    if args.press:
        code, _, err = run.cli("press", args.press)
        run.check(f"press {args.press}", code == 0, err)

    if current:
        with tempfile.TemporaryDirectory() as scratch:
            path = os.path.join(scratch, "shot.png")
            code, _, err = run.cli("screenshot", "--inline", "--out", path)
            # `screenshot_unavailable` is the documented answer of a host
            # that cannot capture (headless, GPUI desktop on Linux and
            # Windows), and also what a capture that broke returns. Only the
            # caller knows which this host is: --require-screenshot.
            declined = "screenshot_unavailable" in err
            if declined and args.require_screenshot:
                run.check("inline screenshot (required)", False, err)
            elif declined and hello.get("platform") == "headless":
                run.skip("inline screenshot", "a headless host has no surface")
            elif declined:
                run.warn(
                    "inline screenshot: the host declined (screenshot_unavailable); "
                    "pass --require-screenshot if this host should capture"
                )
            else:
                run.check(
                    "inline screenshot is a complete PNG on this machine",
                    code == 0 and valid_png(path),
                    err,
                )
    elif args.require_screenshot:
        run.check(
            "inline screenshot (required)",
            False,
            "hello.features does not list screenshot_inline",
        )
    else:
        run.skip("inline screenshot", "host does not advertise screenshot_inline")

    print(f"\n{'PASS' if run.failed == 0 else f'{run.failed} FAILED'}")
    return 1 if run.failed else 0


if __name__ == "__main__":
    sys.exit(main())
