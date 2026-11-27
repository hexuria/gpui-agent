use std::io::{BufReader, ErrorKind, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::error_code::{self, CallFailure, DeliveryStatus, error_code_of};
use crate::hmac_auth::{hmac_hex, parse_challenge_line};
use crate::mailbox::MAX_MAILBOX_DEPTH;
use crate::ndjson::{read_limited_line_into, write_json_line};
use crate::protocol::{
    AssertSpec, DeliveryMode, Op, PROTOCOL_VERSION, ReadBack, Response, ScreenshotMode,
    SnapshotFormat,
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
    hello_protocol: Option<u32>,
    refresh_features_on_reconnect: bool,
    as_written: bool,
}

impl std::fmt::Debug for AgentClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentClient")
            .field("addr", &self.addr)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("timeout", &self.timeout)
            .field("next_id", &self.next_id)
            .field("session", &self.session.as_ref().map(|_| "<open>"))
            .field("features", &self.features)
            .field("as_written", &self.as_written)
            .finish_non_exhaustive()
    }
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

    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "one flag: mask secret values for hosts predating host-side redaction"
    )]
    fn exchange_wire(
        &mut self,
        id: &str,
        op: &Op,
        legacy_secret_redaction: bool,
    ) -> Result<Response, WireError> {
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
        parse_response_line(&self.line_buf, legacy_secret_redaction).map_err(WireError::Fatal)
    }

    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "one flag: mask secret values for hosts predating host-side redaction"
    )]
    fn exchange_wire_with_timeouts(
        &mut self,
        id: &str,
        op: &Op,
        write_deadline: Instant,
        reply_timeout: Duration,
        legacy_secret_redaction: bool,
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
        self.exchange_wire(id, op, legacy_secret_redaction)
    }

    /// Write every request line, flush once, then read one response each.
    ///
    /// Connect/write failures **before** the first line may be retried.
    /// After any line is on the wire, errors are fatal so mutating ops
    /// (and even extra reads) are not replayed.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "one flag: mask secret values for hosts predating host-side redaction"
    )]
    fn exchange_pipeline(
        &mut self,
        start_id: u64,
        ops: &[&Op],
        write_deadline: Instant,
        reply_timeout: Duration,
        legacy_secret_redaction: bool,
    ) -> Result<Vec<Response>, WireError> {
        self.reader
            .get_ref()
            .set_read_timeout(Some(reply_timeout))
            .map_err(|err| WireError::Retryable(err.to_string()))?;
        let mut id_buf = [0u8; 20];
        let mut wrote = false;
        for (i, op) in ops.iter().enumerate() {
            let id = fmt_u64(start_id + u64::try_from(i).unwrap_or(u64::MAX), &mut id_buf);
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
            out.push(
                parse_response_line(&self.line_buf, legacy_secret_redaction)
                    .map_err(WireError::Fatal)?,
            );
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
    /// Ran out of time before the request line was written: stop, but
    /// nothing was sent.
    Unsent(String),
}

/// A transport failure, and whether the request line may have reached the
/// host. `sent: false` means nothing of the op was written, so it cannot
/// have run.
struct SendError {
    message: String,
    sent: bool,
}

impl SendError {
    fn unsent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            sent: false,
        }
    }

    fn sent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            sent: true,
        }
    }
}

