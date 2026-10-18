use std::collections::VecDeque;
use std::sync::Arc;

use crate::hmac_auth::{NONCE_LEN, hmac_verify};
use crate::host::AgentHost;
use crate::protocol::{
    AssertSpec, Op, PROTOCOL_VERSION, PlatformKind, Request, Response, SnapshotFormat,
};
use crate::scroll_capture::ScreenshotSpec;
use crate::tree::{UiNode, UiTree, in_viewport_unavailable, is_in_viewport_unavailable, role};

/// Optional structured payload returned by `invoke` / mutating ops.
#[derive(Debug, Clone, Default)]
pub struct DispatchResult {
    pub value: Option<serde_json::Value>,
}

impl DispatchResult {
    pub fn empty() -> Self {
        Self { value: None }
    }

    pub fn json(value: serde_json::Value) -> Self {
        Self { value: Some(value) }
    }
}

/// Fingerprint-gate error prefixes (`<prefix>: <detail>`). Classify with
/// [`is_stale_dispatch_error`] so callers match the token, not wording.
pub const STALE_TARGET: &str = "stale_target";
/// Fingerprint-gate prefix for targets gone from the tree entirely.
pub const TARGET_GONE: &str = "target_gone";

/// True for fingerprint-gate failures eligible for bounded re-decision.
/// Anything else (auth, bad op, host errors) is terminal, not retried.
pub fn is_stale_dispatch_error(err: &str) -> bool {
    err.starts_with(STALE_TARGET) || err.starts_with(TARGET_GONE)
}

/// Version + optional HMAC gate. Call this before dispatching,
/// including on the mailbox path where virtual ops skip [`handle_request`].
pub fn authorize_request(
    req: &Request,
    expected_token: Option<&str>,
    session_nonce: Option<&[u8]>,
) -> Result<(), Box<Response>> {
    if req.v != PROTOCOL_VERSION {
        return Err(Box::new(Response::err(
            req.id.clone(),
            format!(
                "unsupported protocol version {} (want {PROTOCOL_VERSION})",
                req.v
            ),
        )));
    }

    if let Some(expected) = expected_token {
        if req.token.as_deref().is_some_and(|t| !t.is_empty()) {
            return Err(Box::new(Response::err(
                req.id.clone(),
                "token must not be sent on the wire",
            )));
        }
        let Some(nonce) = session_nonce.filter(|n| n.len() == NONCE_LEN) else {
            return Err(Box::new(Response::err(
                req.id.clone(),
                "automation token required",
            )));
        };
        match req.auth.as_deref() {
            Some(auth) if hmac_verify(expected, nonce, auth) => {}
            Some(_) => {
                return Err(Box::new(Response::err(
                    req.id.clone(),
                    "invalid automation token",
                )));
            }
            None => {
                return Err(Box::new(Response::err(
                    req.id.clone(),
                    "automation token required",
                )));
            }
        }
    }

    Ok(())
}

/// How many past revisions the server ring keeps for `snapshot since`.
const REV_RING_DEPTH: usize = 8;

/// Per-connection snapshot cache + revision ring (M3).
///
/// `snapshot(host)` reuses the cached tree iff `host.revision()` returns
/// `Some` of the cached rev. A `None` revision always rebuilds — the old
/// behavior for hosts that cannot track changes.
///
/// Every rebuild at a known rev pushes `(rev, tree)` onto the ring so a
/// later `snapshot { since }` can answer with a diff instead of the full
/// tree. `rebuild_count` is the GATE-P counter: `wait_until` polling an
/// unchanged tree must not rebuild.
#[derive(Debug, Default)]
pub struct TreeCache {
    ring: VecDeque<(u64, Arc<UiTree>)>,
    rebuilds: u64,
}

impl TreeCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shared tree, rebuilt only when the host reports a different rev
    /// (or no rev at all).
    pub fn snapshot(&mut self, host: &dyn AgentHost) -> Arc<UiTree> {
        if let Some(rev) = host.revision() {
            if let Some((cached_rev, tree)) = self.ring.back() {
                if *cached_rev == rev {
                    return tree.clone();
                }
            }
        }
        self.rebuilds += 1;
        let mut tree = host.snapshot();
        let rev = host.revision().or(tree.rev);
        if let Some(rev) = rev {
            tree.rev = Some(rev);
            self.ring.push_back((rev, Arc::new(tree)));
            while self.ring.len() > REV_RING_DEPTH {
                self.ring.pop_front();
            }
            self.ring.back().expect("just pushed").1.clone()
        } else {
            Arc::new(tree)
        }
    }

    /// A past revision still in the ring (for `snapshot since` diffs).
    pub fn tree_at(&self, rev: u64) -> Option<Arc<UiTree>> {
        self.ring
            .iter()
            .find(|(r, _)| *r == rev)
            .map(|(_, t)| t.clone())
    }

    /// Number of real `host.snapshot()` calls this cache made.
    pub fn rebuild_count(&self) -> u64 {
        self.rebuilds
    }
}

