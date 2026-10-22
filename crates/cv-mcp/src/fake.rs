//! `FakeBackend`: an in-memory repo, review store and event log behind [`McpBackend`], for tests.
//! One window per file, content-addressed blobs (a new blob id per edit, so stale bases are caught),
//! threads with actor attribution, exact edit previews, conflicts and a long-poll event log that emits
//! the same events as the server (thread.upserted, edit.updated, op.changed, view.invalidated,
//! workspace.updated, agent.attention, …).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::backend::{AgentId, BResult, BackendError, McpBackend, ThreadQuery};

pub const VIEW: &str = "v-snapshot";
pub const CHANGE: &str = "kxwqmnpo";

#[derive(Default)]
struct State {
    /// path → current lines.
    files: BTreeMap<String, Vec<String>>,
    /// blob id → lines (immutable).
    blobs: HashMap<String, Vec<String>>,
    /// path → current blob id.
    current: HashMap<String, String>,
    workspaces: Vec<Value>,
    threads: Vec<Value>,
    edits: HashMap<String, (Value, Value)>,
    links: Vec<Value>,
    hubs: Vec<Value>,
    conflicts: Vec<(Value, String)>,
    log: Vec<Value>,
    view_state: Option<Value>,
    next: u64,
    op: u64,
}

pub struct FakeBackend {
    st: Mutex<State>,
    notify: Notify,
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn window_id(path: &str) -> String {
    format!("w:{path}")
}

impl State {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    fn set_file(&mut self, path: &str, lines: Vec<String>) {
        let blob = format!("b{:012x}", fnv(&format!("{path}\0{}", lines.join("\n"))));
        self.blobs.insert(blob.clone(), lines.clone());
        self.current.insert(path.to_string(), blob);
        self.files.insert(path.to_string(), lines);
    }

    fn emit(&mut self, ev: Value) {
        self.log.push(ev);
    }

    fn header(&self, path: &str) -> Value {
        let n = self.files[path].len() as u32;
        let open = self
            .threads
            .iter()
            .filter(|t| t["anchor"]["path"] == path && t["status"] == "open")
            .count();
        let tests = path.contains("test");
        let grade = (fnv(path) % 5) as u32;
        let risk = (grade as f64 + 1.0) / 5.0;
        json!({
            "id": window_id(path), "key": format!("{path}#file"), "kind": "symbol",
            "cluster": "c1", "lane": if tests { "tests" } else { "code" }, "path": path,
            "lang": "python", "rev": "c0ffee", "change": CHANGE, "blob": self.current[path],
            "symbol": { "name": path.rsplit('/').next().unwrap_or(path), "qualname": path.replace('/', ".").trim_end_matches(".py"), "kind": "module" },
            "lines": { "start": 1, "end": n, "fileLines": n },
            "rect": { "x": 0, "y": 0, "w": 100, "h": 100 },
            "heat": { "grade": grade, "rate": risk / 10.0, "ci": [0, 1], "settled": true, "touches": grade * 3 },
            "conn": { "grade": grade, "dependents2": grade * 2, "fanIn": grade, "fanOut": 1, "tests": 0, "reach": grade * 4 },
            "risk": risk, "reviewFirst": grade == 4,
            "counts": { "openThreads": open, "ports": 0, "conflicts": 0 },
            "flags": { "pinned": false, "truncated": false }
        })
    }

    fn thread_mut(&mut self, id: &str) -> BResult<&mut Value> {
        self.threads
            .iter_mut()
            .find(|t| t["id"] == id)
            .ok_or_else(|| BackendError::not_found(format!("unknown thread {id}")))
    }

    fn workspace_updated(&mut self, name: &str) {
        if let Some(w) = self.workspaces.iter().find(|w| w["name"] == name).cloned() {
            self.emit(json!({ "type": "workspace.updated", "workspace": w }));
        }
    }

