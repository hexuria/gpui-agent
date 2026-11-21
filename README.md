# gpui-agent

One protocol, one CLI and one MCP server to drive apps that embed an
`AgentHost`: [Native Shell](https://github.com/hexuria/nsh) (nsh) apps on
Android, iOS, macOS, Windows, Linux and Web, and GPUI apps.

The app publishes a semantic tree (stable ids, roles, names, state) and
accepts actions by id over newline-delimited JSON on a loopback socket. The
server starts only when the app runs with `GPUI_AGENT=1` and a token. This is
not the Chrome DevTools Protocol: it does not attach to an arbitrary process.

| Crate | What |
| --- | --- |
| `gpui-agent` | Protocol types, server, client and tree builders to embed in an app |
| `gpui-agent-cli` | The `gpui-agent` CLI and its MCP stdio server |
| `gpui-agent-recipe` | JSON recipes: many protocol ops in one run |
| `gpui-agent-policy` | Local policy behind `run --goal` (no model, no network) |

## Install

```bash
cargo binstall gpui-agent-cli     # prebuilt binary from the GitHub release
cargo install gpui-agent-cli      # or build it
```

Each [release](https://github.com/hexuria/gpui-agent/releases) also has
tarballs with a `.sha256` for Linux (x86_64, aarch64) and macOS (x86_64,
aarch64). The binary is `gpui-agent`. Features and building from a checkout:
[docs/INSTALL.md](docs/INSTALL.md).

## Quick start: an nsh app

nsh apps reach gpui-agent through our fork
[hexuria/nsh](https://github.com/hexuria/nsh), branch `buwiz`. It tracks
[longbridge/nsh](https://github.com/longbridge/nsh) plus our patches and
changes often; its `crates/nsh-gpui-agent` is the adapter, and `buwiz`
depends on `gpui-agent = "=0.3.0"`. Build `nsh` from a checkout of that
branch (`cargo build -p nsh-cli`), note the commit you tested, and move it
on purpose. The adapter is for development builds.

```bash
export GPUI_AGENT_TOKEN="$(openssl rand -hex 32)"
export GPUI_AGENT_ADDR=127.0.0.1:17421
GPUI_AGENT=1 nsh run --target linux examples/gallery &   # or macos, windows, web, ios, android

gpui-agent wait
gpui-agent snapshot --refs
gpui-agent press @e6
gpui-agent screenshot --inline --out gallery.png
```

On Android, forward the port first: `adb forward tcp:17421 tcp:17421`.
`gpui-agent targets` lists hosts and devices; `gpui-agent doctor` says why a
host does not answer.

## Quick start: a GPUI app

`apps/todo` is a GPUI Kit demo that embeds `AgentHost` (feature
`embedded-host`). `apps/todo-headless` serves the same tree without a window.

```bash
# terminal 1
export GPUI_AGENT=1 GPUI_AGENT_TOKEN=dev-secret
cargo run -p todo --features embedded-host    # or: cargo run -p todo-headless -- serve

# terminal 2
export GPUI_AGENT_TOKEN=dev-secret
gpui-agent wait
gpui-agent set-value todo-input "Buy milk"
gpui-agent click todo-add --wait-for todo-item-1
gpui-agent click nav-settings
gpui-agent assert --id page-settings --role page
gpui-agent shutdown
```

To embed `AgentHost` in your own app: [docs/INTEGRATING.md](docs/INTEGRATING.md).

## CLI

```bash
gpui-agent snapshot --refs --depth 2           # outline; every command takes @eN or an id
gpui-agent click todo-add --wait-for todo-item-1
gpui-agent type --stdin password-input < secret.txt
gpui-agent scroll todo-list-scroll --dy 300    # also swipe, long-press, pinch, hover, drag
gpui-agent invoke todo.list                    # named command the app registered
gpui-agent --session app attach 127.0.0.1:17421   # save address and token
gpui-agent recipe run examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json --set title="Buy milk"   # many ops, one connection
gpui-agent skill                               # usage notes for a coding agent
```

Ops and replies: [docs/PROTOCOL.md](docs/PROTOCOL.md). Recipes:
[docs/RECIPES.md](docs/RECIPES.md).

## MCP

`gpui-agent mcp` serves the same ops as tools over stdio and keeps one
connection to the host. It needs the host's token.

```json
{
  "mcpServers": {
    "gpui-agent": {
      "command": "gpui-agent",
      "args": ["mcp"],
      "env": { "GPUI_AGENT_ADDR": "127.0.0.1:17421", "GPUI_AGENT_TOKEN": "dev-secret" }
    }
  }
}
```

## Platforms

| Platform | Host | How it is reached |
| --- | --- | --- |
| Android | nsh app (`nsh run --target android`) | `adb forward tcp:17421 tcp:17421`, then loopback |
| iOS | nsh app in the Simulator (`--target ios`) | Loopback: the Simulator shares the Mac's network |
| macOS | nsh app (`--target macos`), GPUI app | Loopback |
| Windows | nsh app (`--target windows`), GPUI app | Loopback |
| Linux | nsh app on GTK (`--target linux`), GPUI app | Loopback |
| Web | nsh app (`--target web`) | `nsh` serves the host on loopback; the page runs in a Chromium it opens, or in a tab you open (`GPUI_AGENT_WEB_TAB=1`, dial-out tab) |
| None | `todo-headless` | Loopback |

nsh's GPUI and terminal backends have no agent adapter yet. Tested results
per host: [docs/PLATFORMS.md](docs/PLATFORMS.md).

## Security

The agent is off unless the app runs with `GPUI_AGENT=1`, binds to loopback by
default and requires a token, which the client proves with an HMAC and never
sends. Anyone holding the token can drive the app as the user. Details:
[docs/SECURITY.md](docs/SECURITY.md).

## License

Apache-2.0.
