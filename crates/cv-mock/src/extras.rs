//! The rest of `docs/contracts/http.ts` that the UI calls: view refs, single threads and edits, undo,
//! hubs, interdiff, declared and rated links, cluster labels, agent navigation and review marks.
//! Everything is in memory and lost on restart, like the rest of the mock.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{App, St, api_error, applied, bad, not_found, range};

/// Declared links, link ratings and review marks.
#[derive(Default)]
pub struct Extras {
    /// Link JSON of every declared link (POST /links), in creation order.
    pub declared: Vec<Value>,
    /// Link id → (verdict, note).
    pub ratings: std::collections::HashMap<String, (String, Option<String>)>,
    /// Change id → the commit the human marked reviewed.
    pub marks: std::collections::HashMap<String, String>,
}

/// `GET /views/:id`: the ViewRef of the snapshot or of a range view.
pub async fn get_view(State(app): St, Path(view): Path<String>) -> Response {
    if let Some(r) = app.range(&view) {
        return Json(r.ref_json(&app.view().bootstrap.view.id)).into_response();
    }
    if view == app.view().bootstrap.view.id {
        return Json(app.view().bootstrap.view.clone()).into_response();
    }
    crate::view_unknown(&view)
}

/// `GET /threads/:id`.
pub async fn get_thread(State(app): St, Path(id): Path<String>) -> Response {
    match app.review.lock().unwrap().get(&id) {
        Some(t) => Json(t).into_response(),
        None => not_found("unknown thread"),
    }
}

/// `GET /edits/:id`.
pub async fn get_edit(State(app): St, Path(id): Path<String>) -> Response {
    match app.review.lock().unwrap().get_edit(&id) {
        Some(e) => Json(e).into_response(),
        None => not_found("unknown edit"),
    }
}

/// `POST /edits/:id/undo`: restores the files; edit.updated (status "undone"), op.changed and
/// view.invalidated follow as for an apply. 409 "invalid" unless the edit is applied.
pub async fn undo_edit(State(app): St, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let mut rev = app.review.lock().unwrap();
    let actor = rev.as_actor(&headers).actor.clone();
    let res = rev.undo(&id);
    drop(rev);
    match res {
        Ok(r) => {
            applied(&app, &r, actor);
            Json(r).into_response()
        }
        Err(None) => not_found("unknown edit"),
        Err(Some(status)) => api_error(
            StatusCode::CONFLICT,
            "invalid",
            format!("edit {id} is {status}; only an applied edit can be undone"),
        ),
    }
}

#[derive(Deserialize)]
pub struct HubQ {
    #[serde(default)]
    protocol: Option<String>,
}

/// `GET /hubs/:hub`: a hub of the snapshot view by id, else by key (optionally of one protocol).
pub async fn get_hub(State(app): St, Path(hub): Path<String>, Query(q): Query<HubQ>) -> Response {
    let v = app.view();
    let hubs = &v.bootstrap.hubs;
    let by_key = || {
        hubs.iter().find(|h| {
            h.key == hub
                && q.protocol
                    .as_deref()
                    .is_none_or(|p| p == h.protocol.as_str())
        })
    };
    match hubs.iter().find(|h| h.id == hub).or_else(by_key) {
        Some(h) => Json(h).into_response(),
        None => not_found("unknown hub"),
    }
}

#[derive(Deserialize)]
pub struct InterdiffQ {
    change: String,
    from: String,
}