    /// Apply a stored edit: patch files, new op, events.
    fn apply(&mut self, id: &str, who: &AgentId) -> BResult<Value> {
        let (req, mut result) = self
            .edits
            .get(id)
            .cloned()
            .ok_or_else(|| BackendError::not_found(format!("unknown edit {id}")))?;
        if result["status"] != "previewed" {
            return Err(BackendError::invalid(format!(
                "edit {id} is {}",
                result["status"]
            )));
        }
        for f in req["files"].as_array().into_iter().flatten() {
            let path = f["path"].as_str().unwrap_or("");
            let mut lines = self.files[path].clone();
            if let Some(c) = f["content"].as_str() {
                lines = c.split('\n').map(String::from).collect();
            }
            let mut patches: Vec<&Value> = f["patches"].as_array().into_iter().flatten().collect();
            patches.sort_by_key(|p| std::cmp::Reverse(p["start"].as_u64().unwrap_or(0)));
            for p in patches {
                let start = p["start"].as_u64().unwrap_or(1).max(1) as usize;
                let end = p["end"].as_u64().unwrap_or(0) as usize;
                let from = (start - 1).min(lines.len());
                let to = end.min(lines.len()).max(from);
                let new: Vec<String> = p["text"]
                    .as_str()
                    .unwrap_or("")
                    .split('\n')
                    .map(String::from)
                    .collect();
                lines.splice(from..to, new);
            }
            self.set_file(path, lines);
        }
        self.op += 1;
        result["status"] = json!("applied");
        result["predicted"] = json!(false);
        result["opBefore"] = json!(format!("op{}", self.op - 1));
        result["opAfter"] = json!(format!("op{}", self.op));
        self.edits
            .insert(id.to_string(), (req.clone(), result.clone()));
        self.emit(json!({ "type": "edit.updated", "edit": result }));
        self.emit(json!({ "type": "op.changed", "op": result["opAfter"], "changes": result["rewritten"], "actor": who.actor() }));
        let windows: Vec<String> = req["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["path"].as_str().map(window_id))
            .collect();
        self.emit(json!({ "type": "view.invalidated", "viewId": VIEW, "layoutVersion": self.op, "reason": "op", "windows": windows }));
        if let Some(t) = req["origin"]["thread"].as_str() {
            let id = id.to_string();
            if let Ok(th) = self.thread_mut(t) {
                th["producedEdits"].as_array_mut().unwrap().push(json!(id));
                let th = th.clone();
                self.emit(json!({ "type": "thread.upserted", "thread": th }));
            }
        }
        // The amended change is the @ of every workspace on it: their cards change.
        let names: Vec<String> = self
            .workspaces
            .iter()
            .filter(|w| {
                result["rewritten"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|c| *c == w["wc"]["change"]))
            })
            .filter_map(|w| w["name"].as_str().map(String::from))
            .collect();
        for n in names {
            if let Some(w) = self.workspaces.iter_mut().find(|w| w["name"] == n.as_str()) {
                w["lastOp"] = json!({ "op": result["opAfter"], "time": "now", "description": format!("cv: {}", req["mode"].as_str().unwrap_or("amend")) });
            }
            self.workspace_updated(&n);
        }
        Ok(result)
    }
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBackend {
    /// Empty repo with the workspaces "default" (served) and "gym", both at change [`CHANGE`].
    pub fn new() -> Self {
        let mut st = State::default();
        for (name, serving) in [("default", true), ("gym", false)] {
            st.workspaces.push(json!({
                "name": name, "root": format!("/repo/{name}"), "wc": { "change": CHANGE, "commit": "c0ffee" },
                "serving": serving,
                "stack": { "revset": format!("trunk()..{name}@"), "changes": 1, "files": 1, "conflicts": 0 },
                "unsnapshottedFiles": 0,
                "lastOp": { "op": "op0", "time": "0", "description": "init" },
                "overlaps": []
            }));
        }
        FakeBackend {
            st: Mutex::new(st),
            notify: Notify::new(),
        }
    }

    /// A small repo: a bus, a world module calling it, and a hub between them.
    pub fn sample() -> Self {
        let f = Self::new();
        f.add_file(
            "src/bus.py",
            "class Bus:\n    def __init__(self):\n        self.subs = {}\n\n    def emit(self, topic, payload):\n        retries = 0\n        while True:\n            retries += 1\n            for fn in self.subs.get(topic, []):\n                fn(payload)\n            return retries",
        );
        f.add_file(
            "src/world.py",
            "from bus import Bus\n\nbus = Bus()\n\n\ndef move(entity):\n    entity.x += 1\n    bus.emit(\"world.entity.moved\", entity)",
        );
        f.add_file(
            "tests/test_world.py",
            "from world import move\n\n\ndef test_move(e):\n    move(e)\n    assert e.x == 1",
        );
        {
            let mut st = f.st.lock().unwrap();
            st.links.push(json!({
                "id": "l1", "kind": "call",
                "from": { "path": "src/world.py", "line": 8, "window": window_id("src/world.py") },
                "to": { "path": "src/bus.py", "line": 5, "window": window_id("src/bus.py") },
                "label": "→ Bus.emit", "confidence": 1.0, "provenance": "tree-sitter"
            }));
            st.hubs.push(json!({
                "id": "bus:world.entity.moved", "protocol": "bus", "key": "world.entity.moved",
                "display": "world.entity.moved",
                "producers": [{ "path": "src/world.py", "line": 8 }], "consumers": []
            }));
        }
        f
    }

    pub fn add_file(&self, path: &str, text: &str) {
        let mut st = self.st.lock().unwrap();
        st.set_file(path, text.split('\n').map(String::from).collect());
    }

    pub fn file_text(&self, path: &str) -> Option<String> {
        self.st
            .lock()
            .unwrap()
            .files
            .get(path)
            .map(|l| l.join("\n"))
    }

    /// A thread as the human creates it in the UI (emits thread.upserted).
    pub fn human_thread(
        &self,
        path: &str,
        start: u32,
        end: u32,
        body: &str,
        kind: &str,
        assignee: Option<&str>,
    ) -> Value {
        let human = AgentId::default();
        let mut st = self.st.lock().unwrap();
        let t = make_thread(
            &mut st, path, start, end, body, kind, assignee, &human, false,
        );
        st.threads.push(t.clone());
        st.emit(json!({ "type": "thread.upserted", "thread": t }));
        drop(st);
        self.notify.notify_waiters();
        t
    }

    /// Mark `path` conflicted at lines [start, end] between two sides.
    pub fn add_conflict(&self, path: &str, start: u32, end: u32, ours: &str, theirs: &str) {
        let mut st = self.st.lock().unwrap();
        let rec = json!({ "change": CHANGE, "commit": "c0ffee", "path": path, "sides": 2, "firstSeenOp": "op0",
            "regions": [{ "start": start, "end": end, "sides": [{ "label": "ours", "text": ours }, { "label": "theirs", "text": theirs }] }] });
        st.conflicts.push((rec.clone(), path.to_string()));
        st.emit(json!({ "type": "conflicts.changed", "added": [rec], "resolved": [] }));
        drop(st);
        self.notify.notify_waiters();
    }

    pub fn set_view_state(&self, state: Value) {
        self.st.lock().unwrap().view_state = Some(state);
    }

    /// Every event logged so far.
    pub fn events_log(&self) -> Vec<Value> {
        self.st.lock().unwrap().log.clone()
    }

    pub fn thread(&self, id: &str) -> Option<Value> {
        self.st
            .lock()
            .unwrap()
            .threads
            .iter()
            .find(|t| t["id"] == id)
            .cloned()
    }

    fn with<T>(&self, f: impl FnOnce(&mut State) -> BResult<T>) -> BResult<T> {
        let r = f(&mut self.st.lock().unwrap());
        self.notify.notify_waiters();
        r
    }
}

#[allow(clippy::too_many_arguments)]
fn make_thread(
    st: &mut State,
    path: &str,
    start: u32,
    end: u32,
    body: &str,
    kind: &str,
    assignee: Option<&str>,
    who: &AgentId,
    by_agent: bool,
) -> Value {
    let id = st.id("t");
    let cid = st.id("cm");
    let author = if by_agent {
        who.actor()
    } else {
        json!({ "kind": "human", "name": "you" })
    };
    json!({
        "id": id,
        "anchor": { "change": CHANGE, "commit": "c0ffee", "path": path, "side": "new", "start": start, "end": end,
                    "fingerprint": { "text": "", "before": "", "after": "" } },
        "status": "open", "kind": kind, "assignee": assignee,
        "createdBy": author, "createdAt": "0", "updatedAt": "0",
        "comments": [{ "id": cid, "author": author, "body": body, "createdAt": "0" }],
        "producedEdits": []
    })
}

#[async_trait]
impl McpBackend for FakeBackend {
    async fn workspaces(&self) -> BResult<Vec<Value>> {
        Ok(self.st.lock().unwrap().workspaces.clone())
    }

