mod doctor;
#[cfg(feature = "instinct")]
mod instinct_wiring;
mod mcp;
mod recipe_cmd;
mod refs;
#[cfg(feature = "instinct")]
mod run_cmd;

use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use gpui_agent::CallFailure;
use gpui_agent::client::AgentClient;
use gpui_agent::discovery::NamedSession;
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
    #[arg(long, env = "GPUI_AGENT_ADDR", global = true)]
    addr: Option<SocketAddr>,
    /// Bundle id or executable name of the app to drive. The CLI finds that
    /// app's AgentHost and remembers it; `--connect last` reuses that choice.
    /// A command with neither `--addr` nor `--connect` does not: another agent
    /// on this Mac may have picked a different app.
    #[arg(long, env = "GPUI_AGENT_CONNECT", global = true)]
    connect: Option<String>,
    /// Reuse a saved address and token. Create it with `attach`.
    #[arg(long, env = "GPUI_AGENT_SESSION", global = true)]
    session: Option<String>,
    /// Shared secret; must match `GPUI_AGENT_TOKEN` on the host when the host
    /// has one. With `--session`, the saved token precedes the environment
    /// value; this command-line flag overrides it. Required for `recipe run` and `mcp`.
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
    /// Skip the read-back after an action (`click`, `type`, `set-value`,
    /// `key`, `press`, `paste`, `long-press`). By default the CLI snapshots
    /// the target before and after, reports it as `after`, and sets
    /// `delivery_status` to `delivered_verified` when it changed.
    #[arg(long, global = true)]
    no_read_back: bool,
    /// When `click`, `type`, `set-value`, `key` or `press` names its target
    /// by a ref (`@e2`), send the fingerprint `snapshot --refs` saved for it
    /// as `--expect-fingerprint`: the host refuses with `stale_target`, and
    /// does nothing, when the node changed since. Opt-in because the
    /// fingerprint covers the value: after `type @e2 …`, `key @e2 Enter`
    /// is stale until the next `snapshot --refs`.
    #[arg(long, global = true)]
    guard: bool,
    /// After an action, wait until a node with this id is in the tree, then
    /// print the new tree in compact form as `compact`.
    #[arg(
        long,
        global = true,
        value_name = "ID",
        conflicts_with = "wait_for_gone"
    )]
    wait_for: Option<String>,
    /// After an action, wait until no node with this id is in the tree,
    /// then print the new tree in compact form as `compact`.
    #[arg(long, global = true, value_name = "ID")]
    wait_for_gone: Option<String>,
    /// Bound of `--wait-for` / `--wait-for-gone`, in milliseconds.
    #[arg(long, global = true, default_value_t = 5000, value_name = "MS")]
    wait_timeout_ms: u64,
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
the last pick. Use `--session <name> attach <target>` once, then pass
`--session <name>` for later commands. `gpui-agent sessions` lists saved
sessions without printing tokens; `gpui-agent --session <name> detach`
removes one. With `--session`, the saved token is used before
`GPUI_AGENT_TOKEN`; a command-line `--token` overrides it. Every command that
discovery routes names its app on stderr.";

#[derive(Debug, Subcommand)]
enum Command {
    /// Save a host address and token under a reusable session name.
    Attach { target: String },
    /// Remove the named session.
    Detach,
    /// List saved sessions without revealing their tokens.
    Sessions,
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
        /// Add each node's fingerprint, for `--expect-fingerprint`: the
        /// `fingerprints` map in JSON, `fp=…` on each `--refs` line.
        #[arg(long)]
        fingerprints: bool,
        /// Keep this many levels (1 = the roots alone). A node whose
        /// children were cut shows `children_count`.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
        depth: Option<u64>,
        /// Keep only the subtree of this node (an id or a ref).
        #[arg(long, value_name = "ID")]
        root: Option<String>,
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
        /// Refuse with `stale_target`, without pressing, unless the target's
        /// fingerprint (`snapshot --fingerprints`) is still this value.
        #[arg(long, value_name = "FINGERPRINT", requires = "target")]
        expect_fingerprint: Option<u64>,
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
    Paste {
        target: String,
        #[arg(required_unless_present = "stdin")]
        text: Option<String>,
        /// Read the text from stdin instead (see `type --stdin`).
        #[arg(long, conflicts_with = "text")]
        stdin: bool,
    },
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
        /// Refuse with `stale_target`, without clicking, unless the target's
        /// fingerprint (`snapshot --fingerprints`) is still this value.
        #[arg(long, value_name = "FINGERPRINT")]
        expect_fingerprint: Option<u64>,
    },
    /// Append text to an editable widget.
    Type {
        target: String,
        #[arg(required_unless_present = "stdin")]
        text: Option<String>,
        /// Read the text from stdin instead: all of it, less one trailing
        /// line ending. A secret piped in stays out of the process list
        /// and the shell history.
        #[arg(long, conflicts_with = "text")]
        stdin: bool,
        #[arg(long, default_value = "semantic")]
        delivery: DeliveryMode,
        /// Refuse with `stale_target`, without typing, unless the target's
        /// fingerprint (`snapshot --fingerprints`) is still this value.
        #[arg(long, value_name = "FINGERPRINT")]
        expect_fingerprint: Option<u64>,
    },
    /// Replace the value of an editable widget.
    SetValue {
        target: String,
        #[arg(required_unless_present = "stdin")]
        value: Option<String>,
        /// Read the value from stdin instead (see `type --stdin`).
        #[arg(long, conflicts_with = "value")]
        stdin: bool,
        /// Refuse with `stale_target`, without setting it, unless the
        /// target's fingerprint (`snapshot --fingerprints`) is still this value.
        #[arg(long, value_name = "FINGERPRINT")]
        expect_fingerprint: Option<u64>,
    },
    /// Send a key (`Enter`, `Backspace`) to a widget. No modifiers (use `keybinding`).
    Key {
        target: String,
        key: String,
        #[arg(long, default_value = "semantic")]
        delivery: DeliveryMode,
        /// Refuse with `stale_target`, without sending it, unless the
        /// target's fingerprint (`snapshot --fingerprints`) is still this value.
        #[arg(long, value_name = "FINGERPRINT")]
        expect_fingerprint: Option<u64>,
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
    stamp_skill(include_str!("../skill/SKILL.md"))
}

