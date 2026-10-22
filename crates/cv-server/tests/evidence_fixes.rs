//! Small fixes from the second design audit (GAPS2 A), each against the real routes on a scripted
//! repo: snapshot views accept revsets with the user's aliases (N6), fragments and the bootstrap
//! include live new files (N7), a workspace whose checkout is still in progress is neither stale
//! nor full of edits (N8), and the tile route's by-reference serialization equals the
//! `TileResponse` it replaces (§9 budget). Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
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

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (u16, Vec<u8>) {
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
    (status, bytes.to_vec())
}

async fn ok(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let (st, b) = call(app, method, uri, body).await;
    let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
    assert!((200..300).contains(&st), "{method} {uri}: {st} {v}");
    v
}

/// A repo with `main` (two commits) as `trunk()` and an empty `@` on top.
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
    let big: String = (0..12)
        .map(|i| {
            format!(
                "def g{i}(x):\n{}    return x\n\n",
                "    x = x + 1\n".repeat(12)
            )
        })
        .collect();
    std::fs::write(repo.join("src/big.py"), &big).unwrap();
    std::fs::write(
        repo.join("src/app.py"),
        "from big import g0\n\n\ndef run():\n    return g0(1)\n",
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "one"]);
    std::fs::write(
        repo.join("src/app.py"),
        "from big import g1\n\n\ndef run():\n    return g1(1)\n",
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "two"]);
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
    Some(tmp)
}

fn state(repo: &Path) -> Arc<AppState> {
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(repo, true, Some(USER)).unwrap();
    state.set_review(cv_review::Review::open_with(writer, None).unwrap());
    state
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_views_resolve_user_aliases_and_serve_live_new_files() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    let state = state(&repo);
    let app = routes::router(state.clone());

    // N6: `trunk()` is the repo's alias (main), as ranges already resolved it.
    let main = jj(
        &repo,
        &["log", "--no-graph", "-r", "main", "-T", "commit_id"],
    );
    let v = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "trunk()" })),
    )
    .await;
    assert_eq!(v["resolved"]["head"], json!(main.trim()), "{v}");
    let (st, bad) = call(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "nope()" })),
    )
    .await;
    let bad: Value = serde_json::from_slice(&bad).unwrap();
    assert_eq!(st, 400, "{bad}");
    assert!(bad.to_string().contains("nope"), "{bad}");
    let (st, many) = call(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "::main" })),
    )
    .await;
    assert_eq!(st, 400, "{}", String::from_utf8_lossy(&many));

    // §9: the tile route serializes headers by reference; the body is the TileResponse.
    let at = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "@" })),
    )
    .await;
    let vid = at["id"].as_str().unwrap().to_string();
    let (st, body) = call(
        &app,
        "GET",
        &format!("/api/v1/views/{vid}/tiles/8/0/0"),
        None,
    )
    .await;
    assert_eq!(st, 200);
    let view = state
        .view_for_spec(&json!({ "mode": "snapshot", "rev": "@" }))
        .unwrap();
    assert_eq!(view.id(), vid);
    let want = serde_json::to_vec(&routes::tile_response(&view, 8, 0, 0)).unwrap();
    assert_eq!(
        body, want,
        "tile bytes equal the TileResponse serialization"
    );

    // N7: a new file nobody snapshotted gets windows in the live overlay (§4.1); fragments and
    // the bootstrap serve them as the tiles do.
    let fresh: String = (0..10)
        .map(|i| {
            format!(
                "def h{i}(x):\n{}    return x\n\n",
                "    x = x * 2\n".repeat(13)
            )
        })
        .collect();
    std::fs::write(repo.join("src/fresh.py"), &fresh).unwrap();
    poll_once(&state);
    let tile = ok(
        &app,
        "GET",
        &format!("/api/v1/views/{vid}/tiles/8/0/0"),
        None,
    )
    .await;
    let fresh_wins: Vec<&Value> = tile["windows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["path"] == "src/fresh.py")
        .collect();
    assert!(
        fresh_wins.len() > 1,
        "the live file is split into windows: {fresh_wins:?}"
    );
    let frags = ok(
        &app,
        "GET",
        &format!("/api/v1/views/{vid}/fragments?path=src/fresh.py"),
        None,
    )
    .await;
    assert_eq!(
        frags["fragments"].as_array().unwrap().len(),
        fresh_wins.len(),
        "{frags}"
    );
    let boot = ok(&app, "GET", &format!("/api/v1/views/{vid}/bootstrap"), None).await;
    let clusters: Vec<&str> = boot["clusters"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    for w in &fresh_wins {
        assert!(
            clusters.contains(&w["cluster"].as_str().unwrap()),
            "bootstrap has {w}'s cluster"
        );
    }
    // The pure view (no live overlay) does not know the file.
    assert!(!view.by_path.contains_key("src/fresh.py"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_still_checking_out_is_not_stale() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    // `jj workspace add` records the new `@` before it writes the files; meanwhile the tree state
    // is the empty tree with no files. Reproduce that moment: a workspace at root() has exactly
    // that tree state; copy it over a fully checked-out workspace at main.
    let wsa = tmp.path().join("wsa");
    let empty = tmp.path().join("empty");
    jj(
        &repo,
        &[
            "workspace",
            "add",
            "--name",
            "wsa",
            "-r",
            "main",
            wsa.to_str().unwrap(),
        ],
    );
    jj(
        &repo,
        &[
            "workspace",
            "add",
            "--name",
            "empty",
            "-r",
            "root()",
            empty.to_str().unwrap(),
        ],
    );
    let ts = |d: &Path| d.join(".jj/working_copy/tree_state");
    std::fs::copy(ts(&empty), ts(&wsa)).unwrap();

    let r = cv_vcs::JjRepo::open(&repo).unwrap();
    let states = r.workspaces(&r.revset_env());
    let w = states.iter().find(|w| w.name == "wsa").unwrap();
    assert!(!w.stale, "checking out is not stale: {w:?}");
    assert_eq!(
        w.unsnapshotted, None,
        "files being written are not edits: {w:?}"
    );

    let state = state(&repo);
    let app = routes::router(state.clone());
    let cards = ok(&app, "GET", "/api/v1/workspaces", None).await;
    let card = cards
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "wsa")
        .unwrap()
        .clone();
    assert!(card.get("stale").is_none(), "{card}");
    assert_eq!(card["unsnapshottedFiles"], 0, "{card}");
}
