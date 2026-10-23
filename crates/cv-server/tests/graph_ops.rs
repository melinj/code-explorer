//! Graph view routes on a scripted jj repo: GET /refs finds the trunk bookmark, POST /graph:op
//! previews without writing and lands a workspace's stack by merge (op.changed follows), a
//! backwards bookmark move and a bad strategy are "rejected", and POST /graph:undo reverts the
//! landing. The operation log lists and restores operations, commit and squash run as graph
//! operations, and fetch / push reach a local bare git remote; restore, fetch and push refuse
//! agents. Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use cv_server::index::Options;
use cv_server::routes::{self, AppState};
use serde_json::{Value, json};

const USER: &str = "[user]\nname = \"Test\"\nemail = \"test@example.com\"\n";

fn jj(dir: &Path, cfg: &Path, args: &[&str]) -> String {
    let out = Command::new("jj")
        .args(args)
        .current_dir(dir)
        .env("JJ_CONFIG", cfg)
        .output()
        .expect("run jj");
    assert!(
        out.status.success(),
        "jj {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Minimal HTTP/1.1 request with `Connection: close` (keeps dev-dependencies small).
async fn call(base: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    call_as(base, method, path, body, None).await
}

/// [`call`] as an agent (`X-CV-Agent`) when `agent` is given.
async fn call_as(
    base: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    agent: Option<&str>,
) -> (u16, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = base.trim_start_matches("http://");
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let agent = agent
        .map(|a| format!("X-CV-Agent: {a}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n{agent}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

#[test]
fn land_a_workspace_from_the_graph_view_and_undo_it() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("jjconfig.toml");
    std::fs::write(&cfg, USER).unwrap();
    let repo = tmp.path().join("repo");
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &cfg, &["git", "init"]);
    std::fs::write(repo.join("a.py"), "x = 1\n").unwrap();
    jj(&repo, &cfg, &["commit", "-m", "T0"]);
    jj(&repo, &cfg, &["bookmark", "create", "main", "-r", "@-"]);
    jj(
        &repo,
        &cfg,
        &[
            "workspace",
            "add",
            "--name",
            "ws",
            "-r",
            "main",
            ws.to_str().unwrap(),
        ],
    );
    std::fs::write(ws.join("b.py"), "y = 2\n").unwrap();
    jj(&ws, &cfg, &["commit", "-m", "S1"]);
    jj(&repo, &cfg, &["new", "main"]);
    std::fs::write(repo.join("c.py"), "z = 3\n").unwrap();
    jj(&repo, &cfg, &["commit", "-m", "T1"]);
    jj(&repo, &cfg, &["bookmark", "set", "main", "-r", "@-"]);
    let log = |rev: &str, t: &str| {
        jj(
            &repo,
            &cfg,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                rev,
                "-T",
                t,
            ],
        )
    };
    let op_head = || {
        jj(
            &repo,
            &cfg,
            &[
                "op",
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-n1",
                "-T",
                "id",
            ],
        )
    };

    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());

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

        let (st, refs) = call(&base, "GET", "/api/v1/refs", None).await;
        assert_eq!(st, 200, "{refs}");
        assert_eq!(refs["trunk"], "main", "{refs}");
        assert!(refs["colocated"].is_boolean(), "{refs}");
        let main0 = log("main", "commit_id");
        assert_eq!(refs["bookmarks"][0]["targets"][0], main0.as_str());

        let land = json!({ "kind": "land", "source": { "workspace": "ws" }, "trunk": "main", "strategy": "merge" });
        let op0 = op_head();
        let (st, p) = call(&base, "POST", "/api/v1/graph:op", Some(json!({ "op": land, "dryRun": true }))).await;
        assert_eq!(st, 200, "{p}");
        assert_eq!(p["status"], "previewed", "{p}");
        assert!(p["created"].is_string(), "{p}");
        assert_eq!(op_head(), op0, "a preview writes nothing");

        // Refusals come back as results, nothing written.
        let (_, r) = call(&base, "POST", "/api/v1/graph:op", Some(json!({ "op": { "kind": "set-bookmark", "name": "main", "to": log("main-", "commit_id") } }))).await;
        assert_eq!(r["status"], "rejected", "{r}");
        assert!(r["error"]["message"].as_str().unwrap().contains("backwards"), "{r}");
        let (_, r) = call(&base, "POST", "/api/v1/graph:op", Some(json!({ "op": { "kind": "land", "source": { "workspace": "ws" }, "trunk": "main", "strategy": "squash" } }))).await;
        assert_eq!(r["status"], "rejected", "{r}");
        assert_eq!(op_head(), op0);

        let (st, a) = call(&base, "POST", "/api/v1/graph:op", Some(json!({ "op": land }))).await;
        assert_eq!(st, 200, "{a}");
        assert_eq!(a["status"], "applied", "{a}");
        assert!(a["summary"].as_str().unwrap().starts_with("land ws@ on main"), "{a}");
        assert_eq!(a["bookmarks"][0]["name"], "main");
        assert_eq!(log("parents(main)", "description.first_line() ++ \"\\n\"").lines().count(), 2);
        assert_eq!(log("parents(ws@)", "commit_id"), log("main", "commit_id"));
        // The history graph shows main on the merge.
        let (_, revs) = call(&base, "GET", "/api/v1/revisions?revset=main", None).await;
        assert_eq!(revs["rows"][0]["rev"]["bookmarks"][0], "main", "{revs}");
        assert_eq!(revs["rows"][0]["rev"]["parents"].as_array().unwrap().len(), 2);

        let (_, u) = call(&base, "POST", "/api/v1/graph:undo", Some(json!({ "op": a["opAfter"] }))).await;
        assert_eq!(u["status"], "applied", "{u}");
        assert_eq!(log("main", "commit_id"), main0, "undo puts main back");
    });
}

