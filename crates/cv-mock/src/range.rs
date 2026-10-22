//! Range views (DESIGN D16, §4.10 step 5): the snapshot's map, but only the windows the range
//! changes (as hunks with diff lines) plus 1-hop context windows, which start minimized.

use std::collections::{HashMap, HashSet};

use cv_contracts::ViewBootstrap;
use serde_json::{Value, json};

use crate::synth::{Rng, View, hash};

pub struct RangeView {
    pub id: String,
    pub bootstrap: ViewBootstrap,
    pub changed: HashSet<usize>,
    pub context: HashSet<usize>,
    /// Per changed window: (added lines, removed lines) for headers and diffs.
    pub churn: HashMap<usize, (u32, u32)>,
}

impl RangeView {
    pub fn new(view: &View, revset: &str, options: &Value) -> Self {
        Self::build(view, revset, options, None)
    }

    /// A range limited to `paths` (POST /workspaces:compare: the files both stacks change):
    /// every window of those files is a hunk. The spec carries the fileset.
    pub fn with_files(view: &View, revset: &str, options: &Value, paths: &[String]) -> Self {
        Self::build(view, revset, options, Some(paths))
    }

    fn build(view: &View, revset: &str, options: &Value, paths: Option<&[String]>) -> Self {
        let mut rng = Rng::new(hash(revset));
        let mut changed = HashSet::new();
        if let Some(paths) = paths {
            for f in view.files.iter().filter(|f| paths.contains(&f.path)) {
                changed.extend(f.windows.iter().copied());
            }
        } else {
            // A plausible range: a few dozen files, mostly hot ones, all windows of each file.
            let hot: Vec<usize> = (0..view.files.len())
                .filter(|&f| view.windows[view.files[f].windows[0]].header.heat.grade >= 3)
                .collect();
            let n_files = 12 + (hash(revset) % 24) as usize;
            for _ in 0..n_files {
                let f = hot[(rng.next() % hot.len() as u64) as usize];
                for &w in &view.files[f].windows {
                    if rng.f() < 0.8 {
                        changed.insert(w);
                    }
                }
            }
        }
        let hops = options["contextHops"].as_u64().unwrap_or(1);
        let mut context = HashSet::new();
        if hops > 0 {
            for &w in &changed {
                for &l in view.windows[w]
                    .out_links
                    .iter()
                    .chain(view.windows[w].in_links.iter())
                {
                    let link = &view.links[l];
                    for other in [link.from, link.to] {
                        if !changed.contains(&other) {
                            context.insert(other);
                        }
                    }
                }
            }
        }
        let mut churn = HashMap::new();
        for &w in &changed {
            let h = &view.windows[w].header;
            let n = h.lines.end - h.lines.start + 1;
            churn.insert(w, (1 + rng.range(0, n.min(30)), rng.range(0, n.min(12))));
        }
        let fileset = paths.map(|ps| {
            ps.iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(" | ")
        });
        let id = format!(
            "v_range{:08x}",
            hash(&format!(
                "{revset}{options}{}",
                fileset.as_deref().unwrap_or("")
            )) as u32
        );
        let mut bootstrap = view.bootstrap.clone();
        bootstrap.view.id = id.clone();
        bootstrap.view.spec = json!({ "mode": "range", "revset": revset, "options": options });
        if let Some(f) = fileset {
            bootstrap.view.spec["fileset"] = json!(f);
        }
        // Clusters keep their place; their window count is what the range shows (0 = faint outline).
        let mut per_cluster: HashMap<&str, u32> = HashMap::new();
        for &w in changed.iter().chain(context.iter()) {
            *per_cluster
                .entry(view.windows[w].header.cluster.as_str())
                .or_default() += 1;
        }
        for c in bootstrap.clusters.iter_mut() {
            c.windows = per_cluster.get(c.id.as_str()).copied().unwrap_or(0);
        }
        bootstrap.bundles.retain(|b| {
            per_cluster.contains_key(b.from.as_str()) && per_cluster.contains_key(b.to.as_str())
        });
        bootstrap.view.stats.windows = (changed.len() + context.len()) as u32;
        RangeView {
            id,
            bootstrap,
            changed,
            context,
            churn,
        }
    }

    pub fn ref_json(&self, snapshot: &str) -> Value {
        let mut v = serde_json::to_value(&self.bootstrap.view).unwrap();
        v["sharedPlacement"] = json!(snapshot);
        let (added, removed) = self
            .churn
            .values()
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        v["stats"]["added"] = json!(added);
        v["stats"]["removed"] = json!(removed);
        v
    }

    pub fn contains(&self, w: usize) -> bool {
        self.changed.contains(&w) || self.context.contains(&w)
    }