/// Single place that turns a request into a response. Used by the TCP
/// server, the mailbox drain, and unit tests — no network required.
///
/// Uses a fresh [`TreeCache`] per call: `wait_until` polls inside still
/// avoid rebuilds, but `snapshot since` only diffs against trees this
/// call itself produced. The TCP server keeps one cache per connection
/// via [`handle_request_cached`].
pub fn handle_request(
    host: &mut dyn AgentHost,
    req: Request,
    expected_token: Option<&str>,
    session_nonce: Option<&[u8]>,
) -> Response {
    let mut cache = TreeCache::new();
    handle_request_cached(host, &mut cache, req, expected_token, session_nonce)
}

/// [`handle_request`] with a caller-owned [`TreeCache`] so a session can
/// reuse trees across requests and answer `snapshot since` with diffs.
pub fn handle_request_cached(
    host: &mut dyn AgentHost,
    cache: &mut TreeCache,
    req: Request,
    expected_token: Option<&str>,
    session_nonce: Option<&[u8]>,
) -> Response {
    if let Err(resp) = authorize_request(&req, expected_token, session_nonce) {
        return *resp;
    }

    match req.op {
        Op::Hello => hello_response(host, &req.id, expected_token),
        Op::Wait { timeout_ms: None } => hello_response(host, &req.id, expected_token),
        Op::Wait {
            timeout_ms: Some(ms),
        } => wait_until_ready(host, &req.id, expected_token, ms),
        Op::WaitUntil { timeout_ms, spec } => {
            wait_until_assert(host, &req.id, spec, timeout_ms, cache)
        }
        Op::Revision => {
            let mut resp = Response::ok(&req.id);
            resp.rev = host.revision();
            resp
        }
        Op::Snapshot {
            since,
            format,
            fingerprints,
        } => snapshot_response(host, cache, &req.id, since, format, fingerprints),
        Op::Screenshot {
            path,
            mode,
            target,
            max_height_px,
        } => {
            let spec = ScreenshotSpec::from_op(&path, mode, &target, max_height_px);
            if let Err(error) = spec.validate_request() {
                return Response::err(req.id, error);
            }
            match host.screenshot(spec) {
                Ok(result) => {
                    let mut resp = Response::ok(req.id);
                    resp.result = result.value;
                    resp
                }
                Err(error) => Response::err(req.id, error),
            }
        }
        Op::Assert { spec } => {
            let tree = cache.snapshot(host);
            match assert_tree(&tree, &spec) {
                Ok(()) => Response::ok(req.id),
                Err(error) => Response::err(req.id, error),
            }
        }
        Op::Keybindings => {
            let mut resp = Response::ok(&req.id);
            resp.result = Some(crate::keybinding_list_json(&host.keybindings()));
            resp
        }
        Op::Events { since, limit } => match host.event_ring() {
            None => Response::err(req.id, "events unsupported on this host"),
            Some(ring) => {
                let poll = ring.since(
                    since,
                    limit
                        .map(|l| l as usize)
                        .unwrap_or(crate::event::EVENTS_DEFAULT_LIMIT),
                );
                let mut resp = Response::ok(req.id);
                resp.result = serde_json::to_value(poll).ok();
                resp
            }
        },
        kb @ Op::Keybinding { .. } => {
            match crate::authorize_keybinding_op(&kb, &host.keybindings(), host.is_app_focused()) {
                Err(error) => Response::err(req.id, error),
                Ok(_) => match host.dispatch(&kb) {
                    Ok(result) => {
                        let mut resp = Response::ok(req.id);
                        resp.result = result.value;
                        resp
                    }
                    Err(error) => Response::err(req.id, error),
                },
            }
        }
        Op::Shutdown => match host.dispatch(&Op::Shutdown) {
            Ok(result) => {
                let mut resp = Response::ok(req.id);
                resp.result = result.value;
                resp
            }
            Err(error) => Response::err(req.id, error),
        },
        other => {
            // M5 staleness gate — recomputed in the same call, before
            // dispatch. `cache.snapshot` re-reads whenever the revision
            // moved, so this is never a stale-of-stale check.
            if let Some((target, expected)) = other.fingerprint_gate() {
                let tree = cache.snapshot(host);
                match tree.fingerprint_of(target) {
                    None => {
                        return Response::err(
                            req.id,
                            format!("{TARGET_GONE}: `{target}` is no longer in the tree"),
                        );
                    }
                    Some(actual) if actual != expected => {
                        return Response::err(
                            req.id,
                            format!(
                                "{STALE_TARGET}: `{target}` changed between snapshot and dispatch"
                            ),
                        );
                    }
                    _ => {}
                }
            }
            match host.dispatch(&other) {
                Ok(result) => {
                    let mut resp = Response::ok(req.id);
                    resp.result = result.value;
                    resp
                }
                Err(error) => Response::err(req.id, error),
            }
        }
    }
}

