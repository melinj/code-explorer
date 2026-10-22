//! Review routes, WebSocket and long-poll through a real server on a scripted jj repo: thread,
//! preview (no op written), apply (amend) with edit.updated → op.changed → view.invalidated naming
//! the edited file's windows, new blob text served and the old blob still readable, an external
//! `jj` op seen by the watcher, and a WS reconnect with a cursor replaying what was missed.
//! Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use cv_server::index::Options;
use cv_server::routes::{self, AppState};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

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

async fn call(base: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    // Minimal HTTP/1.1 client over a TcpStream, to keep dev-dependencies small.
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
        // De-chunk.
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

/// Frames until one of type `kind` arrives; returns all of them.
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

#[test]
fn review_flow_over_http_and_websocket() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    jj(&repo, &["git", "init"]);
    let mut body = String::new();
    for i in 0..4 {
        body.push_str(&format!("def f{i}(x):\n    y = x + {i}\n    return y\n\n"));
    }
    std::fs::write(repo.join("src/a.py"), &body).unwrap();
    std::fs::write(repo.join("src/b.py"), "from a import f0\nf0(1)\n").unwrap();
    jj(&repo, &["commit", "-m", "base"]);
    std::fs::write(repo.join("src/b.py"), "from a import f0\nf0(2)\n").unwrap();
    jj(&repo, &["describe", "-m", "work"]);

    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    review.start_watcher(Duration::from_millis(100));

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
        let v = state_view(&state, &vid);
        let wa: Vec<String> = v.0.clone();
        let old_blob = v.1.clone();
        let work_change = jj(&repo, &["log", "--no-graph", "-r", "@", "-T", "change_id", "--ignore-working-copy"]);

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/v1/ws")).await.unwrap();
        let hello = next_frame(&mut ws).await;
        assert_eq!(hello["event"]["type"], "hello");
        ws.send(Message::Text(json!({ "type": "ping" }).to_string().into())).await.unwrap();
        ws.send(Message::Text(json!({ "type": "view.state", "state": { "viewId": vid, "lens": "heat" } }).to_string().into())).await.unwrap();

        // Thread on src/a.py line 2 of the working-copy change.
        let (st, t) = call(&base, "POST", "/api/v1/threads", Some(json!({
            "anchor": { "path": "src/a.py", "rev": work_change, "start": 2, "end": 2 },
            "body": "Rename:\n```suggestion\n    renamed_value = x + 0\n```",
            "kind": "suggestion"
        }))).await;
        assert_eq!(st, 200, "{t}");
        let f = until(&mut ws, "thread.upserted").await;
        assert_eq!(f.last().unwrap()["event"]["thread"]["id"], t["id"]);
        let (_, list) = call(&base, "GET", "/api/v1/threads?path=src/a.py", None).await;
        assert_eq!(list.as_array().unwrap().len(), 1);

        // Preview: nothing written.
        let op0 = jj(&repo, &["op", "log", "-n1", "--no-graph", "-T", "id", "--ignore-working-copy"]);
        let (_, p) = call(&base, "POST", "/api/v1/edits", Some(json!({
            "target": work_change,
            "files": [{ "path": "src/a.py", "baseBlob": old_blob, "patches": [{ "start": 2, "end": 2, "text": "    renamed_value = x + 0" }] }],
            "mode": "amend", "dryRun": true, "origin": { "thread": t["id"] }
        }))).await;
        assert_eq!(p["status"], "previewed", "{p}");
        assert_eq!(p["predicted"], true);
        assert_eq!(jj(&repo, &["op", "log", "-n1", "--no-graph", "-T", "id", "--ignore-working-copy"]), op0);

        // Apply: edit.updated, op.changed, view.invalidated with the a.py windows.
        let (st, a) = call(&base, "POST", &format!("/api/v1/edits/{}/apply", p["id"].as_str().unwrap()), None).await;
        assert_eq!(st, 200, "{a}");
        assert_eq!(a["status"], "applied", "{a}");
        let frames = until(&mut ws, "view.invalidated").await;
        let kinds: Vec<&str> = frames.iter().map(|f| f["event"]["type"].as_str().unwrap()).collect();
        let pos = |k: &str| kinds.iter().position(|x| *x == k).unwrap();
        assert!(pos("edit.updated") < pos("op.changed") && pos("op.changed") < pos("view.invalidated"), "{kinds:?}");
        let inv = &frames.last().unwrap()["event"];
        let ws_ids: Vec<String> = inv["windows"].as_array().unwrap().iter().map(|w| w.as_str().unwrap().to_string()).collect();
        assert!(!ws_ids.is_empty() && ws_ids.iter().all(|w| wa.contains(w)), "{ws_ids:?} vs {wa:?}");
        // Seqs are increasing strings.
        let seqs: Vec<u64> = frames.iter().map(|f| f["seq"].as_str().unwrap().parse().unwrap()).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(std::fs::read_to_string(repo.join("src/a.py")).unwrap().lines().nth(1), Some("    renamed_value = x + 0"));

        // The view serves the new text under the new blob, at the same place; the old blob stays readable.
        let (new_blob, rect_same) = {
            let v2 = state_view(&state, &vid);
            (v2.1.clone(), v2.2 == v.2)
        };
        assert_ne!(new_blob, old_blob);
        assert!(rect_same, "windows keep their positions");
        let (_, txt) = call(&base, "GET", &format!("/api/v1/blobs/{new_blob}/text?from=2&to=2"), None).await;
        assert_eq!(txt["lines"][0], "    renamed_value = x + 0");
        let (_, old) = call(&base, "GET", &format!("/api/v1/blobs/{old_blob}/text?from=2&to=2"), None).await;
        assert_eq!(old["lines"][0], "    y = x + 0");
        let (_, th) = call(&base, "GET", &format!("/api/v1/threads/{}", t["id"].as_str().unwrap()), None).await;
        assert_eq!(th["status"], "open");
        assert_eq!(th["producedEdits"][0], a["id"]);
        // Undo (before anything else touches the change) the edit: jj op revert of exactly that op.
        let (_, u) = call(&base, "POST", &format!("/api/v1/edits/{}/undo", a["id"].as_str().unwrap()), None).await;
        assert_eq!(u["status"], "undone", "{u}");
        assert_eq!(std::fs::read_to_string(repo.join("src/a.py")).unwrap().lines().nth(1), Some("    y = x + 0"));
        let undone = until(&mut ws, "view.invalidated").await;
        let last_seq: u64 = undone.last().unwrap()["seq"].as_str().unwrap().parse().unwrap();
        drop(ws);

        // While disconnected: an external jj op (seen by the watcher).
        jj(&repo, &["describe", "-m", "work, described outside cv"]);
        let (_, evs) = call(&base, "GET", &format!("/api/v1/events?cursor={last_seq}&kinds=view.invalidated&timeoutS=3"), None).await;
        assert_eq!(evs["events"][0]["type"], "view.invalidated", "{evs}");

        // Reconnect with the cursor: the missed op.changed and view.invalidated are replayed.
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/v1/ws?cursor={last_seq}")).await.unwrap();
        assert_eq!(next_frame(&mut ws).await["event"]["type"], "hello");
        let replay = until(&mut ws, "view.invalidated").await;
        // First the rest of the apply's events (re-anchored thread, workspace), then the external op.
        let op = replay.iter().find(|f| f["event"]["type"] == "op.changed").expect("op.changed replayed");
        assert_eq!(op["event"]["actor"]["kind"], "external");
        assert!(replay.iter().all(|f| f["seq"].as_str().unwrap().parse::<u64>().unwrap() > last_seq));
        drop(ws);

        // Retention: once events after a cursor are pruned, a replay from it would have a hole.
        // WS hello and GET /events say resync instead of replaying part of the log.
        assert!(review.events().cursor() > 3);
        review.db().prune_events(2, 2, "9999-01-01T00:00:00.000Z").unwrap();
        review.events().prune();
        let (_, evs) = call(&base, "GET", "/api/v1/events?cursor=1&timeoutS=0", None).await;
        assert_eq!(evs["resync"], true, "{evs}");
        let head = review.events().cursor();
        let (_, evs) = call(&base, "GET", &format!("/api/v1/events?cursor={}&timeoutS=0", head - 1), None).await;
        assert!(evs.get("resync").is_none(), "a kept cursor replays: {evs}");
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/v1/ws?cursor=1")).await.unwrap();
        let hello = next_frame(&mut ws).await;
        assert_eq!(hello["event"]["type"], "hello");
        assert_eq!(hello["event"]["resync"], true, "{hello}");
        assert_eq!(hello["event"]["cursor"], head.to_string());
        drop(ws);


        // Errors carry { error: { code, message } }.
        let (st, e) = call(&base, "GET", "/api/v1/threads/nope", None).await;
        assert_eq!(st, 404);
        assert_eq!(e["error"]["code"], "not-found");
    });
    assert!(
        review.view_state(None).is_some(),
        "view.state kept for get_view_state"
    );
    review.stop_watcher();
}

