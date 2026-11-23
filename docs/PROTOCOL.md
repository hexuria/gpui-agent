# GPUI Agent Protocol v2

Newline-delimited JSON on a loopback TCP socket. One request object, one
response object **per line**. The client (`AgentClient`) keeps the TCP
connection open and reuses it for later ops; `rpc_once` reconnects for
benchmarks. This is **not** Chrome DevTools Protocol.

Default bind: `127.0.0.1:17421` (`GPUI_AGENT_ADDR`). Loopback is the
default. Non-loopback bind is allowed with `GPUI_AGENT_REMOTE=1` **and**
a non-empty token (not a blanket refuse of every non-loopback address).
The CLI refuses non-loopback `--addr` unless `--allow-remote` /
`GPUI_AGENT_ALLOW_REMOTE=1` and a token. Lines larger than 1 MiB are
rejected and the connection is closed. Trust model and audit:
[SECURITY.md](SECURITY.md).

The protocol is **app-agnostic**. This is not a CDP-like attach: any app
that implements `AgentHost`, assigns **stable ids**, and starts the server
under `GPUI_AGENT=1` can be driven by `gpui-agent` / MCP with no CLI
changes. See [INTEGRATING.md](INTEGRATING.md).

## Challenge (when the host has a token)

Immediately after accept the host writes one NDJSON line and flushes:

```json
{"v":2,"op":"challenge","nonce":"<64 hex chars>"}
```

The nonce is 32 bytes from `/dev/urandom`. The client must **not** send
the raw token. Each request on that connection sends `auth` =
lowercase hex(`HMAC-SHA256(key=token, msg=nonce)`). A new connection
gets a new nonce (replay on a later session fails). Untokened servers
(tests / `GPUI_AGENT_INSECURE_NO_TOKEN=1`) do **not** send a challenge.

If a v2 request includes a non-empty `token` field while the host has a
token, the host rejects with `"token must not be sent on the wire"`
and closes. After a challenge (tokened host), a non-JSON / HTTP line
gets `"bad json: …"` and the connection **closes**; a later valid HMAC
on that socket is not served. Blank lines are ignored. Untokened
servers close on bad JSON the same way.

## Request

```json
{
  "v": 2,
  "id": "1",
  "auth": "<hmac-sha256 hex>",
  "op": "snapshot"
}
```

`op` is internally tagged. Operations:

| `op` | Fields | Effect |
| --- | --- | --- |
| `hello` | | Protocol, app name, platform, ready, supported `deliveries`, `auth` (`required` \| `none`), `features` |
| `snapshot` | optional `since`, `format`, `fingerprints` | Semantic UI tree; `since:<rev>` → diff or `unchanged` (see Revisions); `format:"compact"` → one line per visible node; `fingerprints:true` → `id → fingerprint` map |
| `revision` | | Cheap probe — response `rev` is `AgentHost::revision()` without a tree rebuild |
| `click` | `target`, optional `delivery`, optional `expect_fingerprint` | Activate a widget by stable id; fingerprint gate below |
| `type` | `target`, `text`, optional `delivery`, optional `expect_fingerprint` | Append to an editable widget |
| `set_value` | `target`, `value`, optional `expect_fingerprint` | Replace editable value (semantic only) |
| `key` | `target`, `key`, optional `delivery`, optional `expect_fingerprint` | `Enter`, `Backspace`, … (no modifiers; SECURITY I1) |
| `keybinding` | `binding` (Action id), `scope` (`focused` \| `global`), optional `chord`, `confirm`, `activate` | Fire the GPUI **Action** the keymap would. Never OS HID. |
| `keybindings` | | List `{id, chord, scope, dangerous}`. Alias `keybinding.list`. |
| `assert` | `target`, optional `name`/`value`/`role`/`checked`/`exists`/`visible`/`in_viewport` | Check snapshot fields. `exists` is tree presence. `visible` is host-declared. `in_viewport` is geometry (see below). |
| `invoke` | `name`, `args` | Named host command **defined by the app** |
| `wait` | optional `timeout_ms` | `None`: immediate hello (even if `ready: false`). `Some(ms)`: poll `hello.ready` until true or `wait timed out` (sleep ≤ 10 ms between polls) |
| `wait_until` | `timeout_ms`, same fields as `assert` | Poll `assert` until ok or `wait_until timed out: …`. **Not** a `wait` extension — `wait` stays ready/paint only. |
| `screenshot` | optional `path`, optional `mode` (`viewport` \| `scrolled`, default **viewport**), optional `target`, optional `max_height_px` | Observe-only PNG of the **app surface**. Host writes `path` locally (not on the NDJSON line). Default `mode=viewport` is unchanged: the painted window. `mode=scrolled` requires `target` (stable scroll-view id): the host sets scroll offset, waits for paint, captures tiles, stitches, and restores the original offset (semantic scroll API — **not** OS HID / virtual wheel). Headless / daemon / default GUI client / Linux / Windows return `screenshot_unavailable` instead of a fake image (including scrolled). macOS `todo --features embedded-host` writes **this window** via `screencapture -l` (Screen Recording); scrolled stitches those tiles. Never the full desktop. Offscreen `render_to_image` is out of MVP. |
| `shutdown` | | Ask the host to exit |

