use std::io::{BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::dispatch::{TreeCache, authorize_request, handle_request_cached};
use crate::hmac_auth::{challenge_for_nonce, random_nonce};
use crate::host::AgentHost;
use crate::mailbox::AgentMailbox;
use crate::ndjson::{line_is_blank, read_limited_line_into, write_json_line};
use crate::protocol::{Op, Request, Response};

/// Re-export so existing `server::read_limited_line` callers keep compiling.
pub use crate::ndjson::read_limited_line;

pub const DEFAULT_ADDR_STR: &str = "127.0.0.1:17421";
pub const DEFAULT_PORT: u16 = 17421;

/// Reject a single NDJSON line larger than this (1 MiB).
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Drop new TCP clients when this many handler threads are already running.
pub const MAX_CONNECTIONS: usize = 32;
pub(crate) const CLIENT_SESSION_IDLE_RECONNECT_AFTER: Duration = Duration::from_secs(20);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn default_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT))
}

fn publish_discovery_record(
    addr: SocketAddr,
    app: &str,
) -> Option<crate::discovery::PublishedHost> {
    match crate::discovery::publish_current(addr, app) {
        Ok(published) => Some(published),
        Err(error) => {
            log::warn!("could not publish AgentHost discovery record for {addr}: {error}");
            None
        }
    }
}

/// Tunables for the localhost server. Defaults are conservative for a
/// developer control plane; tests may tighten them.
#[derive(Debug, Clone)]
pub struct ServerLimits {
    pub max_line_bytes: usize,
    pub max_connections: usize,
    pub idle_timeout: Duration,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: MAX_LINE_BYTES,
            max_connections: MAX_CONNECTIONS,
            idle_timeout: IDLE_TIMEOUT,
        }
    }
}

/// Localhost NDJSON server. One JSON object per line, request → response.
pub struct AgentServer {
    listener: TcpListener,
    token: Option<String>,
    shutdown: Arc<AtomicBool>,
    limits: ServerLimits,
    /// The `app` its discovery record carries. `serve_host` asks the host's `hello()`; a mailbox
    /// has no host to ask, so it uses this, or the executable's name when unset.
    app: Option<String>,
}

impl AgentServer {
    pub fn bind(addr: SocketAddr, token: Option<String>) -> std::io::Result<Self> {
        Self::bind_with_limits(addr, token, ServerLimits::default())
    }

