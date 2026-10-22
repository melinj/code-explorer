//! N10 resource subscriptions on `cv serve`'s real in-process backend (the cv-mcp tests use
//! FakeBackend): /mcp over Streamable HTTP, resources/subscribe, then
//! notifications/resources/updated on the session's GET stream when a thread is created through
//! the review store, when an external `jj` operation lands (op.changed → cv://conflicts) and when
//! the UI reports another view state; unsubscribe stops them. Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use cv_contracts::review::{Actor, NewAnchor, NewThread};
use cv_server::index::Options;
use cv_server::routes::{self, AppState};
use serde_json::{Value, json};

const USER: &str = "[user]\nname = \"Test\"\nemail = \"test@example.com\"\n";

fn jj(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("jj")
        .args(args)
        .current_dir(dir)
        .env("JJ_CONFIG", dir.join("../jjconfig.toml"))
        .output()
        .expect("run jj");
    assert!(
        out.status.success(),
        "jj {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether `jj` is on PATH. Tests that need it skip without it, loudly; under CI (or with
/// CV_REQUIRE_JJ set, as scripts/ci.sh does) a missing jj fails them instead of skipping.
fn have_jj() -> bool {
    let ok = Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var_os("CI").is_none() && std::env::var_os("CV_REQUIRE_JJ").is_none(),
            "jj is not on PATH: CI must install it (these tests would skip)"
        );
        eprintln!("SKIP: jj is not on PATH");
    }
    ok
}

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
}

impl Session {
    async fn post(&mut self, body: Value) -> String {
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
        let res = rb.send().await.unwrap();
        if let Some(s) = res.headers().get("mcp-session-id") {
            self.session = Some(s.to_str().unwrap().to_string());
        }
        let status = res.status();
        let text = res.text().await.unwrap();
        assert!(status.is_success(), "{status} {text}");
        text
    }

    async fn rpc(&mut self, id: u64, method: &str, params: Value) -> Value {
        let text = self
            .post(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        rpc_body(&text)
    }
}

/// Waits for an update of `uri`; None after `secs`.
async fn updated(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    uri: &str,
    secs: f64,
) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(secs);
    loop {
        let v = tokio::time::timeout_at(deadline, rx.recv()).await.ok()??;
        if v["method"] == "notifications/resources/updated" && v["params"]["uri"] == uri {
            return Some(v);
        }
    }
}

/// Drops whatever arrives within `secs` (updates of earlier changes).
async fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>, secs: f64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(secs);
    while tokio::time::timeout_at(deadline, rx.recv()).await.is_ok() {}
}

#[test]
fn resource_subscriptions_notify_on_the_in_process_backend() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::write(
        repo.join("src/a.py"),
        "def f(x):\n    y = x + 1\n    return y\n",
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "base"]);
    std::fs::write(
        repo.join("src/a.py"),
        "def f(x):\n    y = x + 2\n    return y\n",
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "work"]);

    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    review.start_watcher(Duration::from_millis(100));
    let vid = state
        .view_for_spec(&json!({ "mode": "snapshot", "rev": "@" }))
        .unwrap()
        .id()
        .to_string();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // What `cv serve` mounts: the API and /mcp on the in-process backend.
        let app = routes::router(state.clone()).merge(cv_server::mcp::router(state.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());
        let url = format!("http://{addr}/mcp");
        let mut s = Session {
            http: reqwest::Client::new(),
            url: url.clone(),
            session: None,
        };
        let init = s
            .rpc(
                1,
                "initialize",
                json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": "test-agent", "version": "1" } }),
            )
            .await;
        assert_eq!(init["result"]["serverInfo"]["name"], "cv", "{init}");
        assert!(
            init["result"]["capabilities"]["resources"]["subscribe"] == true,
            "{init}"
        );
        s.post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;

        // The standalone SSE stream that carries this session's server notifications.
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

        for (id, uri) in [
            (2, "cv://threads?status=open"),
            (3, "cv://conflicts"),
            (4, "cv://view/state"),
        ] {
            let r = s
                .rpc(id, "resources/subscribe", json!({ "uri": uri }))
                .await;
            assert!(r["result"].is_object(), "{uri}: {r}");
        }
        // No settling wait: the watcher follows the log from the cursor read before each
        // subscribe was acknowledged (start-up events are before it).

        // A thread created through the real review store → open threads updated.
        let r2 = review.clone();
        tokio::task::spawn_blocking(move || {
            r2.create_thread(
                NewThread {
                    anchor: NewAnchor {
                        path: "src/a.py".into(),
                        rev: "@".into(),
                        start: 2,
                        end: Some(2),
                        side: None,
                        cols: None,
                    },
                    body: "Why 2?".into(),
                    kind: Some("question".into()),
                    severity: None,
                    assignee: None,
                },
                &Actor::human("you"),
            )
            .unwrap()
        })
        .await
        .unwrap();
        assert!(
            updated(&mut rx, "cv://threads?status=open", 10.0)
                .await
                .is_some(),
            "no update for open threads"
        );
        // The resource reads the thread back.
        let read = s
            .rpc(
                5,
                "resources/read",
                json!({ "uri": "cv://threads?status=open" }),
            )
            .await;
        assert!(
            read["result"]["contents"][0]["text"]
                .as_str()
                .unwrap_or("")
                .contains("Why 2?"),
            "{read}"
        );

        // An external jj operation (seen by the review watcher) → op.changed → conflicts.
        let repo2 = repo.clone();
        tokio::task::spawn_blocking(move || jj(&repo2, &["describe", "-m", "work, outside"]))
            .await
            .unwrap();
        assert!(
            updated(&mut rx, "cv://conflicts", 10.0).await.is_some(),
            "no update for conflicts after an external op"
        );

        // The UI reports another view state (WS view.state; not a logged event) → view state.
        drain(&mut rx, 1.5).await;
        review.set_view_state(
            "ui-1",
            json!({ "viewId": vid, "focus": "w:src/a.py",
                    "viewport": { "rect": { "x": 0, "y": 0, "w": 10, "h": 10 }, "zoom": 1, "tier": "L3" },
                    "lens": "heat",
                    "followAgent": false }),
        );
        assert!(
            updated(&mut rx, "cv://view/state", 10.0).await.is_some(),
            "no update for the view state"
        );
        let read = s
            .rpc(7, "resources/read", json!({ "uri": "cv://view/state" }))
            .await;
        let text = read["result"]["contents"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains(&vid) && text.contains("heat"), "{read}");

        // Unsubscribed: no more thread updates.
        let r = s
            .rpc(
                6,
                "resources/unsubscribe",
                json!({ "uri": "cv://threads?status=open" }),
            )
            .await;
        assert!(r["result"].is_object(), "{r}");
        let r2 = review.clone();
        tokio::task::spawn_blocking(move || {
            r2.create_thread(
                NewThread {
                    anchor: NewAnchor {
                        path: "src/a.py".into(),
                        rev: "@".into(),
                        start: 3,
                        end: Some(3),
                        side: None,
                        cols: None,
                    },
                    body: "Again?".into(),
                    kind: Some("question".into()),
                    severity: None,
                    assignee: None,
                },
                &Actor::human("you"),
            )
            .unwrap()
        })
        .await
        .unwrap();
        assert!(
            updated(&mut rx, "cv://threads?status=open", 3.0)
                .await
                .is_none(),
            "update after unsubscribe"
        );
    });
    review.stop_watcher();
}
