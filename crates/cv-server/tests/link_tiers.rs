//! DESIGN §4.5 link tiers and §4.8 connectedness through the router, on a scripted jj repo:
//! lexical candidates for ambiguous refs (≤ 3, ≤ 0.5), SCIP links from a committed `index.scip`
//! (provenance "scip", `capabilities.scip`), a confirmed candidate re-added after a re-index no
//! longer produces it, and the cycle size of a two-file import cycle. Skipped when `jj` is not on
//! PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use cv_link::scip::{Document, Index, Occurrence};
use cv_server::index::Options;
use cv_server::routes::{self, AppState};
use serde_json::{Value, json};
use tower::ServiceExt;

const USER: &str = "[user]\nname = \"Test\"\nemail = \"test@example.com\"\n";

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

fn write(repo: &Path, path: &str, text: &[u8]) {
    let full = repo.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

const MAIN: &str = "def main():\n    render()\n    return helper()\n";
const SCIP_HELPER: &str = "scip-python python app 0.1 `src.lib`/helper().";

struct App {
    router: axum::Router,
}

impl App {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(match body {
                Some(b) => Body::from(b.to_string()),
                None => Body::empty(),
            })
            .unwrap();
        let r = self.router.clone().oneshot(req).await.unwrap();
        let status = r.status().as_u16();
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn view(&self) -> String {
        let (st, v) = self
            .call(
                "POST",
                "/api/v1/views",
                Some(json!({ "mode": "snapshot", "rev": "@" })),
            )
            .await;
        assert_eq!(st, 200, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    async fn out_links(&self, vid: &str, site: &str) -> Vec<Value> {
        let (st, r) = self
            .call(
                "GET",
                &format!("/api/v1/links?viewId={vid}&target={site}&dir=out&depth=1"),
                None,
            )
            .await;
        assert_eq!(st, 200, "{r}");
        r["links"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l["from"]["path"] == site.rsplit_once(':').unwrap().0)
            .filter(|l| l["from"]["line"].as_u64() == site.rsplit_once(':').unwrap().1.parse().ok())
            .cloned()
            .collect()
    }
}

fn start(repo: &Path, store: &Path) -> (Arc<AppState>, App) {
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(repo).unwrap(),
        Options { max_commits: 100 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(repo, true, Some(USER)).unwrap();
    let review =
        cv_review::Review::open_with(writer, Some(cv_store::StorePaths::in_dir(store))).unwrap();
    state.set_review(review);
    let router = routes::router(state.clone());
    (state, App { router })
}

#[test]
fn candidates_scip_confirmed_readd_and_cycles() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    write(&repo, "src/ui/main.py", MAIN.as_bytes());
    write(&repo, "src/ui/render.py", b"def render():\n    return 1\n");
    write(
        &repo,
        "src/other/render.py",
        b"def render():\n    return 2\n",
    );
    write(&repo, "src/lib.py", b"def helper():\n    return 3\n");
    write(
        &repo,
        "src/c1.py",
        b"from c2 import g\n\n\ndef f():\n    return g()\n",
    );
    write(
        &repo,
        "src/c2.py",
        b"from c1 import f\n\n\ndef g():\n    return 1\n\n\ndef h():\n    return f()\n",
    );
    // A SCIP index: main.py:3 references helper, defined at lib.py:1.
    let scip = Index {
        documents: vec![
            Document {
                path: "src/lib.py".into(),
                occurrences: vec![Occurrence {
                    line: 1,
                    symbol: SCIP_HELPER.into(),
                    definition: true,
                }],
                implements: vec![],
            },
            Document {
                path: "src/ui/main.py".into(),
                occurrences: vec![Occurrence {
                    line: 3,
                    symbol: SCIP_HELPER.into(),
                    definition: false,
                }],
                implements: vec![],
            },
        ],
    };
    write(&repo, "index.scip", &cv_link::scip::encode(&scip));
    jj(&repo, &["commit", "-m", "base"]);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (state, app) = start(&repo, &store);
    let rendered_id = rt.block_on(async {
        let vid = app.view().await;
        // Tier 1: `render` is defined in two files and imported in neither → two candidates.
        let ls = app.out_links(&vid, "src/ui/main.py:2").await;
        let mut to: Vec<(&str, f64)> = ls
            .iter()
            .map(|l| {
                (
                    l["to"]["path"].as_str().unwrap(),
                    l["confidence"].as_f64().unwrap(),
                )
            })
            .collect();
        to.sort_by(|a, b| a.0.cmp(b.0));
        assert_eq!(
            to,
            vec![("src/other/render.py", 0.45), ("src/ui/render.py", 0.45)],
            "{ls:?}"
        );
        assert!(ls.iter().all(|l| l["provenance"] == "tree-sitter"));
        // Tier 2: SCIP replaces the guess for `helper` on the line it covers.
        let ls = app.out_links(&vid, "src/ui/main.py:3").await;
        assert_eq!(ls.len(), 1, "{ls:?}");
        assert_eq!(
            (
                ls[0]["to"]["path"].as_str(),
                ls[0]["provenance"].as_str(),
                ls[0]["confidence"].as_f64()
            ),
            (Some("src/lib.py"), Some("scip"), Some(1.0))
        );
        let (_, info) = app.call("GET", "/api/v1/repo", None).await;
        assert_eq!(info["capabilities"]["scip"], true, "{info}");

        // §4.8: the c1 ↔ c2 import cycle shows on their windows; tests apart, typed links.
        let (_, boot) = app
            .call("GET", &format!("/api/v1/views/{vid}/bootstrap"), None)
            .await;
        let w = &boot["world"];
        let (_, tile) = app
            .call(
                "GET",
                &format!(
                    "/api/v1/views/{vid}/tiles/12/{}/{}",
                    (w["x"].as_f64().unwrap() / cv_contracts::TILE_BASE_WU / 4096.0).floor(),
                    (w["y"].as_f64().unwrap() / cv_contracts::TILE_BASE_WU / 4096.0).floor()
                ),
                None,
            )
            .await;
        let win = |path: &str| {
            tile["windows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|h| h["path"] == path)
                .cloned()
                .unwrap_or_else(|| panic!("no window for {path}: {tile}"))
        };
        assert_eq!(win("src/c1.py")["conn"]["cycle"], 2);
        assert_eq!(win("src/c2.py")["conn"]["cycle"], 2);
        assert!(win("src/lib.py")["conn"].get("cycle").is_none());
        // lib.py's helper has one dependent through the SCIP link; candidates do not count.
        assert_eq!(win("src/lib.py")["conn"]["dependents2"], 1);
        assert_eq!(win("src/ui/render.py")["conn"]["dependents2"], 0);

        // Tier 3: confirm the candidate into src/ui/render.py.
        let id = ls_id(
            &app.out_links(&vid, "src/ui/main.py:2").await,
            "src/ui/render.py",
        );
        let (st, r) = app
            .call(
                "POST",
                &format!("/api/v1/links/{id}/rate"),
                Some(json!({ "verdict": "confirmed", "viewId": vid })),
            )
            .await;
        assert_eq!(st, 200, "{r}");
        id
    });
    drop((state, app));

    // `render` gets five more definitions: too ambiguous to guess, so the index drops both
    // candidates. The confirmed one comes back from review.db; the other does not.
    for i in 0..5 {
        write(
            &repo,
            &format!("src/x{i}/render.py"),
            b"def render():\n    return 0\n",
        );
    }
    jj(&repo, &["commit", "-m", "more renders"]);
    let (_state, app) = start(&repo, &store);
    rt.block_on(async {
        let vid = app.view().await;
        let ls = app.out_links(&vid, "src/ui/main.py:2").await;
        assert_eq!(ls.len(), 1, "{ls:?}");
        let l = &ls[0];
        assert_eq!(l["to"]["path"], "src/ui/render.py");
        assert_eq!(l["feedback"], "confirmed");
        assert!(l["to"]["window"].is_string(), "{l}");
        let id = l["id"].as_str().unwrap();
        assert!(id.starts_with("rc-") && id != rendered_id, "{id}");
    });
}

fn ls_id(ls: &[Value], to: &str) -> String {
    ls.iter()
        .find(|l| l["to"]["path"] == to)
        .and_then(|l| l["id"].as_str())
        .unwrap_or_else(|| panic!("no link to {to}: {ls:?}"))
        .to_string()
}