/// (a.py window ids, a.py blob, a.py window rects) of a view.
fn state_view(state: &Arc<AppState>, vid: &str) -> (Vec<String>, String, Vec<(i64, i64)>) {
    // Through the public route helpers: tile 0/0/0 at a high level covers everything small.
    let v = cv_server::review_api::view_snapshot(state, vid).unwrap();
    let f = &v.files[v.by_path["src/a.py"]];
    let ids = f
        .windows
        .iter()
        .map(|&w| v.windows[w].header.id.clone())
        .collect();
    let rects = f
        .windows
        .iter()
        .map(|&w| {
            let r = &v.windows[w].header.rect;
            (r.x as i64, r.y as i64)
        })
        .collect();
    (ids, f.blob.clone(), rects)
}

/// PUT /reviews/marks/:change stores the mark and emits `review.marked` (change, reviewed
/// commit, reviewer) on the event log, for the long-poll and the WebSocket alike.
#[test]
fn marking_a_change_reviewed_emits_review_marked() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::write(repo.join("src/a.py"), "x = 1\n").unwrap();
    jj(&repo, &["describe", "-m", "work"]);
    let q = |t: &str| {
        jj(
            &repo,
            &[
                "log",
                "--no-graph",
                "-r",
                "@",
                "-T",
                t,
                "--ignore-working-copy",
            ],
        )
    };
    let (change, commit) = (q("change_id"), q("commit_id"));

    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    let c0 = review.events().cursor();

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
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://{addr}/api/v1/ws?cursor={c0}"))
                .await
                .unwrap();
        assert_eq!(next_frame(&mut ws).await["event"]["type"], "hello");

        let (st, m) = call(
            &base,
            "PUT",
            &format!("/api/v1/reviews/marks/{change}"),
            Some(json!({ "commit": commit })),
        )
        .await;
        assert_eq!(st, 200, "{m}");
        assert_eq!(m["change"], change.as_str());
        assert_eq!(m["reviewedAt"], commit.as_str());
        assert_eq!(m["changedSinceReview"], false);

        let (_, evs) = call(
            &base,
            "GET",
            &format!("/api/v1/events?cursor={c0}&kinds=review.marked&timeoutS=3"),
            None,
        )
        .await;
        let e = &evs["events"][0];
        assert_eq!(e["type"], "review.marked", "{evs}");
        assert_eq!(e["change"], change.as_str());
        assert_eq!(e["commit"], commit.as_str());
        assert_eq!(e["reviewer"], "you", "{evs}");
        assert_eq!(evs["events"].as_array().unwrap().len(), 1, "{evs}");

        let f = until(&mut ws, "review.marked").await;
        let ev = &f.last().unwrap()["event"];
        assert_eq!(
            (ev["change"].as_str(), ev["commit"].as_str()),
            (Some(change.as_str()), Some(commit.as_str()))
        );

        // An unknown change is a 404 and emits nothing.
        let c1 = review.events().cursor();
        let (st, _) = call(
            &base,
            "PUT",
            "/api/v1/reviews/marks/zzzzzzzzzzzzqqqq",
            Some(json!({ "commit": commit })),
        )
        .await;
        assert_eq!(st, 404);
        assert!(review.events().since(c1, None).is_empty());
    });
}
