//! Stable error codes and recovery hints for failed requests.
//!
//! Hosts on the shared server send `error_code` and `hint` next to the
//! human `error` text. Older hosts send only the text; the client maps it
//! through the same [`error_code_of`], so callers see one vocabulary
//! whichever host answered. Codes are part of the protocol: add new ones,
//! never rename or reuse one.

use serde::{Deserialize, Serialize};

pub const UNAUTHORIZED: &str = "unauthorized";
pub const BAD_REQUEST: &str = "bad_request";
pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
pub const UNSUPPORTED_OP: &str = crate::protocol::UNSUPPORTED_OP;
pub const STALE_TARGET: &str = crate::dispatch::STALE_TARGET;
pub const TARGET_GONE: &str = crate::dispatch::TARGET_GONE;
pub const NOT_FOUND: &str = "not_found";
pub const DUPLICATE_ID: &str = "duplicate_id";
pub const ASSERT_FAILED: &str = "assert_failed";
pub const TIMEOUT: &str = "timeout";
pub const MAILBOX_FULL: &str = "mailbox_full";
pub const SCREENSHOT_UNAVAILABLE: &str = crate::screenshot::SCREENSHOT_UNAVAILABLE;
pub const VIRTUAL_UNAVAILABLE: &str = crate::virtual_input::VIRTUAL_UNAVAILABLE;
pub const SCROLL_UNAVAILABLE: &str = crate::scroll_capture::SCROLL_UNAVAILABLE;
pub const KEYBINDING_UNAVAILABLE: &str = crate::keybinding::KEYBINDING_UNAVAILABLE;
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
    let head = error.split_once(':').map_or(error, |(head, _)| head).trim();
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

/// [`error_code_of`] for an error a host replied with. A reply proves the
/// request reached the host, so the client's own transport codes cannot
/// describe it, however the host worded its error.
pub(crate) fn host_reply_code_of(error: &str) -> &'static str {
    host_reply_code(error_code_of(error))
}

/// `code` as the code of a host reply: the transport-only codes
/// ([`CONNECT_FAILED`], [`NO_REPLY`]) become [`HOST_ERROR`], since a host
/// that answered was reached.
pub(crate) fn host_reply_code(code: &str) -> &str {
    match code {
        CONNECT_FAILED | NO_REPLY => HOST_ERROR,
        code => code,
    }
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
    pub error: String,
    pub error_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
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
    fn a_host_reply_is_never_a_transport_failure() {
        for (error, general, from_host) in [
            ("connect to backend failed", CONNECT_FAILED, HOST_ERROR),
            (
                "connect 10.0.0.2:5432 failed: refused",
                CONNECT_FAILED,
                HOST_ERROR,
            ),
            ("no reply from the payment service", NO_REPLY, HOST_ERROR),
            ("connection closed by the database", NO_REPLY, HOST_ERROR),
            ("bad response from upstream", NO_REPLY, HOST_ERROR),
            ("connect_failed: proxied", CONNECT_FAILED, HOST_ERROR),
            ("no_reply", NO_REPLY, HOST_ERROR),
            ("wait timed out", TIMEOUT, TIMEOUT),
            ("stale_target: `a` changed", STALE_TARGET, STALE_TARGET),
            ("node `a` not found", NOT_FOUND, NOT_FOUND),
            ("cannot click `a`", HOST_ERROR, HOST_ERROR),
        ] {
            assert_eq!(error_code_of(error), general, "{error}");
            assert_eq!(host_reply_code_of(error), from_host, "{error}");
            assert_eq!(host_reply_code(general), from_host, "{error}");
        }
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
