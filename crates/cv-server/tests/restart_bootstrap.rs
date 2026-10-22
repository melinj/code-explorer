//! A view id from before a restart: its tiles come from index.db's R*Tree, and so do its bootstrap
//! and its ViewRef (GET /views/:id, GET /views/:id/bootstrap), instead of a 404. Skipped when `jj`
//! is not on PATH. Own test binary: it switches the process-wide parse cache / index.db location.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use cv_server::index::Options;
use cv_server::routes::{self, AppState};
use cv_server::{layout_input, parse_cache, view_store};
use serde_json::{Value, json};
use tower::ServiceExt;

fn jj(dir: &Path, args: &[&str]) {
    let out = Command::new("jj")
        .args(args)
        .current_dir(dir)
        .env("JJ_CONFIG", dir.parent().unwrap().join("jjconfig.toml"))
        .env("JJ_USER", "Test")
        .env("JJ_EMAIL", "test@example.com")
        .output()
        .expect("run jj");
    assert!(
        out.status.success(),
        "jj {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

async fn call(router: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let r = router.clone().oneshot(req).await.unwrap();
    let status = r.status().as_u16();
    let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The tests switch the process-wide parse cache and index.db location: one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn server(repo: &Path) -> axum::Router {
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(repo).unwrap(),
        Options { max_commits: 50 },
    ));
    routes::router(state)
}

#[test]
fn old_view_id_bootstrap_after_restart() {
    if !have_jj() {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), "").unwrap();
    parse_cache::enable_in(&tmp.path().join("cache"));
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    jj(&repo, &["git", "init"]);
    for i in 0..4 {
        std::fs::write(
            repo.join(format!("src/m{i}.py")),
            format!("from m0 import f0\n\ndef f{i}(x):\n    return f0(x) + {i}\n"),
        )
        .unwrap();
    }
    jj(&repo, &["commit", "-m", "base"]);
    let root = cv_vcs::JjRepo::open(&repo)
        .unwrap()
        .root()
        .display()
        .to_string();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // First process: build the view and write it to index.db before answering.
        layout_input::forget_placement(&root);
        view_store::set_wait_for_save(true);
        let app = server(&repo);
        let (st, v) = call(
            &app,
            "POST",
            "/api/v1/views",
            Some(json!({ "mode": "snapshot", "rev": "@-" })),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        let vid = v["id"].as_str().unwrap().to_string();
        let (st, before) = call(&app, "GET", &format!("/api/v1/views/{vid}/bootstrap"), None).await;
        assert_eq!(st, 200);
        view_store::set_wait_for_save(false);
        drop(app);

        // Restart: a new AppState on the same index.db, nothing loaded, placement forgotten.
        layout_input::forget_placement(&root);
        let app = server(&repo);
        let (st, t) = call(
            &app,
            "GET",
            &format!("/api/v1/views/{vid}/tiles/0/0/0"),
            None,
        )
        .await;
        assert_eq!(st, 200, "tile of the old id: {t}");
        let (st, b) = call(&app, "GET", &format!("/api/v1/views/{vid}/bootstrap"), None).await;
        assert_eq!(st, 200, "bootstrap of the old id: {b}");
        assert_eq!(b["view"]["id"], vid.as_str());
        assert_eq!(b["view"], before["view"]);
        assert_eq!(b["clusters"], before["clusters"]);
        let (st, r) = call(&app, "GET", &format!("/api/v1/views/{vid}"), None).await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["id"], vid.as_str());
        let (st, _) = call(&app, "GET", "/api/v1/views/v_nope/bootstrap", None).await;
        assert_eq!(st, 404);
    });
    parse_cache::disable();
}

