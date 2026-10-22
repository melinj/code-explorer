//! N9 (DESIGN §10): a UI session's agent toggles lapse when the session is gone (no open
//! WebSocket, not seen for `CV_SESSION_TTL_SECS`), so a forgotten tab that switched editing off
//! never blocks the agent forever. Its own test binary: it sets the TTL through the environment.
//! Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use cv_server::index::Options;
use cv_server::routes::{self, AppState};
use serde_json::{Value, json};
use tower::ServiceExt;

const USER: &str = "[user]\nname = \"Test\"\nemail = \"test@example.com\"\n";

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
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

fn jj(dir: &Path, args: &[&str]) {
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
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forgotten_session_stops_blocking_the_agent() {
    // Set before anything else runs in this process (no other thread reads the environment yet).
    unsafe { std::env::set_var("CV_SESSION_TTL_SECS", "60") };
    if !Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("jj not found; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::write(repo.join("a.py"), "def a():\n    return 1\n").unwrap();
    jj(&repo, &["commit", "-m", "a"]);
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 50 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    state.set_review(cv_review::Review::open_with(writer, None).unwrap());
    let app = routes::router(state);

    let claude = [("X-CV-Agent", "claude")];
    let tab = [("X-CV-Session", "forgotten-tab")];
    let (st, p) = call(
        &app,
        "PUT",
        "/api/v1/agent/permissions",
        &tab,
        Some(json!({ "edit": false })),
    )
    .await;
    assert_eq!((st, p["edit"].clone()), (200, json!(false)), "{p}");
    let (_, eff) = call(&app, "GET", "/api/v1/agent/permissions", &claude, None).await;
    assert_eq!(
        eff["edit"], false,
        "in force while the session is fresh: {eff}"
    );

    // Nobody opens that tab again: after the TTL the agent may edit. The session clock moves
    // forward instead of the test sleeping (a 60 s TTL also leaves a slow machine plenty of
    // time for the "fresh" check above).
    cv_server::agent_api::advance_session_clock(std::time::Duration::from_secs(61));
    let (_, eff) = call(&app, "GET", "/api/v1/agent/permissions", &claude, None).await;
    assert_eq!(eff["edit"], true, "lapsed: {eff}");

    // The row is kept: when the tab comes back (asks for its toggles), they are in force again.
    let (_, mine) = call(&app, "GET", "/api/v1/agent/permissions", &tab, None).await;
    assert_eq!(mine["edit"], false, "{mine}");
    let (_, eff) = call(&app, "GET", "/api/v1/agent/permissions", &claude, None).await;
    assert_eq!(eff["edit"], false, "back in force: {eff}");
}