#[test]
fn op_log_restore_commit_squash_fetch_and_push_routes() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("jjconfig.toml");
    std::fs::write(&cfg, USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &cfg, &["git", "init"]);
    std::fs::write(repo.join("a.py"), "x = 1\n").unwrap();
    jj(&repo, &cfg, &["commit", "-m", "T0"]);
    jj(&repo, &cfg, &["bookmark", "create", "main", "-r", "@-"]);
    let remote = tmp.path().join("remote.git");
    std::fs::create_dir(&remote).unwrap();
    let git = Command::new("git")
        .args(["init", "-q", "--bare"])
        .current_dir(&remote)
        .status()
        .unwrap();
    assert!(git.success());
    jj(
        &repo,
        &cfg,
        &["git", "remote", "add", "origin", remote.to_str().unwrap()],
    );
    let log = |rev: &str, t: &str| {
        jj(
            &repo,
            &cfg,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                rev,
                "-T",
                t,
            ],
        )
    };

    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
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

        // Commit @ (a file on disk) with main moving to it, then squash a new change into it.
        std::fs::write(repo.join("b.py"), "y = 2\n").unwrap();
        let (_, c) = call(&base, "POST", "/api/v1/graph:op", Some(json!({ "op": { "kind": "commit", "workspace": "default", "message": "T1: b", "bookmark": "main" } }))).await;
        assert_eq!(c["status"], "applied", "{c}");
        assert_eq!(log("main", "description.first_line()"), "T1: b");
        std::fs::write(repo.join("c.py"), "z = 3\n").unwrap();
        let (_, q) = call(&base, "POST", "/api/v1/graph:op", Some(json!({ "op": { "kind": "squash", "source": log("@", "commit_id") } }))).await;
        assert_eq!(q["status"], "applied", "{q}");
        assert!(q["summary"].as_str().unwrap().contains("into its parent"), "{q}");
        assert_eq!(log("main", "description.first_line()"), "T1: b");
        jj(&repo, &cfg, &["file", "show", "-r", "main", "c.py"]);

        // The operation log: newest first, the squash on top; its detail lists what moved.
        let (st, ops) = call(&base, "GET", "/api/v1/ops?limit=5", None).await;
        assert_eq!(st, 200, "{ops}");
        assert_eq!(ops["ops"][0]["op"], ops["current"]);
        assert!(ops["ops"][0]["description"].as_str().unwrap().contains("squash"), "{ops}");
        assert_eq!(ops["truncated"], true);
        let commit_op = ops["ops"].as_array().unwrap().iter().find(|o| o["description"].as_str().unwrap().contains("commit default@")).unwrap().clone();
        let (_, d) = call(&base, "GET", &format!("/api/v1/ops/{}", commit_op["op"].as_str().unwrap()), None).await;
        assert_eq!(d["bookmarks"][0]["name"], "main", "{d}");

        // Restore to before the commit: previewed, refused for agents, applied.
        let before = commit_op["parents"][0].clone();
        let (_, p) = call(&base, "POST", "/api/v1/ops:restore", Some(json!({ "op": before, "dryRun": true }))).await;
        assert_eq!(p["status"], "previewed", "{p}");
        let (st, _) = call_as(&base, "POST", "/api/v1/ops:restore", Some(json!({ "op": before })), Some("claude")).await;
        assert_eq!(st, 403);
        let (_, r) = call(&base, "POST", "/api/v1/ops:restore", Some(json!({ "op": before }))).await;
        assert_eq!(r["status"], "applied", "{r}");
        assert_eq!(log("main", "description.first_line()"), "T0");
        let (_, r) = call(&base, "POST", "/api/v1/ops:restore", Some(json!({ "op": ops["current"] }))).await;
        assert_eq!(r["status"], "applied", "{r}");
        assert_eq!(log("main", "description.first_line()"), "T1: b");

        // Push main (a dry run first), then fetch; agents may not.
        let (_, refs) = call(&base, "GET", "/api/v1/refs", None).await;
        assert_eq!(refs["remotes"], json!(["origin"]), "{refs}");
        let (_, p) = call(&base, "POST", "/api/v1/git:push", Some(json!({ "bookmarks": ["main"], "dryRun": true }))).await;
        assert_eq!(p["status"], "previewed", "{p}");
        assert!(p["output"].as_str().unwrap().contains("main"), "{p}");
        let (st, _) = call_as(&base, "POST", "/api/v1/git:push", Some(json!({ "bookmarks": ["main"] })), Some("claude")).await;
        assert_eq!(st, 403);
        let (_, p) = call(&base, "POST", "/api/v1/git:push", Some(json!({ "bookmarks": ["main"], "remote": "origin" }))).await;
        assert_eq!(p["status"], "applied", "{p}");
        assert!(p["moved"].as_array().unwrap().iter().any(|m| m["name"] == "main@origin"), "{p}");
        let (_, refs) = call(&base, "GET", "/api/v1/refs", None).await;
        let main = refs["bookmarks"].as_array().unwrap().iter().find(|b| b["name"] == "main").unwrap().clone();
        assert_eq!(main["remotes"][0]["ahead"], 0, "{main}");
        assert_eq!(main["remotes"][0]["behind"], 0, "{main}");
        let (_, f) = call(&base, "POST", "/api/v1/git:fetch", Some(json!({}))).await;
        assert_eq!(f["status"], "applied", "{f}");
        let (_, f) = call(&base, "POST", "/api/v1/git:fetch", Some(json!({ "remote": "nope" }))).await;
        assert_eq!(f["status"], "failed", "{f}");
        assert!(f["error"]["message"].as_str().unwrap().contains("no git remote"), "{f}");
    });
}
