//! GUI → daemon client (ADR-001). Product mutations go through the SoT.
//!
//! Snapshot poll v1 — no event-bus protocol bump. Blocking `AgentClient`
//! lives on a worker thread so the GPUI UI thread never talks TCP.

use std::net::SocketAddr;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use gpui_agent::client::AgentClient;
use gpui_agent::security::{authorize_client, truthy_env};
use gpui_agent::{DEFAULT_ADDR_STR, default_addr};
use todo_core::TodoView;

enum Cmd {
    Click(String),
    SetValue { target: String, value: String },
}

pub struct DaemonBridge {
    cmds: Sender<Cmd>,
    views: Receiver<Result<TodoView, String>>,
}

impl DaemonBridge {
    pub fn start() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (view_tx, view_rx) = mpsc::channel();
        thread::spawn(move || worker(cmd_rx, view_tx));
        Self {
            cmds: cmd_tx,
            views: view_rx,
        }
    }

    pub fn click(&self, target: impl Into<String>) {
        let _ = self.cmds.send(Cmd::Click(target.into()));
    }

    pub fn set_value(&self, target: impl Into<String>, value: impl Into<String>) {
        let _ = self.cmds.send(Cmd::SetValue {
            target: target.into(),
            value: value.into(),
        });
    }

    /// Drain to the latest snapshot or connect error.
    pub fn poll(&self) -> Option<Result<TodoView, String>> {
        match self.views.try_recv() {
            Ok(first) => {
                let mut last = first;
                while let Ok(next) = self.views.try_recv() {
                    last = next;
                }
                Some(last)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("daemon worker exited".into())),
        }
    }
}

fn connect_client() -> Result<AgentClient, String> {
    let addr = match std::env::var("GPUI_AGENT_ADDR") {
        Ok(raw) => raw
            .parse::<SocketAddr>()
            .map_err(|err| format!("invalid GPUI_AGENT_ADDR: {err}"))?,
        Err(_) => default_addr(),
    };
    let token = std::env::var("GPUI_AGENT_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());
    let allow_remote = truthy_env("GPUI_AGENT_ALLOW_REMOTE");
    authorize_client(addr, token.as_deref(), allow_remote).map_err(|err| err.to_string())?;
    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    if let Some(token) = token {
        client = client.with_token(token);
    }
    Ok(client)
}

fn worker(cmds: Receiver<Cmd>, views: Sender<Result<TodoView, String>>) {
    let mut client: Option<AgentClient> = None;
    loop {
        match cmds.try_recv() {
            Ok(Cmd::Click(target)) => {
                if let Err(err) = ensure_client(&mut client).and_then(|c| c.click(target)) {
                    client = None;
                    if views.send(Err(err)).is_err() {
                        break;
                    }
                }
            }
            Ok(Cmd::SetValue { target, value }) => {
                if let Err(err) =
                    ensure_client(&mut client).and_then(|c| c.set_value(target, value))
                {
                    client = None;
                    if views.send(Err(err)).is_err() {
                        break;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => break,
        }

        match ensure_client(&mut client).and_then(|c| c.snapshot()) {
            Ok(resp) => {
                if let Some(tree) = resp.tree {
                    if views.send(Ok(TodoView::from_tree(&tree))).is_err() {
                        break;
                    }
                }
            }
            Err(err) => {
                client = None;
                if views.send(Err(err)).is_err() {
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(150));
    }
}

fn ensure_client(client: &mut Option<AgentClient>) -> Result<&mut AgentClient, String> {
    if client.is_none() {
        *client = Some(connect_client()?);
    }
    Ok(client.as_mut().expect("client"))
}

pub fn banner() {
    let addr = std::env::var("GPUI_AGENT_ADDR").unwrap_or_else(|_| DEFAULT_ADDR_STR.into());
    eprintln!("todo GUI is a daemon client (ADR-001). Connecting to {addr}");
    eprintln!("start the source of truth: GPUI_AGENT=1 todo-headless serve");
    eprintln!("widget E2E / in-process host: cargo run -p todo --features embedded-host");
}
