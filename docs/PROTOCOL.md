# GPUI Agent Protocol v1

Newline-delimited JSON on a loopback TCP socket. One request object, one
response object. This is **not** Chrome DevTools Protocol.

Default bind: `127.0.0.1:17421` (`GPUI_AGENT_ADDR`).

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
| `hello` | | Protocol, app name, platform, ready |
| `snapshot` | | Semantic UI tree |
| `click` | `target` | Activate a widget by stable id |
| `type` | `target`, `text` | Append to an editable widget |
| `set_value` | `target`, `value` | Replace editable value |
| `key` | `target`, `key` | `Enter`, `Backspace`, … |
| `assert` | `target`, optional `name`/`value`/`role`/`checked`/`exists` | Check snapshot fields |
| `invoke` | `name`, `args` | Named host command **defined by the app** |
| `wait` | optional `timeout_ms` | Block until hello/ready |
| `shutdown` | | Ask the host to exit |

These are also the **only** first-class `gpui-agent` CLI commands (plus
`mcp`). App-specific verbs are `invoke` names or click targets — not new
subcommands.

## Response

```json
{
  "v": 1,
  "id": "1",
  "ok": true,
  "hello": { "protocol": 1, "app": "my-app", "platform": "headless", "ready": true },
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

`bounds` are logical pixels. Headless hosts send zeros; a future desktop
walker can fill them from layout/AccessKit.

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

`wait`, `hello`, `snapshot`, `click`, `type`, `set_value`, `key`,
`assert`, `invoke`, `shutdown`

Point Claude Code at the binary (`args: ["mcp"]`, optional
`GPUI_AGENT_ADDR` / `GPUI_AGENT_TOKEN`). Document your app’s ids and
`invoke` names in the project prompt — do not add per-app MCP tools to
this repo.

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

## Extending

- Additive fields may appear on nodes and responses; clients must ignore unknowns.
- A new `op` is a minor bump if old clients can ignore it.
- Changing the meaning of an existing field or removing one is a major bump (`v: 2`).
- Do **not** extend the CLI with app-specific subcommands. New product
  verbs go on the host (`invoke`) or in agent prompts.
