use crate::hmac_auth::{NONCE_LEN, hmac_verify};
use crate::host::AgentHost;
use crate::protocol::{AssertSpec, Op, PROTOCOL_VERSION, Request, Response};
use crate::tree::UiTree;

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
) -> Result<(), Response> {
    if req.v != PROTOCOL_VERSION {
        return Err(Response::err(
            req.id.clone(),
            format!(
                "unsupported protocol version {} (want {PROTOCOL_VERSION})",
                req.v
            ),
        ));
    }

    if let Some(expected) = expected_token {
        if req.token.as_deref().is_some_and(|t| !t.is_empty()) {
            return Err(Response::err(
                req.id.clone(),
                "token must not be sent on the wire",
            ));
        }
        let Some(nonce) = session_nonce.filter(|n| n.len() == NONCE_LEN) else {
            return Err(Response::err(req.id.clone(), "automation token required"));
        };
        match req.auth.as_deref() {
            Some(auth) if hmac_verify(expected, nonce, auth) => {}
            Some(_) => return Err(Response::err(req.id.clone(), "invalid automation token")),
            None => return Err(Response::err(req.id.clone(), "automation token required")),
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
        return resp;
    }

    match req.op {
        Op::Hello => hello_response(host, &req.id, expected_token),
        Op::Wait { timeout_ms: None } => hello_response(host, &req.id, expected_token),
        Op::Wait {
            timeout_ms: Some(ms),
        } => wait_until_ready(host, &req.id, expected_token, ms),
        Op::Snapshot => {
            let mut resp = Response::ok(&req.id);
            resp.tree = Some(host.snapshot());
            resp
        }
        Op::Screenshot { path } => match host.screenshot(path.as_deref()) {
            Ok(result) => {
                let mut resp = Response::ok(req.id);
                resp.result = result.value;
                resp
            }
            Err(error) => Response::err(req.id, error),
        },
        Op::Assert { spec } => match assert_tree(&host.snapshot(), &spec) {
            Ok(()) => Response::ok(req.id),
            Err(error) => Response::err(req.id, error),
        },
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

pub fn assert_tree(tree: &UiTree, spec: &AssertSpec) -> Result<(), String> {
    let node = tree.find(&spec.target);
    let exists = spec.exists.unwrap_or(true);

    match (node, exists) {
        (None, true) => return Err(format!("node `{}` not found", spec.target)),
        (Some(_), false) => {
            return Err(format!(
                "node `{}` exists but should be absent",
                spec.target
            ));
        }
        (None, false) => return Ok(()),
        (Some(node), true) => {
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
        }
    }
    Ok(())
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
        let req = Request::new(
            "1",
            Op::Screenshot {
                path: Some(dest.to_string_lossy().into_owned()),
            },
        );
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
        let nonce = [0x11u8; 32];
        let err = authorize_request(&req, Some("secret"), Some(&nonce)).unwrap_err();
        assert!(
            err.error
                .as_deref()
                .is_some_and(|e| e.contains("token must not be sent on the wire")),
            "{err:?}"
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
}
