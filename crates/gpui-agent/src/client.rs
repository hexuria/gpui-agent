use std::io::{BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::hmac_auth::{hmac_hex, parse_challenge_line};
use crate::mailbox::MAX_MAILBOX_DEPTH;
use crate::ndjson::{read_limited_line_into, write_json_line};
use crate::protocol::{AssertSpec, DeliveryMode, Op, PROTOCOL_VERSION, Response, ScreenshotMode};
use crate::server::{MAX_LINE_BYTES, default_addr};

/// Blocking NDJSON client used by the CLI, MCP shim, recipes, and tests.
///
/// After the first successful connect, subsequent [`rpc`](Self::rpc) calls
/// reuse the same TCP session (one process, one connection, many ops).
/// [`rpc_pipeline`](Self::rpc_pipeline) writes several request lines then
/// reads (independent **read-only** recipe waves). Use [`rpc_once`](Self::rpc_once)
/// to force the old per-op reconnect path (benchmarks / comparison).
pub struct AgentClient {
    addr: SocketAddr,
    token: Option<String>,
    timeout: Duration,
    next_id: u64,
    session: Option<LiveSession>,
}

struct LiveSession {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    encode_buf: Vec<u8>,
    line_buf: Vec<u8>,
    auth: Option<String>,
}

impl LiveSession {
    fn open(addr: SocketAddr, timeout: Duration, token: Option<&str>) -> Result<Self, String> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_millis(400))
            .map_err(|err| err.to_string())?;
        let _ = stream.set_nodelay(true);
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|err| err.to_string())?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|err| err.to_string())?;
        let writer = stream.try_clone().map_err(|err| err.to_string())?;
        let mut session = Self {
            writer,
            reader: BufReader::new(stream),
            encode_buf: Vec::with_capacity(4096),
            line_buf: Vec::with_capacity(4096),
            auth: None,
        };
        if let Some(token) = token {
            if !read_limited_line_into(&mut session.reader, &mut session.line_buf, MAX_LINE_BYTES)
                .map_err(|err| err.to_string())?
            {
                return Err("connection closed before challenge".into());
            }
            let nonce = parse_challenge_line(&session.line_buf)?;
            session.auth = Some(hmac_hex(token, &nonce)?);
        }
        Ok(session)
    }

    fn set_io_timeout(&mut self, timeout: Duration) -> Result<(), String> {
        for stream in [&self.writer, self.reader.get_ref()] {
            stream
                .set_read_timeout(Some(timeout))
                .map_err(|err| err.to_string())?;
            stream
                .set_write_timeout(Some(timeout))
                .map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    fn exchange_wire(&mut self, id: &str, op: &Op) -> Result<Response, String> {
        let req = WireRequest {
            v: PROTOCOL_VERSION,
            id,
            auth: self.auth.as_deref(),
            op,
        };
        write_json_line(&mut self.writer, &mut self.encode_buf, &req)
            .map_err(|err| err.to_string())?;
        self.writer.flush().map_err(|err| err.to_string())?;
        if !read_limited_line_into(&mut self.reader, &mut self.line_buf, MAX_LINE_BYTES)
            .map_err(|err| err.to_string())?
        {
            return Err("connection closed".into());
        }
        parse_response_line(&self.line_buf)
    }

    /// Write every request line, flush once, then read one response each.
    ///
    /// Connect/write failures **before** the first line may be retried.
    /// After any line is on the wire, errors are fatal so mutating ops
    /// (and even extra reads) are not replayed.
    fn exchange_pipeline(
        &mut self,
        start_id: u64,
        ops: &[&Op],
    ) -> Result<Vec<Response>, PipelineError> {
        let mut id_buf = [0u8; 20];
        let mut wrote = false;
        for (i, op) in ops.iter().enumerate() {
            let id = fmt_u64(start_id + i as u64, &mut id_buf);
            let req = WireRequest {
                v: PROTOCOL_VERSION,
                id,
                auth: self.auth.as_deref(),
                op,
            };
            if let Err(err) = write_json_line(&mut self.writer, &mut self.encode_buf, &req) {
                return Err(if wrote {
                    PipelineError::Fatal(err.to_string())
                } else {
                    PipelineError::Retryable(err.to_string())
                });
            }
            wrote = true;
        }
        self.writer
            .flush()
            .map_err(|err| PipelineError::Fatal(err.to_string()))?;
        let mut out = Vec::with_capacity(ops.len());
        for _ in ops {
            match read_limited_line_into(&mut self.reader, &mut self.line_buf, MAX_LINE_BYTES) {
                Ok(true) => {}
                Ok(false) => return Err(pipeline_eof(&out)),
                Err(err) => return Err(PipelineError::Fatal(err.to_string())),
            }
            out.push(parse_response_line(&self.line_buf).map_err(PipelineError::Fatal)?);
        }
        Ok(out)
    }
}

/// Connect/write failures before any request line is sent may retry.
/// Once a line is on the wire, ops may already have run — do not replay.
enum PipelineError {
    Retryable(String),
    Fatal(String),
}

fn pipeline_eof(partial: &[Response]) -> PipelineError {
    if let Some(resp) = partial.iter().find(|resp| !resp.ok) {
        PipelineError::Fatal(
            resp.error
                .clone()
                .unwrap_or_else(|| "connection closed".into()),
        )
    } else {
        PipelineError::Fatal("connection closed".into())
    }
}

#[derive(serde::Serialize)]
struct WireRequest<'a> {
    v: u32,
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth: Option<&'a str>,
    #[serde(flatten)]
    op: &'a Op,
}

