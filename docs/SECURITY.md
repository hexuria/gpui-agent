# Security review — GPUI Agent control plane

This document is an audit of the opt-in localhost control plane
(`crates/gpui-agent`, CLI/MCP, sample hosts). It distinguishes **design
limitations** of a local developer tool from **implementation bugs**, and
records the small hardening patches that landed with this review.

Nothing here weakens the opt-in / loopback model. Automation stays off
unless `GPUI_AGENT=1`, release binaries still need
`GPUI_AGENT_ALLOW_RELEASE=1`, and the server still refuses a non-loopback
bind.

## Trust model (intentional)

| Gate | Behavior |
| --- | --- |
| Compile | Feature-gate the bridge. The sample `todo` app defaults `agent` **on** (it is a demo). Product apps should default it **off**. |
| Runtime | `GPUI_AGENT=1` (`true` / `yes` / `on`). |
| Release | Also `GPUI_AGENT_ALLOW_RELEASE=1`. |
| Bind | Loopback only. `from_env` / `ensure_loopback` reject anything that is not IPv4 `127.0.0.0/8` or IPv6 `::1`. IPv4-mapped loopback (`::ffff:127.0.0.1`) is **rejected** (fail closed). |
| Token | Optional on the host (`GPUI_AGENT_TOKEN`). When set, every request must carry it. CLI **`recipe run` and `mcp` require** a non-empty client token (`GPUI_AGENT_TOKEN` or `--token`). Set the **same** value on host and client for those workflows. One-off `click` / `snapshot` / `hello` do not require a client token. |
| Transport | NDJSON over TCP. Not HTTP, not TLS, not a remote API. |
| Delivery | `semantic` (default) calls widget handlers. `virtual` synthesizes in-process GPUI events. **Never OS HID.** |

**Design limitation (not a bug):** any process that can open the loopback
socket can drive the UI as the user — snapshot field values, click,
`invoke`, `shutdown`. On a single-user developer machine that is the
point. On a shared host it is a cross-user confused deputy. A token
narrows that to “whoever knows the secret,” but the secret is still a
local env var.

There is no sandbox, no origin check, and no encryption beyond “it never
leaves the machine.” Do not enable this in shipping product builds.

## Findings

