//! Stable error codes and recovery hints for failed requests.
//!
//! Hosts on the shared server send `error_code` and `hint` next to the
//! human `error` text. Older hosts send only the text; the client maps it
//! through the same [`error_code_of`], so callers see one vocabulary
//! whichever host answered. Codes are part of the protocol: add new ones,
//! never rename or reuse one.

use serde::{Deserialize, Serialize};

/// The challenge was unanswered or the token wrong.
pub const UNAUTHORIZED: &str = "unauthorized";
/// The request line was not valid NDJSON protocol.
pub const BAD_REQUEST: &str = "bad_request";
/// Client and host disagree on the protocol version.
pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
/// The host does not list this op in `hello.features`.
pub const UNSUPPORTED_OP: &str = crate::protocol::UNSUPPORTED_OP;
/// The target changed between snapshot and dispatch.
pub const STALE_TARGET: &str = crate::dispatch::STALE_TARGET;
/// The target left the tree between snapshot and dispatch.
pub const TARGET_GONE: &str = crate::dispatch::TARGET_GONE;
/// The target id is not in the current tree.
pub const NOT_FOUND: &str = "not_found";
/// Several nodes carry the same id.
pub const DUPLICATE_ID: &str = "duplicate_id";
/// An `assert` spec does not match the node.
pub const ASSERT_FAILED: &str = "assert_failed";
/// A `wait`/`wait_until` deadline elapsed.
pub const TIMEOUT: &str = "timeout";
/// A bounded recipe scroll ended without finding its node.
pub const SCROLL_UNTIL_NOT_FOUND: &str = "scroll_until_not_found";
/// The UI-thread mailbox is not draining.
pub const MAILBOX_FULL: &str = "mailbox_full";
/// The host cannot capture a screenshot here.
pub const SCREENSHOT_UNAVAILABLE: &str = crate::screenshot::SCREENSHOT_UNAVAILABLE;
/// The host cannot deliver virtual input here.
pub const VIRTUAL_UNAVAILABLE: &str = crate::virtual_input::VIRTUAL_UNAVAILABLE;
/// The host cannot drive the scroll target.
pub const SCROLL_UNAVAILABLE: &str = crate::scroll_capture::SCROLL_UNAVAILABLE;
/// The host cannot dispatch the Action here.
pub const KEYBINDING_UNAVAILABLE: &str = crate::keybinding::KEYBINDING_UNAVAILABLE;
/// The host has no geometry for `in_viewport`.
pub const IN_VIEWPORT_UNAVAILABLE: &str = crate::tree::IN_VIEWPORT_UNAVAILABLE;
/// Any other failure the host reported.
pub const HOST_ERROR: &str = "host_error";
/// Client side: the host was never reached, so nothing was sent.
pub const CONNECT_FAILED: &str = "connect_failed";
/// Client side: the request may have been written but no usable reply
/// came back.
pub const NO_REPLY: &str = "no_reply";

/// Every code, in documentation order.
pub const ALL: &[&str] = &[
    UNAUTHORIZED,
    BAD_REQUEST,
    UNSUPPORTED_VERSION,
    UNSUPPORTED_OP,
    STALE_TARGET,
    TARGET_GONE,
    NOT_FOUND,
    DUPLICATE_ID,
    ASSERT_FAILED,
    TIMEOUT,
    SCROLL_UNTIL_NOT_FOUND,
    MAILBOX_FULL,
    SCREENSHOT_UNAVAILABLE,
    VIRTUAL_UNAVAILABLE,
    SCROLL_UNAVAILABLE,
    KEYBINDING_UNAVAILABLE,
    IN_VIEWPORT_UNAVAILABLE,
    HOST_ERROR,
    CONNECT_FAILED,
    NO_REPLY,
];

