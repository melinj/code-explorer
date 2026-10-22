//! Synthetic data behind the F4 panels: a jj history graph with workspaces, workspace cards, Hotspots
//! metrics and co-change pairs, search, and revset validation.

use serde_json::{Value, json};

use crate::synth::{Rng, View, hash};

pub struct Panels {
    pub revisions: Vec<Value>,
    pub workspaces: std::sync::Mutex<Vec<Value>>,
}

fn hex(rng: &mut Rng, n: usize) -> String {
    (0..n).map(|_| format!("{:x}", rng.next() % 16)).collect()
}

/// jj change ids use the letters k–z.
fn change_id(rng: &mut Rng) -> String {
    (0..32)
        .map(|_| (b'k' + (rng.next() % 16) as u8) as char)
        .collect()
}

const SUBJECTS: [&str; 12] = [
    "map: retry district builds on timeout",
    "events: allow '#' in subscriptions",
    "world: emit entity.moved with district id",
    "gym: walk speed curve",
    "routines: day per schedule phase",
    "agent: trim prompt context",
    "store: batch writes per tick",
    "web api: paginate node list",
    "config: split settings per env",
    "tests: cover empty market",
    "docs: district lifecycle",
    "progression: xp table v2",
];

impl Panels {
    pub fn new(view: &View) -> Self {
        let mut rng = Rng::new(5);
        let paths: Vec<String> = view
            .files
            .iter()
            .take(4000)
            .map(|f| f.path.clone())
            .collect();
        let mut rows: Vec<Value> = Vec::new();
        // Stacks: (workspace, length, base offset on trunk, agent?)
        let stacks = [
            ("default", 3, 0, None),
            ("gym", 2, 2, Some("claude")),
            ("progression", 1, 5, Some("opencode")),
        ];
        let mut trunk: Vec<(String, String)> = (0..40)
            .map(|_| (change_id(&mut rng), hex(&mut rng, 40)))
            .collect();
        trunk.reverse(); // newest first
        let t0 = 1_792_300_000i64;
        let push = |rows: &mut Vec<Value>,
                    change: &str,
                    commit: &str,
                    parent: &str,
                    lane: u32,
                    to_lane: u32,
                    desc: &str,
                    ws: Option<&str>,
                    immutable: bool,
                    wc: bool,
                    time: i64,
                    review: Option<Value>| {
            let mut row = json!({
                "rev": {
                    "change": change, "commit": commit, "parents": [parent], "description": desc,
                    "author": { "name": if ws.is_some_and(|w| w != "default") { "agent" } else { "you" }, "email": "dev@example.com", "time": crate::review::iso(time) },
                    "committerTime": crate::review::iso(time),
                    "bookmarks": [], "tags": [],
                    "workspaces": ws.map(|w| vec![w]).unwrap_or_default(),
                    "flags": { "conflict": false, "empty": false, "immutable": immutable, "divergent": false, "hidden": false, "workingCopy": wc },
                    "shortest": { "change": 4, "commit": 4 }
                },
                "lane": lane,
                "edges": [{ "toLane": to_lane, "parent": parent }]
            });
            if let Some(r) = review {
                row["review"] = r;
            }
            rows.push(row);
        };
        for (si, (ws, len, base, _)) in stacks.iter().enumerate() {
            let lane = si as u32 + 1;
            let mut parent = trunk[*base].1.clone();
            let mut chain = Vec::new();
            for k in 0..*len {
                let (c, h) = (change_id(&mut rng), hex(&mut rng, 40));
                chain.push((c, h.clone(), parent.clone(), k));
                parent = h;
            }
            for (k, (c, h, p, _)) in chain.iter().enumerate().rev() {
                let tip = k + 1 == chain.len();
                let desc = SUBJECTS[(rng.next() % SUBJECTS.len() as u64) as usize];
                let review = if *ws == "default" && k == 1 {
                    Some(
                        json!({ "change": c, "openThreads": 2, "changedSinceReview": false, "touchedByAgent": false }),
                    )
                } else if *ws == "gym" && k == 0 {
                    // Rewritten since review: reviewedAt names the commit last reviewed (a stand-in
                    // for an earlier version of the change), which the interdiff starts from.
                    Some(
                        json!({ "change": c, "openThreads": 0, "changedSinceReview": true, "reviewedAt": p, "touchedByAgent": true }),
                    )
                } else if *ws == "gym" {
                    Some(
                        json!({ "change": c, "openThreads": 0, "changedSinceReview": false, "touchedByAgent": true }),
                    )
                } else {
                    None
                };
                push(
                    &mut rows,
                    c,
                    h,
                    p,
                    lane,
                    if k == 0 { 0 } else { lane },
                    if tip && *ws == "default" { "" } else { desc },
                    tip.then_some(*ws),
                    false,
                    tip && *ws == "default",
                    t0 - k as i64 * 600,
                    review,
                );
            }
        }
        for (i, (c, h)) in trunk.iter().enumerate() {
            let parent = trunk
                .get(i + 1)
                .map(|p| p.1.clone())
                .unwrap_or_else(|| "0".repeat(40));
            let desc = SUBJECTS[(rng.next() % SUBJECTS.len() as u64) as usize];
            push(
                &mut rows,
                c,
                h,
                &parent,
                0,
                0,
                desc,
                None,
                true,
                false,
                t0 - 3600 - i as i64 * 7200,
                None,
            );
            if i == 0 {
                rows.last_mut().unwrap()["rev"]["bookmarks"] = json!(["trunk"]);
            }
        }
        // Order: stacks first (newest), then trunk.
        let workspaces = stacks
            .iter()
            .enumerate()
            .map(|(si, (ws, len, _, agent))| {
                let files: Vec<&String> = (0..6).map(|k| &paths[(hash(ws) as usize + k * 97) % paths.len()]).collect();
                let mut card = json!({
                    "name": ws,
                    "root": format!("/home/dev/workspace/sample-app{}", if *ws == "default" { String::new() } else { format!("-{ws}") }),
                    "wc": { "change": "", "commit": "" },
                    "serving": *ws == "default",
                    "stack": { "revset": format!("trunk()..{ws}@"), "changes": len, "files": files.len(), "conflicts": 0 },
                    "unsnapshottedFiles": ([2, 0, 1])[si],
                    "agent": agent.map(|a| json!({ "name": a, "client": if a == "claude" { "claude-code" } else { "opencode" }, "activity": if si == 1 { "editing" } else { "reading" }, "lastSeen": crate::review::iso(t0) })),
                    "lastOp": { "op": hex(&mut rng, 12), "time": crate::review::iso(t0 - si as i64 * 300), "description": "snapshot working copy" },
                    "overlaps": if si == 1 { json!([{ "workspace": "default", "paths": [files[0], files[1]] }]) } else if si == 0 { json!([{ "workspace": "gym", "paths": [files[0]] }]) } else { json!([]) },
                    "files": files,
                });
                // WorkspaceSummary.agent is omitted (not null) when no agent is bound.
                if card["agent"].is_null() {
                    card.as_object_mut().unwrap().remove("agent");
                }
                card
            })
            .collect();
        Panels {
            revisions: rows,
            workspaces: std::sync::Mutex::new(workspaces),
        }
    }