impl From<SendError> for String {
    fn from(err: SendError) -> Self {
        err.message
    }
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
        return std::str::from_utf8(&buf[i - 1..]).expect("ASCII digits");
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + u8::try_from(n % 10).unwrap_or(b'9');
        n /= 10;
    }
    std::str::from_utf8(&buf[i..]).expect("ASCII digits")
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
    /// A client for the host at `addr` (no token, 8s timeout).
    pub fn connect(addr: SocketAddr) -> Self {
        Self {
            addr,
            token: None,
            timeout: Duration::from_secs(8),
            next_id: 1,
            session: None,
            features: None,
            hello_protocol: None,
            refresh_features_on_reconnect: false,
            as_written: false,
        }
    }

    /// A client for [`default_addr`].
    pub fn default_bind() -> Self {
        Self::connect(default_addr())
    }

    /// Answer the host's challenge with `token` (HMAC, never raw on the wire).
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Send capability-gated ops as written, without local refusal or
    /// `invoke` rewriting. After reconnecting, the client still refreshes
    /// `hello.features` before parsing a reply.
    #[must_use]
    pub fn as_written(mut self) -> Self {
        self.as_written = true;
        self
    }

    /// True when capability-gated ops are sent as written, without local
    /// refusal or legacy `invoke` rewriting.
    pub fn is_as_written(&self) -> bool {
        self.as_written
    }

    /// Set the connect/request timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The configured timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Set the connect/request timeout in place.
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

    /// Send `op` and return the reply (`ok: false` included).
    ///
    /// # Errors
    ///
    /// A transport failure (the host's error string is on `resp.error`,
    /// not returned).
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public API since 0.3.0; a reference changes callers"
    )]
    /// Send `op` and return the reply (`ok: false` included).
    ///
    /// # Errors
    ///
    /// A transport failure (the host's error string is on `resp.error`,
    /// not returned).
    pub fn rpc(&mut self, op: Op) -> Result<Response, String> {
        self.rpc_op(&op)
    }

    /// Like [`rpc`](Self::rpc) but does not take ownership of `op`.
    /// Like [`rpc`](Self::rpc) but does not take ownership of `op`.
    ///
    /// # Errors
    ///
    /// A transport failure (the host's error string is on `resp.error`).
    pub fn rpc_op(&mut self, op: &Op) -> Result<Response, String> {
        Ok(self.roundtrip_retry(op)?)
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
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
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
        let mut force_full = Vec::with_capacity(ops.len());
        let mut prepared = Vec::with_capacity(ops.len());
        for op in ops {
            let (op, full) = self.prepare_snapshot_op(op)?;
            prepared.push(op);
            force_full.push(full);
        }
        let prepared: Vec<&Op> = prepared.iter().collect();
        let timeout = prepared
            .iter()
            .map(|op| self.timeout_for(op))
            .max()
            .unwrap_or(self.timeout);
        let mut responses = self.pipeline_retry(&prepared, timeout)?;
        for (response, full) in responses.iter_mut().zip(force_full) {
            if full && response.ok {
                response.result = Some(serde_json::json!({ "full": true }));
            }
        }
        Ok(responses)
    }

    /// One request on a fresh connection, then drop it.
    ///
    /// This is the historical CLI shape (one TCP handshake per op) and
    /// exists so benches can compare it with session reuse.
    ///
    /// # Errors
    ///
    /// A transport failure (the host's error string is on `resp.error`,
    /// not returned).
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public API since 0.3.0; a reference changes callers"
    )]
    pub fn rpc_once(&mut self, op: Op) -> Result<Response, String> {
        self.drop_session();
        let resp = self.roundtrip_retry(&op)?;
        self.drop_session();
        Ok(resp)
    }

    fn roundtrip_retry(&mut self, op: &Op) -> Result<Response, SendError> {
        self.drop_idle_session();
        let (op, force_full) = self.prepare_snapshot_op(op).map_err(SendError::unsent)?;
        let timeout = self.timeout_for(&op);
        let connect_deadline = Instant::now() + self.timeout;
        let mut response = self.roundtrip_retry_until(&op, connect_deadline, timeout, None)?;
        if force_full && response.ok {
            response.result = Some(serde_json::json!({ "full": true }));
        }
        Ok(response)
    }

    fn prepare_snapshot_op(&mut self, op: &Op) -> Result<(Op, bool), String> {
        let Op::Snapshot {
            since,
            format,
            fingerprints,
        } = op
        else {
            return Ok((op.clone(), false));
        };
        if since.is_none() && *fingerprints != Some(true) {
            return Ok((op.clone(), false));
        }
        let secret_redaction = self
            .features()?
            .iter()
            .any(|feature| feature == "secret_redaction");
        if secret_redaction || since.is_none() {
            return Ok((op.clone(), false));
        }
        Ok((
            Op::Snapshot {
                since: None,
                format: if format == &Some(SnapshotFormat::Compact) {
                    None
                } else {
                    *format
                },
                fingerprints: *fingerprints,
            },
            true,
        ))
    }

    fn legacy_secret_redaction(&self) -> bool {
        !self
            .features
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "secret_redaction")
    }

    fn roundtrip_retry_until(
        &mut self,
        op: &Op,
        connect_deadline: Instant,
        timeout: Duration,
        shared_deadline: Option<Instant>,
    ) -> Result<Response, SendError> {
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
                Err(WireError::Fatal(err)) => return Err(SendError::sent(err)),
                Err(WireError::NoReply(err)) => {
                    return Err(SendError::sent(no_answer(self.addr, timeout, &err)));
                }
                Err(WireError::Unsent(err)) => {
                    return Err(SendError::unsent(no_answer(self.addr, timeout, &err)));
                }
            }
        }
        let retry_timeout = if shared_deadline.is_some() {
            timeout
        } else {
            self.timeout
        };
        Err(SendError::unsent(no_answer(
            self.addr,
            retry_timeout,
            &last_err,
        )))
    }

    fn roundtrip_gated_retry(&mut self, op: &Op) -> Result<Response, SendError> {
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
            let request = self
                .negotiated(op, deadline)
                .map_err(SendError::unsent)?
                .unwrap_or_else(|| op.clone());
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(SendError::unsent(no_answer(
                    self.addr,
                    timeout,
                    "operation timed out",
                )));
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
                Err(WireError::Fatal(err)) => return Err(SendError::sent(err)),
                Err(WireError::NoReply(err)) => {
                    return Err(SendError::sent(no_answer(self.addr, remaining, &err)));
                }
                Err(WireError::Unsent(err)) => {
                    return Err(SendError::unsent(no_answer(self.addr, remaining, &err)));
                }
            }
        }
        Err(SendError::unsent(no_answer(self.addr, timeout, &last_err)))
    }

    fn roundtrip_wire(
        &mut self,
        id: &str,
        op: &Op,
        deadline: Instant,
    ) -> Result<Response, WireError> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err(WireError::Unsent("operation deadline elapsed".into()));
        }
        if self.session.is_none() {
            match LiveSession::open_until(self.addr, deadline, self.token.as_deref()) {
                Ok(session) => self.session = Some(session),
                Err(error) if Instant::now() >= deadline => {
                    return Err(WireError::Unsent(error));
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
        if self.refresh_features_on_reconnect && !matches!(op, Op::Hello) {
            self.features().map_err(WireError::Unsent)?;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.drop_session();
            return Err(WireError::Unsent("operation deadline elapsed".into()));
        }
        self.session
            .as_mut()
            .expect("session")
            .set_io_timeout(remaining)
            .map_err(WireError::Retryable)?;
        let legacy_secret_redaction = self.legacy_secret_redaction();
        let result =
            self.session
                .as_mut()
                .expect("session")
                .exchange_wire(id, op, legacy_secret_redaction);
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
            match LiveSession::open_until(self.addr, connect_deadline, self.token.as_deref()) {
                Ok(session) => self.session = Some(session),
                Err(error) => return Err(WireError::Retryable(error)),
            }
        }
        if self.refresh_features_on_reconnect && !matches!(op, Op::Hello) {
            self.features().map_err(WireError::Unsent)?;
        }
        let legacy_secret_redaction = self.legacy_secret_redaction();
        let result = self
            .session
            .as_mut()
            .expect("session")
            .exchange_wire_with_timeouts(
                id,
                op,
                connect_deadline,
                reply_timeout,
                legacy_secret_redaction,
            );
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
        self.next_id += u64::try_from(ops.len()).unwrap_or(u64::MAX);
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
                Err(WireError::NoReply(err) | WireError::Unsent(err)) => {
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
        if self.refresh_features_on_reconnect {
            self.features().map_err(WireError::Unsent)?;
        }
        let legacy_secret_redaction = self.legacy_secret_redaction();
        let result = self.session.as_mut().expect("session").exchange_pipeline(
            start_id,
            ops,
            connect_deadline,
            reply_timeout,
            legacy_secret_redaction,
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

    /// [`rpc`](Self::rpc) that turns `ok: false` into an `Err` of the host's
    /// error string.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn expect_ok(&mut self, op: Op) -> Result<Response, String> {
        let resp = self.rpc(op)?;
        if resp.ok {
            Ok(resp)
        } else {
            Err(resp.error.unwrap_or_else(|| "request failed".into()))
        }
    }

    /// A full `snapshot` (the v2 JSON tree).
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn snapshot(&mut self) -> Result<Response, String> {
        self.expect_ok(Op::snapshot())
    }

    /// Parameterized snapshot: `since` asks for a diff against a held
    /// revision, `format` selects json/compact (M3). `None`/`None` is
    /// the v1 byte-identical op.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
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
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn screenshot(&mut self, path: impl Into<String>) -> Result<Response, String> {
        self.expect_ok(Op::screenshot(path))
    }

    /// Opt-in tall capture of a named scroller (`mode=scrolled`).
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn screenshot_scrolled(
        &mut self,
        path: impl Into<String>,
        target: impl Into<String>,
        max_height_px: Option<u32>,
    ) -> Result<Response, String> {
        self.expect_ok(Op::screenshot_scrolled(path, target, max_height_px))
    }

    /// Fully parameterized `screenshot` (`mode`, `target`, `max_height_px`).
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
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
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn screenshot_bytes(
        &mut self,
        mode: ScreenshotMode,
        target: Option<String>,
        max_height_px: Option<u32>,
    ) -> Result<Vec<u8>, String> {
        self.screenshot_bytes_call(mode, target, max_height_px)
            .map_err(|failure| failure.error)
    }

    /// Typed [`screenshot_bytes`](Self::screenshot_bytes): host failures
    /// retain their stable code, hint, and any action delivery status.
    ///
    /// # Errors
    ///
    /// A [`CallFailure`] on `ok: false` or transport failure.
    pub fn screenshot_bytes_call(
        &mut self,
        mode: ScreenshotMode,
        target: Option<String>,
        max_height_px: Option<u32>,
    ) -> Result<Vec<u8>, CallFailure> {
        let resp = self.call(&Op::Screenshot {
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
            .ok_or_else(|| {
                CallFailure::from_message(crate::protocol::unsupported_op("screenshot_inline"))
            })?;
        let name = blob
            .get("blob")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| CallFailure::from_message("screenshot blob has no name"))?
            .to_string();
        let chunks = blob
            .get("chunks")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| CallFailure::from_message("screenshot blob has no chunk count"))
            .and_then(|count| {
                u32::try_from(count).map_err(|_| {
                    CallFailure::from_message("screenshot blob chunk count exceeds u32")
                })
            })?;
        let mut bytes = Vec::new();
        for index in 0..chunks {
            let chunk = self.call(&Op::Blob {
                blob: name.clone(),
                index,
            })?;
            bytes.extend(
                crate::blob::decode_chunk(
                    chunk
                        .result
                        .as_ref()
                        .ok_or_else(|| CallFailure::from_message("blob chunk has no result"))?,
                )
                .map_err(CallFailure::from_message)?,
            );
        }
        Ok(bytes)
    }

    /// Capabilities are re-read before parsing a response on a new connection.
    fn drop_session(&mut self) {
        if self.session.is_some() {
            self.refresh_features_on_reconnect = true;
        }
        self.session = None;
        self.features = None;
        self.hello_protocol = None;
    }

    fn drop_idle_session(&mut self) {
        if self.session.as_ref().is_some_and(|session| {
            session.last_used.elapsed() >= CLIENT_SESSION_IDLE_RECONNECT_AFTER
        }) {
            self.drop_session();
        }
    }

    /// The host's `hello.features`, asked once per connection.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn features(&mut self) -> Result<&[String], String> {
        if self.features.is_none() {
            let hello = self
                .expect_ok(Op::Hello)?
                .hello
                .ok_or("hello has no info")?;
            self.hello_protocol = Some(hello.protocol);
            self.features = Some(hello.features);
            self.refresh_features_on_reconnect = false;
        }
        Ok(self.features.as_deref().unwrap_or_default())
    }

    /// Send an op that is gated by a `hello.features` entry (`press`,
    /// `scroll`, `system`, …). A host that lists the feature gets the op;
    /// a host built against gpui-agent 0.1 gets the `invoke` spelling it
    /// knew, where one exists; otherwise the call fails with
    /// `unsupported_op` and nothing is sent.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public API since 0.3.0; a reference changes callers"
    )]
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
    ///
    /// # Errors
    ///
    /// A transport failure (the host's error string is on `resp.error`).
    pub fn rpc_gated(&mut self, op: &Op) -> Result<Response, String> {
        if op.feature().is_some() {
            Ok(self.roundtrip_gated_retry(op)?)
        } else {
            self.rpc_op(op)
        }
    }

    /// Typed [`rpc_gated`](Self::rpc_gated): a host reply, `ok: false`
    /// included, comes back as `Ok` with `error_code`/`hint` set and, for an
    /// op that [acts](Op::acts), a `delivery_status`. A transport failure
    /// is a [`CallFailure`] that says whether the request may have run.
    ///
    /// # Errors
    ///
    /// A [`CallFailure`] on `ok: false` or transport failure.
    pub fn call_response(&mut self, op: &Op) -> Result<Response, CallFailure> {
        let sent = if op.feature().is_some() {
            self.roundtrip_gated_retry(op)
        } else {
            self.roundtrip_retry(op)
        };
        let acts = op.acts();
        match sent {
            Ok(mut resp) => {
                resp.fill_error_code();
                if acts && resp.delivery_status.is_none() {
                    resp.delivery_status = Some(match resp.code() {
                        None => DeliveryStatus::DeliveredUnverified,
                        Some(code) => DeliveryStatus::of_failure(code),
                    });
                }
                Ok(resp)
            }
            Err(err) => Err(transport_failure(err, acts)),
        }
    }

    /// [`call_response`](Self::call_response) that turns `ok: false` into
    /// a [`CallFailure`] too.
    ///
    /// # Errors
    ///
    /// A [`CallFailure`] on `ok: false` or transport failure.
    pub fn call(&mut self, op: &Op) -> Result<Response, CallFailure> {
        let resp = self.call_response(op)?;
        if resp.ok {
            Ok(resp)
        } else {
            Err(CallFailure::from_response(&resp, op.acts()))
        }
    }

    /// [`call`](Self::call) for an action, with a read-back when
    /// `read_back` is set and the op has a [target](Op::read_back_target):
    /// the target is read before and after, the reply gets `after`, and
    /// `delivery_status` becomes `delivered_verified` when the target
    /// changed. On hosts that track revisions an unchanged `rev` skips the
    /// second snapshot. A read-back that cannot be taken leaves the action's
    /// own result as it was (`delivered_unverified`). A target absent from
    /// the first read fails with `not_found` and `not_delivered`, and the
    /// action is not sent. On a [secret](crate::tree::UiNode::is_secret)
    /// target `after` says whether the value changed, never what it is.
    ///
    /// # Errors
    ///
    /// A [`CallFailure`] on `ok: false` or transport failure.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "one flag: whether to verify the action with a read-back"
    )]
    pub fn act(&mut self, op: &Op, read_back: bool) -> Result<Response, CallFailure> {
        let Some(target) = op.read_back_target().filter(|_| read_back) else {
            return self.call(op);
        };
        let target = target.to_string();
        let before = match self.call_response(&Op::snapshot()) {
            Ok(resp) => read_target(&resp, &target),
            // Nothing of the action was written yet: report that instead
            // of waiting out a second connect.
            Err(failure) => {
                return Err(CallFailure {
                    delivery_status: op.acts().then_some(DeliveryStatus::NotDelivered),
                    ..failure
                });
            }
        };
        // The read just taken has no such node: the action could only
        // fail, so it is not sent and the caller learns why.
        if before
            .as_ref()
            .is_some_and(|(_, target)| !target.state.exists)
        {
            return Err(CallFailure::with_code(
                format!("node `{target}` not found"),
                crate::error_code::NOT_FOUND,
                op.acts().then_some(DeliveryStatus::NotDelivered),
            ));
        }
        let mut resp = self.call(op)?;
        let Some((before_rev, before_state)) = before else {
            return Ok(resp);
        };
        let before_secret = before_state.secret;
        let unchanged_rev = before_rev.is_some_and(|rev| {
            self.call_response(&Op::Revision)
                .is_ok_and(|probe| probe.ok && probe.rev == Some(rev))
        });
        let after = if unchanged_rev {
            Some(before_state.clone())
        } else {
            match self.call_response(&Op::snapshot()) {
                Ok(snap) => read_target(&snap, &target).map(|(_, state)| state),
                Err(_) => None,
            }
        };
        if let Some(Target {
            state: mut after,
            secret,
        }) = after
        {
            after.changed = after != before_state.state;
            if after.changed {
                resp.delivery_status = Some(DeliveryStatus::DeliveredVerified);
            }
            // The masked value told whether a secret changed; the reply
            // says only that.
            if secret || before_secret {
                after.value = None;
            }
            resp.after = Some(after);
        }
        Ok(resp)
    }

    /// After an action: wait up to `timeout_ms` for `wait`, then return the
    /// new tree as [compact lines](crate::diff::compact_lines). The tree is
    /// fetched as plain JSON and made compact here, so every host serves
    /// it. A wait that is not met fails with the host's `wait_until`
    /// error; the failure has no `delivery_status`, since the action itself
    /// has already been answered: callers set it from that reply.
    ///
    /// # Errors
    ///
    /// A [`CallFailure`] on `ok: false` or transport failure.
    pub fn wait_after(&mut self, wait: &WaitFor, timeout_ms: u64) -> Result<String, CallFailure> {
        self.call(&Op::wait_until(wait.spec(), timeout_ms))?;
        let snap = self.call(&Op::snapshot())?;
        let tree = snap
            .tree
            .ok_or_else(|| CallFailure::from_message("snapshot after the wait carried no tree"))?;
        Ok(crate::diff::compact_lines(&tree))
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
            let info = hello.hello.ok_or("hello has no info")?;
            self.hello_protocol = Some(info.protocol);
            self.features = Some(info.features);
            self.refresh_features_on_reconnect = false;
        }
        let host_protocol = self.hello_protocol;
        let features = self.features.as_deref().unwrap_or_default();
        if features.iter().any(|f| f == feature) {
            return Ok(None);
        }
        // `within` is a field capability, not an op: a 0.4.1 host would
        // ignore it and press the centre, so name the boundary explicitly.
        if feature == "press_within" {
            let protocol = host_protocol.map_or_else(
                || "an unreported wire protocol".to_string(),
                |version| format!("wire protocol {version}"),
            );
            return Err(format!(
                "unsupported_op: this host speaks {protocol} and does not list `press_within` in hello.features; `within` requires gpui-agent 0.5.0 or another host advertising it (a 0.4.1 host would ignore `within` and press the centre)"
            ));
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

    /// A semantic `click` on `target`.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn click(&mut self, target: impl Into<String>) -> Result<Response, String> {
        self.click_with_delivery(target, DeliveryMode::Semantic)
    }

    /// A `click` on `target` delivered by `delivery`.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
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

    /// A semantic `type` of `text` into `target`.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn type_text(
        &mut self,
        target: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Response, String> {
        self.type_with_delivery(target, text, DeliveryMode::Semantic)
    }

    /// A `type` of `text` into `target` delivered by `delivery`.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
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

    /// A semantic `key` press on `target`.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn key(
        &mut self,
        target: impl Into<String>,
        key: impl Into<String>,
    ) -> Result<Response, String> {
        self.key_with_delivery(target, key, DeliveryMode::Semantic)
    }

    /// A `key` press on `target` delivered by `delivery`.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
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

    /// A `set_value` on `target`.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
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

    /// Check a node against `spec` (a failure is `assert_failed`).
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn assert(&mut self, spec: AssertSpec) -> Result<Response, String> {
        self.expect_ok(Op::Assert { spec })
    }

    /// `invoke` a host-registered named action with JSON `args`.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
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

    /// Poll until `hello.ready` (the host's default timeout).
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn wait_ready(&mut self) -> Result<Response, String> {
        self.expect_ok(Op::Wait { timeout_ms: None })
    }

    /// Poll `spec` until it holds or `timeout_ms` elapses.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn wait_until(&mut self, spec: AssertSpec, timeout_ms: u64) -> Result<Response, String> {
        self.expect_ok(Op::WaitUntil { timeout_ms, spec })
    }

    /// The host's registered Action-id keybindings.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn keybindings(&mut self) -> Result<Response, String> {
        self.expect_ok(Op::Keybindings)
    }

    /// `keybindings` result parsed to a catalog (`null`/absent → empty).
    /// Shared by the policy runner and MCP `act` so neither re-parses
    /// the `{"keybindings":[…]}` envelope itself.
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn keybinding_catalog(&mut self) -> Result<Vec<crate::keybinding::KeybindingInfo>, String> {
        let resp = self.keybindings()?;
        crate::keybinding::KeybindingInfo::catalog_from(resp.result.as_ref())
    }

    /// `events` — poll the host's bounded event stream (M9).
    /// `since` returns events with `seq > N` (`None` → all retained);
    /// the reply's `latest` is the cursor for the next poll. Fails on
    /// hosts without a ring ("events unsupported on this host").
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    pub fn events(
        &mut self,
        since: Option<u64>,
        limit: Option<u32>,
    ) -> Result<crate::event::EventPoll, String> {
        let resp = self.expect_ok(Op::Events { since, limit })?;
        serde_json::from_value(resp.result.unwrap_or_default())
            .map_err(|err| format!("bad events response: {err}"))
    }

    /// Fire a `keybinding` Action (optionally `confirm`-gated, `activate`d).
    ///
    /// # Errors
    ///
    /// A transport failure, or the host's error on `ok: false`.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "confirm and activate are wire flags of the op"
    )]
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