/// Classify an error text into one stable code.
///
/// An error that starts with a known code followed by `:` (or is exactly
/// a code) keeps that code; the remaining rules recognise the fixed texts
/// the shared dispatcher, server and client have always produced, so a
/// host that predates `error_code` classifies the same way.
pub fn error_code_of(error: &str) -> &'static str {
    let error = error.trim();
    let head = error.split(':').next().unwrap_or("").trim();
    if let Some(code) = ALL.iter().find(|code| **code == head) {
        return code;
    }
    if error.starts_with("automation token required")
        || error.starts_with("invalid automation token")
        || error.starts_with("token must not be sent")
    {
        return UNAUTHORIZED;
    }
    if error.starts_with("unsupported protocol version")
        || error.starts_with("unsupported challenge version")
    {
        return UNSUPPORTED_VERSION;
    }
    if error.starts_with("bad request") || error.starts_with("bad json") {
        return BAD_REQUEST;
    }
    if error.starts_with("events unsupported") {
        return UNSUPPORTED_OP;
    }
    if error.starts_with("mailbox full") {
        return MAILBOX_FULL;
    }
    if error.starts_with("wait_until timed out")
        || error.starts_with("wait timed out")
        || error.starts_with("timed out waiting for the UI thread")
    {
        return TIMEOUT;
    }
    // Client transport texts (`client.rs`).
    if error.starts_with("connect ") && error.contains(" failed") {
        return CONNECT_FAILED;
    }
    if error.starts_with("no reply")
        || error.starts_with("connection closed")
        || error.starts_with("bad response")
    {
        return NO_REPLY;
    }
    if error.starts_with("duplicate id") {
        return DUPLICATE_ID;
    }
    if error.starts_with("node `") {
        if error.ends_with("` not found") {
            return NOT_FOUND;
        }
        if error.contains(": expected ") || error.contains("should be absent") {
            return ASSERT_FAILED;
        }
    }
    HOST_ERROR
}

/// Fixed recovery hint for a code, `None` when there is nothing better to
/// do than read the error.
pub fn hint_for(code: &str) -> Option<&'static str> {
    Some(match code {
        UNAUTHORIZED => "set GPUI_AGENT_TOKEN to the host's token and retry",
        BAD_REQUEST => "fix the request; this client and host disagree on its shape",
        UNSUPPORTED_VERSION => "use a client and host that speak the same protocol version",
        UNSUPPORTED_OP => "check hello.features; this host does not offer the operation",
        STALE_TARGET => "take a new snapshot and retry",
        TARGET_GONE => "take a new snapshot; the target left the tree",
        NOT_FOUND => "take a new snapshot and use an id from it",
        DUPLICATE_ID => "the host gave several nodes this id; target a unique id",
        ASSERT_FAILED => "take a new snapshot to see the current state",
        TIMEOUT => "raise the timeout or wait for the UI to settle, then retry",
        SCROLL_UNTIL_NOT_FOUND => {
            "check the container and node pattern, or raise max_steps when the list can still move"
        }
        MAILBOX_FULL => "the UI thread is busy; wait and retry",
        SCREENSHOT_UNAVAILABLE | SCROLL_UNAVAILABLE => {
            "this host cannot capture here; use a snapshot instead"
        }
        VIRTUAL_UNAVAILABLE => "retry with delivery=semantic",
        KEYBINDING_UNAVAILABLE => "list keybindings and use one the host offers",
        IN_VIEWPORT_UNAVAILABLE => "this host has no geometry; assert visible instead",
        CONNECT_FAILED => "start the host or check --addr; nothing was sent, so retry is safe",
        NO_REPLY => {
            "the request may have run; take a snapshot before retrying, or raise --reply-timeout-ms"
        }
        _ => return None,
    })
}

/// Codes the host returns before dispatching anything, so the request
/// had no effect and a retry is safe.
pub fn refused_before_dispatch(code: &str) -> bool {
    matches!(
        code,
        UNAUTHORIZED
            | BAD_REQUEST
            | UNSUPPORTED_VERSION
            | UNSUPPORTED_OP
            | STALE_TARGET
            | TARGET_GONE
            | NOT_FOUND
            | DUPLICATE_ID
            | MAILBOX_FULL
            | VIRTUAL_UNAVAILABLE
            | KEYBINDING_UNAVAILABLE
            | CONNECT_FAILED
    )
}