| ID | Severity | Kind | Finding | Evidence | Exploit | Fix |
| --- | --- | --- | --- | --- | --- | --- |
| H1 | **High** | Design | Host token is optional. With `GPUI_AGENT=1` and no `GPUI_AGENT_TOKEN`, **any local process** can snapshot, click, invoke, and shut down the app via one-off CLI ops. | `security.rs` `from_env` treats empty/missing token as `None`. `handle_request` skips auth when `expected_token` is `None`. | Malware, another user on the same box, or a compromised MCP client talks to `127.0.0.1:17421` and drives the UI / reads field values. | **Partially mitigated (P2).** CLI `recipe run` and `mcp` refuse to start without a non-empty `GPUI_AGENT_TOKEN` / `--token`, and they send it on every request. Host token stays optional so `./scripts/smoke.sh` click/snapshot still works. Recipe/MCP workflows **must set the same token on host and client**. `hello.auth` is `"required"` \| `"none"`. Ephemeral Jupyter mint is **not** implemented. Integrators: still set `GPUI_AGENT_TOKEN` on the host. |
| H2 | **High** | Bug | Unbounded NDJSON lines + one OS thread per connection. A client could grow a request line without limit (`BufRead::lines`) and/or open unbounded handler threads. | Former `server.rs` `reader.lines()` and `thread::spawn` on every `accept`. | Local process (no token needed if H1) sends a multi-GB line or opens thousands of connections → memory / thread exhaustion of the GUI process. | **Patched.** `read_limited_line` (default 1 MiB), `MAX_CONNECTIONS` (32), idle read/write timeout (30s). Extra clients are dropped. Oversized lines get an error and the socket is closed (no resync). |
| M1 | **Medium** | Bug | CLI/MCP accepted any `SocketAddr`. A mistyped or injected `--addr` / `GPUI_AGENT_ADDR` would send `GPUI_AGENT_TOKEN` off-box. | Former `gpui-agent-cli` parsed `addr: SocketAddr` and connected with no loopback check. Server bind was already loopback-only. | User or wrapper runs `gpui-agent --addr 1.2.3.4:17421 hello` with a token in the env → secret leaves the machine. | **Patched.** CLI calls `ensure_loopback` before connect/MCP. Server bind unchanged. |
| M2 | **Medium** | Design | TCP loopback has no peer credentials. Any UID on the host can connect. | `TcpListener::bind` on `127.0.0.1`. No Unix socket, no `SO_PEERCRED`. | User B on a shared Linux box automates user A’s app (especially if H1). | **Documented.** Next step: optional `AF_UNIX` socket with `0600` and peer-uid check. Large change; do not add a second transport in this patch. |
| M3 | **Medium** | Design | `snapshot` returns widget `value`s (draft text, later passwords if an app puts them on the tree). | `todo-core` `tree()` includes `todo-input` value. Protocol has no redaction. | Local client (H1) reads whatever the user typed. | **Documented.** Integrators must not put secrets on the semantic tree, or must redact `role=password` / similar. A protocol-level redaction hook is a later feature. |
| M4 | **Medium** | Design | `gpui-agent mcp` is a full confused-deputy: stdio `tools/call` maps 1:1 onto protocol ops, including `invoke` and `shutdown`. | `crates/gpui-agent-cli/src/mcp.rs` | Whoever can write to the MCP process (the IDE/agent) can do anything the socket allows. | **Documented / intended.** Treat the MCP client as equivalent to holding the token. Do not expose this stdio shim on a network. P2: `gpui-agent mcp` **refuses to start** without a non-empty client token. Set the same token on the host. |
| M5 | **Medium** | Bug | Mailbox queue was unbounded. A stalled UI thread + fast TCP clients could grow RAM without bound. | Former `AgentMailbox::push` always `Vec::push`. | Local client floods `wait`/`click` while the UI thread is blocked. | **Patched.** `MAX_MAILBOX_DEPTH` (128); further pushes return `mailbox full` without queueing. |
| L1 | **Low** | Bug | Token compared with `==` (non-constant-time). | Former `dispatch.rs` / mailbox `got == expected`. | Local attacker times responses to recover a short token. Unrealistic vs just reading `/proc/<pid>/environ`. | **Patched.** `tokens_match` XOR-folds both byte strings (`#[inline(never)]`). Still leaks the longer length; acceptable for a local secret. |
| L2 | **Low** | Bug | Auth / version failures kept the connection open, allowing unlimited guesses on one socket. | Former stream loops `continue` after token errors. | Local brute-force of a short token. | **Patched.** `authorize_request` failures write one error and **close**. |
| L3 | **Low** | Bug | `Request` and `AgentConfig` derived `Debug` and would print the token if anyone logged `{:?}`. | `protocol.rs`, `security.rs` | Accidental log / panic formatting leaks the secret. | **Patched.** Custom `Debug` prints `<redacted>`. |
| L4 | **Low** | Bug | Desktop mailbox path authorized the token in the TCP thread, then `delivery=virtual` skipped `handle_request` entirely — so **protocol version was not checked** for virtual click/type/key. | `apps/todo/src/app.rs` `apply_agent` + former `handle_stream_mailbox` | A `v: 99` virtual click still ran if it reached the mailbox. Not a privilege bypass; it would break a future v2 security field. | **Patched.** Mailbox stream calls `authorize_request` (version + token) before `mailbox.wait`. Host path already went through `handle_request`. |
| L5 | **Low** | Design | Token lives in the process environment (`GPUI_AGENT_TOKEN`), visible via `/proc/<pid>/environ` and some `ps` invocations. | `from_env`, clap `env = "GPUI_AGENT_TOKEN"` | Local attacker with the same or root uid reads the secret. Same class as H1/M2. | **Documented.** Unix-socket + file secret (0600) or an ephemeral printed token is the real fix. |
| L6 | **Low** | Leftover | No request-rate limit beyond connection/line/mailbox caps. `Wait.timeout_ms` is ignored (hello is immediate). serde_json nesting is capped by serde’s recursion limit (~128). | `dispatch.rs` `Op::Wait`, `server.rs` | Slowloris is mitigated by idle timeout; CPU spam of small valid ops is still possible. | Acceptable for v1. Add a simple per-connection QPS cap if this becomes a real host. |
| I1 | **Info** | Positive | No `unsafe`, no filesystem or `Command` surface in the protocol. `invoke` is an in-process host callback (sample todo: CRUD only). Virtual keys have **no modifiers** (no synthetic ⌘Q). | repo-wide `unsafe` grep; `todo-core` `invoke`; `virtual_input.rs` `keystroke_token` | — | Keep `invoke` allow-listed in each app. Never map protocol ops onto a shell. |
| I2 | **Info** | Design | Release gate is `cfg!(debug_assertions)`, not `cfg!(feature = …)`. `cargo run` (dev) does not need `GPUI_AGENT_ALLOW_RELEASE`. | `security.rs` | Shipping a **debug** binary with `GPUI_AGENT=1` baked into a wrapper skips the release latch. | Product builds: release profile + feature off + no env. |
| I3 | **Info** | Design | Sample `todo` feature `default = ["agent"]`. Copy-paste into a product without turning it off compiles the bridge in. Runtime still needs `GPUI_AGENT=1`. | `apps/todo/Cargo.toml` | Developer runs the product with leftover env from a test session. | Templates should default the feature **off**. |
| I4 | **Info** | Product | Bind check is IP-literal only (`SocketAddr`). `localhost` as a hostname is not accepted — safer than DNS. CLI and server now agree on loopback. | `from_env`, `ensure_loopback` | — | Keep it this way. |
| I5 | **Info** | Positive | This is not HTTP. A browser `fetch('http://127.0.0.1:17421')` cannot speak NDJSON usefully. DNS rebinding is not in play **until** someone adds an HTTP/WebSocket front. | `server.rs` | — | If HTTP is ever added: Origin allow-list, no `*` CORS, still loopback + token. |

