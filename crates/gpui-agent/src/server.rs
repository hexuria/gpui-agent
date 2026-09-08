use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::dispatch::handle_request;
use crate::host::AgentHost;
use crate::mailbox::AgentMailbox;
use crate::protocol::{Op, Request};

pub const DEFAULT_ADDR_STR: &str = "127.0.0.1:17421";
pub const DEFAULT_PORT: u16 = 17421;

pub fn default_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT))
}

/// Localhost NDJSON server. One JSON object per line, request → response.
pub struct AgentServer {
    listener: TcpListener,
    token: Option<String>,
    shutdown: Arc<AtomicBool>,
}

impl AgentServer {
    pub fn bind(addr: SocketAddr, token: Option<String>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(false)?;
        Ok(Self {
            listener,
            token,
            shutdown: Arc::new(AtomicBool::new(false)),
        })
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
        let token = self.token.clone();
        let shutdown = self.shutdown.clone();
        self.listener.set_nonblocking(true).ok();

        while !shutdown.load(Ordering::SeqCst) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let host = host.clone();
                    let token = token.clone();
                    let shutdown = shutdown.clone();
                    thread::spawn(move || {
                        handle_stream_host(stream, host, token.as_deref(), &shutdown);
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
    pub fn serve_mailbox(self, mailbox: AgentMailbox, timeout: Duration) {
        let token = self.token.clone();
        let shutdown = self.shutdown.clone();
        self.listener.set_nonblocking(true).ok();

        while !shutdown.load(Ordering::SeqCst) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let mailbox = mailbox.clone();
                    let token = token.clone();
                    let shutdown = shutdown.clone();
                    thread::spawn(move || {
                        handle_stream_mailbox(
                            stream,
                            mailbox,
                            token.as_deref(),
                            timeout,
                            &shutdown,
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

fn handle_stream_host<H: AgentHost>(
    stream: TcpStream,
    host: Arc<Mutex<H>>,
    token: Option<&str>,
    shutdown: &AtomicBool,
) {
    let _ = stream.set_nodelay(true);
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(err) => {
                let _ = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&crate::Response::err("?", format!("bad json: {err}")))
                        .unwrap()
                );
                continue;
            }
        };
        let shutdown_op = matches!(req.op, Op::Shutdown);
        let resp = {
            let mut host = host.lock().expect("host");
            handle_request(&mut *host, req, token)
        };
        let _ = writeln!(writer, "{}", serde_json::to_string(&resp).unwrap());
        if shutdown_op {
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
) {
    let _ = stream.set_nodelay(true);
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(err) => {
                let _ = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&crate::Response::err("?", format!("bad json: {err}")))
                        .unwrap()
                );
                continue;
            }
        };
        if let Some(expected) = token {
            match req.token.as_deref() {
                Some(got) if got == expected => {}
                _ => {
                    let resp = crate::Response::err(req.id, "automation token required or invalid");
                    let _ = writeln!(writer, "{}", serde_json::to_string(&resp).unwrap());
                    continue;
                }
            }
        }
        let shutdown_op = matches!(req.op, Op::Shutdown);
        let resp = mailbox
            .wait(req, timeout)
            .unwrap_or_else(|err| crate::Response::err("?", err));
        let _ = writeln!(writer, "{}", serde_json::to_string(&resp).unwrap());
        if shutdown_op {
            shutdown.store(true, Ordering::SeqCst);
            break;
        }
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

/// Spawn `serve_mailbox` on a background thread.
pub fn spawn_mailbox(
    addr: SocketAddr,
    token: Option<String>,
    mailbox: AgentMailbox,
    timeout: Duration,
) -> std::io::Result<(SocketAddr, Arc<AtomicBool>)> {
    let server = AgentServer::bind(addr, token)?;
    let bound = server.local_addr()?;
    let flag = server.shutdown_handle();
    thread::spawn(move || server.serve_mailbox(mailbox, timeout));
    Ok((bound, flag))
}