/// What the client knows about whether an action reached the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[expect(
    clippy::exhaustive_enums,
    reason = "adding #[non_exhaustive] would break 0.3.0 callers' exhaustive matches"
)]
pub enum DeliveryStatus {
    /// Nothing was written, or the host refused before dispatch. Retry is
    /// safe.
    NotDelivered,
    /// The request was written and no usable reply came back, or the host
    /// failed during dispatch. Snapshot before retrying.
    DeliveryUncertain,
    /// The host answered `ok`.
    DeliveredUnverified,
    /// The host answered `ok` and a read-back saw the target change.
    DeliveredVerified,
}

impl DeliveryStatus {
    /// The wire spelling (`snake_case`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotDelivered => "not_delivered",
            Self::DeliveryUncertain => "delivery_uncertain",
            Self::DeliveredUnverified => "delivered_unverified",
            Self::DeliveredVerified => "delivered_verified",
        }
    }

    /// Status of a failed host reply with this code.
    pub fn of_failure(code: &str) -> Self {
        if refused_before_dispatch(code) {
            Self::NotDelivered
        } else {
            Self::DeliveryUncertain
        }
    }
}

/// A failed call, typed: the text, its stable code and hint, and for
/// actions what is known about delivery. Serializes as the failure
/// envelope the CLI prints and MCP tool errors carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallFailure {
    /// The human error text.
    pub error: String,
    /// Its stable [`error_code_of`] classification.
    pub error_code: String,
    /// The code's recovery hint, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// For actions: what is known about delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<DeliveryStatus>,
}

impl CallFailure {
    /// Classify a bare error text (no delivery known).
    pub fn from_message(error: impl Into<String>) -> Self {
        let error = error.into();
        let code = error_code_of(&error);
        Self::with_code(error, code, None)
    }

    /// A failure with an explicit code (skips classification).
    pub fn with_code(
        error: impl Into<String>,
        code: &str,
        delivery_status: Option<DeliveryStatus>,
    ) -> Self {
        Self {
            error: error.into(),
            error_code: code.to_string(),
            hint: hint_for(code).map(str::to_string),
            delivery_status,
        }
    }

    /// A host reply with `ok: false`. `acts` marks an op that changes the
    /// app, which gets a `delivery_status`.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "one flag: whether the op changes the app"
    )]
    pub fn from_response(resp: &crate::protocol::Response, acts: bool) -> Self {
        let code = resp.code().unwrap_or(HOST_ERROR).to_string();
        let hint = resp
            .hint
            .clone()
            .or_else(|| hint_for(&code).map(str::to_string));
        Self {
            error: resp
                .error
                .clone()
                .unwrap_or_else(|| "request failed".into()),
            delivery_status: resp
                .delivery_status
                .or_else(|| acts.then(|| DeliveryStatus::of_failure(&code))),
            error_code: code,
            hint,
        }
    }

    /// `{"ok":false,"error":…,"error_code":…,"hint"?,"delivery_status"?}`.
    ///
    /// # Panics
    ///
    /// Never: [`CallFailure`] serialization cannot fail.
    pub fn envelope(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("CallFailure serializes");
        if let serde_json::Value::Object(map) = &mut value {
            map.insert("ok".into(), serde_json::Value::Bool(false));
        }
        value
    }
}

impl std::fmt::Display for CallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.error)
    }
}

impl std::error::Error for CallFailure {}