/// `snapshot` — with `format:"compact"` the payload is one line per
/// visible node in `result.compact`; with `since` it is `unchanged`, a
/// `diff` against a ring-cached revision, or the full tree + `full:true`
/// when the client's revision is no longer remembered.
fn snapshot_response(
    host: &dyn AgentHost,
    cache: &mut TreeCache,
    id: &str,
    since: Option<u64>,
    format: Option<SnapshotFormat>,
    fingerprints: Option<bool>,
) -> Response {
    let tree = cache.snapshot(host);
    let rev = tree.rev;
    let mut resp = Response::ok(id);
    resp.rev = rev;
    if fingerprints == Some(true) {
        resp.fingerprints = Some(tree.fingerprint_map());
    }
    if format == Some(SnapshotFormat::Compact) {
        resp.result = Some(serde_json::json!({
            "compact": crate::diff::compact_lines(&tree),
        }));
        return resp;
    }
    match since {
        Some(s) if rev == Some(s) => {
            resp.result = Some(serde_json::json!({ "unchanged": true }));
        }
        Some(s) => match rev.and_then(|_| cache.tree_at(s)) {
            Some(old) => {
                let diff = crate::diff::diff_trees(&old, &tree);
                resp.result = Some(serde_json::json!({ "diff": diff }));
            }
            None => {
                resp.tree = Some((*tree).clone());
                resp.result = Some(serde_json::json!({ "full": true }));
            }
        },
        None => {
            resp.tree = Some((*tree).clone());
        }
    }
    resp
}

fn hello_response(host: &dyn AgentHost, id: &str, expected_token: Option<&str>) -> Response {
    let mut resp = Response::ok(id);
    let mut hello = host.hello();
    hello.auth = crate::protocol::HelloAuth::from_token_configured(expected_token);
    crate::discovery::fill_hello_identity(&mut hello);
    for feature in ["snapshot_since", "snapshot_compact", "expect_fingerprint"] {
        if !hello.features.iter().any(|f| f == feature) {
            hello.features.push(feature.to_string());
        }
    }
    if host.revision().is_some() && !hello.features.iter().any(|f| f == "rev") {
        hello.features.push("rev".into());
    }
    if host.event_ring().is_some() && !hello.features.iter().any(|f| f == "events") {
        hello.features.push("events".into());
    }
    resp.hello = Some(hello);
    resp
}

fn wait_until_ready(
    host: &dyn AgentHost,
    id: &str,
    expected_token: Option<&str>,
    timeout_ms: u64,
) -> Response {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let hello = host.hello();
        if hello.ready {
            let mut resp = Response::ok(id);
            let mut hello = hello;
            hello.auth = crate::protocol::HelloAuth::from_token_configured(expected_token);
            crate::discovery::fill_hello_identity(&mut hello);
            resp.hello = Some(hello);
            return resp;
        }
        if std::time::Instant::now() >= deadline {
            return Response::err(id, "wait timed out");
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(10)));
    }
}

fn wait_until_assert(
    host: &dyn AgentHost,
    id: &str,
    spec: AssertSpec,
    timeout_ms: u64,
    cache: &mut TreeCache,
) -> Response {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let tree = cache.snapshot(host);
        match assert_tree(&tree, &spec) {
            Ok(()) => return Response::ok(id),
            Err(error) => {
                if is_in_viewport_unavailable(&error) && tree.platform == PlatformKind::Headless {
                    return Response::err(id, error);
                }
                if std::time::Instant::now() >= deadline {
                    return Response::err(id, format!("wait_until timed out: {error}"));
                }
            }
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(10)));
    }
}

