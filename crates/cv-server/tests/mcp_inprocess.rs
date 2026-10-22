//! The MCP tools on `cv serve`'s in-process backend and on the HTTP backend (the `cv mcp` shim)
//! against the same server: an agent edit is attributed to the agent, and every read after it —
//! read_code by the agent's view, by `@`, by change id and by the new commit id, read_window with
//! a window id from before the edit, get_thread — sees the edit under the same view and window
//! ids. An external `jj` command is seen by the next read too. Skipped when `jj` is not on PATH.

use std::future::IntoFuture;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use cv_mcp::{CvMcp, McpBackend, McpConfig};
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

/// (first content block as JSON, all text) of a tool call; panics on a tool error.
async fn tool(mcp: &CvMcp, name: &str, args: Value) -> (Value, String) {
    let r = mcp.call(name, args, None).await;
    let texts: Vec<String> = r
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect();
    assert_ne!(r.is_error, Some(true), "{name}: {}", texts.join("\n"));
    let first = serde_json::from_str(&texts[0]).unwrap_or(Value::Null);
    (first, texts.join("\n"))
}

fn config() -> McpConfig {
    McpConfig {
        agent: Some("claude".into()),
        profile: cv_mcp::Profile::Full,
        ..Default::default()
    }
}

#[test]
fn reads_after_writes_are_fresh_in_process_and_over_http() {
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
    jj(&repo, &["commit", "-m", "base"]);
    std::fs::write(repo.join("src/a.py"), body.replace("x + 3", "x + 30")).unwrap();
    jj(&repo, &["describe", "-m", "work"]);
    let change = jj(
        &repo,
        &[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "change_id",
            "--ignore-working-copy",
        ],
    );

    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    // The startup view, as `cv serve` builds it.
    let startup = state
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, routes::router(state.clone())).into_future());
        let inproc: Arc<dyn McpBackend> = Arc::new(cv_server::mcp::InProcess::new(state.clone()));
        let http: Arc<dyn McpBackend> = Arc::new(cv_mcp::http::HttpBackend::new(
            &format!("http://{addr}"),
            None,
        ));
        let a = CvMcp::new(inproc, config());
        let h = CvMcp::new(http, config());

        let (b, _) = tool(&a, "bind_workspace", json!({ "workspace": "default" })).await;
        assert_eq!(b["bound"], "default");
        tool(&h, "bind_workspace", json!({ "workspace": "default" })).await;
        let (before, _) = tool(
            &a,
            "read_code",
            json!({ "path": "src/a.py", "start": 1, "end": 16 }),
        )
        .await;
        let window = before["windows"][0].as_str().unwrap().to_string();
        let blob = before["blob"].as_str().unwrap().to_string();

        // The agent's edit, through the in-process backend: attributed to the agent.
        let (e, et) = tool(
            &a,
            "edit_code",
            json!({
                "rev": "@", "path": "src/a.py", "baseBlob": blob,
                "patches": [{ "start": 2, "end": 2, "text": "    y_value = x + 0  # offset" }],
            }),
        )
        .await;
        assert_eq!(e["status"], "applied", "{et}");
        // Slim for the agent: no actor or op ids; the new blob for a follow-up edit.
        assert!(
            e.get("actor").is_none() && e.get("opBefore").is_none(),
            "{et}"
        );
        let new_blob = e["files"][0]["newBlob"].as_str().unwrap().to_string();
        assert_eq!(e["files"][0]["path"], "src/a.py");
        assert_ne!(new_blob, blob);
        let stored = review.get_edit(e["id"].as_str().unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&stored.actor).unwrap(),
            json!({ "kind": "agent", "name": "claude", "workspace": "default" })
        );
        assert_eq!(stored.files[0].new_blob.as_deref(), Some(new_blob.as_str()));
        let commit = jj(
            &repo,
            &[
                "log",
                "--no-graph",
                "-r",
                "@",
                "-T",
                "commit_id",
                "--ignore-working-copy",
            ],
        );

        for (label, mcp) in [("in-process", &a), ("http", &h)] {
            for args in [
                json!({ "path": "src/a.py", "start": 2, "end": 2 }),
                json!({ "path": "src/a.py", "start": 2, "end": 2, "rev": "@" }),
                json!({ "path": "src/a.py", "start": 2, "end": 2, "rev": change }),
                json!({ "path": "src/a.py", "start": 2, "end": 2, "rev": commit }),
            ] {
                let (c, text) = tool(mcp, "read_code", args.clone()).await;
                assert!(text.contains("y_value"), "{label} read_code {args}: {text}");
                assert_eq!(c["commit"], commit.as_str(), "{label} read_code {args}");
            }
            let (_, text) = tool(mcp, "read_window", json!({ "window": window })).await;
            assert!(
                text.contains("y_value"),
                "{label} read_window {window}: {text}"
            );
            let (revs, _) = tool(mcp, "list_revisions", json!({ "revset": "@" })).await;
            assert_eq!(revs[0]["commit"], commit.as_str(), "{label} list_revisions");
        }
        // The snapshot view at @ followed @: same view id as at startup, no re-index.
        let v = state
            .view_for_spec(&json!({ "mode": "snapshot", "rev": "@" }))
            .unwrap();
        assert_eq!(v.id(), startup);
        assert_eq!(v.bootstrap.base.view.resolved.head, commit);

        // A thread reply through the in-process backend is the agent's.
        let t = review
            .create_thread(
                serde_json::from_value(json!({
                    "anchor": { "path": "src/a.py", "rev": change, "start": 6, "end": 6 },
                    "body": "why 1?", "assignee": "agent",
                }))
                .unwrap(),
                &cv_contracts::review::Actor::human("you"),
            )
            .unwrap();
        let (g, gtext) = tool(
            &a,
            "get_thread",
            json!({ "thread": t.id, "contextLines": 5 }),
        )
        .await;
        assert!(
            gtext.contains("y_value"),
            "get_thread code is current: {gtext}"
        );
        assert_eq!(g["id"], t.id.as_str());
        let (r, _) = tool(
            &a,
            "reply",
            json!({ "thread": t.id, "body": "it is", "setStatus": "addressed" }),
        )
        .await;
        assert_eq!(
            r,
            json!({ "id": t.id, "status": "addressed", "comments": 2 })
        );
        let stored = review.get_thread(&t.id).unwrap();
        assert_eq!(
            serde_json::to_value(&stored.comments[1].author).unwrap(),
            json!({ "kind": "agent", "name": "claude", "workspace": "default" })
        );

        // An external jj command (no watcher running): the next read catches up.
        std::fs::write(
            repo.join("src/a.py"),
            std::fs::read_to_string(repo.join("src/a.py"))
                .unwrap()
                .replace("return y\n\ndef f1", "return 7\n\ndef f1"),
        )
        .unwrap();
        jj(&repo, &["status"]);
        for (label, mcp) in [("in-process", &a), ("http", &h)] {
            let (_, text) = tool(
                mcp,
                "read_code",
                json!({ "path": "src/a.py", "start": 3, "end": 3 }),
            )
            .await;
            assert!(
                text.contains("return 7"),
                "{label} after external op: {text}"
            );
        }
    });
}

