//! Layout edits, pins, persisted cluster labels, saved ranges, `GET /repo` and `GET /lanes`
//! (crate::layout_api, crate::repo_api) through the router on a scripted jj repo, including
//! restarts (a new AppState on the same review.db, the in-memory placement forgotten) and a new
//! revision indexed from scratch. Skipped when `jj` is not on PATH.

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

/// Two groups of files that call within themselves, so the code lane has several clusters.
fn files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for g in ["auth", "store"] {
        for i in 0..5 {
            let mut text = String::new();
            if i > 0 {
                text.push_str(&format!("from .{g}{} import {g}{}_f\n\n\n", i - 1, i - 1));
            }
            text.push_str(&format!("def {g}{i}_f(x):\n    y = x + {i}\n"));
            if i > 0 {
                text.push_str(&format!("    y = {g}{}_f(y)\n", i - 1));
            }
            text.push_str("    return y\n");
            out.push((format!("src/{g}/{g}{i}.py"), text));
        }
    }
    out.push(("README.md".into(), "# demo\n\nSome docs.\n".into()));
    out
}

struct App {
    router: axum::Router,
}

impl App {
    async fn call(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
        agent: bool,
    ) -> (u16, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if agent {
            req = req
                .header("x-cv-agent", "claude")
                .header("x-cv-agent-client", "claude-code");
        }
        let req = req
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
        self.call("GET", uri, None, false).await
    }