These are also the **only** first-class `gpui-agent` CLI commands (plus
`mcp`, `run`, and `recipe`). App-specific verbs are `invoke` names
or click targets — not new subcommands. `recipe` is a **client-side**
batch of the ops above (`AgentClient` reuses one TCP session; `rpc_once`
is the old reconnect path for benches). It is not a new wire `op`.
`recipe run --screenshot-dir` issues extra `screenshot` ops after steps
so an agent can visually check UI state mid-run. Headless stays honest.
macOS **embedded-host** writes the app window. Details:
[RECORDING.md](RECORDING.md).

## Response

```json
{
  "v": 2,
  "id": "1",
  "ok": true,
  "hello": { "protocol": 2, "app": "my-app", "platform": "headless", "ready": true, "deliveries": ["semantic"], "auth": "none" },
  "tree": { "app": "my-app", "platform": "headless", "ready": true, "nodes": [] },
  "result": {},
  "error": null
}
```

Omitted fields are absent, not null.

## Error codes, delivery status and read-back

A failed response keeps its human-readable `error` and adds a stable
`error_code` and a fixed recovery `hint`:

```json
{"v":2,"id":"7","ok":false,"error":"stale_target: `todo-add` changed between snapshot and dispatch","error_code":"stale_target","hint":"take a new snapshot and retry"}
```

| `error_code` | Meaning |
| --- | --- |
| `unauthorized` | Missing or wrong token |
| `bad_request` | Malformed request line (over MCP, also bad tool arguments) |
| `unsupported_version` | Request `v` the host does not speak |
| `unsupported_op` | Op or feature the host does not list |
| `stale_target` / `target_gone` | `expect_fingerprint` mismatch / target absent (nothing dispatched) |
| `not_found` / `duplicate_id` | Target id matches no node / several nodes |
| `assert_failed` | An `assert` did not hold |
| `timeout` | A `wait` / `wait_until` bound ran out |
| `mailbox_full` | The UI-thread mailbox is saturated |
| `screenshot_unavailable`, `virtual_unavailable`, `scroll_unavailable`, `keybinding_unavailable`, `in_viewport_unavailable` | The host cannot do this here |
| `host_error` | Anything else, including the app's own refusal |
| `connect_failed` / `no_reply` | Client side only: no connection / no usable reply |

Both fields are optional and wire protocol stays 2. A host that predates
them sends only `error`; the client classifies that text with the same
table (`gpui_agent::error_code_of`), so callers see one code either way.
A host must refuse an action whose target id is not in the tree before
acting, and say so as ``node `<id>` not found`` (the text
`UiTree::require_id` returns), a `not_found: …` prefix or a
`target_gone: …` prefix; conformance accepts `not_found` or `target_gone`,
both `not_delivered`. Any other refusal text is `host_error` with
`delivery_uncertain`.

The client (not the host) adds `delivery_status` to an op that acts:

| `delivery_status` | Meaning |
| --- | --- |
| `not_delivered` | Not written, or refused before dispatch (auth, version, unsupported op, stale/gone/missing target) |
| `delivery_uncertain` | Written, but no usable reply, or the host failed while dispatching |
| `delivered_unverified` | The host answered `ok` |
| `delivered_verified` | `ok`, and the read-back saw the target change |

