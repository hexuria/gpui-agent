//! Bounded host event stream (PRD M9). Apps bridge `log`/`tracing` into
//! an [`EventRing`] they own; agents poll `{"op":"events","since":N}`
//! instead of scraping stderr.
//!
//! Determinism (I5): the only ordering field is `seq`, allocated by the
//! ring at `push`. Events carry no clocks and no host timings.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Events retained per host — the stream's memory bound.
pub const EVENT_RING_CAPACITY: usize = 1024;

/// Default page size when an `events` poll omits `limit`.
pub const EVENTS_DEFAULT_LIMIT: usize = 256;

/// Severity of one event; serialized lowercase on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl EventLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for EventLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One host-emitted record. `seq` is assigned by the ring, monotonically
/// increasing from 1 — it is the ordering field (no timestamps).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub seq: u64,
    pub level: EventLevel,
    /// Emitting component — module path or subsystem name.
    pub target: String,
    /// Pre-rendered message text.
    pub message: String,
}

/// One `events` poll answer: the retained tail plus cursor state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPoll {
    pub events: Vec<AgentEvent>,
    /// Highest allocated seq (0 when nothing was ever pushed). Pollers
    /// advance their cursor to `latest` to avoid re-receiving.
    pub latest: u64,
    /// True when `since + 1` is below the ring's base seq — the caller
    /// lost events to capacity eviction and must not treat the gap as
    /// "nothing happened".
    pub lagged: bool,
}

/// Bounded FIFO of [`AgentEvent`]s owned by the host. `push` past
/// `capacity` evicts the oldest event; `seq`s are never reused.
pub struct EventRing {
    capacity: usize,
    inner: Mutex<RingBuf>,
}

struct RingBuf {
    /// Next seq to allocate (starts at 1).
    next: u64,
    buf: VecDeque<AgentEvent>,
}

impl EventRing {
    /// A ring holding at most `capacity` events (0 → never retains).
    /// [`EventRing::default`] uses [`EVENT_RING_CAPACITY`].
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            inner: Mutex::new(RingBuf {
                next: 1,
                buf: VecDeque::new(),
            }),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Append one event; returns its allocated seq.
    pub fn push(
        &self,
        level: EventLevel,
        target: impl Into<String>,
        message: impl Into<String>,
    ) -> u64 {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let seq = inner.next;
        inner.next += 1;
        if self.capacity > 0 && inner.buf.len() == self.capacity {
            inner.buf.pop_front();
        }
        if self.capacity > 0 {
            inner.buf.push_back(AgentEvent {
                seq,
                level,
                target: target.into(),
                message: message.into(),
            });
        }
        seq
    }

    /// Events with `seq > since` (`None` → all retained), oldest first,
    /// capped at `limit` (clamped to the ring's capacity).
    pub fn since(&self, since: Option<u64>, limit: usize) -> EventPoll {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let limit = limit.min(self.capacity.max(1));
        let base_seq = inner.next - inner.buf.len() as u64;
        let events: Vec<AgentEvent> = inner
            .buf
            .iter()
            .filter(|e| since.is_none_or(|s| e.seq > s))
            .take(limit)
            .cloned()
            .collect();
        EventPoll {
            events,
            latest: inner.next.saturating_sub(1),
            lagged: since.is_some_and(|s| s + 1 < base_seq),
        }
    }

    /// Highest allocated seq (0 when empty).
    pub fn latest(&self) -> u64 {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.next - 1
    }

    pub fn is_empty(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.buf.is_empty()
    }
}

impl Default for EventRing {
    fn default() -> Self {
        Self::new(EVENT_RING_CAPACITY)
    }
}

impl std::fmt::Debug for EventRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("EventRing")
            .field("capacity", &self.capacity)
            .field("len", &inner.buf.len())
            .field("latest", &(inner.next - 1))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_allocates_monotonic_seq() {
        let ring = EventRing::default();
        assert_eq!(ring.push(EventLevel::Info, "app", "one"), 1);
        assert_eq!(ring.push(EventLevel::Warn, "app", "two"), 2);
        let poll = ring.since(None, EVENTS_DEFAULT_LIMIT);
        assert_eq!(poll.latest, 2);
        assert!(!poll.lagged);
        assert_eq!(
            poll.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(poll.events[0].message, "one");
    }

    #[test]
    fn since_filters_and_reports_latest() {
        let ring = EventRing::default();
        for i in 1..=5 {
            ring.push(EventLevel::Info, "t", format!("m{i}"));
        }
        let poll = ring.since(Some(3), EVENTS_DEFAULT_LIMIT);
        assert_eq!(
            poll.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [4, 5]
        );
        assert_eq!(poll.latest, 5);
        assert!(!poll.lagged);
        // Polling at latest yields nothing new.
        assert!(ring.since(Some(5), EVENTS_DEFAULT_LIMIT).events.is_empty());
    }

    #[test]
    fn capacity_evicts_oldest_and_marks_lagged() {
        let ring = EventRing::new(3);
        for i in 1..=5 {
            ring.push(EventLevel::Info, "t", format!("m{i}"));
        }
        let poll = ring.since(None, EVENTS_DEFAULT_LIMIT);
        assert_eq!(
            poll.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [3, 4, 5],
            "oldest evicted"
        );
        // since=1 asks for seq>1 but base is 3 → lagged.
        let lag = ring.since(Some(1), EVENTS_DEFAULT_LIMIT);
        assert!(lag.lagged);
        assert_eq!(lag.events[0].seq, 3);
        // since=0 is lagged: seq 1..2 were evicted — a cursor at the
        // start is missing real events.
        assert!(ring.since(Some(0), EVENTS_DEFAULT_LIMIT).lagged);
        // since within the retained window is not lagged.
        assert!(!ring.since(Some(4), EVENTS_DEFAULT_LIMIT).lagged);
    }

    #[test]
    fn limit_caps_page_size() {
        let ring = EventRing::new(10);
        for i in 0..8 {
            ring.push(EventLevel::Debug, "t", format!("m{i}"));
        }
        assert_eq!(ring.since(None, 3).events.len(), 3);
        // Over-capacity limits clamp to the ring, never panic.
        assert_eq!(ring.since(None, usize::MAX).events.len(), 8);
    }

    #[test]
    fn zero_capacity_never_retaines_but_seq_still_monotonic() {
        let ring = EventRing::new(0);
        assert_eq!(ring.push(EventLevel::Info, "t", "x"), 1);
        let poll = ring.since(None, 10);
        assert!(poll.events.is_empty());
        assert_eq!(poll.latest, 1);
    }
}