pub fn assert_tree(tree: &UiTree, spec: &AssertSpec) -> Result<(), String> {
    let exists = spec.exists.unwrap_or(true);
    if exists {
        let node = tree.require_id(&spec.target)?;
        if let Some(name) = spec.name.as_deref() {
            if node.name != name {
                return Err(format!(
                    "node `{}` name: expected {name:?}, got {:?}",
                    spec.target, node.name
                ));
            }
        }
        if let Some(value) = spec.value.as_deref() {
            if node.value.as_deref() != Some(value) {
                return Err(format!(
                    "node `{}` value: expected {value:?}, got {:?}",
                    spec.target, node.value
                ));
            }
        }
        if let Some(role) = spec.role.as_deref() {
            if node.role != role {
                return Err(format!(
                    "node `{}` role: expected {role:?}, got {:?}",
                    spec.target, node.role
                ));
            }
        }
        if let Some(checked) = spec.checked {
            if node.checked != Some(checked) {
                return Err(format!(
                    "node `{}` checked: expected {checked}, got {:?}",
                    spec.target, node.checked
                ));
            }
        }
        if let Some(visible) = spec.visible {
            if node.visible != visible {
                return Err(format!(
                    "node `{}` visible: expected {visible}, got {}",
                    spec.target, node.visible
                ));
            }
        }
        if let Some(expected) = spec.in_viewport {
            let actual = node_in_viewport(tree, node)?;
            if actual != expected {
                return Err(format!(
                    "node `{}` in_viewport: expected {expected}, got {actual}",
                    spec.target
                ));
            }
        }
        Ok(())
    } else {
        match tree.find_all(&spec.target).len() {
            0 => Ok(()),
            1 => Err(format!(
                "node `{}` exists but should be absent",
                spec.target
            )),
            n => Err(format!("duplicate id `{}` ({n} nodes)", spec.target)),
        }
    }
}

/// Honest geometry check: non-empty intersection of `node.bounds` with the
/// painted window clip (the ancestor `role=window` bounds, same coordinate
/// space as descendants).
///
/// Headless hosts and zero-area bounds return [`in_viewport_unavailable`] —
/// never `Ok(false)` from missing geometry.
pub fn node_in_viewport(tree: &UiTree, node: &UiNode) -> Result<bool, String> {
    if tree.platform == PlatformKind::Headless {
        return Err(in_viewport_unavailable(
            "headless hosts have no painted window clip",
        ));
    }
    if !node.bounds.has_area() {
        return Err(in_viewport_unavailable(format!(
            "node `{}` bounds are zero",
            node.id
        )));
    }
    let clip = window_clip(tree, node)?;
    if !clip.has_area() {
        return Err(in_viewport_unavailable(format!(
            "window clip for `{}` has zero bounds",
            node.id
        )));
    }
    Ok(node.bounds.intersects(clip))
}

fn window_clip(tree: &UiTree, target: &UiNode) -> Result<crate::tree::Bounds, String> {
    if let Some(window) = find_window_for(tree, &target.id) {
        return Ok(window.bounds);
    }
    Err(in_viewport_unavailable(format!(
        "no window clip for `{}`",
        target.id
    )))
}

fn find_window_for<'a>(tree: &'a UiTree, target_id: &str) -> Option<&'a UiNode> {
    for root in &tree.nodes {
        if let Some(window) = window_for_target(root, target_id, None) {
            return Some(window);
        }
    }
    None
}