Read-back (`AgentClient::act`) snapshots the target before and after
`click` / `type` / `set_value` / `key` / `paste` and a targeted `press` /
`long_press`, and reports it as `after`:
`{exists, value?, checked?, focused?, enabled?, changed}`. When the host
reports the same `rev` after the action, the second snapshot is skipped.
On a [secret](#secret-values) target `after` has no `value`: `changed`
still says whether the value moved.
When the first snapshot has no node with the target id, the action is
not sent: it fails with `not_found` and `not_delivered`.
The CLI and MCP read back by default; `--no-read-back` / `no_read_back:
true` turn it off. A failed CLI command prints
`{"ok":false,"error":…,"error_code":…,"hint":…,"delivery_status":…}` on
stdout as well as `error: …` on stderr (exit code 1); an MCP tool error
carries the same JSON as its text. An MCP call rejected before any
request was made (missing or malformed arguments, an unknown tool, a
recipe that does not compile) has `error_code` `bad_request`. A failed
`recipe_run` carries its receipt instead.

## Revisions (`snapshot since`, `revision`)

Hosts that track state mutations report a monotonically increasing
`rev` on every response (`Response.rev`) and from `AgentHost::revision()`.
`hello.features` then lists `rev` / `snapshot_since` / `snapshot_compact`.
A host with no revision (`rev` omitted) tells the client **do not
cache** — every snapshot may differ even at the same logical state.

```json
{"v":2,"id":"1","op":"snapshot","since":4}
{"v":2,"id":"2","op":"revision"}
{"v":2,"id":"3","op":"snapshot","since":4,"format":"compact","fingerprints":true}
```

The server keeps a per-connection ring of recent snapshots. For
`snapshot since`:

| Holds `rev` | Response |
| --- | --- |
| `since` == current | `result: {"unchanged": true}` — tree is not rebuilt |
| `since` in the ring | `result: {"diff": TreeDiff}` — `added` (full node + `parent` id), `removed` (ids), `changed` (id + only the differing scalar fields). A reparent is `removed` + `added`, never a field edit. `rev` is the new revision |
| `since` evicted / unknown | `tree: <full>` + `result: {"full": true}` — resync |

`revision` returns `rev` only — the cheap "did anything move" probe
(mailbox `wait_until` uses it so an idle tree costs no rebuild).

`format:"compact"` returns `result: {"compact": "<text>"}` instead of
`tree`: one line per **visible** node,
`"<indent><id> <role> \"<name>\"  [<flag>|<flag>]"` with flags
`checked` / `disabled` / `focused` / `value="…"` (truncated;
[secret](#secret-values) nodes never emit `value`). Indentation is two spaces per depth. Compact
responses carry `rev` too — they combine with `since`.

`fingerprints:true` adds `Response.fingerprints`:
`{id → FNV-1a hash of the node's stable fields}`, the value in its
[masked](#secret-values) form. Bounds and focus are excluded — layout wobble does not churn the hash. Used by
`expect_fingerprint` below; hosts that implement it list
`expect_fingerprint` in `hello.features`.

## Target fingerprint gate (`expect_fingerprint`)

`click` / `type` / `set_value` / `key` accept optional
`expect_fingerprint: <u64>` — the value the client read for the target
from `snapshot {fingerprints:true}`. The host recomputes the target's
fingerprint **in the same call, before dispatching** (on the UI thread
for mailbox hosts):

- fingerprint mismatch → `stale_target: <id>` (nothing dispatched)
- target id absent → `target_gone: <id>` (nothing dispatched)

Both are fail-closed: the action is discarded and the client should
re-snapshot. This replaces racy "assert then click" pairs with one
atomic check — an agent cannot click a node that moved, renamed, or
re-rendered since it last observed.

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
  "visible": true,
  "bounds": { "x": 0, "y": 0, "w": 0, "h": 0 },
  "states": [],
  "children": []
}
```

Prefer **stable ids** over tree indices. The app chooses the scheme
(`nav-settings`, `page-settings`, `row-3`). Numbered suffixes can be
parsed with `gpui_agent::parse_numbered_id`.

`visible` is **host-declared** (sidebar collapsed, modal closed, toast
dismissed). Omitted on the wire **deserializes as `true`**
(`#[serde(default = "default_visible")]`) so old snapshots stay valid.
Serializers omit `true` so compact trees look like v2 before this field.
Hosts may also put `"hidden"` in `states` for humans; agents must assert
`visible`, not a states convention.

`exists` / `visible` / `in_viewport` are different predicates:

| Field | Means | Fail closed |
| --- | --- | --- |
| `exists` | Node id is in the snapshot | Missing id / duplicate id |
| `visible` | `UiNode.visible` as the host set it | Host must set the field (default true if omitted) |
| `in_viewport` | Non-empty intersection of node `bounds` with the painted **window clip** (the ancestor `role=window` bounds, same coordinate space) | `in_viewport_unavailable: …` when `platform=headless`, node bounds have no area, or the window clip is zero. Never invent geometry. `in_viewport: false` with zero bounds is still unavailable, not “not in view”. |

Modals: `role: dialog` (or app role) **plus** `visible`. Do not name the
field `open` / `shown`. Sidebar collapsed: keep the node, `visible=false`
(so agents can assert closed-but-known). Toast gone: `exists=false` or
`visible=false` — pick one and document it; sample todo keeps the sidebar
node.

`bounds` are logical pixels in the same space as the window clip.
Headless hosts send zeros. The desktop todo host fills them from the last
painted frame when `GPUI_AGENT=1`. Window node bounds are the painted
clip (`x=0,y=0` + window size), not the OS screen origin. bir / other
apps own snapshot `visible` later — this protocol does not wire bir
overlays.

```bash
gpui-agent assert --id todo-nav --visible
gpui-agent assert --id todo-nav --visible false
gpui-agent wait-until --timeout-ms 1000 --id todo-nav --visible false
gpui-agent assert --id todo-add --in-viewport   # desktop with real bounds
```

```json
{"v":2,"id":"1","op":"wait_until","timeout_ms":1000,"target":"todo-nav","visible":false}
```

### Secret values

A node whose `role` or one of whose `states` is `password` or `secret`
(`gpui_agent::SECRET_MARKERS`) holds a secret. Its `value` never leaves
the host: every snapshot, `since` diff (`added` and `changed`) and
resync tree carries it masked, one `•` per character, so an agent still
sees that typing changed the field and how long it is. Compact lines omit
it. Fingerprints hash the masked value, so a fingerprint computed from the
tree a client received matches the host's, and cannot be used to test
guesses. `assert` / `wait_until` compare the real value; a mismatch says
`expected another value (secret values are not shown)` without echoing
either value. A host marks such a field with role `password` or adds the
`secret` state (an API key in a plain text field). The client masks trees
from hosts that predate this rule as they arrive. When `hello.features` lacks
`secret_redaction`, the client does not request `since` diffs: it asks for a
full snapshot, masks it, and marks the reply `result.full` so consumers replace
their cached tree. It also drops secret-node fingerprints; if a partial reply
has no tree to classify, it drops the entire fingerprint map. This prevents
older hosts' raw-value hashes from being exposed.

`run --goal` verifies a `TYPE` / `SET_VALUE` into a secret field by the
masked value's length, and counts the text it typed there as entered
while the field still holds that many characters.
On hosts without `secret_redaction`, `run --goal` and MCP `act` dispatch
known secret-field actions without `expect_fingerprint`, since the legacy
host's raw-value fingerprint cannot be derived safely from the masked tree.
Other node actions still require their fingerprint.

To type a secret without putting it in the process list or the shell
history, pipe it: `gpui-agent type --stdin <id> < secret.txt` (also
`set-value --stdin`, `paste --stdin`).

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
gpui-agent screenshot --out artifacts/steps/mid.png
gpui-agent screenshot --out tall.png --mode scrolled --target todo-list-scroll
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

## Keybindings (Action ids)

Issue [#36](https://github.com/hexuria/gpui-agent/issues/36) **option B**:
a first-class `keybinding` op fires a GPUI **Action** by stable id (the
same handler the keymap would). Free-form `Op::Key` stays
modifier-free. This is **not** OS HID and not a generic “send any
chord” pipe.

Wire field is `binding` so it does not collide with the RPC correlation
`id`. CLI/MCP `--id` maps to `binding`. List alias: `keybinding.list`.

```json
{"v":2,"id":"1","op":"keybinding","binding":"todo.go_settings","scope":"global"}
{"v":2,"id":"2","op":"keybindings"}
{"v":2,"id":"3","op":"keybinding.list"}
{"v":2,"id":"4","op":"keybinding","binding":"app.quit","scope":"global","confirm":true}
```

```bash
gpui-agent keybindings
gpui-agent keybinding --id todo.go_settings --scope global
gpui-agent keybinding --id todo.focus_input --scope focused
gpui-agent keybinding --id todo.focus_input --scope focused --activate
gpui-agent keybinding --id app.quit --scope global --confirm
```

| Field | Meaning |
| --- | --- |
| `binding` | Stable Action id (`app.quit`, `todo.focus_input`). Preferred over the chord. |
| `chord` | Optional metadata / resolve aid (`cmd-q`). Must match the catalog when set. |
| `scope` | `focused` (this window’s map) or `global` (**this app’s** global map only). |
| `confirm` | Required for destructive Actions (quit, discard, file-submit). |
| `activate` | `scope=focused` only. Default **false**. Host GPUI activate, not HID into another app. |

| `scope` | Preconditions | Failure |
| --- | --- | --- |
| `focused` | App window is focused, or `activate: true` | `keybinding_unavailable: app not focused` |
| `global` | Action is on the global map. Does **not** require focus and must **not** activate. | `unknown binding` / scope mismatch; `keybinding_unavailable` if the host cannot dispatch without faking OS focus |

List result:

```json
{ "keybindings": [
  { "id": "todo.focus_input", "chord": "cmd-n", "scope": "focused", "dangerous": false },
  { "id": "todo.go_settings", "chord": "cmd-shift-s", "scope": "global", "dangerous": false },
  { "id": "app.quit", "chord": "cmd-q", "scope": "global", "dangerous": true }
]}
```

Sample todo: focused `todo.focus_input` (`cmd-n`), global `todo.go_settings`
(`cmd-shift-s`), dangerous `app.quit` (`cmd-q`). Desktop
`embedded-host` binds those GPUI Actions and intercepts fire on the UI
thread: **Action dispatch only**, then the mailbox reply after the
handler runs (or `keybinding_unavailable: Action handler did not run`).
Headless runs the **same Action bodies** (it *is* the handler).
Semantic-only apps (bir) may later add `invoke` shims that call those
same handlers — not in this crate.

`gpui-agent key --delivery virtual … cmd-q` still fails (I1).

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

1. Implement `AgentHost` (`hello`, `snapshot`, `dispatch`, `keybindings`).
2. Give every actionable widget a **stable id** and include it in the
   snapshot.
3. Feature-gate the bridge; at runtime require `GPUI_AGENT=1`.
4. Desktop: post requests onto a mailbox the UI thread drains.
   Headless: serve a mutex-protected host.
5. Optional: register `invoke` names for high-level work.

Reuse `gpui-agent-cli` unchanged. Details: [INTEGRATING.md](INTEGRATING.md).

## Claude Code / MCP

`gpui-agent mcp` exposes the same generic tools over stdio:

`wait`, `wait_until`, `hello`, `snapshot`, `screenshot`, `click`, `type`, `set_value`,
`key`, `keybinding`, `keybindings`, `assert`, `invoke`, `shutdown`

plus `recipe_validate` / `recipe_plan` / `recipe_run` and the
experimental `recipe_resolve` (JSON canonical; client-side batching; see
[RECIPES.md](RECIPES.md)), and — with the `instinct` build feature, on
by default — `locate` (ranked target resolution, abstains instead of
guessing), `act` (resolve → fingerprint-gated dispatch → re-observe →
verify), and `snapshot` `since` / `compact` parameters that mirror the
wire fields above.

Point Claude Code at the binary (`args: ["mcp"]` or
`["mcp", "--schema", "examples/schemas/todo.json"]`). Set
`GPUI_AGENT_ADDR` and **`GPUI_AGENT_TOKEN`** (required; same value as
the host). `GPUI_AGENT_SCHEMA` is still honored. Document your app’s ids and `invoke` names in the project
prompt — do not add per-app MCP tools to this repo.

## Pointer, touch and system ops (feature-gated)

These ops were added after the first protocol 2 release. The wire version is
still 2: a host lists each one it implements in `hello.features`, and answers
any it does not list with `unsupported_op: …` before the op reaches the app.

| Op | Fields | Feature |
|---|---|---|
| `press` | `target` or `x`,`y`; optional `expect_fingerprint` | `press` |
| `scroll` | `target` or `x`,`y`; `dx`,`dy` in logical points | `scroll` |
| `hover` | `target` or `x`,`y` | `hover` |
| `drag` | `from`,`to`, or `target` / `x`,`y` with `dx`,`dy` | `drag` |
| `swipe` | `direction` (`up`/`down`/`left`/`right`); optional `target`, `distance` | `swipe` |
| `long_press` | `target` or `x`,`y`; optional `duration_ms` | `long_press` |
| `pinch` | `scale`; optional `target` | `pinch` |
| `viewport` | `width`,`height` or `preset` | `viewport` |
| `paste` | `target`, `text` | `paste` |
| `system` | `action`; `value` for `rotate` and `open_url` | `system.<action>` |

`system` actions: `back`, `home`, `rotate`, `keyboard_show`, `keyboard_hide`,
`background`, `foreground`, `open_url`.

A pointer or touch op must be refused when its target is hidden, disabled,
outside the viewport or covered by another node; a host never moves the press
to a different node to make it succeed.

Hosts built against gpui-agent 0.1 (the nsh adapter) exposed `press`,
`scroll`, `hover`, `drag` and `viewport` as `invoke` names. `AgentClient::gated`
sends that spelling to such a host (one that lists neither the feature nor
`screenshot_inline`), so a new client still drives them. A current host that
omits a feature is never sent the op.

`hello` also carries optional `framework` (`gpui`, `nsh`) and `device`
(`model`, `form_factor`, `scale`, `width`, `height`, `orientation`,
`safe_area`, `input`). A host fills what it knows and omits the rest.

### Screenshots over the connection

`screenshot` with `inline: true` (feature `screenshot_inline`) is for a host
whose files the client cannot read: a device or a browser. `path` may be
omitted. The response's `result.blob` is `{blob, bytes, chunks}`; the client
reads each chunk with `{"op":"blob","blob":"…","index":N}`, which returns
`{data, index, last}` with `data` in base64. Each chunk fits the 1 MiB line.
The host drops the blob after its last chunk and holds at most four.

## Named commands (`invoke`)

`invoke.name` is an **app-defined** string. The protocol does not
reserve a vocabulary. The sample todo host implements:

| Name | Args | Result |
| --- | --- | --- |
| `todo.add` | `{ "title": "…" }` | created item |
| `todo.toggle` | `{ "id": 1 }` | updated item |
| `todo.delete` | `{ "id": 1 }` | deleted item |
| `todo.list` | `{}` | array of items |
| `todo.toggle_sidebar` | `{}` | `{ "sidebar_open": bool }` — demo of `visible` on `todo-nav` |

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
| Desktop macOS (embedded-host) | PNG of **this window** (`screencapture -l`); `mode=scrolled` stitches tiles of a named scroller then restores offset. Permission failure is unavailable, not a fake PNG |
| Desktop macOS (default GUI client) | `screenshot_unavailable` — GUI does not host the agent port |

## Discovery

The host picks its own listen port. On bind it writes a JSON record (address, pid, `hello.app`, executable basename, bundle id when it is inside a macOS `.app` or `GPUI_AGENT_CONNECT_ID` is set) under `$XDG_RUNTIME_DIR/gpui-agent/hosts/` (or a per-user temp directory). `hello` may also include optional `bundle_id` and `executable`.

`gpui-agent --connect <bundle id or executable>` reads those records. `--connect last` reuses that choice; a command with neither `--addr` nor `--connect` goes to the default address, because the saved choice is per user and another agent may have made it. The CLI names the app it is about to act on, on stderr. `--addr` / `GPUI_AGENT_ADDR` wins. Records for dead processes, or for a port that no longer accepts TCP, are removed.

## Extending

- Additive fields may appear on nodes and responses; clients must ignore unknowns.
- A new `op` is a minor bump if old clients can ignore it.
- Changing the meaning of an existing field or removing one is a major bump (`v: 2`).
- Do **not** extend the CLI with app-specific subcommands. New product
  verbs go on the host (`invoke`) or in agent prompts.
- Experimental recipes ([RECIPES.md](RECIPES.md)) batch existing ops on
  the client. Additive only; servers that never heard of recipes still
  speak v1 NDJSON one request at a time. Caps and fail-closed invoke:
  [SECURITY.md](SECURITY.md#recipes).
