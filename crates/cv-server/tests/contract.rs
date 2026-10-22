//! Contract details (http.ts, ws.ts, ids.ts) on scripted jj repos: ETag = layoutVersion with
//! If-None-Match → 304, token auth (no cookie), ISO-8601 times, non-chain ranges rejected with a hint (and
//! allowed per commit on opt-in), ranges stale after an op, the history overlay (thread counts,
//! "changed since review" with a working interdiff, conflicts), Hotspots ranked by in-range
//! touches, and over a real server: WS view.subscribe / unsubscribe, ping → pong, "expand"
//! invalidations, /agent/focus "moved" only while a UI follows, /agent/bind 404.
//! Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cv_server::index::Options;
use cv_server::routes::{self, AppState};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

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

fn engine(extra: &[&str]) -> String {
    let mut s = String::from("from .util import helper\n\n");
    for i in 0..6 {
        s.push_str(&format!("def step_{i}(x):\n"));
        for k in 0..14 {
            s.push_str(&format!("    x = helper(x) + {k}\n"));
            if i == 3 && k == 6 {
                for e in extra {
                    s.push_str(e);
                }
            }
        }
        s.push_str("    return x\n\n");
    }
    s
}

/// init (bookmark main) ← s1 (util, step_3) ← s2 (@, step_3 again).
fn fixture() -> Option<tempfile::TempDir> {
    if !have_jj() {
        eprintln!("jj not found; skipping");
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src/pkg")).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::write(repo.join("src/pkg/__init__.py"), "").unwrap();
    std::fs::write(repo.join("src/pkg/engine.py"), engine(&[])).unwrap();
    std::fs::write(
        repo.join("src/pkg/util.py"),
        "def helper(x):\n    return x * 2\n",
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "init"]);
    jj(&repo, &["bookmark", "create", "main", "-r", "@-"]);
    std::fs::write(
        repo.join("src/pkg/util.py"),
        "def helper(x):\n    return x * 3\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/pkg/engine.py"),
        engine(&["    x = x - 1  # s1\n"]),
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "s1"]);
    std::fs::write(
        repo.join("src/pkg/engine.py"),
        engine(&["    x = x - 1  # s1\n", "    x = x + 7  # s2\n"]),
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "s2"]);
    Some(tmp)
}