/// (is_error, all text) of a tool call.
async fn try_tool(mcp: &CvMcp, name: &str, args: Value) -> (bool, String) {
    let r = mcp.call(name, args, None).await;
    let texts: Vec<String> = r
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect();
    (r.is_error == Some(true), texts.join("\n"))
}

/// The review findings on `cv serve`'s in-process backend: get_diff prints text with real file
/// statuses, qualnames resolve through the symbol index, get_links on a line stays in its symbol,
/// rejected edits are tool errors, the default mode branches immutable targets, and a thread is
/// addressed with list_threads {withCode} + one edit_code {thread, reply} — without a bind.
#[test]
fn review_findings_in_process() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let jjcfg = format!(
        "{USER}[revset-aliases]\n\"immutable_heads()\" = \"description(substring:base)\"\n"
    );
    std::fs::write(tmp.path().join("jjconfig.toml"), &jjcfg).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    jj(&repo, &["git", "init"]);
    let mut body = String::new();
    for i in 0..4 {
        body.push_str(&format!("def f{i}(x):\n    y = x + {i}\n    return y\n\n"));
    }
    std::fs::write(repo.join("src/a.py"), &body).unwrap();
    std::fs::write(
        repo.join("src/b.py"),
        "from a import f1\n\n\ndef g(x):\n    return f1(x)\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/c.py"),
        "from a import f2\n\n\ndef h(x):\n    return f2(x)\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/gone.py"), "def old():\n    return 1\n").unwrap();
    jj(&repo, &["commit", "-m", "base"]);
    std::fs::write(repo.join("src/a.py"), body.replace("x + 3", "x + 30")).unwrap();
    std::fs::write(repo.join("src/new.py"), "def fresh():\n    return 2\n").unwrap();
    std::fs::remove_file(repo.join("src/gone.py")).unwrap();
    jj(&repo, &["describe", "-m", "work"]);
    let log = |rev: &str, t: &str| {
        jj(
            &repo,
            &[
                "log",
                "--no-graph",
                "-r",
                rev,
                "-T",
                t,
                "--ignore-working-copy",
            ],
        )
    };
    let base_change = log("@-", "change_id");

    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(&jjcfg)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    state
        .view_for_spec(&json!({ "mode": "snapshot", "rev": "@" }))
        .unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let backend: Arc<dyn McpBackend> = Arc::new(cv_server::mcp::InProcess::new(state.clone()));
        let a = CvMcp::new(backend, config());

        // get_diff: unified text with every line's text and the real status of each file.
        let (files, text) = tool(&a, "get_diff", json!({ "revset": "@" })).await;
        let status = |p: &str| {
            files
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["path"] == p)
                .map(|f| f["status"].clone())
        };
        assert_eq!(status("src/a.py"), Some(json!("modified")), "{text}");
        assert_eq!(status("src/new.py"), Some(json!("added")), "{text}");
        assert_eq!(status("src/gone.py"), Some(json!("removed")), "{text}");
        assert!(text.contains("+    y = x + 30") && text.contains("-    y = x + 3\n"), "{text}");
        assert!(text.contains("+def fresh():") && text.contains("-def old():"), "{text}");
        assert!(text.contains(" def f3(x):"), "context lines carry text: {text}");
        let (_, text) = tool(&a, "get_diff", json!({ "revset": "@", "fileset": "src/new.py" })).await;
        assert!(!text.contains("a.py"), "{text}");
        // jj fileset syntax.
        let (f, text) = tool(&a, "get_diff", json!({ "revset": "@", "fileset": "glob:\"src/*.py\" ~ src/a.py" })).await;
        assert_eq!(f.as_array().unwrap().len(), 2, "{text}");

        // {from, to}: the trees of two revisions (jj diff --from --to), not the range from..to
        // (which is empty here: @- is an ancestor of @). Reversed, so additions are removals.
        let (files, text) = tool(&a, "get_diff", json!({ "from": "@", "to": "@-" })).await;
        let status = |p: &str| {
            files
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["path"] == p)
                .map(|f| f["status"].clone())
        };
        assert_eq!(status("src/new.py"), Some(json!("removed")), "{text}");
        assert_eq!(status("src/gone.py"), Some(json!("added")), "{text}");
        assert!(text.contains("-    y = x + 30") && text.contains("+    y = x + 3\n"), "{text}");
        // A missing side is @: from @- alone equals the change's own diff.
        let (_, t2) = tool(&a, "get_diff", json!({ "from": "@-" })).await;
        let (_, t3) = tool(&a, "get_diff", json!({ "revset": "@" })).await;
        assert_eq!(t2, t3);
        let r = a.call("get_diff", json!({ "revset": "@", "from": "@-" }), None).await;
        assert_eq!(r.is_error, Some(true), "revset and from/to together");
        let r = a.call("get_diff", json!({ "from": "all()", "to": "@" }), None).await;
        assert_eq!(r.is_error, Some(true), "from must be one commit");

        // The same through `cv mcp` (HttpBackend → GET /api/v1/diff on a served router).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, routes::router(state.clone())).into_future());
        let http: Arc<dyn McpBackend> = Arc::new(cv_mcp::http::HttpBackend::new(&format!("http://{addr}"), None));
        let h = CvMcp::new(http, config());
        let (hf, ht) = tool(&h, "get_diff", json!({ "from": "@", "to": "@-" })).await;
        let (inf, int) = tool(&a, "get_diff", json!({ "from": "@", "to": "@-" })).await;
        assert_eq!((hf, ht), (inf, int), "HTTP backend == in-process");
        let (_, ht) = tool(&h, "get_diff", json!({ "from": "@", "to": "@-", "fileset": "src/new.py" })).await;
        assert!(ht.contains("src/new.py") && !ht.contains("a.py"), "{ht}");
        let r = h.call("get_diff", json!({ "from": "all()", "to": "@" }), None).await;
        assert_eq!(r.is_error, Some(true), "400 from GET /diff is a tool error");

        // Qualnames resolve through the symbol index (get_metrics, focus, get_links).
        tool(&a, "get_metrics", json!({ "target": "f2" })).await;
        let (all_links, _) = tool(&a, "get_links", json!({ "target": "src/a.py", "dir": "in" })).await;
        let (l1, t1) = tool(&a, "get_links", json!({ "target": "f1", "dir": "in" })).await;
        assert_eq!(l1["span"], "src/a.py:5-8", "{t1}");
        // get_links on a line: only the symbol there, not the whole window.
        let (l5, t5) = tool(&a, "get_links", json!({ "target": "src/a.py:5", "dir": "in" })).await;
        assert_eq!(l5["links"], l1["links"], "{t5}");
        assert!(!l5["links"].as_array().unwrap().is_empty(), "b.g calls f1: {t5}");
        assert!(
            all_links["links"].as_array().unwrap().len() > l5["links"].as_array().unwrap().len(),
            "the file has more links than f1: {all_links}"
        );
        for l in l5["links"].as_array().unwrap() {
            let line: u32 = l["to"].as_str().unwrap().rsplit(':').next().unwrap().parse().unwrap();
            assert!((5..=8).contains(&line), "{t5} vs all {all_links}");
        }

        // A thread, addressed without a bind: list_threads {withCode} → one edit_code {thread, reply}.
        let change = log("@", "change_id");
        let t = review
            .create_thread(
                serde_json::from_value(json!({
                    "anchor": { "path": "src/a.py", "rev": change, "start": 10, "end": 10 },
                    "body": "Name y for what it is.", "assignee": "agent", "kind": "task",
                }))
                .unwrap(),
                &cv_contracts::review::Actor::human("you"),
            )
            .unwrap();
        let (meta, text) = tool(
            &a,
            "list_threads",
            json!({ "assignee": "agent", "status": ["open"], "withCode": true }),
        )
        .await;
        assert_eq!(meta["id"], t.id.as_str());
        assert_eq!(meta["lines"], json!([9, 12]), "the enclosing def f2: {text}");
        assert!(text.contains("  10│     y = x + 2"), "{text}");
        let (e, et) = tool(
            &a,
            "edit_code",
            json!({ "thread": t.id, "patches": [{ "start": 10, "end": 11, "text": "    total = x + 2\n    return total" }],
                    "reply": "Renamed y to total." }),
        )
        .await;
        assert_eq!(e["status"], "applied", "{et}");
        assert_eq!(e["thread"], json!({ "id": t.id, "status": "addressed", "comments": 2 }));
        let (c, ctext) = tool(&a, "read_code", json!({ "path": "src/a.py", "start": 9, "end": 12 })).await;
        assert!(ctext.contains("total = x + 2"), "{ctext}");
        assert_eq!(c["blob"], e["files"][0]["newBlob"], "{et}");
        let stored = review.get_thread(&t.id).unwrap();
        assert_eq!(stored.status, "addressed");
        assert_eq!(stored.produced_edits.len(), 1);

        // A stale base that does not overlap the change since is rebased (three-way merge)…
        let (e2, e2t) = tool(
            &a,
            "edit_code",
            json!({ "rev": "@", "path": "src/a.py", "baseBlob": meta["blob"],
                    "patches": [{ "start": 1, "end": 1, "text": "def f0(z):" }] }),
        )
        .await;
        assert_eq!(e2["status"], "applied", "{e2t}");
        // …one that overlaps is rejected: a tool error with a one-line, actionable message.
        let (is_err, msg) = try_tool(
            &a,
            "edit_code",
            json!({ "rev": "@", "path": "src/a.py", "baseBlob": meta["blob"],
                    "patches": [{ "start": 10, "end": 10, "text": "    y = x + 20" }] }),
        )
        .await;
        assert!(is_err && msg.starts_with("stale-base: re-read"), "{msg}");
        assert!(!msg.contains('\n'), "one line: {msg}");

        // The default mode branches an immutable target; an explicit amend is refused.
        let (base, _) = tool(&a, "read_code", json!({ "path": "src/b.py", "rev": base_change })).await;
        let edit = |mode: Option<&str>| {
            let mut args = json!({ "rev": base_change, "path": "src/b.py", "baseBlob": base["blob"],
                                   "patches": [{ "start": 1, "end": 1, "text": "from a import f1  # base" }] });
            if let Some(m) = mode {
                args["mode"] = json!(m);
            }
            args
        };
        let (is_err, msg) = try_tool(&a, "edit_code", edit(Some("amend"))).await;
        assert!(is_err && msg.starts_with("immutable: ") && msg.contains("branch"), "{msg}");
        let (b, bt) = tool(&a, "edit_code", edit(None)).await;
        assert_eq!(b["status"], "applied", "{bt}");
        assert!(b["created"].is_string(), "branched: {bt}");
    });
}

