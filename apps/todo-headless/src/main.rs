use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand};
use gpui_agent::DEFAULT_ADDR_STR;
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{Op, PlatformKind};
use gpui_agent::security::from_env;
use gpui_agent::server::spawn_host;
use todo_core::TodoStore;

/// Headless todo daemon: app domain logic, no GPUI / GPU window.
///
/// Bind policy comes from `SecurityPolicy::from_env` (`GPUI_AGENT=1`,
/// loopback default). Drive it with `gpui-agent` CLI / recipes.
#[derive(Parser, Debug)]
#[command(name = "todo-headless")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
enum Command {
    /// Bind the agent protocol and serve until shutdown (default).
    Serve,
    /// Print hello/ready from a running daemon.
    Status,
    /// Ask a running daemon to exit.
    Shutdown,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

fn run() -> Result<(), ExitCode> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(),
        Command::Status => status(),
        Command::Shutdown => shutdown(),
    }
}

fn serve() -> Result<(), ExitCode> {
    let config = match from_env() {
        Ok(Some(config)) => config,
        Ok(None) => {
            eprintln!(
                "todo-headless serve is an automation host. Start it with GPUI_AGENT=1.\n\
                 Example: GPUI_AGENT=1 todo-headless serve"
            );
            return Err(ExitCode::from(2));
        }
        Err(err) => {
            eprintln!("refusing to start automation: {err}");
            return Err(ExitCode::from(2));
        }
    };

    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let token_set = config.token.is_some();
    let (addr, shutdown) =
        spawn_host(config.addr, config.token, store.clone()).expect("bind agent server");

    eprintln!("gpui-agent listening on {addr} (platform=headless, app=todo)");
    eprintln!("opt-in: GPUI_AGENT=1 · bind via from_env · protocol v1");
    if token_set {
        eprintln!(
            "auth: required (GPUI_AGENT_TOKEN set; recipe/MCP clients must send the same token)"
        );
    } else {
        eprintln!(
            "auth: none (one-off click/snapshot ok; recipe run and mcp need the same token on host and client)"
        );
    }
    eprintln!("delivery: semantic only (virtual_unavailable — no GPUI event pipeline)");

    while !shutdown.load(std::sync::atomic::Ordering::SeqCst) {
        if store.lock().expect("store").wants_shutdown() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn connect_running() -> Result<AgentClient, ExitCode> {
    let addr = match std::env::var("GPUI_AGENT_ADDR") {
        Ok(raw) => raw.parse::<SocketAddr>().map_err(|err| {
            eprintln!("invalid GPUI_AGENT_ADDR: {err}");
            ExitCode::from(2)
        })?,
        Err(_) => DEFAULT_ADDR_STR.parse().expect("default addr"),
    };
    let token = std::env::var("GPUI_AGENT_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());
    let allow_remote = gpui_agent::security::truthy_env("GPUI_AGENT_ALLOW_REMOTE");
    gpui_agent::authorize_client(addr, token.as_deref(), allow_remote).map_err(|err| {
        eprintln!("refusing agent address: {err}");
        ExitCode::from(2)
    })?;
    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));
    if let Some(token) = token {
        client = client.with_token(token);
    }
    Ok(client)
}

fn status() -> Result<(), ExitCode> {
    let mut client = connect_running()?;
    let resp = client.expect_ok(Op::Hello).map_err(|err| {
        eprintln!("status failed: {err}");
        ExitCode::from(1)
    })?;
    println!("{}", serde_json::to_string(&resp).expect("hello json"));
    Ok(())
}

fn shutdown() -> Result<(), ExitCode> {
    let mut client = connect_running()?;
    client.expect_ok(Op::Shutdown).map_err(|err| {
        eprintln!("shutdown failed: {err}");
        ExitCode::from(1)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn default_command_is_serve() {
        let cli = Cli::try_parse_from(["todo-headless"]).unwrap();
        assert_eq!(cli.command, None);
        assert_eq!(cli.command.unwrap_or(Command::Serve), Command::Serve);
    }

    #[test]
    fn parses_serve_status_shutdown() {
        let serve = Cli::try_parse_from(["todo-headless", "serve"]).unwrap();
        assert_eq!(serve.command, Some(Command::Serve));
        let status = Cli::try_parse_from(["todo-headless", "status"]).unwrap();
        assert_eq!(status.command, Some(Command::Status));
        let shutdown = Cli::try_parse_from(["todo-headless", "shutdown"]).unwrap();
        assert_eq!(shutdown.command, Some(Command::Shutdown));
    }
}
