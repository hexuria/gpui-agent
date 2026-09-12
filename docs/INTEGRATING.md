# Integrating gpui-agent into another GPUI Kit app

The CLI does **not** attach to an arbitrary GPUI Kit process. You embed
`AgentHost`, assign stable ids, and start the server under `GPUI_AGENT=1`.

`gpui-agent` is a **generic** control plane. Your app supplies the
semantic tree and action handlers; the CLI/MCP never learn your domain.
Cookbook and `TestHost`: [SDK.md](SDK.md). Sync model: [ADR-001](ADR-001-daemon-sot.md).

## 1. Opt in

| Gate | What you do |
| --- | --- |
| Compile | Feature-gate the in-process bridge. This repo’s sample uses `embedded-host` (default **off**). Other apps often name the flag `agent`. Default it **off** in product builds. |
| Runtime | Start the server only when `GPUI_AGENT=1` (`true`/`yes`/`on`). |
| Release | Also require `GPUI_AGENT_ALLOW_RELEASE=1`. |
| Bind | Loopback default. `from_env` / `authorize_bind`. Non-loopback needs `GPUI_AGENT_REMOTE=1` and a token. See [SECURITY.md](SECURITY.md). |
| Token | **Required** on the host (`GPUI_AGENT_TOKEN`) to bind. `GPUI_AGENT_INSECURE_NO_TOKEN=1` restores untokened loopback for local demos (prints a banner). When a token is set, clients send v2 `auth` HMAC (never the raw token). CLI **`recipe run` and `mcp` require** a non-empty client token. Set the **same** value on host and client. `hello.auth` is `"required"` or `"none"`. |
| DoS caps | The server caps line size (1 MiB), concurrent connections (32), mailbox depth (128), and idle sockets (30s). See [SECURITY.md](SECURITY.md). |

```rust
if let Ok(Some(config)) = gpui_agent::from_env() {
    // bind config.addr, remember config.token
}
```

## 2. Implement `AgentHost`

```rust
impl AgentHost for MyStore {
    fn hello(&self) -> HelloInfo { /* app name, platform, ready; server fills hello.auth */ }
    fn snapshot(&self) -> UiTree { /* nodes with stable ids */ }
    fn keybindings(&self) -> Vec<gpui_agent::KeybindingInfo> { /* Action catalog */ }
    fn is_app_focused(&self) -> bool { false }
    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if op.is_virtual_input() {
            return Err(gpui_agent::virtual_unavailable(
                "this host has no GPUI event pipeline",
            ));
        }
        match op {
            Op::Click { target, .. } => self.click(target),
            Op::SetValue { target, value } => self.set_value(target, value),
            Op::Type { target, text, .. } => self.type_into(target, text),
            Op::Key { target, key, .. } => self.key(target, key),
            Op::Keybinding { .. } => self.fire_keybinding(op),
            Op::Keybindings => Ok(DispatchResult::json(
                gpui_agent::keybinding_list_json(&self.keybindings()),
            )),
            Op::Invoke { name, args } => self.invoke(name, args),
            Op::Shutdown => { self.shutdown = true; Ok(DispatchResult::empty()) }
            _ => Ok(DispatchResult::empty()),
        }
    }
}
```

Desktop GPUI: spawn `spawn_mailbox` and drain `AgentMailbox` on the UI
thread (the TCP thread must not touch GPUI objects). Intercept
`delivery=virtual` there and call `Window::dispatch_event` /
`dispatch_keystroke` — never OS HID. Intercept `Op::Keybinding` the
same way: resolve the Action id, then `Window::dispatch_action` only
(the same path the keymap uses). Listeners / `on_action` handlers own
store mutation. **Do not** call the Action body from the intercept as a
fallback — a no-op `dispatch_action` must fail closed
(`keybinding_unavailable: Action handler did not run`), not look green.
Reply after GPUI has run the handler (`cx.defer` / next effect if
`Window::dispatch_action` defers). `scope=focused` must not
auto-activate; `scope=global` is this app’s global map only — fail
closed if you cannot dispatch without focus (`keybinding_unavailable`).
Never synthesize OS HID. Intercept `Op::Screenshot` the
same way: on macOS call `gpui_agent::capture_window_via_screencapture`
with this window’s `CGWindowID` (viewport). For `mode=scrolled`,
resolve `target`, set scroll offset, wait for paint, capture tiles,
stitch, and restore offset (no OS HID). Other OSes return
`screenshot_unavailable` (including scrolled). Headless / tests:
`spawn_host` with `Arc<Mutex<YourStore>>` and return
`virtual_unavailable` / `screenshot_unavailable`.