/// The node state [`AgentClient::wait_after`] waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WaitFor {
    /// A node with this id is in the tree.
    Present(String),
    /// No node with this id is in the tree.
    Gone(String),
}

impl WaitFor {
    /// The node id waited on.
    pub fn id(&self) -> &str {
        match self {
            Self::Present(id) | Self::Gone(id) => id,
        }
    }

    /// The `wait_until` condition.
    pub fn spec(&self) -> AssertSpec {
        AssertSpec {
            target: self.id().to_string(),
            exists: Some(matches!(self, Self::Present(_))),
            ..AssertSpec::default()
        }
    }
}

/// Classify a transport failure. Nothing written means `connect_failed`
/// and `not_delivered`; a written request with no usable reply means
/// `no_reply` and `delivery_uncertain`, unless the text names a refusal
/// (an unauthenticated challenge, a client-side `unsupported_op`).
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "one flag: whether the op was an action (sets delivery status)"
)]
fn transport_failure(err: SendError, acts: bool) -> CallFailure {
    let mut code = error_code_of(&err.message);
    if code == error_code::HOST_ERROR || (!err.sent && code == error_code::NO_REPLY) {
        code = if err.sent {
            error_code::NO_REPLY
        } else {
            error_code::CONNECT_FAILED
        };
    }
    let delivery = if !err.sent || error_code::refused_before_dispatch(code) {
        DeliveryStatus::NotDelivered
    } else {
        DeliveryStatus::DeliveryUncertain
    };
    CallFailure::with_code(err.message, code, acts.then_some(delivery))
}

