//! Graph view routes on the synthetic history (http.ts `GET /refs`, `POST /graph:op`,
//! `POST /graph:undo`). Bookmarks are an overlay on the static rows of [`crate::panels::Panels`]:
//! creating, moving and deleting them, and landing by fast-forward, work (with undo); operations
//! that would rewrite commits (rebase, merge, describe, abandon, new, commit, squash) answer
//! "rejected" with a note that the demo repository does not rewrite history. It keeps no operation
//! log (`GET /ops` is empty) and has no git remote (fetch and push fail).

use std::collections::{BTreeMap, HashMap};

use axum::{Json, extract::State, http::HeaderMap, response::IntoResponse, response::Response};
use serde_json::{Value, json};

use crate::{St, agent_may_not_edit, bad};

/// Local bookmarks (name → commit) and, per applied op id, the bookmarks before it (for undo).
pub struct Graph {
    bookmarks: BTreeMap<String, String>,
    undo: HashMap<String, BTreeMap<String, String>>,
    seq: u64,
}

impl Graph {
    pub fn new(revisions: &[Value]) -> Self {
        let mut bookmarks = BTreeMap::new();
        for r in revisions {
            for b in r["rev"]["bookmarks"].as_array().into_iter().flatten() {
                if let (Some(b), Some(c)) = (b.as_str(), r["rev"]["commit"].as_str()) {
                    bookmarks.insert(b.to_string(), c.to_string());
                }
            }
        }
        // A feature bookmark on each agent stack's tip, so there is something to land.
        for r in revisions {
            for w in r["rev"]["workspaces"].as_array().into_iter().flatten() {
                if let (Some(w), Some(c)) = (w.as_str(), r["rev"]["commit"].as_str())
                    && w != "default"
                {
                    bookmarks.insert(format!("feature/{w}"), c.to_string());
                }
            }
        }
        Graph {
            bookmarks,
            undo: HashMap::new(),
            seq: 0,
        }
    }

    /// The rows with the overlay's bookmarks.
    pub fn rows(&self, revisions: &[Value]) -> Vec<Value> {
        let mut by_commit: HashMap<&str, Vec<&str>> = HashMap::new();
        for (n, c) in &self.bookmarks {
            by_commit.entry(c.as_str()).or_default().push(n.as_str());
        }
        revisions
            .iter()
            .map(|r| {
                let mut r = r.clone();
                let c = r["rev"]["commit"].as_str().unwrap_or_default().to_string();
                r["rev"]["bookmarks"] =
                    json!(by_commit.get(c.as_str()).cloned().unwrap_or_default());
                r
            })
            .collect()
    }
}

fn parents_of(revisions: &[Value]) -> HashMap<String, Vec<String>> {
    revisions
        .iter()
        .map(|r| {
            let ps = r["rev"]["parents"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str().map(String::from))
                .collect();
            (
                r["rev"]["commit"].as_str().unwrap_or_default().to_string(),
                ps,
            )
        })
        .collect()
}

/// Whether `a` is `b` or one of its ancestors in the synthetic history.
fn is_ancestor(parents: &HashMap<String, Vec<String>>, a: &str, b: &str) -> bool {
    let mut stack = vec![b.to_string()];
    let mut seen = std::collections::HashSet::new();
    while let Some(c) = stack.pop() {
        if c == a {
            return true;
        }
        if seen.insert(c.clone())
            && let Some(ps) = parents.get(&c)
        {
            stack.extend(ps.iter().cloned());
        }
    }
    false
}

