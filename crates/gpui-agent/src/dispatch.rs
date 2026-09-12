use crate::hmac_auth::{NONCE_LEN, hmac_verify};
use crate::host::AgentHost;
use crate::protocol::{AssertSpec, Op, PROTOCOL_VERSION, PlatformKind, Request, Response};
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

/// Single place that turns a request into a response. Used by the TCP
/// server, the mailbox drain, and unit tests — no network required.
pub fn handle_request(
    host: &mut dyn AgentHost,
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
        Op::WaitUntil { timeout_ms, spec } => wait_until_assert(host, &req.id, spec, timeout_ms),
        Op::Snapshot => {
            let mut resp = Response::ok(&req.id);
            resp.tree = Some(host.snapshot());
            resp
        }
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
        Op::Assert { spec } => match assert_tree(&host.snapshot(), &spec) {
            Ok(()) => Response::ok(req.id),
            Err(error) => Response::err(req.id, error),
        },
        Op::Keybindings => {
            let mut resp = Response::ok(&req.id);
            resp.result = Some(crate::keybinding_list_json(&host.keybindings()));
            resp
        }
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
        other => match host.dispatch(&other) {
            Ok(result) => {
                let mut resp = Response::ok(req.id);
                resp.result = result.value;
                resp
            }
            Err(error) => Response::err(req.id, error),
        },
    }
}

fn hello_response(host: &dyn AgentHost, id: &str, expected_token: Option<&str>) -> Response {
    let mut resp = Response::ok(id);
    let mut hello = host.hello();
    hello.auth = crate::protocol::HelloAuth::from_token_configured(expected_token);
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
) -> Response {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let tree = host.snapshot();
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
                ready: true,
                deliveries: vec![],
                auth: crate::protocol::HelloAuth::None,
            }
        }

        fn snapshot(&self) -> UiTree {
            UiTree {
                app: "test".into(),
                platform: PlatformKind::Headless,
                ready: true,
                nodes: vec![UiNode::new("test-window", "window", "Test")],
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
                ready: self.ready,
                deliveries: vec![],
                auth: crate::protocol::HelloAuth::None,
            }
        }

        fn snapshot(&self) -> UiTree {
            UiTree {
                app: "test".into(),
                platform: PlatformKind::Headless,
                ready: self.ready,
                nodes: vec![],
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
        assert_eq!(resp.hello.as_ref().unwrap().ready, false);
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
                    ready: self.ready.load(Ordering::SeqCst),
                    deliveries: vec![],
                    auth: crate::protocol::HelloAuth::None,
                }
            }

            fn snapshot(&self) -> UiTree {
                UiTree {
                    app: "test".into(),
                    platform: PlatformKind::Headless,
                    ready: self.ready.load(Ordering::SeqCst),
                    nodes: vec![],
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
        assert_eq!(resp.hello.as_ref().unwrap().ready, true);
    }

    #[test]
    fn assert_duplicate_id_is_error() {
        let tree = UiTree {
            app: "test".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: vec![UiNode::button("dup", "A").with_child(UiNode::button("dup", "B"))],
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
                ready: true,
                deliveries: vec![],
                auth: crate::protocol::HelloAuth::None,
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
                    ready: true,
                    deliveries: vec![],
                    auth: crate::protocol::HelloAuth::None,
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