## 3. Assign stable ids

Ids are **your** contract with agents. Prefer names that survive layout
changes:

- Page roots: `page-inbox`, `page-settings`
- Nav: `nav-inbox`, `nav-settings`
- Fields / buttons: `search-input`, `compose-send`
- Rows: `thread-1842`, `thread-archive-1842`

Put the same strings on GPUI `.id(...)` (for humans) and in `UiTree`
(for agents). Do not expose tree indices.

Numbered suffixes: `gpui_agent::parse_numbered_id("thread-", target)`.

## 4. How agents navigate

No special “open page” op. Agents:

```bash
gpui-agent click nav-settings
gpui-agent assert --id page-settings
gpui-agent click nav-toggle-sidebar
gpui-agent wait-until --timeout-ms 1000 --id todo-nav --visible false
```

Or register `invoke nav.go --arg page=settings` if a named command is
clearer than clicking. Both are generic CLI; only the **ids / names**
are yours.

### Visible vs exists vs in_viewport

| You want | Do this |
| --- | --- |
| Modal / alert / auth overlay | Keep a stable id (`auth-modal`), `role: dialog` (or `UiNode::dialog`), set `visible=false` when closed. Assert `--visible` / `--visible false`. |
| Sidebar | Keep the sidebar node when collapsed; `visible=false` (and hide descendants). Sample todo: `todo-nav` + `nav-toggle-sidebar` / `invoke todo.toggle_sidebar`. |
| Toast appeared then gone | Either drop the node (`exists=false`) or keep it with `visible=false`. Pick one and document it. |
| Scrolled into the painted window | `assert --in-viewport` when bounds are real. Headless / zero bounds: `in_viewport_unavailable` — do not fake geometry. |

Do **not** overload `states` with `"hidden"` / `"invisible"` as the only
signal. Optional human-readable `states` is fine; PROTOCOL is `visible`.

Mailbox drains: apply painted bounds onto the snapshot **before**
`assert` / `wait_until` so `in_viewport` sees the same clip as
`snapshot`. Zero leftover bounds for nodes you stopped painting.
`wait_until` is polled on the TCP thread for `spawn_mailbox` so the UI
thread can paint between tries. `wait` remains ready/paint only.

## 5. App-specific verbs

Put domain language in one of two places — **not** in a fork of the CLI:

1. **Host `invoke` names** (`prefs.set`, `mail.archive`)
2. **Agent prompts** (“to add a task, `set-value todo-input` then `click todo-add`”)

`examples/todo.sh` shows how a sample app can wrap `invoke` in a shell
script without polluting `gpui-agent`.

## 6. Claude Code

```json
{
  "mcpServers": {
    "gpui-agent": {
      "command": "gpui-agent",
      "args": ["mcp"],
      "env": {
        "GPUI_AGENT_ADDR": "127.0.0.1:17421",
        "GPUI_AGENT_TOKEN": "dev-secret"
      }
    }
  }
}
```

MCP tools are the protocol ops only, plus experimental `recipe_*`
batching tools (not app-specific verbs). `gpui-agent mcp` **refuses to
start** without a non-empty `GPUI_AGENT_TOKEN` / `--token`. Set the
same token on the host. List your ids and invoke names in the project
instructions. Recipe format (JSON canonical) and `--yes`:
[RECIPES.md](RECIPES.md). The MCP process holds one `AgentClient` and
reuses a single TCP session across `tools/call`.

## 7. Checklist