    async fn bind_workspace(&self, who: &AgentId, workspace: &str) -> BResult<Value> {
        self.with(|st| {
            let w = st
                .workspaces
                .iter_mut()
                .find(|w| w["name"] == workspace)
                .ok_or_else(|| BackendError::not_found(format!("unknown workspace {workspace}")))?;
            w["agent"] = json!({ "name": who.name, "client": who.client, "activity": "idle", "lastSeen": "now" });
            let w = w.clone();
            st.emit(json!({ "type": "workspace.updated", "workspace": w }));
            Ok(w)
        })
    }

    async fn attention(&self, event: Value) -> BResult<()> {
        self.with(|st| {
            st.emit(event);
            Ok(())
        })
    }

    async fn view_state(&self) -> BResult<Value> {
        let st = self.st.lock().unwrap();
        Ok(match &st.view_state {
            Some(s) => json!({ "view": { "id": VIEW }, "state": s }),
            None => json!({ "view": null }),
        })
    }

    async fn set_view(&self, _who: &AgentId, _spec: Value) -> BResult<Value> {
        Err(BackendError::Unsupported("POST /api/v1/agent/view".into()))
    }

    async fn focus(&self, who: &AgentId, body: Value) -> BResult<Value> {
        self.with(|st| {
            st.emit(json!({ "type": "focus.request", "by": who.actor(), "target": body["target"], "lines": body["lines"], "note": body["note"] }));
            Ok(json!({ "delivered": "suggested" }))
        })
    }

