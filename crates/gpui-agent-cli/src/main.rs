mod doctor;
#[cfg(feature = "instinct")]
mod instinct_wiring;
mod mcp;
mod recipe_cmd;
mod refs;
#[cfg(feature = "instinct")]
mod run_cmd;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, DeliveryMode, KeybindingScope, Op};
use recipe_cmd::RecipeCommand;

/// Talk to an embedded AgentHost over the opt-in agent protocol (not CDP).
///
/// The host must implement `AgentHost`, publish stable ids, and start the
/// server under `GPUI_AGENT=1`. This CLI does not attach to an arbitrary
/// GPUI Kit process.
///
/// First-class commands are the protocol ops only. App-specific verbs
/// (`todo.add`, `nav.go`, …) belong in the host (`invoke`) or in agent
/// prompts — not as CLI subcommands.
#[derive(Parser)]
#[command(name = "gpui-agent", after_help = AFTER_HELP)]
struct Cli {
    /// Host:port of the automation server. When omitted, `--connect` is used,
    /// then the loopback default. Non-loopback needs `--allow-remote` and a
    /// non-empty token.
    #[arg(long, env = "GPUI_AGENT_ADDR")]
    addr: Option<SocketAddr>,
    /// Bundle id or executable name of the app to drive. The CLI finds that
    /// app's AgentHost and remembers it; `--connect last` reuses that choice.
    /// A command with neither `--addr` nor `--connect` does not: another agent
    /// on this Mac may have picked a different app.
    #[arg(long, env = "GPUI_AGENT_CONNECT")]
    connect: Option<String>,
    /// Shared secret; must match `GPUI_AGENT_TOKEN` on the host when the host
    /// has one. Required (non-empty) for `recipe run` and `mcp`.
    #[arg(long, env = "GPUI_AGENT_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Connect to a non-loopback host. Requires a non-empty token. Plaintext
    /// TCP — lab / trusted network only. Prefer SSH or Tailscale until TLS.
    #[arg(long, env = "GPUI_AGENT_ALLOW_REMOTE")]
    allow_remote: bool,
    /// How long to wait for the host's reply to one request, in
    /// milliseconds (default 8000). Raise it for a slow device or emulator.
    #[arg(
        long = "reply-timeout-ms",
        env = "GPUI_AGENT_REPLY_TIMEOUT_MS",
        global = true,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    request_timeout_ms: Option<u64>,
    /// Send the op as written, without checking the host's feature list
    /// first: the reply is the host's own, refusal included. For testing a
    /// host; an agent wants the default.
    #[arg(long, global = true)]
    as_written: bool,
    #[command(subcommand)]
    command: Command,
}

const AFTER_HELP: &str = "\
App-specific helpers (for example the sample todo app) live in examples/,
not in this CLI. Navigate pages with click + assert on stable ids, or invoke
a command the host registered. Experimental: batch many ops in one process
with `recipe validate|plan|run|resolve` (JSON canonical; `.wants` also
accepted — see docs/RECIPES.md). Host bind is default-deny: set
GPUI_AGENT_TOKEN on the host. `recipe run` and `mcp` require a non-empty
GPUI_AGENT_TOKEN or --token; set the same value on the host. Non-loopback
`--addr` also needs `--allow-remote` (or GPUI_AGENT_ALLOW_REMOTE=1) and a
token so a mistyped address cannot leak the secret (see docs/SECURITY.md).
`--connect <bundle id or executable>` picks the app; `--connect last` reuses
the last pick. Every command that discovery routes names its app on stderr.";

