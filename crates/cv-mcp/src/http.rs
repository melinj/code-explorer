//! `HttpBackend`: the MCP tools on top of a running cv server's /api/v1 (cv serve or cv-mock).
//!
//! Writes carry the agent's identity in headers so the server attributes them to
//! `{ kind: "agent", name, client }`:
//!   X-CV-Agent: claude · X-CV-Agent-Client: claude-code · X-CV-Workspace: gym
//! Routes the server does not serve come back as [`BackendError::Unsupported`]; `events` then falls
//! back to the WebSocket (/api/v1/ws?cursor=N) and `attention` is dropped after one warning.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use crate::backend::{AgentId, BResult, BackendError, McpBackend, ThreadQuery};

pub struct HttpBackend {
    base: String,
    token: Option<String>,
    client: reqwest::Client,
    /// GET /events answered 404 once: use the WebSocket from then on.
    no_events_route: AtomicBool,
    /// POST /agent/attention answered 404 once: stop posting.
    no_attention_route: AtomicBool,
}

const TILE_BASE_WU: f64 = 4096.0;

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A repo path in a URL: segments encoded, slashes kept.
fn enc_path(p: &str) -> String {
    p.split('/').map(enc).collect::<Vec<_>>().join("/")
}

impl HttpBackend {
    /// `server`: e.g. "http://127.0.0.1:7878" (with or without a trailing /api/v1).
    pub fn new(server: &str, token: Option<String>) -> Self {
        let base = server
            .trim_end_matches('/')
            .trim_end_matches("/api/v1")
            .to_string();
        HttpBackend {
            base,
            token,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("http client"),
            no_events_route: AtomicBool::new(false),
            no_attention_route: AtomicBool::new(false),
        }
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        who: Option<&AgentId>,
    ) -> BResult<Value> {
        let mut url = format!("{}/api/v1{}", self.base, path);
        if !query.is_empty() {
            let qs: Vec<String> = query
                .iter()
                .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
                .collect();
            url.push('?');
            url.push_str(&qs.join("&"));
        }
        let mut rb = self.client.request(method.clone(), &url);
        if let Some(t) = &self.token {
            rb = rb.bearer_auth(t);
        }
        if let Some(w) = who {
            rb = rb.header("X-CV-Agent", &w.name);
            if let Some(c) = &w.client {
                rb = rb.header("X-CV-Agent-Client", c);
            }
            if let Some(ws) = &w.workspace {
                rb = rb.header("X-CV-Workspace", ws);
            }
        }
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let t0 = std::time::Instant::now();
        let res = rb
            .send()
            .await
            .map_err(|e| self.send_error(&method, &url, &e))?;
        let status = res.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(self.token_error());
        }
        let text = res.text().await.unwrap_or_default();
        if std::env::var_os("CV_MCP_DEBUG").is_some() {
            eprintln!(
                "cv-mcp: {method} {path} → {status} {} bytes in {:?}",
                text.len(),
                t0.elapsed()
            );
        }
        let parsed: Option<Value> = serde_json::from_str(&text).ok();
        if status.is_success() {
            return Ok(parsed.unwrap_or(Value::Null));
        }
        let route = format!("{method} /api/v1{}", path);
        // A route the router does not know answers with an empty (non-JSON) 404/405.
        if (status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED)
            && parsed.is_none()
        {
            return Err(BackendError::Unsupported(route));
        }
        let err = parsed
            .as_ref()
            .map(|p| p.get("error").unwrap_or(p).clone())
            .unwrap_or(Value::Null);
        Err(BackendError::api(
            status.as_u16(),
            err["code"].as_str().unwrap_or("http-error"),
            err["message"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| format!("{route}: {status} {text}")),
        ))
    }

    /// The port of the server URL, for the hints below.
    fn port(&self) -> String {
        self.base
            .rsplit(':')
            .next()
            .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or("7878")
            .to_string()
    }

    /// A transport failure as an actionable message: is the server running at all?
    fn send_error(&self, method: &Method, url: &str, e: &reqwest::Error) -> BackendError {
        if e.is_connect() {
            return BackendError::api(
                0,
                "unreachable",
                format!(
                    "cv server not reachable at {} — is `cv serve <repo> --port {}` running? ({e})",
                    self.base,
                    self.port()
                ),
            );
        }
        if e.is_timeout() {
            return BackendError::api(
                0,
                "timeout",
                format!("{method} {url}: no answer from the cv server in time ({e})"),
            );
        }
        BackendError::api(0, "unreachable", format!("{method} {url}: {e}"))
    }

    fn token_error(&self) -> BackendError {
        let what = if self.token.is_some() {
            "token rejected"
        } else {
            "no token given"
        };
        BackendError::api(
            401,
            "unauthorized",
            format!(
                "{what} by the cv server at {} — use the token `cv serve` prints (also in \
                 ~/.local/share/code-viewer/token): --token T or CV_TOKEN",
                self.base
            ),
        )
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> BResult<Value> {
        self.call(Method::GET, path, query, None, None).await
    }

    async fn post(&self, path: &str, body: Value, who: Option<&AgentId>) -> BResult<Value> {
        self.call(Method::POST, path, &[], Some(body), who).await
    }

    fn array(v: Value) -> Vec<Value> {
        match v {
            Value::Array(a) => a,
            _ => Vec::new(),
        }
    }

    /// Long-poll over the WebSocket: replay after `cursor`, then live frames until a matching event
    /// arrives (plus a short grace for its siblings) or the timeout passes.
    async fn events_ws(
        &self,
        cursor: Option<&str>,
        kinds: &[String],
        timeout: Duration,
    ) -> BResult<Value> {
        let ws_base = self
            .base
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        let mut url = format!("{ws_base}/api/v1/ws");
        if let Some(c) = cursor {
            url.push_str(&format!("?cursor={}", enc(c)));
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .map_err(|e| match &e {
                tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == 401 => {
                    self.token_error()
                }
                _ => BackendError::api(
                    0,
                    "unreachable",
                    format!(
                        "cv server not reachable at {} — is `cv serve <repo> --port {}` running? ({e})",
                        self.base,
                        self.port()
                    ),
                ),
            })?;
        let mut last: Option<u64> = cursor.and_then(|c| c.parse().ok());
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        let mut grace: Option<tokio::time::Instant> = None;
        loop {
            let until = grace.unwrap_or(deadline).min(deadline);
            let frame = match tokio::time::timeout_at(until, ws.next()).await {
                Err(_) => break,
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    return Err(BackendError::api(0, "ws-error", e.to_string()));
                }
                Ok(Some(Ok(m))) => m,
            };
            let Ok(text) = frame.into_text() else {
                continue;
            };
            let Ok(f) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            let ev = &f["event"];
            if ev["type"] == "hello" {
                if last.is_none() {
                    last = ev["cursor"].as_str().and_then(|c| c.parse().ok());
                }
                continue;
            }
            let seq = f["seq"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| f["seq"].as_u64());
            if let (Some(s), Some(l)) = (seq, last)
                && s <= l
            {
                continue;
            }
            if let Some(s) = seq {
                last = Some(s);
            }
            let ty = ev["type"].as_str().unwrap_or("");
            if kinds.is_empty() || kinds.iter().any(|k| k == ty) {
                events.push(ev.clone());
                grace.get_or_insert(tokio::time::Instant::now() + Duration::from_millis(300));
            }
        }
        let _ = ws.close(None).await;
        Ok(json!({ "cursor": last.unwrap_or(0).to_string(), "events": events }))
    }
}