fn state_with_review(repo: &Path) -> (Arc<AppState>, Arc<cv_review::Review>) {
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    (state, review)
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

async fn req(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    hdrs: &[(&str, &str)],
) -> Res {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in hdrs {
        b = b.header(*k, *v);
    }
    let r = match body {
        Some(j) => b
            .header("content-type", "application/json")
            .body(Body::from(j.to_string())),
        None => b.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(r).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    Res {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

async fn ok(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let r = req(app, method, uri, body, &[]).await;
    assert!(
        r.status.is_success(),
        "{method} {uri}: {} {}",
        r.status,
        r.body
    );
    r.body
}

fn is_iso(s: &str) -> bool {
    // 2026-10-18T13:25:45.123Z
    s.len() >= 20 && s.as_bytes()[4] == b'-' && s.as_bytes()[10] == b'T' && s.ends_with('Z')
}

#[tokio::test(flavor = "multi_thread")]
async fn etags_iso_times_and_range_shapes() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    // A side branch off main (not moving @): main+ is then two roots and two heads.
    jj(&repo, &["new", "main", "--no-edit", "-m", "side"]);
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let app = routes::router(state.clone());

    // ETag = layoutVersion (+ content hash); If-None-Match → 304.
    let v = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "@" })),
    )
    .await;
    let vid = v["id"].as_str().unwrap().to_string();
    let lv = v["layoutVersion"].as_u64().unwrap();
    for (method, uri, body) in [
        ("GET", format!("/api/v1/views/{vid}/bootstrap"), None),
        ("GET", format!("/api/v1/views/{vid}/tiles/6/0/0"), None),
        (
            "POST",
            format!("/api/v1/views/{vid}/windows:batch"),
            Some(json!({ "ids": [], "parts": ["outline"], "layoutVersion": lv })),
        ),
    ] {
        let r = req(&app, method, &uri, body.clone(), &[]).await;
        assert_eq!(r.status, 200, "{uri}");
        let tag = r.headers[header::ETAG].to_str().unwrap().to_string();
        assert!(tag.starts_with(&format!("\"{lv}-")), "{uri}: {tag}");
        let again = req(&app, method, &uri, body.clone(), &[("if-none-match", &tag)]).await;
        assert_eq!(again.status, StatusCode::NOT_MODIFIED, "{uri}");
        assert_eq!(again.headers[header::ETAG].to_str().unwrap(), tag);
        let other = req(&app, method, &uri, body, &[("if-none-match", "\"0-0\"")]).await;
        assert_eq!(other.status, 200, "{uri}");
    }

    // ISO-8601 times on history rows and workspace cards.
    let h = ok(&app, "GET", "/api/v1/revisions?revset=all()&limit=10", None).await;
    for row in h["rows"].as_array().unwrap() {
        let rev = &row["rev"];
        assert!(is_iso(rev["author"]["time"].as_str().unwrap()), "{rev}");
        assert!(is_iso(rev["committerTime"].as_str().unwrap()), "{rev}");
    }
    let ws = ok(&app, "GET", "/api/v1/workspaces", None).await;
    assert!(is_iso(ws[0]["lastOp"]["time"].as_str().unwrap()), "{ws}");

    // Ranges must be chains (D16): two roots/heads → 400 with a hint, unless the spec opts in.
    let r = req(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "range", "revset": "main+" })),
        &[],
    )
    .await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(r.body["error"]["code"], "range-not-chain");
    let msg = r.body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("2 roots") && msg.contains("2 heads"), "{msg}");
    assert!(
        r.body["error"]["details"]["hint"]
            .as_str()
            .unwrap()
            .contains("allowNonChain")
    );
    let opt = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "range", "revset": "main+", "options": { "allowNonChain": true } })),
    )
    .await;
    assert_eq!(opt["resolved"]["commits"].as_array().unwrap().len(), 2);
    let kinds: Vec<&str> = opt["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"multiple-heads"), "{kinds:?}");
    // A chain still works.
    ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "range", "revset": "main..@" })),
    )
    .await;

    // Hotspots ranked by in-range touches (D16): step_3's file is touched by both commits.
    let r = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "range", "revset": "main..@" })),
    )
    .await;
    let rid = r["id"].as_str().unwrap();
    let m = ok(
        &app,
        "GET",
        &format!("/api/v1/views/{rid}/metrics?level=file&rank=inRange"),
        None,
    )
    .await;
    let pts = m["points"].as_array().unwrap();
    assert!(pts.iter().all(|p| p["heat"]["inRange"]["of"] == 2), "{m}");
    assert_eq!(pts[0]["label"], "src/pkg/engine.py", "{m}");
    assert_eq!(pts[0]["heat"]["inRange"]["commits"], 2);
    let counts: Vec<u64> = pts
        .iter()
        .map(|p| p["heat"]["inRange"]["commits"].as_u64().unwrap())
        .collect();
    assert!(counts.windows(2).all(|w| w[0] >= w[1]), "{counts:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn token_auth_for_the_ui_and_no_cookie() {
    let api = axum::Router::new().route("/api/v1/x", axum::routing::get(|| async { "ok" }));
    let app = cv_server::auth::protect(api, "s3cret0000000000");
    // Bearer header and ?token= authenticate; nothing sets a cookie (a cookie for 127.0.0.1 would
    // reach every other local port), and a cookie is not a credential.
    let with = |h: &'static str, v: &'static str| [(h, v)];
    let b = req(
        &app,
        "GET",
        "/api/v1/x",
        None,
        &with("authorization", "Bearer s3cret0000000000"),
    )
    .await;
    assert_eq!(b.status, 200);
    let q = req(&app, "GET", "/api/v1/x?token=s3cret0000000000", None, &[]).await;
    assert_eq!(q.status, 200);
    assert!(
        q.headers.get(header::SET_COOKIE).is_none() && b.headers.get(header::SET_COOKIE).is_none()
    );
    assert_eq!(
        req(
            &app,
            "GET",
            "/api/v1/x",
            None,
            &with("cookie", "cv_token=s3cret0000000000")
        )
        .await
        .status,
        401
    );
    assert_eq!(req(&app, "GET", "/api/v1/x", None, &[]).await.status, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn history_overlay_interdiff_and_conflicts() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    let (state, review) = state_with_review(&repo);
    let app = routes::router(state.clone());
    let log = |r: &str, t: &str| {
        jj(
            &repo,
            &[
                "log",
                "--no-graph",
                "--ignore-working-copy",
                "-r",
                r,
                "-T",
                t,
            ],
        )
    };
    let (s1, s2) = (log("@-", "change_id"), log("@", "change_id"));
    let s2_commit = log("@", "commit_id");

    // A thread on s1 (by an agent) and a review mark on s2.
    let t = req(
        &app,
        "POST",
        "/api/v1/threads",
        Some(json!({ "anchor": { "path": "src/pkg/util.py", "rev": s1, "start": 2, "end": 2 }, "body": "why ×3?" })),
        &[("x-cv-agent", "claude")],
    )
    .await;
    assert_eq!(t.status, 200, "{}", t.body);
    let mark = ok(
        &app,
        "PUT",
        &format!("/api/v1/reviews/marks/{s2}"),
        Some(json!({ "commit": s2_commit })),
    )
    .await;
    assert_eq!(mark["changedSinceReview"], false);
    let rows = ok(&app, "GET", "/api/v1/revisions?revset=all()", None).await;
    let row = |rows: &Value, change: &str| {
        rows["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["rev"]["change"] == change)
            .cloned()
            .unwrap()
    };
    let r1 = row(&rows, &s1);
    assert_eq!(r1["review"]["openThreads"], 1, "{r1}");
    assert_eq!(r1["review"]["touchedByAgent"], true);
    let r2 = row(&rows, &s2);
    assert_eq!(r2["review"]["reviewedAt"], s2_commit.as_str());
    assert_eq!(r2["review"]["changedSinceReview"], false);

    // s2 is rewritten after the review: "changed since review", and the interdiff opens.
    std::fs::write(
        repo.join("src/pkg/engine.py"),
        engine(&["    x = x - 1  # s1\n", "    x = x + 8  # s2, reworked\n"]),
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "s2 reworked"]);
    review.sync().unwrap();
    let rows = ok(&app, "GET", "/api/v1/revisions?revset=all()", None).await;
    let r2 = row(&rows, &s2);
    assert_eq!(r2["review"]["changedSinceReview"], true, "{r2}");
    let reviewed = r2["review"]["reviewedAt"].as_str().unwrap().to_string();
    let d = ok(
        &app,
        "GET",
        &format!("/api/v1/interdiff?change={s2}&from={reviewed}"),
        None,
    )
    .await;
    let f = &d["files"][0];
    assert_eq!(f["path"], "src/pkg/engine.py", "{d}");
    let lines: Vec<&Value> = f["hunks"][0]["lines"].as_array().unwrap().iter().collect();
    assert!(
        lines
            .iter()
            .any(|l| l["kind"] == "add" && l["text"].as_str().unwrap().contains("x + 8")),
        "{d}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l["kind"] == "del" && l["text"].as_str().unwrap().contains("x + 7")),
        "{d}"
    );
    assert_eq!(d["description"][1].as_str().unwrap().trim(), "s2 reworked");

    // Conflicted rows are marked: s1 rebased onto a sibling that changes the same line.
    jj(&repo, &["new", "main", "-m", "side"]);
    std::fs::write(
        repo.join("src/pkg/util.py"),
        "def helper(x):\n    return x * 5\n",
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "side"]);
    let side = log("@", "change_id");
    jj(&repo, &["rebase", "-s", &s1, "-d", &side]);
    review.sync().unwrap();
    let rows = ok(&app, "GET", "/api/v1/revisions?revset=all()", None).await;
    assert_eq!(row(&rows, &s1)["rev"]["flags"]["conflict"], true);
    assert_eq!(row(&rows, &side)["rev"]["flags"]["conflict"], false);
    // s2 moved onto the new base with s1: the interdiff rebases the reviewed commit first, so
    // the base's util.py change is not part of it (jj interdiff).
    let d = ok(
        &app,
        "GET",
        &format!("/api/v1/interdiff?change={s2}&from={reviewed}"),
        None,
    )
    .await;
    let paths: Vec<&str> = d["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["src/pkg/engine.py"], "{d}");
}

// ─── Over a real server: WebSocket and agent routes ──────────────────────────

async fn call(base: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = base.trim_start_matches("http://");
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, rest) = text.split_once("\r\n\r\n").unwrap();
    let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut out = String::new();
        let mut r = rest;
        loop {
            let (len, after) = r.split_once("\r\n").unwrap();
            let n = usize::from_str_radix(len.trim(), 16).unwrap();
            if n == 0 {
                break;
            }
            out.push_str(&after[..n]);
            r = &after[n + 2..];
        }
        out
    } else {
        rest.to_string()
    };
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_frame(ws: &mut Ws) -> Value {
    loop {
        // A hang guard, not a latency check: a loaded machine can take seconds.
        let m = tokio::time::timeout(Duration::from_secs(60), ws.next())
            .await
            .expect("frame within 60 s")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// Frames until one with event type `kind`; returns all of them.
async fn until(ws: &mut Ws, kind: &str) -> Vec<Value> {
    let mut out = Vec::new();
    loop {
        let f = next_frame(ws).await;
        let done = f["event"]["type"] == kind;
        out.push(f);
        if done {
            return out;
        }
    }
}

async fn send(ws: &mut Ws, m: Value) {
    ws.send(Message::Text(m.to_string().into())).await.unwrap();
}

fn invalidations<'a>(frames: &'a [Value], view: &str) -> Vec<&'a Value> {
    frames
        .iter()
        .map(|f| &f["event"])
        .filter(|e| e["type"] == "view.invalidated" && e["viewId"] == view)
        .collect()
}