    /// Hotspots: one point per cluster (or file), plus co-change pairs.
    pub fn metrics(view: &View, level: &str, limit: usize) -> Value {
        let mut points: Vec<Value> = Vec::new();
        if level == "file" {
            let mut ws: Vec<&crate::synth::Win> = view.windows.iter().collect();
            ws.sort_by(|a, b| b.header.risk.total_cmp(&a.header.risk));
            for w in ws.iter().take(limit) {
                let h = &w.header;
                points.push(json!({
                    "key": h.key, "label": h.symbol.as_ref().map(|s| s.qualname.clone()).unwrap_or(h.path.clone()), "lane": h.lane,
                    "heat": h.heat, "conn": h.conn,
                    "heatPct": h.heat.rate / 0.1_f64.max(h.heat.rate), "connPct": (h.conn.dependents2 as f64 / 500.0).min(1.0),
                    "risk": h.risk, "window": h.id,
                }));
            }
        } else {
            for c in view.bootstrap.clusters.iter().take(limit) {
                let heat_pct = (c.heat as f64 + 0.5) / 5.0;
                let conn_pct = (c.conn as f64 + 0.5) / 5.0;
                points.push(json!({
                    "key": c.id, "label": c.label, "lane": c.lane,
                    "heat": { "grade": c.heat, "rate": 0, "ci": [0, 0], "settled": true, "touches": 0 },
                    "conn": { "grade": c.conn, "dependents2": 0, "fanIn": 0, "fanOut": 0, "tests": 0, "reach": 0 },
                    "heatPct": heat_pct, "connPct": conn_pct, "risk": (heat_pct * conn_pct).sqrt(),
                }));
            }
        }
        // Jitter cluster points inside their grade cell so the scatter reads as a cloud.
        let mut rng = Rng::new(3);
        for p in points.iter_mut() {
            for k in ["heatPct", "connPct"] {
                let v = p[k].as_f64().unwrap_or(0.0);
                p[k] = json!((v + (rng.f() - 0.5) * 0.18).clamp(0.0, 1.0));
            }
            let r = (p["heatPct"].as_f64().unwrap() * p["connPct"].as_f64().unwrap()).sqrt();
            p["risk"] = json!(r);
        }
        let hot: Vec<&String> = view
            .files
            .iter()
            .filter(|f| view.windows[f.windows[0]].header.heat.grade >= 3)
            .map(|f| &f.path)
            .take(400)
            .collect();
        let cluster_of = |path: &String| {
            view.files
                .iter()
                .find(|f| &f.path == path)
                .map(|f| view.windows[f.windows[0]].header.cluster.clone())
        };
        let mut cochange = Vec::new();
        // Cluster pairs as cv serve sends them: the strongest file pair between two clusters.
        let mut clusters: std::collections::BTreeMap<(String, String), (u32, f64)> =
            Default::default();
        for i in 0..hot.len().min(120) {
            let j = (i * 37 + 11) % hot.len();
            if i != j {
                let (support, jaccard) = (3 + rng.range(0, 9), 0.3 + rng.f() * 0.5);
                let (ca, cb) = (cluster_of(hot[i]), cluster_of(hot[j]));
                cochange.push(json!({ "a": hot[i], "b": hot[j], "support": support, "jaccard": jaccard, "clusterA": ca, "clusterB": cb }));
                if let (Some(ca), Some(cb)) = (ca, cb)
                    && ca != cb
                {
                    let k = if ca < cb { (ca, cb) } else { (cb, ca) };
                    let e = clusters.entry(k).or_insert((0, 0.0));
                    e.0 += support;
                    e.1 = f64::max(e.1, jaccard * 0.6);
                }
            }
        }
        let cochange_clusters: Vec<Value> = clusters
            .into_iter()
            .enumerate()
            .map(|(i, ((a, b), (support, jaccard)))| json!({ "a": a, "b": b, "support": support, "jaccard": jaccard, "linked": i % 3 == 0 }))
            .collect();
        json!({ "points": points, "cochange": cochange, "cochangeClusters": cochange_clusters })
    }

