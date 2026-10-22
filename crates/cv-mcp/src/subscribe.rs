//! N10: resource subscriptions (mcp.ts: resources are "subscribable where the client supports it").
//!
//! Two client styles reach the same registry:
//! - `resources/subscribe` / `resources/unsubscribe` (protocol ≤ 2025-11-25): per session, the
//!   update goes to the session's peer (over Streamable HTTP: its standalone GET stream);
//! - `subscriptions/listen` (2026-07-28): one request per subscription, updates go to its sink.
//!
//! One watcher task per server long-polls the backend's event log while anyone is subscribed and
//! sends `notifications/resources/updated`:
//! - `cv://threads?status=open` on thread.upserted and review.submitted;
//! - `cv://conflicts` on conflicts.changed and op.changed;
//! - `cv://view/state` on view.requested and view.invalidated, and whenever the view state the UI
//!   reports (WS `view.state`, which is not a logged event) differs from the last one seen: the
//!   watcher compares GET /agent/view-state about once a second while that URI is subscribed.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::RoleServer;
use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::service::{Peer, SubscriptionSink};
use serde_json::Value;

use crate::backend::{BackendError, McpBackend};
use crate::server::{RESOURCE_CONFLICTS, RESOURCE_OPEN_THREADS, RESOURCE_VIEW};

/// Every subscribable resource.
pub const SUBSCRIBABLE: &[&str] = &[RESOURCE_VIEW, RESOURCE_OPEN_THREADS, RESOURCE_CONFLICTS];

/// Event kinds the watcher reads.
const WATCHED: &[&str] = &[
    "thread.upserted",
    "review.submitted",
    "conflicts.changed",
    "op.changed",
    "view.requested",
    "view.invalidated",
];

/// The resources an event changes.
pub fn uris_for(event: &Value) -> &'static [&'static str] {
    match event["type"].as_str().unwrap_or("") {
        "thread.upserted" | "review.submitted" => &[RESOURCE_OPEN_THREADS],
        "conflicts.changed" | "op.changed" => &[RESOURCE_CONFLICTS],
        "view.requested" | "view.invalidated" => &[RESOURCE_VIEW],
        _ => &[],
    }
}

#[derive(Clone)]
enum Sink {
    /// resources/subscribe: the session's peer.
    Peer(Peer<RoleServer>),
    /// subscriptions/listen: the request's sink (it applies the accepted filter itself).
    Listen(SubscriptionSink),
}

impl Sink {
    async fn send(&self, uri: &str) -> bool {
        match self {
            Sink::Peer(p) => p
                .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri))
                .await
                .is_ok(),
            Sink::Listen(s) => s.notify_resource_updated(uri).await.is_ok(),
        }
    }
}

struct Sub {
    uris: HashSet<String>,
    sink: Sink,
}

#[derive(Default)]
struct Registry {
    subs: HashMap<u64, Sub>,
    /// A watcher task is running (cleared by the task itself, under this lock, when it stops).
    watching: bool,
}

/// Subscriptions of every session of one server.
#[derive(Default)]
pub struct Subscriptions {
    next: AtomicU64,
    reg: Mutex<Registry>,
}