fn stamp_skill(text: &str) -> String {
    text.replace("\r\n", "\n").replacen(
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
            // A failed request also prints its envelope on stdout, so a
            // caller parsing JSON gets the code and delivery status.
            if let Some(failure) = err.downcast_ref::<CallFailure>() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&failure.envelope()).unwrap()
                );
            }
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn take_non_empty_token(token: Option<String>) -> Option<String> {
    token.filter(|s| !s.is_empty())
}

#[derive(Clone, Copy)]
struct CliSources {
    token: bool,
    addr: bool,
    connect: bool,
}

fn parse_cli_from<I, T>(args: I) -> (Cli, CliSources)
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let matches = Cli::command().get_matches_from(args);
    let sources = CliSources {
        token: matches.value_source("token") == Some(ValueSource::CommandLine),
        addr: matches.value_source("addr") == Some(ValueSource::CommandLine),
        connect: matches.value_source("connect") == Some(ValueSource::CommandLine),
    };
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|err| err.exit());
    (cli, sources)
}

fn require_session_name<'a>(cli: &'a Cli, command: &str) -> Result<&'a str> {
    cli.session
        .as_deref()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("{command} requires --session <name> or GPUI_AGENT_SESSION"))
}

fn check_cli_target_selector_conflicts(cli: &Cli, sources: CliSources) -> Result<()> {
    if cli.session.is_some() && sources.addr {
        return Err(anyhow!("--session cannot be used with command-line --addr"));
    }
    if cli.session.is_some() && sources.connect {
        return Err(anyhow!(
            "--session cannot be used with command-line --connect"
        ));
    }
    Ok(())
}

fn resolve_cli_target(
    root: &Path,
    cli: &Cli,
    sources: CliSources,
) -> Result<(gpui_agent::ClientTarget, Option<NamedSession>)> {
    check_cli_target_selector_conflicts(cli, sources)?;
    if let Some(name) = cli.session.as_deref() {
        let session = gpui_agent::discovery::load_named_session(root, name)?;
        let addr = session
            .addr
            .parse::<SocketAddr>()
            .with_context(|| format!("session {name} has invalid address '{}'", session.addr))?;
        return Ok((
            gpui_agent::ClientTarget {
                addr,
                app: session.app.clone(),
                connect: None,
            },
            Some(session),
        ));
    }
    Ok((
        gpui_agent::resolve_client_target(root, cli.addr, cli.connect.as_deref())?,
        None,
    ))
}

