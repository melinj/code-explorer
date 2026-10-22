//! `.cv/lanes.toml` (user lanes read from the indexed revision) and stored layout records that
//! follow drifting clusters and renamed files (crate::layout_api "Drift"), through the router on
//! scripted jj repos. Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use cv_contracts::TILE_BASE_WU;
use cv_server::index::Options;
use cv_server::routes::{self, AppState};
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

fn write(repo: &Path, path: &str, text: &str) {
    let full = repo.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

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

    async fn get(&self, uri: &str) -> (u16, Value) {
        self.call("GET", uri, None).await
    }

    async fn view(&self, rev: &str) -> String {
        let (st, v) = self
            .call(
                "POST",
                "/api/v1/views",
                Some(json!({ "mode": "snapshot", "rev": rev })),
            )
            .await;
        assert_eq!(st, 200, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    async fn bootstrap(&self, vid: &str) -> Value {
        let (st, b) = self.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
        assert_eq!(st, 200, "{b}");
        b
    }

    /// Every window header of a view, read through T1 tiles covering the world.
    async fn windows(&self, vid: &str) -> Vec<Value> {
        let b = self.bootstrap(vid).await;
        let w = &b["world"];
        let span = TILE_BASE_WU * 4.0;
        let cell = |v: f64| (v / span).floor() as i64;
        let (x0, y0) = (w["x"].as_f64().unwrap(), w["y"].as_f64().unwrap());
        let (x1, y1) = (x0 + w["w"].as_f64().unwrap(), y0 + w["h"].as_f64().unwrap());
        let mut out: Vec<Value> = Vec::new();
        for tx in cell(x0)..=cell(x1) {
            for ty in cell(y0)..=cell(y1) {
                let (st, t) = self
                    .get(&format!("/api/v1/views/{vid}/tiles/2/{tx}/{ty}"))
                    .await;
                assert_eq!(st, 200, "{t}");
                for h in t["windows"].as_array().unwrap() {
                    if !out.iter().any(|o| o["id"] == h["id"]) {
                        out.push(h.clone());
                    }
                }
            }
        }
        out
    }
}

fn start(repo: &Path, store: &Path) -> (Arc<AppState>, App) {
    let root = cv_vcs::JjRepo::open(repo)
        .unwrap()
        .root()
        .display()
        .to_string();
    // A fresh process: no remembered placement.
    cv_server::layout_input::forget_placement(&root);
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

fn new_repo(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    std::fs::write(tmp.join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.join("repo");
    let store = tmp.join("store");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    (repo, store)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn lane_of(ws: &[Value], path: &str) -> String {
    ws.iter()
        .find(|w| w["path"] == path)
        .unwrap_or_else(|| panic!("no window of {path}"))["lane"]
        .as_str()
        .unwrap()
        .to_string()
}

fn ids(v: &Value, field: &str) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|l| l[field].as_str().unwrap().to_string())
        .collect()
}

const LANES_TOML: &str = r#"
# A lane of its own for prompt files, between config and code.
[[lane]]
id = "prompts"
label = "Prompts"
texture = "dots"
order = 2
globs = ["src/**/prompt*.yaml"]

# A built-in lane takes extra files.
[[lane]]
id = "docs"
globs = ["notes/*.txt"]

# Bad values are skipped with a warning (the lane stays, with no rules and the plain
# texture); the good entries above still apply.
[[lane]]
id = "broken"
texture = "zigzag"
globs = ["src/[unclosed", 7]

[[lane]]
label = "no id"
"#;

#[test]
fn lanes_toml_defines_lanes_and_falls_back() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (repo, store) = new_repo(tmp.path());
    write(&repo, "src/app.py", "def main():\n    return 1\n");
    write(&repo, "src/agents/prompt_plan.yaml", "system: plan\n");
    write(&repo, "src/agents/deep/prompt_act.yaml", "system: act\n");
    write(&repo, "src/settings.yaml", "debug: true\n");
    write(&repo, "notes/todo.txt", "write more tests\n");
    write(&repo, "README.md", "# demo\n");
    write(&repo, ".cv/lanes.toml", LANES_TOML);
    jj(&repo, &["commit", "-m", "lanes"]);

    let rt = runtime();
    let (_state, app) = start(&repo, &store);
    rt.block_on(async {
        let vid = app.view("@-").await;
        let ws = app.windows(&vid).await;
        assert_eq!(lane_of(&ws, "src/agents/prompt_plan.yaml"), "prompts");
        assert_eq!(lane_of(&ws, "src/agents/deep/prompt_act.yaml"), "prompts");
        assert_eq!(lane_of(&ws, "src/settings.yaml"), "config");
        assert_eq!(lane_of(&ws, "src/app.py"), "code");
        assert_eq!(lane_of(&ws, "notes/todo.txt"), "docs");
        assert_eq!(lane_of(&ws, ".cv/lanes.toml"), "config");

        let b = app.bootstrap(&vid).await;
        assert_eq!(
            ids(&b["laneDefs"], "id"),
            [
                "infra", "config", "prompts", "code", "test", "docs", "broken"
            ]
        );
        assert_eq!(b["laneDefs"][6]["texture"], "plain");
        let p = &b["laneDefs"][2];
        assert_eq!(p["label"], "Prompts");
        assert_eq!(p["texture"], "dots");
        assert_eq!(p["order"], 2);
        // Bands follow the lane order (empty lanes have none).
        let bands = ids(&b["lanes"], "lane");
        assert_eq!(bands, ["config", "prompts", "code", "docs"]);
        let band = b["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["lane"] == "prompts")
            .unwrap();
        assert_eq!(band["files"], 2);
        let ys: Vec<f64> = b["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["rect"]["y"].as_f64().unwrap())
            .collect();
        assert!(ys.windows(2).all(|p| p[0] < p[1]), "{ys:?}");
        // Clusters of the new lane say so.
        assert!(
            b["clusters"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["lane"] == "prompts")
        );

        let (st, lanes) = app.get("/api/v1/lanes").await;
        assert_eq!(st, 200, "{lanes}");
        assert_eq!(
            ids(&lanes, "id"),
            [
                "infra", "config", "prompts", "code", "test", "docs", "broken", "other"
            ]
        );
        assert_eq!(lanes[2]["texture"], "dots");
        assert_eq!(lanes[7]["hidden"], true);
        let orders: Vec<u64> = lanes
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["order"].as_u64().unwrap())
            .collect();
        assert_eq!(orders, [0, 1, 2, 3, 4, 5, 6, 7]);

        // A range view draws on the same lanes.
        let (st, r) = app
            .call(
                "POST",
                "/api/v1/views",
                Some(json!({ "mode": "range", "revset": "@-" })),
            )
            .await;
        assert_eq!(st, 200, "{r}");
        let rb = app.bootstrap(r["id"].as_str().unwrap()).await;
        assert_eq!(ids(&rb["laneDefs"], "id"), ids(&b["laneDefs"], "id"));
    });

    // ── A broken file in a new revision: the defaults, indexing still works ──
    write(&repo, ".cv/lanes.toml", "[[lane]\nid = \"prompts\"\n");
    jj(&repo, &["commit", "-m", "broken lanes"]);
    let (_state, app) = start(&repo, &store);
    rt.block_on(async {
        let vid = app.view("@-").await;
        let ws = app.windows(&vid).await;
        assert_eq!(lane_of(&ws, "src/agents/prompt_plan.yaml"), "config");
        assert!(
            !ws.iter().any(|w| w["path"] == "notes/todo.txt"),
            "a .txt outside docs/ is `other` again"
        );
        let b = app.bootstrap(&vid).await;
        assert_eq!(
            ids(&b["laneDefs"], "id"),
            ["infra", "config", "code", "test", "docs"]
        );
        let (_, lanes) = app.get("/api/v1/lanes").await;
        assert_eq!(
            ids(&lanes, "id"),
            ["infra", "config", "code", "test", "docs", "other"]
        );
    })
}

/// A star of files around `src/{g}/hub.py`: each calls the hub, so they cluster together.
fn spoke(g: &str, i: usize) -> (String, String) {
    (
        format!("src/{g}/{g}{i}.py"),
        format!(
            "from .hub import {g}_hub\n\n\ndef {g}{i}_f(x):\n    y = {g}_hub(x) + {i}\n    return {g}_hub(y)\n"
        ),
    )
}

fn hub(g: &str) -> (String, String) {
    (
        format!("src/{g}/hub.py"),
        format!("def {g}_hub(x):\n    return x * 2\n"),
    )
}

fn members_of(ws: &[Value], cluster: &Value) -> Vec<String> {
    let mut m: Vec<String> = ws
        .iter()
        .filter(|w| w["cluster"] == *cluster)
        .map(|w| w["path"].as_str().unwrap().to_string())
        .collect();
    m.sort();
    m.dedup();
    m
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    let i = a.iter().filter(|x| b.contains(x)).count();
    i as f64 / (a.len() + b.len() - i) as f64
}

#[test]
fn labels_and_pins_follow_drifting_clusters_and_renames() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (repo, store) = new_repo(tmp.path());
    for g in ["alpha", "omega"] {
        let (p, t) = hub(g);
        write(&repo, &p, &t);
        for i in 0..9 {
            let (p, t) = spoke(g, i);
            write(&repo, &p, &t);
        }
    }
    jj(&repo, &["commit", "-m", "base"]);

    let rt = runtime();
    let (_state, app) = start(&repo, &store);
    let pinned_at = (-40_000.0, -30_000.0);
    let original = rt.block_on(async {
        let vid = app.view("@-").await;
        let ws = app.windows(&vid).await;
        let c = ws.iter().find(|w| w["path"] == "src/alpha/alpha0.py").unwrap()["cluster"].clone();
        let members = members_of(&ws, &c);
        assert!(members.len() >= 9, "alpha clusters as one: {members:?}");
        let (st, r) = app
            .call(
                "PUT",
                &format!("/api/v1/clusters/{}/label", c.as_str().unwrap()),
                Some(json!({ "label": "Alpha star", "summary": "the alpha group" })),
            )
            .await;
        assert_eq!(st, 200, "{r}");
        // Pin an omega window: its file is renamed below.
        let w = ws.iter().find(|w| w["path"] == "src/omega/omega3.py").unwrap();
        let b = app.bootstrap(&vid).await;
        let lv = b["view"]["layoutVersion"].as_u64().unwrap();
        let (st, r) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{vid}/layout"),
                Some(json!({ "moves": [{ "key": w["key"], "x": pinned_at.0, "y": pinned_at.1, "pinned": true }], "baseLayoutVersion": lv })),
            )
            .await;
        assert_eq!(st, 200, "{r}");
        members
    });

    // Three revisions, each changing 30% of the alpha cluster (10 windows): 3 files go, 3 new
    // ones join, 3 more go. Each step keeps Jaccard 0.7 to the step before; the end shares 4 of
    // 13 with the start.
    let mut next = 9;
    let mut gone = 0;
    let mut previous = original.clone();
    for rev in 0..3 {
        for _ in 0..3 {
            if rev == 1 {
                let (p, t) = spoke("alpha", next);
                write(&repo, &p, &t);
                next += 1;
            } else {
                std::fs::remove_file(repo.join(spoke("alpha", gone).0)).unwrap();
                gone += 1;
            }
        }
        if rev == 0 {
            std::fs::rename(
                repo.join("src/omega/omega3.py"),
                repo.join("src/omega/omega3_renamed.py"),
            )
            .unwrap();
        }
        jj(&repo, &["commit", "-m", &format!("drift {rev}")]);
        let (_state, app) = start(&repo, &store);
        previous = rt.block_on(async {
            let vid = app.view("@-").await;
            let ws = app.windows(&vid).await;
            let b = app.bootstrap(&vid).await;
            let labelled: Vec<&Value> = b["clusters"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["label"] == "Alpha star")
                .collect();
            assert_eq!(labelled.len(), 1, "revision {rev}: label kept");
            assert_eq!(labelled[0]["labelSource"], "human");
            assert_eq!(labelled[0]["summary"], "the alpha group");
            let now = members_of(&ws, &labelled[0]["id"]);
            assert!(
                now.iter().all(|p| p.starts_with("src/alpha/")),
                "revision {rev}: {now:?}"
            );
            let step = jaccard(&previous, &now);
            assert!(
                (0.65..0.75).contains(&step),
                "revision {rev}: ~30% of the members changed (Jaccard {step})"
            );
            // The pin followed the rename.
            let p = ws
                .iter()
                .find(|w| w["path"] == "src/omega/omega3_renamed.py")
                .expect("renamed file indexed");
            assert_eq!(
                p["flags"]["pinned"], true,
                "revision {rev}: pin follows the rename"
            );
            assert_eq!(
                (
                    p["rect"]["x"].as_f64().unwrap(),
                    p["rect"]["y"].as_f64().unwrap()
                ),
                pinned_at
            );
            now
        });
    }
    let total = jaccard(&original, &previous);
    assert!(
        total < 0.4,
        "cumulative drift above 60% (Jaccard {total} to the original members)"
    );
}
