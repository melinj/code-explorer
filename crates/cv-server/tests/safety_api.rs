//! DESIGN §6 / §10 over HTTP: per-actor attribution from X-CV-Workspace (threads, comments,
//! edits, op descriptions), 409 `undo-blocked` with details after an external rewrite (repo left
//! unchanged), undo gated like other agent edits, per-session agent permissions (403
//! `agent-edit-disabled`, agents cannot change them) and permissions and tours persisted in
//! review.db across a restart. Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

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

async fn call(
    base: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = base.trim_start_matches("http://");
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let extra: String = headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

/// A server on `repo` with review.db under `store`; returns (base URL, review, server task).
async fn serve(
    repo: &Path,
    store: &Path,
) -> (
    String,
    Arc<cv_review::Review>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(repo, true, Some(USER)).unwrap();
    let review =
        cv_review::Review::open_with(writer, Some(cv_store::StorePaths::in_dir(store))).unwrap();
    state.set_review(review.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(axum::serve(listener, routes::router(state)).into_future());
    (format!("http://{addr}"), review, task)
}

fn amend(target: &str, line: u32, text: &str, dry_run: bool) -> Value {
    json!({
        "target": target,
        "files": [{ "path": "a.py", "patches": [{ "start": line, "end": line, "text": text }] }],
        "mode": "amend",
        "dryRun": dry_run,
    })
}

const CLAUDE: [(&str, &str); 3] = [
    ("X-CV-Agent", "claude"),
    ("X-CV-Agent-Client", "claude-code"),
    ("X-CV-Workspace", "gym"),
];

#[test]
fn attribution_undo_block_and_persisted_session_permissions() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    std::fs::write(repo.join("a.py"), &body).unwrap();
    jj(&repo, &["commit", "-m", "A"]);
    std::fs::write(repo.join("b.py"), "x = 1\n").unwrap();
    jj(&repo, &["describe", "-m", "B"]);
    let a = jj(
        &repo,
        &[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            "@-",
            "-T",
            "change_id",
        ],
    );
    let op_head = || {
        jj(
            &repo,
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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (base, _review, task) = serve(&repo, &store).await;

        // ── Attribution: X-CV-Workspace lands on threads, comments, edits and op descriptions.
        let (st, t) = call(&base, "POST", "/api/v1/threads", &CLAUDE, Some(json!({
            "anchor": { "path": "a.py", "rev": a, "start": 1, "end": 1 },
            "body": "first line",
        }))).await;
        assert_eq!(st, 200, "{t}");
        assert_eq!(t["createdBy"], json!({ "kind": "agent", "name": "claude", "client": "claude-code", "workspace": "gym" }));
        assert_eq!(t["comments"][0]["author"]["workspace"], "gym");
        let (_, t2) = call(&base, "POST", &format!("/api/v1/threads/{}/comments", t["id"].as_str().unwrap()), &CLAUDE, Some(json!({ "body": "on it" }))).await;
        assert_eq!(t2["comments"][1]["author"]["workspace"], "gym", "{t2}");
        let (st, e) = call(&base, "POST", "/api/v1/edits", &CLAUDE, Some(amend(&a, 1, "line one", false))).await;
        assert_eq!(st, 200, "{e}");
        assert_eq!(e["status"], "applied", "{e}");
        assert_eq!(e["actor"]["workspace"], "gym");
        let desc = jj(&repo, &["op", "log", "--ignore-working-copy", "--no-graph", "-n1", "-T", "description"]);
        assert!(desc.contains("(claude@gym)"), "{desc}");
        let (_, stored) = call(&base, "GET", &format!("/api/v1/edits/{}", e["id"].as_str().unwrap()), &[], None).await;
        assert_eq!(stored["actor"]["workspace"], "gym");

        // ── Undo after an external rewrite of the amended change: 409, nothing written.
        jj(&repo, &["describe", "-r", &a, "-m", "A, described outside cv"]);
        let op0 = op_head();
        let (st, u) = call(&base, "POST", &format!("/api/v1/edits/{}/undo", e["id"].as_str().unwrap()), &[], None).await;
        assert_eq!(st, 409, "{u}");
        assert_eq!(u["error"]["code"], "undo-blocked", "{u}");
        let d = &u["error"]["details"];
        assert!(d["divergent"].as_array().unwrap().iter().any(|c| c == a.as_str()), "{d}");
        assert!(!d["laterOps"].as_array().unwrap().is_empty(), "{d}");
        assert!(d["laterOps"][0]["changes"].as_array().unwrap().iter().any(|c| c == a.as_str()), "{d}");
        assert!(d["suggestion"].as_str().unwrap().contains("jj op revert"), "{d}");
        assert_eq!(op_head(), op0, "undo-blocked leaves the repo alone");
        let (_, still) = call(&base, "GET", &format!("/api/v1/edits/{}", e["id"].as_str().unwrap()), &[], None).await;
        assert_eq!(still["status"], "applied");

        // ── Per-session permissions. The UI without a session id is "default".
        let (_, p) = call(&base, "GET", "/api/v1/agent/permissions", &[], None).await;
        assert_eq!((p["edit"].clone(), p["moveCamera"].clone(), p["session"].clone()), (json!(true), json!(false), json!("default")));
        let (st, _) = call(&base, "PUT", "/api/v1/agent/permissions", &CLAUDE, Some(json!({ "edit": true }))).await;
        assert_eq!(st, 403, "agents cannot change their own permissions");
        let (st, p) = call(&base, "PUT", "/api/v1/agent/permissions", &[], Some(json!({ "edit": false }))).await;
        assert_eq!(st, 200);
        assert_eq!(p["edit"], false);
        let (st, denied) = call(&base, "POST", "/api/v1/edits", &CLAUDE, Some(amend(&a, 2, "x", true))).await;
        assert_eq!(st, 403, "{denied}");
        assert_eq!(denied["error"]["code"], "agent-edit-disabled");
        let (st, denied) = call(&base, "POST", &format!("/api/v1/edits/{}/undo", e["id"].as_str().unwrap()), &CLAUDE, None).await;
        assert_eq!((st, denied["error"]["code"].clone()), (403, json!("agent-edit-disabled")), "undo is gated too");
        let (st, human) = call(&base, "POST", "/api/v1/edits", &[], Some(amend(&a, 2, "x", true))).await;
        assert_eq!((st, human["status"].clone()), (200, json!("previewed")), "{human}");
        let (_, mine) = call(&base, "GET", "/api/v1/agent/permissions", &CLAUDE, None).await;
        assert_eq!(mine["edit"], false, "{mine}");

        // Another UI session allowing it does not override the session that switched it off;
        // switching it back on in "default" while ui2 turns it off for claude only.
        let ui2 = [("X-CV-Session", "ui2")];
        call(&base, "PUT", "/api/v1/agent/permissions", &ui2, Some(json!({ "edit": true }))).await;
        let (st, _) = call(&base, "POST", "/api/v1/edits", &CLAUDE, Some(amend(&a, 2, "x", true))).await;
        assert_eq!(st, 403);
        call(&base, "PUT", "/api/v1/agent/permissions", &[], Some(json!({ "edit": true }))).await;
        let (st, p) = call(&base, "PUT", "/api/v1/agent/permissions?agent=claude", &ui2, Some(json!({ "edit": false, "moveCamera": true }))).await;
        assert_eq!(st, 200);
        assert_eq!(p, json!({ "edit": false, "moveCamera": true, "session": "ui2", "agent": "claude" }));
        let (st, _) = call(&base, "POST", "/api/v1/edits", &CLAUDE, Some(amend(&a, 2, "x", true))).await;
        assert_eq!(st, 403, "claude is off in ui2");
        let codex = [("X-CV-Agent", "codex")];
        let (st, ok) = call(&base, "POST", "/api/v1/edits", &codex, Some(amend(&a, 2, "x", true))).await;
        assert_eq!((st, ok["status"].clone()), (200, json!("previewed")), "other agents may still edit: {ok}");
        let (_, p) = call(&base, "GET", "/api/v1/agent/permissions?session=ui2", &[], None).await;
        assert_eq!((p["edit"].clone(), p["agent"].clone()), (json!(true), json!("*")), "ui2's toggles for every agent");

        // A tour, to check it survives the restart.
        let (st, tour) = call(&base, "POST", "/api/v1/agent/tours", &CLAUDE, Some(json!({
            "title": "Walk", "steps": [{ "target": { "path": "a.py", "line": 1 }, "lines": [1, 2], "note": "start" }]
        }))).await;
        assert_eq!(st, 200, "{tour}");
        task.abort();
        drop(_review);

        // ── Restart: same review.db.
        let (base, _review, task) = serve(&repo, &store).await;
        let (_, tours) = call(&base, "GET", "/api/v1/tours", &[], None).await;
        assert_eq!(tours[0]["id"], tour["tour"], "{tours}");
        assert_eq!(tours[0]["by"]["workspace"], "gym");
        assert_eq!(tours[0]["steps"][0]["lines"], json!([1, 2]));
        let (st, _) = call(&base, "POST", "/api/v1/edits", &CLAUDE, Some(amend(&a, 2, "x", true))).await;
        assert_eq!(st, 403, "claude stays off in ui2 after a restart");
        let (_, p) = call(&base, "GET", "/api/v1/agent/permissions?agent=claude", &ui2, None).await;
        assert_eq!((p["edit"].clone(), p["moveCamera"].clone()), (json!(false), json!(true)));
        task.abort();
    });
}