#[derive(Debug, Subcommand)]
enum Command {
    /// Block until the host answers hello/ready.
    Wait {
        /// How long the client retries connecting (milliseconds).
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Poll assert fields until they match or `timeout_ms` elapses.
    WaitUntil {
        /// How long the host polls the snapshot (milliseconds).
        #[arg(long)]
        timeout_ms: u64,
        /// Stable id of the node (protocol field: `target`).
        #[arg(long, visible_alias = "target")]
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        value: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        checked: Option<bool>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true", default_value_t = true)]
        exists: bool,
        #[arg(long)]
        absent: bool,
        /// `--visible` or `--visible true|false`. Host-declared visibility.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        visible: Option<bool>,
        /// `--in-viewport` or `--in-viewport true|false`. Geometry; fail closed if unknown.
        #[arg(long = "in-viewport", num_args = 0..=1, default_missing_value = "true")]
        in_viewport: Option<bool>,
    },
    /// List the hosts published on this machine and the devices `adb`
    /// sees, with the local addresses forwarded to them.
    Targets,
    /// Print the skill that teaches a coding agent to use this CLI, for the
    /// version of the CLI that prints it. Save it where your agent reads
    /// skills (for example `.agents/skills/gpui-agent/SKILL.md`).
    Skill,
    /// Check the address, the token, the host's capabilities and how fast
    /// it answers; says what to fix when a step fails.
    Doctor,
    /// Handshake: protocol version, app name, platform, ready.
    Hello,
    /// Print the semantic UI tree as JSON.
    Snapshot {
        #[arg(long)]
        pretty: bool,
        /// Print an outline instead of JSON: one line per visible node that
        /// says something or can be acted on, each with a short ref
        /// (`@e1`, `@e2`, …). Any later command takes a ref where it takes a
        /// node id, until the next `snapshot --refs` renumbers them.
        #[arg(long, conflicts_with = "pretty")]
        refs: bool,
    },
    /// Observe-only PNG of the **app surface**. Host writes `--out`.
    /// Headless / Linux / Windows: `screenshot_unavailable` (no fake file).
    /// macOS desktop: PNG of **this window** via `screencapture -l`.
    /// `--mode scrolled` requires `--target` (named scroller); stitches tiles.
    Screenshot {
        /// Destination PNG on this machine so the image does not ride NDJSON.
        #[arg(long)]
        out: std::path::PathBuf,
        /// `viewport` (default) or `scrolled`.
        #[arg(long, default_value = "viewport")]
        mode: gpui_agent::ScreenshotMode,
        /// Stable scroll-view id. Required when `--mode scrolled`.
        #[arg(long)]
        target: Option<String>,
        /// Safety cap for scrolled capture (max 16384).
        #[arg(long)]
        max_height_px: Option<u32>,
        /// Fetch the PNG over the connection and write `--out` here, for a
        /// host whose files this machine cannot read (a device, a browser).
        #[arg(long)]
        inline: bool,
    },
    /// Press (tap) a node, or a point with `--x`/`--y`, as the platform delivers it.
    Press {
        target: Option<String>,
        #[arg(long)]
        x: Option<f64>,
        #[arg(long)]
        y: Option<f64>,
    },
    /// Scroll a node, or what is under `--x`/`--y`, by a delta in points.
    Scroll {
        target: Option<String>,
        #[arg(long)]
        x: Option<f64>,
        #[arg(long)]
        y: Option<f64>,
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        dx: f64,
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        dy: f64,
    },
    /// Move the pointer over a node or point (hosts with a pointer).
    Hover {
        target: Option<String>,
        #[arg(long)]
        x: Option<f64>,
        #[arg(long)]
        y: Option<f64>,
    },
    /// Drag `--from` onto `--to`, or drag a node / point by `--dx`/`--dy`.
    Drag {
        target: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        x: Option<f64>,
        #[arg(long)]
        y: Option<f64>,
        #[arg(long, allow_hyphen_values = true)]
        dx: Option<f64>,
        #[arg(long, allow_hyphen_values = true)]
        dy: Option<f64>,
    },
    /// One-finger swipe (`up`, `down`, `left`, `right`) across a node or the surface.
    Swipe {
        direction: gpui_agent::SwipeDirection,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        distance: Option<f64>,
    },
    /// Press and hold a node or point.
    LongPress {
        target: Option<String>,
        #[arg(long)]
        x: Option<f64>,
        #[arg(long)]
        y: Option<f64>,
        #[arg(long)]
        duration_ms: Option<u64>,
    },
    /// Two-finger pinch on a node: `--scale` above 1 spreads, below 1 pinches in.
    Pinch {
        target: Option<String>,
        #[arg(long)]
        scale: f64,
    },
    /// Resize the app's content area to `--width`/`--height` points or a `--preset`.
    Viewport {
        #[arg(long)]
        width: Option<f64>,
        #[arg(long)]
        height: Option<f64>,
        #[arg(long)]
        preset: Option<String>,
    },
    /// Insert text into an editable node as one paste.
    Paste { target: String, text: String },
    /// Device action: back, home, rotate, keyboard-show, keyboard-hide,
    /// background, foreground, open-url. `rotate` and `open-url` take a value.
    System {
        action: gpui_agent::SystemAction,
        value: Option<String>,
    },
    /// Activate a widget by stable id (`nav-settings`, `submit`, …).
    Click {
        target: String,
        /// `semantic` (default) calls the handler. `virtual` synthesizes GPUI events.
        #[arg(long, default_value = "semantic")]
        delivery: DeliveryMode,
    },
    /// Append text to an editable widget.
    Type {
        target: String,
        text: String,
        #[arg(long, default_value = "semantic")]
        delivery: DeliveryMode,
    },
    /// Replace the value of an editable widget.
    SetValue { target: String, value: String },
    /// Send a key (`Enter`, `Backspace`) to a widget. No modifiers (use `keybinding`).
    Key {
        target: String,
        key: String,
        #[arg(long, default_value = "semantic")]
        delivery: DeliveryMode,
    },
    /// Fire a GPUI Action by id (keymap path, never OS HID).
    Keybinding {
        /// Action id (`app.quit`). Protocol field: `binding` (request `id` is the RPC id).
        #[arg(long)]
        id: String,
        /// `focused` (this window’s map) or `global` (this app’s global map only).
        #[arg(long)]
        scope: KeybindingScope,
        /// Optional chord metadata / resolve aid (`cmd-q`).
        #[arg(long)]
        chord: Option<String>,
        /// Required for destructive Actions (`app.quit`).
        #[arg(long)]
        confirm: bool,
        /// Opt-in: bring this app’s window forward first (`scope=focused` only).
        #[arg(long)]
        activate: bool,
    },
    /// List registered Action-id keybindings (`{id, chord, scope, dangerous}`).
    Keybindings,
    /// Poll the host's bounded event stream (M9). Prints
    /// `{events:[{seq,level,target,message}…], latest, lagged}`;
    /// `latest` is the cursor for the next `--since`.
    Events {
        /// Only events with seq greater than this (default: all retained).
        #[arg(long)]
        since: Option<u64>,
        /// Max events returned (default 256, capped at 1024).
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Assert fields on a node from the current snapshot.
    Assert {
        /// Stable id of the node (protocol field: `target`).
        #[arg(long, visible_alias = "target")]
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        value: Option<String>,
        #[arg(long)]
        role: Option<String>,
        /// `--checked` or `--checked true|false`. A following `true`/`false` is the value, not a positional.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        checked: Option<bool>,
        /// `--exists` or `--exists true|false` (default true).
        #[arg(long, num_args = 0..=1, default_missing_value = "true", default_value_t = true)]
        exists: bool,
        /// Invert `--exists` (node must be absent). Flag only; takes no value.
        #[arg(long)]
        absent: bool,
        /// `--visible` or `--visible true|false`. Host-declared visibility.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        visible: Option<bool>,
        /// `--in-viewport` or `--in-viewport true|false`. Geometry; fail closed if unknown.
        #[arg(long = "in-viewport", num_args = 0..=1, default_missing_value = "true")]
        in_viewport: Option<bool>,
    },
    /// Call a named host command the app registered.
    Invoke {
        name: String,
        /// Repeatable `key=value` pairs. Values are JSON if they parse, else strings.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
    },
    /// Ask the host to exit.
    Shutdown,
    /// Tiny MCP stdio server exposing the same generic tools. Requires `--token` / `GPUI_AGENT_TOKEN`.
    Mcp {
        /// App invoke/id schema JSON. Repeatable. Also `GPUI_AGENT_SCHEMA` (OS path list).
        #[arg(long = "schema", value_name = "PATH")]
        schema: Vec<std::path::PathBuf>,
    },
    /// Experimental (instinct): run a natural-language goal through the local agent loop.
    /// Prints a receipt; exits 0 done, 2 abstained/blocked/cap, 1 error. Requires a token.
    #[cfg(feature = "instinct")]
    Run {
        /// The goal text (`then` / `and then` split sequential clauses).
        #[arg(long)]
        goal: String,
        /// Instinct arbitration profile.
        #[arg(long, default_value = "standard", value_parser = ["fast", "standard", "deep"])]
        profile: String,
        /// Print the raw JSON receipt instead of the text summary.
        #[arg(long)]
        json: bool,
        /// Escalate local-policy abstains to a named engine (M8).
        /// `jev` uses System One via typesafe-sdk (requires
        /// TYPESAFE_API_KEY; feature `jev`).
        #[arg(long = "escalate", value_name = "ENGINE", value_parser = ["jev"], conflicts_with = "escalate_cmd")]
        escalate: Option<String>,
        /// Escalate local-policy abstains to `<program> [args…]` over the
        /// closed JSON stdin/stdout wire (M7). Off by default; the
        /// endpoint may only pick from the offered menu or abstain.
        #[arg(long = "escalate-cmd", value_name = "PROGRAM [ARGS…]", num_args = 1..)]
        escalate_cmd: Vec<String>,
        /// Per-escalation timeout in milliseconds (default 30000).
        #[arg(long = "escalate-timeout-ms", default_value_t = 30000)]
        escalate_timeout_ms: u64,
        /// App invoke/id schema JSON. Repeatable. Also `GPUI_AGENT_SCHEMA` (OS path list).
        #[arg(long = "schema", value_name = "PATH")]
        schema: Vec<std::path::PathBuf>,
    },
    /// Experimental: validate / plan / run / resolve a JSON recipe of protocol ops (`.wants` also accepted). `run` requires a token.
    Recipe {
        /// App invoke/id schema JSON. Repeatable. Also `GPUI_AGENT_SCHEMA` (OS path list).
        #[arg(long = "schema", value_name = "PATH", global = true)]
        schema: Vec<std::path::PathBuf>,
        #[command(subcommand)]
        action: RecipeCommand,
    },
}

/// The skill text, stamped with this build's version.
fn skill() -> String {
    include_str!("../skill/SKILL.md").replacen(
        "\n---\n",
        concat!("\nversion: ", env!("CARGO_PKG_VERSION"), "\n---\n"),
        1,
    )
}

fn connect(addr: SocketAddr, timeout: Option<Duration>) -> AgentClient {
    let client = AgentClient::connect(addr);
    match timeout {
        Some(timeout) => client.with_timeout(timeout),
        None => client,
    }
}

fn wait_reply_timeout(reply_timeout: Duration, bound_ms: Option<u64>) -> Duration {
    let bound = bound_ms
        .map(|ms| Duration::from_millis(ms.saturating_add(2_000)))
        .unwrap_or_default();
    reply_timeout.max(bound)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn take_non_empty_token(token: Option<String>) -> Option<String> {
    token.filter(|s| !s.is_empty())
}

const RECIPE_MCP_TOKEN_REQUIRED: &str = "recipe run and mcp require a non-empty GPUI_AGENT_TOKEN or --token \
     (set the same token on the host)";

fn require_recipe_mcp_token(token: Option<&str>) -> Result<&str> {
    match token {
        Some(t) if !t.is_empty() => Ok(t),
        _ => Err(anyhow!("{RECIPE_MCP_TOKEN_REQUIRED}")),
    }
}

fn run() -> Result<()> {
    let mut cli = Cli::parse();
    if matches!(cli.command, Command::Skill) {
        print!("{}", skill());
        return Ok(());
    }
    if matches!(cli.command, Command::Targets) {
        println!("{}", serde_json::to_string_pretty(&doctor::targets())?);
        return Ok(());
    }
    let target = gpui_agent::resolve_client_target(
        &gpui_agent::runtime_root(),
        cli.addr,
        cli.connect.as_deref(),
    )?;
    // Say which app discovery picked before acting on it, so a command that lands in the wrong
    // window is visible. Stderr keeps stdout (and `mcp`'s protocol) clean.
    if target.app.is_some() {
        eprintln!("gpui-agent: {}", target.describe());
    }
    let addr = target.addr;
    // `@e3` stands for a node of the last `snapshot --refs` of this host.
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args
        .iter()
        .skip(1)
        .any(|arg| arg.to_str().is_some_and(refs::is_ref))
    {
        refs::resolve(&gpui_agent::runtime_root(), addr, &mut args[1..]).map_err(|e| anyhow!(e))?;
        cli = Cli::parse_from(args);
    }
    let token = take_non_empty_token(cli.token);
    let request_timeout = cli.request_timeout_ms.map(Duration::from_millis);
    gpui_agent::authorize_client(addr, token.as_deref(), cli.allow_remote)
        .with_context(|| format!("refusing agent address {addr}"))?;

    if matches!(cli.command, Command::Doctor) {
        let mut client = connect(addr, request_timeout);
        if let Some(token) = token.clone() {
            client = client.with_token(token);
        }
        if doctor::doctor(addr, client, token.is_some()) {
            return Ok(());
        }
        return Err(anyhow!("the host cannot be driven as it stands"));
    }
    if let Command::Mcp { schema } = cli.command {
        let token = require_recipe_mcp_token(token.as_deref())?;
        return mcp::run(addr, token.to_string(), request_timeout, schema);
    }
    if let Command::Recipe { schema, action } = cli.command {
        let client = if matches!(action, RecipeCommand::Run { .. }) {
            let token = require_recipe_mcp_token(token.as_deref())?;
            Some(connect(addr, request_timeout).with_token(token))
        } else {
            None
        };
        return recipe_cmd::run(client, action, &schema);
    }

    #[cfg(feature = "instinct")]
    if let Command::Run {
        ref goal,
        ref profile,
        json,
        ref escalate,
        ref escalate_cmd,
        escalate_timeout_ms,
        ref schema,
    } = cli.command
    {
        let token = require_recipe_mcp_token(token.as_deref())?;
        let client = connect(addr, request_timeout).with_token(token);
        return run_cmd::run(
            client,
            run_cmd::RunOpts {
                goal,
                profile,
                json_out: json,
                escalate: escalate.as_deref(),
                escalate_cmd,
                escalate_timeout_ms,
                schema,
            },
        );
    }

    let mut client = connect(addr, request_timeout);
    if let Some(token) = token {
        client = client.with_token(token);
    }
    if cli.as_written {
        client = client.as_written();
    }

    match cli.command {
        Command::Wait { timeout_ms } => {
            let reply_timeout = request_timeout.unwrap_or_else(|| client.timeout());
            client.set_timeout(wait_reply_timeout(reply_timeout, timeout_ms));
            print_resp(rpc(client.expect_ok(Op::Wait { timeout_ms }))?)
        }
        Command::WaitUntil {
            timeout_ms,
            id,
            name,
            value,
            role,
            checked,
            exists,
            absent,
            visible,
            in_viewport,
        } => {
            let reply_timeout = request_timeout.unwrap_or_else(|| client.timeout());
            client.set_timeout(wait_reply_timeout(reply_timeout, Some(timeout_ms)));
            let spec = AssertSpec {
                target: id,
                name,
                value,
                role,
                checked,
                exists: Some(if absent { false } else { exists }),
                visible,
                in_viewport,
            };
            print_resp(rpc(client.wait_until(spec, timeout_ms))?)
        }
        Command::Hello => print_resp(rpc(client.expect_ok(Op::Hello))?),
        Command::Snapshot { pretty, refs } => {
            let resp = rpc(client.snapshot())?;
            if refs {
                let tree = resp
                    .tree
                    .ok_or_else(|| anyhow!("the reply carried no tree"))?;
                let (outline, table) = refs::render(&tree);
                refs::save(&gpui_agent::runtime_root(), addr, &table)
                    .context("saving the ref table")?;
                print!("{outline}");
            } else if pretty {
                println!("{}", serde_json::to_string_pretty(&resp.tree)?);
            } else {
                print_resp(resp);
            }
        }
        Command::Screenshot {
            out,
            mode,
            target,
            max_height_px,
            inline,
        } => {
            if inline {
                let png = rpc(client.screenshot_bytes(mode, target, max_height_px))?;
                std::fs::write(&out, &png)
                    .map_err(|err| anyhow!("write {}: {err}", out.display()))?;
                println!(
                    "{}",
                    serde_json::json!({"ok": true, "result": {"path": out, "bytes": png.len()}})
                );
                return Ok(());
            }
            if mode.is_scrolled() && target.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(anyhow!(
                    "screenshot --mode scrolled requires --target <scroll-view-id>"
                ));
            }
            print_resp(rpc(client.screenshot_with(
                out.to_string_lossy().into_owned(),
                mode,
                target,
                max_height_px,
            ))?)
        }
        Command::Press { target, x, y } => print_resp(rpc(client.gated(Op::Press {
            target,
            x,
            y,
            expect_fingerprint: None,
        }))?),
        Command::Scroll {
            target,
            x,
            y,
            dx,
            dy,
        } => print_resp(rpc(client.gated(Op::Scroll {
            target,
            x,
            y,
            dx,
            dy,
        }))?),
        Command::Hover { target, x, y } => {
            print_resp(rpc(client.gated(Op::Hover { target, x, y }))?)
        }
        Command::Drag {
            target,
            from,
            to,
            x,
            y,
            dx,
            dy,
        } => print_resp(rpc(client.gated(Op::Drag {
            from,
            to,
            target,
            x,
            y,
            dx,
            dy,
        }))?),
        Command::Swipe {
            direction,
            target,
            distance,
        } => print_resp(rpc(client.gated(Op::Swipe {
            target,
            direction,
            distance,
        }))?),
        Command::LongPress {
            target,
            x,
            y,
            duration_ms,
        } => print_resp(rpc(client.gated(Op::LongPress {
            target,
            x,
            y,
            duration_ms,
        }))?),
        Command::Pinch { target, scale } => {
            print_resp(rpc(client.gated(Op::Pinch { target, scale }))?)
        }
        Command::Viewport {
            width,
            height,
            preset,
        } => print_resp(rpc(client.gated(Op::Viewport {
            width,
            height,
            preset,
        }))?),
        Command::Paste { target, text } => {
            print_resp(rpc(client.gated(Op::Paste { target, text }))?)
        }
        Command::System { action, value } => {
            print_resp(rpc(client.gated(Op::System { action, value }))?)
        }
        Command::Events { since, limit } => {
            let poll = client.events(since, limit).map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&poll)?);
        }
        Command::Click { target, delivery } => {
            print_resp(rpc(client.click_with_delivery(target, delivery))?)
        }
        Command::Type {
            target,
            text,
            delivery,
        } => print_resp(rpc(client.type_with_delivery(target, text, delivery))?),
        Command::SetValue { target, value } => print_resp(rpc(client.set_value(target, value))?),
        Command::Key {
            target,
            key,
            delivery,
        } => print_resp(rpc(client.key_with_delivery(target, key, delivery))?),
        Command::Keybinding {
            id,
            scope,
            chord,
            confirm,
            activate,
        } => print_resp(rpc(client.keybinding(id, scope, chord, confirm, activate))?),
        Command::Keybindings => print_resp(rpc(client.keybindings())?),
        Command::Assert {
            id,
            name,
            value,
            role,
            checked,
            exists,
            absent,
            visible,
            in_viewport,
        } => {
            let spec = AssertSpec {
                target: id,
                name,
                value,
                role,
                checked,
                exists: Some(if absent { false } else { exists }),
                visible,
                in_viewport,
            };
            print_resp(rpc(client.assert(spec))?);
        }
        Command::Invoke { name, args } => print_resp(rpc(client.invoke(name, parse_args(&args)?))?),
        Command::Shutdown => print_resp(rpc(client.expect_ok(Op::Shutdown))?),
        Command::Targets
        | Command::Skill
        | Command::Doctor
        | Command::Mcp { .. }
        | Command::Recipe { .. } => {
            unreachable!()
        }
        #[cfg(feature = "instinct")]
        Command::Run { .. } => unreachable!(),
    }
    Ok(())
}