impl From<CallFailure> for String {
    fn from(failure: CallFailure) -> Self {
        failure.error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_carries_code_hint_and_delivery() {
        let failure = CallFailure::from_response(
            &crate::protocol::Response::err("1", "stale_target: `a` changed"),
            true,
        );
        assert_eq!(
            failure.envelope(),
            serde_json::json!({
                "ok": false,
                "error": "stale_target: `a` changed",
                "error_code": "stale_target",
                "hint": "take a new snapshot and retry",
                "delivery_status": "not_delivered",
            })
        );
        let read = CallFailure::from_response(
            &crate::protocol::Response::err("1", "node `a` not found"),
            false,
        );
        assert_eq!(read.delivery_status, None);
        assert_eq!(read.error_code, NOT_FOUND);
    }

    #[test]
    fn host_texts_map_to_codes() {
        let cases = [
            ("automation token required", UNAUTHORIZED),
            ("invalid automation token", UNAUTHORIZED),
            ("token must not be sent on the wire", UNAUTHORIZED),
            (
                "unsupported protocol version 1 (want 2)",
                UNSUPPORTED_VERSION,
            ),
            (
                "unsupported challenge version 3 (want 2)",
                UNSUPPORTED_VERSION,
            ),
            ("bad request: missing field `op`", BAD_REQUEST),
            ("bad json: EOF", BAD_REQUEST),
            (
                "unsupported_op: this host does not list `press` in hello.features",
                UNSUPPORTED_OP,
            ),
            ("events unsupported on this host", UNSUPPORTED_OP),
            (
                "stale_target: `a` changed between snapshot and dispatch",
                STALE_TARGET,
            ),
            ("target_gone: `a` is no longer in the tree", TARGET_GONE),
            ("node `a` not found", NOT_FOUND),
            ("duplicate id `a` (2 nodes)", DUPLICATE_ID),
            ("node `a` value: expected \"x\", got None", ASSERT_FAILED),
            ("wait_until timed out: node `a` not found", TIMEOUT),
            ("wait timed out", TIMEOUT),
            (
                "timed out waiting for the UI thread to drain the agent mailbox",
                TIMEOUT,
            ),
            ("mailbox full (UI thread not draining)", MAILBOX_FULL),
            ("screenshot_unavailable: headless", SCREENSHOT_UNAVAILABLE),
            ("virtual_unavailable: no window", VIRTUAL_UNAVAILABLE),
            ("scroll_unavailable: no scroll", SCROLL_UNAVAILABLE),
            ("keybinding_unavailable: x", KEYBINDING_UNAVAILABLE),
            ("in_viewport_unavailable: headless", IN_VIEWPORT_UNAVAILABLE),
            (
                "connect 127.0.0.1:1 failed: connection refused",
                CONNECT_FAILED,
            ),
            ("no reply from 127.0.0.1:1 within 5 ms: busy", NO_REPLY),
            ("connection closed", NO_REPLY),
            ("bad response: EOF", NO_REPLY),
            ("cannot click `x`", HOST_ERROR),
            ("", HOST_ERROR),
        ];
        for (text, code) in cases {
            assert_eq!(error_code_of(text), code, "{text}");
        }
    }

    #[test]
    fn every_code_classifies_as_itself_and_has_a_stale_hint() {
        for code in ALL {
            assert_eq!(error_code_of(code), *code);
            assert_eq!(error_code_of(&format!("{code}: detail")), *code);
        }
        assert_eq!(
            hint_for(STALE_TARGET),
            Some("take a new snapshot and retry")
        );
        assert_eq!(hint_for(HOST_ERROR), None);
    }

    #[test]
    fn delivery_status_serializes_snake_case() {
        for status in [
            DeliveryStatus::NotDelivered,
            DeliveryStatus::DeliveryUncertain,
            DeliveryStatus::DeliveredUnverified,
            DeliveryStatus::DeliveredVerified,
        ] {
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::json!(status.as_str())
            );
        }
        assert_eq!(
            DeliveryStatus::of_failure(STALE_TARGET),
            DeliveryStatus::NotDelivered
        );
        assert_eq!(
            DeliveryStatus::of_failure(HOST_ERROR),
            DeliveryStatus::DeliveryUncertain
        );
    }
}
