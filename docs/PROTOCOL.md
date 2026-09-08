# GPUI Agent Protocol v1

Newline-delimited JSON on a loopback TCP socket. One request object, one
response object. This is **not** Chrome DevTools Protocol.

Default bind: `127.0.0.1:17421` (`GPUI_AGENT_ADDR`).

## Request

```json
{
  "v": 1,
  "id": "1",
  "token": "optional-shared-secret",
  "op": "snapshot"
}
```

`op` is internally tagged. Other operations:

| `op` | Fields | Effect |
| --- | --- | --- |
| `hello` | | Protocol, app name, platform, ready |
| `snapshot` | | Semantic UI tree |
| `click` | `target` | Activate a widget by stable id |
| `type` | `target`, `text` | Append to an editable widget |
| `set_value` | `target`, `value` | Replace editable value |
| `key` | `target`, `key` | `Enter`, `Backspace`, … |
| `assert` | `target`, optional `name`/`value`/`role`/`checked`/`exists` | Check snapshot fields |
| `invoke` | `name`, `args` | Named host command |
| `wait` | optional `timeout_ms` | Block until hello/ready |
| `shutdown` | | Ask the host to exit |

## Response

```json
{
  "v": 1,
  "id": "1",
  "ok": true,
  "hello": { "protocol": 1, "app": "todo", "platform": "headless", "ready": true },
  "tree": { "app": "todo", "platform": "headless", "ready": true, "nodes": [] },
  "result": { "id": 1, "title": "Buy milk", "done": false },
  "error": null
}
```

Omitted fields are absent, not null.

## Semantic tree

Each node:

```json
{
  "id": "todo-item-1",
  "role": "listitem",
  "name": "Buy milk",
  "value": null,
  "checked": false,
  "enabled": true,
  "focused": false,
  "bounds": { "x": 0, "y": 0, "w": 0, "h": 0 },
  "states": ["unchecked"],
  "children": []
}
```

Prefer **stable ids** over tree indices:

- `todo-window`, `todo-input`, `todo-add`, `todo-list`, `todo-empty`, `todo-status`
- `todo-item-{id}`, `todo-toggle-{id}`, `todo-delete-{id}`

`bounds` are logical pixels. Headless hosts send zeros; a future desktop
walker can fill them from layout/AccessKit.

## Named commands (todo demo)

| Name | Args | Result |
| --- | --- | --- |
| `todo.add` | `{ "title": "…" }` | created item |
| `todo.toggle` | `{ "id": 1 }` | updated item |
| `todo.delete` | `{ "id": 1 }` | deleted item |
| `todo.list` | `{}` | array of items |

## Platforms

`platform` is `desktop` | `headless` | `web` | `mobile`. Only the first two
are implemented. New hosts implement `AgentHost` and keep this document.

## Extending

- Additive fields may appear on nodes and responses; clients must ignore unknowns.
- A new `op` is a minor bump if old clients can ignore it.
- Changing the meaning of an existing field or removing one is a major bump (`v: 2`).