fn rpc<T>(result: std::result::Result<T, String>) -> Result<T> {
    result.map_err(|e| anyhow!("{e}"))
}

fn parse_args(pairs: &[String]) -> Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for pair in pairs {
        let (key, raw) = pair
            .split_once('=')
            .with_context(|| format!("expected KEY=VALUE, got {pair}"))?;
        let value =
            serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.into()));
        map.insert(key.to_string(), value);
    }
    Ok(serde_json::Value::Object(map))
}

fn print_resp(resp: gpui_agent::Response) {
    println!("{}", serde_json::to_string_pretty(&resp).unwrap());
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    #[test]
    fn wait_reply_timeout_covers_the_bound_and_preserves_longer_explicit_values() {
        assert_eq!(
            wait_reply_timeout(Duration::from_secs(8), Some(20_000)),
            Duration::from_secs(22)
        );
        assert_eq!(
            wait_reply_timeout(Duration::from_secs(30), Some(20_000)),
            Duration::from_secs(30)
        );
        assert_eq!(
            wait_reply_timeout(Duration::from_secs(8), None),
            Duration::from_secs(8)
        );
    }

    #[test]
    fn reply_timeout_rejects_zero_during_parsing() {
        assert!(Cli::try_parse_from(["gpui-agent", "--reply-timeout-ms", "0", "hello"]).is_err());
    }

    #[test]
    fn the_skill_names_only_commands_this_cli_has() {
        let text = skill();
        assert!(text.contains(concat!("\nversion: ", env!("CARGO_PKG_VERSION"), "\n")));
        let cmd = Cli::command();
        let known: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
        // The first word of each command cell in the actions table.
        let named = text
            .lines()
            .filter(|line| line.starts_with("| `"))
            .flat_map(|line| line.split('|').nth(1).unwrap_or("").split(", "))
            .filter_map(|cell| cell.trim().strip_prefix('`'))
            .filter_map(|cell| cell.split([' ', '`']).next());
        let mut seen = 0;
        for name in named {
            assert!(known.contains(&name), "the skill names `{name}`");
            seen += 1;
        }
        assert!(seen >= 12, "{seen} commands found in the skill's table");
    }

    #[test]
    fn first_class_commands_are_generic_ops() {
        let cmd = Cli::command();
        let names: Vec<String> = cmd
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        let mut expected = vec![
            "wait",
            "wait-until",
            "targets",
            "skill",
            "doctor",
            "hello",
            "snapshot",
            "screenshot",
            "press",
            "scroll",
            "hover",
            "drag",
            "swipe",
            "long-press",
            "pinch",
            "viewport",
            "paste",
            "system",
            "click",
            "type",
            "set-value",
            "key",
            "keybinding",
            "keybindings",
            "events",
            "assert",
            "invoke",
            "shutdown",
            "mcp",
        ];
        #[cfg(feature = "instinct")]
        expected.push("run");
        expected.push("recipe");
        assert_eq!(names, expected);
        assert!(!names.iter().any(|n| n == "todo"));
    }

    #[test]
    fn smoke_desktop_script_starts_todo_headless() {
        let script = include_str!("../../../scripts/smoke-desktop.sh");
        assert!(
            script.contains("todo-headless"),
            "desktop smoke must start the daemon SoT (ADR-001), not only the GUI:\n{script}"
        );
        assert!(
            script.contains("ADR-001"),
            "script header must cite ADR-001:\n{script}"
        );
        assert!(
            !script.lines().any(|line| {
                let t = line.trim_start();
                t.starts_with("cargo ") && t.contains("--features embedded-host")
            }),
            "must not silently use embedded-host as the only path:\n{script}"
        );
    }

    #[test]
    fn help_does_not_advertise_todo() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            !help
                .lines()
                .any(|line| line.trim_start().starts_with("todo")),
            "help should not list a todo subcommand:\n{help}"
        );
        assert!(
            help.to_ascii_lowercase().contains("experimental"),
            "top-level help should label recipes experimental:\n{help}"
        );
    }

    #[test]
    fn cli_help_does_not_claim_cdp_attach() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            !help.contains("Drive any GPUI Kit app"),
            "help must not claim CDP-like attach to any process:\n{help}"
        );
        assert!(
            help.contains("AgentHost") || help.to_ascii_lowercase().contains("embed"),
            "help must mention AgentHost or embed:\n{help}"
        );
    }

    #[test]
    fn recipe_help_is_experimental_and_json_first() {
        let mut recipe = Cli::command()
            .find_subcommand("recipe")
            .expect("recipe")
            .clone();
        let about = recipe
            .get_about()
            .map(|s| s.to_string())
            .unwrap_or_default();
        assert!(
            about.to_ascii_lowercase().contains("experimental"),
            "recipe about should say experimental: {about}"
        );
        assert!(
            about.to_ascii_lowercase().contains("json"),
            "recipe about should mention JSON: {about}"
        );

        let help = recipe.render_long_help().to_string();
        for name in ["validate", "plan", "run", "resolve"] {
            assert!(help.contains(name), "missing {name} in:\n{help}");
        }
        let validate = recipe.find_subcommand("validate").expect("validate");
        let about = validate
            .get_about()
            .map(|s| s.to_string())
            .unwrap_or_default();
        assert!(
            about.to_ascii_lowercase().contains("experimental"),
            "{about}"
        );
        assert!(about.to_ascii_lowercase().contains("json"), "{about}");
    }

    #[test]
    fn invoke_args_parse_json_or_string() {
        let value = parse_args(&["title=Buy milk".into(), "id=1".into()]).unwrap();
        assert_eq!(value["title"], "Buy milk");
        assert_eq!(value["id"], 1);
    }

    #[test]
    fn assert_bool_flags_do_not_leave_positional_true() {
        let checked_true = Cli::try_parse_from([
            "gpui-agent",
            "assert",
            "--id",
            "page-root",
            "--checked",
            "true",
        ])
        .expect("checked true");
        match checked_true.command {
            Command::Assert {
                id,
                checked,
                exists,
                absent,
                ..
            } => {
                assert_eq!(id, "page-root");
                assert_eq!(checked, Some(true));
                assert!(exists);
                assert!(!absent);
            }
            other => panic!("unexpected {other:?}"),
        }

        let checked_false = Cli::try_parse_from([
            "gpui-agent",
            "assert",
            "--id",
            "page-root",
            "--checked",
            "false",
        ])
        .expect("checked false");
        match checked_false.command {
            Command::Assert { checked, .. } => assert_eq!(checked, Some(false)),
            other => panic!("unexpected {other:?}"),
        }

        let exists_true = Cli::try_parse_from([
            "gpui-agent",
            "assert",
            "--id",
            "page-root",
            "--exists",
            "true",
        ])
        .expect("exists true");
        match exists_true.command {
            Command::Assert { exists, absent, .. } => {
                assert!(exists);
                assert!(!absent);
            }
            other => panic!("unexpected {other:?}"),
        }

        let exists_false = Cli::try_parse_from([
            "gpui-agent",
            "assert",
            "--id",
            "page-root",
            "--exists",
            "false",
        ])
        .expect("exists false");
        match exists_false.command {
            Command::Assert { exists, .. } => assert!(!exists),
            other => panic!("unexpected {other:?}"),
        }

        let absent = Cli::try_parse_from(["gpui-agent", "assert", "--id", "page-root", "--absent"])
            .expect("absent");
        match absent.command {
            Command::Assert { absent, .. } => assert!(absent),
            other => panic!("unexpected {other:?}"),
        }

        let bare_checked =
            Cli::try_parse_from(["gpui-agent", "assert", "--id", "page-root", "--checked"])
                .expect("bare checked");
        match bare_checked.command {
            Command::Assert { checked, .. } => assert_eq!(checked, Some(true)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn assert_and_wait_until_parse_visible_and_in_viewport() {
        let vis = Cli::try_parse_from([
            "gpui-agent",
            "assert",
            "--id",
            "todo-nav",
            "--visible",
            "false",
        ])
        .expect("visible false");
        match vis.command {
            Command::Assert {
                visible,
                in_viewport,
                ..
            } => {
                assert_eq!(visible, Some(false));
                assert!(in_viewport.is_none());
            }
            other => panic!("{other:?}"),
        }

        let wait = Cli::try_parse_from([
            "gpui-agent",
            "wait-until",
            "--timeout-ms",
            "1000",
            "--id",
            "todo-nav",
            "--visible",
            "--in-viewport",
            "false",
        ])
        .expect("wait-until");
        match wait.command {
            Command::WaitUntil {
                timeout_ms,
                id,
                visible,
                in_viewport,
                ..
            } => {
                assert_eq!(timeout_ms, 1000);
                assert_eq!(id, "todo-nav");
                assert_eq!(visible, Some(true));
                assert_eq!(in_viewport, Some(false));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn click_delivery_defaults_to_semantic() {
        let cli = Cli::try_parse_from(["gpui-agent", "click", "todo-add"]).unwrap();
        match cli.command {
            Command::Click { target, delivery } => {
                assert_eq!(target, "todo-add");
                assert_eq!(delivery, DeliveryMode::Semantic);
            }
            other => panic!("unexpected {other:?}"),
        }

        let virt =
            Cli::try_parse_from(["gpui-agent", "click", "--delivery", "virtual", "todo-add"])
                .unwrap();
        match virt.command {
            Command::Click { delivery, .. } => assert_eq!(delivery, DeliveryMode::Virtual),
            other => panic!("unexpected {other:?}"),
        }

        let typed = Cli::try_parse_from([
            "gpui-agent",
            "type",
            "--delivery",
            "virtual",
            "todo-input",
            "Hi",
        ])
        .unwrap();
        match typed.command {
            Command::Type { delivery, text, .. } => {
                assert_eq!(delivery, DeliveryMode::Virtual);
                assert_eq!(text, "Hi");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn keybinding_cli_parses_id_scope_confirm_activate() {
        let fire = Cli::try_parse_from([
            "gpui-agent",
            "keybinding",
            "--id",
            "app.quit",
            "--scope",
            "global",
            "--chord",
            "cmd-q",
            "--confirm",
        ])
        .unwrap();
        match fire.command {
            Command::Keybinding {
                id,
                scope,
                chord,
                confirm,
                activate,
            } => {
                assert_eq!(id, "app.quit");
                assert_eq!(scope, KeybindingScope::Global);
                assert_eq!(chord.as_deref(), Some("cmd-q"));
                assert!(confirm);
                assert!(!activate);
            }
            other => panic!("{other:?}"),
        }

        let focused = Cli::try_parse_from([
            "gpui-agent",
            "keybinding",
            "--id",
            "todo.focus_input",
            "--scope",
            "focused",
            "--activate",
        ])
        .unwrap();
        match focused.command {
            Command::Keybinding {
                scope, activate, ..
            } => {
                assert_eq!(scope, KeybindingScope::Focused);
                assert!(activate);
            }
            other => panic!("{other:?}"),
        }

        let list = Cli::try_parse_from(["gpui-agent", "keybindings"]).unwrap();
        assert!(matches!(list.command, Command::Keybindings));
    }

    #[test]
    fn recipe_subcommands_are_validate_plan_run_resolve() {
        let validate = Cli::try_parse_from(["gpui-agent", "recipe", "validate", "x.json"]).unwrap();
        match validate.command {
            Command::Recipe {
                schema: _,
                action: RecipeCommand::Validate { path },
            } => assert_eq!(path.as_os_str(), "x.json"),
            other => panic!("unexpected {other:?}"),
        }

        let run = Cli::try_parse_from([
            "gpui-agent",
            "recipe",
            "run",
            "x.wants",
            "--set",
            "title=Milk",
            "--yes",
        ])
        .unwrap();
        match run.command {
            Command::Recipe {
                schema: _,
                action: RecipeCommand::Run { yes, set, .. },
            } => {
                assert!(yes);
                assert_eq!(set, vec!["title=Milk"]);
            }
            other => panic!("unexpected {other:?}"),
        }

        let with_schema = Cli::try_parse_from([
            "gpui-agent",
            "recipe",
            "validate",
            "x.json",
            "--schema",
            "examples/schemas/todo.json",
        ])
        .unwrap();
        match with_schema.command {
            Command::Recipe { schema, .. } => {
                assert_eq!(
                    schema.as_slice(),
                    [std::path::PathBuf::from("examples/schemas/todo.json")]
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn mcp_schema_flag_parses_like_recipe() {
        let parsed = Cli::try_parse_from([
            "gpui-agent",
            "mcp",
            "--schema",
            "examples/schemas/todo.json",
        ]);
        assert!(
            parsed.is_ok(),
            "mcp --schema must parse like recipe --schema: {:?}",
            parsed.as_ref().err().map(|e| e.to_string())
        );
        let help = parsed.unwrap();
        match help.command {
            Command::Mcp { schema } => {
                assert_eq!(
                    schema.as_slice(),
                    [std::path::PathBuf::from("examples/schemas/todo.json")]
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn screenshot_and_recipe_screenshot_dir_parse() {
        let shot = Cli::try_parse_from([
            "gpui-agent",
            "screenshot",
            "--out",
            "artifacts/steps/001-wait.png",
        ])
        .unwrap();
        match shot.command {
            Command::Screenshot {
                out,
                mode,
                target,
                max_height_px,
                ..
            } => {
                assert_eq!(out.as_os_str(), "artifacts/steps/001-wait.png");
                assert!(mode.is_viewport());
                assert!(target.is_none());
                assert!(max_height_px.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }

        let scrolled = Cli::try_parse_from([
            "gpui-agent",
            "screenshot",
            "--out",
            "tall.png",
            "--mode",
            "scrolled",
            "--target",
            "todo-list-scroll",
            "--max-height-px",
            "4096",
        ])
        .unwrap();
        match scrolled.command {
            Command::Screenshot {
                out,
                mode,
                target,
                max_height_px,
                ..
            } => {
                assert_eq!(out.as_os_str(), "tall.png");
                assert!(mode.is_scrolled());
                assert_eq!(target.as_deref(), Some("todo-list-scroll"));
                assert_eq!(max_height_px, Some(4096));
            }
            other => panic!("unexpected {other:?}"),
        }

        let run = Cli::try_parse_from([
            "gpui-agent",
            "recipe",
            "run",
            "x.json",
            "--screenshot-dir",
            "artifacts/steps",
            "--screenshot-flagged",
        ])
        .unwrap();
        match run.command {
            Command::Recipe {
                schema: _,
                action:
                    RecipeCommand::Run {
                        screenshot_dir,
                        screenshot_flagged,
                        ..
                    },
            } => {
                assert_eq!(
                    screenshot_dir.as_deref(),
                    Some(std::path::Path::new("artifacts/steps"))
                );
                assert!(screenshot_flagged);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn invoke_without_args_is_empty_object() {
        let cli = Cli::try_parse_from(["gpui-agent", "invoke", "demo.ping"]).unwrap();
        match cli.command {
            Command::Invoke { name, args } => {
                assert_eq!(name, "demo.ping");
                assert!(args.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cli_addr_must_be_loopback() {
        let remote =
            Cli::try_parse_from(["gpui-agent", "--addr", "8.8.8.8:17421", "hello"]).unwrap();
        assert!(gpui_agent::ensure_loopback(remote.addr.unwrap()).is_err());
        assert!(
            gpui_agent::authorize_client(
                remote.addr.unwrap(),
                remote.token.as_deref(),
                remote.allow_remote
            )
            .is_err()
        );

        let local = Cli::try_parse_from(["gpui-agent", "hello"]).unwrap();
        assert!(local.addr.is_none());
        assert!(local.connect.is_none());
        let fallback = gpui_agent::default_addr();
        assert!(gpui_agent::ensure_loopback(fallback).is_ok());
        assert!(gpui_agent::authorize_client(fallback, None, local.allow_remote).is_ok());
    }

    #[test]
    fn cli_remote_requires_allow_and_token() {
        let denied = Cli::try_parse_from([
            "gpui-agent",
            "--addr",
            "8.8.8.8:17421",
            "--token",
            "lab",
            "hello",
        ])
        .unwrap();
        assert!(!denied.allow_remote);
        assert!(
            gpui_agent::authorize_client(
                denied.addr.unwrap(),
                denied.token.as_deref(),
                denied.allow_remote
            )
            .is_err()
        );

        let no_token = Cli::try_parse_from([
            "gpui-agent",
            "--addr",
            "8.8.8.8:17421",
            "--allow-remote",
            "hello",
        ])
        .unwrap();
        assert!(no_token.allow_remote);
        assert!(
            gpui_agent::authorize_client(
                no_token.addr.unwrap(),
                take_non_empty_token(no_token.token).as_deref(),
                no_token.allow_remote
            )
            .is_err()
        );

        let allowed = Cli::try_parse_from([
            "gpui-agent",
            "--addr",
            "8.8.8.8:17421",
            "--allow-remote",
            "--token",
            "lab",
            "hello",
        ])
        .unwrap();
        assert!(allowed.allow_remote);
        assert!(
            gpui_agent::authorize_client(
                allowed.addr.unwrap(),
                take_non_empty_token(allowed.token).as_deref(),
                allowed.allow_remote
            )
            .is_ok()
        );
    }

    #[test]
    fn empty_cli_token_is_treated_as_unset() {
        assert_eq!(take_non_empty_token(None), None);
        assert_eq!(take_non_empty_token(Some(String::new())), None);
        assert_eq!(
            take_non_empty_token(Some("secret".into())),
            Some("secret".into())
        );
    }

    #[test]
    fn recipe_run_and_mcp_require_non_empty_token() {
        assert!(require_recipe_mcp_token(None).is_err());
        assert!(require_recipe_mcp_token(Some("")).is_err());
        let err = require_recipe_mcp_token(None).unwrap_err().to_string();
        assert!(err.contains("GPUI_AGENT_TOKEN"), "{err}");
        assert!(err.contains("recipe run"), "{err}");
        assert!(err.contains("mcp"), "{err}");
        assert_eq!(require_recipe_mcp_token(Some("secret")).unwrap(), "secret");
    }

    #[test]
    fn connect_flag_parses_bundle_id() {
        let cli =
            Cli::try_parse_from(["gpui-agent", "--connect", "dev.example.nativechat", "hello"])
                .unwrap();
        assert_eq!(cli.connect.as_deref(), Some("dev.example.nativechat"));
        assert!(cli.addr.is_none());
        let by_exe = Cli::try_parse_from([
            "gpui-agent",
            "--connect",
            "eBIRForms",
            "invoke",
            "profile.list",
            "--arg",
            "x=1",
        ])
        .unwrap();
        assert_eq!(by_exe.connect.as_deref(), Some("eBIRForms"));
    }

    #[test]
    fn help_mentions_recipe_mcp_token() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            help.contains("GPUI_AGENT_TOKEN") && help.to_ascii_lowercase().contains("recipe run"),
            "{help}"
        );
    }
}
