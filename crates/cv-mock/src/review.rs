//! In-memory review threads and edits (B8's routes from `docs/contracts/http.ts`), so the web client's
//! comment and edit flows work end to end against the mock. Applied edits patch the file text that
//! `/blobs/{blob}/text` and outline batches serve afterwards.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::synth::{Rng, View};
use crate::text::{self, Line};

/// A conflicted region of a file at the viewed commit: two sides replacing lines start..=end.
pub struct Conflict {
    pub start: u32,
    pub end: u32,
    pub sides: Vec<(String, Vec<String>)>,
    pub resolved: bool,
}

/// A file index and its edited lines before an edit (None = the generated text).
type FileBefore = (usize, Option<Vec<Line>>);

pub struct Review {
    /// Who is writing right now (from the X-CV-Agent headers; a human by default).
    pub actor: Value,
    pub threads: Vec<Value>,
    /// Open conflicts by file index.
    pub conflicts: HashMap<usize, Conflict>,
    edits: HashMap<String, (Value, Value)>,
    /// Edited files: whole-file lines replacing the generated text.
    pub files: HashMap<usize, Vec<Line>>,
    /// Per applied edit: each touched file's state before it (None = generated text), for undo.
    before: HashMap<String, Vec<FileBefore>>,
    next: u64,
}

/// Now as an ISO-8601 UTC time (ids.ts wire convention); ordering comes from creation order.
pub fn now() -> String {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    iso(s as i64)
}

