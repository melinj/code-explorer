//! Workspaces are first-class (D12) on a scripted repo with a second workspace: unsnapshotted
//! files overlaid live (flag, live blob, text from disk) without snapshotting, alsoChangedIn from
//! another workspace's stack, workspace.updated with real payloads when files are dirtied, agent
//! activity on the cards, and the Compare route. Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use cv_contracts::review::ServerEvent;
use cv_server::index::Options;
use cv_server::routes::{self, AppState};
use cv_server::workspaces::poll_once;
use serde_json::{Value, json};
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

fn op_head(repo: &Path) -> String {
    jj(
        repo,
        &[
            "op",
            "log",
            "-n1",
            "--no-graph",
            "--ignore-working-copy",
            "-T",
            "id",
        ],
    )
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
    let req = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn ok(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let (st, v) = call(app, method, uri, body).await;
    assert!((200..300).contains(&st), "{method} {uri}: {st} {v}");
    v
}

/// Every window of a view (one tile covers the small world).
async fn windows(app: &axum::Router, view: &str) -> Vec<Value> {
    ok(
        app,
        "GET",
        &format!("/api/v1/views/{view}/tiles/8/0/0"),
        None,
    )
    .await["windows"]
        .as_array()
        .unwrap()
        .clone()
}

fn of_path<'a>(ws: &'a [Value], path: &str) -> Vec<&'a Value> {
    ws.iter().filter(|w| w["path"] == path).collect()
}

/// default: init (main) ← s1 (util.py) ← @ (app.py). ws2: a stack on init changing util.py.
fn fixture() -> Option<tempfile::TempDir> {
    if !Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("jj not found; skipping");
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    jj(&repo, &["git", "init"]);
    // 30 functions of 14 lines: each its own window; f{i} returns on line 14·i + 13.
    let util: String = (0..30)
        .map(|i| {
            format!(
                "def f{i}(x):\n{}    return x + {i}\n\n",
                "    x = x * 1\n".repeat(11)
            )
        })
        .collect();
    std::fs::write(repo.join("src/util.py"), &util).unwrap();
    std::fs::write(
        repo.join("src/app.py"),
        "from util import f0\n\n\ndef run():\n    return f0(1)\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/other.py"), "def other():\n    return 0\n").unwrap();
    jj(&repo, &["commit", "-m", "init"]);
    jj(&repo, &["bookmark", "create", "main", "-r", "@-"]);
    jj(
        &repo,
        &[
            "config",
            "set",
            "--repo",
            "revset-aliases.\"trunk()\"",
            "main",
        ],
    );
    std::fs::write(
        repo.join("src/util.py"),
        util.replace("x + 1\n", "x + 100\n"),
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "s1"]);
    std::fs::write(
        repo.join("src/app.py"),
        "from util import f0\n\n\ndef run():\n    return f0(2)\n",
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "s2"]);
    // ws2 on main, changing f25 in util.py (far from s1's f1).
    let ws2 = tmp.path().join("ws2");
    jj(
        &repo,
        &[
            "workspace",
            "add",
            "--name",
            "ws2",
            "-r",
            "main",
            ws2.to_str().unwrap(),
        ],
    );
    std::fs::write(
        ws2.join("src/util.py"),
        util.replace("x + 25\n", "x + 2500\n"),
    )
    .unwrap();
    jj(&ws2, &["describe", "-m", "ws2 work"]);
    Some(tmp)
}

