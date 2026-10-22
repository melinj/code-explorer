//! review.db records in tables (D5): a review.db from an earlier build (schema version 1, link
//! verdicts, saved ranges and pins as JSON in `meta`) is upgraded on open, serves the same HTTP
//! responses, and keeps them across restarts. Skipped when `jj` is not on PATH.

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

fn start(repo: &Path, store: &Path) -> (Arc<AppState>, axum::Router) {
    let root = cv_vcs::JjRepo::open(repo)
        .unwrap()
        .root()
        .display()
        .to_string();
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
    (state, router)
}

async fn get(app: &axum::Router, uri: &str) -> (u16, Value) {
    let res = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let st = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// What the routes answer about ranges, pins and verdicts.
async fn observe(app: &axum::Router, view: &str, pinned_key: &str, link_id: &str) -> Value {
    let (_, ranges) = get(app, "/api/v1/ranges").await;
    let (_, b) = get(app, &format!("/api/v1/views/{view}/bootstrap")).await;
    let w = &b["world"];
    let mut pinned = Value::Null;
    let mut edge_seen = false;
    let span = cv_contracts::TILE_BASE_WU * 4.0;
    let cell = |v: f64| (v / span).floor() as i64;
    let (x0, y0) = (w["x"].as_f64().unwrap(), w["y"].as_f64().unwrap());
    let (x1, y1) = (x0 + w["w"].as_f64().unwrap(), y0 + w["h"].as_f64().unwrap());
    for tx in cell(x0) - 1..=cell(x1) + 1 {
        for ty in cell(y0) - 1..=cell(y1) + 1 {
            let (_, t) = get(app, &format!("/api/v1/views/{view}/tiles/2/{tx}/{ty}")).await;
            for h in t["windows"].as_array().into_iter().flatten() {
                if h["key"] == pinned_key {
                    pinned = json!([h["rect"]["x"], h["rect"]["y"], h["flags"]["pinned"]]);
                }
            }
            edge_seen |= t["edges"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|e| e["id"] == link_id);
        }
    }
    json!({ "ranges": ranges, "pinned": pinned, "rejectedEdgeShown": edge_seen })
}

#[test]
fn version_1_review_db_upgrades_and_serves_the_same() {
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
    let store = tmp.path().join("store");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::write(
        repo.join("src/a.py"),
        "from .b import g\n\n\ndef f(x):\n    return g(x)\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/b.py"), "def g(x):\n    return x + 1\n").unwrap();
    jj(&repo, &["commit", "-m", "base"]);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // A view, one of its links and a window to pin.
    let (state, _) = start(&repo, &store);
    let v = state
        .view_for_spec(&json!({ "mode": "snapshot", "rev": "@-" }))
        .unwrap();
    let l = v
        .links
        .iter()
        .find(|l| l.channel.is_none() && l.from != l.to)
        .expect("a code link");
    let link = cv_server::extras_api::render_link(&v, l);
    let key = cv_server::extras_api::link_key(&link);
    let link_id = l.id.clone();
    let pinned_key = v.windows[l.to].header.key.clone();
    drop((state, v));

    // Turn review.db into what a version-1 build left: schema 1, records as JSON in meta.
    let db_path = cv_store::StorePaths::in_dir(&store).review_db;
    {
        let c = cv_store::rusqlite::Connection::open(&db_path).unwrap();
        c.execute_batch(
            "DROP TABLE layout_pin; DROP TABLE cluster_pin; DROP TABLE link_feedback;
             DROP TABLE cluster_label;
             CREATE TABLE layout_pin (view_key TEXT NOT NULL, item_key TEXT NOT NULL, x REAL NOT NULL,
               y REAL NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY (view_key, item_key)) STRICT, WITHOUT ROWID;
             CREATE TABLE link_feedback (link_key TEXT PRIMARY KEY, verdict TEXT NOT NULL, actor_json TEXT NOT NULL,
               note TEXT, at TEXT NOT NULL) STRICT, WITHOUT ROWID;
             CREATE TABLE cluster_label (members_hash TEXT PRIMARY KEY, members_json TEXT NOT NULL,
               label TEXT NOT NULL, summary TEXT, source TEXT NOT NULL, at TEXT NOT NULL) STRICT;
             UPDATE meta SET value = '1' WHERE key = 'schema_version';",
        )
        .unwrap();
        let by = json!({ "kind": "human", "name": "you" });
        let records = [
            (
                "cv.links.v1",
                json!({ "declared": [], "ratings": [{
                    "key": key, "id": link_id, "verdict": "rejected", "by": by,
                    "at": "2026-10-18T00:00:00.000Z" }] }),
            ),
            (
                "cv.ranges.v1",
                json!([
                    { "name": "wip", "revset": "trunk()..@", "createdBy": by },
                    { "name": "base", "revset": "@-", "fileset": "src/", "createdBy": by }
                ]),
            ),
            (
                "cv.layout.v1",
                json!({ "pins": { pinned_key.clone(): [-9000.0, -7000.0] } }),
            ),
        ];
        for (k, v) in records {
            c.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                cv_store::rusqlite::params![k, v.to_string()],
            )
            .unwrap();
        }
    }

    // Upgraded on open; the routes see the records.
    let (state, app) = start(&repo, &store);
    let vid = state
        .view_for_spec(&json!({ "mode": "snapshot", "rev": "@-" }))
        .unwrap()
        .id()
        .to_string();
    let first = rt.block_on(observe(&app, &vid, &pinned_key, &link_id));
    assert_eq!(
        first["ranges"],
        json!([
            { "name": "wip", "revset": "trunk()..@", "createdBy": { "kind": "human", "name": "you" } },
            { "name": "base", "revset": "@-", "fileset": "src/", "createdBy": { "kind": "human", "name": "you" } }
        ])
    );
    assert_eq!(first["pinned"], json!([-9000.0, -7000.0, true]));
    assert_eq!(first["rejectedEdgeShown"], false);
    let db = cv_store::ReviewDb::open(&db_path).unwrap();
    assert_eq!(db.meta("schema_version").unwrap().as_deref(), Some("2"));
    for k in ["cv.links.v1", "cv.ranges.v1", "cv.layout.v1"] {
        assert_eq!(db.meta(k).unwrap(), None);
    }
    assert_eq!(db.link_records().unwrap().1.len(), 1);
    assert_eq!(db.saved_ranges().unwrap().len(), 2);
    assert_eq!(db.layout_records().unwrap().pins.len(), 1);
    drop(db);

    // Writes go to the tables; a restart serves the same.
    rt.block_on(async {
        let res = app
            .clone()
            .oneshot(
                Request::put("/api/v1/ranges/later")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "name": "later", "revset": "@" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 200);
        let res = app
            .clone()
            .oneshot(
                Request::delete("/api/v1/ranges/wip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 200);
    });
    let before = rt.block_on(observe(&app, &vid, &pinned_key, &link_id));
    drop((state, app));
    let (state, app) = start(&repo, &store);
    let vid2 = state
        .view_for_spec(&json!({ "mode": "snapshot", "rev": "@-" }))
        .unwrap()
        .id()
        .to_string();
    assert_eq!(vid, vid2);
    let after = rt.block_on(observe(&app, &vid2, &pinned_key, &link_id));
    assert_eq!(before, after);
    let names: Vec<&str> = after["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["base", "later"]);
}
