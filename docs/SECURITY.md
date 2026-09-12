# Security review — GPUI Agent control plane

This document is an audit of the opt-in localhost control plane
(`crates/gpui-agent`, CLI/MCP, sample hosts). It distinguishes **design
limitations** of a local developer tool from **implementation bugs**, and
records the small hardening patches that landed with this review.

Nothing here weakens the opt-in model. Automation stays off unless
`GPUI_AGENT=1`, release binaries still need
`GPUI_AGENT_ALLOW_RELEASE=1`, and a non-loopback bind is **fail closed**
unless the authenticated remote triple is set (see below). Loopback without a token is **not** the default. Bind requires a
non-empty `GPUI_AGENT_TOKEN` unless `GPUI_AGENT_INSECURE_NO_TOKEN=1`.

## Trust model (intentional)

| Gate | Behavior |
| --- | --- |
| Compile | Feature-gate the in-process bridge. This repo’s sample `todo` defaults `embedded-host` **off** (the window is a daemon client). Enable it only for widget E2E. Product apps should keep the equivalent flag **off**. |
| Runtime | `GPUI_AGENT=1` (`true` / `yes` / `on`). |
| Release | Also `GPUI_AGENT_ALLOW_RELEASE=1`. |
| Bind | **Loopback default.** `from_env` calls `authorize_bind`. IPv4 `127.0.0.0/8` and IPv6 `::1` require a non-empty `GPUI_AGENT_TOKEN` unless `GPUI_AGENT_INSECURE_NO_TOKEN=1`. IPv4-mapped loopback (`::ffff:127.0.0.1`) is **rejected**. Non-loopback (including `0.0.0.0` / `::`) requires `GPUI_AGENT_REMOTE=1` **and** a non-empty `GPUI_AGENT_TOKEN`. |
| Token | **Required** to bind a host (`GPUI_AGENT_TOKEN`). `GPUI_AGENT_INSECURE_NO_TOKEN=1` restores untokened loopback and prints a banner. When a token is set, every request must send v2 `auth` HMAC (never the raw token). CLI **`recipe run` and `mcp` require** a non-empty client token even on loopback (P2). One-off `click` / `snapshot` / `hello` use the same token as the host for the HMAC. |
| Remote client | CLI refuses non-loopback `--addr` unless `--allow-remote` / `GPUI_AGENT_ALLOW_REMOTE=1` **and** a non-empty token (M1: do not leak the secret on a mistype). |
| Transport | NDJSON over TCP. **Not HTTP. Not TLS.** Remote bind is plaintext + token — lab / trusted network only. Prefer an SSH or Tailscale hop until TLS / mTLS is specified. |
| Delivery | `semantic` (default) calls widget handlers. `virtual` synthesizes in-process GPUI events. **Never OS HID.** |

**Design limitation (not a bug):** any process that can open the loopback
socket can drive the UI as the user — snapshot field values, click,
`invoke`, `shutdown`. On a single-user developer machine that is the
point. On a shared host it is a cross-user confused deputy. A token
narrows that to “whoever knows the secret,” but the secret is still a
local env var.

There is no sandbox, no origin check, and no encryption on the wire.
Loopback traffic never leaves the machine. Remote bind **does** leave
the machine as plaintext TCP plus a shared secret. Do not enable this
in shipping product builds. Do not put a daemon on the public internet.

## Threat model — daemon on an agent VM (#14)

**Chosen (2026-09-11):** host bind is default-deny (a token is required).
`GPUI_AGENT_INSECURE_NO_TOKEN=1` restores untokened loopback for local
demos and prints a banner. Authenticated remote bind is unchanged.