#[tokio::test(flavor = "multi_thread")]
async fn workspaces_are_first_class() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    let app = routes::router(state.clone());

    let view = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "@" })),
    )
    .await;
    let vid = view["id"].as_str().unwrap().to_string();
    let first = poll_once(&state);
    assert!(
        !first
            .iter()
            .any(|e| matches!(e, ServerEvent::WorkspaceUpdated { .. })),
        "the first look only records the cards"
    );

    // alsoChangedIn: ws2's stack changes f25 of util.py; only the window holding it gets the chip.
    let ws = windows(&app, &vid).await;
    let util = of_path(&ws, "src/util.py");
    let chipped: Vec<&&Value> = util
        .iter()
        .filter(|w| w["flags"]["alsoChangedIn"] == json!(["ws2"]))
        .collect();
    assert!(!chipped.is_empty(), "{util:?}");
    let f25 = 25 * 14 + 13; // "    return x + 25"
    assert!(util.len() > 10, "util.py is split into windows");
    assert_eq!(chipped.len(), 1, "{chipped:?}");
    assert!(
        chipped.iter().all(|w| {
            w["lines"]["start"].as_u64().unwrap() <= f25
                && f25 <= w["lines"]["end"].as_u64().unwrap()
        }),
        "only windows with ws2's lines: {chipped:?}"
    );
    assert!(
        of_path(&ws, "src/app.py")
            .iter()
            .all(|w| w["flags"].get("alsoChangedIn").is_none())
    );

    // Overlaps on the cards; ISO times.
    let cards = ok(&app, "GET", "/api/v1/workspaces", None).await;
    let card = |cards: &Value, n: &str| {
        cards
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == n)
            .cloned()
            .unwrap()
    };
    let def = card(&cards, "default");
    assert!(
        def["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["workspace"] == "ws2"
                && o["paths"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("src/util.py"))),
        "{def}"
    );
    assert_eq!(def["unsnapshottedFiles"], 0);

    // An unsnapshotted edit in the default workspace: overlaid live, nothing snapshotted.
    let op0 = op_head(&repo);
    std::fs::write(
        repo.join("src/app.py"),
        "from util import f0\n\n\ndef run():\n    return f0(3)  # live\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/new_file.py"), "x = 1\n").unwrap();
    let evs = poll_once(&state);
    assert_eq!(op_head(&repo), op0, "the overlay never snapshots");
    let app_ids: Vec<String> = of_path(&ws, "src/app.py")
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_string())
        .collect();
    let inv = evs.iter().find_map(|e| match e {
        ServerEvent::ViewInvalidated {
            view_id,
            reason,
            windows,
            ..
        } if *view_id == vid && reason == "index" => windows.clone(),
        _ => None,
    });
    // The edited file's windows, and the never-snapshotted new file's new window (§4.1, D12).
    let inv = inv.expect("view.invalidated for the overlay");
    assert!(app_ids.iter().all(|id| inv.contains(id)), "{evs:?}");
    assert_eq!(inv.len(), app_ids.len() + 1, "{inv:?}");
    let upd = evs.iter().find_map(|e| match e {
        ServerEvent::WorkspaceUpdated { workspace } if workspace["name"] == "default" => {
            Some(workspace.clone())
        }
        _ => None,
    });
    let upd = upd.expect("workspace.updated for the dirtied workspace");
    assert_eq!(upd["unsnapshottedFiles"], 2, "{upd}");
    assert!(
        !upd["overlaps"].as_array().unwrap().is_empty(),
        "real overlaps: {upd}"
    );
    assert!(upd["lastOp"]["time"].as_str().unwrap().ends_with('Z'));

    let ws = windows(&app, &vid).await;
    let live = of_path(&ws, "src/app.py");
    assert!(
        live.iter().all(|w| w["flags"]["unsnapshotted"] == true),
        "{live:?}"
    );
    let blob = live[0]["blob"].as_str().unwrap();
    assert!(blob.starts_with("live"), "{blob}");
    let text = ok(&app, "GET", &format!("/api/v1/blobs/{blob}/text"), None).await;
    assert!(
        text["lines"][4].as_str().unwrap().contains("f0(3)  # live"),
        "{text}"
    );
    assert!(
        of_path(&ws, "src/util.py")
            .iter()
            .all(|w| w["flags"].get("unsnapshotted").is_none())
    );
    // The never-snapshotted file is drawn as a new window: flagged, live text, placed with its
    // directory's cluster, clear of every other window.
    let nf = of_path(&ws, "src/new_file.py");
    assert_eq!(nf.len(), 1, "{ws:?}");
    assert_eq!(nf[0]["flags"]["unsnapshotted"], true);
    assert!(inv.contains(&nf[0]["id"].as_str().unwrap().to_string()));
    let nblob = nf[0]["blob"].as_str().unwrap();
    assert!(nblob.starts_with("live"), "{nblob}");
    let ntext = ok(&app, "GET", &format!("/api/v1/blobs/{nblob}/text"), None).await;
    assert_eq!(ntext["lines"][0], "x = 1", "{ntext}");
    assert_eq!(nf[0]["cluster"], live[0]["cluster"], "next to src/app.py");
    let rect = |w: &Value| {
        let r = &w["rect"];
        (
            r["x"].as_f64().unwrap(),
            r["y"].as_f64().unwrap(),
            r["w"].as_f64().unwrap(),
            r["h"].as_f64().unwrap(),
        )
    };
    let (x, y, w, h) = rect(nf[0]);
    for o in ws.iter().filter(|o| o["id"] != nf[0]["id"]) {
        let (ox, oy, ow, oh) = rect(o);
        assert!(
            x + w <= ox || ox + ow <= x || y + h <= oy || oy + oh <= y,
            "overlaps {}",
            o["path"]
        );
    }
    // An edit on the live text (baseBlob = the live blob) previews like any other.
    let wc = jj(
        &repo,
        &[
            "log",
            "--no-graph",
            "--ignore-working-copy",
            "-r",
            "@",
            "-T",
            "change_id",
        ],
    );
    let p = ok(&app, "POST", "/api/v1/edits", Some(json!({
        "target": wc, "mode": "amend", "dryRun": true,
        "files": [{ "path": "src/app.py", "baseBlob": blob, "patches": [{ "start": 5, "end": 5, "text": "    return f0(4)  # live" }] }],
    }))).await;
    assert_eq!(p["status"], "previewed", "{p}");
    // Nothing changed on disk: no new events.
    assert!(poll_once(&state).is_empty());

    // The file goes back to its snapshot content: the flag and the overlay go away.
    std::fs::write(
        repo.join("src/app.py"),
        "from util import f0\n\n\ndef run():\n    return f0(2)\n",
    )
    .unwrap();
    std::fs::remove_file(repo.join("src/new_file.py")).unwrap();
    let evs = poll_once(&state);
    assert!(
        evs.iter()
            .any(|e| matches!(e, ServerEvent::ViewInvalidated { reason, .. } if reason == "index"))
    );
    let ws = windows(&app, &vid).await;
    assert!(
        of_path(&ws, "src/app.py")
            .iter()
            .all(|w| w["flags"].get("unsnapshotted").is_none()
                && !w["blob"].as_str().unwrap().starts_with("live"))
    );
    assert!(
        of_path(&ws, "src/new_file.py").is_empty(),
        "gone with the file"
    );
    let upd = evs.iter().find_map(|e| match e {
        ServerEvent::WorkspaceUpdated { workspace } if workspace["name"] == "default" => {
            Some(workspace.clone())
        }
        _ => None,
    });
    assert_eq!(upd.expect("card update")["unsnapshottedFiles"], 0);

    // Agent activity on the cards: bound → reading; an edit tool → editing.
    ok(
        &app,
        "POST",
        "/api/v1/agent/bind",
        Some(json!({ "agent": "claude", "workspace": "ws2" })),
    )
    .await;
    let c = card(&ok(&app, "GET", "/api/v1/workspaces", None).await, "ws2");
    assert_eq!(c["agent"]["activity"], "reading", "{c}");
    ok(
        &app,
        "POST",
        "/api/v1/agent/attention",
        Some(json!({ "agent": "claude", "workspace": "ws2", "tool": "edit_code", "windows": [] })),
    )
    .await;
    let evs = poll_once(&state);
    let upd = evs.iter().find_map(|e| match e {
        ServerEvent::WorkspaceUpdated { workspace } if workspace["name"] == "ws2" => {
            Some(workspace.clone())
        }
        _ => None,
    });
    assert_eq!(
        upd.expect("activity change is a card change")["agent"]["activity"],
        "editing"
    );

    // Compare: a per-commit range of both stacks, filtered to the shared files.
    let cmp = ok(
        &app,
        "POST",
        "/api/v1/workspaces:compare",
        Some(json!({ "a": "default", "b": "ws2" })),
    )
    .await;
    assert_eq!(cmp["spec"]["options"]["diff"], "per-commit");
    assert_eq!(cmp["spec"]["options"]["allowNonChain"], true);
    assert_eq!(cmp["spec"]["fileset"], "\"src/util.py\"", "{cmp}");
    let rid = cmp["id"].as_str().unwrap();
    let rw = windows(&app, rid).await;
    assert!(
        !rw.is_empty() && rw.iter().all(|w| w["path"] == "src/util.py"),
        "{rw:?}"
    );
    let (st, e) = call(
        &app,
        "POST",
        "/api/v1/workspaces:compare",
        Some(json!({ "a": "default", "b": "nope" })),
    )
    .await;
    assert_eq!((st, e["error"]["code"].as_str()), (404, Some("not-found")));
    assert_eq!(op_head(&repo), op0, "no operation written");
}

