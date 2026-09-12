//! In-process test host helpers for SDK / recipe E2E.
//!
//! Spawns [`spawn_host`](crate::server::spawn_host) on `127.0.0.1:0` and
//! hands back an [`AgentClient`](crate::client::AgentClient). Screenshot
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
                app: "ping".into(),
                platform: PlatformKind::Headless,
                ready: true,
                deliveries: vec![],
                auth: crate::HelloAuth::None,
            }
        }

        fn snapshot(&self) -> UiTree {
            UiTree {
                app: "ping".into(),
                platform: PlatformKind::Headless,
                ready: true,
                nodes: vec![UiNode::window("ping-window", "Ping").with_value(self.n.to_string())],
            }
        }

        fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
            match op {
                Op::Click { .. } => {
                    self.n += 1;
                    Ok(DispatchResult::empty())
                }
                Op::Shutdown => Ok(DispatchResult::empty()),
                _ => Ok(DispatchResult::empty()),
            }
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
}
