//! The event log behind WS frames, GET /api/v1/events and MCP `wait_for_events`: append-only, in
//! review.db (`event_log`, seq = cursor), with a broadcast channel for live subscribers and a
//! condition variable for blocking long-polls.
//!
//! Retention (DESIGN §6): the newest [`KEEP_MAX`] events, and nothing older than [`MAX_AGE`]
//! beyond the newest [`KEEP_MIN`]; pruned at open and every [`PRUNE_EVERY`] appends. Seqs are
//! never reused. A cursor older than what is kept is [`EventLog::expired`]: the WS `hello` and
//! GET /events answer `resync: true` and the client refetches its state instead of replaying.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cv_contracts::review::ServerEvent;
use cv_store::ReviewDb;
use tokio::sync::broadcast;

pub struct EventLog {
    db: Arc<ReviewDb>,
    tx: broadcast::Sender<(u64, Arc<ServerEvent>)>,
    last: Mutex<u64>,
    cv: Condvar,
    /// The oldest seq kept (0 = nothing pruned yet); cursors before it cannot be replayed.
    first: AtomicU64,
    retention: Retention,
}

/// At most this many events per read.
const READ_LIMIT: usize = 500;
/// Events kept at most.
pub const KEEP_MAX: u64 = 100_000;
/// Events kept whatever their age.
pub const KEEP_MIN: u64 = 1_000;
/// Older events (beyond [`KEEP_MIN`]) are dropped.
pub const MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);
/// Appends between prunes.
const PRUNE_EVERY: u64 = 1_000;

/// Retention bounds (the constants above; tests shrink them).
#[derive(Clone, Copy, Debug)]
pub struct Retention {
    pub keep_max: u64,
    pub keep_min: u64,
    pub max_age: Duration,
    pub prune_every: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            keep_max: KEEP_MAX,
            keep_min: KEEP_MIN,
            max_age: MAX_AGE,
            prune_every: PRUNE_EVERY,
        }
    }
}

impl EventLog {
    pub fn new(db: Arc<ReviewDb>) -> Self {
        Self::with_retention(db, Retention::default())
    }

    pub fn with_retention(db: Arc<ReviewDb>, retention: Retention) -> Self {
        let last = db.last_seq().unwrap_or(0);
        let log = EventLog {
            db,
            tx: broadcast::channel(4096).0,
            last: Mutex::new(last),
            cv: Condvar::new(),
            first: AtomicU64::new(0),
            retention,
        };
        log.prune();
        log
    }

    /// Applies retention; remembers the oldest seq kept.
    pub fn prune(&self) {
        let r = self.retention;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let cutoff = cv_store::iso_from_ms(now.saturating_sub(r.max_age).as_millis() as i64);
        if let Err(e) = self.db.prune_events(r.keep_max, r.keep_min, &cutoff) {
            eprintln!("cv: event log prune failed: {e}");
        }
        // An emptied log (everything past retention) keeps no event: every earlier cursor expired.
        let first = match self.db.first_seq() {
            Ok(Some(f)) => f,
            Ok(None) => self.cursor() + 1,
            Err(_) => 0,
        };
        self.first.store(first, Ordering::Relaxed);
    }

    /// True when events after `after` were pruned: a replay from it would have a hole, so the
    /// client must resync (refetch its state) instead.
    pub fn expired(&self, after: u64) -> bool {
        let first = self.first.load(Ordering::Relaxed);
        first > 1 && after + 1 < first
    }