/// After a restart with a changed operation, the old view id is not loaded: windows:batch,
/// fragments, metrics, expand and PATCH layout answer 404 "view-unknown" (not a bare "not-found"
/// that a client would swallow, leaving windows without code), GET /views/:id says "stale", and
/// re-POSTing the spec gives a view whose batches and fragments work.
#[test]
fn old_view_id_after_restart_says_view_unknown() {
    if !have_jj() {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), "").unwrap();
    parse_cache::enable_in(&tmp.path().join("cache"));
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    jj(&repo, &["git", "init"]);
    for i in 0..3 {
        std::fs::write(
            repo.join(format!("src/m{i}.py")),
            format!("from m0 import f0\n\ndef f{i}(x):\n    return f0(x) + {i}\n"),
        )
        .unwrap();
    }
    jj(&repo, &["commit", "-m", "base"]);
    let root = cv_vcs::JjRepo::open(&repo)
        .unwrap()
        .root()
        .display()
        .to_string();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        layout_input::forget_placement(&root);
        view_store::set_wait_for_save(true);
        let app = server(&repo);
        let spec = json!({ "mode": "snapshot", "rev": "@-" });
        let (st, v) = call(&app, "POST", "/api/v1/views", Some(spec.clone())).await;
        assert_eq!(st, 200, "{v}");
        let vid = v["id"].as_str().unwrap().to_string();
        let (_, t) = call(
            &app,
            "GET",
            &format!("/api/v1/views/{vid}/tiles/8/0/0"),
            None,
        )
        .await;
        let wid = t["windows"][0]["id"].as_str().unwrap().to_string();
        let path = t["windows"][0]["path"].as_str().unwrap().to_string();
        let batch =
            json!({ "ids": [wid], "parts": ["outline"], "layoutVersion": v["layoutVersion"] });
        let (st, b) = call(
            &app,
            "POST",
            &format!("/api/v1/views/{vid}/windows:batch"),
            Some(batch.clone()),
        )
        .await;
        assert_eq!(st, 200, "{b}");
        view_store::set_wait_for_save(false);
        drop(app);

        // Another operation, then a restart: nothing loaded, placement forgotten.
        jj(&repo, &["describe", "-m", "wip"]);
        layout_input::forget_placement(&root);
        let app = server(&repo);
        let gone = |what: &str, st: u16, body: &Value| {
            assert_eq!(st, 404, "{what}: {body}");
            assert_eq!(body["error"]["code"], "view-unknown", "{what}: {body}");
        };
        let (st, b) = call(
            &app,
            "POST",
            &format!("/api/v1/views/{vid}/windows:batch"),
            Some(batch.clone()),
        )
        .await;
        gone("windows:batch", st, &b);
        let frag = format!("/api/v1/views/{vid}/fragments?path={path}");
        let (st, b) = call(&app, "GET", &frag, None).await;
        gone("fragments", st, &b);
        let (st, b) = call(
            &app,
            "GET",
            &format!("/api/v1/views/{vid}/metrics?level=file"),
            None,
        )
        .await;
        gone("metrics", st, &b);
        let (st, b) = call(
            &app,
            "POST",
            &format!("/api/v1/views/{vid}/windows/{wid}/expand"),
            Some(json!({ "after": 2 })),
        )
        .await;
        gone("expand", st, &b);
        let (st, b) = call(
            &app,
            "PATCH",
            &format!("/api/v1/views/{vid}/layout"),
            Some(json!({ "moves": [], "baseLayoutVersion": 1 })),
        )
        .await;
        gone("PATCH layout", st, &b);
        // The ViewRef from index.db says the client should re-POST the spec.
        let (st, r) = call(&app, "GET", &format!("/api/v1/views/{vid}"), None).await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["status"], "stale", "{r}");
        // Re-POSTing the spec: a loaded view whose batches and fragments work.
        let (st, v2) = call(&app, "POST", "/api/v1/views", Some(spec)).await;
        assert_eq!(st, 200, "{v2}");
        let vid2 = v2["id"].as_str().unwrap().to_string();
        assert_ne!(vid2, vid, "a new operation, a new id");
        let (_, t) = call(
            &app,
            "GET",
            &format!("/api/v1/views/{vid2}/tiles/8/0/0"),
            None,
        )
        .await;
        let w2 = t["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["path"] == path.as_str())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (st, b) = call(
            &app,
            "POST",
            &format!("/api/v1/views/{vid2}/windows:batch"),
            Some(
                json!({ "ids": [w2], "parts": ["outline"], "layoutVersion": v2["layoutVersion"] }),
            ),
        )
        .await;
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["outlines"].as_array().map(Vec::len), Some(1), "{b}");
        let (st, f) = call(
            &app,
            "GET",
            &format!("/api/v1/views/{vid2}/fragments?path={path}"),
            None,
        )
        .await;
        assert_eq!(st, 200, "{f}");
        assert_eq!(f["path"], path.as_str());
        // A plain missing path stays a plain not-found.
        let (st, f) = call(
            &app,
            "GET",
            &format!("/api/v1/views/{vid2}/fragments?path=nope.py"),
            None,
        )
        .await;
        assert_eq!(
            (st, f["error"]["code"].as_str()),
            (404, Some("not-found")),
            "{f}"
        );
    });
    parse_cache::disable();
}