fn attach_session(cli: &Cli, target: &str) -> Result<()> {
    let name = require_session_name(cli, "attach")?;
    let root = gpui_agent::runtime_root();
    let addr = match target.parse::<SocketAddr>() {
        Ok(addr) => addr,
        Err(_) => gpui_agent::discovery::resolve_id(&root, target)?
            .addr
            .parse::<SocketAddr>()
            .with_context(|| format!("discovery returned an invalid address for {target}"))?,
    };
    let token = take_non_empty_token(cli.token.clone());
    let request_timeout = cli.request_timeout_ms.map(Duration::from_millis);
    gpui_agent::authorize_client(addr, token.as_deref(), cli.allow_remote)
        .with_context(|| format!("refusing agent address {addr}"))?;

    let mut client = connect(addr, request_timeout);
    if let Some(token) = &token {
        client = client.with_token(token.clone());
    }
    let hello = rpc(client.expect_ok(Op::Hello))?
        .hello
        .ok_or_else(|| anyhow!("the host returned no hello info"))?;
    let display = hello
        .framework
        .clone()
        .filter(|framework| !framework.is_empty())
        .unwrap_or_else(|| hello.app.clone());
    let session = NamedSession {
        name: name.to_string(),
        addr: addr.to_string(),
        token,
        allow_remote: cli.allow_remote,
        app: Some(hello.app),
    };
    gpui_agent::discovery::save_named_session(&root, &session).context("saving named session")?;
    println!("session {}: {} ({display})", session.name, session.addr);
    Ok(())
}

