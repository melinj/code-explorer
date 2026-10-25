//! /mcp over Streamable HTTP (what `cv serve` mounts): raw JSON-RPC as an HTTP MCP client sends it.

use std::sync::Arc;

use cv_mcp::fake::FakeBackend;
use cv_mcp::{McpConfig, Profile};
use serde_json::{Value, json};

/// The JSON-RPC response in a JSON or SSE body.
fn rpc_body(text: &str) -> Value {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return v;
    }
    text.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .find(|v| v.get("id").is_some())
        .unwrap_or_else(|| panic!("no JSON-RPC response in {text:?}"))
}

struct Session {
    http: reqwest::Client,
    url: String,
    session: Option<String>,
    headers: Vec<(&'static str, &'static str)>,
}

impl Session {
    async fn post(&mut self, body: Value) -> (reqwest::StatusCode, String) {
        let mut rb = self
            .http
            .post(&self.url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2025-06-18")
            .body(body.to_string());
        if let Some(s) = &self.session {
            rb = rb.header("mcp-session-id", s);
        }
        for (k, v) in &self.headers {
            rb = rb.header(*k, *v);
        }
        let res = rb.send().await.unwrap();
        if let Some(s) = res.headers().get("mcp-session-id") {
            self.session = Some(s.to_str().unwrap().to_string());
        }
        (res.status(), res.text().await.unwrap())
    }

    async fn rpc(&mut self, id: u64, method: &str, params: Value) -> Value {
        let (status, text) = self
            .post(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        assert!(status.is_success(), "{method}: {status} {text}");
        rpc_body(&text)
    }
}

async fn open(url: String) -> Session {
    open_with(url, Vec::new()).await
}

async fn open_with(url: String, headers: Vec<(&'static str, &'static str)>) -> Session {
    let mut s = Session {
        http: reqwest::Client::new(),
        url,
        session: None,
        headers,
    };
    let init = s
        .rpc(
            1,
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": { "name": "opencode", "version": "1" } }),
        )
        .await;
    assert_eq!(init["result"]["serverInfo"]["name"], "cv");
    s.post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;
    s
}

#[tokio::test]
async fn mcp_route_serves_profiles_and_binds_by_agent() {
    let fake = Arc::new(FakeBackend::sample());
    let app = cv_mcp::router(
        fake.clone(),
        McpConfig {
            profile: Profile::Editor,
            ..Default::default()
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut e = open(format!("http://{addr}/mcp")).await;
    let tools = e.rpc(2, "tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 15);
    let mut s = open(format!("http://{addr}/mcp/full")).await;
    let tools = s.rpc(2, "tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 34);
    let r = s
        .rpc(
            3,
            "tools/call",
            json!({ "name": "bind_workspace", "arguments": { "workspace": "gym" } }),
        )
        .await;
    assert_eq!(r["result"]["isError"], json!(false), "{r}");
    // The agent name comes from the MCP client ("opencode") and the binding is shared state.
    let att = fake
        .events_log()
        .into_iter()
        .find(|e| e["type"] == "agent.attention")
        .unwrap();
    assert_eq!(att["agent"], "opencode");
    assert_eq!(att["workspace"], "gym");
    assert!(
        fake.events_log()
            .iter()
            .any(|e| e["type"] == "workspace.updated"
                && e["workspace"]["agent"]["name"] == "opencode")
    );

    // A second session of the same agent on the reader endpoint sees the binding and fewer tools.
    let mut r2 = open(format!("http://{addr}/mcp/reader")).await;
    let tools = r2.rpc(2, "tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 16);
    let ws = r2
        .rpc(
            3,
            "tools/call",
            json!({ "name": "list_workspaces", "arguments": {} }),
        )
        .await;
    let text = ws["result"]["content"][0]["text"].as_str().unwrap();
    let ws: Value = serde_json::from_str(text).unwrap();
    let gym = ws
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "gym")
        .unwrap();
    assert_eq!(gym["boundToThisSession"], true);
    let denied = r2
        .rpc(
            4,
            "tools/call",
            json!({ "name": "edit_code", "arguments": {} }),
        )
        .await;
    assert!(
        denied
            .to_string()
            .contains("not available in the reader profile"),
        "{denied}"
    );
}

#[tokio::test]
async fn http_headers_name_the_agent_and_its_workspace() {
    let fake = Arc::new(FakeBackend::sample());
    let app = cv_mcp::router(fake.clone(), McpConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut s = open_with(
        format!("http://{addr}/mcp/editor"),
        vec![("x-cv-agent", "otto"), ("x-cv-workspace", "gym")],
    )
    .await;
    let r = s
        .rpc(
            2,
            "tools/call",
            json!({ "name": "read_code", "arguments": { "path": "src/bus.py" } }),
        )
        .await;
    assert_eq!(r["result"]["isError"], json!(false), "{r}");
    let att = fake
        .events_log()
        .into_iter()
        .find(|e| e["type"] == "agent.attention")
        .unwrap();
    assert_eq!(att["agent"], "otto");
    assert_eq!(att["workspace"], "gym");
}

#[tokio::test]
async fn connection_errors_say_what_to_do() {
    use cv_mcp::McpBackend;
    // Nothing listens on this port.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let b = cv_mcp::http::HttpBackend::new(&format!("http://127.0.0.1:{port}"), None);
    let e = b.workspaces().await.unwrap_err().to_string();
    assert!(
        e.contains("not reachable") && e.contains(&format!("cv serve <repo> --port {port}")),
        "{e}"
    );
    // A server that rejects the token.
    let app = axum::Router::new().fallback(|| async {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": { "code": "unauthorized", "message": "missing or wrong bearer token" } })),
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let b = cv_mcp::http::HttpBackend::new(&format!("http://{addr}"), Some("nope".into()));
    let e = b.workspaces().await.unwrap_err().to_string();
    assert!(
        e.contains("token rejected") && e.contains(".local/share/code-viewer"),
        "{e}"
    );
}

/// N10 over Streamable HTTP: resources/subscribe, then notifications/resources/updated on the
/// session's GET stream when threads, conflicts or the view state change; unsubscribe stops them.
#[tokio::test]
async fn resource_subscriptions_notify_over_streamable_http() {
    use std::time::Duration;
    let fake = Arc::new(FakeBackend::sample());
    let app = cv_mcp::router(fake.clone(), McpConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}/mcp");
    let mut s = open(url.clone()).await;

    // The standalone SSE stream that carries server notifications of this session.
    let res = s
        .http
        .get(&url)
        .header("accept", "text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .header("mcp-session-id", s.session.clone().unwrap())
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "GET stream: {}", res.status());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let mut res = res;
        let mut buf = String::new();
        while let Ok(Some(chunk)) = res.chunk().await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(i) = buf.find('\n') {
                let line = buf[..i].trim().to_string();
                buf.drain(..=i);
                if let Some(d) = line.strip_prefix("data:")
                    && let Ok(v) = serde_json::from_str::<Value>(d.trim())
                {
                    let _ = tx.send(v);
                }
            }
        }
    });
    // Waits for an update of `uri`; None after `secs`.
    async fn updated(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
        uri: &str,
        secs: u64,
    ) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        loop {
            let v = tokio::time::timeout_at(deadline, rx.recv()).await.ok()??;
            if v["method"] == "notifications/resources/updated" && v["params"]["uri"] == uri {
                return Some(v);
            }
        }
    }

    let unknown = s
        .rpc(2, "resources/subscribe", json!({ "uri": "cv://nope" }))
        .await;
    assert!(unknown["error"].is_object(), "{unknown}");
    for (id, uri) in [
        (3, "cv://threads?status=open"),
        (4, "cv://conflicts"),
        (5, "cv://view/state"),
    ] {
        let r = s
            .rpc(id, "resources/subscribe", json!({ "uri": uri }))
            .await;
        assert!(r["result"].is_object(), "{uri}: {r}");
    }
    // No settling wait: the watcher follows the log from the cursor read before each subscribe
    // was acknowledged, so the next event is never missed.
    fake.human_thread("src/bus.py", 1, 1, "Name?", "question", None);
    assert!(
        updated(&mut rx, "cv://threads?status=open", 10)
            .await
            .is_some(),
        "no update for open threads"
    );
    fake.add_conflict("src/world.py", 7, 7, "a", "b");
    assert!(
        updated(&mut rx, "cv://conflicts", 10).await.is_some(),
        "no update for conflicts"
    );
    fake.set_view_state(json!({ "viewId": "v-snapshot", "focus": "w:src/bus.py",
        "viewport": { "rect": [0, 0, 1, 1], "zoom": 1 }, "lens": "none", "followAgent": false }));
    assert!(
        updated(&mut rx, "cv://view/state", 10).await.is_some(),
        "no update for the view state"
    );

    let r = s
        .rpc(
            6,
            "resources/unsubscribe",
            json!({ "uri": "cv://threads?status=open" }),
        )
        .await;
    assert!(r["result"].is_object(), "{r}");
    fake.human_thread("src/bus.py", 2, 2, "Again?", "question", None);
    assert!(
        updated(&mut rx, "cv://threads?status=open", 3)
            .await
            .is_none(),
        "update after unsubscribe"
    );
}