- [ ] `AgentHost` + stable ids on every actionable widget
- [ ] Server starts only with `GPUI_AGENT=1`, loopback bind
- [ ] Desktop mailbox drain on the UI thread
- [ ] Page roots assertable after nav clicks
- [ ] Overlays/sidebar/modals: stable id + `visible` (keep the node when closed if agents must assert “known but hidden”)
- [ ] Honest bounds for `in_viewport`; headless stays `in_viewport_unavailable`
- [ ] Optional `invoke` map documented for agents
- [ ] Optional: check in a JSON `recipe` of those ops so agents run one
      CLI invocation instead of one process per click
      ([RECIPES.md](RECIPES.md); laptop verify: [TRY_ON_MAC.md](TRY_ON_MAC.md)).
      `invoke` names in the recipe must match the host allow-list;
      shutdown recipes need `--yes`. Optional `--screenshot-dir` is
      observe-only; headless / Linux / Windows stay
      `screenshot_unavailable`. macOS **embedded-host** writes this
      window (`screencapture -l`). Recipes still cannot bypass
      [SECURITY.md](SECURITY.md#recipes-experimental). Visual note:
      [RECORDING.md](RECORDING.md).
- [ ] Product builds leave the feature off
- [ ] `hello.deliveries` lists `semantic` and, on a painted GPUI window, `virtual`
- [ ] `hello.auth` is `"required"` when `GPUI_AGENT_TOKEN` is set on the host (`"none"` otherwise)
- [ ] Recipe / MCP clients export the **same** `GPUI_AGENT_TOKEN` as the host
- [ ] Virtual click/type/key go through the mailbox → UI thread → `Window::dispatch_event` / `dispatch_keystroke` (never OS HID)
- [ ] `keybinding` fire goes through the mailbox → UI thread → the **Action** the keymap would dispatch (never OS HID). Listeners own mutation; no intercept fallback. `scope=focused` does not auto-activate. `scope=global` is this app’s global map only.
- [ ] Destructive bindings (`app.quit`, …) require `confirm=true` and list as `dangerous: true`
- [ ] Free-form `key` stays modifier-free (`keystroke_token` rejects `cmd-q`)
- [ ] Headless returns `virtual_unavailable` / `screenshot_unavailable`
      instead of pretending
- [ ] Desktop screenshot runs on the UI thread with a real `Window`
      (macOS **embedded-host**: `screencapture -l` of that window only).
      `mode=scrolled` stitches named-scroller tiles and restores offset.
      A GUI that is only a daemon client cannot serve a window PNG.

## 8. Copy-paste GPUI adapter

Do **not** add `gpui-kit` to `gpui-agent`. Copy this into the app crate:

1. **Mailbox drain (desktop).** `spawn_mailbox` on a background thread; drain `AgentMailbox` on the GPUI UI thread. Never touch GPUI objects from the TCP thread.
2. **Virtual dispatch.** On the UI thread, intercept `delivery=virtual` and call `Window::dispatch_event` / `dispatch_keystroke`. Never OS HID.
3. **Keybinding dispatch.** On the UI thread, intercept `Op::Keybinding`, authorize against the host catalog (`confirm` for dangerous, no silent scope promote), then dispatch the GPUI Action the keymap would. Do **not** mutate the store from the intercept as a fallback if Action dispatch is a no-op — reply after the handler runs, or fail closed (`keybinding_unavailable: Action handler did not run`). `scope=global` must not activate. If the kit pin cannot dispatch a global Action without focus, return `keybinding_unavailable`.
4. **macOS screenshot intercept.** On the UI thread, intercept `Op::Screenshot`. Viewport: `capture_window_via_screencapture` with this window’s `CGWindowID`. `mode=scrolled`: resolve `target`, set scroll offset, wait for paint, capture tiles, stitch, restore offset (no OS HID). Other OSes: `screenshot_unavailable` (including scrolled). Path confinement is unchanged.
5. **`spawn_mailbox` vs `spawn_host`.** Painted GPUI: mailbox. Headless / tests: `spawn_host(Arc<Mutex<Store>>)`.
6. **Default-deny token.** Bind via `from_env` requires `GPUI_AGENT_TOKEN` unless `GPUI_AGENT_INSECURE_NO_TOKEN=1`.
7. **Confined screenshots.** Host writes relative `.png` names under `GPUI_AGENT_SCREENSHOT_DIR` (default `{temp_dir}/gpui-agent-screenshots/`). Clients send a file name, not an absolute path.
8. **Protocol v2 HMAC.** After accept the host sends a challenge nonce; clients send `auth` = hex(`HMAC-SHA256(token, nonce)`). Do not put the raw token on the wire.