/// D12/D16: a range view's hunk windows carry `alsoChangedIn` when another workspace's stack
/// changes their lines, as snapshot windows do.
#[tokio::test(flavor = "multi_thread")]
async fn range_hunks_carry_also_changed_in() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    // ws3 on main changes f1, the function the default stack's s1 changes.
    let ws3 = tmp.path().join("ws3");
    jj(
        &repo,
        &[
            "workspace",
            "add",
            "--name",
            "ws3",
            "-r",
            "main",
            ws3.to_str().unwrap(),
        ],
    );
    let util = std::fs::read_to_string(ws3.join("src/util.py")).unwrap();
    std::fs::write(ws3.join("src/util.py"), util.replace("x + 1\n", "x + 7\n")).unwrap();
    jj(&ws3, &["describe", "-m", "ws3 work"]);

    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    let app = routes::router(state.clone());
    let range = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "range", "revset": "main..@" })),
    )
    .await;
    let rid = range["id"].as_str().unwrap().to_string();
    let evs = poll_once(&state);
    let inv: Vec<String> = evs
        .iter()
        .filter_map(|e| match e {
            ServerEvent::ViewInvalidated {
                view_id, windows, ..
            } if *view_id == rid => windows.clone(),
            _ => None,
        })
        .flatten()
        .collect();

    let rw = windows(&app, &rid).await;
    let hunks: Vec<&Value> = rw
        .iter()
        .filter(|w| w["kind"] == "hunk" && w["path"] == "src/util.py")
        .collect();
    assert_eq!(hunks.len(), 1, "s1's change to f1: {rw:?}");
    let f1 = 14 + 13; // "    return x + 1"
    let h = hunks[0];
    assert!(
        h["lines"]["start"].as_u64().unwrap() <= f1 && f1 <= h["lines"]["end"].as_u64().unwrap()
    );
    let also = &h["flags"]["alsoChangedIn"];
    assert!(
        also.as_array().unwrap().contains(&json!("ws3")),
        "the hunk ws3 also changes: {h}"
    );
    // ws2 changes f25, outside the hunk; app.py's hunk is changed by nobody else.
    assert!(!also.as_array().unwrap().contains(&json!("ws2")), "{h}");
    assert!(
        inv.contains(&h["id"].as_str().unwrap().to_string()),
        "{evs:?}"
    );
    for w in rw.iter().filter(|w| w["path"] == "src/app.py") {
        assert!(w["flags"].get("alsoChangedIn").is_none(), "{w}");
    }
    // Nothing moved: no new event on the next look.
    assert!(
        !poll_once(&state)
            .iter()
            .any(|e| matches!(e, ServerEvent::ViewInvalidated { view_id, .. } if *view_id == rid))
    );
}