## Surfaces reviewed

### Bind / loopback

`from_env` parses `GPUI_AGENT_ADDR` as `SocketAddr` (no DNS) and calls
`ensure_loopback`. Default `127.0.0.1:17421`. `0.0.0.0`, `::`, and
public addresses are refused. The CLI now applies the same check so a
token cannot be sent to a remote IP.

### Auth

`authorize_request` is the single gate (version + optional host token).
Used by `handle_request` and by both TCP stream handlers **before**
mailbox post / host dispatch. Comparison is `tokens_match`. Failures
close the socket. `hello.auth` is `"required"` when the host has a
token and `"none"` otherwise. CLI `recipe run` and `mcp` require a
client token before they connect.

### Request parsing / DoS

Capped line reader, UTF-8 required, connection cap, idle timeout,
mailbox depth cap. Invalid JSON still returns `bad json: …` (serde
messages do not echo the line) and keeps the connection so a stray
blank line is cheap. Oversized / non-UTF-8 close the connection so a
partial line cannot be interpreted as the next request.

### `invoke` / click / set-value / type / key

No path traversal or command injection in the crate. The sample host
maps ids onto in-memory CRUD. Virtual delivery is in-process GPUI only;
`keystroke_token` / `text_keystrokes` reject modifiers and non-ASCII
(except `\n`/`\t`/` `). Headless returns `virtual_unavailable`.

Apps that register `invoke` names are responsible for not exposing a
shell, filesystem, or privileged IPC. Treat `invoke` as **code you
wrote**, not as a sandbox.

### MCP stdio

