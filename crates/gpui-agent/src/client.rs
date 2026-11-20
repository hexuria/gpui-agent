use std::io::{BufReader, ErrorKind, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::hmac_auth::{hmac_hex, parse_challenge_line};
use crate::mailbox::MAX_MAILBOX_DEPTH;
use crate::ndjson::{read_limited_line_into, write_json_line};
use crate::protocol::{
    AssertSpec, DeliveryMode, Op, PROTOCOL_VERSION, Response, ScreenshotMode, SnapshotFormat,
};
use crate::server::{CLIENT_SESSION_IDLE_RECONNECT_AFTER, MAX_LINE_BYTES, default_addr};

const FIRST_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(400);

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
    features: Option<Vec<String>>,
    as_written: bool,
}

struct LiveSession {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    encode_buf: Vec<u8>,
    line_buf: Vec<u8>,
    auth: Option<String>,
    last_used: Instant,
}

impl LiveSession {
    fn open_until(
        addr: SocketAddr,
        deadline: Instant,
        token: Option<&str>,
    ) -> Result<Self, String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let connect_timeout = remaining.min(Duration::from_millis(400));
        if connect_timeout.is_zero() {
            return Err(format!("{TCP_CONNECT}connection deadline elapsed"));
        }
        let stream = TcpStream::connect_timeout(&addr, connect_timeout)
            .map_err(|err| format!("{TCP_CONNECT}{err}"))?;
        let _ = stream.set_nodelay(true);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("connection deadline elapsed".into());
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|err| err.to_string())?;
        stream
            .set_write_timeout(Some(remaining))
            .map_err(|err| err.to_string())?;
        let writer = stream.try_clone().map_err(|err| err.to_string())?;
        let mut session = Self {
            writer,
            reader: BufReader::new(stream),
            encode_buf: Vec::with_capacity(4096),
            line_buf: Vec::with_capacity(4096),
            auth: None,
            last_used: Instant::now(),
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
        session.last_used = Instant::now();
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

    fn peer_closed(&self) -> std::io::Result<bool> {
        let stream = self.reader.get_ref();
        stream.set_nonblocking(true)?;
        let mut byte = [0u8; 1];
        let peek = stream.peek(&mut byte);
        stream.set_nonblocking(false)?;
        match peek {
            Ok(0) => Ok(true),
            Ok(_) => Ok(false),
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn exchange_wire(&mut self, id: &str, op: &Op) -> Result<Response, WireError> {
        let req = WireRequest {
            v: PROTOCOL_VERSION,
            id,
            auth: self.auth.as_deref(),
            op,
        };
        write_json_line(&mut self.writer, &mut self.encode_buf, &req)
            .map_err(|err| WireError::Retryable(err.to_string()))?;
        self.writer
            .flush()
            .map_err(|err| WireError::Retryable(err.to_string()))?;
        match read_limited_line_into(&mut self.reader, &mut self.line_buf, MAX_LINE_BYTES) {
            Ok(true) => {}
            Ok(false) => return Err(WireError::Fatal("connection closed".into())),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(WireError::NoReply(err.to_string()));
            }
            Err(err) => return Err(WireError::Fatal(err.to_string())),
        }
        parse_response_line(&self.line_buf).map_err(WireError::Fatal)
    }

    fn exchange_wire_with_timeouts(
        &mut self,
        id: &str,
        op: &Op,
        write_deadline: Instant,
        reply_timeout: Duration,
    ) -> Result<Response, WireError> {
        let write_timeout = write_deadline.saturating_duration_since(Instant::now());
        if write_timeout.is_zero() {
            return Err(WireError::Retryable(
                "connection deadline elapsed before request".into(),
            ));
        }
        self.writer
            .set_write_timeout(Some(write_timeout))
            .map_err(|err| WireError::Retryable(err.to_string()))?;
        self.reader
            .get_ref()
            .set_read_timeout(Some(reply_timeout))
            .map_err(|err| WireError::Retryable(err.to_string()))?;
        self.exchange_wire(id, op)
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
        write_deadline: Instant,
        reply_timeout: Duration,
    ) -> Result<Vec<Response>, WireError> {
        self.reader
            .get_ref()
            .set_read_timeout(Some(reply_timeout))
            .map_err(|err| WireError::Retryable(err.to_string()))?;
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
            let write_timeout = write_deadline.saturating_duration_since(Instant::now());
            if write_timeout.is_zero() {
                return Err(if wrote {
                    WireError::Fatal("connection deadline elapsed while writing pipeline".into())
                } else {
                    WireError::Retryable("connection deadline elapsed before request".into())
                });
            }
            if let Err(err) = self.writer.set_write_timeout(Some(write_timeout)) {
                return Err(if wrote {
                    WireError::Fatal(err.to_string())
                } else {
                    WireError::Retryable(err.to_string())
                });
            }
            if let Err(err) = write_json_line(&mut self.writer, &mut self.encode_buf, &req) {
                return Err(if wrote {
                    WireError::Fatal(err.to_string())
                } else {
                    WireError::Retryable(err.to_string())
                });
            }
            wrote = true;
        }
        self.writer
            .flush()
            .map_err(|err| WireError::Fatal(err.to_string()))?;
        let mut out = Vec::with_capacity(ops.len());
        for _ in ops {
            match read_limited_line_into(&mut self.reader, &mut self.line_buf, MAX_LINE_BYTES) {
                Ok(true) => {}
                Ok(false) => return Err(pipeline_eof(&out)),
                Err(err) => return Err(WireError::Fatal(err.to_string())),
            }
            out.push(parse_response_line(&self.line_buf).map_err(WireError::Fatal)?);
        }
        Ok(out)
    }
}

