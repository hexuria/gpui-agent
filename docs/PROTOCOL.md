# GPUI Agent Protocol v1

Newline-delimited JSON on a loopback TCP socket. One request object, one
response object **per line**. The client (`AgentClient`) keeps the TCP
connection open and reuses it for later ops; `rpc_once` reconnects for
benchmarks. This is **not** Chrome DevTools Protocol.

Default bind: `127.0.0.1:17421` (`GPUI_AGENT_ADDR`). The server and the
CLI refuse non-loopback addresses. Lines larger than 1 MiB are rejected
and the connection is closed. Trust model and audit: [SECURITY.md](SECURITY.md).

The protocol is **app-agnostic**. Any GPUI Kit app that implements
`AgentHost`, assigns **stable ids**, and starts the server under
`GPUI_AGENT=1` can be driven by `gpui-agent` / MCP with no CLI changes.
See [INTEGRATING.md](INTEGRATING.md).

## Request

```json
{
  "v": 1,
  "id": "1",
  "token": "optional-shared-secret",
  "op": "snapshot"
}
```

`op` is internally tagged. Operations:

| `op` | Fields | Effect |
| --- | --- | --- |
| `hello` | | Protocol, app name, platform, ready, supported `deliveries`, `auth` (`required` \| `none`) |
| `snapshot` | | Semantic UI tree |
| `click` | `target`, optional `delivery` | Activate a widget by stable id |
| `type` | `target`, `text`, optional `delivery` | Append to an editable widget |
| `set_value` | `target`, `value` | Replace editable value (semantic only) |
| `key` | `target`, `key`, optional `delivery` | `Enter`, `Backspace`, … |
| `assert` | `target`, optional `name`/`value`/`role`/`checked`/`exists` | Check snapshot fields |
| `invoke` | `name`, `args` | Named host command **defined by the app** |
| `wait` | optional `timeout_ms` | Block until hello/ready |
| `screenshot` | optional `path` | Observe-only PNG of the **app surface**. Host writes `path` locally (not on the NDJSON line). Headless / Linux / Windows desktop return `screenshot_unavailable` instead of a fake image. macOS desktop `todo` writes **this window** via `screencapture -l` (Screen Recording). Never the full desktop. |
| `shutdown` | | Ask the host to exit |

These are also the **only** first-class `gpui-agent` CLI commands (plus
`mcp` and experimental `recipe`). App-specific verbs are `invoke` names
or click targets — not new subcommands. `recipe` is a **client-side**
batch of the ops above (`AgentClient` reuses one TCP session; `rpc_once`
is the old reconnect path for benches). It is not a new wire `op`.
`recipe run --screenshot-dir` issues extra `screenshot` ops after steps
so an agent can visually check UI state mid-run. Headless stays honest.
macOS desktop writes the app window. Details: [RECORDING.md](RECORDING.md)
and [TRY_ON_MAC.md](TRY_ON_MAC.md).

## Response

```json
{
  "v": 1,
  "id": "1",
  "ok": true,
  "hello": { "protocol": 1, "app": "my-app", "platform": "headless", "ready": true, "deliveries": ["semantic"], "auth": "none" },
  "tree": { "app": "my-app", "platform": "headless", "ready": true, "nodes": [] },
  "result": {},
  "error": null
}
```

Omitted fields are absent, not null.

## Semantic tree

Each node:

```json
{
  "id": "page-settings",
  "role": "window",
  "name": "Settings",
  "value": null,
  "checked": null,
  "enabled": true,
  "focused": false,
  "bounds": { "x": 0, "y": 0, "w": 0, "h": 0 },
  "states": [],
  "children": []
}
```

Prefer **stable ids** over tree indices. The app chooses the scheme
(`nav-settings`, `page-settings`, `row-3`). Numbered suffixes can be
parsed with `gpui_agent::parse_numbered_id`.

`bounds` are logical pixels. Headless hosts send zeros. The desktop todo
host fills them from the last painted frame when `GPUI_AGENT=1`.

## Delivery modes (`click` / `type` / `key`)

`delivery` is optional and defaults to **`semantic`**. Omitted on the wire
so v1 clients stay valid.

| Mode | When to use | What happens |
| --- | --- | --- |
| `semantic` (default) | CI, agents, happy-path automation | Call the same handler the widget uses. No focus steal, no pointer. Fast and deterministic. |
| `virtual` | Bugs that only appear on the real input path (hover, hit-test, focus, press/release, IME) | Resolve the id → bounds from the semantic tree, then synthesize **in-process GPUI** mouse/key events on the UI thread. |

Virtual delivery is **not** OS HID:

- It never warps the real cursor (`XWarpPointer`, `CGWarpMouseCursorPosition`, …).
- It never raises/focuses the OS window as a side effect if GPUI can avoid it.
- The painted **agent cursor** is a Div overlay inside the app window (session-colored). It does not control the OS pointer.

```bash
gpui-agent click todo-add                    # semantic (default)
gpui-agent click --delivery virtual todo-add
gpui-agent type --delivery virtual todo-input "Hi"
gpui-agent key --delivery virtual todo-input Enter
```

```json
{"v":1,"id":"1","op":"click","target":"todo-add","delivery":"virtual"}
```

Hosts that cannot run the GPUI event pipeline (headless, or desktop before
the first paint / zero bounds) return:

```text
virtual_unavailable: …
```

Do not treat that as success. Use `delivery=semantic`, or a painted desktop
window. `hello.deliveries` lists what the host actually implements
(`["semantic"]` on headless; `["semantic","virtual"]` on desktop).
`hello.auth` is `"required"` when the host was started with a
non-empty `GPUI_AGENT_TOKEN`, otherwise `"none"`. Agents can fail
closed from that field. CLI `recipe run` and `mcp` still require a
client token even when `auth` is `"none"` — set the same token on the
host for those workflows.

## Navigation

There is no `goto` / `open-page` op. Agents change screens the same way
a user would:

1. `snapshot` (or already know the nav ids)
2. `click` a nav control (`nav-settings`)
3. `assert` the destination root is present (`page-settings`)

Alternatively, the host can register `invoke` names such as
`nav.go` with `{ "page": "settings" }`. That is an **app** contract,
not part of this protocol.

## Integrating any app

1. Implement `AgentHost` (`hello`, `snapshot`, `dispatch`).
2. Give every actionable widget a **stable id** and include it in the
   snapshot.
3. Feature-gate the bridge; at runtime require `GPUI_AGENT=1`.
4. Desktop: post requests onto a mailbox the UI thread drains.
   Headless: serve a mutex-protected host.
5. Optional: register `invoke` names for high-level work.

Reuse `gpui-agent-cli` unchanged. Details: [INTEGRATING.md](INTEGRATING.md).

## Claude Code / MCP

`gpui-agent mcp` exposes the same generic tools over stdio:

`wait`, `hello`, `snapshot`, `screenshot`, `click`, `type`, `set_value`,
`key`, `assert`, `invoke`, `shutdown`

plus experimental `recipe_validate` / `recipe_plan` / `recipe_run` /
`recipe_resolve` (JSON canonical; client-side batching; see
[RECIPES.md](RECIPES.md)).

Point Claude Code at the binary (`args: ["mcp"]`). Set
`GPUI_AGENT_ADDR` and **`GPUI_AGENT_TOKEN`** (required; same value as
the host). Document your app’s ids and `invoke` names in the project
prompt — do not add per-app MCP tools to this repo.

## Named commands (`invoke`)

`invoke.name` is an **app-defined** string. The protocol does not
reserve a vocabulary. The sample todo host implements:

| Name | Args | Result |
| --- | --- | --- |
| `todo.add` | `{ "title": "…" }` | created item |
| `todo.toggle` | `{ "id": 1 }` | updated item |
| `todo.delete` | `{ "id": 1 }` | deleted item |
| `todo.list` | `{}` | array of items |

Those names are demo-only. A settings app might expose `prefs.set`;
a mail app might expose `mail.archive`. Agents call them with:

```bash
gpui-agent invoke prefs.set --arg theme=dark
```

## Platforms

`platform` is `desktop` | `headless` | `web` | `mobile`. Only the first two
are implemented. New hosts implement `AgentHost` and keep this document.

`screenshot` backends:

| Host | Result |
| --- | --- |
| Headless | `screenshot_unavailable`, no file |
| Desktop Linux / Windows | Same (no production GPUI framebuffer export on this pin) |
| Desktop macOS | PNG of **this window** (`screencapture -l`); permission failure is unavailable, not a fake PNG |

## Extending

- Additive fields may appear on nodes and responses; clients must ignore unknowns.
- A new `op` is a minor bump if old clients can ignore it.
- Changing the meaning of an existing field or removing one is a major bump (`v: 2`).
- Do **not** extend the CLI with app-specific subcommands. New product
  verbs go on the host (`invoke`) or in agent prompts.
- Experimental recipes ([RECIPES.md](RECIPES.md)) batch existing ops on
  the client. Additive only; servers that never heard of recipes still
  speak v1 NDJSON one request at a time. Caps and fail-closed invoke:
  [SECURITY.md](SECURITY.md#recipes-experimental).