    async fn create_tour(&self, _who: &AgentId, _body: Value) -> BResult<Value> {
        self.with(|st| Ok(json!({ "tour": st.id("tour") })))
    }

    async fn create_view(&self, spec: Value) -> BResult<Value> {
        let id = if spec["mode"] == "range" {
            "v-range"
        } else {
            VIEW
        };
        let op = self.st.lock().unwrap().op;
        Ok(
            json!({ "id": id, "spec": spec, "resolved": { "op": format!("op{op}"), "head": "c0ffee", "commits": ["c0ffee"] },
                   "status": "ready", "layoutVersion": op }),
        )
    }

    async fn view_windows(&self, view: &str) -> BResult<Vec<Value>> {
        let st = self.st.lock().unwrap();
        if view != VIEW {
            return Ok(Vec::new());
        }
        Ok(st.files.keys().map(|p| st.header(p)).collect())
    }

    async fn view_hubs(&self, _view: &str) -> BResult<Vec<Value>> {
        Ok(self.st.lock().unwrap().hubs.clone())
    }

    async fn window_batch(&self, _view: &str, ids: &[String], _parts: &[&str]) -> BResult<Value> {
        let st = self.st.lock().unwrap();
        let mut outlines = Vec::new();
        let mut links = Vec::new();
        let mut missing = Vec::new();
        for id in ids {
            let Some(path) = id.strip_prefix("w:").filter(|p| st.files.contains_key(*p)) else {
                missing.push(id.clone());
                continue;
            };
            let mut ports = Vec::new();
            for l in &st.links {
                for (side, end) in [("out", &l["from"]), ("in", &l["to"])] {
                    if end["path"] == path {
                        ports.push(json!({ "id": format!("{side}{}", l["id"].as_str().unwrap_or("")), "side": side,
                            "line": end["line"], "label": l["label"], "kind": l["kind"], "links": [l["id"]] }));
                        links.push(l.clone());
                    }
                }
            }
            outlines.push(json!({ "id": id, "silhouette": "", "headings": [], "ports": ports }));
        }
        Ok(json!({ "outlines": outlines, "diffs": [], "links": links, "missing": missing }))
    }