| Actor | What they can do | Mitigation |
| --- | --- | --- |
| Local process on the host | Drive an untokened loopback daemon only if the operator set `GPUI_AGENT_INSECURE_NO_TOKEN=1` (H1). | Default-deny bind. Set `GPUI_AGENT_TOKEN` on any long-lived host. |
| Agent on another machine | Drive the daemon only if the host bound non-loopback with `GPUI_AGENT=1` + `GPUI_AGENT_REMOTE=1` + token, **and** the client passed `--allow-remote` + the same token. | Fail closed without that triple. Caps (1 MiB / 32 conn / 128 mailbox) still apply. |
| Network observer | Read NDJSON metadata (ops, ids). The raw token is **not** on the v2 wire (HMAC over a per-connection nonce). | Remote bind is still plaintext metadata. Tunnel with SSH or Tailscale. TLS / mTLS / pairing codes are follow-up. |
| Random Internet | Bind `0.0.0.0` without the triple is refused. Accidental public listen without a token is refused. | `authorize_bind` + tests. Still do not advertise a public port. |

`hello.auth` remains `"required"` when the host has a token and `"none"`
otherwise. Remote hosts always have a token, so `hello.auth` is
`"required"`. P2 recipe/MCP token policy is **not** weakened.

**Deferred:** TLS vs SSH vs mutual TLS, certificate story, OpenGrok /
Grok Bot pairing UX, ephemeral minted tokens.

## Findings