    /// Persists the event, then wakes long-polls and live subscribers. Returns its seq.
    pub fn append(&self, event: ServerEvent) -> u64 {
        let json = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
        // Hold `last` while appending so seqs reach subscribers in order.
        let mut last = self.last.lock().unwrap();
        let seq = match self.db.append_event(event.kind(), &json) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cv: event log append failed: {e}");
                return *last;
            }
        };
        *last = seq;
        let _ = self.tx.send((seq, Arc::new(event)));
        self.cv.notify_all();
        drop(last);
        if seq.is_multiple_of(self.retention.prune_every.max(1)) {
            self.prune();
        }
        seq
    }

    /// The newest seq (0 when the log is empty).
    pub fn cursor(&self) -> u64 {
        *self.last.lock().unwrap()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<(u64, Arc<ServerEvent>)> {
        self.tx.subscribe()
    }

    /// Events after `after` (all kinds when `kinds` is None), oldest first.
    pub fn since(&self, after: u64, kinds: Option<&[String]>) -> Vec<(u64, ServerEvent)> {
        self.db
            .events_after(after, kinds, READ_LIMIT)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(seq, json)| serde_json::from_str(&json).ok().map(|e| (seq, e)))
            .collect()
    }

    /// Long-poll: events after `after`, blocking up to `timeout` while there are none. The cursor
    /// returned is the last event's seq, or the log's head when nothing matched (so a filtered
    /// poller skips what it does not want). `after = None` starts at the head.
    pub fn read(
        &self,
        after: Option<u64>,
        kinds: Option<&[String]>,
        timeout: Duration,
    ) -> (u64, Vec<ServerEvent>) {
        let deadline = Instant::now() + timeout;
        let after = after.unwrap_or_else(|| self.cursor());
        let mut seen = after;
        loop {
            // Everything up to `head_before` is covered by this query.
            let head_before = self.cursor();
            let found = self.since(seen, kinds);
            if let Some((last, _)) = found.last() {
                let last = *last;
                return (last, found.into_iter().map(|(_, e)| e).collect());
            }
            seen = seen.max(head_before);
            let guard = self.last.lock().unwrap();
            let head = *guard;
            if head > seen {
                // New events since the query: look at them.
                drop(guard);
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                return (head.max(after), vec![]);
            }
            let (guard, _) = self.cv.wait_timeout(guard, deadline - now).unwrap();
            drop(guard);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_contracts::review::Actor;

    fn op(n: &str) -> ServerEvent {
        ServerEvent::OpChanged {
            op: n.into(),
            changes: vec![],
            actor: Actor::External,
        }
    }

    #[test]
    fn reconnect_with_cursor_replays_missed_events() {
        let log = EventLog::new(Arc::new(ReviewDb::in_memory().unwrap()));
        let a = log.append(op("a"));
        log.append(op("b"));
        log.append(op("c"));
        let missed = log.since(a, None);
        assert_eq!(missed.len(), 2);
        assert!(matches!(&missed[0].1, ServerEvent::OpChanged { op, .. } if op == "b"));
    }

    #[test]
    fn retention_prunes_and_old_cursors_expire() {
        let db = Arc::new(ReviewDb::in_memory().unwrap());
        let r = Retention {
            keep_max: 5,
            keep_min: 2,
            max_age: Duration::from_secs(3600),
            prune_every: 4,
        };
        let log = EventLog::with_retention(db.clone(), r);
        assert!(!log.expired(0), "nothing pruned yet");
        for i in 0..12 {
            log.append(op(&i.to_string()));
        }
        // Pruned at seq 4, 8 and 12: the newest 5 (8..=12) are kept.
        assert_eq!(db.first_seq().unwrap(), Some(8));
        assert!(log.expired(0) && log.expired(6), "events 7.. were dropped");
        assert!(
            !log.expired(7) && !log.expired(12),
            "replay from 7 is complete"
        );
        assert_eq!(log.since(7, None).len(), 5);
        // A restart keeps the cursor monotonic and the expiry.
        let log2 = EventLog::with_retention(db, r);
        assert_eq!(log2.cursor(), 12);
        assert!(log2.expired(3) && !log2.expired(9));
        assert_eq!(log2.append(op("x")), 13);
        // Everything pruned: every earlier cursor expired, the head does not.
        log2.db
            .prune_events(0, 0, "9999-01-01T00:00:00.000Z")
            .unwrap();
        log2.prune();
        assert!(log2.expired(12) && !log2.expired(13));
    }

    #[test]
    fn long_poll_wakes_on_append_and_filters_kinds() {
        let log = Arc::new(EventLog::new(Arc::new(ReviewDb::in_memory().unwrap())));
        let start = log.cursor();
        let l2 = log.clone();
        // The bound only guards against a hang: the reader returns on the matching append, whether
        // it is already parked by then (the wake path) or not (the replay path).
        let h = std::thread::spawn(move || {
            l2.read(
                Some(start),
                Some(&["review.submitted".to_string()]),
                Duration::from_secs(60),
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        log.append(op("x"));
        let seq = log.append(ServerEvent::ReviewSubmitted {
            threads: vec!["t1".into()],
            by: Actor::human("you"),
            note: None,
        });
        let (cursor, events) = h.join().unwrap();
        assert_eq!(cursor, seq);
        assert_eq!(events.len(), 1);
        let t = Instant::now();
        let (c, e) = log.read(Some(seq), None, Duration::from_millis(100));
        assert!(e.is_empty() && c == seq && t.elapsed() >= Duration::from_millis(90));
    }
}
