//! GPUI Agent — embeddable SDK + protocol for controllable GPUI Kit apps.
//!
//! This is **not** Chrome DevTools Protocol. GPUI Kit apps are GPU-rendered
//! native surfaces with no DOM, so Playwright/CDP cannot attach. Agents talk
//! to an opt-in NDJSON server that publishes a semantic UI tree
//! (stable ids, roles, names, state) and dispatches actions into the same
//! handlers the widgets use.
//!
//! **SDK entry:** [`prelude`], [`testing::TestHost`], [`UiNode`] constructors
//! in [`tree::role`]. Cookbook: `docs/SDK.md`. Product agent-machine
//! mutations go through a **daemon** ([`docs/ADR-001-daemon-sot.md`] in the
//! repo); in-process [`AgentHost`] is for widget E2E and embedders.
//!
//! Platform seams (`PlatformKind`) keep the protocol stable across desktop,
//! a headless test host, and later web (GPUI WASM) or mobile hosts.

pub mod client;
pub mod dispatch;
pub mod host;
pub mod mailbox;
pub mod ndjson;
pub mod prelude;
pub mod protocol;
pub mod screenshot;
pub mod security;
pub mod server;
pub mod testing;
pub mod tree;
pub mod virtual_input;

pub use client::AgentClient;
pub use dispatch::{DispatchResult, authorize_request, handle_request};
pub use host::AgentHost;
pub use mailbox::{AgentMailbox, MAX_MAILBOX_DEPTH, MailboxRequest};
pub use ndjson::{line_is_blank, read_limited_line, read_limited_line_into, write_json_line};
pub use protocol::{
    AssertSpec, DeliveryMode, HelloAuth, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind, Request,
    Response,
};
pub use screenshot::{
    SCREENSHOT_UNAVAILABLE, TEST_PNG, is_screenshot_unavailable, screenshot_unavailable, write_png,
};
pub use security::{
    AgentConfig, SecurityError, authorize_bind, authorize_client, ensure_loopback, from_env,
    is_loopback_addr, tokens_match,
};
pub use server::{
    AgentServer, DEFAULT_ADDR_STR, DEFAULT_PORT, MAX_CONNECTIONS, MAX_LINE_BYTES, ServerLimits,
    default_addr,
};
pub use testing::TestHost;
pub use tree::{Bounds, UiNode, UiTree, role};
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

#[cfg(test)]
mod cap_tests {
    use super::{MAX_CONNECTIONS, MAX_LINE_BYTES, MAX_MAILBOX_DEPTH};

    #[test]
    fn security_caps_unchanged() {
        assert_eq!(MAX_LINE_BYTES, 1024 * 1024);
        assert_eq!(MAX_CONNECTIONS, 32);
        assert_eq!(MAX_MAILBOX_DEPTH, 128);
    }
}