/// `GET /interdiff?change&from`: a synthetic interdiff between the commit the human reviewed
/// (`from`) and the change's current commit: one file, one hunk, every line with its text.
pub async fn interdiff(State(app): St, Query(q): Query<InterdiffQ>) -> Response {
    let Some(row) = revision(&app, &q.change) else {
        return not_found("unknown change");
    };
    if q.from.is_empty() {
        return bad("from required");
    }
    let v = app.view();
    let h = (q.change.bytes().map(u64::from).sum::<u64>() as usize) % v.files.len().max(1);
    let path = v.files.get(h).map(|f| f.path.clone()).unwrap_or_default();
    let desc = row["rev"]["description"].as_str().unwrap_or("").to_string();
    let files = json!([{
        "path": path,
        "status": "modified",
        "hunks": [{
            "old": [10, 4],
            "new": [10, 5],
            "lines": [
                { "kind": "ctx", "oldLine": 10, "newLine": 10, "text": "    let n = items.len();" },
                { "kind": "del", "oldLine": 11, "text": "    for i in 0..n {" },
                { "kind": "add", "newLine": 11, "text": "    for i in 0..n.min(LIMIT) {" },
                { "kind": "add", "newLine": 12, "text": "        // capped after review" },
                { "kind": "ctx", "oldLine": 12, "newLine": 13, "text": "        process(&items[i]);" },
                { "kind": "ctx", "oldLine": 13, "newLine": 14, "text": "    }" }
            ]
        }]
    }]);
    Json(json!({ "files": files, "description": [desc.clone(), desc] })).into_response()
}

#[derive(Deserialize)]
pub struct DiffQ {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    fileset: Option<String>,
}

/// `GET /diff?from&to`: a two-revision diff with one synthetic modified file (as interdiff);
/// from/to come back as given (the mock has no commit graph to resolve them in).
pub async fn diff(State(app): St, Query(q): Query<DiffQ>) -> Response {
    if q.from.is_empty() || q.to.is_empty() {
        return bad("from and to required");
    }
    let v = app.view();
    let key = format!("{}..{}", q.from, q.to);
    // As cv serve: only files the fileset matches (none matched → no files).
    let paths: Vec<&str> = v
        .files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| range::fileset_matches(q.fileset.as_deref(), p))
        .collect();
    if paths.is_empty() {
        return Json(json!({ "files": [], "from": q.from, "to": q.to })).into_response();
    }
    let h = (key.bytes().map(u64::from).sum::<u64>() as usize) % paths.len();
    let path = paths[h].to_string();
    let files = json!([{
        "path": path,
        "status": "modified",
        "hunks": [{
            "old": [3, 2],
            "new": [3, 2],
            "lines": [
                { "kind": "ctx", "oldLine": 3, "newLine": 3, "text": "    let n = items.len();" },
                { "kind": "del", "oldLine": 4, "text": "    let limit = 10;" },
                { "kind": "add", "newLine": 4, "text": "    let limit = 20;" }
            ]
        }]
    }]);
    Json(json!({ "files": files, "from": q.from, "to": q.to })).into_response()
}

/// The history row of a change (exact id or unique prefix).
fn revision(app: &App, change: &str) -> Option<Value> {
    if change.is_empty() {
        return None;
    }
    let rows: Vec<&Value> = app
        .panels
        .revisions
        .iter()
        .filter(|r| {
            r["rev"]["change"]
                .as_str()
                .is_some_and(|c| c.starts_with(change))
        })
        .collect();
    (rows.len() == 1).then(|| rows[0].clone())
}