    async fn blob_text(&self, blob: &str, from: Option<u32>, to: Option<u32>) -> BResult<Value> {
        let st = self.st.lock().unwrap();
        let lines = st
            .blobs
            .get(blob)
            .ok_or_else(|| BackendError::not_found(format!("unknown blob {blob}")))?;
        let n = lines.len() as u32;
        let from = from.unwrap_or(1).clamp(1, n.max(1));
        let to = to.unwrap_or(n).min(n).max(from.min(n));
        let slice = &lines[(from - 1) as usize..to as usize];
        Ok(
            json!({ "blob": blob, "lang": "python", "from": from, "to": to, "lines": slice, "tokens": vec![Vec::<u32>::new(); slice.len()] }),
        )
    }

    async fn metrics(
        &self,
        _view: &str,
        _level: &str,
        _lane: Option<&str>,
        limit: usize,
    ) -> BResult<Value> {
        let st = self.st.lock().unwrap();
        let points: Vec<Value> = st
            .files
            .keys()
            .take(limit)
            .map(|p| {
                let h = st.header(p);
                json!({ "key": h["key"], "label": p, "lane": h["lane"], "heat": h["heat"], "conn": h["conn"],
                        "heatPct": h["risk"], "connPct": h["risk"], "risk": h["risk"], "window": h["id"] })
            })
            .collect();
        Ok(
            json!({ "points": points, "cochange": [{ "a": "src/world.py", "b": "tests/test_world.py", "support": 3, "jaccard": 0.5 }] }),
        )
    }

    async fn search(
        &self,
        q: &str,
        _kinds: Option<&str>,
        _view: Option<&str>,
        limit: usize,
    ) -> BResult<Vec<Value>> {
        let st = self.st.lock().unwrap();
        let q = q.to_lowercase();
        let mut hits: Vec<Value> = st
            .files
            .iter()
            .filter(|(p, lines)| {
                p.to_lowercase().contains(&q) || lines.iter().any(|l| l.to_lowercase().contains(&q))
            })
            .map(|(p, _)| json!({ "kind": "file", "label": p, "detail": format!("{p}:1"), "target": { "window": window_id(p) }, "score": 0.5 }))
            .collect();
        hits.truncate(limit);
        Ok(hits)
    }

    async fn links(&self, _: &str, _: &str, _: &str, _: Option<&str>, _: u8) -> BResult<Value> {
        Err(BackendError::Unsupported("GET /api/v1/links".into()))
    }

    async fn hub(&self, _view: &str, _hub: &str) -> BResult<Value> {
        Err(BackendError::Unsupported("GET /api/v1/hubs/:hub".into()))
    }

    async fn revisions(&self, _revset: Option<&str>, _limit: Option<usize>) -> BResult<Value> {
        let op = self.st.lock().unwrap().op;
        Ok(
            json!({ "rows": [{ "rev": { "change": CHANGE, "commit": "c0ffee", "parents": [], "description": format!("work (op{op})"),
            "author": { "name": "you", "email": "you@example.com", "time": "0" }, "committerTime": "0", "bookmarks": [], "tags": [],
            "workspaces": ["default", "gym"], "flags": {}, "shortest": { "change": 4, "commit": 4 } }, "lane": 0, "edges": [] }],
            "truncated": false }),
        )
    }

