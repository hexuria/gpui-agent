use std::mem;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::protocol::{Request, Response};

/// Cap queued work so a stalled UI thread cannot grow without bound.
pub const MAX_MAILBOX_DEPTH: usize = 128;

/// Cross-thread inbox so a background TCP server can post work onto the
/// GPUI UI thread (or any other single-threaded host).
#[derive(Clone)]
pub struct AgentMailbox {
    inner: Arc<Mutex<Vec<MailboxRequest>>>,
}

impl Default for AgentMailbox {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::with_capacity(16))),
        }
    }
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
        let mut inner = self.inner.lock().expect("mailbox");
        if inner.len() >= MAX_MAILBOX_DEPTH {
            let _ = tx.send(Response::err(
                request.id,
                "mailbox full (UI thread not draining)",
            ));
            return rx;
        }
        inner.push(MailboxRequest {
            request,
            sender: tx,
        });
        rx
    }

    pub fn take(&self) -> Vec<MailboxRequest> {
        mem::take(&mut *self.inner.lock().expect("mailbox"))
    }

    pub fn wait(&self, request: Request, timeout: Duration) -> Result<Response, String> {
        let rx = self.push(request);
        rx.recv_timeout(timeout)
            .map_err(|_| "timed out waiting for the UI thread to drain the agent mailbox".into())
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("mailbox").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Op;

    #[test]
    fn push_rejects_when_full() {
        let mailbox = AgentMailbox::new();
        for i in 0..MAX_MAILBOX_DEPTH {
            let _rx = mailbox.push(Request::new(i.to_string(), Op::Hello));
        }
        assert_eq!(mailbox.len(), MAX_MAILBOX_DEPTH);
        let resp = mailbox
            .wait(
                Request::new("overflow", Op::Hello),
                Duration::from_millis(50),
            )
            .unwrap();
        assert!(!resp.ok);
        assert!(
            resp.error.unwrap().contains("mailbox full"),
            "expected mailbox full error"
        );
        assert_eq!(mailbox.len(), MAX_MAILBOX_DEPTH);
    }

    #[test]
    fn take_is_empty_after_drain() {
        let mailbox = AgentMailbox::new();
        let _rx = mailbox.push(Request::new("1", Op::Hello));
        assert_eq!(mailbox.len(), 1);
        let taken = mailbox.take();
        assert_eq!(taken.len(), 1);
        assert_eq!(mailbox.len(), 0);
        assert!(mailbox.take().is_empty());
    }
}