    async fn view(&self, rev: &str) -> String {
        let (st, v) = self
            .call(
                "POST",
                "/api/v1/views",
                Some(json!({ "mode": "snapshot", "rev": rev })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    /// Every window header of a view, read through T1 tiles covering the world.
    async fn windows(&self, vid: &str) -> Vec<Value> {
        let (_, b) = self.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
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

    /// Windows in the base tile holding world point (x, y).
    async fn tile_at(&self, vid: &str, x: f64, y: f64) -> Value {
        let (tx, ty) = (
            (x / TILE_BASE_WU).floor() as i64,
            (y / TILE_BASE_WU).floor() as i64,
        );
        let (st, t) = self
            .get(&format!("/api/v1/views/{vid}/tiles/0/{tx}/{ty}"))
            .await;
        assert_eq!(st, 200, "{t}");
        t
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

/// What a process restart does to the in-memory state: forget the repo's last placement.
fn restart(repo: &Path, store: &Path) -> (Arc<AppState>, App) {
    let root = cv_vcs::JjRepo::open(repo)
        .unwrap()
        .root()
        .display()
        .to_string();
    cv_server::layout_input::forget_placement(&root);
    start(repo, store)
}

fn rect(v: &Value) -> (f64, f64) {
    (
        v["rect"]["x"].as_f64().unwrap(),
        v["rect"]["y"].as_f64().unwrap(),
    )
}

fn by_key<'a>(ws: &'a [Value], key: &str) -> &'a Value {
    ws.iter()
        .find(|w| w["key"] == key)
        .unwrap_or_else(|| panic!("no window {key}"))
}

#[test]
fn layout_pins_labels_ranges_repo_lanes() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    for (p, text) in files() {
        let full = repo.join(&p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    jj(&repo, &["commit", "-m", "base"]);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // ── PATCH layout: a window, a cluster, a stale version ──
    let (_state, app) = start(&repo, &store);
    let (pin_key, pin_at, cluster_members, cluster_pin_at, label_members) = rt.block_on(async {
        let vid = app.view("@-").await;
        let (_, b) = app.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
        assert_eq!(b["view"]["layoutVersion"], 1);
        assert!(b["clusters"].as_array().unwrap().iter().all(|c| c["pinned"] == false));
        let world = b["world"].clone();
        let ws = app.windows(&vid).await;
        assert!(ws.len() >= 10, "{} windows", ws.len());
        assert!(ws.iter().all(|w| w["flags"]["pinned"] == false));

        let w0 = ws.iter().find(|w| w["path"] == "src/auth/auth2.py").unwrap().clone();
        let key = w0["key"].as_str().unwrap().to_string();
        let far = (
            world["x"].as_f64().unwrap() + world["w"].as_f64().unwrap() + 3.0 * TILE_BASE_WU,
            world["y"].as_f64().unwrap() + 100.0,
        );
        let (st, r) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{vid}/layout"),
                Some(json!({ "moves": [{ "key": key, "x": far.0, "y": far.1, "pinned": false }], "baseLayoutVersion": 1 })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r, json!({ "layoutVersion": 2 }));
        // The tile at the new spot serves the window there; the old spot no longer does.
        let t = app.tile_at(&vid, far.0 + 1.0, far.1 + 1.0).await;
        assert_eq!(t["layoutVersion"], 2);
        let moved = t["windows"].as_array().unwrap().iter().find(|h| h["key"] == key.as_str());
        assert_eq!(rect(moved.expect("moved window in its new tile")), far);
        assert_eq!(moved.unwrap()["flags"]["pinned"], false);
        let (ox, oy) = rect(&w0);
        let t = app.tile_at(&vid, ox + 1.0, oy + 1.0).await;
        assert!(!t["windows"].as_array().unwrap().iter().any(|h| h["key"] == key.as_str()));
        // Its cluster grew to contain it, and so did the world.
        let (_, b) = app.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
        let c = b["clusters"].as_array().unwrap().iter().find(|c| c["id"] == w0["cluster"]).unwrap();
        assert!(c["rect"]["x"].as_f64().unwrap() + c["rect"]["w"].as_f64().unwrap() >= far.0);
        assert!(b["world"]["w"].as_f64().unwrap() > world["w"].as_f64().unwrap());
        assert_eq!(b["view"]["layoutVersion"], 2);

        // Stale base version.
        let (st, r) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{vid}/layout"),
                Some(json!({ "moves": [{ "key": key, "x": 0.0, "y": 0.0, "pinned": false }], "baseLayoutVersion": 1 })),
                false,
            )
            .await;
        assert_eq!(st, 409, "{r}");
        assert_eq!(r["error"]["code"], "layout-stale");
        // Unknown keys are refused as a whole.
        let (st, _) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{vid}/layout"),
                Some(json!({ "moves": [{ "key": "nope#x", "x": 0.0, "y": 0.0, "pinned": false }], "baseLayoutVersion": 2 })),
                false,
            )
            .await;
        assert_eq!(st, 400);

        // A cluster move: the rect moves and every window moves by the same delta.
        let ws = app.windows(&vid).await;
        let store_w = ws.iter().find(|w| w["path"] == "src/store/store1.py").unwrap();
        let cid = store_w["cluster"].as_str().unwrap().to_string();
        let c = b["clusters"].as_array().unwrap().iter().find(|c| c["id"] == cid.as_str()).unwrap().clone();
        let (cx, cy) = rect(&c);
        let to = (cx - 5000.0, cy + 7000.0);
        let members: Vec<Value> = ws.iter().filter(|w| w["cluster"] == cid.as_str()).cloned().collect();
        assert!(!members.is_empty());
        let (st, r) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{vid}/layout"),
                Some(json!({ "moves": [{ "key": format!("cluster:{cid}"), "x": to.0, "y": to.1, "pinned": false }], "baseLayoutVersion": 2 })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["layoutVersion"], 3);
        let ws2 = app.windows(&vid).await;
        for m in &members {
            let n = by_key(&ws2, m["key"].as_str().unwrap());
            let (a, b) = (rect(m), rect(n));
            assert_eq!((b.0 - a.0, b.1 - a.1), (-5000.0, 7000.0), "{}", m["key"]);
        }
        let (_, b) = app.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
        let c2 = b["clusters"].as_array().unwrap().iter().find(|c| c["id"] == cid.as_str()).unwrap();
        assert_eq!(rect(c2), to);
        assert_eq!(c2["pinned"], false);

        // view.invalidated (reason "layout") lists the moved windows.
        let (st, ev) = app.get("/api/v1/events?cursor=0&kinds=view.invalidated").await;
        assert_eq!(st, 200, "{ev}");
        let layout: Vec<&Value> = ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["reason"] == "layout" && e["viewId"] == vid.as_str())
            .collect();
        assert_eq!(layout.len(), 2, "{ev}");
        assert_eq!(layout[0]["layoutVersion"], 2);
        assert_eq!(layout[0]["windows"], json!([w0["id"]]));
        assert_eq!(layout[1]["windows"].as_array().unwrap().len(), members.len());

        // Pins: a window and a cluster (the auth one, by its id), and a label on that cluster.
        let pin_at = (far.0 + 500.0, far.1 + 250.0);
        let auth_w = ws2.iter().find(|w| w["path"] == "src/auth/auth0.py").unwrap();
        let auth_c = auth_w["cluster"].as_str().unwrap().to_string();
        let ac = b["clusters"].as_array().unwrap().iter().find(|c| c["id"] == auth_c.as_str()).unwrap();
        let (ax, ay) = rect(ac);
        let cluster_pin_at = (ax - 9000.0, ay - 3000.0);
        let (st, r) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{vid}/layout"),
                Some(json!({ "moves": [
                    { "key": key, "x": pin_at.0, "y": pin_at.1, "pinned": true },
                    { "key": format!("cluster:{auth_c}"), "x": cluster_pin_at.0, "y": cluster_pin_at.1, "pinned": true }
                ], "baseLayoutVersion": 3 })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{r}");
        let ws3 = app.windows(&vid).await;
        let p = by_key(&ws3, &key);
        assert_eq!(rect(p), pin_at);
        assert_eq!(p["flags"]["pinned"], true);
        let (_, b) = app.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
        let ac = b["clusters"].as_array().unwrap().iter().find(|c| c["id"] == auth_c.as_str()).unwrap();
        assert_eq!(ac["pinned"], true);
        let auth_members: Vec<String> = ws3
            .iter()
            .filter(|w| w["cluster"] == auth_c.as_str() && w["flags"]["pinned"] == false)
            .map(|w| w["key"].as_str().unwrap().to_string())
            .collect();

        // A label (and summary) on the store cluster.
        let (st, r) = app
            .call(
                "PUT",
                &format!("/api/v1/clusters/{cid}/label"),
                Some(json!({ "label": "Storage core", "summary": "where rows live" })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{r}");
        let label_members: Vec<String> =
            members.iter().map(|w| w["key"].as_str().unwrap().to_string()).collect();
        (key, pin_at, auth_members, cluster_pin_at, label_members)
    });

    // ── Restart: pins, cluster pins and labels come back ──
    let check = |app: &App, vid: String| {
        rt.block_on(async {
            let ws = app.windows(&vid).await;
            let p = by_key(&ws, &pin_key);
            assert_eq!(rect(p), pin_at, "pinned window keeps its spot");
            assert_eq!(p["flags"]["pinned"], true);
            assert!(ws.iter().filter(|w| w["flags"]["pinned"] == true).count() == 1);
            let (_, b) = app.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
            let clusters = b["clusters"].as_array().unwrap();
            let pinned: Vec<&Value> = clusters.iter().filter(|c| c["pinned"] == true).collect();
            assert_eq!(pinned.len(), 1, "one pinned cluster");
            let pc = pinned[0];
            assert_eq!(rect(pc), cluster_pin_at, "pinned cluster keeps its spot");
            let inside: Vec<&str> = ws
                .iter()
                .filter(|w| w["cluster"] == pc["id"] && w["flags"]["pinned"] == false)
                .map(|w| w["key"].as_str().unwrap())
                .collect();
            assert!(
                cluster_members
                    .iter()
                    .filter(|k| inside.contains(&k.as_str()))
                    .count()
                    * 10
                    >= cluster_members.len() * 6
            );
            for w in ws.iter().filter(|w| w["cluster"] == pc["id"]) {
                if w["flags"]["pinned"] == false {
                    assert!(rect(w).0 >= cluster_pin_at.0 && rect(w).1 >= cluster_pin_at.1);
                }
            }
            let l = clusters
                .iter()
                .find(|c| c["label"] == "Storage core")
                .unwrap_or_else(|| panic!("label restored: {clusters:?}"));
            assert_eq!(l["labelSource"], "human");
            assert_eq!(l["summary"], "where rows live");
            let lw: Vec<&str> = ws
                .iter()
                .filter(|w| w["cluster"] == l["id"])
                .map(|w| w["key"].as_str().unwrap())
                .collect();
            assert!(label_members.iter().any(|k| lw.contains(&k.as_str())));
        })
    };
    let (_state, app) = restart(&repo, &store);
    let vid = rt.block_on(app.view("@-"));
    check(&app, vid);

    // ── A new revision indexed from scratch keeps them too ──
    let pin_path = pin_key.split('#').next().unwrap().to_string();
    let text = std::fs::read_to_string(repo.join(&pin_path)).unwrap();
    std::fs::write(
        repo.join(&pin_path),
        format!("{text}\n\ndef extra():\n    return 1\n"),
    )
    .unwrap();
    std::fs::write(
        repo.join("src/store/store9.py"),
        "def store9_f():\n    return 9\n",
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "edit"]);
    let (_state, app) = restart(&repo, &store);
    let vid = rt.block_on(app.view("@-"));
    check(&app, vid.clone());

    // ── Unpin: the same position with pinned: false ──
    rt.block_on(async {
        let (_, b) = app.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
        let lv = b["view"]["layoutVersion"].as_u64().unwrap();
        let pc = b["clusters"].as_array().unwrap().iter().find(|c| c["pinned"] == true).unwrap();
        let (st, r) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{vid}/layout"),
                Some(json!({ "moves": [
                    { "key": pin_key, "x": pin_at.0, "y": pin_at.1, "pinned": false },
                    { "key": format!("cluster:{}", pc["id"].as_str().unwrap()), "x": cluster_pin_at.0, "y": cluster_pin_at.1, "pinned": false }
                ], "baseLayoutVersion": lv })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["layoutVersion"], lv + 1);
    });
    let (_state, app) = restart(&repo, &store);
    rt.block_on(async {
        let vid = app.view("@-").await;
        let ws = app.windows(&vid).await;
        assert!(ws.iter().all(|w| w["flags"]["pinned"] == false));
        assert_ne!(
            rect(by_key(&ws, &pin_key)),
            pin_at,
            "unpinned: back to its layout spot"
        );
        let (_, b) = app.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
        assert!(
            b["clusters"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["pinned"] == false)
        );
        // The label is still there.
        assert!(
            b["clusters"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["label"] == "Storage core")
        );
    });

    // ── Saved ranges ──
    rt.block_on(async {
        assert_eq!(app.get("/api/v1/ranges").await, (200, json!([])));
        let (st, r) = app
            .call(
                "PUT",
                "/api/v1/ranges/my%20stack",
                Some(json!({ "name": "ignored", "revset": "trunk()..@", "fileset": "src/**" })),
                true,
            )
            .await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["name"], "my stack");
        assert_eq!(r["revset"], "trunk()..@");
        assert_eq!(r["fileset"], "src/**");
        assert_eq!(r["createdBy"]["kind"], "agent");
        assert_eq!(r["createdBy"]["name"], "claude");
        let (st, r) = app
            .call(
                "PUT",
                "/api/v1/ranges/second",
                Some(json!({ "name": "second", "revset": "@-", "options": { "diff": "per-commit" }, "createdBy": { "kind": "human", "name": "x" } })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["createdBy"]["kind"], "human");
        assert_eq!(r["options"]["diff"], "per-commit");
        // Update keeps the creator.
        let (st, r) = app
            .call("PUT", "/api/v1/ranges/my%20stack", Some(json!({ "revset": "@" })), false)
            .await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["revset"], "@");
        assert_eq!(r["createdBy"]["kind"], "agent");
        let (st, r) = app
            .call("PUT", "/api/v1/ranges/bad", Some(json!({ "revset": " " })), false)
            .await;
        assert_eq!(st, 400, "{r}");
    });
    let (_state, app) = restart(&repo, &store);
    rt.block_on(async {
        let (st, list) = app.get("/api/v1/ranges").await;
        assert_eq!(st, 200);
        let names: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["my stack", "second"],
            "saved ranges survive a restart, in order"
        );
        assert_eq!(
            app.call("DELETE", "/api/v1/ranges/my%20stack", None, false)
                .await,
            (200, json!({ "ok": true }))
        );
        assert_eq!(
            app.call("DELETE", "/api/v1/ranges/my%20stack", None, false)
                .await
                .0,
            404
        );
        let (_, list) = app.get("/api/v1/ranges").await;
        assert_eq!(list.as_array().unwrap().len(), 1);
    });

    // ── Repo and lanes ──
    rt.block_on(async {
        let vid = app.view("@").await;
        assert!(!vid.is_empty());
        let (st, r) = app.get("/api/v1/repo").await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["name"], "repo");
        assert!(r["root"].as_str().unwrap().ends_with("repo"));
        assert_eq!(r["jjVersion"], "0.45.1");
        assert!(r["op"].as_str().unwrap().len() >= 12);
        let ws = r["workspaces"].as_array().unwrap();
        assert_eq!(ws.len(), 1, "{r}");
        assert_eq!(ws[0]["name"], "default");
        assert!(ws[0]["wc"]["commit"].is_string());
        assert_eq!(r["index"]["state"], "idle");
        assert!(r["index"]["files"].as_u64().unwrap() >= 11);
        assert!(r["index"]["blobsParsed"].as_u64().unwrap() >= 10);
        assert!(r["index"]["churn"]["scannedCommits"].is_number());
        assert_eq!(
            r["capabilities"],
            json!({ "scip": false, "runtimeLinks": false, "jjLib": true })
        );

        let (st, lanes) = app.get("/api/v1/lanes").await;
        assert_eq!(st, 200);
        let ids: Vec<&str> = lanes
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["infra", "config", "code", "test", "docs", "other"]);
        let other = &lanes[5];
        assert_eq!(other["hidden"], true);
        assert_eq!(lanes[2]["texture"], "plain");
        assert_eq!(lanes[2]["order"], 2);
    });
}

