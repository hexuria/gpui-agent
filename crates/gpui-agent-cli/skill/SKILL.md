---
name: gpui-agent
description: Drive a running GPUI or Native Shell (nsh) app on desktop, web or Android with the gpui-agent CLI. Use to click, type, scroll, swipe, read the UI tree or take screenshots of an app started with GPUI_AGENT=1.
---

# gpui-agent

`gpui-agent` talks to an app that started its agent server (`GPUI_AGENT=1`, a
development build). It reads the app's semantic tree and acts on nodes by id.
It does not click pixels on the screen and cannot attach to an app that did
not start the server.

## Connect

```bash
export GPUI_AGENT_ADDR=127.0.0.1:17421     # the host's address
export GPUI_AGENT_TOKEN=...                # the value the host was started with
gpui-agent targets                         # hosts on this machine, adb devices, forwarded ports
gpui-agent doctor                          # address, token, capabilities, answer time
```

- Android: `adb forward tcp:17421 tcp:17421`, then use `127.0.0.1:17421`.
- Several apps running: always pass the address of the one you started. Do not
  rely on `--connect`, which picks whichever matching app is running.
- `no reply from … within 8000 ms` means the app is up but busy. Raise
  `--reply-timeout-ms` (or `GPUI_AGENT_REPLY_TIMEOUT_MS`), or pause what animates.

## The loop

1. `gpui-agent snapshot --refs` prints one line per node with a ref:
   `@e6 button "Pause the market"`.
2. Act on a ref: `gpui-agent press @e6`.
3. Run `snapshot --refs` again and read the result. Refs are renumbered by
   every `snapshot --refs`; never reuse one from before the screen changed.

Use `gpui-agent snapshot` for the full JSON tree (bounds, states, ids) and
`gpui-agent assert` / `wait-until` to check a condition without reading the tree.

## Actions

| Command | Does |
|---|---|
| `press <node>` or `press --x X --y Y` | Tap or click |
| `type <node> <text>`, `set-value <node> <value>`, `paste <node> <text>` | Enter text |
| `key <node> <key>` | Press a key or chord on a node |
| `scroll <node> --dy N` | Scroll a scroller by points |
| `swipe up\|down\|left\|right --target <node>` | Touch swipe (mobile) |
| `long-press <node>`, `pinch --scale S` | Touch gestures (mobile) |
| `hover <node>`, `drag <node> --dx N --dy N` | Pointer (desktop, web) |
| `viewport --width W --height H` | Resize the window (desktop, web) |
| `system back\|home\|rotate\|…` | Platform actions (mobile) |
| `screenshot --inline --out shot.png` | PNG of the app, written on this machine |

Run `gpui-agent <command> --help` for the options of each.

## What a host supports

A host lists what it implements in `hello.features` (`gpui-agent hello`). An
action the host does not list fails with `unsupported_op`; that is a fact about
the platform (no hover on a phone, no swipe on desktop), not an error to retry.
Pick another action.

## Rules

- Read before acting: take a snapshot first, and again after every action that
  changes the screen.
- Prefer node ids or refs to coordinates. Coordinates are in the app's points,
  not screen pixels.
- An action that returns `ok` was delivered; check the next snapshot to see
  what it did.
- `screenshot` without `--inline` writes the file on the host's machine or
  device, which is not this one for Android or a remote host.
