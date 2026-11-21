use std::mem;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
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

/// One queued request; answer it with [`reply`](Self::reply).
pub struct MailboxRequest {
    pub request: Request,
    sender: mpsc::Sender<Response>,
}

impl MailboxRequest {
    /// Send `response` to the waiting caller. A caller that has already
    /// timed out is gone, and the reply is dropped.
    pub fn reply(self, response: Response) {
        let _ = self.sender.send(response);
    }
}

impl AgentMailbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// The queue, past a panic on another thread that held it. Each use
    /// is a single push, take or length read, so a panic cannot leave the
    /// queue half updated.
    fn queue(&self) -> MutexGuard<'_, Vec<MailboxRequest>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue `request` for the UI thread; its reply arrives on the
    /// returned receiver. A full mailbox replies `mailbox full` at once.
    pub fn push(&self, request: Request) -> mpsc::Receiver<Response> {
        let (tx, rx) = mpsc::channel();
        let mut inner = self.queue();
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

    /// Every queued request, oldest first, for the UI thread to answer.
    pub fn take(&self) -> Vec<MailboxRequest> {
        mem::take(&mut *self.queue())
    }

    /// [`push`](Self::push) `request` and wait up to `timeout` for its
    /// reply.
    ///
    /// A timeout does not withdraw the request: there is no cancellation,
    /// so the UI thread can still drain and run it later, after the caller
    /// has reported the timeout. Callers must not treat a timed-out action
    /// as not run.
    pub fn wait(&self, request: Request, timeout: Duration) -> Result<Response, String> {
        let rx = self.push(request);
        rx.recv_timeout(timeout)
            .map_err(|_| "timed out waiting for the UI thread to drain the agent mailbox".into())
    }

    pub fn len(&self) -> usize {
        self.queue().len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue().is_empty()
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
    fn a_poisoned_mailbox_still_queues() {
        let mailbox = AgentMailbox::new();
        let inner = Arc::clone(&mailbox.inner);
        let panicked = std::thread::spawn(move || {
            let _guard = inner.lock().unwrap();
            panic!("panicked while holding the mailbox");
        })
        .join();
        assert!(panicked.is_err() && mailbox.inner.is_poisoned());
        let _rx = mailbox.push(Request::new("1", Op::Hello));
        assert_eq!(mailbox.len(), 1);
        assert_eq!(mailbox.take().len(), 1);
        assert!(mailbox.is_empty());
    }

    #[test]
    fn take_is_empty_after_drain() {
        let mailbox = AgentMailbox::new();
        let _rx = mailbox.push(Request::new("1", Op::Hello));
        assert_eq!(mailbox.len(), 1);
        let taken = mailbox.take();
        assert_eq!(taken.len(), 1);
        assert!(mailbox.is_empty());
        assert!(mailbox.take().is_empty());
    }
}
