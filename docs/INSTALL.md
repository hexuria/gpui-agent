# Install CLI + daemon (no GPUI)

Agents install two binaries. Neither pulls `gpui-kit` / Metal / Vulkan.

| Binary | Crate | Role |
| --- | --- | --- |
| `gpui-agent` | `crates/gpui-agent-cli` | Protocol CLI + MCP stdio |
| `todo-headless` | `apps/todo-headless` | Logic daemon (`serve` / `status` / `shutdown`) |

Protocol version is **v1** (`hello.protocol`). Skew: clients and hosts
with a different `v` fail closed (`authorize_request`).

## cargo install (from a checkout)

```bash
cargo install --path crates/gpui-agent-cli --locked=false
cargo install --path apps/todo-headless --locked=false
```

The repo does not commit `Cargo.lock`. CI builds release artifacts
without a lockfile.

## Run

```bash
export GPUI_AGENT=1
export GPUI_AGENT_TOKEN=dev-secret   # required for recipe/MCP; required for remote bind
todo-headless serve

# other terminal — same token
export GPUI_AGENT_TOKEN=dev-secret
gpui-agent wait
gpui-agent recipe run examples/recipes/todo-crud.json --set title="Buy milk"
todo-headless status
todo-headless shutdown
```

Smoke without a GUI: `./scripts/smoke-daemon.sh`.

## Remote (lab only)

Plaintext TCP + token. Prefer SSH or Tailscale until TLS.

```bash
# host
export GPUI_AGENT=1
export GPUI_AGENT_REMOTE=1
export GPUI_AGENT_TOKEN=lab-secret
export GPUI_AGENT_ADDR=10.0.0.2:17421
todo-headless serve

# client
export GPUI_AGENT_ALLOW_REMOTE=1
export GPUI_AGENT_TOKEN=lab-secret
gpui-agent --addr 10.0.0.2:17421 --allow-remote hello
```

`0.0.0.0` without the remote triple is refused.

## Claude Code / Codex / OpenGrok / Grok Bot

Install the two binaries on the **agent machine**. Point MCP at the
CLI (token required):

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

Start `todo-headless serve` with the **same** token. Do not expose MCP
stdio on a network. The GPUI `todo` window is optional and is a client
of this daemon ([ADR-001](ADR-001-daemon-sot.md)).

## CI artifacts

`.github/workflows/ci.yml` job `packages` builds
`target/release/gpui-agent` and `target/release/todo-headless` on
`ubuntu-latest` and uploads them. It also fails if `todo-headless`
gains a `gpui-kit` dependency.
