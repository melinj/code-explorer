//! End-to-end B9 acceptance against a running cv server (cv-mock or cv serve) with a real MCP client
//! talking to the `cv-mcp stdio` shim as a child process:
//!   bind_workspace → wait_for_events → get_thread → apply_suggestion (amend) → reply
//! while a WebSocket "UI" records what the browser would receive.
//!
//! ```text
//! cargo run --release -p cv-mock -- --port 7896 &
//! cargo build --release -p cv-mcp
//! cargo run --release -p cv-mcp --example e2e_mock -- --server http://127.0.0.1:7896 \
//!     --shim target/release/cv-mcp --path src/dialogue/edge/trade_4200.py --line 11
//! ```

use std::time::Duration;

use futures::StreamExt;
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ClientConfig>;

fn arg(name: &str, default: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| default.to_string())
}

async fn call(c: &Client, name: &str, args: Value) -> (Value, String) {
    let t0 = std::time::Instant::now();
    let r = c
        .call_tool(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().cloned().unwrap_or_default()),
        )
        .await
        .expect("call_tool");
    let texts: Vec<String> = r
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect();
    if r.is_error == Some(true) {
        panic!("{name} failed: {}", texts.join("\n"));
    }
    let first = texts
        .first()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or(Value::Null);
    eprintln!("  ✓ {name} ({:?})", t0.elapsed());
    (first, texts.join("\n"))
}

#[tokio::main]
async fn main() {
    let server = arg("--server", "http://127.0.0.1:7896");
    let shim = arg("--shim", "target/release/cv-mcp");
    let path = arg("--path", "src/dialogue/edge/trade_4200.py");
    let line: u32 = arg("--line", "11").parse().unwrap();
    let api = format!("{server}/api/v1");
    let http = reqwest::Client::new();

    let mut child = tokio::process::Command::new(&shim)
        .args(["stdio", "--server", &server, "--profile", "editor"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn shim");
    let transport = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let c = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("claude-code", "e2e"),
    )
    .serve(transport)
    .await
    .expect("MCP initialize");
    let tools = c.list_all_tools().await.unwrap();
    eprintln!("connected: {} tools (editor profile)", tools.len());

    // Warm the window index so the loop below is quick.
    let (code, _) = call(
        &c,
        "read_code",
        json!({ "path": path, "start": line, "end": line }),
    )
    .await;
    let before = code.clone();

    let (b, _) = call(&c, "bind_workspace", json!({ "workspace": "gym" })).await;
    assert_eq!(b["bound"], "gym");
    let (w, _) = call(&c, "wait_for_events", json!({ "timeoutS": 1 })).await;
    let cursor = w["cursor"].as_str().unwrap().to_string();

    // The UI: a WebSocket from the same cursor.
    let ws_url = format!("{}/ws?cursor={cursor}", api.replacen("http://", "ws://", 1));
    let (mut ui, _) = tokio_tungstenite::connect_async(ws_url.as_str())
        .await
        .expect("ui ws");
    let ui_task = tokio::spawn(async move {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
        while let Ok(Some(Ok(m))) = tokio::time::timeout_at(deadline, ui.next()).await {
            if let Ok(t) = m.into_text()
                && let Ok(f) = serde_json::from_str::<Value>(t.as_str())
            {
                seen.push(f["event"].clone());
            }
        }
        seen
    });

    // The human: a suggestion assigned to the agent.
    let new_line = "    payload = DEFAULT_PAYLOAD  # was a magic 490";
    let t: Value = http
        .post(format!("{api}/threads"))
        .json(&json!({
            "anchor": { "path": path, "rev": before["change"], "start": line, "end": line },
            "body": format!("Magic number: name it.\n```suggestion\n{new_line}\n```"),
            "kind": "suggestion", "assignee": "agent"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = t["id"].as_str().unwrap().to_string();
    eprintln!("human created thread {id} on {path}:{line}");

    let (evs, _) = call(
        &c,
        "wait_for_events",
        json!({ "cursor": cursor, "kinds": ["thread.upserted"], "timeoutS": 10 }),
    )
    .await;
    let got = evs["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["thread"]["id"] == id.as_str())
        .expect("thread.upserted for the new thread");
    assert_eq!(got["thread"]["assignee"], "agent");

    let (th, text) = call(&c, "get_thread", json!({ "thread": id })).await;
    assert_eq!(th["suggestion"]["text"], new_line);
    eprintln!(
        "{}",
        text.lines()
            .skip_while(|l| !l.contains('│'))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let (r, _) = call(
        &c,
        "apply_suggestion",
        json!({ "thread": id, "mode": "amend" }),
    )
    .await;
    assert_eq!(r["status"], "applied", "{r}");
    let (t, _) = call(
        &c,
        "reply",
        json!({ "thread": id, "body": format!("Applied the suggestion: amended {} ({}).", before["change"].as_str().unwrap_or(""), r["id"].as_str().unwrap_or("")), "setStatus": "addressed" }),
    )
    .await;
    assert_eq!(t["status"], "addressed");

    // The file text changed under the same path.
    let (after, _) = call(
        &c,
        "read_code",
        json!({ "path": path, "start": line, "end": line }),
    )
    .await;
    let blob = after["blob"].as_str().unwrap();
    let txt: Value = http
        .get(format!("{api}/blobs/{blob}/text?from={line}&to={line}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(txt["lines"][0], new_line);

    let seen = ui_task.await.unwrap();
    let kinds: Vec<&str> = seen.iter().filter_map(|e| e["type"].as_str()).collect();
    eprintln!("UI received: {kinds:?}");
    assert!(kinds.contains(&"thread.upserted"));
    assert!(kinds.contains(&"edit.updated"));
    let ws_updated = kinds.contains(&"workspace.updated");
    let attention = kinds.contains(&"agent.attention");
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true, "thread": id, "edit": r["id"], "line": txt["lines"][0],
            "uiEvents": kinds, "workspaceUpdated": ws_updated, "agentAttention": attention,
        }))
        .unwrap()
    );
    let _ = c.cancel().await;
}