/// Connect/write failures before any request line is sent may retry.
/// Once a line is on the wire, ops may already have run — do not replay.
enum WireError {
    Retryable(String),
    Fatal(String),
    NoReply(String),
}

fn pipeline_eof(partial: &[Response]) -> WireError {
    if let Some(resp) = partial.iter().find(|resp| !resp.ok) {
        WireError::Fatal(
            resp.error
                .clone()
                .unwrap_or_else(|| "connection closed".into()),
        )
    } else {
        WireError::Fatal("connection closed".into())
    }
}

fn sleep_before_retry(deadline: std::time::Instant) {
    std::thread::sleep(
        deadline
            .saturating_duration_since(std::time::Instant::now())
            .min(Duration::from_millis(50)),
    );
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

/// Error prefix when the host took the request and sent no reply in time.
pub const NO_REPLY: &str = "no reply";

/// The message for an exchange that never produced a response.
///
/// A read that ran out of time (the OS spells it differently on each
/// platform) means the host is there and busy, which calls for a longer
/// timeout, not for checking the address.
fn no_answer(addr: SocketAddr, timeout: Duration, last_err: &str) -> String {
    if read_timed_out(last_err) {
        format!(
            "{NO_REPLY} from {addr} within {ms} ms: the host is up but did not answer \
             (busy or animating); raise the timeout",
            ms = timeout.as_millis()
        )
    } else {
        let cause = last_err.strip_prefix(TCP_CONNECT).unwrap_or(last_err);
        format!("connect {addr} failed: {cause}")
    }
}

/// Marks an error from opening the TCP connection, where running out of
/// time means the host was never reached.
const TCP_CONNECT: &str = "tcp connect: ";

/// `true` for an error of reading or writing on an open connection that ran
/// out of time.
fn read_timed_out(err: &str) -> bool {
    !err.starts_with(TCP_CONNECT)
        && [
            "timed out",
            "os error 11)",
            "os error 35)",
            "os error 10060)",
        ]
        .iter()
        .any(|mark| err.contains(mark))
}

impl AgentClient {
    pub fn connect(addr: SocketAddr) -> Self {
        Self {
            addr,
            token: None,
            timeout: Duration::from_secs(8),
            next_id: 1,
            session: None,
            features: None,
            as_written: false,
        }
    }

    pub fn default_bind() -> Self {
        Self::connect(default_addr())
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Send capability-gated ops as written, without reading
    /// `hello.features` first: no local refusal and no `invoke` spelling.
    /// What comes back is the host's own answer, which is what a
    /// conformance test of the host needs to see.
    pub fn as_written(mut self) -> Self {
        self.as_written = true;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
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
            Op::WaitUntil { timeout_ms, .. } => self
                .timeout
                .max(Duration::from_millis(timeout_ms.saturating_add(2_000))),
            Op::Wait {
                timeout_ms: Some(ms),
            } => self
                .timeout
                .max(Duration::from_millis(ms.saturating_add(2_000))),
            _ => self.timeout,
        }
    }

    /// True when a live TCP session is being reused.
    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// Drop the live session (next [`rpc`](Self::rpc) reconnects).
    pub fn close_session(&mut self) {
        self.drop_session();
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
        let timeout = ops
            .iter()
            .map(|op| self.timeout_for(op))
            .max()
            .unwrap_or(self.timeout);
        self.pipeline_retry(ops, timeout)
    }

    /// One request on a fresh connection, then drop it.
    ///
    /// This is the historical CLI shape (one TCP handshake per op) and
    /// exists so benches can compare it with session reuse.
    pub fn rpc_once(&mut self, op: Op) -> Result<Response, String> {
        self.drop_session();
        let resp = self.roundtrip_retry(&op)?;
        self.drop_session();
        Ok(resp)
    }

    fn roundtrip_retry(&mut self, op: &Op) -> Result<Response, String> {
        self.drop_idle_session();
        let timeout = self.timeout_for(op);
        let connect_deadline = Instant::now() + self.timeout;
        self.roundtrip_retry_until(op, connect_deadline, timeout, None)
    }

    fn roundtrip_retry_until(
        &mut self,
        op: &Op,
        connect_deadline: Instant,
        timeout: Duration,
        shared_deadline: Option<Instant>,
    ) -> Result<Response, String> {
        let id_num = self.next_id;
        self.next_id += 1;
        let mut id_buf = [0u8; 20];
        let id = fmt_u64(id_num, &mut id_buf);
        let mut last_err = String::new();
        let mut first_attempt = true;
        loop {
            let now = Instant::now();
            if connect_deadline <= now && (!first_attempt || shared_deadline.is_some()) {
                break;
            }
            let attempt_deadline = if first_attempt && shared_deadline.is_none() {
                connect_deadline.max(now + FIRST_ATTEMPT_TIMEOUT)
            } else {
                connect_deadline
            };
            let attempt_timeout = if first_attempt && shared_deadline.is_none() {
                timeout.max(FIRST_ATTEMPT_TIMEOUT)
            } else {
                timeout
            };
            let result = match shared_deadline {
                Some(deadline) => self.roundtrip_wire(id, op, deadline),
                None => self.roundtrip_wire_with_reply_timeout(
                    id,
                    op,
                    attempt_deadline,
                    attempt_timeout,
                ),
            };
            first_attempt = false;
            match result {
                Ok(resp) => return Ok(resp),
                Err(WireError::Retryable(err)) => {
                    last_err = err;
                    self.drop_session();
                    sleep_before_retry(connect_deadline);
                }
                Err(WireError::Fatal(err)) => return Err(err),
                Err(WireError::NoReply(err)) => {
                    return Err(no_answer(self.addr, timeout, &err));
                }
            }
        }
        let retry_timeout = if shared_deadline.is_some() {
            timeout
        } else {
            self.timeout
        };
        Err(no_answer(self.addr, retry_timeout, &last_err))
    }

    fn roundtrip_gated_retry(&mut self, op: &Op) -> Result<Response, String> {
        let timeout = self.timeout_for(op);
        let deadline = std::time::Instant::now() + timeout;
        let mut last_err = String::new();
        while std::time::Instant::now() < deadline {
            self.drop_idle_session();
            let peer_closed = self
                .session
                .as_ref()
                .is_some_and(|session| !matches!(session.peer_closed(), Ok(false)));
            if peer_closed {
                self.drop_session();
            }
            if self.session.is_none() {
                self.features = None;
            }
            let request = self.negotiated(op, deadline)?.unwrap_or_else(|| op.clone());
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(no_answer(self.addr, timeout, "operation timed out"));
            }
            let id_num = self.next_id;
            self.next_id += 1;
            let mut id_buf = [0u8; 20];
            let id = fmt_u64(id_num, &mut id_buf);
            match self.roundtrip_wire(id, &request, deadline) {
                Ok(response) => return Ok(response),
                Err(WireError::Retryable(err)) => {
                    last_err = err;
                    self.drop_session();
                    sleep_before_retry(deadline);
                }
                Err(WireError::Fatal(err)) => return Err(err),
                Err(WireError::NoReply(err)) => {
                    return Err(no_answer(self.addr, remaining, &err));
                }
            }
        }
        Err(no_answer(self.addr, timeout, &last_err))
    }

    fn roundtrip_wire(
        &mut self,
        id: &str,
        op: &Op,
        deadline: Instant,
    ) -> Result<Response, WireError> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err(WireError::NoReply("operation deadline elapsed".into()));
        }
        if self.session.is_none() {
            self.features = None;
            match LiveSession::open_until(self.addr, deadline, self.token.as_deref()) {
                Ok(session) => self.session = Some(session),
                Err(error) if Instant::now() >= deadline => {
                    return Err(WireError::NoReply(error));
                }
                Err(error) => return Err(WireError::Retryable(error)),
            }
        } else {
            let peer_closed = self.session.as_ref().expect("session").peer_closed();
            match peer_closed {
                Ok(false) => {}
                Ok(true) => {
                    self.drop_session();
                    return Err(WireError::Retryable(
                        "connection closed before request".into(),
                    ));
                }
                Err(error) => {
                    self.drop_session();
                    return Err(WireError::Retryable(error.to_string()));
                }
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.drop_session();
            return Err(WireError::NoReply("operation deadline elapsed".into()));
        }
        self.session
            .as_mut()
            .expect("session")
            .set_io_timeout(remaining)
            .map_err(WireError::Retryable)?;
        let result = self
            .session
            .as_mut()
            .expect("session")
            .exchange_wire(id, op);
        match result {
            Ok(resp) => {
                self.session.as_mut().expect("session").last_used = Instant::now();
                Ok(resp)
            }
            Err(err) => {
                self.drop_session();
                Err(err)
            }
        }
    }

    fn roundtrip_wire_with_reply_timeout(
        &mut self,
        id: &str,
        op: &Op,
        connect_deadline: Instant,
        reply_timeout: Duration,
    ) -> Result<Response, WireError> {
        if connect_deadline <= Instant::now() {
            return Err(WireError::Retryable(
                "connection deadline elapsed before request".into(),
            ));
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| !matches!(session.peer_closed(), Ok(false)))
        {
            self.drop_session();
        }
        if self.session.is_none() {
            self.features = None;
            match LiveSession::open_until(self.addr, connect_deadline, self.token.as_deref()) {
                Ok(session) => self.session = Some(session),
                Err(error) => return Err(WireError::Retryable(error)),
            }
        }
        let result = self
            .session
            .as_mut()
            .expect("session")
            .exchange_wire_with_timeouts(id, op, connect_deadline, reply_timeout);
        match result {
            Ok(resp) => {
                self.session.as_mut().expect("session").last_used = Instant::now();
                Ok(resp)
            }
            Err(err) => {
                self.drop_session();
                Err(err)
            }
        }
    }

    fn pipeline_retry(
        &mut self,
        ops: &[&Op],
        reply_timeout: Duration,
    ) -> Result<Vec<Response>, String> {
        self.drop_idle_session();
        let start_id = self.next_id;
        self.next_id += ops.len() as u64;
        let mut last_err = String::new();
        let connect_deadline = Instant::now() + self.timeout;
        while Instant::now() < connect_deadline {
            match self.pipeline_once_until(start_id, ops, connect_deadline, reply_timeout) {
                Ok(resps) => return Ok(resps),
                Err(WireError::Retryable(err)) => {
                    last_err = err;
                    self.drop_session();
                    sleep_before_retry(connect_deadline);
                }
                Err(WireError::Fatal(err)) => {
                    self.drop_session();
                    return Err(if read_timed_out(&err) {
                        no_answer(self.addr, reply_timeout, &err)
                    } else {
                        err
                    });
                }
                Err(WireError::NoReply(err)) => {
                    self.drop_session();
                    return Err(no_answer(self.addr, reply_timeout, &err));
                }
            }
        }
        Err(no_answer(self.addr, self.timeout, &last_err))
    }

    fn pipeline_once_until(
        &mut self,
        start_id: u64,
        ops: &[&Op],
        connect_deadline: Instant,
        reply_timeout: Duration,
    ) -> Result<Vec<Response>, WireError> {
        if self.session.is_none() {
            self.features = None;
            match LiveSession::open_until(self.addr, connect_deadline, self.token.as_deref()) {
                Ok(session) => self.session = Some(session),
                Err(err) => return Err(WireError::Retryable(err)),
            }
        } else {
            let peer_closed = self.session.as_ref().expect("session").peer_closed();
            match peer_closed {
                Ok(false) => {}
                Ok(true) => {
                    self.drop_session();
                    return Err(WireError::Retryable(
                        "connection closed before request".into(),
                    ));
                }
                Err(error) => {
                    self.drop_session();
                    return Err(WireError::Retryable(error.to_string()));
                }
            }
        }
        let result = self.session.as_mut().expect("session").exchange_pipeline(
            start_id,
            ops,
            connect_deadline,
            reply_timeout,
        );
        match result {
            Ok(resps) => {
                self.session.as_mut().expect("session").last_used = Instant::now();
                Ok(resps)
            }
            Err(err) => {
                self.drop_session();
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
        self.expect_ok(Op::snapshot())
    }

    /// Parameterized snapshot: `since` asks for a diff against a held
    /// revision, `format` selects json/compact (M3). `None`/`None` is
    /// the v1 byte-identical op.
    pub fn snapshot_with(
        &mut self,
        since: Option<u64>,
        format: Option<SnapshotFormat>,
    ) -> Result<Response, String> {
        self.expect_ok(Op::Snapshot {
            since,
            format,
            fingerprints: None,
        })
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
            inline: false,
        })
    }

    /// Capture the app surface and return the PNG bytes over the wire,
    /// for hosts whose files this machine cannot read (a device, a
    /// browser). Needs the `screenshot_inline` feature.
    pub fn screenshot_bytes(
        &mut self,
        mode: ScreenshotMode,
        target: Option<String>,
        max_height_px: Option<u32>,
    ) -> Result<Vec<u8>, String> {
        let resp = self.gated(Op::Screenshot {
            path: None,
            mode,
            target,
            max_height_px,
            inline: true,
        })?;
        let blob = resp
            .result
            .as_ref()
            .and_then(|result| result.get("blob"))
            .ok_or_else(|| crate::protocol::unsupported_op("screenshot_inline"))?;
        let name = blob
            .get("blob")
            .and_then(serde_json::Value::as_str)
            .ok_or("screenshot blob has no name")?
            .to_string();
        let chunks = blob
            .get("chunks")
            .and_then(serde_json::Value::as_u64)
            .ok_or("screenshot blob has no chunk count")? as u32;
        let mut bytes = Vec::new();
        for index in 0..chunks {
            let chunk = self.expect_ok(Op::Blob {
                blob: name.clone(),
                index,
            })?;
            bytes.extend(crate::blob::decode_chunk(
                chunk.result.as_ref().ok_or("blob chunk has no result")?,
            )?);
        }
        Ok(bytes)
    }

    /// A new connection may reach a different build of the host, so what
    /// the last one advertised is forgotten with it.
    fn drop_session(&mut self) {
        self.session = None;
        self.features = None;
    }

    fn drop_idle_session(&mut self) {
        if self.session.as_ref().is_some_and(|session| {
            session.last_used.elapsed() >= CLIENT_SESSION_IDLE_RECONNECT_AFTER
        }) {
            self.drop_session();
        }
    }

    /// The host's `hello.features`, asked once per connection.
    pub fn features(&mut self) -> Result<&[String], String> {
        if self.features.is_none() {
            let hello = self
                .expect_ok(Op::Hello)?
                .hello
                .ok_or("hello has no info")?;
            self.features = Some(hello.features);
        }
        Ok(self.features.as_deref().unwrap_or_default())
    }

    /// Send an op that is gated by a `hello.features` entry (`press`,
    /// `scroll`, `system`, …). A host that lists the feature gets the op;
    /// a host built against gpui-agent 0.1 gets the `invoke` spelling it
    /// knew, where one exists; otherwise the call fails with
    /// `unsupported_op` and nothing is sent.
    pub fn gated(&mut self, op: Op) -> Result<Response, String> {
        let response = if op.feature().is_some() {
            self.roundtrip_gated_retry(&op)?
        } else {
            self.rpc_op(&op)?
        };
        if response.ok {
            Ok(response)
        } else {
            Err(response.error.unwrap_or_else(|| "request failed".into()))
        }
    }

    /// [`gated`](Self::gated) for callers that keep the failed response
    /// (the recipe runner's receipts): `ok: false` is returned, not an `Err`.
    pub fn rpc_gated(&mut self, op: &Op) -> Result<Response, String> {
        if op.feature().is_some() {
            self.roundtrip_gated_retry(op)
        } else {
            self.rpc_op(op)
        }
    }

    /// `Some(invoke)` when the host needs the 0.1 spelling, `None` when
    /// `op` goes as written, `Err(unsupported_op)` when it cannot be sent.
    fn negotiated(&mut self, op: &Op, deadline: std::time::Instant) -> Result<Option<Op>, String> {
        let Some(feature) = op.feature() else {
            return Ok(None);
        };
        if self.as_written {
            return Ok(None);
        }
        if self.features.is_none() {
            let hello_op = Op::Hello;
            let hello_timeout = self.timeout_for(&hello_op);
            let hello_started = std::time::Instant::now();
            let hello_budget = hello_timeout.min(deadline.saturating_duration_since(hello_started));
            if hello_budget.is_zero() {
                return Err(no_answer(self.addr, hello_timeout, "operation timed out"));
            }
            let hello_deadline = hello_started + hello_budget;
            let hello = self.roundtrip_retry_until(
                &hello_op,
                hello_deadline,
                hello_budget,
                Some(hello_deadline),
            )?;
            if !hello.ok {
                return Err(hello.error.unwrap_or_else(|| "request failed".into()));
            }
            self.features = Some(hello.hello.ok_or("hello has no info")?.features);
        }
        let features = self.features.as_deref().unwrap_or_default();
        if features.iter().any(|f| f == feature) {
            return Ok(None);
        }
        // A host on this dispatcher (it lists `screenshot_inline`) that
        // omits a feature means it: only a 0.1 host gets the old spelling.
        if features.iter().any(|f| f == "screenshot_inline") {
            return Err(crate::protocol::unsupported_op(feature));
        }
        op.legacy_invoke()
            .map(Some)
            .ok_or_else(|| crate::protocol::unsupported_op(feature))
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
            expect_fingerprint: None,
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
            expect_fingerprint: None,
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
            expect_fingerprint: None,
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
            expect_fingerprint: None,
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

    pub fn wait_until(&mut self, spec: AssertSpec, timeout_ms: u64) -> Result<Response, String> {
        self.expect_ok(Op::WaitUntil { timeout_ms, spec })
    }

    pub fn keybindings(&mut self) -> Result<Response, String> {
        self.expect_ok(Op::Keybindings)
    }

    /// `keybindings` result parsed to a catalog (`null`/absent → empty).
    /// Shared by the policy runner and MCP `act` so neither re-parses
    /// the `{"keybindings":[…]}` envelope itself.
    pub fn keybinding_catalog(&mut self) -> Result<Vec<crate::keybinding::KeybindingInfo>, String> {
        let resp = self.keybindings()?;
        crate::keybinding::KeybindingInfo::catalog_from(resp.result.as_ref())
    }

    /// `events` — poll the host's bounded event stream (M9).
    /// `since` returns events with `seq > N` (`None` → all retained);
    /// the reply's `latest` is the cursor for the next poll. Fails on
    /// hosts without a ring ("events unsupported on this host").
    pub fn events(
        &mut self,
        since: Option<u64>,
        limit: Option<u32>,
    ) -> Result<crate::event::EventPoll, String> {
        let resp = self.expect_ok(Op::Events { since, limit })?;
        serde_json::from_value(resp.result.unwrap_or_default())
            .map_err(|err| format!("bad events response: {err}"))
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
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

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
    fn pipeline_reapplies_the_current_socket_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());

            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            serde_json::to_writer(
                &mut stream,
                &serde_json::json!({
                    "v": PROTOCOL_VERSION,
                    "id": request["id"],
                    "ok": true,
                    "hello": {
                        "protocol": PROTOCOL_VERSION,
                        "app": "fake",
                        "platform": "headless",
                        "ready": true
                    }
                }),
            )
            .unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();

            line.clear();
            reader.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            thread::sleep(Duration::from_millis(500));
            let _ = serde_json::to_writer(
                &mut stream,
                &serde_json::json!({
                    "v": PROTOCOL_VERSION,
                    "id": request["id"],
                    "ok": true
                }),
            );
            let _ = stream.write_all(b"\n");
        });

        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(5));
        client.expect_ok(Op::Hello).unwrap();
        client.set_timeout(Duration::from_millis(100));
        let started = std::time::Instant::now();
        let timeout = client.timeout;
        let response =
            client.pipeline_once_until(2, &[&Op::Hello], Instant::now() + timeout, timeout);
        let elapsed = started.elapsed();
        server.join().unwrap();

        assert!(response.is_err(), "the changed timeout must bound the read");
        assert!(
            elapsed < Duration::from_millis(300),
            "pipeline took {elapsed:?} with a 100 ms timeout"
        );
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

    #[test]
    fn a_read_that_ran_out_of_time_is_not_reported_as_a_failed_connect() {
        let addr: SocketAddr = "127.0.0.1:17421".parse().unwrap();
        let timeout = Duration::from_millis(1500);
        for err in [
            "Resource temporarily unavailable (os error 11)",
            "Resource temporarily unavailable (os error 35)",
            "operation timed out",
        ] {
            let message = no_answer(addr, timeout, err);
            assert!(message.starts_with(NO_REPLY), "{message}");
            assert!(message.contains("1500 ms"), "{message}");
        }
        let unreachable = no_answer(addr, timeout, "tcp connect: connection timed out");
        assert_eq!(
            unreachable,
            "connect 127.0.0.1:17421 failed: connection timed out"
        );
        let refused = no_answer(addr, timeout, "Connection refused (os error 111)");
        assert!(
            refused.starts_with("connect 127.0.0.1:17421 failed"),
            "{refused}"
        );
    }
}