    async fn interdiff(&self, _change: &str, _from: &str) -> BResult<Value> {
        Err(BackendError::Unsupported("GET /api/v1/interdiff".into()))
    }

    async fn threads(&self, q: &ThreadQuery) -> BResult<Vec<Value>> {
        let st = self.st.lock().unwrap();
        Ok(st
            .threads
            .iter()
            .filter(|t| {
                q.path
                    .as_ref()
                    .is_none_or(|p| t["anchor"]["path"] == p.as_str())
            })
            .filter(|t| q.status.is_empty() || q.status.iter().any(|s| t["status"] == s.as_str()))
            .filter(|t| {
                q.assignee
                    .as_ref()
                    .is_none_or(|a| t["assignee"] == a.as_str())
            })
            .cloned()
            .collect())
    }

    async fn create_thread(&self, who: &AgentId, body: Value) -> BResult<Value> {
        self.with(|st| {
            let a = &body["anchor"];
            let path = a["path"]
                .as_str()
                .ok_or_else(|| BackendError::invalid("anchor.path required"))?;
            if !st.files.contains_key(path) {
                return Err(BackendError::invalid(format!("unknown path {path}")));
            }
            let start = a["start"].as_u64().unwrap_or(1) as u32;
            let end = a["end"].as_u64().unwrap_or(start as u64) as u32;
            let kind = body["kind"].as_str().unwrap_or("comment");
            let t = make_thread(
                st,
                path,
                start,
                end,
                body["body"].as_str().unwrap_or(""),
                kind,
                body["assignee"].as_str(),
                who,
                true,
            );
            st.threads.push(t.clone());
            st.emit(json!({ "type": "thread.upserted", "thread": t }));
            Ok(t)
        })
    }

    async fn comment(
        &self,
        who: &AgentId,
        thread: &str,
        body: &str,
        set_status: Option<&str>,
    ) -> BResult<Value> {
        self.with(|st| {
            let cid = st.id("cm");
            let t = st.thread_mut(thread)?;
            if !body.is_empty() {
                t["comments"].as_array_mut().unwrap().push(
                    json!({ "id": cid, "author": who.actor(), "body": body, "createdAt": "1" }),
                );
            }
            if let Some(s) = set_status {
                t["status"] = json!(s);
            }
            let t = t.clone();
            st.emit(json!({ "type": "thread.upserted", "thread": t }));
            Ok(t)
        })
    }

    async fn patch_thread(&self, _who: &AgentId, thread: &str, patch: Value) -> BResult<Value> {
        self.with(|st| {
            let t = st.thread_mut(thread)?;
            for k in ["status", "assignee"] {
                if let Some(v) = patch.get(k) {
                    t[k] = v.clone();
                }
            }
            let t = t.clone();
            st.emit(json!({ "type": "thread.upserted", "thread": t }));
            Ok(t)
        })
    }

    async fn declare_link(&self, _who: &AgentId, body: Value) -> BResult<Value> {
        self.with(|st| {
            let id = st.id("l");
            Ok(json!({ "id": id, "kind": body["kind"], "label": body["label"], "provenance": "agent" }))
        })
    }

    async fn rate_link(&self, _who: &AgentId, link: &str, body: Value) -> BResult<Value> {
        Ok(json!({ "id": link, "verdict": body["verdict"] }))
    }

    async fn label_cluster(&self, _who: &AgentId, _c: &str, _body: Value) -> BResult<Value> {
        Ok(json!({ "ok": true }))
    }