fn result(status: &str, summary: String, extra: Value) -> Value {
    let mut r = json!({
        "status": status, "actor": { "kind": "human", "name": "you" }, "summary": summary,
        "rewritten": [], "bookmarks": [], "workspaces": [], "conflicts": [], "divergent": [],
    });
    if let (Some(o), Some(e)) = (r.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    r
}

fn rejected(message: impl Into<String>) -> Value {
    let message = message.into();
    result(
        "rejected",
        message.clone(),
        json!({ "error": { "code": "invalid", "message": message } }),
    )
}

pub async fn refs(State(app): St) -> Response {
    let g = app.graph.lock().unwrap();
    let bookmarks: Vec<Value> = g
        .bookmarks
        .iter()
        .map(|(n, c)| json!({ "name": n, "targets": [c], "remotes": [] }))
        .collect();
    Json(json!({ "bookmarks": bookmarks, "trunk": "trunk", "colocated": false, "remotes": [] }))
        .into_response()
}

/// GET /ops: the demo repository keeps no operation log.
pub async fn ops() -> Response {
    Json(json!({ "ops": [], "truncated": false, "current": "" })).into_response()
}

/// POST /git:fetch, /git:push: the demo repository has no remotes.
pub async fn no_remote() -> Response {
    Json(json!({
        "status": "failed", "summary": "the demo repository has no git remote", "output": "", "moved": [],
        "error": { "code": "invalid", "message": "the demo repository has no git remote" }
    }))
    .into_response()
}

pub async fn op(State(app): St, headers: HeaderMap, Json(req): Json<Value>) -> Response {
    if let Some(r) = agent_may_not_edit(&app, &headers) {
        return r;
    }
    let op = &req["op"];
    let dry = req["dryRun"].as_bool().unwrap_or(false);
    let rows = &app.panels.revisions;
    let parents = parents_of(rows);
    let mut g = app.graph.lock().unwrap();
    let mut next = g.bookmarks.clone();
    let summary = match op["kind"].as_str().unwrap_or_default() {
        "set-bookmark" => {
            let (Some(name), Some(to)) = (op["name"].as_str(), op["to"].as_str()) else {
                return bad("set-bookmark needs name and to");
            };
            if !parents.contains_key(to) {
                return Json(rejected(format!("unknown commit {to}"))).into_response();
            }
            if let Some(old) = g.bookmarks.get(name)
                && !op["allowBackwards"].as_bool().unwrap_or(false)
                && !is_ancestor(&parents, old, to)
            {
                return Json(rejected(format!(
                    "moving {name} from {} to {} goes backwards or sideways; allow it to move it anyway",
                    &old[..12],
                    &to[..12]
                )))
                .into_response();
            }
            next.insert(name.to_string(), to.to_string());
            format!("bookmark {name} → {}", &to[..12])
        }
        "delete-bookmark" => {
            let name = op["name"].as_str().unwrap_or_default();
            if next.remove(name).is_none() {
                return Json(rejected(format!("no bookmark named {name}"))).into_response();
            }
            format!("delete bookmark {name}")
        }
        "land" => {
            let trunk = op["trunk"].as_str().unwrap_or_default();
            let Some(t) = g.bookmarks.get(trunk).cloned() else {
                return Json(rejected(format!("no bookmark named {trunk}"))).into_response();
            };
            let src = &op["source"];
            let head = if let Some(b) = src["bookmark"].as_str() {
                g.bookmarks.get(b).cloned()
            } else if let Some(c) = src["commit"].as_str() {
                Some(c.to_string())
            } else if let Some(w) = src["workspace"].as_str() {
                // The workspace's @, or its parent when @ is an undescribed scratch commit.
                rows.iter()
                    .find(|r| {
                        r["rev"]["workspaces"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|x| x == w))
                    })
                    .and_then(|r| {
                        if r["rev"]["description"] == "" {
                            r["rev"]["parents"][0].as_str().map(String::from)
                        } else {
                            r["rev"]["commit"].as_str().map(String::from)
                        }
                    })
            } else {
                None
            };
            let Some(head) = head else {
                return Json(rejected("land source not found")).into_response();
            };
            if is_ancestor(&parents, &head, &t) {
                return Json(rejected(format!("{} is already in {trunk}", &head[..12])))
                    .into_response();
            }
            if !is_ancestor(&parents, &t, &head) {
                return Json(rejected(format!(
                    "{trunk} is not an ancestor of {}: it cannot fast-forward, and the demo repository does not rewrite history (rebase or merge it in a real repository)",
                    &head[..12]
                )))
                .into_response();
            }
            next.insert(trunk.to_string(), head.clone());
            if let Some(b) = op["deleteBookmark"].as_str() {
                next.remove(b);
            }
            format!("land {} on {trunk}", &head[..12])
        }
        k => {
            return Json(rejected(format!(
                "{k}: the demo repository does not rewrite history; try it on a real jj repository"
            )))
            .into_response();
        }
    };
    let moves: Vec<Value> = next
        .keys()
        .chain(g.bookmarks.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|n| next.get(*n) != g.bookmarks.get(*n))
        .map(|n| json!({ "name": n, "from": g.bookmarks.get(n), "to": next.get(n) }))
        .collect();
    if dry {
        return Json(result("previewed", summary, json!({ "bookmarks": moves }))).into_response();
    }
    g.seq += 1;
    let op_after = format!("{:0>12x}{:0>116}", g.seq, 0);
    let before = std::mem::replace(&mut g.bookmarks, next);
    g.undo.insert(op_after.clone(), before);
    drop(g);
    app.events.emit(json!({ "type": "op.changed", "op": op_after, "changes": [], "actor": { "kind": "human", "name": "you" } }));
    Json(result(
        "applied",
        summary,
        json!({ "bookmarks": moves, "opAfter": op_after }),
    ))
    .into_response()
}

pub async fn undo(State(app): St, headers: HeaderMap, Json(req): Json<Value>) -> Response {
    if let Some(r) = agent_may_not_edit(&app, &headers) {
        return r;
    }
    let op = req["op"].as_str().unwrap_or_default();
    let mut g = app.graph.lock().unwrap();
    let Some(before) = g.undo.remove(op) else {
        return Json(rejected(format!("unknown operation {op}"))).into_response();
    };
    g.bookmarks = before;
    g.seq += 1;
    let op_after = format!("{:0>12x}{:0>116}", g.seq, 0);
    drop(g);
    app.events.emit(json!({ "type": "op.changed", "op": op_after, "changes": [], "actor": { "kind": "human", "name": "you" } }));
    Json(result(
        "applied",
        format!("undo operation {}", &op[..12.min(op.len())]),
        json!({ "opAfter": op_after }),
    ))
    .into_response()
}