#[async_trait]
impl McpBackend for HttpBackend {
    async fn workspaces(&self) -> BResult<Vec<Value>> {
        Ok(Self::array(self.get("/workspaces", &[]).await?))
    }

    async fn bind_workspace(&self, who: &AgentId, workspace: &str) -> BResult<Value> {
        self.post(
            "/agent/bind",
            json!({ "agent": who.name, "client": who.client, "workspace": workspace }),
            Some(who),
        )
        .await
    }

    async fn attention(&self, event: Value) -> BResult<()> {
        if self.no_attention_route.load(Ordering::Relaxed) {
            return Ok(());
        }
        match self.post("/agent/attention", event, None).await {
            Ok(_) => Ok(()),
            Err(BackendError::Unsupported(r)) => {
                self.no_attention_route.store(true, Ordering::Relaxed);
                eprintln!("cv-mcp: {r} is not served; agent.attention is not delivered");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    async fn view_state(&self) -> BResult<Value> {
        self.get("/agent/view-state", &[]).await
    }

    async fn set_view(&self, who: &AgentId, spec: Value) -> BResult<Value> {
        self.post("/agent/view", json!({ "spec": spec }), Some(who))
            .await
    }

    async fn focus(&self, who: &AgentId, body: Value) -> BResult<Value> {
        self.post("/agent/focus", body, Some(who)).await
    }

    async fn create_tour(&self, who: &AgentId, body: Value) -> BResult<Value> {
        self.post("/agent/tours", body, Some(who)).await
    }

    async fn create_view(&self, spec: Value) -> BResult<Value> {
        self.post("/views", spec, None).await
    }

    async fn view_windows(&self, view: &str) -> BResult<Vec<Value>> {
        let boot = self
            .get(&format!("/views/{}/bootstrap", enc(view)), &[])
            .await?;
        let lv = boot["view"]["layoutVersion"].as_u64().unwrap_or(0);
        let w = &boot["world"];
        let (x, y) = (
            w["x"].as_f64().unwrap_or(0.0),
            w["y"].as_f64().unwrap_or(0.0),
        );
        let (ww, wh) = (
            w["w"].as_f64().unwrap_or(0.0),
            w["h"].as_f64().unwrap_or(0.0),
        );
        let mut z = 0u32;
        while z < 12 && ((1u64 << z) as f64) * TILE_BASE_WU < ww.max(wh) {
            z += 1;
        }
        let span = ((1u64 << z) as f64) * TILE_BASE_WU;
        let (tx0, tx1) = ((x / span).floor() as i64, ((x + ww) / span).floor() as i64);
        let (ty0, ty1) = ((y / span).floor() as i64, ((y + wh) / span).floor() as i64);
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for tx in tx0..=tx1 {
            for ty in ty0..=ty1 {
                let t = self
                    .get(&format!("/views/{}/tiles/{z}/{tx}/{ty}", enc(view)), &[])
                    .await?;
                for h in Self::array(t["windows"].clone()) {
                    if let Some(id) = h["id"].as_str()
                        && seen.insert(id.to_string())
                    {
                        out.push(h);
                    }
                }
            }
        }
        let _ = lv;
        Ok(out)
    }

    async fn view_hubs(&self, view: &str) -> BResult<Vec<Value>> {
        let boot = self
            .get(&format!("/views/{}/bootstrap", enc(view)), &[])
            .await?;
        Ok(Self::array(boot["hubs"].clone()))
    }

    async fn window_batch(&self, view: &str, ids: &[String], parts: &[&str]) -> BResult<Value> {
        let mut merged = json!({ "outlines": [], "diffs": [], "links": [], "missing": [] });
        for chunk in ids.chunks(200) {
            let r = self
                .post(
                    &format!("/views/{}/windows:batch", enc(view)),
                    json!({ "ids": chunk, "parts": parts, "layoutVersion": 0 }),
                    None,
                )
                .await?;
            for k in ["outlines", "diffs", "links", "missing"] {
                if let (Some(dst), Some(src)) = (merged[k].as_array_mut(), r[k].as_array()) {
                    dst.extend(src.iter().cloned());
                }
            }
        }
        Ok(merged)
    }

    async fn blob_text(&self, blob: &str, from: Option<u32>, to: Option<u32>) -> BResult<Value> {
        let mut q = Vec::new();
        if let Some(f) = from {
            q.push(("from", f.to_string()));
        }
        if let Some(t) = to {
            q.push(("to", t.to_string()));
        }
        self.get(&format!("/blobs/{}/text", enc(blob)), &q).await
    }

    async fn metrics(
        &self,
        view: &str,
        level: &str,
        lane: Option<&str>,
        limit: usize,
    ) -> BResult<Value> {
        let mut q = vec![("level", level.to_string()), ("limit", limit.to_string())];
        if let Some(l) = lane {
            q.push(("lane", l.to_string()));
        }
        self.get(&format!("/views/{}/metrics", enc(view)), &q).await
    }

    async fn search(
        &self,
        q: &str,
        kinds: Option<&str>,
        view: Option<&str>,
        limit: usize,
    ) -> BResult<Vec<Value>> {
        let mut qs = vec![("q", q.to_string()), ("limit", limit.to_string())];
        if let Some(k) = kinds {
            qs.push(("kinds", k.to_string()));
        }
        if let Some(v) = view {
            qs.push(("viewId", v.to_string()));
        }
        Ok(Self::array(self.get("/search", &qs).await?))
    }

    async fn links(
        &self,
        view: &str,
        target: &str,
        dir: &str,
        kinds: Option<&str>,
        depth: u8,
    ) -> BResult<Value> {
        let mut q = vec![
            ("viewId", view.to_string()),
            ("target", target.to_string()),
            ("dir", dir.to_string()),
            ("depth", depth.to_string()),
        ];
        if let Some(k) = kinds {
            q.push(("kinds", k.to_string()));
        }
        self.get("/links", &q).await
    }

    async fn hub(&self, view: &str, hub: &str) -> BResult<Value> {
        self.get(
            &format!("/hubs/{}", enc(hub)),
            &[("viewId", view.to_string())],
        )
        .await
    }

    async fn revisions(&self, revset: Option<&str>, limit: Option<usize>) -> BResult<Value> {
        let mut q = Vec::new();
        if let Some(r) = revset {
            q.push(("revset", r.to_string()));
        }
        if let Some(l) = limit {
            q.push(("limit", l.to_string()));
        }
        self.get("/revisions", &q).await
    }

    async fn interdiff(&self, change: &str, from: &str) -> BResult<Value> {
        self.get(
            "/interdiff",
            &[("change", change.to_string()), ("from", from.to_string())],
        )
        .await
    }

    async fn tree_diff(&self, from: &str, to: &str, fileset: Option<&str>) -> BResult<Value> {
        let mut q = vec![("from", from.to_string()), ("to", to.to_string())];
        if let Some(f) = fileset {
            q.push(("fileset", f.to_string()));
        }
        self.get("/diff", &q).await
    }

    async fn threads(&self, f: &ThreadQuery) -> BResult<Vec<Value>> {
        let mut q = Vec::new();
        if !f.status.is_empty() {
            q.push(("status", f.status.join(",")));
        }
        if let Some(a) = &f.assignee {
            q.push(("assignee", a.clone()));
        }
        if let Some(r) = &f.revset {
            q.push(("revset", r.clone()));
        }
        if let Some(p) = &f.path {
            q.push(("path", p.clone()));
        }
        Ok(Self::array(self.get("/threads", &q).await?))
    }

    async fn create_thread(&self, who: &AgentId, body: Value) -> BResult<Value> {
        self.post("/threads", body, Some(who)).await
    }

    async fn comment(
        &self,
        who: &AgentId,
        thread: &str,
        body: &str,
        set_status: Option<&str>,
    ) -> BResult<Value> {
        let mut b = json!({ "body": body });
        if let Some(s) = set_status {
            b["setStatus"] = json!(s);
        }
        self.post(&format!("/threads/{}/comments", enc(thread)), b, Some(who))
            .await
    }

    async fn patch_thread(&self, who: &AgentId, thread: &str, patch: Value) -> BResult<Value> {
        self.call(
            Method::PATCH,
            &format!("/threads/{}", enc(thread)),
            &[],
            Some(patch),
            Some(who),
        )
        .await
    }

    async fn declare_link(&self, who: &AgentId, body: Value) -> BResult<Value> {
        self.post("/links", body, Some(who)).await
    }

    async fn rate_link(&self, who: &AgentId, link: &str, body: Value) -> BResult<Value> {
        self.post(&format!("/links/{}/rate", enc(link)), body, Some(who))
            .await
    }

    async fn label_cluster(&self, who: &AgentId, cluster: &str, body: Value) -> BResult<Value> {
        self.call(
            Method::PUT,
            &format!("/clusters/{}/label", enc(cluster)),
            &[],
            Some(body),
            Some(who),
        )
        .await
    }

    async fn edit(&self, who: &AgentId, req: Value) -> BResult<Value> {
        self.post("/edits", req, Some(who)).await
    }

    async fn apply_edit(&self, who: &AgentId, edit: &str) -> BResult<Value> {
        self.post(&format!("/edits/{}/apply", enc(edit)), json!({}), Some(who))
            .await
    }

    async fn undo_edit(&self, who: &AgentId, edit: &str) -> BResult<Value> {
        self.post(&format!("/edits/{}/undo", enc(edit)), json!({}), Some(who))
            .await
    }

    async fn conflicts(&self, revset: Option<&str>) -> BResult<Vec<Value>> {
        let q: Vec<(&str, String)> = revset
            .map(|r| vec![("revset", r.to_string())])
            .unwrap_or_default();
        Ok(Self::array(self.get("/conflicts", &q).await?))
    }

    async fn conflict(&self, commit: &str, path: &str) -> BResult<Value> {
        self.get(
            &format!("/conflicts/{}/{}", enc(commit), enc_path(path)),
            &[],
        )
        .await
    }

    async fn resolve_conflict(
        &self,
        who: &AgentId,
        commit: &str,
        path: &str,
        content: &str,
    ) -> BResult<Value> {
        self.post(
            &format!("/conflicts/{}/{}/resolve", enc(commit), enc_path(path)),
            json!({ "content": content }),
            Some(who),
        )
        .await
    }

    async fn events(
        &self,
        cursor: Option<&str>,
        kinds: &[String],
        timeout: Duration,
    ) -> BResult<Value> {
        if !self.no_events_route.load(Ordering::Relaxed) {
            let mut q = vec![("timeoutS", timeout.as_secs().max(1).to_string())];
            if let Some(c) = cursor {
                q.push(("cursor", c.to_string()));
            }
            if !kinds.is_empty() {
                q.push(("kinds", kinds.join(",")));
            }
            match self.get("/events", &q).await {
                Err(BackendError::Unsupported(_)) => {
                    self.no_events_route.store(true, Ordering::Relaxed);
                }
                r => return r,
            }
        }
        self.events_ws(cursor, kinds, timeout).await
    }

    async fn cursor(&self) -> BResult<Option<String>> {
        if self.no_events_route.load(Ordering::Relaxed) {
            return Ok(None);
        }
        // GET /events without a cursor and without waiting: the head of the log.
        Ok(self
            .get("/events", &[("timeoutS", "0".to_string())])
            .await
            .ok()
            .and_then(|v| v["cursor"].as_str().map(String::from)))
    }
}
