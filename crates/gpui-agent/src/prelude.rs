//! Common types for embedders. `use gpui_agent::prelude::*;`
//!
//! This is the SDK surface for a controllable GPUI (or headless) app:
//! implement [`AgentHost`](crate::AgentHost), build a [`UiTree`](crate::UiTree)
//! with [`UiNode`](crate::UiNode) helpers, then [`spawn_host`](crate::server::spawn_host)
//! or [`spawn_mailbox`](crate::server::spawn_mailbox).

pub use crate::client::AgentClient;
pub use crate::dispatch::{DispatchResult, handle_request};
pub use crate::host::AgentHost;
pub use crate::keybinding::{
    KeybindingInfo, authorize_keybinding, complete_keybinding_action, keybinding_unavailable,
    op_is_confirmed_quit,
};
pub use crate::mailbox::AgentMailbox;
pub use crate::protocol::{
    AssertSpec, DeliveryMode, HelloAuth, HelloInfo, KeybindingScope, Op, PROTOCOL_VERSION,
    PlatformKind, Request, Response,
};
pub use crate::security::{
    AgentConfig, authorize_bind, authorize_client, from_env, is_loopback_addr,
};
pub use crate::server::{
    DEFAULT_ADDR_STR, MAX_CONNECTIONS, MAX_LINE_BYTES, default_addr, spawn_host, spawn_mailbox,
};
pub use crate::tree::{Bounds, UiNode, UiTree, role};
pub use crate::{
    MAX_MAILBOX_DEPTH, parse_numbered_id, screenshot_unavailable, virtual_unavailable,
};
