//! Event log + WebSocket (docs/contracts/ws.ts) and a simulated agent, so the web client's live
//! updates and the agent review loop can be exercised before the real server and MCP agent exist.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use serde_json::{Value, json};
use tokio::sync::broadcast;

pub struct Events {
    /// The latest view.state a client sent (what the human is looking at), for MCP.
    pub view_state: Mutex<Option<Value>>,
    log: Mutex<Vec<(u64, Value)>>,
    tx: broadcast::Sender<(u64, Value)>,
}

impl Events {
    pub fn new() -> Self {
        Events {
            view_state: Mutex::new(None),
            log: Mutex::new(Vec::new()),
            tx: broadcast::channel(1024).0,
        }
    }

    /// Append an event to the log and broadcast it.
    pub fn emit(&self, event: Value) {
        let mut log = self.log.lock().unwrap();
        let seq = log.len() as u64 + 1;
        log.push((seq, event.clone()));
        let _ = self.tx.send((seq, event));
    }

    pub fn cursor(&self) -> u64 {
        self.log.lock().unwrap().len() as u64
    }

    /// GET /events: what happened after `cursor`, waiting up to `timeout` for something new.
    pub async fn poll(
        &self,
        cursor: u64,
        kinds: Option<Vec<String>>,
        timeout: Duration,
    ) -> (u64, Vec<Value>) {
        let wanted = |e: &Value| {
            kinds
                .as_ref()
                .is_none_or(|k| k.iter().any(|k| e["type"] == k.as_str()))
        };
        let mut rx = self.tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let found: Vec<(u64, Value)> = self
                .since(cursor)
                .into_iter()
                .filter(|(_, e)| wanted(e))
                .collect();
            if !found.is_empty() {
                let last = found.last().unwrap().0;
                return (last, found.into_iter().map(|(_, e)| e).collect());
            }
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                _ => return (self.cursor().max(cursor), Vec::new()),
            }
        }
    }

    fn since(&self, cursor: u64) -> Vec<(u64, Value)> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| *s > cursor)
            .cloned()
            .collect()
    }

    /// Serve one client: hello, replay from its cursor, then live frames.
    pub async fn serve(self: Arc<Self>, mut ws: WebSocket, cursor: Option<u64>) {
        let mut rx = self.tx.subscribe();
        let hello = json!({ "seq": self.cursor().to_string(), "event": {
            "type": "hello", "session": "mock", "op": "4eb0965a", "serverVersion": "cv-mock", "cursor": self.cursor().to_string()
        }});
        if ws
            .send(Message::Text(hello.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        if let Some(c) = cursor {
            for (seq, ev) in self.since(c) {
                let frame = json!({ "seq": seq.to_string(), "event": ev });
                if ws
                    .send(Message::Text(frame.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Ok((seq, ev)) => {
                        let frame = json!({ "seq": seq.to_string(), "event": ev });
                        if ws.send(Message::Text(frame.to_string().into())).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                },
                incoming = ws.recv() => match incoming {
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(m) = serde_json::from_str::<Value>(&t)
                            && m["type"] == "view.state"
                        {
                            *self.view_state.lock().unwrap() = Some(m["state"].clone());
                        }
                    }
                    Some(Ok(_)) => continue,
                    _ => return,
                },
            }
        }
    }
}

/// The pause between steps, so the human can watch the agent work.
pub const AGENT_STEP: Duration = Duration::from_millis(1500);

/// A plausible small refactor of `line` (rename a local, or tidy a comment).
pub fn refactor(line: &str) -> String {
    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let body = line.trim();
    if let Some((lhs, rhs)) = body.split_once(" = ")
        && !lhs.contains(' ')
        && !lhs.contains('.')
    {
        return format!("{indent}{lhs}_value = {rhs}");
    }
    if body.starts_with('#') {
        return format!("{indent}# NOTE: {}", body.trim_start_matches('#').trim());
    }
    format!("{indent}{body}  # reviewed")
}
