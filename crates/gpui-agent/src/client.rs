use std::io::{BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::ndjson::{read_limited_line_into, write_json_line};
use crate::protocol::{AssertSpec, DeliveryMode, Op, Request, Response};
use crate::server::{MAX_LINE_BYTES, default_addr};

/// Blocking NDJSON client used by the CLI, MCP shim, recipes, and tests.
///
/// After the first successful connect, subsequent [`rpc`](Self::rpc) calls
/// reuse the same TCP session (one process, one connection, many ops).
/// Use [`rpc_once`](Self::rpc_once) to force the old per-op reconnect path
/// (benchmarks / comparison).
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
}

impl LiveSession {
    fn open(addr: SocketAddr, timeout: Duration) -> Result<Self, String> {
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
        Ok(Self {
            writer,
            reader: BufReader::new(stream),
            encode_buf: Vec::with_capacity(256),
            line_buf: Vec::with_capacity(256),
        })
    }

    fn exchange(&mut self, req: &Request) -> Result<Response, String> {
        write_json_line(&mut self.writer, &mut self.encode_buf, req)
            .map_err(|err| err.to_string())?;
        self.writer.flush().map_err(|err| err.to_string())?;
        if !read_limited_line_into(&mut self.reader, &mut self.line_buf, MAX_LINE_BYTES)
            .map_err(|err| err.to_string())?
        {
            return Err("connection closed".into());
        }
        serde_json::from_slice(&self.line_buf).map_err(|err| format!("bad response: {err}"))
    }
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

    /// True when a live TCP session is being reused.
    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// Drop the live session (next [`rpc`](Self::rpc) reconnects).
    pub fn close_session(&mut self) {
        self.session = None;
    }

    pub fn rpc(&mut self, op: Op) -> Result<Response, String> {
        let req = self.build_request(op);
        self.roundtrip_retry(&req)
    }

    /// One request on a fresh connection, then drop it.
    ///
    /// This is the pre-experiment CLI shape (one TCP handshake per op) and
    /// exists so benches can compare it with session reuse.
    pub fn rpc_once(&mut self, op: Op) -> Result<Response, String> {
        self.session = None;
        let req = self.build_request(op);
        let resp = self.roundtrip_retry(&req)?;
        self.session = None;
        Ok(resp)
    }

    fn build_request(&mut self, op: Op) -> Request {
        let id = {
            let id = self.next_id;
            self.next_id += 1;
            id.to_string()
        };
        let mut req = Request::new(id, op);
        if let Some(token) = &self.token {
            req.token = Some(token.clone());
        }
        req
    }

    fn roundtrip_retry(&mut self, req: &Request) -> Result<Response, String> {
        let mut last_err = String::new();
        let deadline = std::time::Instant::now() + self.timeout;
        while std::time::Instant::now() < deadline {
            match self.roundtrip(req) {
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

    fn roundtrip(&mut self, req: &Request) -> Result<Response, String> {
        if self.session.is_none() {
            self.session = Some(LiveSession::open(self.addr, self.timeout)?);
        }
        match self.session.as_mut().expect("session").exchange(req) {
            Ok(resp) => Ok(resp),
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
}