/// A link end from a string: hub id, window id, or `path:line`.
fn end(app: &App, s: &str) -> Option<Value> {
    let v = app.view();
    if v.bootstrap.hubs.iter().any(|h| h.id == s) {
        return Some(json!({ "hub": s }));
    }
    if let Some(&w) = v.by_id.get(s) {
        let h = &v.windows[w].header;
        return Some(json!({ "path": h.path, "line": h.lines.start, "window": h.id }));
    }
    let (path, line) = s.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    let window = v
        .windows
        .iter()
        .find(|w| {
            w.header.path == path && w.header.lines.start <= line && line <= w.header.lines.end
        })
        .map(|w| w.header.id.clone());
    if window.is_none() && !v.files.iter().any(|f| f.path == path) {
        return None;
    }
    let mut site = json!({ "path": path, "line": line });
    if let Some(w) = window {
        site["window"] = json!(w);
    }
    Some(site)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclareBody {
    from: String,
    to: String,
    kind: String,
    label: String,
    #[serde(default)]
    note: Option<String>,
}

/// `POST /links`: declare a link (provenance "human", or "agent" with X-CV-Agent).
pub async fn declare_link(
    State(app): St,
    headers: HeaderMap,
    Json(b): Json<DeclareBody>,
) -> Response {
    let Some(from) = end(&app, &b.from).filter(|f| f.get("hub").is_none()) else {
        return bad(format!("unknown from {}", b.from));
    };
    let Some(to) = end(&app, &b.to) else {
        return bad(format!("unknown to {}", b.to));
    };
    if b.kind.is_empty() {
        return bad("kind required");
    }
    let agent = headers.contains_key("x-cv-agent");
    let mut x = app.extras.lock().unwrap();
    let mut link = json!({
        "id": format!("decl{}", x.declared.len() + 1),
        "kind": b.kind, "from": from, "to": to, "label": b.label,
        "confidence": 1.0,
        "provenance": if agent { "agent" } else { "human" },
    });
    if let Some(n) = b.note {
        link["note"] = json!(n);
    }
    x.declared.push(link.clone());
    Json(link).into_response()
}

#[derive(Deserialize)]
pub struct RateBody {
    verdict: String,
    #[serde(default)]
    note: Option<String>,
}

/// `POST /links/:id/rate`: confirm or reject a declared or indexed link; GET /links carries it.
pub async fn rate_link(
    State(app): St,
    Path(id): Path<String>,
    Json(b): Json<RateBody>,
) -> Response {
    if b.verdict != "confirmed" && b.verdict != "rejected" {
        return bad("verdict must be confirmed or rejected");
    }
    let Some(mut link) = find_link(&app, &id) else {
        return not_found("unknown link");
    };
    let mut x = app.extras.lock().unwrap();
    x.ratings.insert(id, (b.verdict.clone(), b.note.clone()));
    rate(&mut link, &x);
    Json(link).into_response()
}

fn find_link(app: &App, id: &str) -> Option<Value> {
    if let Some(l) = app
        .extras
        .lock()
        .unwrap()
        .declared
        .iter()
        .find(|l| l["id"] == id)
    {
        return Some(l.clone());
    }
    let v = app.view();
    let l = v.links.iter().find(|l| l.id == id)?;
    let (f, t) = (&v.windows[l.from].header, &v.windows[l.to].header);
    Some(json!({
        "id": l.id, "kind": l.kind,
        "from": { "path": f.path, "line": l.from_line, "window": f.id },
        "to": { "path": t.path, "line": l.to_line, "window": t.id },
        "label": t.symbol.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
        "confidence": l.confidence, "provenance": "tree-sitter",
    }))
}

/// Sets `feedback` (and the rating's note) on a Link JSON when it was rated.
pub fn rate(link: &mut Value, x: &Extras) {
    if let Some((verdict, note)) = link["id"].as_str().and_then(|id| x.ratings.get(id)) {
        link["feedback"] = json!(verdict);
        if let Some(n) = note {
            link["note"] = json!(n);
        }
    }
}

/// GET /links post-processing: ratings on every link, plus declared links touching `target`.
pub fn decorate_links(app: &App, target: &str, dir: &str, out: &mut Value) {
    let x = app.extras.lock().unwrap();
    if let Some(links) = out["links"].as_array_mut() {
        for l in links.iter_mut() {
            rate(l, &x);
        }
        for d in &x.declared {
            let is_from = d["from"]["window"] == target;
            let is_to = d["to"]["window"] == target;
            if (is_from && dir != "in") || (is_to && dir != "out") {
                let mut d = d.clone();
                rate(&mut d, &x);
                links.push(d);
            }
        }
    }
}

#[derive(Deserialize)]
pub struct LabelBody {
    label: String,
    #[serde(default)]
    summary: Option<String>,
}

/// `PUT /clusters/:id/label`: relabels a cluster of the snapshot view; view.invalidated
/// { reason: "layout", windows: [] } tells clients to reload the bootstrap.
pub async fn label_cluster(
    State(app): St,
    headers: HeaderMap,
    Path(cluster): Path<String>,
    Json(b): Json<LabelBody>,
) -> Response {
    let label = b.label.trim();
    if label.is_empty() {
        return bad("label is empty");
    }
    let source = if headers.contains_key("x-cv-agent") {
        "agent"
    } else {
        "human"
    };
    let (view_id, version) = {
        let mut slot = app.view.write().unwrap();
        if !slot.bootstrap.clusters.iter().any(|c| c.id == cluster) {
            return not_found("unknown cluster");
        }
        let mut v = (**slot).clone();
        let c = v
            .bootstrap
            .clusters
            .iter_mut()
            .find(|c| c.id == cluster)
            .unwrap();
        c.label = label.to_string();
        c.label_source = source.to_string();
        if b.summary.is_some() {
            c.summary = b.summary.clone();
        }
        let out = (v.bootstrap.view.id.clone(), v.bootstrap.view.layout_version);
        *slot = Arc::new(v);
        out
    };
    app.events.emit(json!({
        "type": "view.invalidated", "viewId": view_id, "layoutVersion": version, "reason": "layout", "windows": []
    }));
    Json(json!({ "ok": true })).into_response()
}

/// `POST /agent/view`: 403 "not-allowed" unless the human allowed camera moves; else the view for
/// the spec (built like POST /views) and view.requested.
pub async fn agent_view(State(app): St, headers: HeaderMap, Json(req): Json<Value>) -> Response {
    let allowed = app.agent_may("moveCamera");
    if !allowed {
        return api_error(
            StatusCode::FORBIDDEN,
            "not-allowed",
            "The human has not allowed the agent to move the camera; use focus to suggest a place instead.".into(),
        );
    }
    let spec = req["spec"].clone();
    let snapshot = app.view().bootstrap.view.id.clone();
    let view_ref = if spec["mode"] == "range" {
        let revset = spec["revset"].as_str().unwrap_or("trunk()..@");
        let r = Arc::new(range::RangeView::new(&app.view(), revset, &spec["options"]));
        let out = r.ref_json(&snapshot);
        app.ranges.lock().unwrap().insert(r.id.clone(), r);
        out
    } else {
        serde_json::to_value(&app.view().bootstrap.view).unwrap()
    };
    let by = app.review.lock().unwrap().as_actor(&headers).actor.clone();
    app.events.emit(json!({
        "type": "view.requested", "viewId": view_ref["id"], "spec": spec, "by": by
    }));
    Json(view_ref).into_response()
}

#[derive(Deserialize)]
pub struct MarkBody {
    commit: String,
}

/// `PUT /reviews/marks/:change`: the human marks a change reviewed at `commit`.
pub async fn mark(State(app): St, Path(change): Path<String>, Json(b): Json<MarkBody>) -> Response {
    let Some(row) = revision(&app, &change) else {
        return not_found("unknown change");
    };
    let change = row["rev"]["change"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let head = row["rev"]["commit"].as_str().unwrap_or_default();
    app.extras
        .lock()
        .unwrap()
        .marks
        .insert(change.clone(), b.commit.clone());
    let threads = app.review.lock().unwrap().threads.clone();
    let mine = threads
        .iter()
        .filter(|t| t["anchor"]["change"] == change.as_str());
    let open = mine
        .clone()
        .filter(|t| {
            ["open", "in-progress", "addressed"]
                .iter()
                .any(|s| t["status"] == *s)
        })
        .count();
    let touched = mine.clone().any(|t| {
        t["comments"]
            .as_array()
            .is_some_and(|cs| cs.iter().any(|c| c["author"]["kind"] == "agent"))
    }) || row["review"]["touchedByAgent"] == true;
    app.events.emit(
        json!({ "type": "review.marked", "change": change, "commit": b.commit, "reviewer": "you" }),
    );
    Json(json!({
        "change": change,
        "openThreads": open,
        "reviewedAt": b.commit,
        "changedSinceReview": b.commit != head,
        "touchedByAgent": touched,
    }))
    .into_response()
}