/// The agent loop's edges on `cv serve` (in process and over HTTP):
/// - b13: edit_code {thread, dryRun, reply} then apply_edit posts the reply and addresses the thread;
/// - a1: an answer of several patches leaves the thread addressed (not outdated) on the new text,
///   also after the next jj operations;
/// - b1: edit_code creates a new file without a thread or baseBlob (a path with a space, both
///   backends), but an existing file still needs a baseBlob;
/// - c8: get_diff with a fileset naming a path with spaces, end to end over HTTP;
/// - c10: list_conflicts returns short ids that get_conflict takes back (also over HTTP, for a
///   path with a space: the conflict URL is encoded); get_diff marks a file conflicted on either side;
/// - c8: wait_for_events on an expired cursor answers resync (both backends).
#[test]
fn agent_answers_previews_creates_files_and_resyncs() {
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
    std::fs::write(repo.join("src/c d.py"), "C = 1\n").unwrap();
    jj(&repo, &["commit", "-m", "base"]);
    let log = |rev: &str, t: &str| {
        jj(
            &repo,
            &[
                "log",
                "--no-graph",
                "-r",
                rev,
                "-T",
                t,
                "--ignore-working-copy",
            ],
        )
    };
    let base = log("@-", "change_id");
    // A conflict in "src/c d.py" (a space: conflict URLs are encoded end to end): "work" and
    // "other" both rewrite its line; work goes onto other.
    std::fs::write(repo.join("src/c d.py"), "C = 2\n").unwrap();
    jj(&repo, &["describe", "-m", "work"]);
    let work = log("@", "change_id");
    jj(&repo, &["new", &base, "-m", "other"]);
    std::fs::write(repo.join("src/c d.py"), "C = 3\n").unwrap();
    jj(&repo, &["rebase", "-s", &work, "-d", "@"]);
    jj(&repo, &["new", &work, "-m", "top"]);
    let top = log("@", "change_id");

    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    state
        .view_for_spec(&json!({ "mode": "snapshot", "rev": "@" }))
        .unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, routes::router(state.clone())).into_future());
        let inproc: Arc<dyn McpBackend> = Arc::new(cv_server::mcp::InProcess::new(state.clone()));
        let http: Arc<dyn McpBackend> = Arc::new(cv_mcp::http::HttpBackend::new(
            &format!("http://{addr}"),
            None,
        ));
        let a = CvMcp::new(inproc, config());
        let h = CvMcp::new(http, config());
        tool(&a, "bind_workspace", json!({ "workspace": "default" })).await;
        tool(&h, "bind_workspace", json!({ "workspace": "default" })).await;

        // c10: short ids, as the other tools return; get_conflict takes them back.
        let (cs, ct) = tool(&a, "list_conflicts", json!({ "revset": "mutable()" })).await;
        let cs = cs.as_array().unwrap().clone();
        assert!(!cs.is_empty(), "{ct}");
        for c in &cs {
            assert_eq!(c["change"].as_str().unwrap().len(), 12, "{ct}");
            assert_eq!(c["commit"].as_str().unwrap().len(), 12, "{ct}");
            assert_eq!(c["path"], "src/c d.py");
        }
        let w = cs
            .iter()
            .find(|c| work.starts_with(c["change"].as_str().unwrap()))
            .expect("work is conflicted");
        let (d, dt) = tool(
            &a,
            "get_conflict",
            json!({ "rev": w["change"], "path": "src/c d.py" }),
        )
        .await;
        assert!(
            d["materialized"].as_str().unwrap().contains("<<<<<<<"),
            "{dt}"
        );
        let (hd, _) = tool(
            &h,
            "get_conflict",
            json!({ "rev": w["commit"], "path": "src/c d.py" }),
        )
        .await;
        assert_eq!(hd["materialized"], d["materialized"]);

        // c10: file_diffs checks conflicts on both sides: from the conflicted work to base, c.py
        // is a conflicted modification (no hunks), not an added file.
        for (from, to) in [
            (work.as_str(), base.as_str()),
            (base.as_str(), work.as_str()),
        ] {
            let (f, ft) = tool(
                &a,
                "get_diff",
                json!({ "from": from, "to": to, "fileset": "\"src/c d.py\"" }),
            )
            .await;
            let c = &f.as_array().unwrap()[0];
            assert_eq!(
                (c["status"].as_str(), c["conflicted"].as_bool()),
                (Some("modified"), Some(true)),
                "{from}→{to}: {ft}"
            );
        }

        // b13 + a1: a thread on f2's body, answered by a previewed edit of three patches.
        let t = review
            .create_thread(
                serde_json::from_value(json!({
                    "anchor": { "path": "src/a.py", "rev": top, "start": 10, "end": 11 },
                    "body": "Name y for what it is.", "assignee": "agent", "kind": "task",
                }))
                .unwrap(),
                &cv_contracts::review::Actor::human("you"),
            )
            .unwrap();
        tool(
            &a,
            "list_threads",
            json!({ "assignee": "agent", "withCode": true }),
        )
        .await;
        let (p, pt) = tool(
            &a,
            "edit_code",
            json!({ "thread": t.id, "dryRun": true, "reply": "Renamed y to total; documented f0.",
                    "patches": [
                        { "start": 1, "end": 0, "text": "\"\"\"Adders.\"\"\"" },
                        { "start": 10, "end": 11, "text": "    total = x + 2\n    return total" },
                        { "start": 14, "end": 14, "text": "    y = x + 3  # three" },
                    ] }),
        )
        .await;
        assert_eq!(p["status"], "previewed", "{pt}");
        assert!(
            p.get("thread").is_none(),
            "no answer before the apply: {pt}"
        );
        assert_eq!(review.get_thread(&t.id).unwrap().status, "open");
        let (e, et) = tool(&a, "apply_edit", json!({ "edit": p["id"] })).await;
        assert_eq!(e["status"], "applied", "{et}");
        assert_eq!(
            e["thread"]["status"], "addressed",
            "the previewed reply was posted: {et}"
        );
        let th = review.get_thread(&t.id).unwrap();
        assert_eq!(th.status, "addressed");
        assert_eq!(th.comments.len(), 2);
        assert!(th.outdated.is_none(), "{th:?}");
        let anchored = |th: &cv_contracts::review::Thread| {
            let out = jj(
                &repo,
                &[
                    "file",
                    "show",
                    "--ignore-working-copy",
                    "-r",
                    &th.anchor.commit,
                    "src/a.py",
                ],
            );
            out.lines()
                .skip(th.anchor.start as usize - 1)
                .take((th.anchor.end - th.anchor.start + 1) as usize)
                .map(String::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(anchored(&th), vec!["    total = x + 2", "    return total"]);
        // The next operations: a working-copy snapshot and a describe. Still addressed.
        std::fs::write(repo.join("README"), "hi\n").unwrap();
        jj(&repo, &["status"]);
        review.sync().unwrap();
        jj(&repo, &["describe", "-m", "top, described"]);
        review.sync().unwrap();
        let th2 = review.get_thread(&t.id).unwrap();
        assert_eq!(th2.status, "addressed", "{th2:?}");
        assert!(th2.outdated.is_none());
        assert_ne!(th2.anchor.commit, th.anchor.commit);
        assert_eq!(
            anchored(&th2),
            vec!["    total = x + 2", "    return total"]
        );
        // apply_edit of the same edit again answers nothing twice.
        let (again, at) = tool(&a, "apply_edit", json!({ "edit": p["id"] })).await;
        assert!(again.get("thread").is_none(), "{at}");
        assert_eq!(review.get_thread(&t.id).unwrap().comments.len(), 2);

        // b1: a new file, no thread and no baseBlob (rev defaults to the workspace's @).
        let (n, nt) = tool(
            &a,
            "edit_code",
            json!({ "path": "src/new mod.py", "content": "def extracted():\n    return 1\n" }),
        )
        .await;
        assert_eq!(n["status"], "applied", "{nt}");
        let (n2, n2t) = tool(
            &h,
            "edit_code",
            json!({ "path": "src/other new.py", "content": "X = 1\n" }),
        )
        .await;
        assert_eq!(n2["status"], "applied", "over HTTP: {n2t}");
        assert_eq!(
            std::fs::read_to_string(repo.join("src/new mod.py")).unwrap(),
            "def extracted():\n    return 1\n"
        );
        // An existing file still needs the blob of the code the agent read.
        let (is_err, msg) = try_tool(
            &a,
            "edit_code",
            json!({ "path": "src/c d.py", "rev": base, "content": "C = 9\n" }),
        )
        .await;
        assert!(is_err && msg.contains("baseBlob"), "{msg}");

        // c8: a fileset naming a path with a space, end to end over HTTP (GET /diff?fileset=…).
        let (f, ft) = tool(
            &h,
            "get_diff",
            json!({ "from": "@-", "to": "@", "fileset": "\"src/new mod.py\"" }),
        )
        .await;
        let paths: Vec<&str> = f
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["src/new mod.py"], "{ft}");
        assert!(ft.contains("+def extracted():"), "{ft}");
        let (fi, fit) = tool(
            &a,
            "get_diff",
            json!({ "from": "@-", "to": "@", "fileset": "\"src/new mod.py\"" }),
        )
        .await;
        assert_eq!((fi, fit), (f, ft), "in process == HTTP");

        // c8: an expired cursor (the log was pruned past it) answers resync on both backends.
        review
            .db()
            .prune_events(2, 2, "9999-01-01T00:00:00.000Z")
            .unwrap();
        review.events().prune();
        for m in [&a, &h] {
            let (ev, evt) = tool(
                m,
                "wait_for_events",
                json!({ "cursor": "1", "timeoutS": 1 }),
            )
            .await;
            assert_eq!(ev["resync"], true, "{evt}");
            // The cursor to continue from is a kept one (background indexing may add events).
            let head = ev["cursor"].as_str().unwrap().to_string();
            assert!(head.parse::<u64>().unwrap() > 2, "{evt}");
            let (ev, evt) = tool(
                m,
                "wait_for_events",
                json!({ "cursor": head, "timeoutS": 1 }),
            )
            .await;
            assert!(ev.get("resync").is_none(), "{evt}");
        }
    });
}