Same generic ops as the CLI. Stdio lines are now capped at `MAX_LINE_BYTES`.
Framing is still newline JSON, not MCP `Content-Length` (product gap).
The parent process is trusted.

### Recipes (experimental)

`gpui-agent recipe run` and MCP `recipe_run` compile a local **JSON**
recipe (`.wants` also accepted) into ordinary protocol ops and send
them **sequentially** on one reused TCP session. Design:
[RECIPES.md](RECIPES.md). Laptop verify: [TRY_ON_MAC.md](TRY_ON_MAC.md).

They do **not** add privilege and do **not** bypass PR #3 caps:

| Gate | Recipe path |
| --- | --- |
| Opt-in / loopback | Host still needs `GPUI_AGENT=1`. CLI still `ensure_loopback`. |
| Token / version | Every step is a normal `Request`. `authorize_request` still runs. Missing or **wrong** token fails the step; the server still closes. CLI `recipe run` and `mcp` **refuse to start** without a non-empty client token (P2). Host token remains optional so one-off `click`/`snapshot` smoke still works. Set the **same** token on host and client for recipe/MCP. `hello.auth` advertises `"required"` \| `"none"`. Not a wire `batch` op. |
| Line / conn / mailbox | Unchanged. Extra recipe cap: 256 steps. |
| `invoke` | Names must be `SchemaKind::Invoke` on the local registry. Unknown names and protocol names used as invoke (`click`) fail closed. Schema names are `[A-Za-z0-9_.-]`. |
| Resolve | Keyword score, fail closed. Shell-like / unknown / ambiguous intents do nothing. Never `Command`. |
| Shutdown | `Effect::Exit` requires CLI `--yes` or MCP `yes: true`. The run does not start without it. |
| Delivery | Default `semantic`. `virtual` is still in-process GPUI (never OS HID). |

Session reuse is a client convenience (`AgentClient::rpc` keeps the
socket; `rpc_once` reconnects for benches). None of these skip auth.
A mid-recipe failure returns a partial receipt and stops; later
siblings in the same DAG wave do **not** run (P1 is sequential).