/// Decimal `n` into `buf`. Returns a subslice of `buf`.
fn fmt_u64(n: u64, buf: &mut [u8; 20]) -> &str {
    let mut n = n;
    let mut i = buf.len();
    if n == 0 {
        buf[i - 1] = b'0';
        return std::str::from_utf8(&buf[i - 1..]).unwrap();
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    std::str::from_utf8(&buf[i..]).unwrap()
}

impl AgentClient {
    pub fn connect(addr: SocketAddr) -> Self {
        Self {
            addr,
            token: None,
            timeout: Duration::from_secs(8),
            next_id: 1,
            session: None,
        }
    }

    pub fn default_bind() -> Self {
        Self::connect(default_addr())
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Scrolled capture waits for paint + N window grabs. The default 8s
    /// RPC timeout is shorter than the embedded-host mailbox (30s).
    fn timeout_for(&self, op: &Op) -> Duration {
        match op {
            Op::Screenshot { mode, .. } if mode.is_scrolled() => {
                self.timeout.max(Duration::from_secs(30))
            }
            _ => self.timeout,
        }
    }

    /// True when a live TCP session is being reused.
    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// Drop the live session (next [`rpc`](Self::rpc) reconnects).
    pub fn close_session(&mut self) {
        self.session = None;
    }

    pub fn rpc(&mut self, op: Op) -> Result<Response, String> {
        self.rpc_op(&op)
    }

    /// Like [`rpc`](Self::rpc) but does not take ownership of `op`.
    pub fn rpc_op(&mut self, op: &Op) -> Result<Response, String> {
        self.roundtrip_retry(op)
    }

    /// Write `ops` on the live session, then read one response each.
    ///
    /// Each op is still its own NDJSON request (token, version, 1 MiB line).
    /// Independent **read-only** recipe waves use this to hide localhost RTT.
    /// Chunks larger than [`MAX_MAILBOX_DEPTH`] are split so a pipeline cannot
    /// exceed the mailbox cap. The server still reads/authorizes/handles **one
    /// line at a time** on this connection; the chunk is a client-side ceiling.
    ///
    /// After any request line is written, a transport error is **not** retried
    /// (the host may already have run prefix ops). Connect failures before
    /// the first write still retry until [`Self::with_timeout`].
    pub fn rpc_pipeline(&mut self, ops: &[&Op]) -> Result<Vec<Response>, String> {
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        if ops.len() == 1 {
            return Ok(vec![self.rpc_op(ops[0])?]);
        }
        if ops.len() > MAX_MAILBOX_DEPTH {
            let mut out = Vec::with_capacity(ops.len());
            for chunk in ops.chunks(MAX_MAILBOX_DEPTH) {
                out.extend(self.rpc_pipeline(chunk)?);
            }
            return Ok(out);
        }
        self.pipeline_retry(ops)
    }

    /// One request on a fresh connection, then drop it.
    ///
    /// This is the historical CLI shape (one TCP handshake per op) and
    /// exists so benches can compare it with session reuse.
    pub fn rpc_once(&mut self, op: Op) -> Result<Response, String> {
        self.session = None;
        let resp = self.roundtrip_retry(&op)?;
        self.session = None;
        Ok(resp)
    }

    fn roundtrip_retry(&mut self, op: &Op) -> Result<Response, String> {
        let id_num = self.next_id;
        self.next_id += 1;
        let mut id_buf = [0u8; 20];
        let id = fmt_u64(id_num, &mut id_buf);
        let mut last_err = String::new();
        let timeout = self.timeout_for(op);
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            match self.roundtrip_wire(id, op, timeout) {
                Ok(resp) => return Ok(resp),
                Err(err) => {
                    last_err = err;
                    self.session = None;
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        Err(format!(
            "connect {addr} failed: {last_err}",
            addr = self.addr
        ))
    }

    fn roundtrip_wire(&mut self, id: &str, op: &Op, timeout: Duration) -> Result<Response, String> {
        if self.session.is_none() {
            self.session = Some(LiveSession::open(
                self.addr,
                timeout,
                self.token.as_deref(),
            )?);
        } else if let Some(session) = self.session.as_mut() {
            session.set_io_timeout(timeout)?;
        }
        match self
            .session
            .as_mut()
            .expect("session")
            .exchange_wire(id, op)
        {
            Ok(resp) => Ok(resp),
            Err(err) => {
                self.session = None;
                Err(err)
            }
        }
    }

    fn pipeline_retry(&mut self, ops: &[&Op]) -> Result<Vec<Response>, String> {
        let start_id = self.next_id;
        self.next_id += ops.len() as u64;
        let mut last_err = String::new();
        let deadline = std::time::Instant::now() + self.timeout;
        while std::time::Instant::now() < deadline {
            match self.pipeline_once(start_id, ops) {
                Ok(resps) => return Ok(resps),
                Err(PipelineError::Retryable(err)) => {
                    last_err = err;
                    self.session = None;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(PipelineError::Fatal(err)) => {
                    self.session = None;
                    return Err(err);
                }
            }
        }
        Err(format!(
            "connect {addr} failed: {last_err}",
            addr = self.addr
        ))
    }

    fn pipeline_once(
        &mut self,
        start_id: u64,
        ops: &[&Op],
    ) -> Result<Vec<Response>, PipelineError> {
        if self.session.is_none() {
            match LiveSession::open(self.addr, self.timeout, self.token.as_deref()) {
                Ok(session) => self.session = Some(session),
                Err(err) => return Err(PipelineError::Retryable(err)),
            }
        }
        match self
            .session
            .as_mut()
            .expect("session")
            .exchange_pipeline(start_id, ops)
        {
            Ok(resps) => Ok(resps),
            Err(err) => {
                self.session = None;
                Err(err)
            }
        }
    }

    pub fn expect_ok(&mut self, op: Op) -> Result<Response, String> {
        let resp = self.rpc(op)?;
        if resp.ok {
            Ok(resp)
        } else {
            Err(resp.error.unwrap_or_else(|| "request failed".into()))
        }
    }

    pub fn snapshot(&mut self) -> Result<Response, String> {
        self.expect_ok(Op::Snapshot)
    }

    /// Observe-only PNG of the app surface. `path` is on the host machine.
    pub fn screenshot(&mut self, path: impl Into<String>) -> Result<Response, String> {
        self.expect_ok(Op::screenshot(path))
    }

    /// Opt-in tall capture of a named scroller (`mode=scrolled`).
    pub fn screenshot_scrolled(
        &mut self,
        path: impl Into<String>,
        target: impl Into<String>,
        max_height_px: Option<u32>,
    ) -> Result<Response, String> {
        self.expect_ok(Op::screenshot_scrolled(path, target, max_height_px))
    }

    pub fn screenshot_with(
        &mut self,
        path: impl Into<String>,
        mode: ScreenshotMode,
        target: Option<String>,
        max_height_px: Option<u32>,
    ) -> Result<Response, String> {
        self.expect_ok(Op::Screenshot {
            path: Some(path.into()),
            mode,
            target,
            max_height_px,
        })
    }

    pub fn click(&mut self, target: impl Into<String>) -> Result<Response, String> {
        self.click_with_delivery(target, DeliveryMode::Semantic)
    }

    pub fn click_with_delivery(
        &mut self,
        target: impl Into<String>,
        delivery: DeliveryMode,
    ) -> Result<Response, String> {
        self.expect_ok(Op::Click {
            target: target.into(),
            delivery,
        })
    }

    pub fn type_text(
        &mut self,
        target: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Response, String> {
        self.type_with_delivery(target, text, DeliveryMode::Semantic)
    }

    pub fn type_with_delivery(
        &mut self,
        target: impl Into<String>,
        text: impl Into<String>,
        delivery: DeliveryMode,
    ) -> Result<Response, String> {
        self.expect_ok(Op::Type {
            target: target.into(),
            text: text.into(),
            delivery,
        })
    }

    pub fn key(
        &mut self,
        target: impl Into<String>,
        key: impl Into<String>,
    ) -> Result<Response, String> {
        self.key_with_delivery(target, key, DeliveryMode::Semantic)
    }

    pub fn key_with_delivery(
        &mut self,
        target: impl Into<String>,
        key: impl Into<String>,
        delivery: DeliveryMode,
    ) -> Result<Response, String> {
        self.expect_ok(Op::Key {
            target: target.into(),
            key: key.into(),
            delivery,
        })
    }

    pub fn set_value(
        &mut self,
        target: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Response, String> {
        self.expect_ok(Op::SetValue {
            target: target.into(),
            value: value.into(),
        })
    }

    pub fn assert(&mut self, spec: AssertSpec) -> Result<Response, String> {
        self.expect_ok(Op::Assert { spec })
    }

    pub fn invoke(
        &mut self,
        name: impl Into<String>,
        args: serde_json::Value,
    ) -> Result<Response, String> {
        self.expect_ok(Op::Invoke {
            name: name.into(),
            args,
        })
    }

    pub fn wait_ready(&mut self) -> Result<Response, String> {
        self.expect_ok(Op::Wait { timeout_ms: None })
    }

    pub fn keybindings(&mut self) -> Result<Response, String> {
        self.expect_ok(Op::Keybindings)
    }

    pub fn keybinding(
        &mut self,
        binding: impl Into<String>,
        scope: crate::protocol::KeybindingScope,
        chord: Option<String>,
        confirm: bool,
        activate: bool,
    ) -> Result<Response, String> {
        self.expect_ok(Op::Keybinding {
            binding: binding.into(),
            chord,
            scope,
            confirm,
            activate,
        })
    }
}

fn parse_response_line(line: &[u8]) -> Result<Response, String> {
    if parse_challenge_line(line).is_ok() {
        return Err("automation token required".into());
    }
    serde_json::from_slice(line).map_err(|err| format!("bad response: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Request;

    #[test]
    fn wire_request_matches_owned_request_json() {
        let op = Op::click("todo-add");
        let req = Request::new("42", op.clone()).with_auth("deadbeef");
        let wire = WireRequest {
            v: PROTOCOL_VERSION,
            id: "42",
            auth: Some("deadbeef"),
            op: &op,
        };
        let req_json = serde_json::to_value(&req).unwrap();
        let wire_json = serde_json::to_value(&wire).unwrap();
        assert_eq!(req_json, wire_json);
        assert!(req_json.get("token").is_none());
        let bare = Request::new("7", op.clone());
        let bare_wire = WireRequest {
            v: PROTOCOL_VERSION,
            id: "7",
            auth: None,
            op: &op,
        };
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::to_value(&bare_wire).unwrap()
        );
        assert!(serde_json::to_value(&bare).unwrap().get("token").is_none());
        assert!(serde_json::to_value(&bare).unwrap().get("auth").is_none());
    }

    #[test]
    fn fmt_u64_matches_display() {
        let mut buf = [0u8; 20];
        assert_eq!(fmt_u64(0, &mut buf), "0");
        assert_eq!(fmt_u64(1, &mut buf), "1");
        assert_eq!(fmt_u64(10, &mut buf), "10");
        assert_eq!(fmt_u64(u64::MAX, &mut buf), u64::MAX.to_string());
    }

    #[test]
    fn rpc_pipeline_empty_is_ok() {
        let mut client = AgentClient::default_bind();
        assert!(client.rpc_pipeline(&[]).unwrap().is_empty());
    }
}
