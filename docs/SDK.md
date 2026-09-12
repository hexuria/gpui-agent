# Building a controllable GPUI app (SDK)

`crates/gpui-agent` is the embeddable SDK. The CLI/MCP never learn your
domain. You implement [`AgentHost`](../crates/gpui-agent/src/host.rs),
publish a semantic [`UiTree`](../crates/gpui-agent/src/tree.rs), and
either:

- **Product / agent-machine:** run a headless daemon that owns the store
  ([ADR-001](ADR-001-daemon-sot.md)). Agents talk to the daemon.
- **Widget E2E:** keep an in-process `AgentHost` + mailbox on the UI
  thread so virtual delivery can hit painted GPUI widgets.

```rust
use gpui_agent::prelude::*;
use gpui_agent::testing::TestHost;
```

## 1. Tree builders

Prefer typed constructors over stringly `UiNode::new`:

```rust
UiNode::window("app-window", "Mail")
    .with_child(UiNode::navigation("nav", "Nav")
        .with_child(UiNode::button("nav-inbox", "Inbox"))
        .with_child(UiNode::button("nav-settings", "Settings")))
    .with_child(UiNode::page("page-inbox", "Inbox")
        .with_child(UiNode::textbox("search-input", "Search").with_value(q)));
```

Roles live in `gpui_agent::role`. Wire format is still a string so you
can add app-specific roles. Focus/value: `with_focused`, `with_value`,
`with_enabled`, `with_bounds`.

Numbered ids: `gpui_agent::parse_numbered_id("thread-", target)`.

## 2. Host + test helper

```rust
impl AgentHost for MyStore {
    fn hello(&self) -> HelloInfo { /* app, platform, ready */ }
    fn snapshot(&self) -> UiTree { self.tree() }
    fn keybindings(&self) -> Vec<KeybindingInfo> { /* Action catalog */ }
    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if op.is_virtual_input() {
            return Err(virtual_unavailable("no GPUI pipeline on this host"));
        }
        // click / set_value / keybinding / invoke …
        Ok(DispatchResult::empty())
    }
}

#[test]
fn recipe_against_sdk_host() {
    let host = TestHost::spawn(MyStore::default(), Some("ci".into())).unwrap();
    let mut client = host.client().with_token("ci");
    client.wait_ready().unwrap();
    client.click("nav-settings").unwrap();
    let tree = client.snapshot().unwrap().tree.unwrap();
    assert!(tree.find("page-settings").is_some());
}
```

`TestHost` binds `127.0.0.1:0`. Screenshot stays honest: hosts without a
surface return `screenshot_unavailable` (do not invent PNGs).

## 3. Desktop mailbox vs daemon

| Path | When | Who owns state |
| --- | --- | --- |
| `spawn_host` | daemon, CI, `TestHost` | The `AgentHost` you passed in |
| `spawn_mailbox` | GPUI window widget E2E | UI-thread store; TCP thread must not touch GPUI |
| GUI as `AgentClient` | product Mac window (this repo’s `apps/todo`) | The **daemon**. GUI polls `snapshot`. |

Do not run a competing product store in the GUI. Feature
`embedded-host` on `apps/todo` is the widget-E2E exception.

## 4. Opt-in / bind

Same gates as [SECURITY.md](SECURITY.md): `GPUI_AGENT=1`, release latch,
loopback default, remote only with token + `GPUI_AGENT_REMOTE=1`.
Recipe/MCP still require a client token.

Caps: 1 MiB line, 32 connections, 128 mailbox. No OS HID. No wire
`batch`. Semantic default. `keybinding` is Action-id dispatch (never
HID); free-form `key` stays modifier-free.

## 5. Sample

`todo-core` is the smallest host: builder tree, nav + settings, CRUD
`invoke` names. Drive it with `todo-headless serve` and
`gpui-agent recipe run examples/recipes/todo-crud.json`. CI:
[INSTALL.md](INSTALL.md) and `.github/workflows/ci.yml`.
