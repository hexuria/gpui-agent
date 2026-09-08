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
pub mod virtual_input;

pub use client::AgentClient;
pub use dispatch::{DispatchResult, handle_request};
pub use host::AgentHost;
pub use mailbox::{AgentMailbox, MailboxRequest};
pub use protocol::{
    AssertSpec, DeliveryMode, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind, Request, Response,
};
pub use security::{AgentConfig, SecurityError, from_env};
pub use server::{AgentServer, DEFAULT_ADDR_STR, DEFAULT_PORT, default_addr};
pub use tree::{Bounds, UiNode, UiTree};
pub use virtual_input::{
    AgentCursor, VIRTUAL_UNAVAILABLE, VirtualPointerClick, hit_point, keystroke_token, plan_click,
    text_keystrokes, virtual_unavailable,
};

/// Parse `"{prefix}{n}"` into `n`. Apps use this for numbered stable ids
/// (`row-3`, `tab-1`); prefixes themselves are app-defined.
pub fn parse_numbered_id(prefix: &str, target: &str) -> Option<u64> {
    target
        .strip_prefix(prefix)
        .and_then(|rest| rest.parse().ok())
}

#[cfg(test)]
mod parse_id_tests {
    use super::parse_numbered_id;

    #[test]
    fn parses_app_defined_prefixes() {
        assert_eq!(parse_numbered_id("row-", "row-3"), Some(3));
        assert_eq!(parse_numbered_id("tab-", "tab-1"), Some(1));
        assert_eq!(parse_numbered_id("row-", "other-3"), None);
        assert_eq!(parse_numbered_id("row-", "row-x"), None);
    }
}