impl Subscriptions {
    /// Register a subscriber; starts the watcher when it is not running, following the event log
    /// from `start` (the cursor read before the subscription was acknowledged; a running watcher
    /// is at or before it already).
    fn add(
        self: &Arc<Self>,
        backend: &Arc<dyn McpBackend>,
        uris: HashSet<String>,
        sink: Sink,
        start: Option<String>,
    ) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let mut reg = self.reg.lock().unwrap();
        reg.subs.insert(id, Sub { uris, sink });
        if !reg.watching {
            reg.watching = true;
            tokio::spawn(watch(self.clone(), backend.clone(), start));
        }
        id
    }

    fn remove(&self, id: u64) {
        self.reg.lock().unwrap().subs.remove(&id);
    }

    /// resources/subscribe for the session whose subscription id is in `slot`.
    pub fn subscribe(
        self: &Arc<Self>,
        backend: &Arc<dyn McpBackend>,
        slot: &Mutex<Option<u64>>,
        peer: Peer<RoleServer>,
        uri: &str,
        start: Option<String>,
    ) {
        let mut slot = slot.lock().unwrap();
        if let Some(id) = *slot {
            let mut reg = self.reg.lock().unwrap();
            if let Some(sub) = reg.subs.get_mut(&id) {
                sub.uris.insert(uri.to_string());
                return;
            }
        }
        let uris = HashSet::from([uri.to_string()]);
        *slot = Some(self.add(backend, uris, Sink::Peer(peer), start));
    }

    /// resources/unsubscribe.
    pub fn unsubscribe(&self, slot: &Mutex<Option<u64>>, uri: &str) {
        let mut slot = slot.lock().unwrap();
        let Some(id) = *slot else { return };
        let mut reg = self.reg.lock().unwrap();
        let empty = match reg.subs.get_mut(&id) {
            Some(sub) => {
                sub.uris.remove(uri);
                sub.uris.is_empty()
            }
            None => true,
        };
        if empty {
            reg.subs.remove(&id);
            *slot = None;
        }
    }

    /// subscriptions/listen: registered until the returned guard drops.
    pub fn listen(
        self: &Arc<Self>,
        backend: &Arc<dyn McpBackend>,
        uris: HashSet<String>,
        sink: SubscriptionSink,
        start: Option<String>,
    ) -> ListenGuard {
        ListenGuard {
            subs: self.clone(),
            id: self.add(backend, uris, Sink::Listen(sink), start),
        }
    }

    /// The URIs anyone subscribes to; `None` stops the watcher (under the lock, so a concurrent
    /// `add` either sees `watching` cleared and starts a new one, or is seen here).
    fn wanted_or_stop(&self) -> Option<HashSet<String>> {
        let mut reg = self.reg.lock().unwrap();
        let wanted: HashSet<String> = reg
            .subs
            .values()
            .flat_map(|s| s.uris.iter().cloned())
            .collect();
        if wanted.is_empty() {
            reg.watching = false;
            return None;
        }
        Some(wanted)
    }

    /// Send `notifications/resources/updated` for `uri` to its subscribers; drops subscribers
    /// whose session is gone.
    async fn notify(&self, uri: &str) {
        let targets: Vec<(u64, Sink)> = self
            .reg
            .lock()
            .unwrap()
            .subs
            .iter()
            .filter(|(_, s)| s.uris.contains(uri))
            .map(|(id, s)| (*id, s.sink.clone()))
            .collect();
        for (id, sink) in targets {
            if !sink.send(uri).await {
                self.remove(id);
            }
        }
    }

    /// Subscribers currently registered (tests).
    pub fn len(&self) -> usize {
        self.reg.lock().unwrap().subs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Unregisters a `subscriptions/listen` subscriber when its request ends.
pub struct ListenGuard {
    subs: Arc<Subscriptions>,
    id: u64,
}

impl Drop for ListenGuard {
    fn drop(&mut self) {
        self.subs.remove(self.id);
    }
}

/// How often the view state is compared while `cv://view/state` is subscribed.
const VIEW_POLL: Duration = Duration::from_secs(1);
/// Long-poll timeout otherwise.
const EVENTS_POLL: Duration = Duration::from_secs(10);

async fn view_state(backend: &dyn McpBackend) -> Option<Value> {
    backend.view_state().await.ok()
}

/// The watcher: follows the event log (and the view state) while there are subscribers.
async fn watch(subs: Arc<Subscriptions>, backend: Arc<dyn McpBackend>, start: Option<String>) {
    let kinds: Vec<String> = WATCHED.iter().map(|k| k.to_string()).collect();
    let mut cursor: Option<String> = start;
    let mut last_view: Option<Option<Value>> = None;
    while let Some(wanted) = subs.wanted_or_stop() {
        let view_wanted = wanted.contains(RESOURCE_VIEW);
        if view_wanted && last_view.is_none() {
            last_view = Some(view_state(backend.as_ref()).await);
        }
        let timeout = if view_wanted { VIEW_POLL } else { EVENTS_POLL };
        let mut touched: Vec<&'static str> = Vec::new();
        match backend.events(cursor.as_deref(), &kinds, timeout).await {
            Ok(r) => {
                if let Some(c) = r["cursor"].as_str() {
                    cursor = Some(c.to_string());
                }
                for e in r["events"].as_array().into_iter().flatten() {
                    for uri in uris_for(e) {
                        if !touched.contains(uri) {
                            touched.push(uri);
                        }
                    }
                }
            }
            // No event log (e.g. cv-mock): the view state is still polled.
            Err(BackendError::Unsupported(_)) => tokio::time::sleep(timeout).await,
            Err(e) => {
                eprintln!("cv-mcp: resource watcher: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        if view_wanted {
            let now = view_state(backend.as_ref()).await;
            if last_view.as_ref() != Some(&now) {
                last_view = Some(now);
                if !touched.contains(&RESOURCE_VIEW) {
                    touched.push(RESOURCE_VIEW);
                }
            }
        } else {
            last_view = None;
        }
        for uri in touched {
            if wanted.contains(uri) {
                subs.notify(uri).await;
            }
        }
    }
}