/// A read-back of the target, and whether it is a
/// [secret](crate::tree::UiNode::is_secret) node: its `value` is then
/// masked, compared, and left out of the reply.
#[derive(Debug, Clone)]
struct Target {
    state: ReadBack,
    secret: bool,
}

/// The snapshot's revision and `target`'s state in it. `None` when the
/// snapshot failed or the id is ambiguous, so there is nothing to compare.
/// A secret value is masked here as well, for hosts that predate redaction.
fn read_target(snap: &Response, target: &str) -> Option<(Option<u64>, Target)> {
    let tree = snap.tree.as_ref().filter(|_| snap.ok)?;
    let rev = snap.rev.or(tree.rev);
    let found = tree.find_all(target);
    let state = match found.as_slice() {
        [] => Target {
            state: ReadBack {
                exists: false,
                value: None,
                checked: None,
                focused: None,
                enabled: None,
                changed: false,
            },
            secret: false,
        },
        [node] => Target {
            state: ReadBack {
                exists: true,
                value: node.redacted_value().map(std::borrow::Cow::into_owned),
                checked: node.checked,
                focused: Some(node.focused),
                enabled: Some(node.enabled),
                changed: false,
            },
            secret: node.is_secret(),
        },
        _ => return None,
    };
    Some((rev, state))
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "one flag: mask secret values for hosts predating host-side redaction"
)]
fn parse_response_line(line: &[u8], legacy_secret_redaction: bool) -> Result<Response, String> {
    if parse_challenge_line(line).is_ok() {
        return Err("automation token required".into());
    }
    let mut resp: Response =
        serde_json::from_slice(line).map_err(|err| format!("bad response: {err}"))?;
    // Hosts that predate `error_code` send only the prefixed text; classify
    // it with the same table the server uses.
    resp.fill_error_code();
    // Hosts that predate redaction send secret values in full: mask them on
    // arrival, so no output built from this tree can show one.
    if let Some(tree) = resp.tree.as_mut() {
        tree.redact_secrets();
    }
    if legacy_secret_redaction {
        drop_legacy_secret_fingerprints(&mut resp);
    }
    Ok(resp)
}