fn window_for_target<'a>(
    node: &'a UiNode,
    target_id: &str,
    current_window: Option<&'a UiNode>,
) -> Option<&'a UiNode> {
    let window = if node.role == role::WINDOW {
        Some(node)
    } else {
        current_window
    };
    if node.id == target_id {
        return window;
    }
    for child in &node.children {
        if let Some(found) = window_for_target(child, target_id, window) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{HelloInfo, PlatformKind};
    use crate::tree::{UiNode, UiTree};

    struct EmptyHost;

    impl AgentHost for EmptyHost {
        fn hello(&self) -> HelloInfo {
            HelloInfo {
                protocol: PROTOCOL_VERSION,
                app: "test".into(),
                platform: PlatformKind::Headless,
                os: crate::protocol::host_os(),
                ready: true,
                deliveries: vec![],
                auth: crate::protocol::HelloAuth::None,
                bundle_id: None,
                executable: None,
                features: vec![],
            }
        }

        fn snapshot(&self) -> UiTree {
            UiTree {
                app: "test".into(),
                platform: PlatformKind::Headless,
                ready: true,
                nodes: vec![UiNode::new("test-window", "window", "Test")],
                rev: None,
            }
        }

        fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
            Err("unsupported".into())
        }
    }

    #[test]
    fn rejects_wrong_version() {
        let mut host = EmptyHost;
        let mut req = Request::new("1", Op::Hello);
        req.v = 99;
        let resp = handle_request(&mut host, req, None, None);
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("unsupported protocol"));
    }

    #[test]
    fn rejects_missing_token() {
        let mut host = EmptyHost;
        let nonce = [0x11u8; 32];
        let req = Request::new("1", Op::Hello);
        let resp = handle_request(&mut host, req, Some("secret"), Some(&nonce));
        assert!(!resp.ok);
    }

    #[test]
    fn accepts_matching_token() {
        let mut host = EmptyHost;
        let nonce = [0x11u8; 32];
        let auth = crate::hmac_auth::hmac_hex("secret", &nonce).unwrap();
        let req = Request::new("1", Op::Hello).with_auth(auth);
        let resp = handle_request(&mut host, req, Some("secret"), Some(&nonce));
        assert!(resp.ok);
        assert!(resp.hello.is_some());
    }

    /// A host that owns an [`crate::event::EventRing`] (M9 fixture).
    struct RingHost {
        ring: crate::event::EventRing,
    }

    impl AgentHost for RingHost {
        fn hello(&self) -> HelloInfo {
            EmptyHost.hello()
        }
        fn snapshot(&self) -> UiTree {
            EmptyHost.snapshot()
        }
        fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
            Err("unsupported".into())
        }
        fn event_ring(&self) -> Option<&crate::event::EventRing> {
            Some(&self.ring)
        }
    }

    #[test]
    fn events_op_answers_the_ring() {
        let mut host = RingHost {
            ring: crate::event::EventRing::default(),
        };
        host.ring
            .push(crate::event::EventLevel::Info, "todo", "invoke todo.add");
        host.ring
            .push(crate::event::EventLevel::Warn, "todo", "slow op");

        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::Events {
                    since: None,
                    limit: None,
                },
            ),
            None,
            None,
        );
        assert!(resp.ok);
        let result = resp.result.expect("events result");
        assert_eq!(result["latest"], 2);
        assert_eq!(result["lagged"], false);
        let events = result["events"].as_array().expect("events array");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["seq"], 1);
        assert_eq!(events[0]["level"], "info");
        assert_eq!(events[0]["target"], "todo");
        assert_eq!(events[0]["message"], "invoke todo.add");
        assert_eq!(events[1]["level"], "warn");

        // since=1 → only seq 2.
        let resp = handle_request(
            &mut host,
            Request::new(
                "2",
                Op::Events {
                    since: Some(1),
                    limit: None,
                },
            ),
            None,
            None,
        );
        let events = resp.result.expect("result")["events"]
            .as_array()
            .expect("array")
            .clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["seq"], 2);
    }

    #[test]
    fn events_op_fails_closed_without_a_ring() {
        let mut host = EmptyHost;
        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::Events {
                    since: None,
                    limit: None,
                },
            ),
            None,
            None,
        );
        assert!(!resp.ok);
        assert_eq!(
            resp.error.as_deref(),
            Some("events unsupported on this host")
        );
    }

    #[test]
    fn hello_advertises_events_only_with_a_ring() {
        let mut bare = EmptyHost;
        let resp = handle_request(&mut bare, Request::new("1", Op::Hello), None, None);
        let features = resp.hello.unwrap().features;
        assert!(!features.iter().any(|f| f == "events"));

        let mut host = RingHost {
            ring: crate::event::EventRing::default(),
        };
        let resp = handle_request(&mut host, Request::new("1", Op::Hello), None, None);
        let features = resp.hello.unwrap().features;
        assert!(features.iter().any(|f| f == "events"));
    }

    #[test]
    fn screenshot_is_honestly_unavailable_on_empty_host() {
        let mut host = EmptyHost;
        let dest =
            std::env::temp_dir().join(format!("gpui-agent-empty-shot-{}", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        let req = Request::new("1", Op::screenshot(dest.to_string_lossy().into_owned()));
        let resp = handle_request(&mut host, req, None, None);
        assert!(!resp.ok);
        let err = resp.error.unwrap();
        assert!(crate::is_screenshot_unavailable(&err), "{err}");
        assert!(
            !dest.exists(),
            "unavailable must not invent a PNG at {}",
            dest.display()
        );
    }

    #[test]
    fn scrolled_without_target_is_request_error() {
        let mut host = EmptyHost;
        let req = Request::new(
            "1",
            Op::Screenshot {
                path: Some("tall.png".into()),
                mode: crate::protocol::ScreenshotMode::Scrolled,
                target: None,
                max_height_px: None,
            },
        );
        let resp = handle_request(&mut host, req, None, None);
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(err.contains("requires target"), "{err}");
        assert!(
            !crate::is_screenshot_unavailable(&err),
            "missing target is not unavailable: {err}"
        );
    }

    #[test]
    fn scrolled_on_empty_host_is_still_unavailable() {
        let mut host = EmptyHost;
        let dest = std::env::temp_dir().join(format!(
            "gpui-agent-empty-scrolled-{}.png",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&dest);
        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::screenshot_scrolled("tall.png", "todo-list-scroll", None),
            ),
            None,
            None,
        );
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(crate::is_screenshot_unavailable(&err), "{err}");
        assert!(!dest.exists(), "must not invent {}", dest.display());
    }

    #[test]
    fn scrolled_max_height_too_large_is_request_error() {
        let mut host = EmptyHost;
        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::Screenshot {
                    path: Some("tall.png".into()),
                    mode: crate::protocol::ScreenshotMode::Scrolled,
                    target: Some("todo-list-scroll".into()),
                    max_height_px: Some(crate::DEFAULT_MAX_HEIGHT_PX + 1),
                },
            ),
            None,
            None,
        );
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(err.contains("exceeds"), "{err}");
        assert!(
            !crate::is_screenshot_unavailable(&err),
            "cap must fail closed as a request error: {err}"
        );
    }

    #[test]
    fn authorize_rejects_virtual_wrong_version() {
        let mut req = Request::new("1", Op::click_virtual("todo-add"));
        req.v = 99;
        let err = authorize_request(&req, None, None).unwrap_err();
        assert!(!err.ok);
        assert!(err.error.unwrap().contains("unsupported protocol"));
    }

    #[test]
    fn hello_auth_none_without_host_token() {
        let mut host = EmptyHost;
        let resp = handle_request(&mut host, Request::new("1", Op::Hello), None, None);
        assert!(resp.ok);
        assert_eq!(resp.hello.unwrap().auth, crate::protocol::HelloAuth::None);
    }

    #[test]
    fn hello_auth_required_with_host_token() {
        let mut host = EmptyHost;
        let nonce = [0x11u8; 32];
        let auth = crate::hmac_auth::hmac_hex("secret", &nonce).unwrap();
        let req = Request::new("1", Op::Hello).with_auth(auth);
        let resp = handle_request(&mut host, req, Some("secret"), Some(&nonce));
        assert!(resp.ok);
        assert_eq!(
            resp.hello.unwrap().auth,
            crate::protocol::HelloAuth::Required
        );
    }

    #[test]
    fn v2_raw_token_on_wire_is_rejected() {
        let req = Request::new("1", Op::Hello).with_token("secret");
        assert_eq!(
            req.v, PROTOCOL_VERSION,
            "this red must not be a version mismatch"
        );
        let nonce = [0x11u8; 32];
        let err = authorize_request(&req, Some("secret"), Some(&nonce)).unwrap_err();
        let msg = err.error.as_deref().unwrap_or("");
        assert!(
            msg.contains("token must not be sent on the wire"),
            "{err:?}"
        );
        assert!(
            !msg.contains("unsupported protocol version"),
            "must reject the token field, not the version: {msg}"
        );
    }

    #[test]
    fn v2_challenge_hmac_accepts_matching_token() {
        let mut host = EmptyHost;
        let nonce = [0x22u8; 32];
        let auth = crate::hmac_auth::hmac_hex("secret", &nonce).unwrap();
        let req = Request::new("1", Op::Hello).with_auth(auth);
        let resp = handle_request(&mut host, req, Some("secret"), Some(&nonce));
        assert!(resp.ok, "{resp:?}");
    }

    #[test]
    fn v2_hmac_from_wrong_nonce_is_rejected() {
        let mut host = EmptyHost;
        let nonce = [0x22u8; 32];
        let other = [0x33u8; 32];
        let auth = crate::hmac_auth::hmac_hex("secret", &other).unwrap();
        let req = Request::new("1", Op::Hello).with_auth(auth);
        let resp = handle_request(&mut host, req, Some("secret"), Some(&nonce));
        assert!(!resp.ok, "{resp:?}");
        assert!(
            resp.error
                .as_deref()
                .is_some_and(|e| e.contains("invalid automation token")),
            "{resp:?}"
        );
    }

    struct ReadyHost {
        ready: bool,
    }

    impl AgentHost for ReadyHost {
        fn hello(&self) -> HelloInfo {
            HelloInfo {
                protocol: PROTOCOL_VERSION,
                app: "test".into(),
                platform: PlatformKind::Headless,
                os: crate::protocol::host_os(),
                ready: self.ready,
                deliveries: vec![],
                auth: crate::protocol::HelloAuth::None,
                bundle_id: None,
                executable: None,
                features: vec![],
            }
        }

        fn snapshot(&self) -> UiTree {
            UiTree {
                app: "test".into(),
                platform: PlatformKind::Headless,
                ready: self.ready,
                nodes: vec![],
                rev: None,
            }
        }

        fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
            Err("unsupported".into())
        }
    }

    #[test]
    fn wait_times_out_when_not_ready() {
        let mut host = ReadyHost { ready: false };
        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::Wait {
                    timeout_ms: Some(50),
                },
            ),
            None,
            None,
        );
        assert!(!resp.ok, "{resp:?}");
        assert!(
            resp.error
                .as_deref()
                .is_some_and(|e| e.contains("timed out")),
            "{resp:?}"
        );
        assert!(resp.hello.is_none() || resp.hello.as_ref().is_some_and(|h| !h.ready));
    }

    #[test]
    fn wait_none_is_immediate_hello() {
        let mut host = ReadyHost { ready: false };
        let started = std::time::Instant::now();
        let resp = handle_request(
            &mut host,
            Request::new("1", Op::Wait { timeout_ms: None }),
            None,
            None,
        );
        let elapsed = started.elapsed();
        assert!(resp.ok, "{resp:?}");
        assert!(!resp.hello.as_ref().unwrap().ready);
        assert!(
            elapsed < std::time::Duration::from_millis(40),
            "Wait None must not poll: {elapsed:?}"
        );
    }

    #[test]
    fn wait_succeeds_when_host_becomes_ready() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct FlipHost {
            ready: Arc<AtomicBool>,
        }

        impl AgentHost for FlipHost {
            fn hello(&self) -> HelloInfo {
                HelloInfo {
                    protocol: PROTOCOL_VERSION,
                    app: "test".into(),
                    platform: PlatformKind::Headless,
                    os: crate::protocol::host_os(),
                    ready: self.ready.load(Ordering::SeqCst),
                    deliveries: vec![],
                    auth: crate::protocol::HelloAuth::None,
                    bundle_id: None,
                    executable: None,
                    features: vec![],
                }
            }

            fn snapshot(&self) -> UiTree {
                UiTree {
                    app: "test".into(),
                    platform: PlatformKind::Headless,
                    ready: self.ready.load(Ordering::SeqCst),
                    nodes: vec![],
                    rev: None,
                }
            }

            fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
                Err("unsupported".into())
            }
        }

        let ready = Arc::new(AtomicBool::new(false));
        let mut host = FlipHost {
            ready: ready.clone(),
        };
        let flag = ready.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            flag.store(true, Ordering::SeqCst);
        });
        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::Wait {
                    timeout_ms: Some(500),
                },
            ),
            None,
            None,
        );
        assert!(resp.ok, "{resp:?}");
        assert!(resp.hello.as_ref().unwrap().ready);
    }

    #[test]
    fn assert_duplicate_id_is_error() {
        let tree = UiTree {
            app: "test".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: vec![UiNode::button("dup", "A").with_child(UiNode::button("dup", "B"))],
            rev: None,
        };
        let spec = AssertSpec {
            target: "dup".into(),
            ..Default::default()
        };
        let err = assert_tree(&tree, &spec).unwrap_err();
        assert!(err.contains("duplicate id"), "{err}");
    }

    #[test]
    fn keybindings_list_is_empty_on_hosts_without_a_catalog() {
        let mut host = EmptyHost;
        let resp = handle_request(&mut host, Request::new("1", Op::Keybindings), None, None);
        assert!(resp.ok, "{resp:?}");
        let list = resp.result.expect("list");
        assert_eq!(list["keybindings"], serde_json::json!([]));
    }

    #[test]
    fn keybinding_unknown_fails_before_dispatch() {
        let mut host = EmptyHost;
        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::keybinding("app.quit", crate::KeybindingScope::Global),
            ),
            None,
            None,
        );
        assert!(!resp.ok, "{resp:?}");
        assert!(
            resp.error
                .as_deref()
                .is_some_and(|e| e.contains("unknown binding")),
            "{resp:?}"
        );
    }

    fn sample_tree(platform: PlatformKind, visible: bool, bounds: crate::tree::Bounds) -> UiTree {
        UiTree {
            app: "test".into(),
            platform,
            ready: true,
            nodes: vec![
                UiNode::window("test-window", "Test")
                    .with_bounds(crate::tree::Bounds {
                        x: 0.0,
                        y: 0.0,
                        w: 800.0,
                        h: 600.0,
                    })
                    .with_child(
                        UiNode::button("panel", "Panel")
                            .with_visible(visible)
                            .with_bounds(bounds),
                    ),
            ],
            rev: None,
        }
    }

    #[test]
    fn assert_visible_matches_host_field() {
        let tree = sample_tree(
            PlatformKind::Headless,
            false,
            crate::tree::Bounds::default(),
        );
        let spec = AssertSpec {
            target: "panel".into(),
            visible: Some(false),
            ..Default::default()
        };
        assert_tree(&tree, &spec).expect("hidden");
        let err = assert_tree(
            &tree,
            &AssertSpec {
                target: "panel".into(),
                visible: Some(true),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("visible"), "{err}");
    }

    #[test]
    fn in_viewport_fails_closed_on_headless() {
        let tree = sample_tree(
            PlatformKind::Headless,
            true,
            crate::tree::Bounds {
                x: 10.0,
                y: 10.0,
                w: 40.0,
                h: 20.0,
            },
        );
        let err = assert_tree(
            &tree,
            &AssertSpec {
                target: "panel".into(),
                in_viewport: Some(true),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(crate::is_in_viewport_unavailable(&err), "{err}");
        assert!(err.contains("headless"), "{err}");
    }

    #[test]
    fn in_viewport_fails_closed_on_zero_bounds() {
        let tree = sample_tree(PlatformKind::Desktop, true, crate::tree::Bounds::default());
        let err = assert_tree(
            &tree,
            &AssertSpec {
                target: "panel".into(),
                in_viewport: Some(true),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(crate::is_in_viewport_unavailable(&err), "{err}");
        assert!(err.contains("zero"), "{err}");
        let err_false = assert_tree(
            &tree,
            &AssertSpec {
                target: "panel".into(),
                in_viewport: Some(false),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            crate::is_in_viewport_unavailable(&err_false),
            "must not invent not-in-viewport from zero bounds: {err_false}"
        );
    }

    #[test]
    fn in_viewport_intersects_window_clip() {
        let inside = sample_tree(
            PlatformKind::Desktop,
            true,
            crate::tree::Bounds {
                x: 10.0,
                y: 10.0,
                w: 40.0,
                h: 20.0,
            },
        );
        assert_tree(
            &inside,
            &AssertSpec {
                target: "panel".into(),
                in_viewport: Some(true),
                ..Default::default()
            },
        )
        .expect("inside clip");

        let outside = sample_tree(
            PlatformKind::Desktop,
            true,
            crate::tree::Bounds {
                x: 900.0,
                y: 10.0,
                w: 40.0,
                h: 20.0,
            },
        );
        assert_tree(
            &outside,
            &AssertSpec {
                target: "panel".into(),
                in_viewport: Some(false),
                ..Default::default()
            },
        )
        .expect("outside clip");
        let err = assert_tree(
            &outside,
            &AssertSpec {
                target: "panel".into(),
                in_viewport: Some(true),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("in_viewport"), "{err}");
        assert!(!crate::is_in_viewport_unavailable(&err), "{err}");
    }

    struct VisibleHost {
        visible: bool,
    }

    impl AgentHost for VisibleHost {
        fn hello(&self) -> HelloInfo {
            HelloInfo {
                protocol: PROTOCOL_VERSION,
                app: "test".into(),
                platform: PlatformKind::Headless,
                os: crate::protocol::host_os(),
                ready: true,
                deliveries: vec![],
                auth: crate::protocol::HelloAuth::None,
                bundle_id: None,
                executable: None,
                features: vec![],
            }
        }

        fn snapshot(&self) -> UiTree {
            sample_tree(
                PlatformKind::Headless,
                self.visible,
                crate::tree::Bounds::default(),
            )
        }

        fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
            Err("unsupported".into())
        }
    }

    #[test]
    fn wait_until_times_out_when_assert_never_matches() {
        let mut host = VisibleHost { visible: false };
        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::wait_until(
                    AssertSpec {
                        target: "panel".into(),
                        visible: Some(true),
                        ..Default::default()
                    },
                    50,
                ),
            ),
            None,
            None,
        );
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(err.contains("timed out"), "{err}");
        assert!(err.contains("visible"), "{err}");
    }

    #[test]
    fn wait_until_succeeds_when_visible_flips() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct FlipVisible {
            visible: Arc<AtomicBool>,
        }

        impl AgentHost for FlipVisible {
            fn hello(&self) -> HelloInfo {
                HelloInfo {
                    protocol: PROTOCOL_VERSION,
                    app: "test".into(),
                    platform: PlatformKind::Headless,
                    os: crate::protocol::host_os(),
                    ready: true,
                    deliveries: vec![],
                    auth: crate::protocol::HelloAuth::None,
                    bundle_id: None,
                    executable: None,
                    features: vec![],
                }
            }

            fn snapshot(&self) -> UiTree {
                sample_tree(
                    PlatformKind::Headless,
                    self.visible.load(Ordering::SeqCst),
                    crate::tree::Bounds::default(),
                )
            }

            fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
                Err("unsupported".into())
            }
        }

        let visible = Arc::new(AtomicBool::new(false));
        let mut host = FlipVisible {
            visible: visible.clone(),
        };
        let flag = visible.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            flag.store(true, Ordering::SeqCst);
        });
        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::wait_until(
                    AssertSpec {
                        target: "panel".into(),
                        visible: Some(true),
                        ..Default::default()
                    },
                    500,
                ),
            ),
            None,
            None,
        );
        assert!(resp.ok, "{resp:?}");
    }

    #[test]
    fn wait_until_headless_in_viewport_fails_fast() {
        let mut host = VisibleHost { visible: true };
        let started = std::time::Instant::now();
        let resp = handle_request(
            &mut host,
            Request::new(
                "1",
                Op::wait_until(
                    AssertSpec {
                        target: "panel".into(),
                        in_viewport: Some(true),
                        ..Default::default()
                    },
                    400,
                ),
            ),
            None,
            None,
        );
        let elapsed = started.elapsed();
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(crate::is_in_viewport_unavailable(&err), "{err}");
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "headless in_viewport must fail closed without waiting out the timeout: {elapsed:?}"
        );
    }
}
