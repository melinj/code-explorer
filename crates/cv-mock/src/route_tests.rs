//! The routes cv-mock added for the UI (GET view/thread/edit, undo, hubs, interdiff, declared and
//! rated links, cluster labels, agent navigation, review marks): status codes and response shapes.

use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct T {
    router: Router,
}

impl T {
    fn new() -> (Self, std::sync::Arc<crate::App>) {
        let app = crate::build(1500, Duration::ZERO, false);
        (
            T {
                router: crate::router(app.clone()),
            },
            app,
        )
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let res = self
            .router
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 26)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.call(Method::GET, uri, None).await
    }

    async fn post(&self, uri: &str, b: Value) -> (StatusCode, Value) {
        self.call(Method::POST, uri, Some(b)).await
    }

    async fn put(&self, uri: &str, b: Value) -> (StatusCode, Value) {
        self.call(Method::PUT, uri, Some(b)).await
    }

    async fn events(&self, kind: &str) -> Vec<Value> {
        let (_, e) = self
            .get(&format!("/api/v1/events?cursor=0&kinds={kind}&timeoutS=0"))
            .await;
        e["events"].as_array().cloned().unwrap_or_default()
    }
}

#[tokio::test]
async fn view_thread_and_edit_by_id_and_undo() {
    let (t, app) = T::new();
    let view = app.view().bootstrap.view.id.clone();
    let (s, v) = t.get(&format!("/api/v1/views/{view}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["id"], view.as_str());
    assert!(v["layoutVersion"].is_number() && v["resolved"]["head"].is_string());
    assert_eq!(t.get("/api/v1/views/nope").await.0, StatusCode::NOT_FOUND);
    // A range view is found by id too.
    let (_, r) = t
        .post(
            "/api/v1/views",
            json!({ "mode": "range", "revset": "trunk()..@" }),
        )
        .await;
    let (s, rv) = t
        .get(&format!("/api/v1/views/{}", r["id"].as_str().unwrap()))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(rv["sharedPlacement"], view.as_str());

    let path = app.view().files[0].path.clone();
    let (s, th) = t
        .post(
            "/api/v1/threads",
            json!({ "anchor": { "path": path, "rev": "c0ffee", "start": 1, "end": 2 }, "body": "hm" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let id = th["id"].as_str().unwrap();
    let (s, got) = t.get(&format!("/api/v1/threads/{id}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(got["id"], id);
    assert!(got["comments"].is_array());
    assert_eq!(t.get("/api/v1/threads/nope").await.0, StatusCode::NOT_FOUND);

    let blob = app
        .view()
        .windows
        .iter()
        .find(|w| w.header.path == path)
        .unwrap()
        .header
        .blob
        .clone();
    let line1 = |b: Value| b["lines"][0].as_str().unwrap_or("").to_string();
    let before = line1(
        t.get(&format!("/api/v1/blobs/{blob}/text?from=1&to=1"))
            .await
            .1,
    );
    let (s, e) = t
        .post(
            "/api/v1/edits",
            json!({ "target": "c0ffee", "mode": "amend", "dryRun": true,
                    "files": [{ "path": path, "patches": [{ "start": 1, "end": 1, "text": "// changed" }] }] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let eid = e["id"].as_str().unwrap();
    let (s, ge) = t.get(&format!("/api/v1/edits/{eid}")).await;
    assert_eq!(
        (s, ge["status"].as_str()),
        (StatusCode::OK, Some("previewed"))
    );
    assert_eq!(t.get("/api/v1/edits/nope").await.0, StatusCode::NOT_FOUND);
    // Only an applied edit can be undone.
    let (s, err) = t
        .post(&format!("/api/v1/edits/{eid}/undo"), json!({}))
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(err["error"]["code"], "invalid");
    assert_eq!(
        t.post(&format!("/api/v1/edits/{eid}/apply"), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let after = line1(
        t.get(&format!("/api/v1/blobs/{blob}/text?from=1&to=1"))
            .await
            .1,
    );
    assert_eq!(after, "// changed");
    let (s, u) = t
        .post(&format!("/api/v1/edits/{eid}/undo"), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(u["status"], "undone");
    assert!(u["opAfter"].is_string() && u["opBefore"].is_string());
    assert_eq!(
        t.get(&format!("/api/v1/edits/{eid}")).await.1["status"],
        "undone"
    );
    let restored = line1(
        t.get(&format!("/api/v1/blobs/{blob}/text?from=1&to=1"))
            .await
            .1,
    );
    assert_eq!(restored, before);
    let ev = t.events("edit.updated").await;
    assert!(ev.iter().any(|e| e["edit"]["status"] == "undone"));
    assert_eq!(
        t.post("/api/v1/edits/nope/undo", json!({})).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn hubs_interdiff_and_marks() {
    let (t, app) = T::new();
    let hub = app.view().bootstrap.hubs[0].clone();
    let (s, h) = t.get(&format!("/api/v1/hubs/{}", hub.id)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["id"], hub.id.as_str());
    assert!(h["producers"].is_array() && h["consumers"].is_array() && h["display"].is_string());
    // By key (additive), optionally narrowed by protocol.
    let (s, h) = t
        .get(&format!(
            "/api/v1/hubs/{}?protocol={}",
            hub.key, hub.protocol
        ))
        .await;
    assert_eq!(
        (s, h["key"].as_str()),
        (StatusCode::OK, Some(hub.key.as_str()))
    );
    assert_eq!(
        t.get("/api/v1/hubs/no-such-hub").await.0,
        StatusCode::NOT_FOUND
    );

    let row = app.panels.revisions[0].clone();
    let change = row["rev"]["change"].as_str().unwrap();
    let commit = row["rev"]["commit"].as_str().unwrap();
    let (s, d) = t
        .get(&format!("/api/v1/interdiff?change={change}&from=deadbeef"))
        .await;
    assert_eq!(s, StatusCode::OK);
    let f = &d["files"][0];
    assert!(f["path"].is_string() && f["status"].is_string());
    let hunk = &f["hunks"][0];
    assert_eq!(hunk["old"].as_array().unwrap().len(), 2);
    assert!(
        hunk["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["text"].is_string())
    );
    assert_eq!(d["description"].as_array().unwrap().len(), 2);
    assert_eq!(
        t.get("/api/v1/interdiff?change=zzzzzzzz&from=x").await.0,
        StatusCode::NOT_FOUND
    );

    let (s, m) = t
        .put(
            &format!("/api/v1/reviews/marks/{change}"),
            json!({ "commit": commit }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m["change"], change);
    assert_eq!(m["reviewedAt"], commit);
    assert_eq!(m["changedSinceReview"], false);
    assert!(m["openThreads"].is_number() && m["touchedByAgent"].is_boolean());
    let (_, m) = t
        .put(
            &format!("/api/v1/reviews/marks/{change}"),
            json!({ "commit": "0ld" }),
        )
        .await;
    assert_eq!(m["changedSinceReview"], true);
    assert_eq!(
        t.put("/api/v1/reviews/marks/zzzzzzzz", json!({ "commit": "x" }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn declared_and_rated_links() {
    let (t, app) = T::new();
    let v = app.view();
    let (a, b) = (&v.windows[0].header, &v.windows[1].header);
    let (s, l) = t
        .post(
            "/api/v1/links",
            json!({ "from": a.id, "to": format!("{}:{}", b.path, b.lines.start), "kind": "reference", "label": "uses", "note": "seen at runtime" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(l["provenance"], "human");
    assert_eq!(l["from"]["window"], a.id.as_str());
    assert_eq!(l["to"]["path"], b.path.as_str());
    assert_eq!(l["note"], "seen at runtime");
    let id = l["id"].as_str().unwrap();
    // To a hub.
    let (s, lh) = t
        .post(
            "/api/v1/links",
            json!({ "from": a.id, "to": v.bootstrap.hubs[0].id, "kind": "publish", "label": "emits" }),
        )
        .await;
    assert_eq!(
        (s, &lh["to"]["hub"]),
        (StatusCode::OK, &json!(v.bootstrap.hubs[0].id))
    );
    assert_eq!(
        t.post(
            "/api/v1/links",
            json!({ "from": "nowhere", "to": a.id, "kind": "call", "label": "" })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    let (s, r) = t
        .post(
            &format!("/api/v1/links/{id}/rate"),
            json!({ "verdict": "confirmed" }),
        )
        .await;
    assert_eq!(
        (s, r["feedback"].as_str()),
        (StatusCode::OK, Some("confirmed"))
    );
    let (_, ls) = t
        .get(&format!("/api/v1/links?target={}&dir=out", a.id))
        .await;
    let mine = ls["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .cloned();
    assert_eq!(mine.unwrap()["feedback"], "confirmed");

    let indexed = v.links[0].id.clone();
    let (s, r) = t
        .post(
            &format!("/api/v1/links/{indexed}/rate"),
            json!({ "verdict": "rejected", "note": "wrong overload" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (r["feedback"].as_str(), r["provenance"].as_str()),
        (Some("rejected"), Some("tree-sitter"))
    );
    assert_eq!(
        t.post(
            &format!("/api/v1/links/{id}/rate"),
            json!({ "verdict": "maybe" })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.post("/api/v1/links/nope/rate", json!({ "verdict": "confirmed" }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn cluster_labels_and_agent_view() {
    let (t, app) = T::new();
    let c = app.view().bootstrap.clusters[0].id.clone();
    let (s, ok) = t
        .put(
            &format!("/api/v1/clusters/{c}/label"),
            json!({ "label": "Event bus", "summary": "pub/sub core" }),
        )
        .await;
    assert_eq!((s, ok), (StatusCode::OK, json!({ "ok": true })));
    let view = app.view().bootstrap.view.id.clone();
    let (_, b) = t.get(&format!("/api/v1/views/{view}/bootstrap")).await;
    let cl = b["clusters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == c.as_str())
        .cloned()
        .unwrap();
    assert_eq!(
        (cl["label"].as_str(), cl["labelSource"].as_str()),
        (Some("Event bus"), Some("human"))
    );
    assert_eq!(cl["summary"], "pub/sub core");
    let inv = t.events("view.invalidated").await;
    assert!(
        inv.iter()
            .any(|e| e["reason"] == "layout" && e["windows"] == json!([]))
    );
    assert_eq!(
        t.put(
            &format!("/api/v1/clusters/{c}/label"),
            json!({ "label": " " })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.put("/api/v1/clusters/nope/label", json!({ "label": "x" }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    let spec = json!({ "spec": { "mode": "range", "revset": "trunk()..@" } });
    let (s, e) = t.post("/api/v1/agent/view", spec.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(e["error"]["code"], "not-allowed");
    assert_eq!(
        t.put("/api/v1/agent/permissions", json!({ "move_camera": true }))
            .await
            .0,
        StatusCode::BAD_REQUEST,
        "unknown fields are a 400"
    );
    assert_eq!(
        t.put("/api/v1/agent/permissions", json!({ "moveCamera": true }))
            .await
            .0,
        StatusCode::OK
    );
    let (s, vr) = t.post("/api/v1/agent/view", spec).await;
    assert_eq!(s, StatusCode::OK);
    assert!(vr["id"].is_string() && vr["resolved"].is_object());
    let req = t.events("view.requested").await;
    assert_eq!(req.len(), 1);
    assert_eq!(req[0]["viewId"], vr["id"]);
    assert_eq!(req[0]["spec"]["mode"], "range");
    let (s, _) = t
        .get(&format!("/api/v1/views/{}", vr["id"].as_str().unwrap()))
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn compare_workspaces_opens_a_per_commit_range_of_shared_files() {
    let (t, _app) = T::new();
    let (_, ws) = t.get("/api/v1/workspaces").await;
    let names: Vec<String> = ws
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap().to_string())
        .collect();
    // Two workspaces whose cards warn about an overlap.
    let gym = ws
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["overlaps"].as_array().is_some_and(|o| !o.is_empty()))
        .unwrap();
    let a = gym["name"].as_str().unwrap();
    let b = gym["overlaps"][0]["workspace"].as_str().unwrap();
    let (st, v) = t
        .post("/api/v1/workspaces:compare", json!({ "a": a, "b": b }))
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["spec"]["mode"], "range");
    assert_eq!(v["spec"]["options"]["diff"], "per-commit");
    assert_eq!(v["spec"]["options"]["allowNonChain"], true);
    let fileset = v["spec"]["fileset"].as_str().unwrap();
    assert!(fileset.starts_with('"'), "{fileset}");
    let id = v["id"].as_str().unwrap();
    let (st, boot) = t.get(&format!("/api/v1/views/{id}/bootstrap")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(boot["view"]["stats"]["windows"].as_u64().unwrap() > 0);

    let (st, e) = t
        .post("/api/v1/workspaces:compare", json!({ "a": a, "b": "nope" }))
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not-found"))
    );
    let (st, _) = t
        .post("/api/v1/workspaces:compare", json!({ "a": a, "b": a }))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(names.len() >= 2);
}

#[tokio::test]
async fn two_revision_diff() {
    let (t, _) = T::new();
    let (s, d) = t.get("/api/v1/diff?from=%40-&to=%40").await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["from"], "@-");
    assert_eq!(d["to"], "@");
    assert_eq!(d["files"][0]["status"], "modified");
    assert!(d["files"][0]["hunks"][0]["lines"].is_array());
    assert_eq!(
        t.get("/api/v1/diff?from=%40").await.0,
        StatusCode::BAD_REQUEST
    );
}

/// As cv serve: GET /diff honours the fileset, range options outside the contract are a 400,
/// unknown view ids answer "view-unknown", and permissions are per UI session.
#[tokio::test]
async fn diff_fileset_range_options_unknown_views_and_session_permissions() {
    let (t, _) = T::new();
    let (_, d) = t.get("/api/v1/diff?from=%40-&to=%40").await;
    let path = d["files"][0]["path"].as_str().unwrap().to_string();
    let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let (s, d) = t
        .get(&format!("/api/v1/diff?from=%40-&to=%40&fileset={dir}/"))
        .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert!(
        d["files"][0]["path"].as_str().unwrap().starts_with(dir),
        "{d}"
    );
    let (_, d) = t
        .get("/api/v1/diff?from=%40-&to=%40&fileset=no%2Fsuch%2Fdir%2F")
        .await;
    assert_eq!(d["files"], serde_json::json!([]), "{d}");

    for bad in [
        serde_json::json!({ "contextHops": 9 }),
        serde_json::json!({ "diff": "sideways" }),
        serde_json::json!({ "contextHop": 1 }),
    ] {
        let (s, e) = t
            .post(
                "/api/v1/views",
                serde_json::json!({ "mode": "range", "revset": "@", "options": bad }),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    }
    let (s, e) = t
        .post(
            "/api/v1/views/v_nope/windows:batch",
            serde_json::json!({ "ids": [], "parts": [], "layoutVersion": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(e["error"]["code"], "view-unknown", "{e}");

    let (_, a) = t
        .put(
            "/api/v1/agent/permissions?session=a",
            serde_json::json!({ "moveCamera": true }),
        )
        .await;
    assert_eq!(a["moveCamera"], true);
    assert_eq!(a["session"], "a");
    let (_, b) = t.get("/api/v1/agent/permissions?session=b").await;
    assert_eq!(b["moveCamera"], false, "session b keeps its own toggle");
    let ev = t.events("agent.permissions").await;
    assert_eq!(ev.last().unwrap()["permissions"]["session"], "a");
}

#[tokio::test]
async fn graph_view_moves_bookmarks_and_fast_forwards_with_undo() {
    let (t, _) = T::new();
    let (s, refs) = t.get("/api/v1/refs").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(refs["trunk"], "trunk");
    let target = |name: &str| {
        refs["bookmarks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["name"] == name)
            .map(|b| b["targets"][0].as_str().unwrap().to_string())
    };
    let trunk0 = target("trunk").unwrap();
    assert!(target("feature/gym").is_some(), "{refs}");

    // Landing the gym stack is a fast-forward only when trunk is its base: the synthetic stacks
    // branch off older trunk commits, so it is refused, nothing written.
    let land = json!({ "op": { "kind": "land", "source": { "bookmark": "feature/gym" }, "trunk": "trunk", "strategy": "merge" } });
    let (_, r) = t.post("/api/v1/graph:op", land).await;
    assert_eq!(r["status"], "rejected", "{r}");

    // A new bookmark, previewed then applied, shows up in /revisions; undo removes it.
    let op = json!({ "kind": "set-bookmark", "name": "review/me", "to": trunk0 });
    let (_, p) = t
        .post("/api/v1/graph:op", json!({ "op": op, "dryRun": true }))
        .await;
    assert_eq!(p["status"], "previewed", "{p}");
    assert_eq!(p["bookmarks"][0]["name"], "review/me");
    let (_, a) = t.post("/api/v1/graph:op", json!({ "op": op })).await;
    assert_eq!(a["status"], "applied", "{a}");
    let has = |rows: &Value| {
        rows["rows"].as_array().unwrap().iter().any(|r| {
            r["rev"]["bookmarks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b == "review/me")
        })
    };
    assert!(has(&t.get("/api/v1/revisions").await.1));
    let (_, u) = t
        .post("/api/v1/graph:undo", json!({ "op": a["opAfter"] }))
        .await;
    assert_eq!(u["status"], "applied", "{u}");
    assert!(!has(&t.get("/api/v1/revisions").await.1));

    // History rewrites are refused in the demo.
    let (_, r) = t
        .post(
            "/api/v1/graph:op",
            json!({ "op": { "kind": "abandon", "commits": [trunk0] } }),
        )
        .await;
    assert_eq!(r["status"], "rejected");
}