Treat `recipe run` / `recipe_run` as equivalent to holding the token
(same class as M4). Tests for the fail-closed cases live in
`gpui-agent-recipe` and the CLI/MCP suite — see
[RECIPES.md](RECIPES.md#edge-case-coverage) and
[RECIPES.md](RECIPES.md#threat-model-recipes-must-not-bypass-caps).

Optional `--screenshot-dir` asks the host for an app-surface PNG after
steps (receipt lists paths). Headless returns `screenshot_unavailable`
and does not invent a file. Do not put tokens or CI secrets on the
painted window. Real desktop PNG is P3.

### Logging of secrets

Startup logs print the bind address, not the token. Responses do not
echo the token. `Debug` for `Request` / `AgentConfig` redacts it.
Do not log raw request lines.

### Races / host state

Headless: `Arc<Mutex<H>>` serializes dispatch. Desktop: TCP thread
posts onto `AgentMailbox`; the UI thread drains in `TodoApp::render`.
The TCP thread does not touch GPUI objects. A poisoned mutex still
`expect`s (one panic fails that connection / the host). Acceptable for
v1.

Shutdown sets an atomic flag after the `shutdown` op; in-flight
connections may finish one more request. Fine.

### Dependencies

`gpui-agent` itself depends only on `serde`, `serde_json`, `thiserror`.
The CLI adds `anyhow` + `clap`. `cargo audit` at review time reported
**no yanked crates and no vulnerability advisories** on a generated
lockfile. Six *unmaintained* warnings appear in the GPUI Kit /
windowing stack (`bincode`, `instant`, `paste`, `rustls-pemfile`,
`rustybuzz`, `ttf-parser`) — transitive, not introduced by this crate.
There is no `unsafe` in this workspace. The repo does not currently
commit `Cargo.lock`; CI should generate one and run `cargo audit`.

## What this patch changed

- Line / connection / idle / mailbox caps (`ServerLimits`, `MAX_LINE_BYTES`,
  `MAX_CONNECTIONS`, `MAX_MAILBOX_DEPTH`).
- Shared `authorize_request` (version + constant-time token) on host
  **and** mailbox streams; disconnect on failure.
- CLI/MCP refuse non-loopback addresses.
- MCP stdin uses the same capped line reader.
- `Debug` redaction for tokens.
- Tests for loopback classification, token compare, debug redaction,
  oversized lines, auth disconnect, connection cap, mailbox overflow,
  virtual-op version gate.

## Ranked product / architecture backlog

Security-relevant items first, then reliability and DX. These are
intentionally **not** half-implemented in this patch.

1. **Required token for recipe/MCP (H1 / P2 — done).** CLI `recipe run`
   and `mcp` require a non-empty `GPUI_AGENT_TOKEN` / `--token`. Host
   token stays optional. No ephemeral Jupyter mint. Remaining H1: a
   host started without a token is still driveable by one-off CLI ops
   and any local process that speaks NDJSON.
2. **Unix-domain socket + peer uid (M2).** Optional
   `GPUI_AGENT_SOCK=~/.gpui-agent.sock` with `0600` and
   `SO_PEERCRED` / equivalent. Stronger than TCP loopback on multi-user
   hosts. Large enough to be its own protocol-transport slice.
3. **Snapshot redaction (M3).** Host hook or well-known roles
   (`password`, `secret`) that strip `value` from `snapshot`. Document
   the convention in PROTOCOL.md.
4. **`hello` advertises auth (done in P2).** `hello.auth: "required" | "none"`
   reflects whether the host has a token configured. Agents can fail
   closed early. CLI `recipe run` / `mcp` still require a client token
   even when `auth` is `"none"` — set the same token on the host anyway.
5. **`wait.timeout_ms` should wait.** Today `Wait` is immediate hello.
   Poll `ready` / first painted frame (desktop bounds non-zero) until
   the budget expires. Fixes a real agent flake.
6. **Virtual delivery completeness.** Scroll, drag, modifier chords,
   IME composition, `set_value` via the event path, double-click. Keep
   the “no OS HID” rule. Headless stays `virtual_unavailable`.
7. **Stable `error_code` field.** Agents already branch on the
   `virtual_unavailable:` prefix. Promote that to
   `error_code: "virtual_unavailable" | "unauthorized" | "mailbox_full" | …`
   and keep the prose in `error`.
8. **MCP `Content-Length` framing.** The shim is newline JSON. Spec
   hosts (Claude Code, etc.) usually speak MCP-over-stdio with headers.
   Dual-read or migrate; don’t break the current line parser overnight.
9. **CI: `cargo test` + `cargo audit`.** Generate or commit a lockfile
   so audits are reproducible. Fail on vulnerability advisories; warn
   on unmaintained.
10. **Default `agent` feature off in app templates (I3).** Keep it on
    for `apps/todo` (this is a lab) but say so in INTEGRATING.md as a
    copy-paste trap.
11. **Mailbox + token + virtual integration test.** Needs a GPU/display
    or a fake `Window`. Until then, keep the unit gates
    (`authorize_request` on virtual ops, mailbox overflow).
12. **In-app GPUI offscreen PNG** so `screenshot` can write real pixels
    on desktop (the op exists; hosts without a surface stay honest).
13. **AccessKit auto-export** so apps register fewer ids by hand.
14. **Per-connection QPS cap** if anyone runs this as a long-lived
    host. Line/connection/mailbox caps are enough for v1.
15. **Invoke allow-lists in docs per app.** Sample todo is CRUD-only;
    integrators must not map `invoke` onto a shell. Experimental recipes
    fail closed on unknown invoke names; that is not a substitute for
    a tight host allow-list.

## Recommended (not implemented here)

Unix sockets (M2), snapshot redaction, and CI audit remain the next
security follow-ups. P2 does not mint ephemeral tokens. Do not enable
the bridge in shipping product builds; do not add HTTP without an
Origin allow-list.