/// `secs` since the epoch as ISO-8601 UTC, e.g. "2026-10-18T13:25:45Z" (civil-from-days).
pub fn iso(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

const SEED_BODIES: [&str; 5] = [
    "Unbounded retries here hide a stuck builder; cap them and back off.",
    "This emits before the store write commits. Is that ordering intended?",
    "Can this branch be reached? The caller already checks for None.",
    "Name is misleading: it mutates the shared state, it doesn't just read it.",
    "Missing test for the empty-input case.",
];

impl Review {
    pub fn new(view: &View) -> Self {
        let mut r = Review {
            actor: json!({ "kind": "human", "name": "you" }),
            threads: Vec::new(),
            conflicts: HashMap::new(),
            edits: HashMap::new(),
            files: HashMap::new(),
            before: HashMap::new(),
            next: 1,
        };
        let mut rng = Rng::new(99);
        // Conflicts where the canvas shows them: two sides rewriting a few lines of the window.
        for w in view
            .windows
            .iter()
            .filter(|w| w.header.counts.conflicts > 0)
        {
            let h = &w.header;
            if r.conflicts.contains_key(&w.file) || h.lines.end - h.lines.start < 8 {
                continue;
            }
            let lines = text::file_lines(&view.files[w.file]);
            let start = h.lines.start + 3;
            let end = start + 2;
            let base: Vec<String> = (start..=end)
                .map(|n| lines[(n - 1) as usize].text.clone())
                .collect();
            let ours: Vec<String> = base.iter().map(|l| format!("{l}  # kxwq: retry")).collect();
            let theirs: Vec<String> = base.iter().map(|l| crate::events::refactor(l)).collect();
            r.conflicts.insert(
                w.file,
                Conflict {
                    start,
                    end,
                    sides: vec![
                        ("kxwqmnpo (amend)".into(), ours),
                        ("rqtmtvrn (rebased)".into(), theirs),
                    ],
                    resolved: false,
                },
            );
        }
        // Seed threads so window thread counts match what the canvas shows.
        for w in &view.windows {
            let h = &w.header;
            for k in 0..h.counts.open_threads {
                let line = h.lines.start + rng.range(0, (h.lines.end - h.lines.start).min(30));
                let body = SEED_BODIES[(rng.next() % SEED_BODIES.len() as u64) as usize];
                let agent = k % 2 == 1;
                let t = r.make_thread(
                    &h.path,
                    line,
                    line,
                    body,
                    if agent { "finding" } else { "comment" },
                    None,
                    agent,
                );
                r.threads.push(t);
            }
        }
        r
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    #[allow(clippy::too_many_arguments)]
    fn make_thread(
        &mut self,
        path: &str,
        start: u32,
        end: u32,
        body: &str,
        kind: &str,
        assignee: Option<&str>,
        by_agent: bool,
    ) -> Value {
        let id = self.id("t");
        let cid = self.id("cm");
        let author = if by_agent {
            json!({ "kind": "agent", "name": "claude", "client": "claude-code" })
        } else {
            json!({ "kind": "human", "name": "you" })
        };
        let at = now();
        let mut t = json!({
            "id": id,
            "anchor": {
                "change": "kxwqmnpo", "commit": "c0ffee", "path": path, "side": "new",
                "start": start, "end": end,
                "fingerprint": { "text": "", "before": "", "after": "" }
            },
            "status": "open",
            "kind": kind,
            "createdBy": author,
            "createdAt": at,
            "updatedAt": at,
            "comments": [{ "id": cid, "author": author, "body": body, "createdAt": at }],
            "producedEdits": []
        });
        // Optional fields are omitted, never null (http.ts).
        if let Some(a) = assignee {
            t["assignee"] = json!(a);
        }
        t
    }

    pub fn list(&self, path: Option<&str>) -> Vec<Value> {
        self.threads
            .iter()
            .filter(|t| path.is_none_or(|p| t["anchor"]["path"] == p))
            .cloned()
            .collect()
    }

    pub fn create(&mut self, req: &Value) -> Result<Value, String> {
        let a = &req["anchor"];
        let path = a["path"].as_str().ok_or("anchor.path required")?;
        let start = a["start"].as_u64().ok_or("anchor.start required")? as u32;
        let end = a["end"].as_u64().unwrap_or(start as u64) as u32;
        let body = req["body"].as_str().unwrap_or("");
        let kind = req["kind"].as_str().unwrap_or("comment");
        let assignee = req["assignee"].as_str();
        let mut t = self.make_thread(path, start, end, body, kind, assignee, false);
        t["createdBy"] = self.actor.clone();
        t["comments"][0]["author"] = self.actor.clone();
        self.threads.push(t.clone());
        Ok(t)
    }

    pub fn comment(&mut self, id: &str, req: &Value) -> Option<Value> {
        let cid = self.id("cm");
        let t = self.threads.iter_mut().find(|t| t["id"] == id)?;
        if let Some(body) = req["body"].as_str().filter(|b| !b.is_empty()) {
            t["comments"].as_array_mut()?.push(json!({
                "id": cid, "author": self.actor.clone(), "body": body, "createdAt": now()
            }));
        }
        if let Some(s) = req["setStatus"].as_str() {
            t["status"] = json!(s);
        }
        t["updatedAt"] = json!(now());
        Some(t.clone())
    }

    /// Set the acting identity from MCP attribution headers (X-CV-Agent …); human otherwise.
    pub fn as_actor(&mut self, headers: &axum::http::HeaderMap) -> &mut Self {
        let h = |k: &str| {
            headers
                .get(k)
                .and_then(|v| v.to_str().ok())
                .map(String::from)
        };
        self.actor = match h("x-cv-agent") {
            Some(name) => {
                json!({ "kind": "agent", "name": name, "client": h("x-cv-agent-client") })
            }
            None => json!({ "kind": "human", "name": "you" }),
        };
        self
    }

    /// A comment from the (simulated) agent, optionally moving the thread to `status`.
    pub fn agent_comment(&mut self, id: &str, body: &str, status: Option<&str>) -> Option<Value> {
        let cid = self.id("cm");
        let t = self.threads.iter_mut().find(|t| t["id"] == id)?;
        t["comments"].as_array_mut()?.push(json!({
            "id": cid, "author": { "kind": "agent", "name": "claude", "client": "claude-code" }, "body": body, "createdAt": now()
        }));
        if let Some(s) = status {
            t["status"] = json!(s);
        }
        t["updatedAt"] = json!(now());
        Some(t.clone())
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        self.threads.iter().find(|t| t["id"] == id).cloned()
    }

    pub fn patch(&mut self, id: &str, req: &Value) -> Option<Value> {
        let t = self.threads.iter_mut().find(|t| t["id"] == id)?;
        if let Some(s) = req.get("status") {
            t["status"] = s.clone();
        }
        match req.get("assignee") {
            Some(Value::Null) => {
                t.as_object_mut().map(|o| o.remove("assignee"));
            }
            Some(a) => t["assignee"] = a.clone(),
            None => {}
        }
        Some(t.clone())
    }

    /// Paths an edit touches.
    pub fn paths_of(&self, id: &Value) -> Vec<String> {
        let Some((req, _)) = id.as_str().and_then(|id| self.edits.get(id)) else {
            return Vec::new();
        };
        req["files"]
            .as_array()
            .map(|fs| {
                fs.iter()
                    .filter_map(|f| f["path"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn conflict_records(&self, view: &View) -> Vec<Value> {
        let mut out: Vec<Value> = self
            .conflicts
            .iter()
            .filter(|(_, c)| !c.resolved)
            .map(|(&f, c)| {
                json!({ "change": "kxwqmnpo", "commit": "c0ffee", "path": view.files[f].path, "sides": c.sides.len(), "firstSeenOp": "4eb0965a" })
            })
            .collect();
        out.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        out
    }

    /// ConflictDetail: the file with jj-style conflict markers, plus the structured regions.
    pub fn conflict_detail(&self, view: &View, path: &str) -> Option<Value> {
        let f = view.files.iter().position(|x| x.path == path)?;
        let c = self.conflicts.get(&f).filter(|c| !c.resolved)?;
        let lines = self.lines(view, f);
        let mut out: Vec<String> = lines[..(c.start - 1) as usize]
            .iter()
            .map(|l| l.text.clone())
            .collect();
        out.push("<<<<<<< Conflict 1 of 1".into());
        for (i, (label, side)) in c.sides.iter().enumerate() {
            out.push(format!(
                "{} {label}",
                if i == 0 { "+++++++" } else { "%%%%%%%" }
            ));
            out.extend(side.iter().cloned());
        }
        out.push(">>>>>>> Conflict 1 of 1 ends".into());
        out.extend(lines[c.end as usize..].iter().map(|l| l.text.clone()));
        let base: Vec<String> = (c.start..=c.end)
            .map(|n| lines[(n - 1) as usize].text.clone())
            .collect();
        Some(json!({
            "change": "kxwqmnpo", "commit": "c0ffee", "path": path, "sides": c.sides.len(), "firstSeenOp": "4eb0965a",
            "materialized": out.join("\n"),
            "regions": [{
                "start": c.start, "end": c.end, "base": base.join("\n"),
                "sides": c.sides.iter().map(|(l, t)| json!({ "label": l, "text": t.join("\n") })).collect::<Vec<_>>()
            }]
        }))
    }

    /// Resolve with the whole new file content (as the contract says) and record it as an edit.
    pub fn resolve_conflict(
        &mut self,
        view: &View,
        path: &str,
        content: &str,
        by_agent: bool,
    ) -> Option<Value> {
        let f = view.files.iter().position(|x| x.path == path)?;
        let c = self.conflicts.get_mut(&f)?;
        c.resolved = true;
        self.files
            .insert(f, content.split('\n').map(plain).collect());
        let id = self.id("e");
        let result = json!({
            "id": id, "status": "applied",
            "actor": if by_agent { json!({ "kind": "agent", "name": "claude", "client": "claude-code" }) } else { json!({ "kind": "human", "name": "you" }) },
            "rewritten": ["kxwqmnpo"], "workspaces": [{ "name": "default", "snapshotted": true, "updated": true }],
            "conflicts": [], "predicted": false, "opBefore": "4eb0965a", "opAfter": format!("op{id}")
        });
        self.edits
            .insert(id, (json!({ "files": [{ "path": path }] }), result.clone()));
        Some(result)
    }

    /// Current lines of a file (edited or generated).
    pub fn lines(&self, view: &View, file: usize) -> Vec<Line> {
        self.files
            .get(&file)
            .cloned()
            .unwrap_or_else(|| text::file_lines(&view.files[file]))
    }

    /// Preview (dryRun) or apply an EditRequest; the mock predicts no conflicts and amends in place.
    pub fn edit(&mut self, view: &View, req: &Value) -> Result<Value, String> {
        let target = req["target"].as_str().unwrap_or("c0ffee");
        let mode = req["mode"].as_str().unwrap_or("amend");
        if mode != "amend" && mode != "branch" && mode != "insert-after" {
            return Err(format!("mode {mode} is not supported by the mock"));
        }
        for f in req["files"].as_array().ok_or("files required")? {
            let path = f["path"].as_str().ok_or("files[].path required")?;
            view.files
                .iter()
                .position(|x| x.path == path)
                .ok_or(format!("unknown path {path}"))?;
        }
        let id = self.id("e");
        let mut result = json!({
            "id": id,
            "status": "previewed",
            "actor": self.actor.clone(),
            "rewritten": [target],
            "workspaces": [{ "name": "default", "snapshotted": true, "updated": true }],
            "conflicts": [],
            "predicted": true
        });
        // EditResult.created: only when the edit made a new change (omitted for amend).
        if mode != "amend" {
            result["created"] = json!("newchange");
        }
        self.edits.insert(id.clone(), (req.clone(), result.clone()));
        if req["dryRun"].as_bool() == Some(false) {
            return self.apply(view, &id).ok_or("edit vanished".into());
        }
        Ok(result)
    }

    pub fn apply(&mut self, view: &View, id: &str) -> Option<Value> {
        let (req, mut result) = self.edits.get(id).cloned()?;
        let mut before = Vec::new();
        for f in req["files"].as_array()? {
            let path = f["path"].as_str()?;
            let fi = view.files.iter().position(|x| x.path == path)?;
            before.push((fi, self.files.get(&fi).cloned()));
            let mut lines = self.lines(view, fi);
            if let Some(content) = f["content"].as_str() {
                lines = content.split('\n').map(plain).collect();
            }
            // Patches apply bottom-up so earlier line numbers stay valid.
            let mut patches: Vec<&Value> = f["patches"]
                .as_array()
                .map(|p| p.iter().collect())
                .unwrap_or_default();
            patches.sort_by_key(|p| std::cmp::Reverse(p["start"].as_u64().unwrap_or(0)));
            for p in patches {
                let start = (p["start"].as_u64()? as usize).max(1);
                let end = p["end"].as_u64()? as usize;
                let new: Vec<Line> = p["text"].as_str()?.split('\n').map(plain).collect();
                let from = (start - 1).min(lines.len());
                let to = end.min(lines.len()).max(from);
                lines.splice(from..to, new);
            }
            self.files.insert(fi, lines);
        }
        result["status"] = json!("applied");
        result["predicted"] = json!(false);
        result["opBefore"] = json!("4eb0965a");
        result["opAfter"] = json!(format!("op{id}"));
        self.edits.insert(id.to_string(), (req, result.clone()));
        self.before.insert(id.to_string(), before);
        Some(result)
    }

    /// The edit's latest EditResult (GET /edits/:id).
    pub fn get_edit(&self, id: &str) -> Option<Value> {
        self.edits.get(id).map(|(_, r)| r.clone())
    }

    /// Undo an applied edit: restores the files it touched. `Err(None)` = unknown edit,
    /// `Err(Some(status))` = not applied (409 in the real server).
    pub fn undo(&mut self, id: &str) -> Result<Value, Option<String>> {
        let (req, mut result) = self.edits.get(id).cloned().ok_or(None)?;
        if result["status"] != "applied" {
            return Err(Some(result["status"].as_str().unwrap_or("?").to_string()));
        }
        for (fi, old) in self.before.remove(id).unwrap_or_default().into_iter().rev() {
            match old {
                Some(lines) => self.files.insert(fi, lines),
                None => self.files.remove(&fi),
            };
        }
        result["status"] = json!("undone");
        result["opBefore"] = result["opAfter"].clone();
        result["opAfter"] = json!(format!("op{id}-undo"));
        result["actor"] = self.actor.clone();
        self.edits.insert(id.to_string(), (req, result.clone()));
        Ok(result)
    }
}

/// An edited line: text without tokens (the mock does not re-highlight).
fn plain(s: &str) -> Line {
    Line {
        text: s.to_string(),
        tokens: Vec::new(),
    }
}
