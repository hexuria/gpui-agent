use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::protocol::{Request, Response};

/// Cross-thread inbox so a background TCP server can post work onto the
/// GPUI UI thread (or any other single-threaded host).
#[derive(Clone, Default)]
pub struct AgentMailbox {
    inner: Arc<Mutex<Vec<(Request, mpsc::Sender<Response>)>>>,
}

pub struct MailboxRequest {
    pub request: Request,
    sender: mpsc::Sender<Response>,
}

impl MailboxRequest {
    pub fn reply(self, response: Response) {
        let _ = self.sender.send(response);
    }
}

impl AgentMailbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, request: Request) -> mpsc::Receiver<Response> {
        let (tx, rx) = mpsc::channel();
        self.inner.lock().expect("mailbox").push((request, tx));
        rx
    }

    pub fn take(&self) -> Vec<MailboxRequest> {
        self.inner
            .lock()
            .expect("mailbox")
            .drain(..)
            .map(|(request, sender)| MailboxRequest { request, sender })
            .collect()
    }

    pub fn wait(&self, request: Request, timeout: Duration) -> Result<Response, String> {
        let rx = self.push(request);
        rx.recv_timeout(timeout)
            .map_err(|_| "timed out waiting for the UI thread to drain the agent mailbox".into())
    }
}