| ID | Severity | Kind | Finding | Evidence | Exploit | Fix |
| --- | --- | --- | --- | --- | --- | --- |
| H1 | **High** | Design | Host token used to be optional. With `GPUI_AGENT=1` and no `GPUI_AGENT_TOKEN`, **any local process** can snapshot, click, invoke, and shut down the app. | Former `from_env` treated empty/missing token as `None`. `handle_request` skips auth when `expected_token` is `None`. | Malware, another user on the same box, or a compromised MCP client talks to `127.0.0.1:17421` and drives the UI. | **Patched (D1 default-deny).** `authorize_bind` / `from_env` refuse loopback without a token. `GPUI_AGENT_INSECURE_NO_TOKEN=1` restores the old hole for local demos and prints `INSECURE_NO_TOKEN_BANNER`. Smoke scripts set a token. No mint-at-bind (PR #8 stays closed). |
| H2 | **High** | Bug | Unbounded NDJSON lines + one OS thread per connection. A client could grow a request line without limit (`BufRead::lines`) and/or open unbounded handler threads. | Former `server.rs` `reader.lines()` and `thread::spawn` on every `accept`. | Local process (no token needed if H1) sends a multi-GB line or opens thousands of connections → memory / thread exhaustion of the GUI process. | **Patched.** `read_limited_line` (default 1 MiB), `MAX_CONNECTIONS` (32), idle read/write timeout (30s). Extra clients are dropped. Oversized lines get an error and the socket is closed (no resync). |
| M1 | **Medium** | Bug | CLI/MCP accepted any `SocketAddr`. A mistyped or injected `--addr` / `GPUI_AGENT_ADDR` would send `GPUI_AGENT_TOKEN` off-box. | Former `gpui-agent-cli` parsed `addr: SocketAddr` and connected with no loopback check. | User or wrapper runs `gpui-agent --addr 1.2.3.4:17421 hello` with a token in the env → secret leaves the machine. | **Patched, then extended (#14).** Default: `authorize_client` refuses non-loopback. Remote connect requires `--allow-remote` **and** a non-empty token. Host `authorize_bind` refuses `0.0.0.0` / public addrs without `GPUI_AGENT_REMOTE=1` + token. |
| M2 | **Medium** | Design | TCP loopback has no peer credentials. Any UID on the host can connect. | `TcpListener::bind` on `127.0.0.1`. No Unix socket, no `SO_PEERCRED`. | User B on a shared Linux box automates user A’s app (especially if H1). | **Documented.** Next step: optional `AF_UNIX` socket with `0600` and peer-uid check. Large change; do not add a second transport in this patch. |
| M3 | **Medium** | Design | `snapshot` returns widget `value`s (draft text, later passwords if an app puts them on the tree). A PNG of the window shows the same pixels. | `todo-core` `tree()` includes `todo-input` value. Protocol has no redaction. | Local client (H1) reads whatever the user typed **or** screenshots the window. | **Documented.** Integrators must not put secrets on the semantic tree **or** the painted window. A protocol-level redaction hook is a later feature. |
| M4 | **Medium** | Design | `gpui-agent mcp` is a full confused-deputy: stdio `tools/call` maps 1:1 onto protocol ops, including `invoke` and `shutdown`. | `crates/gpui-agent-cli/src/mcp.rs` | Whoever can write to the MCP process (the IDE/agent) can do anything the socket allows. | **Documented / intended.** Treat the MCP client as equivalent to holding the token. Do not expose this stdio shim on a network. P2: `gpui-agent mcp` **refuses to start** without a non-empty client token. Set the same token on the host. |
| M5 | **Medium** | Bug | Mailbox queue was unbounded. A stalled UI thread + fast TCP clients could grow RAM without bound. | Former `AgentMailbox::push` always `Vec::push`. | Local client floods `wait`/`click` while the UI thread is blocked. | **Patched.** `MAX_MAILBOX_DEPTH` (128); further pushes return `mailbox full` without queueing. |
| L1 | **Low** | Bug | Token compared with `==` (non-constant-time). | Former `dispatch.rs` / mailbox `got == expected`. | Local attacker times responses to recover a short token. Unrealistic vs just reading `/proc/<pid>/environ`. | **Patched.** `tokens_match` XOR-folds both byte strings (`#[inline(never)]`). Still leaks the longer length; acceptable for a local secret. |
| L2 | **Low** | Bug | Auth / version failures kept the connection open, allowing unlimited guesses on one socket. | Former stream loops `continue` after token errors. | Local brute-force of a short token. | **Patched.** `authorize_request` failures write one error and **close**. |
| L3 | **Low** | Bug | `Request` and `AgentConfig` derived `Debug` and would print the token if anyone logged `{:?}`. | `protocol.rs`, `security.rs` | Accidental log / panic formatting leaks the secret. | **Patched.** Custom `Debug` prints `<redacted>`. |
| L4 | **Low** | Bug | Desktop mailbox path authorized the token in the TCP thread, then `delivery=virtual` skipped `handle_request` entirely — so **protocol version was not checked** for virtual click/type/key. | `apps/todo/src/app.rs` `apply_agent` + former `handle_stream_mailbox` | A `v: 99` virtual click still ran if it reached the mailbox. Not a privilege bypass; it would break a future v2 security field. | **Patched.** Mailbox stream calls `authorize_request` (version + token) before `mailbox.wait`. Host path already went through `handle_request`. |
| L5 | **Low** | Design | Token lives in the process environment (`GPUI_AGENT_TOKEN`), visible via `/proc/<pid>/environ` and some `ps` invocations. | `from_env`, clap `env = "GPUI_AGENT_TOKEN"` | Local attacker with the same or root uid reads the secret. Same class as H1/M2. | **Documented.** Unix-socket + file secret (0600) or an ephemeral printed token is the real fix. |
| L6 | **Low** | Leftover | No request-rate limit beyond connection/line/mailbox caps. `Wait.timeout_ms` polls `hello.ready` (R9). serde_json nesting is capped by serde’s recursion limit (~128). | `dispatch.rs` `Op::Wait`, `server.rs` | Slowloris is mitigated by idle timeout; CPU spam of small valid ops is still possible. | Acceptable. Add a simple per-connection QPS cap if this becomes a real host. |
| I1 | **Info** | Positive | `gpui-agent` still has no `unsafe`. Recipe/CLI never map ops onto a shell. `invoke` is an in-process host callback (sample todo: CRUD only). Virtual keys have **no modifiers** (no synthetic ⌘Q on free-form `Op::Key`). Modifier chords go through allow-listed `keybinding` Action ids with `confirm=true` for quit. P3: desktop macOS may exec **`screencapture`** with a host-chosen `-l<CGWindowID>` and a client `path` (same write as `write_png`). Argv is otherwise fixed — not a shell. Tiny `unsafe` lives in `apps/todo` (`objc` `windowNumber` only). | repo-wide `unsafe` grep; `todo-core` `invoke`; `virtual_input.rs` `keystroke_token`; `keybinding.rs` confirm gate; `screenshot.rs` `screencapture_window_argv` | — | Keep `invoke` allow-listed in each app. Never map protocol ops onto a shell. Do not add an env override for the `screencapture` binary. Keep `keystroke_token` modifier-free. |
| I2 | **Info** | Design | Release gate is `cfg!(debug_assertions)`, not `cfg!(feature = …)`. `cargo run` (dev) does not need `GPUI_AGENT_ALLOW_RELEASE`. | `security.rs` | Shipping a **debug** binary with `GPUI_AGENT=1` baked into a wrapper skips the release latch. | Product builds: release profile + feature off + no env. |
| I3 | **Info** | Design | Sample `todo` feature `embedded-host` defaults **off**. Copy-paste of an older `default = ["agent"]` snippet would compile the in-process bridge into a product. Runtime still needs `GPUI_AGENT=1`. | `apps/todo/Cargo.toml` | Developer runs a product with leftover env from a test session. | Templates should default the in-process host **off**. This repo already does. |
| I4 | **Info** | Product | Bind check is IP-literal only (`SocketAddr`). `localhost` as a hostname is not accepted — safer than DNS. CLI and server share `authorize_*`. | `from_env`, `authorize_bind` | — | Keep it this way. |
| I5 | **Info** | Positive | This is not HTTP. A browser `fetch('http://127.0.0.1:17421')` cannot speak NDJSON usefully. DNS rebinding is not in play **until** someone adds an HTTP/WebSocket front. | `server.rs` | — | If HTTP is ever added: Origin allow-list, no `*` CORS, still loopback + token. |

## Surfaces reviewed

### Bind / loopback

`from_env` parses `GPUI_AGENT_ADDR` as `SocketAddr` (no DNS) and calls
`authorize_bind`. Default `127.0.0.1:17421`. `0.0.0.0`, `::`, and
public addresses are refused unless `GPUI_AGENT_REMOTE=1` and a
non-empty token are both set. The CLI uses `authorize_client` so a
token cannot be sent to a remote IP without an explicit allow.

### Auth

`authorize_request` is the single gate (version + HMAC over the session
nonce when the host has a token). Used by `handle_request` and by both
TCP stream handlers **before** mailbox post / host dispatch. The raw
token must not appear on a v2 request; `tokens_match` remains for
byte compares. Failures close the socket. `hello.auth` is `"required"`
when the host has a token and `"none"` otherwise. CLI `recipe run`
and `mcp` require a client token before they connect.

### Request parsing / DoS

Capped line reader, UTF-8 required, connection cap, idle timeout,
mailbox depth cap. Invalid JSON returns `bad json: …` (serde
messages do not echo the line) and **closes** the connection. Blank
lines remain cheap (`continue`). Oversized / non-UTF-8 close the
connection so a
partial line cannot be interpreted as the next request.

### `invoke` / click / set-value / type / key

No path traversal or command injection in the crate. The sample host
maps ids onto in-memory CRUD. Virtual delivery is in-process GPUI only;
`keystroke_token` / `text_keystrokes` reject modifiers and non-ASCII
(except `\n`/`\t`/` `). Headless returns `virtual_unavailable`.
Free-form `key` still rejects `cmd-q`. Modifier chords use `keybinding`.

### `keybinding` / `keybindings`

Allow-listed GPUI **Action** dispatch (PROTOCOL option B). The host
resolves `binding` against its catalog and runs the **same** Action
handler the keymap uses. Never OS HID, never Accessibility injection
into another process, never a shell. Desktop intercepts must
`dispatch_action` only and reply after the handler runs; they must not
mutate the store as a fallback when Action dispatch is a no-op.

| Gate | Behavior |
| --- | --- |
| Catalog | Unknown ids fail (`unknown binding`). Scope mismatch does **not** promote a window-only Action to “global OS” quit. |
| `scope=focused` | Requires the app to be focused. Default: **no** auto-activate (`keybinding_unavailable: app not focused`). Optional `activate: true` is a host GPUI activate of **this** window. |
| `scope=global` | This app’s global map only. Must not require focus and must not activate. If the kit pin cannot dispatch a global Action without faking focus, fail closed (`keybinding_unavailable`). |
| Destructive | Quit / discard / file-submit always need `confirm=true` and appear `dangerous: true` in the list (`keybinding_list_json` serializes `binding_is_dangerous`, so a host that forgot the flag still lists quit/`cmd-q` as dangerous). Token auth is not enough. `app.quit` is gated even if the host forgot the flag. |
| Free-form `key` | Still modifier-free (I1). |

Apps that stay semantic-only (bir) may later ship `invoke app.quit` /
`window.minimize` that call those **same** Action handlers. That is an
app contract, not a gpui-agent C shim.

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
them on one reused TCP session (all-Read waves may pipeline). Design:
[RECIPES.md](RECIPES.md). Laptop verify: [TRY_ON_MAC.md](TRY_ON_MAC.md).

They do **not** add privilege and do **not** bypass PR #3 caps:

| Gate | Recipe path |
| --- | --- |
| Opt-in / bind | Host still needs `GPUI_AGENT=1`. CLI still `authorize_client` (loopback default; remote needs `--allow-remote` + token). |
| Token / version | Every step is a normal `Request`. `authorize_request` still runs. Missing or **wrong** token fails the step; the server still closes. CLI `recipe run` and `mcp` **refuse to start** without a non-empty client token (P2). Host bind is default-deny (`GPUI_AGENT_TOKEN`); `GPUI_AGENT_INSECURE_NO_TOKEN=1` is the only untokened loopback. Set the **same** token on host and client. `hello.auth` advertises `"required"` \| `"none"`. Not a wire `batch` op. |
| Line / conn / mailbox | Unchanged. Extra recipe cap: 256 steps. |
| `invoke` | Names must be `SchemaKind::Invoke` on the local registry. Unknown names and protocol names used as invoke (`click`) fail closed. Schema names are `[A-Za-z0-9_.-]`. |
| Resolve | Keyword score, fail closed. Shell-like / unknown / ambiguous intents do nothing. Never `Command`. |
| Shutdown | `Effect::Exit` requires CLI `--yes` or MCP `yes: true`. The run does not start without it. |
| Delivery | Default `semantic`. `virtual` is still in-process GPUI (never OS HID). `keybinding` is allow-listed Action dispatch (confirm for quit). |

Session reuse is a client convenience (`AgentClient::rpc` keeps the
socket; `rpc_once` reconnects for benches). `rpc_pipeline` writes
several ordinary lines then reads; each line still runs
`authorize_request`. None of these skip auth.
A mid-recipe failure returns a partial receipt and stops the **next
wave**. Write / Exit / mixed waves stay sequential: later siblings do
**not** run. All-Read waves may already have run their siblings (extra
observes only); those appear on the receipt. `--screenshot-dir` stays
sequential so a PNG RPC can land after each step.

Treat `recipe run` / `recipe_run` as equivalent to holding the token
(same class as M4). Tests for the fail-closed cases live in
`gpui-agent-recipe` and the CLI/MCP suite — see
[RECIPES.md](RECIPES.md#edge-case-coverage) and
[RECIPES.md](RECIPES.md#threat-model-recipes-must-not-bypass-caps).

Optional `--screenshot-dir` asks the host for an app-surface PNG after
steps (receipt lists paths). Headless / Linux / Windows return
`screenshot_unavailable` and do not invent a file. macOS
`todo --features embedded-host` writes **this window** via
`screencapture -l` (Screen Recording). The default GUI client does not
host the agent port. Do not put tokens or CI secrets on the painted
window. See [RECORDING.md](RECORDING.md).

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

`gpui-agent` itself depends on `serde`, `serde_json`, `thiserror`, `hmac`, and
`sha2`. The CLI adds `anyhow` + `clap`. P3 screenshot execs the system
`screencapture` binary on macOS only (fixed argv). `cargo audit` at review time reported
**no yanked crates and no vulnerability advisories** on a generated
lockfile. Six *unmaintained* warnings appear in the GPUI Kit /
windowing stack (`bincode`, `instant`, `paste`, `rustls-pemfile`,
`rustybuzz`, `ttf-parser`) — transitive, not introduced by this crate.
There is no `unsafe` in the `gpui-agent` crate. Workspace `unsafe` is
only in `apps/todo` (`macos_window.rs` objc `windowNumber`). The repo does
not currently
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

1. **Required host token (H1 / D1 — done).** `from_env` / `authorize_bind`
   refuse loopback without `GPUI_AGENT_TOKEN`. `GPUI_AGENT_INSECURE_NO_TOKEN=1`
   restores untokened loopback with a banner. CLI `recipe run` and `mcp`
   still require a client token. No ephemeral Jupyter mint.
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
6. **Virtual delivery completeness.** Scroll, drag, modifier chords
   on **free-form** `key`/`type`, IME composition, `set_value` via the
   event path, double-click. Keep the “no OS HID” rule. Allow-listed
   Action-id `keybinding` (focused/global, confirm for quit) shipped in
   [#36](https://github.com/hexuria/gpui-agent/issues/36). Headless
   stays `virtual_unavailable` for pointer/key synthesis.
7. **Stable `error_code` field.** Agents already branch on the
   `virtual_unavailable:` prefix. Promote that to
   `error_code: "virtual_unavailable" | "unauthorized" | "mailbox_full" | …`
   and keep the prose in `error`.
8. **MCP `Content-Length` framing.** The shim is newline JSON. Spec
   hosts (Claude Code, etc.) usually speak MCP-over-stdio with headers.
   Dual-read or migrate; don’t break the current line parser overnight.
9. **CI: `cargo test` + recipe receipt (P4 — done).**
   `.github/workflows/ci.yml` runs protocol-crate tests and
   `scripts/ci-recipe.sh` (receipt `ok` + `session_reused`). **Still
   later:** generate or commit a lockfile and `cargo audit` (fail on
   vulnerability advisories; warn on unmaintained).
10. **Default in-process host off in app templates (I3).** This repo’s
    `apps/todo` already defaults `embedded-host` off. Say so in
    INTEGRATING.md as a copy-paste trap if someone still sees `agent`.
11. **Mailbox + token + virtual integration test.** Needs a GPU/display
    or a fake `Window`. Until then, keep the unit gates
    (`authorize_request` on virtual ops, mailbox overflow).
12. **In-app GPUI offscreen PNG (P3).** Protocol `screenshot` writes a
    real PNG on **macOS embedded-host** (`screencapture -l` of this
    window; [#20](https://github.com/hexuria/gpui-agent/pull/20)).
    Headless / daemon / default GUI client / Linux / Windows stay
    honest. GPUI `render_to_image` is still `test-support` only — do
    not enable it in production without asking. `--record` /
    ScreenCaptureKit crate stay later.
13. **AccessKit auto-export** so apps register fewer ids by hand.
14. **Per-connection QPS cap** if anyone runs this as a long-lived
    host. Line/connection/mailbox caps are enough for v1.
15. **Invoke allow-lists in docs per app.** Sample todo is CRUD-only;
    integrators must not map `invoke` onto a shell. Experimental recipes
    fail closed on unknown invoke names; that is not a substitute for
    a tight host allow-list.

## Recommended (not implemented here)

Unix sockets (M2), snapshot redaction, and `cargo audit` remain the next
security follow-ups. P4 does not mint ephemeral tokens. P3 does not add
ScreenCaptureKit or entitlements. Do not enable the bridge in shipping
product builds; do not add HTTP without an Origin allow-list.
