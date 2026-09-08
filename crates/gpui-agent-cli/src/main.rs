mod mcp;

use std::net::SocketAddr;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, Op};
use gpui_agent::DEFAULT_ADDR_STR;

#[derive(Parser)]
#[command(
    name = "gpui-agent",
    about = "Drive a GPUI Kit app over the opt-in agent protocol (not CDP)."
)]
struct Cli {
    /// Host:port of the automation server (loopback only on the app side).
    #[arg(long, default_value = DEFAULT_ADDR_STR, env = "GPUI_AGENT_ADDR")]
    addr: SocketAddr,
    /// Shared secret; must match GPUI_AGENT_TOKEN in the app when set.
    #[arg(long, env = "GPUI_AGENT_TOKEN")]
    token: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Handshake: protocol version, app name, platform, ready.
    Hello,
    /// Print the semantic UI tree as JSON.
    Snapshot {
        #[arg(long)]
        pretty: bool,
    },
    /// Activate a widget by stable id (`todo-add`, `todo-toggle-1`, …).
    Click { target: String },
    /// Append text to an editable widget.
    Type { target: String, text: String },
    /// Replace the value of an editable widget.
    SetValue { target: String, value: String },
    /// Send a key (`Enter`, `Backspace`) to a widget.
    Key { target: String, key: String },
    /// Assert fields on a node from the current snapshot.
    Assert {
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        value: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        checked: Option<bool>,
        #[arg(long, default_value_t = true)]
        exists: bool,
        #[arg(long)]
        absent: bool,
    },
    /// Call a named host command (`todo.add`, `todo.toggle`, …).
    Invoke {
        name: String,
        /// Repeatable `key=value` pairs. Values are JSON if they parse, else strings.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
    },
    /// Block until the host answers hello/ready.
    Wait,
    /// Ask the host to exit.
    Shutdown,
    /// Todo helpers (compose invoke + snapshot).
    #[command(subcommand)]
    Todo(TodoCommand),
    /// Tiny MCP stdio server exposing the same tools.
    Mcp,
}

#[derive(Subcommand)]
enum TodoCommand {
    Add { title: String },
    Toggle { id: u64 },
    Delete { id: u64 },
    List,
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

fn run() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Mcp) {
        return mcp::run(cli.addr, cli.token);
    }

    let mut client = AgentClient::connect(cli.addr);
    if let Some(token) = cli.token {
        client = client.with_token(token);
    }

    match cli.command {
        Command::Hello => print_resp(rpc(client.expect_ok(Op::Hello))?),
        Command::Snapshot { pretty } => {
            let resp = rpc(client.snapshot())?;
            if pretty {
                println!("{}", serde_json::to_string_pretty(&resp.tree)?);
            } else {
                print_resp(resp);
            }
        }
        Command::Click { target } => print_resp(rpc(client.click(target))?),
        Command::Type { target, text } => {
            print_resp(rpc(client.expect_ok(Op::Type { target, text }))?)
        }
        Command::SetValue { target, value } => print_resp(rpc(client.set_value(target, value))?),
        Command::Key { target, key } => print_resp(rpc(client.expect_ok(Op::Key { target, key }))?),
        Command::Assert {
            id,
            name,
            value,
            role,
            checked,
            exists,
            absent,
        } => {
            let spec = AssertSpec {
                target: id,
                name,
                value,
                role,
                checked,
                exists: Some(if absent { false } else { exists }),
                checked,
                exists: Some(if absent { false } else { exists }),
            };
            print_resp(rpc(client.assert(spec))?);
        }
        Command::Invoke { name, args } => {
            print_resp(rpc(client.invoke(name, parse_args(&args)?))?)
        }
        Command::Wait => print_resp(rpc(client.wait_ready())?),
        Command::Shutdown => print_resp(rpc(client.expect_ok(Op::Shutdown))?),
        Command::Todo(TodoCommand::Add { title }) => {
            print_resp(rpc(client.invoke("todo.add", serde_json::json!({ "title": title })))?)
        }
        Command::Todo(TodoCommand::Toggle { id }) => {
            print_resp(rpc(client.invoke("todo.toggle", serde_json::json!({ "id": id })))?)
        }
        Command::Todo(TodoCommand::Delete { id }) => {
            print_resp(rpc(client.invoke("todo.delete", serde_json::json!({ "id": id })))?)
        }
        Command::Todo(TodoCommand::List) => {
            let resp = rpc(client.invoke("todo.list", serde_json::json!({})))?;
            if let Some(value) = resp.result {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
        }
        Command::Mcp => unreachable!(),
    }
    Ok(())
}

fn rpc<T>(result: std::result::Result<T, String>) -> Result<T> {
    result.map_err(|e| anyhow!("{e}"))
}

fn parse_args(pairs: &[String]) -> Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for pair in pairs {

fn parse_args(pairs: &[String]) -> Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for pair in pairs {
        let (key, raw) = pair
            .split_once('=')
            .with_context(|| format!("expected KEY=VALUE, got {pair}"))?;
        let value = serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.into()));
        map.insert(key.to_string(), value);
    }
    Ok(serde_json::Value::Object(map))
}

fn print_resp(resp: gpui_agent::Response) {
    println!("{}", serde_json::to_string_pretty(&resp).unwrap());
}