    pub fn search(view: &View, threads: &[Value], q: &str, limit: usize) -> Vec<Value> {
        let q = q.to_lowercase();
        let mut hits: Vec<(f64, Value)> = Vec::new();
        for w in &view.windows {
            let h = &w.header;
            let name = h
                .symbol
                .as_ref()
                .map(|s| s.qualname.to_lowercase())
                .unwrap_or_default();
            let path = h.path.to_lowercase();
            let score = if name == q {
                1.0
            } else if name.starts_with(&q) {
                0.8
            } else if name.contains(&q) {
                0.6
            } else if path.contains(&q) {
                0.4
            } else {
                continue;
            };
            hits.push((score + h.risk * 0.1, json!({
                "kind": if score >= 0.6 { "symbol" } else { "file" },
                "label": h.symbol.as_ref().map(|s| s.qualname.clone()).unwrap_or(h.path.clone()),
                "detail": format!("{}:{}", h.path, h.lines.start),
                "target": { "window": h.id },
                "score": score,
            })));
        }
        for h in &view.bootstrap.hubs {
            if h.key.to_lowercase().contains(&q) {
                hits.push((0.7, json!({ "kind": "hub", "label": h.display, "detail": h.protocol, "target": { "hub": h.id }, "score": 0.7 })));
            }
        }
        for t in threads {
            let body = t["comments"][0]["body"].as_str().unwrap_or("");
            if body.to_lowercase().contains(&q) {
                hits.push((0.5, json!({
                    "kind": "thread", "label": body.chars().take(80).collect::<String>(),
                    "detail": format!("{}:{}", t["anchor"]["path"].as_str().unwrap_or(""), t["anchor"]["start"]),
                    "target": { "thread": t["id"], "site": { "path": t["anchor"]["path"], "line": t["anchor"]["start"] } }, "score": 0.5
                })));
            }
        }
        hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        hits.into_iter().take(limit).map(|(_, v)| v).collect()
    }

    /// A tolerant revset check: balanced parentheses and known functions.
    pub fn validate(&self, revset: &str) -> Value {
        let depth = revset.chars().try_fold(0i32, |d, c| {
            let d = d + (c == '(') as i32 - (c == ')') as i32;
            (d >= 0).then_some(d)
        });
        if revset.trim().is_empty() {
            return json!({ "ok": false, "error": "empty revset", "hint": "try trunk()..@" });
        }
        if depth != Some(0) {
            return json!({ "ok": false, "error": "unbalanced parentheses", "hint": "check the brackets" });
        }
        if let Some(i) = revset.find('(') {
            let name = revset[..i]
                .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("");
            let known = [
                "trunk",
                "mine",
                "heads",
                "roots",
                "ancestors",
                "descendants",
                "bookmarks",
                "all",
                "none",
                "present",
                "empty",
                "conflicts",
                "description",
                "author",
                "files",
            ];
            if !name.is_empty() && !known.contains(&name) {
                return json!({ "ok": false, "error": format!("function `{name}` doesn't exist"), "hint": "did you mean trunk()?" });
            }
        }
        let stack: Vec<&Value> = self.revisions.iter().filter(|r| r["lane"] != 0).collect();
        let n = if revset.contains("..") {
            stack.len().min(6)
        } else {
            1
        };
        let warnings = if revset.contains('|') {
            json!([{ "kind": "gap", "missing": [stack[0]["rev"]["change"]] }])
        } else {
            json!([])
        };
        json!({
            "ok": true, "count": n,
            "changes": stack.iter().take(n).map(|r| r["rev"]["change"].clone()).collect::<Vec<_>>(),
            "files": n * 3, "added": n * 41, "removed": n * 12, "conflicts": 0, "warnings": warnings
        })
    }
}