/// D12, DESIGN §6: an operation run outside cv (here the jj CLI in the default workspace)
/// rewrites ws2's `@` under its unsnapshotted edit. The watcher's next look flags ws2's card stale
/// (recovering is ws2's owner's job); after `jj workspace update-stale` in ws2 splits its `@`
/// into divergent commits, the card says divergent and `workspace.recovered` names the sibling
/// that kept the edit.
#[tokio::test(flavor = "multi_thread")]
async fn external_ops_flag_stale_and_divergent_workspaces() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    let ws2 = tmp.path().join("ws2");
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    let app = routes::router(state.clone());
    let card = |cards: &Value, n: &str| {
        cards
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == n)
            .cloned()
            .unwrap()
    };
    poll_once(&state);
    let c = card(&ok(&app, "GET", "/api/v1/workspaces", None).await, "ws2");
    assert!(c.get("stale").is_none(), "{c}");

    // An unsnapshotted edit in ws2, then the CLI in the default workspace rebases ws2@.
    std::fs::write(ws2.join("src/other.py"), "def other():\n    return 42\n").unwrap();
    jj(&repo, &["rebase", "-r", "ws2@", "-d", "@-"]);
    let evs = poll_once(&state);
    let upd = evs
        .iter()
        .find_map(|e| match e {
            ServerEvent::WorkspaceUpdated { workspace } if workspace["name"] == "ws2" => {
                Some(workspace.clone())
            }
            _ => None,
        })
        .expect("workspace.updated for ws2");
    assert_eq!(upd["stale"]["reason"], "working-copy", "{upd}");
    assert!(
        upd["stale"]["note"]
            .as_str()
            .unwrap()
            .contains("jj workspace update-stale"),
        "{upd}"
    );
    // N8: a stale card counts none (its files are not overlaid); the note names ws2's own edit
    // (not the rebase's changes).
    assert_eq!(upd["unsnapshottedFiles"], 0, "{upd}");
    assert!(
        upd["stale"]["note"]
            .as_str()
            .unwrap()
            .contains("(1 file edited there, not shown until then)"),
        "{upd}"
    );
    assert!(
        !evs.iter()
            .any(|e| matches!(e, ServerEvent::WorkspaceRecovered { .. })),
        "recovering a stale workspace is not ours to do"
    );
    assert_eq!(
        std::fs::read_to_string(ws2.join("src/other.py")).unwrap(),
        "def other():\n    return 42\n",
        "ws2's files untouched"
    );

    // ws2's owner recovers: jj splits its @ into divergent commits, one keeping the edit.
    jj(&ws2, &["workspace", "update-stale"]);
    let first = poll_once(&state);
    let second = poll_once(&state);
    let rec: Vec<(String, String)> = first
        .iter()
        .chain(&second)
        .filter_map(|e| match e {
            ServerEvent::WorkspaceRecovered {
                workspace,
                divergent_commit,
                ..
            } => Some((workspace.clone(), divergent_commit.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(rec.len(), 1, "announced once: {first:?} {second:?}");
    assert_eq!(rec[0].0, "ws2");
    let c = card(&ok(&app, "GET", "/api/v1/workspaces", None).await, "ws2");
    assert_eq!(c["stale"]["reason"], "divergent", "{c}");
    assert!(
        c["stale"]["commits"]
            .as_array()
            .unwrap()
            .contains(&json!(rec[0].1)),
        "{c}"
    );
    // The sibling holds the edit.
    let kept = jj(
        &repo,
        &[
            "file",
            "show",
            "--ignore-working-copy",
            "-r",
            &rec[0].1,
            "src/other.py",
        ],
    );
    assert!(kept.contains("return 42"), "{kept}");
    assert!(
        !poll_once(&state)
            .iter()
            .any(|e| matches!(e, ServerEvent::WorkspaceRecovered { .. })),
        "not announced again"
    );
}

/// jj's own staleness rule: a workspace whose `@` was rewritten by another workspace's operation is
/// stale even when the rewrite keeps the tree (here a `jj describe` of ws2@ from the default
/// workspace), and fresh again once it is updated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rewrite_that_keeps_the_tree_still_makes_a_workspace_stale() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    let ws2 = tmp.path().join("ws2");
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    let app = routes::router(state.clone());
    let ws2_card = |cards: Value| {
        cards
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "ws2")
            .cloned()
            .unwrap()
    };
    poll_once(&state);
    let c = ws2_card(ok(&app, "GET", "/api/v1/workspaces", None).await);
    assert!(c.get("stale").is_none(), "fresh before: {c}");

    jj(
        &repo,
        &[
            "describe",
            "-r",
            "ws2@",
            "-m",
            "renamed from the default workspace",
        ],
    );
    poll_once(&state);
    let c = ws2_card(ok(&app, "GET", "/api/v1/workspaces", None).await);
    assert_eq!(
        c["stale"]["reason"], "working-copy",
        "same tree, rewritten @: {c}"
    );

    jj(&ws2, &["workspace", "update-stale"]);
    poll_once(&state);
    let c = ws2_card(ok(&app, "GET", "/api/v1/workspaces", None).await);
    assert!(
        c.get("stale").is_none() || c["stale"].is_null(),
        "fresh after update-stale: {c}"
    );
}
