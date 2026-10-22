//! Snapshot specs mean what they say (model.ts SnapshotOptions, DESIGN §7): `fileset`,
//! `liveWorkingCopy: false` and `workspace` each give a view of their own (never the plain `@` view
//! at the same commit), and bad values are 400s. Also: overlap warnings with no remote (trunk() is
//! root()), thread hits in search, range options that are 400s instead of defaults, ETags on
//! GET /views/:id, and `cv serve --help`. Skipped when `jj` is not on PATH.

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

fn have_jj() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

async fn send(app: &axum::Router, req: Request<Body>) -> (u16, axum::http::HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
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
    let (st, _, v) = send(app, req).await;
    (st, v)
}

async fn ok(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let (st, v) = call(app, method, uri, body).await;
    assert!((200..300).contains(&st), "{method} {uri}: {st} {v}");
    v
}

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

/// No remote and no trunk() alias: trunk() is root(), so every stack holds all of history.
/// default: base (a.py, b.py, c.py) ← @ (edits a.py). ws2: on base, edits b.py.
fn fixture() -> Option<tempfile::TempDir> {
    if !have_jj() {
        eprintln!("jj not found; skipping");
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    jj(&repo, &["git", "init"]);
    for f in ["a", "b", "c"] {
        std::fs::write(
            repo.join(format!("src/{f}.py")),
            format!("def {f}(x):\n    return x + 1\n"),
        )
        .unwrap();
    }
    jj(&repo, &["commit", "-m", "base"]);
    jj(&repo, &["bookmark", "create", "base", "-r", "@-"]);
    std::fs::write(repo.join("src/a.py"), "def a(x):\n    return x + 2\n").unwrap();
    jj(&repo, &["describe", "-m", "default work on a"]);
    let ws2 = tmp.path().join("ws2");
    jj(
        &repo,
        &[
            "workspace",
            "add",
            "--name",
            "ws2",
            "-r",
            "base",
            ws2.to_str().unwrap(),
        ],
    );
    std::fs::write(ws2.join("src/b.py"), "def b(x):\n    return x + 20\n").unwrap();
    jj(&ws2, &["describe", "-m", "ws2 work on b"]);
    Some(tmp)
}

fn state(repo: &Path) -> Arc<AppState> {
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review);
    state
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_spec_options_are_honoured() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    let ws2 = tmp.path().join("ws2");
    let state = state(&repo);
    let app = routes::router(state.clone());

    let at = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "@" })),
    )
    .await;
    let at_id = at["id"].as_str().unwrap().to_string();
    // Unsnapshotted edits in both workspaces.
    std::fs::write(
        repo.join("src/c.py"),
        "def c(x):\n    return x + 3  # live\n",
    )
    .unwrap();
    std::fs::write(
        ws2.join("src/c.py"),
        "def c(x):\n    return x + 30  # ws2 live\n",
    )
    .unwrap();

    // liveWorkingCopy: false is a view of its own, without the overlay.
    let pure = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "@", "options": { "liveWorkingCopy": false } })),
    )
    .await;
    let pure_id = pure["id"].as_str().unwrap().to_string();
    assert_ne!(
        pure_id, at_id,
        "liveWorkingCopy: false must not reuse the @ view"
    );
    assert_eq!(pure["resolved"]["head"], at["resolved"]["head"]);
    // A fileset is a view of its own with only the files it matches.
    let only_a = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "@", "fileset": "src/a.py" })),
    )
    .await;
    let only_a_id = only_a["id"].as_str().unwrap().to_string();
    assert_ne!(only_a_id, at_id);
    assert_eq!(only_a["stats"]["files"], 1, "{only_a}");
    let ws = windows(&app, &only_a_id).await;
    assert!(
        !ws.is_empty() && ws.iter().all(|w| w["path"] == "src/a.py"),
        "{ws:?}"
    );
    // workspace without rev: that workspace's @, with its live files.
    let other = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "options": { "workspace": "ws2" } })),
    )
    .await;
    let other_id = other["id"].as_str().unwrap().to_string();
    assert_eq!(other["spec"]["rev"], "ws2@", "{other}");
    assert_ne!(other["resolved"]["head"], at["resolved"]["head"]);
    // Its live files are there from the first tile on, before any poll (a client subscribes to
    // the new view only after it opened it, so it could miss the poller's invalidation).
    let first = windows(&app, other["id"].as_str().unwrap()).await;
    assert!(
        first
            .iter()
            .any(|w| w["path"] == "src/c.py" && w["flags"]["unsnapshotted"] == true),
        "{first:?}"
    );
    // Naming the workspace the overlay would use anyway reuses the @ view.
    let same = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "@", "options": { "workspace": "default" } })),
    )
    .await;
    assert_eq!(same["id"], at_id.as_str(), "{same}");

    poll_once(&state);
    let flagged = |ws: &[Value], path: &str| {
        ws.iter()
            .filter(|w| w["path"] == path)
            .all(|w| w["flags"]["unsnapshotted"] == true)
            && ws.iter().any(|w| w["path"] == path)
    };
    assert!(
        flagged(&windows(&app, &at_id).await, "src/c.py"),
        "@ overlays its live files"
    );
    let pw = windows(&app, &pure_id).await;
    assert!(
        pw.iter().all(|w| w["flags"].get("unsnapshotted").is_none()),
        "liveWorkingCopy: false shows the pure snapshot: {pw:?}"
    );
    let ow = windows(&app, &other_id).await;
    assert!(flagged(&ow, "src/c.py"), "ws2's own live files: {ow:?}");
    let blob = ow.iter().find(|w| w["path"] == "src/c.py").unwrap()["blob"]
        .as_str()
        .unwrap()
        .to_string();
    let text = ok(&app, "GET", &format!("/api/v1/blobs/{blob}/text"), None).await;
    assert!(
        text["lines"][1].as_str().unwrap().contains("ws2 live"),
        "{text}"
    );
    // A fileset view never gains files outside it (live or after an op).
    std::fs::write(repo.join("src/d.py"), "def d():\n    return 4\n").unwrap();
    poll_once(&state);
    let ws = windows(&app, &only_a_id).await;
    assert!(ws.iter().all(|w| w["path"] == "src/a.py"), "{ws:?}");

    // Bad options are 400s, not ignored.
    for (spec, code) in [
        (
            json!({ "mode": "snapshot", "rev": "@", "options": { "workspace": "nope" } }),
            "invalid",
        ),
        (
            json!({ "mode": "snapshot", "rev": "@", "options": { "liveWorkingCopy": "no" } }),
            "invalid",
        ),
        (
            json!({ "mode": "snapshot", "rev": "@", "fileset": "src/(" }),
            "fileset-invalid",
        ),
    ] {
        let (st, e) = call(&app, "POST", "/api/v1/views", Some(spec.clone())).await;
        assert_eq!(st, 400, "{spec}: {e}");
        assert_eq!(e["error"]["code"], code, "{spec}: {e}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn overlaps_count_each_stacks_own_changes() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    let ws2 = tmp.path().join("ws2");
    let state = state(&repo);
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
    // No remote: both stacks hold base (a.py, b.py, c.py), but their own changes do not overlap.
    let cards = ok(&app, "GET", "/api/v1/workspaces", None).await;
    for n in ["default", "ws2"] {
        assert_eq!(card(&cards, n)["overlaps"], json!([]), "{n}: {cards}");
    }
    let (st, e) = call(
        &app,
        "POST",
        "/api/v1/workspaces:compare",
        Some(json!({ "a": "default", "b": "ws2" })),
    )
    .await;
    assert_eq!(
        (st, e["error"]["code"].as_str()),
        (400, Some("no-overlap")),
        "{e}"
    );

    // ws2 now also changes a.py: the overlap is a.py alone (not b.py, not the shared base).
    std::fs::write(ws2.join("src/a.py"), "def a(x):\n    return x + 200\n").unwrap();
    jj(&ws2, &["status"]);
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await; // the cards' cache
    let cards = ok(&app, "GET", "/api/v1/workspaces", None).await;
    assert_eq!(
        card(&cards, "default")["overlaps"],
        json!([{ "workspace": "ws2", "paths": ["src/a.py"] }]),
        "{cards}"
    );
    assert_eq!(
        card(&cards, "ws2")["overlaps"],
        json!([{ "workspace": "default", "paths": ["src/a.py"] }]),
        "{cards}"
    );
    let cmp = ok(
        &app,
        "POST",
        "/api/v1/workspaces:compare",
        Some(json!({ "a": "default", "b": "ws2" })),
    )
    .await;
    assert_eq!(cmp["spec"]["fileset"], "\"src/a.py\"", "{cmp}");
}

#[tokio::test(flavor = "multi_thread")]
async fn search_finds_threads_and_range_options_are_checked() {
    let Some(tmp) = fixture() else { return };
    let repo = tmp.path().join("repo");
    let state = state(&repo);
    let app = routes::router(state.clone());
    let view = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "@" })),
    )
    .await;
    let vid = view["id"].as_str().unwrap();
    let t = ok(
        &app,
        "POST",
        "/api/v1/threads",
        Some(json!({
            "anchor": { "path": "src/a.py", "rev": "@", "start": 2, "end": 2 },
            "body": "Why plus two? Zebrafish constant.",
        })),
    )
    .await;
    let hits = ok(
        &app,
        "GET",
        &format!("/api/v1/search?q=zebrafish&viewId={vid}"),
        None,
    )
    .await;
    let hit = hits
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["kind"] == "thread")
        .cloned()
        .unwrap_or_else(|| panic!("a thread hit: {hits}"));
    assert_eq!(hit["target"]["thread"], t["id"], "{hit}");
    assert_eq!(hit["target"]["site"]["path"], "src/a.py");
    assert_eq!(hit["target"]["site"]["line"], 2);
    // kinds filters them out like any other kind.
    let only = ok(&app, "GET", "/api/v1/search?q=zebrafish&kinds=symbol", None).await;
    assert!(
        only.as_array()
            .unwrap()
            .iter()
            .all(|h| h["kind"] != "thread"),
        "{only}"
    );

    // Range options: wrong values are 400 "invalid", never the defaults.
    let range = |options: Value| json!({ "mode": "range", "revset": "@", "options": options });
    for bad in [
        json!({ "contextHops": 7 }),
        json!({ "diff": "sideways" }),
        json!({ "contextDisplay": "folded" }),
        json!({ "contextLines": "three" }),
        json!({ "contextHop": 1 }),
        json!("cumulative"),
    ] {
        let (st, e) = call(&app, "POST", "/api/v1/views", Some(range(bad.clone()))).await;
        assert_eq!(
            (st, e["error"]["code"].as_str()),
            (400, Some("invalid")),
            "{bad}: {e}"
        );
    }
    let r = ok(
        &app,
        "POST",
        "/api/v1/views",
        Some(range(json!({ "diff": "per-commit", "contextHops": 0 }))),
    )
    .await;
    assert_eq!(r["spec"]["mode"], "range", "{r}");

    // GET /views/:id carries an ETag; If-None-Match answers 304.
    let get = |etag: Option<&str>| {
        let mut req = Request::builder().uri(format!("/api/v1/views/{vid}"));
        if let Some(t) = etag {
            req = req.header("if-none-match", t);
        }
        req.body(Body::empty()).unwrap()
    };
    let (st, h, body) = send(&app, get(None)).await;
    assert_eq!(st, 200);
    assert_eq!(body["id"], vid);
    let etag = h["etag"].to_str().unwrap().to_string();
    assert!(etag.starts_with('"'), "{etag}");
    let (st, _, _) = send(&app, get(Some(&etag))).await;
    assert_eq!(st, 304);
}

#[test]
fn serve_help_prints_usage_without_serving() {
    for args in [&["serve", "--help"][..], &["index", "-h"], &["--help"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_cv"))
            .args(args)
            // A path that is not a repo: had it tried to serve or index, it would fail, not print usage.
            .current_dir(std::env::temp_dir())
            .output()
            .expect("run cv");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{args:?}: {out:?}");
        assert!(stdout.starts_with("usage: cv serve"), "{args:?}: {stdout}");
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("serving"),
            "{args:?}"
        );
    }
}