/// A move in the snapshot view reaches an open range view of the same revision (D16: ranges share
/// the snapshot's placement): same window, same new place, a bumped layoutVersion.
#[test]
fn layout_moves_reach_other_open_views() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    for (p, text) in files() {
        let full = repo.join(&p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    jj(&repo, &["commit", "-m", "base"]);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (_state, app) = start(&repo, &store);
    rt.block_on(async {
        let snap = app.view("@-").await;
        let (st, r) = app
            .call(
                "POST",
                "/api/v1/views",
                Some(json!({ "mode": "range", "revset": "@-", "options": { "contextHops": 0 } })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{r}");
        let range = r["id"].as_str().unwrap().to_string();
        let auth2 = |ws: &[Value]| {
            ws.iter()
                .find(|w| w["path"] == "src/auth/auth2.py")
                .expect("auth2 is shown")
                .clone()
        };
        let before = rect(&auth2(&app.windows(&range).await));
        let sw = auth2(&app.windows(&snap).await);
        let key = sw["key"].as_str().unwrap().to_string();
        let (x, y) = rect(&sw);
        let (_, b) = app.get(&format!("/api/v1/views/{snap}/bootstrap")).await;
        let base = b["view"]["layoutVersion"].as_u64().unwrap();
        let (_, rb) = app.get(&format!("/api/v1/views/{range}/bootstrap")).await;
        let range_base = rb["view"]["layoutVersion"].as_u64().unwrap();
        let (st, out) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{snap}/layout"),
                Some(json!({ "moves": [{ "key": key, "x": x + 400.0, "y": y + 200.0, "pinned": true }], "baseLayoutVersion": base })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{out}");
        // The range's hunk window for auth2 sits where its symbol now sits: moved by the same delta.
        let after = rect(&auth2(&app.windows(&range).await));
        assert_eq!(
            (after.0 - before.0, after.1 - before.1),
            (400.0, 200.0),
            "the range view follows the move"
        );
        let (_, rb2) = app.get(&format!("/api/v1/views/{range}/bootstrap")).await;
        assert_eq!(rb2["view"]["layoutVersion"].as_u64().unwrap(), range_base + 1);
    });
}

fn rect4(v: &Value) -> (f64, f64, f64, f64) {
    let r = &v["rect"];
    let f = |k: &str| r[k].as_f64().unwrap();
    (f("x"), f("y"), f("w"), f("h"))
}

#[test]
fn expand_pushes_what_it_would_cover_down() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    // A file over 120 lines with two long functions: two windows of one file, one under the other.
    let long = |name: &str| {
        let body: String = (0..70).map(|i| format!("    y = y + {i}\n")).collect();
        format!("def {name}(x):\n    y = x\n{body}    return y\n\n\n")
    };
    let big = format!("{}{}", long("first"), long("second"));
    for (p, text) in files()
        .into_iter()
        .chain([("src/auth/big.py".to_string(), big)])
    {
        let full = repo.join(&p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    jj(&repo, &["commit", "-m", "base"]);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (_state, app) = start(&repo, &store);
    rt.block_on(async {
        let snap = app.view("@-").await;
        let overlaps =
            |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| a.0 < b.0 + b.2 && b.0 < a.0 + a.2;
        let ws = app.windows(&snap).await;
        // big.py's first window can grow (the file goes on); put its second window (same cluster)
        // in the row under it, a layout gap below.
        let w = by_key(&ws, "src/auth/big.py#first").clone();
        let r0 = rect4(&w);
        let other = by_key(&ws, "src/auth/big.py#second").clone();
        assert_eq!(other["cluster"], w["cluster"]);
        let (_, b) = app.get(&format!("/api/v1/views/{snap}/bootstrap")).await;
        let base = b["view"]["layoutVersion"].as_u64().unwrap();
        let (st, out) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{snap}/layout"),
                Some(json!({ "moves": [{ "key": other["key"], "x": r0.0, "y": r0.1 + r0.3 + 90.0, "pinned": false }], "baseLayoutVersion": base })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{out}");
        let ws = app.windows(&snap).await;
        let below: Vec<Value> = ws
            .iter()
            .filter(|o| o["cluster"] == w["cluster"] && o["id"] != w["id"] && rect4(o).1 >= r0.1 + r0.3 - 1.0)
            .cloned()
            .collect();
        assert!(below.iter().any(|o| o["id"] == other["id"]), "{below:?}");
        let id = w["id"].as_str().unwrap();
        let (st, h) = app
            .call(
                "POST",
                &format!("/api/v1/views/{snap}/windows/{id}/expand"),
                Some(json!({ "after": "file" })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{h}");
        assert_eq!(h["layoutVersion"].as_u64().unwrap(), base + 2, "{h}");
        assert_eq!(h["lines"]["end"], w["lines"]["fileLines"]);
        let r1 = rect4(&h);
        let grow = r1.3 - r0.3;
        assert!(grow > 0.0, "it grew: {h}");
        assert_eq!((r1.0, r1.1), (r0.0, r0.1), "its top stays where it was");
        // Tiles serve the widened window; the rows below moved down together by the growth (the
        // moved window sat exactly one gap under it).
        let after = app.windows(&snap).await;
        assert_eq!(rect4(by_key(&after, "src/auth/big.py#first")), r1);
        for o in &below {
            let n = after.iter().find(|n| n["id"] == o["id"]).unwrap();
            assert_eq!(rect4(n).1 - rect4(o).1, grow, "{} moved down by the growth", o["key"]);
        }
        // Nothing of the cluster is covered by the widened window.
        for o in after.iter().filter(|o| o["cluster"] == w["cluster"] && o["id"] != w["id"]) {
            let r = rect4(o);
            let covered = overlaps(r1, r) && r.1 < r1.1 + r1.3 && r1.1 < r.1 + r.3;
            assert!(!covered, "{} is covered: {r:?} vs {r1:?}", o["key"]);
        }
        // The header says what it was expanded from.
        assert_eq!(h["expandedFrom"]["start"], w["lines"]["start"], "{h}");
        assert_eq!(h["expandedFrom"]["end"], w["lines"]["end"], "{h}");

        // Collapse puts back exactly what the expand changed: the window's span and height, and
        // every window and cluster frame it moved.
        let (_, b1) = app.get(&format!("/api/v1/views/{snap}/bootstrap")).await;
        let (st, c) = app
            .call(
                "POST",
                &format!("/api/v1/views/{snap}/windows/{id}/collapse"),
                None,
                false,
            )
            .await;
        assert_eq!(st, 200, "{c}");
        assert_eq!(c["layoutVersion"].as_u64().unwrap(), base + 3, "{c}");
        assert_eq!(c["lines"], w["lines"], "{c}");
        assert!(c.get("expandedFrom").is_none(), "{c}");
        assert_eq!(rect4(&c), r0);
        let back = app.windows(&snap).await;
        for o in &ws {
            let n = back.iter().find(|n| n["id"] == o["id"]).unwrap();
            assert_eq!(rect4(n), rect4(o), "{} is back", o["key"]);
        }
        let (_, b2) = app.get(&format!("/api/v1/views/{snap}/bootstrap")).await;
        let frame = |b: &Value| {
            let c = b["clusters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == w["cluster"])
                .unwrap()
                .clone();
            rect4(&c)
        };
        assert!(frame(&b2).3 < frame(&b1).3, "the frame shrank back");
        assert!(
            back.iter()
                .filter(|o| o["cluster"] == w["cluster"])
                .all(|o| rect4(o).1 + rect4(o).3 <= frame(&b2).1 + frame(&b2).3),
            "and still holds its windows"
        );
        // Collapsing again is refused: it is not expanded.
        let (st, _) = app
            .call(
                "POST",
                &format!("/api/v1/views/{snap}/windows/{id}/collapse"),
                None,
                false,
            )
            .await;
        assert_eq!(st, 409);
    });
}

/// A window pinned far from its cluster floats on its own: the cluster's frame does not stretch
/// across the canvas to hold it (a huge box over its neighbours), before or after a restart, and
/// a pin on that cluster still moves the cluster's other windows with it.
#[test]
fn far_pinned_window_does_not_stretch_its_cluster() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    for (p, text) in files() {
        let full = repo.join(&p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    jj(&repo, &["commit", "-m", "base"]);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let cluster = |b: &Value, id: &str| {
        b["clusters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap()
            .clone()
    };
    let (_state, app) = start(&repo, &store);
    let (key, to, far, size0) = rt.block_on(async {
        let snap = app.view("@-").await;
        let ws = app.windows(&snap).await;
        let w = ws
            .iter()
            .find(|w| w["path"] == "src/auth/auth2.py")
            .unwrap()
            .clone();
        let (key, cid) = (
            w["key"].as_str().unwrap().to_string(),
            w["cluster"].as_str().unwrap().to_string(),
        );
        let (_, b) = app.get(&format!("/api/v1/views/{snap}/bootstrap")).await;
        let c0 = cluster(&b, &cid);
        let size0 = (
            c0["rect"]["w"].as_f64().unwrap(),
            c0["rect"]["h"].as_f64().unwrap(),
        );
        let base = b["view"]["layoutVersion"].as_u64().unwrap();
        // Far below the whole world.
        let far = (
            c0["rect"]["x"].as_f64().unwrap(),
            b["world"]["y"].as_f64().unwrap() + b["world"]["h"].as_f64().unwrap() + 30_000.0,
        );
        let (st, out) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{snap}/layout"),
                Some(json!({ "moves": [{ "key": key, "x": far.0, "y": far.1, "pinned": true }], "baseLayoutVersion": base })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{out}");
        let (_, b) = app.get(&format!("/api/v1/views/{snap}/bootstrap")).await;
        let r = &cluster(&b, &cid)["rect"];
        assert!(
            r["h"].as_f64().unwrap() <= size0.1 + 1.0,
            "the frame stretched to the pinned window: {r}"
        );
        // Then the cluster itself is dragged and pinned, 3000 to the right.
        let to = (
            r["x"].as_f64().unwrap() + 3_000.0,
            r["y"].as_f64().unwrap(),
        );
        let base = b["view"]["layoutVersion"].as_u64().unwrap();
        let (st, out) = app
            .call(
                "PATCH",
                &format!("/api/v1/views/{snap}/layout"),
                Some(json!({ "moves": [{ "key": format!("cluster:{cid}"), "x": to.0, "y": to.1, "pinned": true }], "baseLayoutVersion": base })),
                false,
            )
            .await;
        assert_eq!(st, 200, "{out}");
        let ws = app.windows(&snap).await;
        assert_eq!(rect(by_key(&ws, &key)), far, "a pinned window stays put");
        (key, to, far, size0)
    });
    // After a restart the pin is honoured, and the frame still holds only the other windows.
    // After a restart both pins are honoured: the window where it was pinned, the cluster (with
    // its other windows) at its pin, and the frame holds only the other windows.
    let (_state, app) = restart(&repo, &store);
    rt.block_on(async {
        let snap = app.view("@-").await;
        let ws = app.windows(&snap).await;
        assert_eq!(rect(by_key(&ws, &key)), far);
        let (_, b) = app.get(&format!("/api/v1/views/{snap}/bootstrap")).await;
        let cid = by_key(&ws, &key)["cluster"].as_str().unwrap().to_string();
        let c = cluster(&b, &cid);
        assert_eq!(c["pinned"], true, "{c}");
        let r = &c["rect"];
        assert_eq!(
            (r["x"].as_f64().unwrap(), r["y"].as_f64().unwrap()),
            to,
            "{c}"
        );
        assert!(
            r["h"].as_f64().unwrap() <= size0.1 + 1.0 && r["w"].as_f64().unwrap() <= size0.0 + 1.0,
            "after a restart the frame stretched to the pinned window: {r}"
        );
        let rr = Rect4::of(r);
        for w in ws.iter().filter(|w| w["cluster"] == cid && w["key"] != key) {
            assert!(rr.holds(&Rect4::of(&w["rect"])), "{} outside {r}", w["key"]);
        }
    });
}

struct Rect4(f64, f64, f64, f64);

impl Rect4 {
    fn of(v: &Value) -> Self {
        let f = |k: &str| v[k].as_f64().unwrap();
        Rect4(f("x"), f("y"), f("w"), f("h"))
    }
    fn holds(&self, o: &Rect4) -> bool {
        o.0 >= self.0
            && o.1 >= self.1
            && o.0 + o.2 <= self.0 + self.2
            && o.1 + o.3 <= self.1 + self.3
    }
}