    pub fn bind_with_limits(
        addr: SocketAddr,
        token: Option<String>,
        limits: ServerLimits,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        // The serve loops poll `accept` so they notice a shutdown request.
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            token,
            shutdown: Arc::new(AtomicBool::new(false)),
            limits,
            app: None,
        })
    }

    /// The app name `--connect` can find a mailbox host by (its `hello().app`, usually).
    pub fn with_app(mut self, app: impl Into<String>) -> Self {
        self.app = Some(app.into());
        self
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn shutdown_handle(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }

    /// Serve a mutex-protected host on this thread (plus one thread per
    /// accepted connection). Used by `todo-headless` and integration tests.
    pub fn serve_host<H: AgentHost + 'static>(self, host: Arc<Mutex<H>>) {
        let _published = match self.local_addr() {
            Ok(addr) => {
                let app = host
                    .lock()
                    .map(|guard| guard.hello().app)
                    .unwrap_or_default();
                publish_discovery_record(addr, &app)
            }
            Err(_) => None,
        };
        let token = self.token.clone();
        let shutdown = self.shutdown.clone();
        let limits = self.limits.clone();
        let inflight = Arc::new(AtomicUsize::new(0));

        while !shutdown.load(Ordering::SeqCst) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if !claim_connection(&inflight, limits.max_connections) {
                        drop(stream);
                        continue;
                    }
                    let host = host.clone();
                    let token = token.clone();
                    let shutdown = shutdown.clone();
                    let inflight = inflight.clone();
                    let limits = limits.clone();
                    thread::spawn(move || {
                        let _guard = InflightGuard(inflight);
                        handle_stream_host(stream, host, token.as_deref(), &shutdown, &limits);
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    }

    /// Serve by posting each request onto a mailbox the UI thread drains.
    ///
    /// Like `serve_host`, it writes a discovery record for as long as it serves, so
    /// `--connect` finds a GPUI app that runs the agent this way (#47).
    pub fn serve_mailbox(self, mailbox: AgentMailbox, timeout: Duration) {
        let _published = match self.local_addr() {
            Ok(addr) => {
                let app = self
                    .app
                    .clone()
                    .unwrap_or_else(|| crate::discovery::detect_identity().executable);
                publish_discovery_record(addr, &app)
            }
            Err(_) => None,
        };
        let token = self.token.clone();
        let shutdown = self.shutdown.clone();
        let limits = self.limits.clone();
        let inflight = Arc::new(AtomicUsize::new(0));

        while !shutdown.load(Ordering::SeqCst) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if !claim_connection(&inflight, limits.max_connections) {
                        drop(stream);
                        continue;
                    }
                    let mailbox = mailbox.clone();
                    let token = token.clone();
                    let shutdown = shutdown.clone();
                    let inflight = inflight.clone();
                    let limits = limits.clone();
                    thread::spawn(move || {
                        let _guard = InflightGuard(inflight);
                        handle_stream_mailbox(
                            stream,
                            mailbox,
                            token.as_deref(),
                            timeout,
                            &shutdown,
                            &limits,
                        );
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    }
}

struct InflightGuard(Arc<AtomicUsize>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn claim_connection(inflight: &AtomicUsize, max: usize) -> bool {
    let prev = inflight.fetch_add(1, Ordering::SeqCst);
    if prev >= max {
        inflight.fetch_sub(1, Ordering::SeqCst);
        false
    } else {
        true
    }
}

fn prepare_stream(stream: &TcpStream, idle: Duration) -> bool {
    // Accepted sockets inherit nonblocking from the listener on macOS/BSD.
    // Timeouts only work on blocking sockets; a nonblocking read returns
    // WouldBlock and the handler used to drop the connection with no reply.
    if stream.set_nonblocking(false).is_err() {
        return false;
    }
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(idle)).is_ok() && stream.set_write_timeout(Some(idle)).is_ok()
}

/// Every reply leaves through here, so every failure the server sends
/// carries its stable `error_code` and `hint`.
fn write_resp(writer: &mut TcpStream, encode_buf: &mut Vec<u8>, resp: &Response) {
    if !resp.ok && resp.error_code.is_none() {
        let mut coded = resp.clone();
        coded.fill_host_reply_code();
        let _ = write_json_line(writer, encode_buf, &coded);
        return;
    }
    let _ = write_json_line(writer, encode_buf, resp);
}

fn begin_session(
    writer: &mut TcpStream,
    encode_buf: &mut Vec<u8>,
    token: Option<&str>,
) -> Result<Option<[u8; 32]>, ()> {
    let Some(_token) = token else {
        return Ok(None);
    };
    let nonce = random_nonce().map_err(|_| ())?;
    let challenge = challenge_for_nonce(&nonce);
    write_json_line(writer, encode_buf, &challenge).map_err(|_| ())?;
    writer.flush().map_err(|_| ())?;
    Ok(Some(nonce))
}

fn handle_stream_host<H: AgentHost>(
    stream: TcpStream,
    host: Arc<Mutex<H>>,
    token: Option<&str>,
    shutdown: &AtomicBool,
    limits: &ServerLimits,
) {
    if !prepare_stream(&stream, limits.idle_timeout) {
        return;
    }
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line_buf = Vec::with_capacity(4096);
    let mut encode_buf = Vec::with_capacity(4096);
    let session_nonce = match begin_session(&mut writer, &mut encode_buf, token) {
        Ok(nonce) => nonce,
        Err(()) => return,
    };
    // One TreeCache per connection: snapshot/assert/wait_until reuse the
    // cached tree while the host's revision is unchanged (M3).
    let mut cache = TreeCache::new();
    loop {
        match read_limited_line_into(&mut reader, &mut line_buf, limits.max_line_bytes) {
            Ok(true) => {}
            Ok(false) => break,
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                write_resp(
                    &mut writer,
                    &mut encode_buf,
                    &Response::err("?", format!("bad request: {err}")),
                );
                break;
            }
            Err(_) => break,
        }
        if line_is_blank(&line_buf) {
            continue;
        }
        let req: Request = match serde_json::from_slice(&line_buf) {
            Ok(req) => req,
            Err(err) => {
                write_resp(
                    &mut writer,
                    &mut encode_buf,
                    &Response::err("?", format!("bad json: {err}")),
                );
                break;
            }
        };
        if let Err(resp) =
            authorize_request(&req, token, session_nonce.as_ref().map(|n| n.as_slice()))
        {
            write_resp(&mut writer, &mut encode_buf, &resp);
            break;
        }
        let shutdown_op = matches!(req.op, Op::Shutdown);
        let confirmed_quit = crate::op_is_confirmed_quit(&req.op);
        let timeout_ms = match &req.op {
            Op::Wait {
                timeout_ms: Some(timeout_ms),
            } => Some(*timeout_ms),
            Op::WaitUntil { timeout_ms, .. } => Some(*timeout_ms),
            _ => None,
        };
        let resp = if let Some(timeout_ms) = timeout_ms {
            let deadline = crate::dispatch::deadline_after(Duration::from_millis(timeout_ms));
            loop {
                let response = {
                    let host = host.lock().expect("host");
                    crate::dispatch::wait_step(&*host, &mut cache, &req, token, deadline)
                };
                if let Some(response) = response {
                    break response;
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                thread::sleep(remaining.min(Duration::from_millis(10)));
            }
        } else {
            let mut host = host.lock().expect("host");
            handle_request_cached(
                &mut *host,
                &mut cache,
                req,
                token,
                session_nonce.as_ref().map(|n| n.as_slice()),
            )
        };
        write_resp(&mut writer, &mut encode_buf, &resp);
        if shutdown_op || (confirmed_quit && resp.ok) {
            shutdown.store(true, Ordering::SeqCst);
            break;
        }
    }
}

fn handle_stream_mailbox(
    stream: TcpStream,
    mailbox: AgentMailbox,
    token: Option<&str>,
    timeout: Duration,
    shutdown: &AtomicBool,
    limits: &ServerLimits,
) {
    if !prepare_stream(&stream, limits.idle_timeout) {
        return;
    }
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line_buf = Vec::with_capacity(4096);
    let mut encode_buf = Vec::with_capacity(4096);
    let session_nonce = match begin_session(&mut writer, &mut encode_buf, token) {
        Ok(nonce) => nonce,
        Err(()) => return,
    };
    loop {
        match read_limited_line_into(&mut reader, &mut line_buf, limits.max_line_bytes) {
            Ok(true) => {}
            Ok(false) => break,
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                write_resp(
                    &mut writer,
                    &mut encode_buf,
                    &Response::err("?", format!("bad request: {err}")),
                );
                break;
            }
            Err(_) => break,
        }
        if line_is_blank(&line_buf) {
            continue;
        }
        let req: Request = match serde_json::from_slice(&line_buf) {
            Ok(req) => req,
            Err(err) => {
                write_resp(
                    &mut writer,
                    &mut encode_buf,
                    &Response::err("?", format!("bad json: {err}")),
                );
                break;
            }
        };
        // Authorize here so virtual ops (which skip handle_request on the
        // UI thread) still get version + token checks.
        if let Err(resp) =
            authorize_request(&req, token, session_nonce.as_ref().map(|n| n.as_slice()))
        {
            write_resp(&mut writer, &mut encode_buf, &resp);
            break;
        }
        let shutdown_op = matches!(req.op, Op::Shutdown);
        let confirmed_quit = crate::op_is_confirmed_quit(&req.op);
        // Inline screenshots and blob reads are served on this thread, so
        // a UI-thread host that only writes path screenshots needs no change.
        let mut req = req;
        let inline_scratch = crate::dispatch::prepare_inline_screenshot(&mut req.op);
        let mut resp = if let Op::Blob { blob, index } = &req.op {
            match crate::blob::read_chunk(blob, *index) {
                Ok(value) => {
                    let mut resp = Response::ok(req.id.clone());
                    resp.result = Some(value);
                    resp
                }
                Err(error) => Response::err(req.id.clone(), error),
            }
        } else if matches!(req.op, Op::WaitUntil { .. }) {
            wait_until_via_mailbox(&mailbox, &req, timeout)
        } else if let Op::Wait {
            timeout_ms: Some(timeout_ms),
        } = req.op
        {
            wait_ready_via_mailbox(&mailbox, &req, timeout_ms, timeout)
        } else {
            let id = req.id.clone();
            mailbox
                .wait(req, timeout)
                .unwrap_or_else(|err| Response::err(id, err))
        };
        if let Some(scratch) = inline_scratch {
            crate::dispatch::finish_inline_screenshot(&mut resp, scratch);
        }
        if let Some(hello) = resp.hello.as_mut() {
            hello.auth = crate::protocol::HelloAuth::from_token_configured(token);
            if !hello
                .features
                .iter()
                .any(|feature| feature == "screenshot_inline")
            {
                hello.features.push("screenshot_inline".into());
            }
        }
        write_resp(&mut writer, &mut encode_buf, &resp);
        if shutdown_op || (confirmed_quit && resp.ok) {
            shutdown.store(true, Ordering::SeqCst);
            break;
        }
    }
}

/// Poll readiness from the TCP thread so a timed `wait` never parks the UI
/// thread: each poll posts an immediate `wait` (a hello) and the loop
/// sleeps here, between drains.
fn wait_ready_via_mailbox(
    mailbox: &AgentMailbox,
    req: &Request,
    timeout_ms: u64,
    drain_timeout: Duration,
) -> Response {
    let deadline = crate::dispatch::deadline_after(Duration::from_millis(timeout_ms));
    loop {
        let poll = Request {
            v: req.v,
            id: req.id.clone(),
            token: req.token.clone(),
            auth: req.auth.clone(),
            op: Op::Wait { timeout_ms: None },
        };
        let resp = match mailbox.wait(poll, drain_timeout) {
            Ok(resp) => resp,
            Err(err) => return Response::err(req.id.clone(), err),
        };
        if !resp.ok || resp.hello.as_ref().is_some_and(|hello| hello.ready) {
            return resp;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Response::err(req.id.clone(), "wait timed out");
        }
        thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

/// Poll assert on the UI thread from the TCP thread so `wait_until` does
/// not sleep inside `render` (toasts / overlays can paint between polls).
///
/// When the host tracks a revision, the loop posts a cheap `Revision`
/// probe each poll and only posts the tree-building `Assert` when the
/// revision actually changed — an unchanged tree costs zero rebuilds
/// (M3.5). A host that answers `revision` with `null` gets the old
/// every-poll behavior.
fn wait_until_via_mailbox(
    mailbox: &AgentMailbox,
    req: &Request,
    drain_timeout: Duration,
) -> Response {
    let (timeout_ms, spec) = match &req.op {
        Op::WaitUntil { timeout_ms, spec } => (*timeout_ms, spec.clone()),
        _ => {
            return Response::err(
                req.id.clone(),
                "wait_until_via_mailbox requires op wait_until",
            );
        }
    };
    let deadline = crate::dispatch::deadline_after(Duration::from_millis(timeout_ms));
    // Revision the last Assert covered. `None` = "never checked".
    let mut checked_rev: Option<Option<u64>> = None;
    loop {
        let probe = Request {
            v: req.v,
            id: req.id.clone(),
            token: req.token.clone(),
            auth: req.auth.clone(),
            op: Op::Revision,
        };
        let mut probed_rev = None;
        if let Ok(probe_resp) = mailbox.wait(probe, drain_timeout) {
            let rev = probe_resp.rev;
            if rev.is_some() && checked_rev == Some(rev) {
                // Same revision already asserted — nothing to re-check.
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Response::err(req.id.clone(), "wait_until timed out");
                }
                thread::sleep(remaining.min(Duration::from_millis(10)));
                continue;
            }
            probed_rev = rev;
        }

        let poll = Request {
            v: req.v,
            id: req.id.clone(),
            token: req.token.clone(),
            auth: req.auth.clone(),
            op: Op::Assert { spec: spec.clone() },
        };
        let resp = match mailbox.wait(poll, drain_timeout) {
            Ok(resp) => resp,
            Err(err) => return Response::err(req.id.clone(), err),
        };
        if resp.ok {
            return resp;
        }
        // The assert covered the tree at the probed revision — a later
        // mutation bumps it, so the next probe either matches (skip) or
        // differs (assert again). A rev change between probe and assert
        // is caught the same way.
        if probed_rev.is_some() {
            checked_rev = Some(probed_rev);
        }
        let last_err = resp.error.unwrap_or_else(|| "assert failed".into());
        if crate::is_in_viewport_unavailable(&last_err) && last_err.contains("headless") {
            return Response::err(req.id.clone(), last_err);
        }
        if std::time::Instant::now() >= deadline {
            return Response::err(req.id.clone(), format!("wait_until timed out: {last_err}"));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

/// Spawn `serve_host` on a background thread and return the bound address.
pub fn spawn_host<H: AgentHost + 'static>(
    addr: SocketAddr,
    token: Option<String>,
    host: Arc<Mutex<H>>,
) -> std::io::Result<(SocketAddr, Arc<AtomicBool>)> {
    let server = AgentServer::bind(addr, token)?;
    let bound = server.local_addr()?;
    let flag = server.shutdown_handle();
    thread::spawn(move || server.serve_host(host));
    Ok((bound, flag))
}

/// Spawn `serve_mailbox` on a background thread. Its discovery record names the app after the
/// executable; `spawn_mailbox_named` gives it the app's own name.
pub fn spawn_mailbox(
    addr: SocketAddr,
    token: Option<String>,
    mailbox: AgentMailbox,
    timeout: Duration,
) -> std::io::Result<(SocketAddr, Arc<AtomicBool>)> {
    let server = AgentServer::bind(addr, token)?;
    spawn_mailbox_server(server, mailbox, timeout)
}

/// `spawn_mailbox`, listed under `app` (what `hello().app` answers) so `--connect <app>` finds
/// it whatever the executable is called.
pub fn spawn_mailbox_named(
    addr: SocketAddr,
    token: Option<String>,
    mailbox: AgentMailbox,
    timeout: Duration,
    app: &str,
) -> std::io::Result<(SocketAddr, Arc<AtomicBool>)> {
    let server = AgentServer::bind(addr, token)?.with_app(app);
    spawn_mailbox_server(server, mailbox, timeout)
}

fn spawn_mailbox_server(
    server: AgentServer,
    mailbox: AgentMailbox,
    timeout: Duration,
) -> std::io::Result<(SocketAddr, Arc<AtomicBool>)> {
    let bound = server.local_addr()?;
    let flag = server.shutdown_handle();
    thread::spawn(move || server.serve_mailbox(mailbox, timeout));
    Ok((bound, flag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::AgentClient;
    use crate::protocol::{HelloInfo, Op, PROTOCOL_VERSION, PlatformKind, Request};
    use crate::tree::UiTree;
    use crate::{DeliveryMode, DispatchResult};
    use std::io::{BufRead, Cursor, Read, Write};

    struct EmptyHost;

    impl AgentHost for EmptyHost {
        fn hello(&self) -> HelloInfo {
            HelloInfo {
                os: crate::protocol::host_os(),
                protocol: PROTOCOL_VERSION,
                framework: None,
                device: None,
                app: "test".into(),
                platform: PlatformKind::Headless,
                ready: true,
                deliveries: vec![DeliveryMode::Semantic],
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
                nodes: vec![],
                rev: None,
            }
        }

        fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
            Ok(DispatchResult::empty())
        }
    }

    fn spawn_test_host(
        token: Option<String>,
        limits: ServerLimits,
    ) -> (SocketAddr, Arc<AtomicBool>) {
        let host = Arc::new(Mutex::new(EmptyHost));
        let server =
            AgentServer::bind_with_limits("127.0.0.1:0".parse().unwrap(), token, limits).unwrap();
        let bound = server.local_addr().unwrap();
        let flag = server.shutdown_handle();
        thread::spawn(move || server.serve_host(host));
        (bound, flag)
    }

    #[test]
    fn largest_wire_wait_timeout_leaves_the_server_serving() {
        let (addr, shutdown) = spawn_test_host(None, ServerLimits::default());
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        let resp = client
            .rpc(Op::Wait {
                timeout_ms: Some(u64::MAX),
            })
            .expect("wait reply");
        assert!(resp.ok, "{resp:?}");
        let mut fresh = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        assert!(fresh.expect_ok(Op::Hello).is_ok(), "the host lock survived");
        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn limited_line_accepts_short_utf8() {
        let mut cursor = Cursor::new("hello\nmore");
        let line = read_limited_line(&mut cursor, 16).unwrap().unwrap();
        assert_eq!(line, "hello");
    }

    #[test]
    fn limited_line_rejects_oversize() {
        let mut cursor = Cursor::new("x".repeat(8) + "\n");
        let err = read_limited_line(&mut cursor, 4).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn oversized_line_is_rejected_over_tcp() {
        let (addr, shutdown) = spawn_test_host(
            None,
            ServerLimits {
                max_line_bytes: 64,
                max_connections: 4,
                idle_timeout: Duration::from_secs(2),
            },
        );

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let attack = "x".repeat(200) + "\n";
        stream.write_all(attack.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut resp = String::new();
        reader.read_line(&mut resp).unwrap();
        assert!(resp.contains("line too long"), "{resp}");

        // Connection must not stay open for a follow-up hello.
        let mut hello = Request::new("1", Op::Hello);
        hello.v = PROTOCOL_VERSION;
        let line = serde_json::to_string(&hello).unwrap();
        let write_ok = writeln!(reader.get_mut(), "{line}");
        assert!(
            write_ok.is_err() || {
                let mut second = String::new();
                reader.read_line(&mut second).is_err() || second.is_empty()
            }
        );

        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn token_mismatch_disconnects() {
        let (addr, shutdown) = spawn_test_host(
            Some("correct-token".into()),
            ServerLimits {
                max_line_bytes: 4096,
                max_connections: 4,
                idle_timeout: Duration::from_secs(2),
            },
        );

        let mut client = AgentClient::connect(addr)
            .with_token("wrong-token")
            .with_timeout(Duration::from_secs(2));
        let resp = client.rpc(Op::Hello).expect("got a response");
        assert!(!resp.ok);
        assert!(
            resp.error.as_deref() == Some("invalid automation token"),
            "{resp:?}"
        );

        // Next RPC must open a new connection (old one is closed). Hello
        // with the right token still works.
        let mut ok = AgentClient::connect(addr)
            .with_token("correct-token")
            .with_timeout(Duration::from_secs(2));
        let hello = ok.expect_ok(Op::Hello).expect("authed hello");
        assert_eq!(
            hello.hello.unwrap().auth,
            crate::protocol::HelloAuth::Required
        );

        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn connection_cap_drops_extra_clients() {
        let (addr, shutdown) = spawn_test_host(
            None,
            ServerLimits {
                max_line_bytes: 4096,
                max_connections: 1,
                idle_timeout: Duration::from_secs(3),
            },
        );

        let holder = TcpStream::connect(addr).unwrap();
        // Give the accept loop time to claim the slot.
        thread::sleep(Duration::from_millis(80));

        let mut extra = TcpStream::connect(addr).unwrap();
        extra
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        let hello = serde_json::to_string(&Request::new("1", Op::Hello)).unwrap();
        let _ = writeln!(extra, "{hello}");
        let mut buf = String::new();
        let mut reader = BufReader::new(extra);
        let read = reader.read_line(&mut buf);
        assert!(
            read.is_err() || buf.is_empty(),
            "over-cap client should not be served, got {read:?} {buf:?}"
        );

        drop(holder);
        thread::sleep(Duration::from_millis(80));
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        assert!(client.expect_ok(Op::Hello).is_ok());

        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn rpc_pipeline_wrong_token_fails_fast() {
        let (addr, shutdown) = spawn_test_host(
            Some("correct-token".into()),
            ServerLimits {
                idle_timeout: Duration::from_secs(2),
                ..ServerLimits::default()
            },
        );
        let hello = Op::Hello;
        let ops: Vec<&Op> = vec![&hello, &hello, &hello];
        let mut bad = AgentClient::connect(addr)
            .with_token("wrong")
            .with_timeout(Duration::from_secs(2));
        let started = std::time::Instant::now();
        let err = bad.rpc_pipeline(&ops);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(800),
            "auth close must not retry the wave until timeout: {elapsed:?} {err:?}"
        );
        match err {
            Ok(resps) => assert!(
                resps
                    .iter()
                    .any(|r| !r.ok && r.error.as_deref().is_some_and(|e| e.contains("token"))),
                "wrong token must not run the wave: {resps:?}"
            ),
            Err(msg) => assert!(
                msg.contains("token")
                    || msg.contains("connection closed")
                    || msg.to_ascii_lowercase().contains("reset")
                    || msg.contains("Broken pipe")
                    || msg.contains("os error 32")
                    // Windows WSAECONNRESET
                    || msg.contains("os error 10054"),
                "wrong token must not run the wave: {msg}"
            ),
        }
        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn rpc_pipeline_eof_after_first_reply_does_not_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                drop(listener);
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let _ = writeln!(stream, r#"{{"v":{PROTOCOL_VERSION},"id":"1","ok":true}}"#);
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });

        let hello = Op::Hello;
        let ops: Vec<&Op> = vec![&hello, &hello, &hello];
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        let started = std::time::Instant::now();
        let err = client.rpc_pipeline(&ops);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(800),
            "EOF after the first pipelined reply must not retry until timeout: {elapsed:?} {err:?}"
        );
        assert!(err.is_err(), "partial wave must be Fatal, not Ok: {err:?}");
    }

    #[test]
    fn rpc_pipeline_reuses_session_for_hello_wave() {
        let (addr, shutdown) = spawn_test_host(None, ServerLimits::default());
        let hello = Op::Hello;
        let ops: Vec<&Op> = vec![&hello, &hello, &hello];
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        let resps = client.rpc_pipeline(&ops).expect("pipeline hellos");
        assert_eq!(resps.len(), 3);
        assert!(resps.iter().all(|r| r.ok), "{resps:?}");
        assert!(client.has_session());
        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn client_reuses_one_tcp_session_for_many_ops() {
        let (addr, shutdown) = spawn_test_host(None, ServerLimits::default());
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        assert!(!client.has_session());
        client.expect_ok(Op::Hello).unwrap();
        assert!(client.has_session());
        for _ in 0..16 {
            client.expect_ok(Op::Hello).unwrap();
            assert!(client.has_session(), "session should survive hello");
        }
        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn rpc_once_drops_session_then_rpc_reconnects() {
        let (addr, shutdown) = spawn_test_host(None, ServerLimits::default());
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        client.rpc_once(Op::Hello).expect("rpc_once");
        assert!(
            !client.has_session(),
            "rpc_once is the reconnect/bench path and must drop the socket"
        );
        client.expect_ok(Op::Hello).unwrap();
        assert!(client.has_session(), "rpc should open a reusable session");
        client.close_session();
        assert!(!client.has_session());
        client.expect_ok(Op::Hello).unwrap();
        assert!(client.has_session());
        shutdown.store(true, Ordering::SeqCst);
    }

    fn spawn_echo_peer(reply: Vec<u8>) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = Read::read(&mut stream, &mut buf);
                let _ = stream.write_all(&reply);
            }
        });
        addr
    }

    #[test]
    fn client_rejects_invalid_json_response() {
        let addr = spawn_echo_peer(b"not-json\n".to_vec());
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_millis(400));
        let err = client.rpc(Op::Hello).unwrap_err();
        assert!(
            err.contains("bad response") || err.contains("connect"),
            "{err}"
        );
        assert!(!client.has_session());
    }

    #[test]
    fn client_rejects_oversized_response_line() {
        let mut reply = vec![b'x'; crate::MAX_LINE_BYTES + 8];
        reply.push(b'\n');
        let addr = spawn_echo_peer(reply);
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_millis(800));
        let err = client.rpc(Op::Hello).unwrap_err();
        assert!(
            err.contains("line too long") || err.contains("connect"),
            "{err}"
        );
        assert!(!client.has_session());
    }

    #[test]
    fn mailbox_hello_auth_matches_server_token() {
        let mailbox = AgentMailbox::new();
        let drain = mailbox.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        thread::spawn(move || {
            let mut host = EmptyHost;
            while !stop_t.load(Ordering::SeqCst) {
                for posted in drain.take() {
                    let resp = crate::handle_request(&mut host, posted.request.clone(), None, None);
                    posted.reply(resp);
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        let (addr, shutdown) = spawn_mailbox(
            "127.0.0.1:0".parse().unwrap(),
            Some("secret".into()),
            mailbox,
            Duration::from_secs(2),
        )
        .expect("bind mailbox");
        let mut client = AgentClient::connect(addr)
            .with_token("secret")
            .with_timeout(Duration::from_secs(2));
        let resp = client.rpc(Op::Hello).expect("hello");
        assert!(resp.ok, "{resp:?}");
        assert_eq!(
            resp.hello.expect("hello payload").auth,
            crate::protocol::HelloAuth::Required
        );
        shutdown.store(true, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
    }

    /// Drain `mailbox` like a UI thread whose app turns ready at the
    /// `ready_after`-th poll; returns the stop flag and whether any posted
    /// request would have parked the UI thread in a timed wait.
    fn drain_readiness_polls(
        mailbox: &AgentMailbox,
        ready_after: Option<usize>,
    ) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
        let drain = mailbox.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let parked = Arc::new(AtomicBool::new(false));
        let (stop_t, parked_t) = (Arc::clone(&stop), Arc::clone(&parked));
        thread::spawn(move || {
            let mut polls = 0;
            while !stop_t.load(Ordering::SeqCst) {
                for posted in drain.take() {
                    if matches!(
                        posted.request.op,
                        Op::Wait {
                            timeout_ms: Some(_)
                        }
                    ) {
                        parked_t.store(true, Ordering::SeqCst);
                    }
                    polls += 1;
                    let mut hello = EmptyHost.hello();
                    hello.ready = ready_after.is_some_and(|after| polls >= after);
                    let mut response = Response::ok(posted.request.id.clone());
                    response.hello = Some(hello);
                    posted.reply(response);
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        (stop, parked)
    }

    #[test]
    fn mailbox_timed_wait_polls_without_parking_the_ui_thread() {
        let mailbox = AgentMailbox::new();
        let (stop, parked) = drain_readiness_polls(&mailbox, Some(3));
        let (addr, shutdown) = spawn_mailbox(
            "127.0.0.1:0".parse().unwrap(),
            None,
            mailbox,
            Duration::from_secs(2),
        )
        .expect("bind mailbox");
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        let resp = client
            .rpc(Op::Wait {
                timeout_ms: Some(u64::MAX),
            })
            .expect("wait reply");
        assert!(resp.ok, "{resp:?}");
        assert!(resp.hello.as_ref().is_some_and(|hello| hello.ready));
        assert!(!parked.load(Ordering::SeqCst), "UI thread got a timed wait");
        shutdown.store(true, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn mailbox_timed_wait_times_out_with_the_request_id() {
        let mailbox = AgentMailbox::new();
        let (stop, parked) = drain_readiness_polls(&mailbox, None);
        let (addr, shutdown) = spawn_mailbox(
            "127.0.0.1:0".parse().unwrap(),
            None,
            mailbox,
            Duration::from_secs(2),
        )
        .expect("bind mailbox");
        let mut stream = TcpStream::connect(addr).expect("connect");
        let mut wait = serde_json::to_vec(&Request::new(
            "w-1",
            Op::Wait {
                timeout_ms: Some(30),
            },
        ))
        .expect("encode wait");
        wait.push(b'\n');
        stream.write_all(&wait).expect("send wait");
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).expect("reply");
        let resp: Response = serde_json::from_str(&line).expect("response json");
        assert_eq!(resp.id, "w-1");
        assert_eq!(resp.error.as_deref(), Some("wait timed out"));
        assert!(!parked.load(Ordering::SeqCst), "UI thread got a timed wait");
        shutdown.store(true, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn undrained_mailbox_times_out_with_the_request_id() {
        let mailbox = AgentMailbox::new();
        let (addr, shutdown) = spawn_mailbox(
            "127.0.0.1:0".parse().unwrap(),
            None,
            mailbox,
            Duration::from_millis(30),
        )
        .expect("bind mailbox");
        let mut stream = TcpStream::connect(addr).expect("connect");
        let mut revision =
            serde_json::to_vec(&Request::new("r-7", Op::Revision)).expect("encode revision");
        revision.push(b'\n');
        stream.write_all(&revision).expect("send revision");
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).expect("reply");
        let resp: Response = serde_json::from_str(&line).expect("response json");
        assert_eq!(resp.id, "r-7");
        assert_eq!(resp.error_code.as_deref(), Some(crate::error_code::TIMEOUT));
        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn mailbox_hello_adds_inline_screenshot_feature() {
        let mailbox = AgentMailbox::new();
        let drain = mailbox.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                for posted in drain.take() {
                    let hello = EmptyHost.hello();
                    assert!(hello.features.is_empty());
                    let mut response = Response::ok(posted.request.id.clone());
                    response.hello = Some(hello);
                    posted.reply(response);
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        let (addr, shutdown) = spawn_mailbox(
            "127.0.0.1:0".parse().unwrap(),
            None,
            mailbox,
            Duration::from_secs(2),
        )
        .expect("bind mailbox");
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        let response = client.rpc(Op::Hello).expect("hello");
        let hello = response.hello.expect("hello payload");
        assert!(
            hello
                .features
                .iter()
                .any(|feature| feature == "screenshot_inline"),
            "{:?}",
            hello.features
        );
        shutdown.store(true, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn v2_untokened_server_does_not_send_challenge() {
        let (addr, shutdown) = spawn_test_host(None, ServerLimits::default());
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut buf = [0u8; 16];
        let n = stream.read(&mut buf);
        match n {
            Ok(0) | Err(_) => {}
            Ok(got) => panic!(
                "untokened server must not write a challenge first: {:?}",
                &buf[..got]
            ),
        }
        let line = format!(r#"{{"v":{PROTOCOL_VERSION},"id":"1","op":"hello"}}"#);
        writeln!(stream, "{line}").unwrap();
        let mut reader = BufReader::new(stream);
        let mut resp = String::new();
        reader.read_line(&mut resp).unwrap();
        assert!(resp.contains("\"ok\":true"), "{resp}");
        assert!(!resp.contains("challenge"), "{resp}");
        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn bad_json_after_challenge_closes_connection() {
        let (addr, shutdown) = spawn_test_host(
            Some("secret".into()),
            ServerLimits {
                idle_timeout: Duration::from_secs(2),
                ..ServerLimits::default()
            },
        );
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut challenge = String::new();
        reader.read_line(&mut challenge).unwrap();
        let nonce = crate::hmac_auth::parse_challenge_line(challenge.trim_end().as_bytes())
            .expect("challenge");
        writeln!(stream, "GET / HTTP/1.1").unwrap();
        let mut err_line = String::new();
        reader.read_line(&mut err_line).unwrap();
        assert!(
            err_line.contains("bad json"),
            "HTTP line must be bad json, got {err_line}"
        );
        assert!(
            err_line.contains(r#""error_code":"bad_request""#),
            "the server codes its own refusals: {err_line}"
        );
        let auth = crate::hmac_auth::hmac_hex("secret", &nonce).unwrap();
        let hello = format!(r#"{{"v":{PROTOCOL_VERSION},"id":"1","auth":"{auth}","op":"hello"}}"#);
        let write_hello = writeln!(stream, "{hello}");
        let mut second = String::new();
        let n = reader.read_line(&mut second);
        let closed = write_hello.is_err() || matches!(n, Ok(0) | Err(_)) || second.is_empty();
        assert!(
            closed && !second.contains("\"ok\":true"),
            "non-JSON after challenge must close; later HMAC must not be served: write={write_hello:?} read={n:?} {second:?}"
        );
        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn dispatcher_errors_carry_error_code_and_hint() {
        let (addr, shutdown) = spawn_test_host(None, ServerLimits::default());
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        writeln!(
            stream,
            r#"{{"v":{PROTOCOL_VERSION},"id":"1","op":"assert","target":"no-such-node"}}"#
        )
        .unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert!(!resp.ok, "{line}");
        assert_eq!(resp.error_code.as_deref(), Some("not_found"), "{line}");
        assert!(resp.hint.is_some(), "{line}");
        assert!(
            resp.delivery_status.is_none(),
            "hosts never send it: {line}"
        );
        shutdown.store(true, Ordering::SeqCst);
    }
}
