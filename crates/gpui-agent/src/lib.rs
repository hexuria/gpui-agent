//! GPUI Agent — a purpose-built, versioned control plane for GPUI Kit apps.
//!
//! This is **not** Chrome DevTools Protocol. GPUI Kit apps are GPU-rendered
//! native surfaces with no DOM, so Playwright/CDP cannot attach. Agents talk
//! to an opt-in localhost NDJSON server that publishes a semantic UI tree
//! (stable ids, roles, names, state) and dispatches actions into the same
//! handlers the widgets use.
//!
//! Platform seams (`PlatformKind`) keep the protocol stable across desktop,
//! a headless test host, and later web (GPUI WASM) or mobile hosts.

pub mod client;
pub mod dispatch;
pub mod host;
pub mod mailbox;
pub mod protocol;
pub mod security;
pub mod server;
pub mod tree;

pub use client::AgentClient;
pub use dispatch::{DispatchResult, handle_request};
pub use host::AgentHost;
pub use mailbox::{AgentMailbox, MailboxRequest};
pub use protocol::{
    AssertSpec, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind, Request, Response,
};
pub use security::{AgentConfig, SecurityError, from_env};
pub use server::{AgentServer, DEFAULT_ADDR_STR, DEFAULT_PORT, default_addr};
pub use tree::{Bounds, UiNode, UiTree};

/// Semantic-id helpers shared by hosts and agents.
pub mod ids {
    pub const INPUT: &str = "todo-input";
    pub const ADD: &str = "todo-add";
    pub const LIST: &str = "todo-list";
    pub const EMPTY: &str = "todo-empty";
    pub const STATUS: &str = "todo-status";
    pub const WINDOW: &str = "todo-window";

    pub fn item(id: u64) -> String {
        format!("todo-item-{id}")
    }

    pub fn toggle(id: u64) -> String {
        format!("todo-toggle-{id}")
    }

    pub fn delete(id: u64) -> String {
        format!("todo-delete-{id}")
    }

    pub fn parse_numbered(prefix: &str, target: &str) -> Option<u64> {
        target
            .strip_prefix(prefix)
            .and_then(|rest| rest.parse().ok())
    }
}
