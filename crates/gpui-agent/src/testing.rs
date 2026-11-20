//! In-process test host helpers for SDK / recipe E2E.
//!
//! Spawns [`spawn_host`] on `127.0.0.1:0` and
//! hands back an [`AgentClient`]. Screenshot
//! stays honest: hosts without a surface must return `screenshot_unavailable`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::client::AgentClient;
use crate::host::AgentHost;
use crate::server::spawn_host;

/// A live headless `AgentHost` bound on an ephemeral loopback port.
pub struct TestHost<H: AgentHost + 'static> {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    host: Arc<Mutex<H>>,
}

impl<H: AgentHost + 'static> TestHost<H> {
    /// Bind `127.0.0.1:0` and serve `host` on a background thread.
    pub fn spawn(host: H, token: Option<String>) -> std::io::Result<Self> {
        let host = Arc::new(Mutex::new(host));
        let (addr, shutdown) =
            spawn_host(SocketAddr::from(([127, 0, 0, 1], 0)), token, host.clone())?;
        Ok(Self {
            addr,
            shutdown,
            host,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Client aimed at this host. Caller adds `.with_token` when the host has one.
    pub fn client(&self) -> AgentClient {
        AgentClient::connect(self.addr).with_timeout(Duration::from_secs(3))
    }

    /// Shared host lock for assertions that peek at domain state.
    pub fn host(&self) -> Arc<Mutex<H>> {
        self.host.clone()
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

impl<H: AgentHost + 'static> Drop for TestHost<H> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::DispatchResult;
    use crate::protocol::{HelloInfo, Op, PROTOCOL_VERSION, PlatformKind};
    use crate::tree::{UiNode, UiTree};

    struct PingHost {
        n: u32,
    }

    impl AgentHost for PingHost {
        fn hello(&self) -> HelloInfo {
            HelloInfo {
                os: crate::protocol::host_os(),
                protocol: PROTOCOL_VERSION,
                framework: None,
                device: None,
                app: "ping".into(),
                platform: PlatformKind::Headless,
                ready: true,
                deliveries: vec![],
                auth: crate::HelloAuth::None,
                bundle_id: None,
                executable: None,
                features: vec![],
            }
        }

        fn snapshot(&self) -> UiTree {
            UiTree {
                app: "ping".into(),
                platform: PlatformKind::Headless,
                ready: true,
                nodes: vec![UiNode::window("ping-window", "Ping").with_value(self.n.to_string())],
                rev: None,
            }
        }

        fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
            match op {
                Op::Click { .. } => {
                    self.n += 1;
                    Ok(DispatchResult::empty())
                }
                Op::Shutdown => Ok(DispatchResult::empty()),
                // What a 0.1 host does with a gesture: an `invoke` name.
                Op::Invoke { name, .. } => {
                    Ok(DispatchResult::json(serde_json::json!({ "invoked": name })))
                }
                _ => Ok(DispatchResult::empty()),
            }
        }

        fn screenshot(
            &self,
            spec: crate::scroll_capture::ScreenshotSpec<'_>,
        ) -> Result<DispatchResult, String> {
            let path = crate::require_screenshot_path(spec.path)?;
            crate::write_png(path, crate::TEST_PNG).map(DispatchResult::json)
        }
    }

    #[test]
    fn test_host_roundtrip_click() {
        let host = TestHost::spawn(PingHost { n: 0 }, None).expect("bind");
        let mut client = host.client();
        client.wait_ready().expect("hello");
        client.click("ping-window").expect("click");
        let tree = client.snapshot().expect("snap").tree.expect("tree");
        assert_eq!(
            tree.find("ping-window").unwrap().value.as_deref(),
            Some("1")
        );
        assert_eq!(host.host().lock().unwrap().n, 1);
    }

    #[test]
    fn inline_screenshot_returns_the_png_and_leaves_no_file() {
        let host = TestHost::spawn(PingHost { n: 0 }, None).expect("bind");
        let mut client = host.client();
        let before = std::fs::read_dir(crate::screenshot_base_dir())
            .map(|dir| dir.count())
            .unwrap_or(0);
        let png = client
            .screenshot_bytes(crate::ScreenshotMode::Viewport, None, None)
            .expect("inline screenshot");
        assert_eq!(png, crate::TEST_PNG);
        let after = std::fs::read_dir(crate::screenshot_base_dir())
            .map(|dir| dir.count())
            .unwrap_or(0);
        assert_eq!(before, after, "the scratch file is removed");
    }

    #[test]
    fn a_current_host_that_omits_a_feature_is_never_sent_the_op() {
        let host = TestHost::spawn(PingHost { n: 0 }, None).expect("bind");
        let mut client = host.client();
        let scroll = Op::Scroll {
            target: Some("list".into()),
            x: None,
            y: None,
            dx: 0.0,
            dy: 40.0,
        };
        // This host runs the current dispatcher, so leaving `scroll` out
        // of its features is a statement: it is not sent as an `invoke`.
        let err = client.rpc_gated(&scroll).unwrap_err();
        assert!(crate::is_unsupported_op(&err), "{err}");

        for op in [
            Op::Swipe {
                target: None,
                direction: crate::SwipeDirection::Up,
                distance: None,
            },
            Op::Press {
                target: Some("save".into()),
                x: None,
                y: None,
                expect_fingerprint: Some(42),
            },
        ] {
            let err = client.gated(op).unwrap_err();
            assert!(crate::is_unsupported_op(&err), "{err}");
        }

        // Sent as written, the op reaches the host, and the refusal is the
        // host's own: a response off the wire, not a local error.
        let mut blunt = host.client().as_written();
        let resp = blunt.rpc_gated(&scroll).expect("the host answers");
        assert!(!resp.ok);
        let error = resp.error.expect("an error");
        assert!(crate::is_unsupported_op(&error), "{error}");
    }
}