    async fn edit(&self, who: &AgentId, req: Value) -> BResult<Value> {
        self.with(|st| {
            let target = req["target"].as_str().unwrap_or("");
            let mode = req["mode"].as_str().unwrap_or("amend");
            for f in req["files"].as_array().into_iter().flatten() {
                let path = f["path"].as_str().unwrap_or("");
                let cur = st
                    .current
                    .get(path)
                    .ok_or_else(|| BackendError::invalid(format!("unknown path {path}")))?;
                if let Some(base) = f["baseBlob"].as_str()
                    && base != cur
                {
                    return Err(BackendError::api(
                        409,
                        "stale-base",
                        format!("{path} moved on: base {base}, now {cur}; re-read the file"),
                    ));
                }
            }
            let id = st.id("e");
            let result = json!({
                "id": id, "status": "previewed", "actor": who.actor(),
                "created": if mode == "amend" { Value::Null } else { json!("newchange") },
                "rewritten": [target],
                "workspaces": [{ "name": who.workspace.clone().unwrap_or_else(|| "default".into()), "snapshotted": true, "updated": true }],
                "conflicts": [], "predicted": true
            });
            st.edits.insert(id.clone(), (req.clone(), result.clone()));
            if req["dryRun"].as_bool() == Some(false) {
                return st.apply(&id, who);
            }
            Ok(result)
        })
    }

    async fn apply_edit(&self, who: &AgentId, edit: &str) -> BResult<Value> {
        self.with(|st| st.apply(edit, who))
    }

    async fn undo_edit(&self, _who: &AgentId, edit: &str) -> BResult<Value> {
        Err(BackendError::Unsupported(format!(
            "POST /api/v1/edits/{edit}/undo"
        )))
    }

    async fn conflicts(&self, _revset: Option<&str>) -> BResult<Vec<Value>> {
        let st = self.st.lock().unwrap();
        Ok(st
            .conflicts
            .iter()
            .map(|(c, _)| {
                let mut r = c.clone();
                r.as_object_mut().unwrap().remove("regions");
                r
            })
            .collect())
    }

    async fn conflict(&self, _commit: &str, path: &str) -> BResult<Value> {
        let st = self.st.lock().unwrap();
        let (c, _) = st
            .conflicts
            .iter()
            .find(|(_, p)| p == path)
            .ok_or_else(|| BackendError::not_found("no open conflict in that file"))?;
        let mut d = c.clone();
        d["materialized"] = json!(st.files[path].join("\n"));
        Ok(d)
    }

    async fn resolve_conflict(
        &self,
        who: &AgentId,
        _commit: &str,
        path: &str,
        content: &str,
    ) -> BResult<Value> {
        self.with(|st| {
            let i = st
                .conflicts
                .iter()
                .position(|(_, p)| p == path)
                .ok_or_else(|| BackendError::not_found("no open conflict in that file"))?;
            let (mut rec, _) = st.conflicts.remove(i);
            rec.as_object_mut().unwrap().remove("regions");
            st.emit(json!({ "type": "conflicts.changed", "added": [], "resolved": [rec] }));
            let id = st.id("e");
            let req = json!({ "target": CHANGE, "mode": "amend", "files": [{ "path": path, "content": content }] });
            let result = json!({ "id": id, "status": "previewed", "actor": who.actor(), "rewritten": [CHANGE],
                                 "workspaces": [], "conflicts": [], "predicted": true });
            st.edits.insert(id.clone(), (req, result));
            st.apply(&id, who)
        })
    }

    async fn events(
        &self,
        cursor: Option<&str>,
        kinds: &[String],
        timeout: Duration,
    ) -> BResult<Value> {
        let start = match cursor {
            Some(c) => c
                .parse::<usize>()
                .map_err(|_| BackendError::invalid(format!("bad cursor {c}")))?,
            None => self.st.lock().unwrap().log.len(),
        };
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let st = self.st.lock().unwrap();
                let end = st.log.len();
                let events: Vec<Value> = st.log[start.min(end)..]
                    .iter()
                    .filter(|e| kinds.is_empty() || kinds.iter().any(|k| e["type"] == k.as_str()))
                    .cloned()
                    .collect();
                if !events.is_empty() || tokio::time::Instant::now() >= deadline {
                    return Ok(json!({ "cursor": end.to_string(), "events": events }));
                }
            }
            let _ = tokio::time::timeout_at(deadline, notified).await;
        }
    }

    async fn cursor(&self) -> BResult<Option<String>> {
        Ok(Some(self.st.lock().unwrap().log.len().to_string()))
    }
}
