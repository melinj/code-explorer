//! Links, hubs, declared links, ratings, cluster labels, agent navigation and interdiff
//! (crate::extras_api) through the router on a scripted jj repo, including the 403/404 paths and
//! declared links and ratings surviving a server restart (review.db on disk). Skipped when `jj`
//! is not on PATH.

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

const FILES: &[(&str, &str)] = &[
    ("src/app/events.py", "MOVED = \"world.entity.moved\"\n"),
    (
        "src/app/presence.py",
        r#"from .events import MOVED


class Presence:
    def moved(self):
        self._bus.emit(EventEnvelope(topic=MOVED, payload=1))
"#,
    ),
    (
        "src/app/flow.yaml",
        "pool:\n  - id: moved_sub\n    config_id: event_subscribe\n    params:\n      topic: world.entity.moved\n",
    ),
    (
        "src/a.py",
        "def f0(x):\n    y = x + 1\n    return y\n\n\ndef f1(x):\n    return f0(x) * 2\n",
    ),
    (
        "src/b.py",
        "from a import f0\n\n\ndef main():\n    return f0(1)\n",
    ),
];

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

fn ids(links: &Value) -> Vec<String> {
    links
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn links_hubs_feedback_labels_navigation_interdiff() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    let store = tmp.path().join("store");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    for (p, text) in FILES {
        let full = repo.join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    jj(&repo, &["commit", "-m", "base"]);
    std::fs::write(
        repo.join("src/b.py"),
        "from a import f0\n\n\ndef main():\n    return f0(2)\n",
    )
    .unwrap();
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
    let reviewed = jj(
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
    // The change moves on after "review": new text and a new description.
    std::fs::write(
        repo.join("src/b.py"),
        "from a import f0\n\n\ndef main():\n    return f0(3) + 1\n",
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "work, v2"]);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (state, app) = start(&repo, &store);
    let (vid, rejected_id, declared_id) = rt.block_on(async {
        let (st, view) = app.call("POST", "/api/v1/views", Some(json!({ "mode": "snapshot", "rev": "@" })), false).await;
        assert_eq!(st, 200, "{view}");
        let vid = view["id"].as_str().unwrap().to_string();

        // ── GET /links ──
        let (st, r) = app.get(&format!("/api/v1/links?viewId={vid}&target=src/b.py:5&dir=out")).await;
        assert_eq!(st, 200, "{r}");
        let call = r["links"].as_array().unwrap().iter().find(|l| l["kind"] == "call").cloned().unwrap_or_else(|| panic!("no call link: {r}"));
        assert_eq!(call["to"]["path"], "src/a.py");
        assert_eq!(call["from"]["path"], "src/b.py");
        // By symbol (a declaration inside a window), incoming, calls only.
        let (_, r) = app.get(&format!("/api/v1/links?viewId={vid}&target=f0&dir=in&kinds=call")).await;
        let froms: Vec<&str> = r["links"].as_array().unwrap().iter().map(|l| l["from"]["path"].as_str().unwrap()).collect();
        assert_eq!(froms, vec!["src/b.py"], "{r}");
        assert!(r["links"].as_array().unwrap().iter().all(|l| l["kind"] == "call"));
        assert!(r["hubs"].as_array().unwrap().is_empty(), "no channel kinds asked: {r}");
        // Channel links bring their hubs.
        let (_, r) = app.get(&format!("/api/v1/links?viewId={vid}&target=src/app/presence.py:6&dir=out")).await;
        let hubs: Vec<&str> = r["hubs"].as_array().unwrap().iter().map(|h| h["id"].as_str().unwrap()).collect();
        assert!(hubs.contains(&"bus:world.entity.moved"), "{r}");
        assert!(r["links"].as_array().unwrap().iter().any(|l| l["kind"] == "publish" || l["kind"] == "subscribe"), "{r}");
        // Errors.
        let (st, e) = app.get(&format!("/api/v1/links?viewId={vid}&target=nope_nothing")).await;
        assert_eq!(st, 404, "{e}");
        assert_eq!(e["error"]["code"], "not-found");
        let (st, _) = app.get("/api/v1/links?viewId=v_nope&target=f0").await;
        assert_eq!(st, 404);
        let (st, _) = app.get(&format!("/api/v1/links?viewId={vid}&target=f0&dir=sideways")).await;
        assert_eq!(st, 400);

        // ── GET /hubs/:hub ──
        let (st, h) = app.get(&format!("/api/v1/hubs/bus:world.entity.moved?viewId={vid}")).await;
        assert_eq!(st, 200, "{h}");
        assert_eq!(h["key"], "world.entity.moved");
        assert_eq!(h["producers"][0]["path"], "src/app/presence.py");
        let (st, h) = app.get(&format!("/api/v1/hubs/world.entity.moved?viewId={vid}&protocol=bus")).await;
        assert_eq!((st, h["id"].as_str()), (200, Some("bus:world.entity.moved")));
        let (st, e) = app.get(&format!("/api/v1/hubs/bus:nope?viewId={vid}")).await;
        assert_eq!(st, 404, "{e}");

        // ── POST /links ──
        let (st, d) = app.call("POST", "/api/v1/links", Some(json!({
            "from": "src/b.py:5", "to": "src/app/presence.py:6", "kind": "reference",
            "label": "main pokes presence", "note": "seen in prod logs"
        })), true).await;
        assert_eq!(st, 200, "{d}");
        assert_eq!(d["provenance"], "agent");
        assert_eq!(d["note"], "seen in prod logs");
        let declared_id = d["id"].as_str().unwrap().to_string();
        assert!(declared_id.starts_with("dl-"));
        let (st, h) = app.call("POST", "/api/v1/links", Some(json!({
            "from": "src/a.py:2", "to": "bus:world.entity.moved", "kind": "publish", "label": "human says so"
        })), false).await;
        assert_eq!(st, 200, "{h}");
        assert_eq!(h["provenance"], "human");
        assert_eq!(h["to"]["hub"], "bus:world.entity.moved");
        let (st, _) = app.call("POST", "/api/v1/links", Some(json!({ "from": "src/a.py:2", "to": "src/b.py:1", "kind": "teleport", "label": "x" })), true).await;
        assert_eq!(st, 400);
        let (st, _) = app.call("POST", "/api/v1/links", Some(json!({ "from": "bus:world.entity.moved", "to": "src/b.py:1", "kind": "subscribe", "label": "x" })), true).await;
        assert_eq!(st, 400);
        // The UI hears about it: view.invalidated (index) naming both windows.
        let (_, ev) = app.get("/api/v1/events?cursor=0&kinds=view.invalidated&timeoutS=1").await;
        let inv = ev["events"].as_array().unwrap().iter().find(|e| e["reason"] == "index").cloned().unwrap_or_else(|| panic!("{ev}"));
        assert_eq!(inv["viewId"], vid.as_str());
        assert_eq!(inv["windows"].as_array().unwrap().len(), 2, "{inv}");
        // windows:batch serves it with a port on the callsite line.
        let from_w = inv["windows"][0].as_str().unwrap().to_string();
        let (st, b) = app.call("POST", &format!("/api/v1/views/{vid}/windows:batch"), Some(json!({ "ids": [from_w], "parts": ["outline"], "layoutVersion": 0 })), false).await;
        assert_eq!(st, 200);
        let batch_ids = ids(&b["links"]);
        let (_, r) = app.get(&format!("/api/v1/links?viewId={vid}&target=src/b.py:5&dir=out")).await;
        assert!(ids(&r["links"]).contains(&declared_id), "{r}");
        let in_batch = batch_ids.contains(&declared_id);
        if in_batch {
            let ports = b["outlines"][0]["ports"].as_array().unwrap();
            assert!(ports.iter().any(|p| p["links"].as_array().unwrap().iter().any(|l| l == declared_id.as_str())), "{b}");
        } else {
            // The first window was the target's: ask for both.
            let (_, b) = app.call("POST", &format!("/api/v1/views/{vid}/windows:batch"), Some(json!({ "ids": inv["windows"], "parts": ["outline"], "layoutVersion": 0 })), false).await;
            assert!(ids(&b["links"]).contains(&declared_id), "{b}");
        }

        // ── POST /links/:id/rate ──
        let (st, rated) = app.call("POST", &format!("/api/v1/links/{declared_id}/rate"), Some(json!({ "verdict": "confirmed" })), true).await;
        assert_eq!(st, 200, "{rated}");
        assert_eq!(rated["feedback"], "confirmed");
        let rejected_id = call["id"].as_str().unwrap().to_string();
        // The call link is in the tile covering the world before it is rejected, and not after.
        let tile_of = |id: &str, t: &Value| t["edges"].as_array().unwrap().iter().any(|e| e["id"] == id);
        let (_, boot) = app.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
        let w = &boot["world"];
        let tile_uri = format!("/api/v1/views/{vid}/tiles/12/{}/{}", (w["x"].as_f64().unwrap() / cv_contracts::TILE_BASE_WU / 4096.0).floor(), (w["y"].as_f64().unwrap() / cv_contracts::TILE_BASE_WU / 4096.0).floor());
        let (st, t) = app.get(&tile_uri).await;
        assert_eq!(st, 200, "{t}");
        assert!(tile_of(&rejected_id, &t), "{t}");
        let (st, rj) = app.call("POST", &format!("/api/v1/links/{rejected_id}/rate"), Some(json!({ "verdict": "rejected", "note": "not a real call", "viewId": vid })), false).await;
        assert_eq!(st, 200, "{rj}");
        assert_eq!(rj["feedback"], "rejected");
        let (_, t) = app.get(&tile_uri).await;
        assert!(!tile_of(&rejected_id, &t), "rejected links leave the tiles");
        let (_, r) = app.get(&format!("/api/v1/links?viewId={vid}&target=src/b.py:5&dir=out")).await;
        let fb = |id: &str, r: &Value| r["links"].as_array().unwrap().iter().find(|l| l["id"] == id).map(|l| l["feedback"].clone());
        assert_eq!(fb(&rejected_id, &r), Some(json!("rejected")), "{r}");
        assert_eq!(fb(&declared_id, &r), Some(json!("confirmed")), "{r}");
        let (st, _) = app.call("POST", "/api/v1/links/l99999/rate", Some(json!({ "verdict": "confirmed" })), true).await;
        assert_eq!(st, 404);
        let (st, _) = app.call("POST", &format!("/api/v1/links/{declared_id}/rate"), Some(json!({ "verdict": "meh" })), true).await;
        assert_eq!(st, 400);

        // ── PUT /clusters/:id/label ──
        let (st, ok) = app.call("PUT", "/api/v1/clusters/c0/label", Some(json!({ "label": "Event bus", "summary": "Publishes entity moves" })), true).await;
        assert_eq!(st, 200, "{ok}");
        assert_eq!(ok, json!({ "ok": true }));
        let (_, boot) = app.get(&format!("/api/v1/views/{vid}/bootstrap")).await;
        let c0 = boot["clusters"].as_array().unwrap().iter().find(|c| c["id"] == "c0").unwrap();
        assert_eq!((c0["label"].as_str(), c0["labelSource"].as_str(), c0["summary"].as_str()), (Some("Event bus"), Some("agent"), Some("Publishes entity moves")));
        let (_, ev) = app.get("/api/v1/events?cursor=0&kinds=view.invalidated&timeoutS=1").await;
        assert!(ev["events"].as_array().unwrap().iter().any(|e| e["reason"] == "layout" && e["windows"] == json!([])), "{ev}");
        let (st, _) = app.call("PUT", "/api/v1/clusters/c999/label", Some(json!({ "label": "x" })), true).await;
        assert_eq!(st, 404);
        let (st, _) = app.call("PUT", "/api/v1/clusters/c0/label", Some(json!({ "label": "  " })), true).await;
        assert_eq!(st, 400);

        // ── POST /agent/view ──
        let spec = json!({ "mode": "snapshot", "rev": "@-" });
        let (st, e) = app.call("POST", "/api/v1/agent/view", Some(json!({ "spec": spec })), true).await;
        assert_eq!(st, 403, "{e}");
        assert_eq!(e["error"]["code"], "not-allowed");
        // "districts" is the only layout: "dependency" (never specified) is a 400, not ignored.
        for spec in [
            json!({ "mode": "snapshot", "rev": "@-", "options": { "layout": "dependency" } }),
            json!({ "mode": "range", "revset": "@-", "options": { "diff": "cumulative", "contextHops": 0, "contextDisplay": "minimized", "contextLines": 3, "ignoreWhitespace": false, "layout": "dependency" } }),
        ] {
            let (st, e) = app.call("POST", "/api/v1/views", Some(spec), false).await;
            assert_eq!(st, 400, "{e}");
            assert_eq!(e["error"]["code"], "invalid");
        }
        let (st, e) = app.call("POST", "/api/v1/views", Some(json!({ "mode": "snapshot", "rev": "@-", "options": { "layout": "districts" } })), false).await;
        assert_eq!(st, 200, "{e}");

        // Unknown fields (a misspelt toggle) and wrong types are a 400, not a silent no-op.
        let (st, e) = app.call("PUT", "/api/v1/agent/permissions", Some(json!({ "move_camera": true })), false).await;
        assert_eq!(st, 400, "{e}");
        assert_eq!(e["error"]["code"], "invalid");
        let (st, _) = app.call("PUT", "/api/v1/agent/permissions", Some(json!({ "edit": "yes" })), false).await;
        assert_eq!(st, 400);
        let (st, _) = app.call("PUT", "/api/v1/agent/permissions", Some(json!({ "moveCamera": true })), false).await;
        assert_eq!(st, 200);
        let (st, vr) = app.call("POST", "/api/v1/agent/view", Some(json!({ "spec": spec })), true).await;
        app.call("PUT", "/api/v1/agent/permissions", Some(json!({ "moveCamera": false })), false).await;
        assert_eq!(st, 200, "{vr}");
        let (_, ev) = app.get("/api/v1/events?cursor=0&kinds=view.requested&timeoutS=1").await;
        let req = &ev["events"][0];
        assert_eq!(req["viewId"], vr["id"], "{ev}");
        assert_eq!(req["spec"], spec);
        assert_eq!(req["by"]["kind"], "agent");

        // ── GET /interdiff ──
        let (st, d) = app.get(&format!("/api/v1/interdiff?change={change}&from={reviewed}")).await;
        assert_eq!(st, 200, "{d}");
        let files = d["files"].as_array().unwrap();
        assert_eq!(files.len(), 1, "{d}");
        assert_eq!(files[0]["path"], "src/b.py");
        assert_eq!(files[0]["status"], "modified");
        let lines = files[0]["hunks"][0]["lines"].as_array().unwrap();
        assert!(lines.iter().any(|l| l["kind"] == "del" && l["text"] == "    return f0(2)"), "{d}");
        assert!(lines.iter().any(|l| l["kind"] == "add" && l["text"] == "    return f0(3) + 1" && l["newLine"] == 5), "{d}");
        assert_eq!(d["description"], json!(["work\n", "work, v2\n"]));
        let (st, _) = app.get(&format!("/api/v1/interdiff?change=nope_change&from={reviewed}")).await;
        assert_eq!(st, 404);

        (vid, rejected_id, declared_id)
    });
    drop(app);
    drop(state);

    // ── Restart: declared links and ratings come back from review.db ──
    let (_state, app) = start(&repo, &store);
    rt.block_on(async {
        let (_, view) = app
            .call(
                "POST",
                "/api/v1/views",
                Some(json!({ "mode": "snapshot", "rev": "@" })),
                false,
            )
            .await;
        let vid2 = view["id"].as_str().unwrap().to_string();
        let _ = vid;
        let (st, r) = app
            .get(&format!(
                "/api/v1/links?viewId={vid2}&target=src/b.py:5&dir=out"
            ))
            .await;
        assert_eq!(st, 200, "{r}");
        let find = |id: &str| {
            r["links"]
                .as_array()
                .unwrap()
                .iter()
                .find(|l| l["id"] == id)
                .cloned()
        };
        let d = find(&declared_id).unwrap_or_else(|| panic!("declared link survives: {r}"));
        assert_eq!(d["feedback"], "confirmed");
        assert_eq!(d["provenance"], "agent");
        let call = r["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["kind"] == "call")
            .unwrap();
        assert_eq!(
            call["feedback"], "rejected",
            "rating keyed by what the link connects: {r}"
        );
        let _ = rejected_id;
    });
}

#[test]
fn interdiff_hunks_have_context_and_counts() {
    let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
    let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\n";
    let h = cv_server::extras_api::hunks(old, new);
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].old, [2, 8]);
    assert_eq!(h[0].new, [2, 9]);
    let kinds: String = h[0]
        .lines
        .iter()
        .map(|l| match l.kind.as_str() {
            "ctx" => 'c',
            "add" => '+',
            _ => '-',
        })
        .collect();
    assert_eq!(kinds, "ccc-+cccc+");
}