fn drop_legacy_secret_fingerprints(resp: &mut Response) {
    if resp.tree.is_none() {
        resp.fingerprints = None;
        return;
    }
    let tree = resp.tree.as_ref().expect("tree checked above");
    if let Some(fingerprints) = resp.fingerprints.as_mut() {
        fingerprints.retain(|id, _| !tree.find_all(id).iter().any(|node| node.is_secret()));
    }
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

    /// One-connection fake host. Each entry answers one request line:
    /// `Some(reply)` is sent with the request's id, `None` closes the
    /// connection without replying. Returns the ops it read.
    fn fake_host(
        replies: Vec<Option<serde_json::Value>>,
    ) -> (SocketAddr, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut ops = Vec::new();
            for reply in replies {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                ops.push(request["op"].as_str().unwrap_or_default().to_string());
                let Some(mut reply) = reply else {
                    break;
                };
                reply["v"] = serde_json::json!(PROTOCOL_VERSION);
                reply["id"] = request["id"].clone();
                serde_json::to_writer(&mut stream, &reply).unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
            ops
        });
        (addr, server)
    }

    fn tree_with(value: &str, rev: u64) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "rev": rev,
            "tree": {
                "app": "fake",
                "platform": "headless",
                "ready": true,
                "rev": rev,
                "nodes": [{ "id": "a", "role": "textbox", "name": "A", "value": value, "enabled": true }]
            }
        })
    }

    fn client_for(addr: SocketAddr) -> AgentClient {
        AgentClient::connect(addr).with_timeout(Duration::from_millis(600))
    }

    fn legacy_since_host() -> (SocketAddr, thread::JoinHandle<Vec<serde_json::Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut secret =
            crate::tree::UiNode::new("secret", "textbox", "Password").with_value("hunter2");
        secret.states.push("secret".into());
        let raw_fingerprint = secret.fingerprint(None);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut requests = Vec::new();
            for _ in 0..2 {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                requests.push(request.clone());
                let response = match request["op"].as_str() {
                    Some("hello") => serde_json::json!({
                        "ok": true,
                        "hello": {
                            "protocol": PROTOCOL_VERSION,
                            "app": "legacy",
                            "platform": "headless",
                            "ready": true,
                            "features": []
                        }
                    }),
                    Some("snapshot") if request.get("since").is_some() => serde_json::json!({
                        "ok": true,
                        "result": {
                            "diff": {
                                "added": [],
                                "removed": [],
                                "changed": [{"id": "secret", "value": "hunter2"}]
                            }
                        },
                        "fingerprints": {"secret": raw_fingerprint, "plain": 456}
                    }),
                    Some("snapshot") => serde_json::json!({
                        "ok": true,
                        "rev": 2,
                        "tree": {
                            "app": "legacy",
                            "platform": "headless",
                            "ready": true,
                            "nodes": [
                                {
                                    "id": "secret",
                                    "role": "textbox",
                                    "name": "Password",
                                    "value": "hunter2",
                                    "states": ["secret"],
                                    "enabled": true
                                },
                                {
                                    "id": "plain",
                                    "role": "button",
                                    "name": "Submit",
                                    "enabled": true
                                }
                            ]
                        },
                        "fingerprints": {"secret": raw_fingerprint, "plain": 456}
                    }),
                    op => panic!("unexpected op: {op:?}"),
                };
                let mut response = response;
                response["v"] = serde_json::json!(PROTOCOL_VERSION);
                response["id"] = request["id"].clone();
                serde_json::to_writer(&mut stream, &response).unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
            requests
        });
        (addr, server)
    }

    fn legacy_fingerprint_host(
        snapshots: usize,
    ) -> (SocketAddr, thread::JoinHandle<Vec<serde_json::Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut secret =
            crate::tree::UiNode::new("secret", "textbox", "Password").with_value("hunter2");
        secret.states.push("secret".into());
        let raw_fingerprint = secret.fingerprint(None);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut requests = Vec::new();
            for _ in 0..=snapshots {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                requests.push(request.clone());
                let response = match request["op"].as_str() {
                    Some("hello") => serde_json::json!({
                        "ok": true,
                        "hello": {
                            "protocol": PROTOCOL_VERSION,
                            "app": "legacy",
                            "platform": "headless",
                            "ready": true,
                            "features": []
                        }
                    }),
                    Some("snapshot") => serde_json::json!({
                        "ok": true,
                        "rev": 2,
                        "tree": {
                            "app": "legacy",
                            "platform": "headless",
                            "ready": true,
                            "nodes": [
                                {
                                    "id": "secret",
                                    "role": "textbox",
                                    "name": "Password",
                                    "value": "hunter2",
                                    "states": ["secret"],
                                    "enabled": true
                                },
                                {
                                    "id": "plain",
                                    "role": "button",
                                    "name": "Submit",
                                    "enabled": true
                                }
                            ]
                        },
                        "fingerprints": {"secret": raw_fingerprint, "plain": 456}
                    }),
                    op => panic!("unexpected op: {op:?}"),
                };
                let mut response = response;
                response["v"] = serde_json::json!(PROTOCOL_VERSION);
                response["id"] = request["id"].clone();
                serde_json::to_writer(&mut stream, &response).unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
            requests
        });
        (addr, server)
    }

    fn assert_legacy_snapshot_is_sanitized(response: &Response) {
        let tree = response.tree.as_ref().expect("full tree");
        assert_eq!(
            tree.find("secret").and_then(|node| node.value.as_deref()),
            Some("•••••••")
        );
        let fingerprints = response.fingerprints.as_ref().expect("fingerprints");
        assert!(!fingerprints.contains_key("secret"));
        assert_eq!(fingerprints.get("plain"), Some(&456));
    }

    #[test]
    fn legacy_since_snapshot_requests_a_full_masked_tree() {
        let (addr, server) = legacy_since_host();
        let mut client = client_for(addr);
        let response = client
            .call(&Op::Snapshot {
                since: Some(1),
                format: Some(SnapshotFormat::Compact),
                fingerprints: Some(true),
            })
            .unwrap();
        let tree = response.tree.expect("full tree");
        assert_eq!(
            tree.find("secret").and_then(|node| node.value.as_deref()),
            Some("•••••••")
        );
        assert_eq!(response.result, Some(serde_json::json!({ "full": true })));
        let fingerprints = response.fingerprints.unwrap();
        assert!(!fingerprints.contains_key("secret"));
        assert_eq!(fingerprints.get("plain"), Some(&456));
        let requests = server.join().unwrap();
        assert_eq!(requests[0]["op"], "hello");
        assert_eq!(requests[1]["op"], "snapshot");
        assert!(requests[1].get("since").is_none(), "{requests:?}");
        assert!(requests[1].get("format").is_none(), "{requests:?}");
    }

    #[test]
    fn rpc_op_and_pipeline_sanitize_legacy_secret_fingerprints() {
        let (addr, server) = legacy_fingerprint_host(3);
        let mut client = client_for(addr);
        let op = Op::Snapshot {
            since: None,
            format: None,
            fingerprints: Some(true),
        };
        let direct = client.rpc_op(&op).unwrap();
        assert_legacy_snapshot_is_sanitized(&direct);

        let pipeline = client.rpc_pipeline(&[&op, &op]).unwrap();
        assert_eq!(pipeline.len(), 2);
        for response in &pipeline {
            assert_legacy_snapshot_is_sanitized(response);
        }

        let requests = server.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|request| request["op"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["hello", "snapshot", "snapshot", "snapshot"]
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "a scripted two-phase fixture conversation"
    )]
    fn pipeline_relearns_features_after_reconnect() {
        fn read_request(reader: &mut BufReader<std::net::TcpStream>) -> serde_json::Value {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            serde_json::from_str(&line).unwrap()
        }

        fn send_reply(
            stream: &mut std::net::TcpStream,
            request: &serde_json::Value,
            mut reply: serde_json::Value,
        ) {
            reply["v"] = serde_json::json!(PROTOCOL_VERSION);
            reply["id"] = request["id"].clone();
            serde_json::to_writer(&mut *stream, &reply).unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut secret =
            crate::tree::UiNode::new("secret", "textbox", "Password").with_value("•••••••");
        secret.states.push("secret".into());
        let secret_fingerprint = secret.fingerprint(None);
        let plain = crate::tree::UiNode::new("plain", "button", "Submit");
        let plain_fingerprint = plain.fingerprint(None);
        let snapshot = serde_json::json!({
            "ok": true,
            "rev": 2,
            "tree": {
                "app": "modern",
                "platform": "headless",
                "ready": true,
                "rev": 2,
                "nodes": [
                    {
                        "id": "secret",
                        "role": "textbox",
                        "name": "Password",
                        "value": "•••••••",
                        "states": ["secret"],
                        "enabled": true
                    },
                    {
                        "id": "plain",
                        "role": "button",
                        "name": "Submit",
                        "enabled": true
                    }
                ]
            },
            "fingerprints": {
                "secret": secret_fingerprint,
                "plain": plain_fingerprint
            }
        });
        let server = thread::spawn(move || {
            let mut ops = Vec::new();
            {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let request = read_request(&mut reader);
                assert_eq!(request["op"], "hello");
                ops.push("hello".to_owned());
                send_reply(
                    &mut stream,
                    &request,
                    serde_json::json!({
                        "ok": true,
                        "hello": {
                            "protocol": PROTOCOL_VERSION,
                            "app": "modern",
                            "platform": "headless",
                            "ready": true,
                            "features": ["secret_redaction"]
                        }
                    }),
                );
            }
            {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let first = read_request(&mut reader);
                ops.push(first["op"].as_str().unwrap().to_owned());
                let mut snapshots_left = 2;
                if first["op"] == "hello" {
                    send_reply(
                        &mut stream,
                        &first,
                        serde_json::json!({
                            "ok": true,
                            "hello": {
                                "protocol": PROTOCOL_VERSION,
                                "app": "modern",
                                "platform": "headless",
                                "ready": true,
                                "features": ["secret_redaction"]
                            }
                        }),
                    );
                } else {
                    assert_eq!(first["op"], "snapshot");
                    send_reply(&mut stream, &first, snapshot.clone());
                    snapshots_left -= 1;
                }
                while snapshots_left > 0 {
                    let request = read_request(&mut reader);
                    assert_eq!(request["op"], "snapshot");
                    ops.push("snapshot".to_owned());
                    send_reply(&mut stream, &request, snapshot.clone());
                    snapshots_left -= 1;
                }
            }
            ops
        });

        let mut client = client_for(addr).with_timeout(Duration::from_secs(2));
        assert!(
            client
                .features()
                .unwrap()
                .iter()
                .any(|feature| feature == "secret_redaction")
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let closed = client
                .session
                .as_ref()
                .expect("hello opened a session")
                .peer_closed();
            if !matches!(closed, Ok(false)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "fake host did not close connection"
            );
            thread::sleep(Duration::from_millis(5));
        }

        let op = Op::Snapshot {
            since: None,
            format: None,
            fingerprints: Some(true),
        };
        let responses = client.rpc_pipeline(&[&op, &op]).unwrap();
        assert_eq!(responses.len(), 2);
        for response in responses {
            let fingerprints = response.fingerprints.expect("fingerprints");
            assert!(fingerprints.contains_key("secret"));
            assert!(fingerprints.contains_key("plain"));
        }
        assert_eq!(
            server.join().unwrap(),
            vec!["hello", "hello", "snapshot", "snapshot"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn legacy_secret_fingerprints_are_dropped_from_snapshots_and_diffs() {
        let tree = serde_json::json!({
            "app": "fake",
            "platform": "headless",
            "ready": true,
            "nodes": [
                {
                    "id": "secret",
                    "role": "textbox",
                    "name": "Password",
                    "value": "hunter2",
                    "states": ["secret"],
                    "enabled": true
                },
                {
                    "id": "plain",
                    "role": "button",
                    "name": "Submit",
                    "enabled": true
                }
            ]
        });
        let fingerprints = serde_json::json!({"secret": 123, "plain": 456});
        let (addr, server) = fake_host(vec![
            Some(serde_json::json!({
                "ok": true,
                "hello": {
                    "protocol": PROTOCOL_VERSION,
                    "app": "fake",
                    "platform": "headless",
                    "ready": true,
                    "features": []
                }
            })),
            Some(serde_json::json!({
                "ok": true,
                "tree": tree,
                "fingerprints": fingerprints
            })),
            Some(serde_json::json!({
                "ok": true,
                "result": { "diff": { "added": [], "removed": [], "changed": [] } },
                "fingerprints": fingerprints
            })),
            Some(serde_json::json!({
                "ok": true,
                "tree": tree,
                "result": { "full": true },
                "fingerprints": fingerprints
            })),
        ]);
        let mut client = client_for(addr);
        let full = client
            .call(&Op::Snapshot {
                since: None,
                format: None,
                fingerprints: Some(true),
            })
            .unwrap();
        let full_fingerprints = full.fingerprints.unwrap();
        assert!(!full_fingerprints.contains_key("secret"));
        assert_eq!(full_fingerprints.get("plain"), Some(&456));

        let diff = client
            .call(&Op::Snapshot {
                since: Some(1),
                format: None,
                fingerprints: Some(true),
            })
            .unwrap();
        assert!(diff.fingerprints.is_none());

        let resync = client
            .call(&Op::Snapshot {
                since: Some(2),
                format: None,
                fingerprints: Some(true),
            })
            .unwrap();
        let resync_fingerprints = resync.fingerprints.unwrap();
        assert!(!resync_fingerprints.contains_key("secret"));
        assert_eq!(resync_fingerprints.get("plain"), Some(&456));
        assert_eq!(
            server.join().unwrap(),
            ["hello", "snapshot", "snapshot", "snapshot"]
        );
    }

    #[test]
    fn nothing_listening_is_not_delivered() {
        let addr = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let failure = client_for(addr).call(&Op::click("a")).unwrap_err();
        assert_eq!(
            failure.error_code,
            error_code::CONNECT_FAILED,
            "{failure:?}"
        );
        assert_eq!(failure.delivery_status, Some(DeliveryStatus::NotDelivered));
        assert!(failure.hint.is_some());
    }

    #[test]
    fn a_host_that_drops_after_the_write_is_delivery_uncertain() {
        let (addr, server) = fake_host(vec![None]);
        let failure = client_for(addr).call(&Op::click("a")).unwrap_err();
        assert_eq!(server.join().unwrap(), ["click"]);
        assert_eq!(failure.error_code, error_code::NO_REPLY, "{failure:?}");
        assert_eq!(
            failure.delivery_status,
            Some(DeliveryStatus::DeliveryUncertain)
        );
    }

    #[test]
    fn host_replies_classify_delivery() {
        let (addr, server) = fake_host(vec![
            Some(serde_json::json!({ "ok": true })),
            // A host that predates `error_code`: only the prefixed text.
            Some(
                serde_json::json!({ "ok": false, "error": "stale_target: `a` changed between snapshot and dispatch" }),
            ),
            Some(serde_json::json!({ "ok": false, "error": "cannot click `a`" })),
            Some(serde_json::json!({ "ok": false, "error": "node `a` not found" })),
        ]);
        let mut client = client_for(addr);
        let ok = client.call(&Op::click("a")).unwrap();
        assert_eq!(
            ok.delivery_status,
            Some(DeliveryStatus::DeliveredUnverified)
        );
        assert_eq!(ok.error_code, None);

        let stale = client.call(&Op::click("a")).unwrap_err();
        assert_eq!(stale.error_code, error_code::STALE_TARGET);
        assert_eq!(stale.hint.as_deref(), Some("take a new snapshot and retry"));
        assert_eq!(stale.delivery_status, Some(DeliveryStatus::NotDelivered));

        let mid_dispatch = client.call(&Op::click("a")).unwrap_err();
        assert_eq!(mid_dispatch.error_code, error_code::HOST_ERROR);
        assert_eq!(
            mid_dispatch.delivery_status,
            Some(DeliveryStatus::DeliveryUncertain)
        );

        // A read is not an action: code, but no delivery status.
        let missing = client
            .call(&Op::Assert {
                spec: AssertSpec {
                    target: "a".into(),
                    name: None,
                    value: None,
                    role: None,
                    checked: None,
                    exists: None,
                    visible: None,
                    in_viewport: None,
                },
            })
            .unwrap_err();
        assert_eq!(missing.error_code, error_code::NOT_FOUND);
        assert_eq!(missing.delivery_status, None);
        server.join().unwrap();
    }

    #[test]
    fn the_string_api_still_returns_old_host_errors_as_text() {
        let (addr, server) = fake_host(vec![Some(
            serde_json::json!({ "ok": false, "error": "target_gone: `a` is no longer in the tree" }),
        )]);
        let mut client = client_for(addr);
        let resp = client.rpc(Op::click("a")).unwrap();
        assert_eq!(resp.error_code.as_deref(), Some(error_code::TARGET_GONE));
        assert_eq!(resp.delivery_status, None);
        server.join().unwrap();
    }

    #[test]
    fn read_back_verifies_a_changed_target() {
        let (addr, server) = fake_host(vec![
            Some(tree_with("x", 1)),
            Some(serde_json::json!({ "ok": true })),
            Some(serde_json::json!({ "ok": true, "rev": 2 })),
            Some(tree_with("xy", 2)),
        ]);
        let resp = client_for(addr)
            .act(&Op::type_text("a", "y"), true)
            .unwrap();
        assert_eq!(
            server.join().unwrap(),
            ["snapshot", "type", "revision", "snapshot"]
        );
        assert_eq!(
            resp.delivery_status,
            Some(DeliveryStatus::DeliveredVerified)
        );
        let after = resp.after.unwrap();
        assert!(after.exists && after.changed);
        assert_eq!(after.value.as_deref(), Some("xy"));
        assert_eq!(after.enabled, Some(true));
    }

    /// A host that predates redaction sends a password field's real
    /// value: the read-back still says it changed, and never what it is.
    #[test]
    fn read_back_of_a_password_reports_the_change_without_the_value() {
        let password = |value: &str, rev: u64| {
            let mut snap = tree_with(value, rev);
            snap["tree"]["nodes"][0]["role"] = serde_json::json!("password");
            snap
        };
        let (addr, server) = fake_host(vec![
            Some(password("hunter", 1)),
            Some(serde_json::json!({ "ok": true })),
            Some(serde_json::json!({ "ok": true, "rev": 2 })),
            Some(password("hunter2", 2)),
        ]);
        let resp = client_for(addr)
            .act(&Op::type_text("a", "2"), true)
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            resp.delivery_status,
            Some(DeliveryStatus::DeliveredVerified)
        );
        let after = resp.after.unwrap();
        assert!(after.exists && after.changed);
        assert_eq!(after.value, None);
        let json = serde_json::to_string(&after).unwrap();
        assert!(!json.contains("hunter"), "{json}");
    }

    #[test]
    fn an_unchanged_revision_skips_the_second_snapshot() {
        let (addr, server) = fake_host(vec![
            Some(tree_with("x", 7)),
            Some(serde_json::json!({ "ok": true })),
            Some(serde_json::json!({ "ok": true, "rev": 7 })),
        ]);
        let resp = client_for(addr).act(&Op::click("a"), true).unwrap();
        assert_eq!(server.join().unwrap(), ["snapshot", "click", "revision"]);
        assert_eq!(
            resp.delivery_status,
            Some(DeliveryStatus::DeliveredUnverified)
        );
        let after = resp.after.unwrap();
        assert!(after.exists && !after.changed);
    }

    #[test]
    fn read_back_does_not_send_an_action_on_a_missing_target() {
        let (addr, server) = fake_host(vec![Some(tree_with("x", 3))]);
        let failure = client_for(addr).act(&Op::click("b"), true).unwrap_err();
        assert_eq!(server.join().unwrap(), ["snapshot"]);
        assert_eq!(failure.error_code, error_code::NOT_FOUND, "{failure:?}");
        assert_eq!(failure.error, "node `b` not found");
        assert_eq!(failure.delivery_status, Some(DeliveryStatus::NotDelivered));
        assert!(failure.hint.is_some());
    }

    #[test]
    fn no_read_back_sends_only_the_action() {
        let (addr, server) = fake_host(vec![Some(serde_json::json!({ "ok": true }))]);
        let resp = client_for(addr).act(&Op::click("a"), false).unwrap();
        assert_eq!(server.join().unwrap(), ["click"]);
        assert!(resp.after.is_none());
        assert_eq!(
            resp.delivery_status,
            Some(DeliveryStatus::DeliveredUnverified)
        );
    }

    #[test]
    fn wait_after_waits_then_returns_the_compact_tree() {
        let (addr, server) = fake_host(vec![
            Some(serde_json::json!({ "ok": true })),
            Some(tree_with("x", 4)),
        ]);
        let compact = client_for(addr)
            .wait_after(&WaitFor::Present("a".into()), 900)
            .unwrap();
        assert_eq!(server.join().unwrap(), ["wait_until", "snapshot"]);
        assert!(compact.starts_with("a "), "{compact}");
        assert!(compact.contains("value=\"x\""), "{compact}");
        let gone = WaitFor::Gone("a".into()).spec();
        assert_eq!((gone.target.as_str(), gone.exists), ("a", Some(false)));
    }

    #[test]
    fn an_unmet_wait_fails_without_a_snapshot() {
        let (addr, server) = fake_host(vec![Some(
            serde_json::json!({ "ok": false, "error": "wait_until timed out: node `b` not found" }),
        )]);
        let failure = client_for(addr)
            .wait_after(&WaitFor::Present("b".into()), 900)
            .unwrap_err();
        assert_eq!(server.join().unwrap(), ["wait_until"]);
        assert_eq!(failure.error_code, error_code::TIMEOUT, "{failure:?}");
        assert_eq!(failure.delivery_status, None);
    }
}
