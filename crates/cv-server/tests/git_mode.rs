//! Git mode on a plain git repository (no jj): /setup says "git", /revisions draws the history with
//! branches, tags and HEAD, /git/commits lists a commit's files, client revisions cannot be git
//! options, every jj route answers 409 "jj-required", and "Initialise jj" needs `confirm`, refuses
//! agents, runs `jj git init --colocate` and asks to restart. Skipped without `git` or `jj`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use axum::http::Request;
use cv_server::git_mode::{GitRepo, GitState, router};
use serde_json::Value;
use tower::ServiceExt;

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.com",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn have(bin: &str) -> bool {
    let ok = Command::new(bin)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var_os("CI").is_none() && std::env::var_os("CV_REQUIRE_JJ").is_none(),
            "{bin} is not on PATH: CI must install it"
        );
        eprintln!("SKIP: {bin} is not on PATH");
    }
    ok
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<&str>,
    agent: bool,
) -> (u16, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if agent {
        req = req.header("x-cv-agent", "claude");
    }
    let res = app
        .clone()
        .oneshot(
            req.body(
                body.map(|b| Body::from(b.to_string()))
                    .unwrap_or_else(Body::empty),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 24)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[test]
fn a_plain_git_repository_is_served_read_only_until_jj_is_initialised() {
    if !have("git") || !have("jj") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.py"), "x = 1\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "first"]);
    git(&repo, &["checkout", "-qb", "feature"]);
    std::fs::write(repo.join("b.py"), "y = 2\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "feature work"]);
    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("a.py"), "x = 3\n").unwrap();
    git(&repo, &["commit", "-qam", "main moves on"]);
    git(
        &repo,
        &["merge", "-q", "--no-ff", "-m", "merge feature", "feature"],
    );
    git(&repo, &["tag", "-a", "v1", "-m", "v1"]);

    let g = GitRepo::discover(&repo.join(".")).expect("a git repository");
    assert!(
        GitRepo::discover(tmp.path()).is_none(),
        "no repository above the temp dir"
    );
    let restarted = Arc::new(AtomicBool::new(false));
    let r2 = restarted.clone();
    let app = router(Arc::new(GitState {
        repo: g,
        restart: Some(Box::new(move || r2.store(true, Ordering::SeqCst))),
    }));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (st, s) = call(&app, "GET", "/api/v1/setup", None, false).await;
        assert_eq!(st, 200);
        assert_eq!(s["vcs"], "git");
        assert_eq!(s["jj"]["installed"], true);
        assert_eq!(s["initCommand"], "jj git init --colocate");

        let (st, r) = call(&app, "GET", "/api/v1/revisions", None, false).await;
        assert_eq!(st, 200, "{r}");
        let rows = r["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        let top = &rows[0]["rev"];
        assert_eq!(top["description"], "merge feature");
        assert_eq!(top["parents"].as_array().unwrap().len(), 2);
        assert_eq!(top["flags"]["workingCopy"], true);
        assert_eq!(top["workspaces"][0], "HEAD");
        assert!(
            top["bookmarks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b == "main")
        );
        assert_eq!(
            top["tags"][0], "v1",
            "annotated tags are peeled to their commit"
        );
        assert_eq!(rows[0]["edges"].as_array().unwrap().len(), 2);
        let feature = rows
            .iter()
            .find(|x| x["rev"]["description"] == "feature work")
            .unwrap();
        assert_eq!(feature["rev"]["bookmarks"][0], "feature");

        // Revisions narrow the graph; options other than the listed ones are refused.
        let (_, r) = call(&app, "GET", "/api/v1/revisions?revset=feature", None, false).await;
        assert_eq!(r["rows"].as_array().unwrap().len(), 2);
        let (st, e) = call(
            &app,
            "GET",
            "/api/v1/revisions?revset=--output%3D%2Ftmp%2Fx",
            None,
            false,
        )
        .await;
        assert_eq!(st, 400, "{e}");

        let (_, refs) = call(&app, "GET", "/api/v1/refs", None, false).await;
        assert_eq!(refs["trunk"], "main");

        let id = feature["rev"]["commit"].as_str().unwrap();
        let (st, d) = call(
            &app,
            "GET",
            &format!("/api/v1/git/commits/{}", &id[..10]),
            None,
            false,
        )
        .await;
        assert_eq!(st, 200, "{d}");
        assert_eq!(d["commit"], id);
        assert_eq!(d["files"][0]["path"], "b.py");
        assert_eq!(d["files"][0]["added"], 1);
        assert_eq!(
            call(&app, "GET", "/api/v1/git/commits/HEAD", None, false)
                .await
                .0,
            404
        );

        // The jj routes say what is missing.
        let (st, e) = call(
            &app,
            "POST",
            "/api/v1/views",
            Some(r#"{"mode":"snapshot"}"#),
            false,
        )
        .await;
        assert_eq!(st, 409);
        assert_eq!(e["error"]["code"], "jj-required");
        assert_eq!(
            call(&app, "GET", "/api/v1/threads", None, false).await.0,
            409
        );
        assert_eq!(
            call(&app, "GET", "/api/v1/workspaces", None, false).await.1,
            serde_json::json!([])
        );

        // Initialising: confirmation required, humans only.
        assert_eq!(
            call(&app, "POST", "/api/v1/setup:init-jj", Some("{}"), false)
                .await
                .0,
            400
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/setup:init-jj",
                Some(r#"{"confirm":true}"#),
                true
            )
            .await
            .0,
            403
        );
        assert!(!repo.join(".jj").exists());
        let (st, ok) = call(
            &app,
            "POST",
            "/api/v1/setup:init-jj",
            Some(r#"{"confirm":true}"#),
            false,
        )
        .await;
        assert_eq!(st, 200, "{ok}");
        assert_eq!(ok["restarting"], true);
        assert!(repo.join(".jj").is_dir(), "jj initialised alongside git");
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(
            restarted.load(Ordering::SeqCst),
            "the server restarts after initialising"
        );
    });
    // The repository now opens as jj, with git's history.
    let jj = cv_vcs::JjRepo::open(&repo).unwrap();
    assert!(jj.resolve_one("main").is_ok());
}
