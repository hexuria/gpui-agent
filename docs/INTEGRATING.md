# Integrating gpui-agent into another GPUI Kit app

`gpui-agent` is a **generic** control plane. Your app supplies the
semantic tree and action handlers; the CLI/MCP never learn your domain.

## 1. Opt in

| Gate | What you do |
| --- | --- |
| Compile | Feature-gate the bridge (`agent`). Default it **off** in product builds. |
| Runtime | Start the server only when `GPUI_AGENT=1` (`true`/`yes`/`on`). |
| Release | Also require `GPUI_AGENT_ALLOW_RELEASE=1`. |
| Bind | Loopback only. `gpui_agent::security::from_env` enforces this. |
| Token | Optional `GPUI_AGENT_TOKEN` on both app and CLI. **Set it** unless you are on a single-user box and accept that any local process can drive the UI. Every recipe step carries the same token. P1 does not require a token; **ask before requiring one when recipes/MCP are on** (P2). |
| DoS caps | The server caps line size (1 MiB), concurrent connections (32), mailbox depth (128), and idle sockets (30s). See [SECURITY.md](SECURITY.md). |

```rust
if let Ok(Some(config)) = gpui_agent::from_env() {
    // bind config.addr, remember config.token
}
```

## 2. Implement `AgentHost`

```rust
impl AgentHost for MyStore {
    fn hello(&self) -> HelloInfo { /* app name, platform, ready */ }
    fn snapshot(&self) -> UiTree { /* nodes with stable ids */ }
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
`dispatch_keystroke` — never OS HID. Headless / tests: `spawn_host`
with `Arc<Mutex<YourStore>>` and return `virtual_unavailable` for
virtual ops.

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
```

Or register `invoke nav.go --arg page=settings` if a named command is
clearer than clicking. Both are generic CLI; only the **ids / names**
are yours.

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
      "env": { "GPUI_AGENT_ADDR": "127.0.0.1:17421" }
    }
  }
}
```

MCP tools are the protocol ops only, plus experimental `recipe_*`
batching tools (not app-specific verbs). List your ids and invoke
names in the project instructions. Recipe format (JSON canonical) and
`--yes`: [RECIPES.md](RECIPES.md). The MCP process holds one
`AgentClient` and reuses a single TCP session across `tools/call`.

## 7. Checklist

- [ ] `AgentHost` + stable ids on every actionable widget
- [ ] Server starts only with `GPUI_AGENT=1`, loopback bind
- [ ] Desktop mailbox drain on the UI thread
- [ ] Page roots assertable after nav clicks
- [ ] Optional `invoke` map documented for agents
- [ ] Optional: check in a JSON `recipe` of those ops so agents run one
      CLI invocation instead of one process per click
      ([RECIPES.md](RECIPES.md); laptop verify: [TRY_ON_MAC.md](TRY_ON_MAC.md)).
      `invoke` names in the recipe must match the host allow-list;
      shutdown recipes need `--yes`. Optional `--screenshot-dir` is
      observe-only; headless stays `screenshot_unavailable`. Recipes
      still cannot bypass [SECURITY.md](SECURITY.md#recipes-experimental).
- [ ] Product builds leave the feature off
- [ ] `hello.deliveries` lists `semantic` and, on a painted GPUI window, `virtual`
- [ ] Virtual click/type/key go through the mailbox → UI thread → `Window::dispatch_event` / `dispatch_keystroke` (never OS HID)
- [ ] Headless returns `virtual_unavailable` instead of pretending