    /// The header as this range shows it: hunks with in-range churn, context windows minimized.
    pub fn header(&self, view: &View, w: usize) -> Value {
        let mut h = serde_json::to_value(&view.windows[w].header).unwrap();
        if self.changed.contains(&w) {
            let (a, r) = self.churn[&w];
            h["kind"] = json!("hunk");
            h["heat"]["inRange"] = json!({ "commits": 1 + (a % 3), "of": 4 });
            h["diffStat"] = json!({ "added": a, "removed": r });
        } else {
            h["kind"] = json!("context");
            h["flags"]["minimized"] = json!(true);
        }
        h
    }

    /// WindowDiff lines for a changed window: added lines (with in-range owner) and deleted lines
    /// (with their old text) placed among the window's lines.
    pub fn diff(&self, view: &View, w: usize, lines: &[crate::text::Line]) -> Value {
        let h = &view.windows[w].header;
        let Some(&(added, removed)) = self.churn.get(&w) else {
            return Value::Null;
        };
        let mut rng = Rng::new(hash(&h.id));
        let n = h.lines.end - h.lines.start + 1;
        let start = h.lines.start + rng.range(0, n.saturating_sub(added + 1));
        let mut out = Vec::new();
        let mut old = start;
        for k in 0..added.min(n) {
            let line = start + k;
            if k == added / 2 {
                for d in 0..removed {
                    let text = lines
                        .get((line - 1) as usize)
                        .map(|l| l.text.clone())
                        .unwrap_or_default();
                    out.push(json!({ "kind": "del", "oldLine": old + d, "text": format!("{text}  # old {d}"), "owner": "kxwqmnpo" }));
                }
                old += removed;
            }
            out.push(json!({ "kind": "add", "newLine": line, "owner": if k % 3 == 0 { "rqtmtvrn" } else { "kxwqmnpo" } }));
        }
        json!({ "id": h.id, "lines": out })
    }
}

/// A small stand-in for jj's fileset matcher (the mock has no jj-lib): `a | b` unions of exact
/// paths or directories, quoted or not, with an optional `root-file:` / `file:` / `root:` /
/// `cwd:` prefix, and `glob:` patterns with `*`. Empty or absent matches everything.
pub fn fileset_matches(fileset: Option<&str>, path: &str) -> bool {
    let Some(f) = fileset.map(str::trim).filter(|f| !f.is_empty()) else {
        return true;
    };
    f.split('|').any(|term| {
        let t = term.trim();
        let (glob, t) = match t.split_once(':') {
            Some(("glob" | "root-glob" | "cwd-glob", rest)) => (true, rest),
            Some(("root-file" | "file" | "cwd-file" | "root" | "cwd", rest)) => (false, rest),
            _ => (false, t),
        };
        let t = t.trim().trim_matches('"').trim_end_matches('/');
        if glob {
            return glob_match(t, path);
        }
        path == t || path.starts_with(&format!("{t}/"))
    })
}

fn glob_match(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((head, rest)) => {
            let Some(s) = s.strip_prefix(head) else {
                return false;
            };
            if rest.is_empty() {
                return !s.contains('/');
            }
            (0..=s.len()).any(|i| {
                s.is_char_boundary(i) && !s[..i].contains('/') && glob_match(rest, &s[i..])
            })
        }
    }
}

/// Range options as cv serve checks them (readside::range::range_options): a wrong type, an
/// unknown field or a value outside the contract is a 400.
pub fn check_options(v: &Value) -> Result<(), String> {
    let Some(o) = (match v {
        Value::Null => return Ok(()),
        Value::Object(o) => Some(o),
        _ => None,
    }) else {
        return Err(format!("range options {v}: expected an object"));
    };
    const KNOWN: [&str; 7] = [
        "diff",
        "contextHops",
        "contextDisplay",
        "contextLines",
        "ignoreWhitespace",
        "layout",
        "allowNonChain",
    ];
    if let Some(k) = o.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        return Err(format!(
            "range options: unknown field `{k}` (known: {})",
            KNOWN.join(", ")
        ));
    }
    let s = |k: &str, ok: &[&str]| match o.get(k) {
        None => Ok(()),
        Some(Value::String(x)) if ok.contains(&x.as_str()) => Ok(()),
        Some(x) => Err(format!(
            "range options: {k} {x} must be one of {}",
            ok.join(", ")
        )),
    };
    s("diff", &["cumulative", "per-commit"])?;
    s("contextDisplay", &["minimized", "hidden", "expanded"])?;
    match o.get("contextHops") {
        None => {}
        Some(x) if x.as_u64().is_some_and(|n| n <= 2) => {}
        Some(x) => return Err(format!("range options: contextHops {x} must be 0, 1 or 2")),
    }
    match o.get("contextLines") {
        None => {}
        Some(x) if x.as_u64().is_some_and(|n| n <= 1000) => {}
        Some(x) => {
            return Err(format!(
                "range options: contextLines {x} must be a whole number up to 1000"
            ));
        }
    }
    for k in ["ignoreWhitespace", "allowNonChain"] {
        if o.get(k).is_some_and(|x| !x.is_boolean()) {
            return Err(format!("range options: {k} must be a boolean"));
        }
    }
    Ok(())
}