fn list_sessions() -> Result<()> {
    let sessions = gpui_agent::discovery::list_named_sessions(&gpui_agent::runtime_root())?;
    let summaries: Vec<_> = sessions
        .into_iter()
        .map(|session| {
            serde_json::json!({
                "name": session.name,
                "addr": session.addr,
                "app": session.app,
                "token": if session.token.is_some() { "yes" } else { "no" },
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&summaries)?);
    Ok(())
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
    let (mut cli, mut sources) = parse_cli_from(std::env::args_os());
    requested_wait(&cli)?;
    if matches!(&cli.command, Command::Attach { .. } | Command::Detach) {
        check_cli_target_selector_conflicts(&cli, sources)?;
    }
    if matches!(cli.command, Command::Skill) {
        print!("{}", skill());
        return Ok(());
    }
    if matches!(cli.command, Command::Targets) {
        let (targets, discovery_error) = doctor::targets();
        println!("{}", serde_json::to_string_pretty(&targets)?);
        if let Some(error) = discovery_error {
            return Err(anyhow!(error));
        }
        return Ok(());
    }
    if matches!(cli.command, Command::Sessions) {
        return list_sessions();
    }
    if let Command::Attach { target } = &cli.command {
        return attach_session(&cli, target);
    }
    if matches!(cli.command, Command::Detach) {
        let name = require_session_name(&cli, "detach")?;
        let removed =
            gpui_agent::discovery::remove_named_session(&gpui_agent::runtime_root(), name)?;
        println!(
            "session {name}: {}",
            if removed { "detached" } else { "not found" }
        );
        return Ok(());
    }
    if matches!(
        &cli.command,
        Command::Recipe {
            action: RecipeCommand::Validate { .. }
                | RecipeCommand::Plan { .. }
                | RecipeCommand::Resolve { .. },
            ..
        }
    ) {
        let Command::Recipe { schema, action } = cli.command else {
            unreachable!();
        };
        return recipe_cmd::run(None, action, &schema);
    }
    let root = gpui_agent::runtime_root();
    if matches!(&cli.command, Command::Doctor) {
        check_cli_target_selector_conflicts(&cli, sources)?;
    }
    let (target, session) = resolve_cli_target(&root, &cli, sources)?;
    // Say which app discovery picked before acting on it, so a command that lands in the wrong
    // window is visible. Stderr keeps stdout (and `mcp`'s protocol) clean.
    // `@e3` stands for a node of the last `snapshot --refs` of this host.
    let guard_target_is_ref = match &cli.command {
        Command::Click { target, .. }
        | Command::Type { target, .. }
        | Command::SetValue { target, .. }
        | Command::Key { target, .. } => refs::is_ref(target),
        Command::Press {
            target: Some(target),
            ..
        } => refs::is_ref(target),
        _ => false,
    };
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let mut used_refs = Vec::new();
    if args
        .iter()
        .skip(1)
        .any(|arg| arg.to_str().is_some_and(refs::has_ref))
    {
        used_refs = refs::resolve(&root, target.addr, &mut args[1..]).map_err(|e| anyhow!(e))?;
        (cli, sources) = parse_cli_from(args);
    }
    let addr = target.addr;
    let wait = requested_wait(&cli)?;
    if let Some(session) = &session {
        eprintln!("gpui-agent: session {} -> {addr}", session.name);
    } else if target.app.is_some() {
        eprintln!("gpui-agent: {}", target.describe());
    }
    let token = take_non_empty_token(if sources.token {
        cli.token
    } else {
        session
            .as_ref()
            .and_then(|session| session.token.clone())
            .or(cli.token)
    });
    let allow_remote = cli.allow_remote || session.is_some_and(|session| session.allow_remote);
    let request_timeout = cli.request_timeout_ms.map(Duration::from_millis);
    gpui_agent::authorize_client(addr, token.as_deref(), allow_remote)
        .with_context(|| format!("refusing agent address {addr}"))?;

    if matches!(cli.command, Command::Doctor) {
        let mut client = connect(addr, request_timeout);
        if let Some(token) = token.clone() {
            client = client.with_token(token);
        }
        if doctor::doctor(addr, client, token.is_some(), &root) {
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

    let guarded = cli.guard;
    let read_back = !cli.no_read_back;
    // The fingerprint `snapshot --refs` saved for `target`, when `--guard`
    // is on and `target` was given as a ref.
    let guard = |target: Option<&str>, explicit: Option<u64>| -> Result<Option<u64>> {
        if explicit.is_some() || !guarded || !guard_target_is_ref {
            return Ok(explicit);
        }
        let Some(saved) = target.and_then(|id| used_refs.iter().find(|saved| saved.id == id))
        else {
            return Ok(None);
        };
        saved.fp.map(Some).ok_or_else(|| {
            anyhow!(
                "--guard: no fingerprint saved for `{}`; run `gpui-agent snapshot --refs` again (the host may not report fingerprints)",
                saved.id
            )
        })
    };
    let finish = |client: &mut AgentClient, resp: gpui_agent::Response| -> Result<()> {
        match &wait {
            None => print_resp(resp),
            Some((wait, timeout_ms)) => {
                let reply_timeout = request_timeout.unwrap_or_else(|| client.timeout());
                client.set_timeout(wait_reply_timeout(reply_timeout, Some(*timeout_ms)));
                let waited = client.wait_after(wait, *timeout_ms);
                client.set_timeout(reply_timeout);
                let compact = call(waited.map_err(|failure| CallFailure {
                    delivery_status: resp.delivery_status,
                    ..failure
                }))?;
                let out = WithCompact {
                    resp: &resp,
                    compact: &compact,
                };
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        }
        Ok(())
    };
    match cli.command {
        Command::Wait { timeout_ms } => {
            let reply_timeout = request_timeout.unwrap_or_else(|| client.timeout());
            client.set_timeout(wait_reply_timeout(reply_timeout, timeout_ms));
            print_resp(call(client.call(&Op::Wait { timeout_ms }))?)
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
            print_resp(call(client.call(&Op::WaitUntil { timeout_ms, spec }))?)
        }
        Command::Hello => print_resp(call(client.call(&Op::Hello))?),
        Command::Snapshot {
            pretty,
            refs,
            fingerprints,
            depth,
            root,
        } => {
            // `--refs` always asks for fingerprints, for `--guard`; a host
            // that has none answers without them.
            let resp = call(client.call(&Op::Snapshot {
                since: None,
                format: None,
                fingerprints: (refs || fingerprints).then_some(true),
            }))?;
            let mut options = gpui_agent::OutlineOptions::new();
            options.root = root;
            options.depth = depth.map(|n| usize::try_from(n).unwrap_or(usize::MAX));
            print_snapshot(resp, &options, refs, pretty, fingerprints, addr)?;
        }
        Command::Screenshot {
            out,
            mode,
            target,
            max_height_px,
            inline,
        } => {
            if inline {
                let png = call(client.screenshot_bytes_call(mode, target, max_height_px))?;
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
            print_resp(call(client.call(&Op::Screenshot {
                path: Some(out.to_string_lossy().into_owned()),
                mode,
                target,
                max_height_px,
                inline: false,
            }))?)
        }
        Command::Press {
            target,
            x,
            y,
            expect_fingerprint,
        } => {
            let expect_fingerprint = guard(target.as_deref(), expect_fingerprint)?;
            let resp = call(client.act(
                &Op::Press {
                    target,
                    x,
                    y,
                    expect_fingerprint,
                },
                read_back,
            ))?;
            finish(&mut client, resp)?
        }
        Command::Scroll {
            target,
            x,
            y,
            dx,
            dy,
        } => {
            let resp = call(client.call(&Op::Scroll {
                target,
                x,
                y,
                dx,
                dy,
            }))?;
            finish(&mut client, resp)?
        }
        Command::Hover { target, x, y } => {
            let resp = call(client.call(&Op::Hover { target, x, y }))?;
            finish(&mut client, resp)?
        }
        Command::Drag {
            target,
            from,
            to,
            x,
            y,
            dx,
            dy,
        } => {
            let resp = call(client.call(&Op::Drag {
                from,
                to,
                target,
                x,
                y,
                dx,
                dy,
            }))?;
            finish(&mut client, resp)?
        }
        Command::Swipe {
            direction,
            target,
            distance,
        } => {
            let resp = call(client.call(&Op::Swipe {
                target,
                direction,
                distance,
            }))?;
            finish(&mut client, resp)?
        }
        Command::LongPress {
            target,
            x,
            y,
            duration_ms,
        } => {
            let resp = call(client.act(
                &Op::LongPress {
                    target,
                    x,
                    y,
                    duration_ms,
                },
                read_back,
            ))?;
            finish(&mut client, resp)?
        }
        Command::Pinch { target, scale } => {
            let resp = call(client.call(&Op::Pinch { target, scale }))?;
            finish(&mut client, resp)?
        }
        Command::Viewport {
            width,
            height,
            preset,
        } => {
            let resp = call(client.call(&Op::Viewport {
                width,
                height,
                preset,
            }))?;
            finish(&mut client, resp)?
        }
        Command::Paste {
            target,
            text,
            stdin,
        } => {
            let text = text_arg(text, stdin)?;
            let resp = call(client.act(&Op::Paste { target, text }, read_back))?;
            finish(&mut client, resp)?
        }
        Command::System { action, value } => {
            let resp = call(client.call(&Op::System { action, value }))?;
            finish(&mut client, resp)?
        }
        Command::Events { since, limit } => {
            let poll = rpc(client.events(since, limit))?;
            println!("{}", serde_json::to_string_pretty(&poll)?);
        }
        Command::Click {
            target,
            delivery,
            expect_fingerprint,
        } => {
            let expect_fingerprint = guard(Some(target.as_str()), expect_fingerprint)?;
            let resp = call(client.act(
                &Op::Click {
                    target,
                    delivery,
                    expect_fingerprint,
                },
                read_back,
            ))?;
            finish(&mut client, resp)?
        }
        Command::Type {
            target,
            text,
            stdin,
            delivery,
            expect_fingerprint,
        } => {
            let text = text_arg(text, stdin)?;
            let expect_fingerprint = guard(Some(target.as_str()), expect_fingerprint)?;
            let resp = call(client.act(
                &Op::Type {
                    target,
                    text,
                    delivery,
                    expect_fingerprint,
                },
                read_back,
            ))?;
            finish(&mut client, resp)?
        }
        Command::SetValue {
            target,
            value,
            stdin,
            expect_fingerprint,
        } => {
            let value = text_arg(value, stdin)?;
            let expect_fingerprint = guard(Some(target.as_str()), expect_fingerprint)?;
            let resp = call(client.act(
                &Op::SetValue {
                    target,
                    value,
                    expect_fingerprint,
                },
                read_back,
            ))?;
            finish(&mut client, resp)?
        }
        Command::Key {
            target,
            key,
            delivery,
            expect_fingerprint,
        } => {
            let expect_fingerprint = guard(Some(target.as_str()), expect_fingerprint)?;
            let resp = call(client.act(
                &Op::Key {
                    target,
                    key,
                    delivery,
                    expect_fingerprint,
                },
                read_back,
            ))?;
            finish(&mut client, resp)?
        }
        Command::Keybinding {
            id,
            scope,
            chord,
            confirm,
            activate,
        } => {
            let resp = call(client.call(&Op::Keybinding {
                binding: id,
                chord,
                scope,
                confirm,
                activate,
            }))?;
            finish(&mut client, resp)?
        }
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
            print_resp(call(client.call(&Op::Assert { spec }))?);
        }
        Command::Invoke { name, args } => {
            let resp = call(client.call(&Op::Invoke {
                name,
                args: parse_args(&args)?,
            }))?;
            finish(&mut client, resp)?
        }
        Command::Shutdown => print_resp(call(client.call(&Op::Shutdown))?),
        Command::Targets
        | Command::Skill
        | Command::Attach { .. }
        | Command::Detach
        | Command::Sessions
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

/// A request that failed with only a text: classify it, so `main` prints
/// its envelope (no delivery status is known here).
fn rpc<T>(result: std::result::Result<T, String>) -> Result<T> {
    result.map_err(|e| anyhow::Error::new(CallFailure::from_message(e)))
}

/// A typed request: its [`CallFailure`] reaches `main` intact.
fn call<T>(result: std::result::Result<T, CallFailure>) -> Result<T> {
    result.map_err(anyhow::Error::new)
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

/// `--wait-for` / `--wait-for-gone` with their bound, for an action.
fn requested_wait(cli: &Cli) -> Result<Option<(gpui_agent::WaitFor, u64)>> {
    let wait = match (&cli.wait_for, &cli.wait_for_gone) {
        (Some(id), _) => gpui_agent::WaitFor::Present(id.clone()),
        (None, Some(id)) => gpui_agent::WaitFor::Gone(id.clone()),
        (None, None) => return Ok(None),
    };
    if !cli.command.is_action() {
        return Err(anyhow!(
            "--wait-for and --wait-for-gone follow an action command (click, type, set-value, key, press, paste, long-press, scroll, hover, drag, swipe, pinch, viewport, system, keybinding, invoke)"
        ));
    }
    Ok(Some((wait, cli.wait_timeout_ms)))
}

/// The text of `type` / `set-value` / `paste`: the argument, or with
/// `--stdin` all of stdin (clap requires exactly one).
fn text_arg(text: Option<String>, stdin: bool) -> Result<String> {
    match text {
        Some(text) if !stdin => Ok(text),
        _ => read_text(std::io::stdin().lock()),
    }
}

/// All of `input`, less one trailing line ending: the one `echo` and a
/// heredoc add, which would otherwise be typed into the field.
fn read_text(mut input: impl std::io::Read) -> Result<String> {
    let mut text = String::new();
    input
        .read_to_string(&mut text)
        .context("--stdin: reading the text from stdin")?;
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    Ok(text)
}

impl Command {
    /// Commands that act on the app, and so may take `--wait-for`.
    fn is_action(&self) -> bool {
        matches!(
            self,
            Command::Press { .. }
                | Command::Scroll { .. }
                | Command::Hover { .. }
                | Command::Drag { .. }
                | Command::Swipe { .. }
                | Command::LongPress { .. }
                | Command::Pinch { .. }
                | Command::Viewport { .. }
                | Command::Paste { .. }
                | Command::System { .. }
                | Command::Click { .. }
                | Command::Type { .. }
                | Command::SetValue { .. }
                | Command::Key { .. }
                | Command::Keybinding { .. }
                | Command::Invoke { .. }
        )
    }
}

/// `snapshot`: the reply cut to `options`, printed as JSON, the bare tree
/// (`pretty`) or a ref outline (`refs`, whose table is saved for `addr`).
fn print_snapshot(
    mut resp: gpui_agent::Response,
    options: &gpui_agent::OutlineOptions,
    refs: bool,
    pretty: bool,
    fingerprints: bool,
    addr: SocketAddr,
) -> Result<()> {
    if options.is_full() && !refs && !pretty {
        print_resp(resp);
        return Ok(());
    }
    if !options.is_full() {
        resp.rev = None;
    }
    let tree = resp
        .tree
        .take()
        .ok_or_else(|| anyhow!("the reply carried no tree"))?;
    let outline = gpui_agent::outline(&tree, options).map_err(|err| {
        anyhow::Error::new(CallFailure::with_code(
            err,
            gpui_agent::error_code::BAD_REQUEST,
            None,
        ))
    })?;
    let mut fps = resp.fingerprints.take().unwrap_or_default();
    outline.retain_fingerprints(&mut fps);
    if refs {
        let (text, table) = refs::render(&outline, &fps, fingerprints);
        refs::save(&gpui_agent::runtime_root(), addr, &table).context("saving the ref table")?;
        print!("{text}");
        return Ok(());
    }
    if pretty && !fingerprints {
        println!("{}", serde_json::to_string_pretty(&outline)?);
        return Ok(());
    }
    if pretty {
        let out = serde_json::json!({ "tree": outline, "fingerprints": fps });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if fingerprints {
        resp.fingerprints = Some(fps);
    }
    let out = WithTree {
        resp: &resp,
        tree: &outline,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// A reply whose tree was cut to an outline: it prints like the reply,
/// with the outline as `tree`.
#[derive(serde::Serialize)]
pub(crate) struct WithTree<'a> {
    #[serde(flatten)]
    pub resp: &'a gpui_agent::Response,
    pub tree: &'a gpui_agent::Outline,
}

/// An action's reply after `--wait-for`: the tree it waited for, in
/// compact form, rides as `compact`.
#[derive(serde::Serialize)]
pub(crate) struct WithCompact<'a> {
    #[serde(flatten)]
    pub resp: &'a gpui_agent::Response,
    pub compact: &'a str,
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
        assert!(
            text.lines()
                .any(|line| line == concat!("version: ", env!("CARGO_PKG_VERSION")))
        );
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
    fn the_skill_documents_the_token_saving_options() {
        let text = skill();
        let cmd = Cli::command();
        let snapshot = cmd.find_subcommand("snapshot").unwrap();
        let flag = |cmd: &clap::Command, long: &str| {
            cmd.get_arguments().any(|a| a.get_long() == Some(long))
        };
        for long in ["depth", "root", "fingerprints"] {
            assert!(flag(snapshot, long), "snapshot --{long}");
            assert!(
                text.contains(&format!("--{long}")),
                "the skill omits --{long}"
            );
        }
        for long in ["guard", "wait-for", "wait-for-gone", "wait-timeout-ms"] {
            assert!(flag(&cmd, long), "--{long}");
            assert!(
                text.contains(&format!("`--{long}")),
                "the skill omits --{long}"
            );
        }
        for action in ["click", "type", "set-value", "key", "press"] {
            assert!(
                flag(cmd.find_subcommand(action).unwrap(), "expect-fingerprint"),
                "{action}"
            );
        }
        assert!(text.contains("--expect-fingerprint"));
        for arg in [
            "depth",
            "root",
            "wait_for",
            "wait_for_gone",
            "wait_timeout_ms",
            "expect_fingerprint",
        ] {
            assert!(
                text.contains(&format!("`{arg}`")),
                "the skill omits MCP {arg}"
            );
        }
    }

    #[test]
    fn skill_version_stamp_accepts_lf_and_crlf() {
        for text in [
            "---\nname: demo\n---\nbody\n",
            "---\r\nname: demo\r\n---\r\nbody\r\n",
        ] {
            assert_eq!(
                stamp_skill(text),
                concat!(
                    "---\nname: demo\nversion: ",
                    env!("CARGO_PKG_VERSION"),
                    "\n---\nbody\n"
                )
            );
        }
    }

    #[test]
    fn first_class_commands_are_generic_ops() {
        let cmd = Cli::command();
        let names: Vec<String> = cmd
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        let mut expected = vec![
            "attach",
            "detach",
            "sessions",
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
            "desktop smoke must start the daemon SoT, not only the GUI:\n{script}"
        );
        assert!(
            script.contains("source of truth"),
            "script header must name todo-headless as the source of truth:\n{script}"
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
            Command::Click {
                target,
                delivery,
                expect_fingerprint,
            } => {
                assert_eq!(target, "todo-add");
                assert_eq!(delivery, DeliveryMode::Semantic);
                assert_eq!(expect_fingerprint, None);
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
                assert_eq!(text.as_deref(), Some("Hi"));
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
    fn session_conflicts_with_addr_and_connect() {
        let (addr_cli, addr_sources) = parse_cli_from([
            "gpui-agent",
            "--session",
            "work",
            "--addr",
            "127.0.0.1:9",
            "hello",
        ]);
        let addr_error = match resolve_cli_target(Path::new("unused"), &addr_cli, addr_sources) {
            Ok(_) => panic!("--session and command-line --addr should conflict"),
            Err(error) => error.to_string(),
        };
        assert!(addr_error.contains("command-line --addr"), "{addr_error}");

        let (connect_cli, connect_sources) = parse_cli_from([
            "gpui-agent",
            "--session",
            "work",
            "--connect",
            "demo",
            "hello",
        ]);
        let connect_error =
            match resolve_cli_target(Path::new("unused"), &connect_cli, connect_sources) {
                Ok(_) => panic!("--session and command-line --connect should conflict"),
                Err(error) => error.to_string(),
            };
        assert!(
            connect_error.contains("command-line --connect"),
            "{connect_error}"
        );
    }

    #[test]
    fn attach_requires_session() {
        let missing = Cli::try_parse_from(["gpui-agent", "attach", "127.0.0.1:17421"]).unwrap();
        let err = require_session_name(&missing, "attach")
            .unwrap_err()
            .to_string();
        assert!(err.contains("attach requires --session"), "{err}");

        let cli = Cli::try_parse_from([
            "gpui-agent",
            "--session",
            "work",
            "attach",
            "127.0.0.1:17421",
        ])
        .unwrap();
        assert_eq!(cli.session.as_deref(), Some("work"));
        assert!(matches!(cli.command, Command::Attach { .. }));
    }

    #[test]
    fn help_mentions_recipe_mcp_token() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            help.contains("GPUI_AGENT_TOKEN") && help.to_ascii_lowercase().contains("recipe run"),
            "{help}"
        );
    }

    #[test]
    fn read_back_is_on_unless_no_read_back_is_given() {
        let cli = Cli::try_parse_from(["gpui-agent", "click", "todo-add"]).unwrap();
        assert!(!cli.no_read_back);
        let cli =
            Cli::try_parse_from(["gpui-agent", "click", "todo-add", "--no-read-back"]).unwrap();
        assert!(cli.no_read_back);
        let cli = Cli::try_parse_from(["gpui-agent", "--no-read-back", "type", "todo-input", "x"])
            .unwrap();
        assert!(cli.no_read_back);
    }

    #[test]
    fn click_takes_an_expected_fingerprint() {
        let cli = Cli::try_parse_from([
            "gpui-agent",
            "click",
            "todo-add",
            "--expect-fingerprint",
            "42",
        ])
        .unwrap();
        match cli.command {
            Command::Click {
                expect_fingerprint, ..
            } => assert_eq!(expect_fingerprint, Some(42)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn stdin_replaces_the_text_of_type_set_value_and_paste() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["gpui-agent"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| cli.command)
        };
        for verb in ["type", "set-value", "paste"] {
            match parse(&[verb, "--stdin", "pw"]).unwrap() {
                Command::Type { text, stdin, .. }
                | Command::SetValue {
                    value: text, stdin, ..
                }
                | Command::Paste { text, stdin, .. } => {
                    assert!(stdin, "{verb}");
                    assert_eq!(text, None, "{verb}");
                }
                other => panic!("unexpected {other:?}"),
            }
            assert!(parse(&[verb, "pw"]).is_err(), "{verb} needs a text");
            assert!(
                parse(&[verb, "--stdin", "pw", "hunter2"]).is_err(),
                "{verb} takes the text once"
            );
        }
    }

    #[test]
    fn stdin_text_drops_one_trailing_line_ending() {
        let read = |input: &str| read_text(input.as_bytes()).unwrap();
        assert_eq!(read("hunter2\n"), "hunter2");
        assert_eq!(read("hunter2\r\n"), "hunter2");
        assert_eq!(read("hunter2"), "hunter2");
        assert_eq!(read("two\nlines\n\n"), "two\nlines\n");
        assert_eq!(read(""), "");
    }

    #[test]
    fn every_guarded_action_takes_an_expected_fingerprint() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["gpui-agent"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| cli.command)
        };
        let fp = |cmd: Command| match cmd {
            Command::Click {
                expect_fingerprint, ..
            }
            | Command::Type {
                expect_fingerprint, ..
            }
            | Command::SetValue {
                expect_fingerprint, ..
            }
            | Command::Key {
                expect_fingerprint, ..
            }
            | Command::Press {
                expect_fingerprint, ..
            } => expect_fingerprint,
            other => panic!("unexpected {other:?}"),
        };
        for args in [
            &["click", "a", "--expect-fingerprint", "7"][..],
            &["type", "a", "hi", "--expect-fingerprint", "7"],
            &["set-value", "a", "hi", "--expect-fingerprint", "7"],
            &["key", "a", "Enter", "--expect-fingerprint", "7"],
            &["press", "a", "--expect-fingerprint", "7"],
        ] {
            assert_eq!(fp(parse(args).unwrap()), Some(7), "{args:?}");
        }
        assert_eq!(
            fp(parse(&[
                "key",
                "a",
                "Enter",
                "--expect-fingerprint",
                "18446744073709551615"
            ])
            .unwrap()),
            Some(u64::MAX)
        );
        // A point press has no node to fingerprint.
        assert!(parse(&["press", "--x", "1", "--y", "2", "--expect-fingerprint", "7"]).is_err());
    }

    #[test]
    fn snapshot_outline_flags_parse_and_refuse_a_zero_depth() {
        let cli = Cli::try_parse_from([
            "gpui-agent",
            "snapshot",
            "--depth",
            "2",
            "--root",
            "page",
            "--fingerprints",
            "--refs",
        ])
        .unwrap();
        match cli.command {
            Command::Snapshot {
                refs,
                fingerprints,
                depth,
                root,
                ..
            } => {
                assert!(refs && fingerprints);
                assert_eq!(depth, Some(2));
                assert_eq!(root.as_deref(), Some("page"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(Cli::try_parse_from(["gpui-agent", "snapshot", "--depth", "0"]).is_err());
    }

    #[test]
    fn wait_flags_follow_only_actions() {
        let cli = Cli::try_parse_from(["gpui-agent", "click", "a", "--wait-for", "b"]).unwrap();
        assert!(!cli.guard);
        let (wait, timeout_ms) = requested_wait(&cli).unwrap().unwrap();
        assert_eq!(wait, gpui_agent::WaitFor::Present("b".into()));
        assert_eq!(timeout_ms, 5000);

        let cli = Cli::try_parse_from([
            "gpui-agent",
            "--wait-for-gone",
            "b",
            "--wait-timeout-ms",
            "250",
            "--guard",
            "invoke",
            "todo.clear",
        ])
        .unwrap();
        assert!(cli.guard);
        let (wait, timeout_ms) = requested_wait(&cli).unwrap().unwrap();
        assert_eq!(wait, gpui_agent::WaitFor::Gone("b".into()));
        assert_eq!(timeout_ms, 250);

        let cli = Cli::try_parse_from(["gpui-agent", "snapshot"]).unwrap();
        assert!(requested_wait(&cli).unwrap().is_none());
        let cli = Cli::try_parse_from(["gpui-agent", "snapshot", "--wait-for", "b"]).unwrap();
        assert!(
            requested_wait(&cli)
                .unwrap_err()
                .to_string()
                .contains("action")
        );
        assert!(
            Cli::try_parse_from([
                "gpui-agent",
                "click",
                "a",
                "--wait-for",
                "b",
                "--wait-for-gone",
                "c"
            ])
            .is_err()
        );
    }
}