#[test]
fn websocket_subscriptions_ping_focus_and_bind() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    let (state, review) = state_with_review(&repo);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, routes::router(state.clone())).into_future());
        let base = format!("http://{addr}");
        let (_, view) = call(&base, "POST", "/api/v1/views", Some(json!({ "mode": "snapshot", "rev": "@" }))).await;
        let vid = view["id"].as_str().unwrap().to_string();
        let (st, range) = call(&base, "POST", "/api/v1/views", Some(json!({ "mode": "range", "revset": "main..@" }))).await;
        assert_eq!(st, 200, "{range}");
        let rid = range["id"].as_str().unwrap().to_string();

        let url = format!("ws://{addr}/api/v1/ws");
        let (mut sub, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let (mut legacy, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let (mut other, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        for ws in [&mut sub, &mut legacy, &mut other] {
            assert_eq!(next_frame(ws).await["event"]["type"], "hello");
        }
        send(&mut sub, json!({ "type": "view.subscribe", "viewId": vid })).await;
        send(&mut sub, json!({ "type": "view.subscribe", "viewId": rid })).await;
        send(&mut other, json!({ "type": "view.subscribe", "viewId": "v_elsewhere" })).await;
        send(&mut other, json!({ "type": "view.subscribe", "viewId": rid })).await;
        send(&mut other, json!({ "type": "view.unsubscribe", "viewId": rid })).await;

        // ping → pong (not logged).
        send(&mut sub, json!({ "type": "ping" })).await;
        let pong = next_frame(&mut sub).await;
        assert_eq!(pong["event"]["type"], "pong", "{pong}");

        // /agent/focus: nobody follows → "suggested"; a UI with followAgent → "moved".
        let focus = json!({ "target": { "path": "src/pkg/util.py", "line": 1 } });
        let (_, f) = call(&base, "POST", "/api/v1/agent/focus", Some(focus.clone())).await;
        assert_eq!(f["delivered"], "suggested");
        // Moving the camera needs both the human's permission (§10) and a following UI.
        let (st, p) = call(&base, "PUT", "/api/v1/agent/permissions", Some(json!({ "moveCamera": true }))).await;
        assert_eq!(st, 200, "{p}");
        let (_, f) = call(&base, "POST", "/api/v1/agent/focus", Some(focus.clone())).await;
        assert_eq!(f["delivered"], "suggested", "permission alone does not move a UI that is not following");
        send(&mut legacy, json!({ "type": "view.state", "state": { "viewId": vid, "lens": "none", "followAgent": true } })).await;
        // Until the server has read that view.state (a condition, bounded only against a hang).
        let mut moved = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while tokio::time::Instant::now() < deadline {
            let (_, f) = call(&base, "POST", "/api/v1/agent/focus", Some(focus.clone())).await;
            if f["delivered"] == "moved" {
                moved = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(moved, "a following UI makes focus move the camera");

        // /agent/bind: unknown workspace → 404; a real one binds.
        let (st, e) = call(&base, "POST", "/api/v1/agent/bind", Some(json!({ "agent": "claude", "workspace": "nope" }))).await;
        assert_eq!(st, 404, "{e}");
        assert_eq!(e["error"]["code"], "not-found");
        let (st, b) = call(&base, "POST", "/api/v1/agent/bind", Some(json!({ "agent": "claude", "workspace": "default" }))).await;
        assert_eq!(st, 200, "{b}");
        assert!(b["agent"]["lastSeen"].as_str().unwrap().ends_with('Z'), "{b}");

        // An external op: view-scoped events reach subscribers of that view and clients that
        // never subscribed; the range drawn on old commits turns stale.
        jj(&repo, &["describe", "-m", "s2 renamed"]);
        review.sync().unwrap();
        let marker = |note: &str| json!({ "agent": "claude", "tool": "read_window", "windows": [], "note": note });
        call(&base, "POST", "/api/v1/agent/attention", Some(marker("after-op"))).await;
        let fs = until(&mut sub, "agent.attention").await;
        assert_eq!(invalidations(&fs, &vid).len(), 1, "{fs:?}");
        let ri = invalidations(&fs, &rid);
        assert_eq!(ri.len(), 1, "{fs:?}");
        assert_eq!(ri[0]["reason"], "op");
        assert!(ri[0].get("windows").is_none(), "stale range: everything");
        let fl = until(&mut legacy, "agent.attention").await;
        assert_eq!(invalidations(&fl, &vid).len(), 1);
        let fo = until(&mut other, "agent.attention").await;
        assert!(fs.len() > fo.len() && invalidations(&fo, &vid).is_empty() && invalidations(&fo, &rid).is_empty(), "{fo:?}");
        let (_, rv) = call(&base, "GET", &format!("/api/v1/views/{rid}"), None).await;
        assert_eq!(rv["status"], "stale");

        // Expand in a snapshot view: a layout change ("layout", like a PATCH) naming the widened
        // window first among what moved, with the new layoutVersion in the response.
        let v = cv_server::review_api::view_snapshot(&state, &vid).unwrap();
        let w = v.windows[0].header.id.clone();
        let lv = v.bootstrap.base.view.layout_version;
        let (st, h) = call(&base, "POST", &format!("/api/v1/views/{vid}/windows/{w}/expand"), Some(json!({ "after": 3 }))).await;
        assert_eq!(st, 200, "{h}");
        assert_eq!(h["layoutVersion"], json!(lv + 1), "{h}");
        call(&base, "POST", "/api/v1/agent/attention", Some(marker("after-expand"))).await;
        let fs = until(&mut sub, "agent.attention").await;
        let ex = invalidations(&fs, &vid);
        assert_eq!(ex.len(), 1, "{fs:?}");
        assert_eq!(ex[0]["reason"], "layout");
        assert!(ex[0]["windows"].as_array().unwrap().contains(&json!(w)), "{ex:?}");
        let fl = until(&mut legacy, "agent.attention").await;
        assert_eq!(invalidations(&fl, &vid).len(), 1, "layout changes reach every client of the view: {fl:?}");

        // The following UI disconnects: focus is a suggestion again.
        drop(legacy);
        let mut suggested = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while tokio::time::Instant::now() < deadline {
            let (_, f) = call(&base, "POST", "/api/v1/agent/focus", Some(focus.clone())).await;
            if f["delivered"] == "suggested" {
                suggested = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(suggested);
    });
}
