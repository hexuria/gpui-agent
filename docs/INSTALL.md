# Install CLI + daemon (no GPUI)

Agents install two binaries. Neither pulls `gpui-kit` / Metal / Vulkan.

| Binary | Crate | Role |
| --- | --- | --- |
| `gpui-agent` | `crates/gpui-agent-cli` | Protocol CLI + MCP stdio |
| `todo-headless` | `apps/todo-headless` | Logic daemon (`serve` / `status` / `shutdown`) |

Protocol version is 2 (`hello.protocol`). A request with another `v` fails
with `unsupported protocol version`.

## cargo install

From crates.io, no git checkout. The package is `gpui-agent-cli`; the binary is `gpui-agent`.

```bash
cargo install gpui-agent-cli
```

`cargo binstall gpui-agent-cli` downloads the prebuilt binary from the `v*` GitHub Release (`gpui-agent-cli-<target>-v<version>.tgz` and a sibling `.sha256`). `todo-headless` is not published.

## Release tarball

Each `v*` release has `gpui-agent-cli-<target>-v<version>.tgz` and a
`.sha256` for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin` and `aarch64-apple-darwin`:

```bash
T=x86_64-unknown-linux-gnu V=0.3.0
gh release download v$V -R hexuria/gpui-agent -p "gpui-agent-cli-$T-v$V.tgz*"
sha256sum -c gpui-agent-cli-$T-v$V.tgz.sha256    # macOS: shasum -a 256 -c
tar xzf gpui-agent-cli-$T-v$V.tgz                # gpui-agent-cli/gpui-agent
```

## From a checkout

`rust-toolchain.toml` pins Rust 1.99.0.

```bash
cargo build -p gpui-agent-cli -p todo-headless   # target/debug/gpui-agent, todo-headless
cargo build -p todo --features embedded-host     # optional: GPUI window with the host in process
```

Or install them:

```bash
cargo install --path crates/gpui-agent-cli --locked=false
cargo install --path apps/todo-headless --locked=false
```

The repo does not commit `Cargo.lock`. CI builds release artifacts
without a lockfile.

## Features

The CLI's default build includes `instinct`; `jev` is opt-in.

| Feature | On crate | Default | Adds | Needs |
| --- | --- | --- | --- | --- |
| `instinct` | `gpui-agent-cli` | on | `run --goal` local driver; `locate` + `act` MCP tools; turns on `gpui-agent-recipe/instinct` | nothing — deterministic, local, no model/network |
| `instinct` | `gpui-agent-recipe` | off (on through the CLI) | Instinct-backed `resolve_intent` (abstains instead of guessing) | nothing |
| `jev` | `gpui-agent-cli` | off | `run --goal --escalate jev` (System One fallback, only after a local abstain) | `TYPESAFE_API_KEY`; implies `instinct` |

```bash
# + optional System One escalation on abstain
cargo install --path crates/gpui-agent-cli --features jev --locked=false
# without instinct: the original tool set and keyword resolver
cargo install --path crates/gpui-agent-cli --no-default-features --locked=false
```

A binary built without `jev` answers `--escalate jev` with a clear
feature error; a `--no-default-features` build has no
`run`/`locate`/`act`.
Wire compatibility is additive (`confidence_millis`, `rev`,
`expect_fingerprint` are optional fields), so old clients keep working
against feature-enabled hosts and vice versa. `snapshot since=<rev>`,
`format=compact`, and `fingerprints=true` are protocol params in every
build — not feature-gated.

## Connect by app

`--addr` defaults to the loopback demo port when nothing else is set. To drive a specific app, name it. Do not hardcode that app's port.

```bash
gpui-agent --connect dev.example.nativechat hello
gpui-agent --connect eBIRForms invoke profile.list
# same session: no --addr and no second --connect
gpui-agent hello
```

`--connect` matches the host's bundle id, `GPUI_AGENT_CONNECT_ID`, executable name, or `hello.app`. A missing app is one error that names the id. Two running apps do not share a port. `--addr` and `GPUI_AGENT_ADDR` override discovery.

Hosts publish a record while they are listening and delete it on shutdown. A record whose process has exited is ignored.

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
stdio on a network. The GPUI `todo` window is optional and, without
`embedded-host`, is a client of this daemon.

## CI artifacts

`.github/workflows/ci.yml` job `packages` builds
`target/release/gpui-agent` and `target/release/todo-headless` on
`ubuntu-latest` and uploads them. It also fails if `todo-headless`
gains a `gpui-kit` dependency.
