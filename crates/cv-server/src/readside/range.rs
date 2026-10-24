//! Range views (D16, DESIGN §4.4, §4.10 step 5, §7): the snapshot map of the range tip, showing
//! only diff-focused hunk windows and unchanged callers/callees as (minimized) context windows.
//!
//! A range view is an ordinary [`View`] registered under its own id, so bootstrap, tiles, outlines,
//! fragments and blob text work unchanged. What the typed headers can't carry (heat.inRange,
//! diffStat, flags.minimized) is patched into tile JSON, and WindowDiff lines join window batches.
//!
//! Placement: a hunk window sits at the x and width of the snapshot window that holds its lines,
//! at the y those lines have there, so hunks never overlap each other and land exactly where the
//! code is in the full view. Context windows keep their snapshot rect. Clusters keep their rects;
//! a cluster with nothing changed or connected reports `windows: 0` (drawn as a faint outline).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use axum::http::StatusCode;
use cv_contracts::readside::{DiffLine, RangeOptions, WindowDiff};
use cv_contracts::*;
use cv_lang::{SymbolFact, hash, silhouette, split_lines};
use cv_vcs::CommitInfo;
use cv_vcs::read::{self, FileChange, LineRun};
use serde_json::{Value, json};

use super::{api_err, fresh, warnings};
use crate::index::{self, BootstrapOut, FileRec, LinkRec, View, Win};
use crate::routes::{ApiErr, AppState};

/// Context windows beyond this many are dropped (furthest hop, lowest risk first).
const MAX_CONTEXT: usize = 250;
/// In-range blame (owners) is skipped for ranges longer than this.
const MAX_BLAME_COMMITS: usize = 300;

#[derive(Clone, Default)]
struct Extra {
    in_range: Option<(u32, u32)>,
    diff_stat: Option<(u32, u32)>,
    minimized: bool,
}

pub struct RangeView {
    pub id: String,
    pub view: Arc<View>,
    /// The ViewRef returned by POST /views (with sharedPlacement, base, commits, warnings).
    pub view_ref: Value,
    pub snapshot: String,
    pub diffs: HashMap<String, WindowDiff>,
    extras: HashMap<String, Extra>,
}

impl RangeView {
    /// A window header as this range shows it.
    pub fn header_json(&self, h: &WindowHeader) -> Value {
        let mut v = serde_json::to_value(h).unwrap_or_default();
        if let Some(x) = self.extras.get(&h.id) {
            if let Some((c, of)) = x.in_range {
                v["heat"]["inRange"] = json!({ "commits": c, "of": of });
            }
            if let Some((a, r)) = x.diff_stat {
                v["diffStat"] = json!({ "added": a, "removed": r });
            }
            if x.minimized {
                v["flags"]["minimized"] = json!(true);
            }
        }
        v
    }

    pub fn tile_json(&self, res: TileResponse) -> Value {
        let windows: Vec<Value> = res.windows.iter().map(|h| self.header_json(h)).collect();
        json!({ "layoutVersion": res.layout_version, "windows": windows, "edges": res.edges })
    }
}

/// The snapshot view of `head` (reused when one is indexed already, else built and registered).
pub fn snapshot_for(app: &AppState, head: &CommitInfo) -> Result<Arc<View>, ApiErr> {
    let find = || {
        app.views
            .read()
            .unwrap()
            .values()
            .find(|v| {
                let r = &v.bootstrap.base.view;
                r.spec["mode"] != "range" && r.resolved.head == head.commit
            })
            .cloned()
    };
    if let Some(v) = find() {
        return Ok(v);
    }
    let _guard = app.build.lock().unwrap();
    if let Some(v) = find() {
        return Ok(v);
    }
    let spec = json!({ "mode": "snapshot", "rev": head.commit });
    let repo = fresh(app)?;
    let v = crate::layout_api::build_snapshot(app, &repo, spec, head)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, "index-failed", e))?;
    let v = Arc::new(v);
    app.views
        .write()
        .unwrap()
        .insert(v.id().to_string(), v.clone());
    Ok(v)
}

/// `ViewSpec.options` of a range (model.ts RangeOptions): missing fields take the defaults, but a
/// field of the wrong type, an unknown field or a value outside the contract is a 400 "invalid"
/// (it must not silently become the default).
pub fn range_options(v: &Value) -> Result<RangeOptions, ApiErr> {
    let bad = |m: String| api_err(StatusCode::BAD_REQUEST, "invalid", m);
    let Some(obj) = (match v {
        Value::Null => return Ok(RangeOptions::default()),
        Value::Object(o) => Some(o),
        _ => None,
    }) else {
        return Err(bad(format!("range options {v}: expected an object")));
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
    if let Some(k) = obj.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        return Err(bad(format!(
            "range options: unknown field `{k}` (known: {})",
            KNOWN.join(", ")
        )));
    }
    let o: RangeOptions =
        serde_json::from_value(v.clone()).map_err(|e| bad(format!("range options: {e}")))?;
    if !matches!(o.diff.as_str(), "cumulative" | "per-commit") {
        return Err(bad(format!(
            "range options: diff {:?} must be \"cumulative\" or \"per-commit\"",
            o.diff
        )));
    }
    if o.context_hops > 2 {
        return Err(bad(format!(
            "range options: contextHops {} must be 0, 1 or 2",
            o.context_hops
        )));
    }
    if !matches!(
        o.context_display.as_str(),
        "minimized" | "hidden" | "expanded"
    ) {
        return Err(bad(format!(
            "range options: contextDisplay {:?} must be \"minimized\", \"hidden\" or \"expanded\"",
            o.context_display
        )));
    }
    if o.context_lines > MAX_CONTEXT_LINES {
        return Err(bad(format!(
            "range options: contextLines {} is over {MAX_CONTEXT_LINES}",
            o.context_lines
        )));
    }
    Ok(o)
}

/// The most context lines a range hunk asks for (http.ts RangeOptions.contextLines).
pub const MAX_CONTEXT_LINES: u32 = 1000;

/// Builds the range view for `spec` or returns the one already built for the same resolution.
pub fn get_or_build(app: &AppState, spec: &Value) -> Result<Arc<RangeView>, ApiErr> {
    crate::routes::check_layout(spec)?;
    let revset = spec["revset"].as_str().unwrap_or("trunk()..@").to_string();
    let mut opts = range_options(&spec["options"])?;
    let fileset = spec["fileset"].as_str().map(str::to_string);
    let filter = super::path_filter(fileset.as_deref())?;
    let repo = fresh(app)?;
    let env = repo.revset_env();
    let info = repo
        .analyze_range(&env, &revset, 5000)
        .map_err(|e| api_err(StatusCode::BAD_REQUEST, "revset-invalid", e.to_string()))?;
    if info.commits.is_empty() {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            "range-empty",
            format!("`{revset}` resolves to no changes"),
        ));
    }
    // D16, DESIGN §7: a range is a chain (one root, one head) unless the spec opts in
    // (`options.allowNonChain`, e.g. Compare), which is then shown per commit.
    if info.roots.len() > 1 || info.heads.len() > 1 {
        if spec["options"]["allowNonChain"].as_bool() != Some(true) {
            return Err(not_a_chain(&revset, &info));
        }
        opts.diff = "per-commit".into();
    }
    let members: Vec<String> = info.commits.iter().map(|c| c.commit.clone()).collect();
    let id = format!(
        "v_r{:011x}",
        hash(&format!("{spec}|{}|{}", members.join(","), repo.op_id())) & 0x7ff_ffff_ffff
    );
    if let Some(r) = app.read.range(&id) {
        return Ok(r);
    }
    let _guard = app.read.build.lock().unwrap();
    if let Some(r) = app.read.range(&id) {
        return Ok(r);
    }
    let head = info.head().cloned().expect("non-empty range has a head");
    let snap = snapshot_for(app, &head)?;
    let built = build(
        app,
        &repo,
        &snap,
        BuildIn {
            id: id.clone(),
            spec: spec.clone(),
            opts,
            filter,
            info: &info,
            warnings: serde_json::to_value(warnings(&info)).unwrap_or_default(),
        },
    )?;
    let r = Arc::new(built);
    app.views
        .write()
        .unwrap()
        .insert(id.clone(), r.view.clone());
    app.read.ranges.write().unwrap().insert(id, r.clone());
    Ok(r)
}

/// 400 `range-not-chain` naming the extra roots or heads, with a hint (ApiErr's "\nhint: ").
fn not_a_chain(revset: &str, info: &read::RangeInfo) -> ApiErr {
    let short = |v: &[CommitInfo]| {
        v.iter()
            .map(|c| c.change[..8.min(c.change.len())].to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut what = Vec::new();
    if info.roots.len() > 1 {
        what.push(format!(
            "{} roots ({})",
            info.roots.len(),
            short(&info.roots)
        ));
    }
    if info.heads.len() > 1 {
        what.push(format!(
            "{} heads ({})",
            info.heads.len(),
            short(&info.heads)
        ));
    }
    let head = info.head().map(|h| &h.change[..8.min(h.change.len())]);
    api_err(
        StatusCode::BAD_REQUEST,
        "range-not-chain",
        format!(
            "`{revset}` has {}: a range view needs a chain with one root and one head (D16)\nhint: review one stack, e.g. `trunk()..{}`, or set options.allowNonChain to see these commits per commit",
            what.join(" and "),
            head.unwrap_or("@"),
        ),
    )
}

/// After an operation: each cached range whose revset now resolves to other commits is marked
/// stale (`status: "stale"`) and gets `view.invalidated { reason: "op" }` with no window list
/// (everything); ranges that still resolve to the same commits get an empty one, as snapshots do.
pub fn invalidate_on_op(
    app: &AppState,
    repo: &cv_vcs::JjRepo,
) -> Vec<cv_contracts::review::ServerEvent> {
    let ranges: Vec<Arc<RangeView>> = app.read.ranges.read().unwrap().values().cloned().collect();
    let env = repo.revset_env();
    let mut out = Vec::new();
    for r in ranges {
        if r.view_ref["status"] == "stale" {
            continue;
        }
        let revset = r.view_ref["spec"]["revset"]
            .as_str()
            .unwrap_or("trunk()..@");
        let now: Option<Vec<String>> = repo
            .analyze_range(&env, revset, 5000)
            .ok()
            .map(|i| i.commits.into_iter().map(|c| c.commit).collect());
        let same = now.as_ref() == Some(&r.view.bootstrap.base.view.resolved.commits);
        if !same {
            let mut view_ref = r.view_ref.clone();
            view_ref["status"] = json!("stale");
            let mut v = (*r.view).clone();
            v.bootstrap.base.view.status = "stale".into();
            let stale = RangeView {
                id: r.id.clone(),
                view: Arc::new(v),
                view_ref,
                snapshot: r.snapshot.clone(),
                diffs: r.diffs.clone(),
                extras: r.extras.clone(),
            };
            let v = stale.view.clone();
            app.read
                .ranges
                .write()
                .unwrap()
                .insert(r.id.clone(), Arc::new(stale));
            app.views.write().unwrap().insert(r.id.clone(), v);
        }
        out.push(cv_contracts::review::ServerEvent::ViewInvalidated {
            view_id: r.id.clone(),
            layout_version: r.view.bootstrap.base.view.layout_version,
            reason: "op".into(),
            windows: same.then(Vec::new),
            tiles: None,
        });
    }
    out
}

impl RangeView {
    /// "n of m commits" of a range window (hunks), when known.
    pub fn in_range(&self, window: &str) -> Option<(u32, u32)> {
        self.extras.get(window).and_then(|x| x.in_range)
    }
}

struct BuildIn<'a> {
    id: String,
    spec: Value,
    opts: RangeOptions,
    filter: super::PathFilter,
    info: &'a read::RangeInfo,
    warnings: Value,
}

/// One file version whose diff becomes hunk windows: the cumulative diff of the range, or one
/// commit's diff in per-commit mode.
struct Unit {
    commit: CommitInfo,
    /// The commit's own diff (per-commit mode), not the range's cumulative diff.
    per_commit: bool,
    change: FileChange,
}

/// A hunk window under construction.
struct Hunk {
    snap_win: usize,
    file: usize,
    start: u32,
    end: u32,
    commit: CommitInfo,
    blob: String,
    symbol: Option<SymbolRef>,
    lines: Vec<DiffLine>,
    stat: (u32, u32),
    in_range: u32,
    stack: u32,
}

fn copy_file(f: &FileRec) -> FileRec {
    FileRec {
        path: f.path.clone(),
        blob: f.blob.clone(),
        lang: f.lang,
        lane: f.lane,
        text: f.text.clone(),
        line_starts: f.line_starts.clone(),
        tokens: f.tokens.clone(),
        silhouette: f.silhouette.clone(),
        headings: f.headings.clone(),
        windows: Vec::new(),
    }
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' && i + 1 < text.len() {
            v.push(i + 1);
        }
    }
    v
}

/// A file version not in the snapshot (per-commit mode): text, tokens, silhouette, headings.
fn analyzed_file(
    reg: &cv_lang::Registry,
    like: &FileRec,
    blob: &str,
    text: String,
) -> (FileRec, Vec<SymbolFact>) {
    let facts = reg
        .pack_for(&like.path)
        .map(|(_, p)| p.analyze(&like.path, &text));
    let starts = line_starts(&text);
    let lines = split_lines(&text);
    let mut tokens = facts.as_ref().map(|f| f.tokens.clone()).unwrap_or_default();
    tokens.resize(starts.len(), Vec::new());
    let sil = silhouette::records(&lines, &tokens);
    let symbols = facts.map(|f| f.symbols).unwrap_or_default();
    let headings = symbols
        .iter()
        .filter(|s| (s.name_line as usize) <= lines.len() && s.name_line >= 1)
        .map(|s| {
            let mut d = 0;
            let mut p = s.parent;
            while let Some(i) = p {
                d += 1;
                p = symbols[i].parent;
            }
            let label: String = lines[(s.name_line - 1) as usize]
                .trim()
                .chars()
                .take(120)
                .collect();
            (s.name_line, d, label)
        })
        .collect();
    (
        FileRec {
            path: like.path.clone(),
            blob: blob.to_string(),
            lang: like.lang,
            lane: like.lane,
            text,
            line_starts: starts,
            tokens,
            silhouette: sil,
            headings,
            windows: Vec::new(),
        },
        symbols,
    )
}

fn is_binary(b: &[u8]) -> bool {
    b[..b.len().min(8000)].contains(&0)
}

/// Clusters of changed lines in one snapshot window, merged when closer than 2 × context and
/// widened to the innermost enclosing symbol when it fits (≤ 500 lines), else to ± context.
pub fn hunk_spans(
    marks: &[u32],
    win: (u32, u32),
    symbols: &[(u32, u32)],
    context: u32,
) -> Vec<(u32, u32)> {
    let mut marks = marks.to_vec();
    marks.sort_unstable();
    marks.dedup();
    let mut clusters: Vec<(u32, u32)> = Vec::new();
    for m in marks {
        match clusters.last_mut() {
            Some(c) if m <= c.1 + 2 * context + 1 => c.1 = m,
            _ => clusters.push((m, m)),
        }
    }
    let mut spans: Vec<(u32, u32)> = clusters
        .into_iter()
        .map(|(a, b)| {
            let sym = symbols
                .iter()
                .filter(|(s, e)| *s <= a && b <= *e && e - s < WINDOW_MAX_LINES)
                .min_by_key(|(s, e)| e - s);
            let (s, e) = match sym {
                Some(&(s, e)) => (s, e),
                None => (a.saturating_sub(context).max(1), b + context),
            };
            (s.max(win.0), e.min(win.1))
        })
        .collect();
    spans.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for s in spans {
        match out.last_mut() {
            Some(o) if s.0 <= o.1 + 1 => o.1 = o.1.max(s.1),
            _ => out.push(s),
        }
    }
    out
}

/// Diff lines for new lines `start..=end` (1-based): ctx and add lines in order, deleted lines
/// (with their old text) where they were removed.
fn diff_lines(
    runs: &[LineRun],
    old_lines: &[&str],
    start: u32,
    end: u32,
    owner_of: &dyn Fn(u32) -> Option<String>,
    del_owner: Option<&str>,
) -> Vec<DiffLine> {
    let mut out = Vec::new();
    for r in runs {
        let first_new = r.new.start as u32 + 1; // line the run starts at (1-based)
        if r.same {
            for (k, n) in r.new.clone().enumerate() {
                let n = n as u32 + 1;
                if n >= start && n <= end {
                    out.push(DiffLine {
                        kind: "ctx".into(),
                        old_line: Some((r.old.start + k) as u32 + 1),
                        new_line: Some(n),
                        text: None,
                        owner: None,
                    });
                }
            }
            continue;
        }
        // Deletions sit before the run's first new line; at the file end, after the last.
        if first_new >= start && first_new <= end + 1 {
            for o in r.old.clone() {
                out.push(DiffLine {
                    kind: "del".into(),
                    old_line: Some(o as u32 + 1),
                    new_line: None,
                    text: Some(old_lines.get(o).copied().unwrap_or("").to_string()),
                    owner: del_owner.map(str::to_string),
                });
            }
        }
        for n in r.new.clone() {
            let n = n as u32 + 1;
            if n >= start && n <= end {
                out.push(DiffLine {
                    kind: "add".into(),
                    old_line: None,
                    new_line: Some(n),
                    text: None,
                    owner: owner_of(n),
                });
            }
        }
    }
    out
}

fn build(
    app: &AppState,
    repo: &cv_vcs::JjRepo,
    snap: &Arc<View>,
    inp: BuildIn<'_>,
) -> Result<RangeView, ApiErr> {
    let info = inp.info;
    let opts = &inp.opts;
    let per_commit = opts.diff == "per-commit";
    let head = info.head().cloned().expect("head");
    let root = info.roots.last().cloned().expect("root");
    let members: Vec<String> = info.commits.iter().map(|c| c.commit.clone()).collect();
    let n_members = members.len() as u32;
    let internal = |e: cv_vcs::Error| api_err(StatusCode::INTERNAL_SERVER_ERROR, "vcs", e.0);

    // Units of diff.
    let mut units: Vec<Unit> = Vec::new();
    if per_commit {
        for c in info.commits.iter().rev() {
            for ch in repo.commit_diff(&c.commit).map_err(internal)? {
                units.push(Unit {
                    commit: c.clone(),
                    per_commit: true,
                    change: ch,
                });
            }
        }
    } else {
        for ch in repo
            .range_diff(&root.commit, &head.commit)
            .map_err(internal)?
        {
            units.push(Unit {
                commit: head.clone(),
                per_commit: false,
                change: ch,
            });
        }
    }
    units.retain(|u| inp.filter.matches(&u.change.path));
    // Commits touching each path (for "n of m commits" when a hunk has no added lines).
    let per_commit_files: HashMap<String, u32> = if members.len() <= MAX_BLAME_COMMITS {
        let (_, per) = read::files_of(repo, &info.commits);
        let mut m: HashMap<String, u32> = HashMap::new();
        for files in per.values() {
            for f in files {
                *m.entry(f.clone()).or_default() += 1;
            }
        }
        m
    } else {
        HashMap::new()
    };

    let mut files: Vec<FileRec> = Vec::new();
    let mut file_ix: HashMap<(String, String), usize> = HashMap::new();
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut stacked: HashMap<usize, u32> = HashMap::new();
    let (mut added_total, mut removed_total) = (0u32, 0u32);
    for u in &units {
        let ch = &u.change;
        let Some(after) = &ch.after else {
            if let Some(b) = &ch.before
                && let Ok(old) = repo.read_blob(&ch.path, b)
                && !is_binary(&old)
            {
                removed_total += read::line_stat(&old, b"").1;
            }
            continue;
        };
        let Some(&sfi) = snap.by_path.get(&ch.path) else {
            continue; // not drawn in the snapshot (lockfiles, binaries, `other` lane)
        };
        let sf = &snap.files[sfi];
        let old = match &ch.before {
            Some(b) => repo.read_blob(&ch.path, b).map_err(internal)?,
            None => Vec::new(),
        };
        if is_binary(&old) {
            continue;
        }
        // The new side: the snapshot's text (cumulative), else this commit's version.
        let key = (ch.path.clone(), after.clone());
        let (fi, symbols) = if !u.per_commit && *after == sf.blob {
            let fi = *file_ix.entry(key).or_insert_with(|| {
                files.push(copy_file(sf));
                files.len() - 1
            });
            let syms = app
                .registry
                .pack_for(&sf.path)
                .map(|(_, p)| p.analyze(&sf.path, &sf.text).symbols)
                .unwrap_or_default();
            (fi, syms)
        } else {
            let bytes = repo.read_blob(&ch.path, after).map_err(internal)?;
            if is_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let (rec, syms) = analyzed_file(&app.registry, sf, after, text);
            let fi = match file_ix.get(&key) {
                Some(&fi) => fi,
                None => {
                    files.push(rec);
                    file_ix.insert(key, files.len() - 1);
                    files.len() - 1
                }
            };
            (fi, syms)
        };
        let new_text = files[fi].text.clone();
        let old_text = String::from_utf8_lossy(&old).into_owned();
        let old_lines = split_lines(&old_text);
        let runs = read::line_runs(
            old_text.as_bytes(),
            new_text.as_bytes(),
            opts.ignore_whitespace,
        );
        let n_lines = files[fi].line_count();
        // Owners: in-range blame (cumulative) or the commit itself (per-commit).
        let owners: Vec<Option<String>> = if u.per_commit {
            Vec::new()
        } else if members.len() <= MAX_BLAME_COMMITS {
            repo.annotate_in_range(&head.commit, &ch.path, &members)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let commit_change = u.commit.change.clone();
        let owner_of = |n: u32| -> Option<String> {
            if u.per_commit {
                Some(commit_change.clone())
            } else {
                owners.get(n as usize - 1).cloned().flatten()
            }
        };
        // Changed lines and deletion anchors, grouped by the snapshot window holding them.
        let mut marks: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
        let win_of = |line: u32| -> usize {
            let l = line.clamp(1, sf.line_count().max(1));
            sf.windows
                .iter()
                .copied()
                .find(|&w| {
                    let h = &snap.windows[w].header;
                    h.lines.start <= l && l <= h.lines.end
                })
                .unwrap_or_else(|| {
                    *sf.windows
                        .iter()
                        .min_by_key(|&&w| {
                            let h = &snap.windows[w].header;
                            h.lines.start.abs_diff(l).min(h.lines.end.abs_diff(l))
                        })
                        .expect("a drawn file has windows")
                })
        };
        for r in runs.iter().filter(|r| !r.same) {
            if r.new.is_empty() {
                let anchor = (r.new.start as u32 + 1).min(n_lines.max(1));
                marks.entry(win_of(anchor)).or_default().push(anchor);
            } else {
                for n in r.new.clone() {
                    let n = n as u32 + 1;
                    marks.entry(win_of(n)).or_default().push(n);
                }
            }
        }
        let sym_spans: Vec<(u32, u32)> = symbols.iter().map(|s| (s.start, s.end)).collect();
        for (sw, ms) in marks {
            let sh = &snap.windows[sw].header;
            // In per-commit mode the commit's file may be shorter or longer than the tip's.
            let win = if u.per_commit {
                (
                    sh.lines.start.min(n_lines.max(1)),
                    sh.lines
                        .end
                        .min(n_lines.max(1))
                        .max(sh.lines.start.min(n_lines.max(1))),
                )
            } else {
                (sh.lines.start, sh.lines.end)
            };
            for (s, e) in hunk_spans(&ms, win, &sym_spans, opts.context_lines) {
                let del_owner = u.per_commit.then_some(u.commit.change.as_str());
                let lines = diff_lines(&runs, &old_lines, s, e, &owner_of, del_owner);
                let stat = lines
                    .iter()
                    .fold((0, 0), |(a, d), l| match l.kind.as_str() {
                        "add" => (a + 1, d),
                        "del" => (a, d + 1),
                        _ => (a, d),
                    });
                if stat == (0, 0) {
                    continue;
                }
                added_total += stat.0;
                removed_total += stat.1;
                let owners_here: BTreeSet<&str> =
                    lines.iter().filter_map(|l| l.owner.as_deref()).collect();
                let in_range = if u.per_commit {
                    1
                } else if !owners_here.is_empty() {
                    owners_here.len() as u32
                } else {
                    per_commit_files.get(&ch.path).copied().unwrap_or(1)
                };
                // The innermost symbol holding the whole hunk, else the one it overlaps most.
                let overlap = |x: &&SymbolFact| x.end.min(e).saturating_sub(x.start.max(s));
                let symbol = symbols
                    .iter()
                    .filter(|x| x.start <= s && e <= x.end)
                    .min_by_key(|x| x.end - x.start)
                    .or_else(|| {
                        symbols
                            .iter()
                            .filter(|x| overlap(x) > 0)
                            .max_by_key(overlap)
                    })
                    .map(|x| SymbolRef {
                        name: x.name.clone(),
                        qualname: x.qualname.clone(),
                        kind: x.kind.as_str().into(),
                    })
                    .or_else(|| sh.symbol.clone());
                let stack = if u.per_commit {
                    let k = stacked.entry(sw).or_default();
                    *k += 1;
                    *k - 1
                } else {
                    0
                };
                hunks.push(Hunk {
                    snap_win: sw,
                    file: fi,
                    start: s,
                    end: e,
                    commit: u.commit.clone(),
                    blob: after.clone(),
                    symbol,
                    lines,
                    stat,
                    in_range: in_range.min(n_members),
                    stack,
                });
            }
        }
    }

    // Context: unchanged callers and callees within `contextHops` of a changed snapshot window.
    let changed: HashSet<usize> = hunks.iter().map(|h| h.snap_win).collect();
    let mut context: Vec<(usize, u32)> = Vec::new();
    if opts.context_hops > 0 && opts.context_display != "hidden" {
        let mut seen: HashSet<usize> = changed.clone();
        let mut queue: VecDeque<(usize, u32)> = changed.iter().map(|&w| (w, 0)).collect();
        while let Some((w, d)) = queue.pop_front() {
            if d >= opts.context_hops as u32 {
                continue;
            }
            let win = &snap.windows[w];
            for &l in win.out_links.iter().chain(win.in_links.iter()) {
                let link = &snap.links[l];
                for other in [link.from, link.to] {
                    if seen.insert(other) {
                        context.push((other, d + 1));
                        queue.push_back((other, d + 1));
                    }
                }
            }
        }
        context.sort_by(|a, b| {
            a.1.cmp(&b.1).then(
                snap.windows[b.0]
                    .header
                    .risk
                    .total_cmp(&snap.windows[a.0].header.risk),
            )
        });
        context.truncate(MAX_CONTEXT);
    }

    // Windows.
    let mut windows: Vec<Win> = Vec::new();
    let mut extras: HashMap<String, Extra> = HashMap::new();
    let mut diffs: HashMap<String, WindowDiff> = HashMap::new();
    let mut from_snap: HashMap<usize, Vec<usize>> = HashMap::new();
    let wid = |key: &str| {
        format!(
            "w{:012x}",
            hash(&format!("{}|{key}", inp.id)) & 0xffff_ffff_ffff
        )
    };
    let mut ids: HashSet<String> = HashSet::new();
    let mut unique = |mut id: String| {
        while !ids.insert(id.clone()) {
            id.push('_');
        }
        id
    };
    for h in hunks {
        let sh = &snap.windows[h.snap_win].header;
        let key = if per_commit {
            format!(
                "{}@{}~{}",
                sh.key,
                h.start,
                &h.commit.change[..8.min(h.commit.change.len())]
            )
        } else {
            format!("{}@{}", sh.key, h.start)
        };
        let id = unique(wid(&key));
        let offset = h.stack as f64 * HEADER_WU;
        let mut header = sh.clone();
        header.id = id.clone();
        header.key = key;
        header.kind = "hunk".into();
        header.rev = h.commit.commit.clone();
        header.change = h.commit.change.clone();
        header.blob = h.blob.clone();
        header.symbol = h.symbol;
        header.lines = LineSpan {
            start: h.start,
            end: h.end,
            file_lines: files[h.file].line_count(),
        };
        header.rect = Rect {
            x: sh.rect.x + offset,
            y: sh.rect.y + (h.start.saturating_sub(sh.lines.start)) as f64 * LINE_WU + offset,
            w: sh.rect.w,
            h: HEADER_WU + (h.end - h.start + 1) as f64 * LINE_WU,
        };
        header.fragment = None;
        header.flags = WindowFlags {
            pinned: false,
            truncated: false,
        };
        extras.insert(
            id.clone(),
            Extra {
                in_range: Some((h.in_range.max(1), n_members)),
                diff_stat: Some(h.stat),
                minimized: false,
            },
        );
        diffs.insert(
            id.clone(),
            WindowDiff {
                id: id.clone(),
                lines: h.lines,
            },
        );
        from_snap.entry(h.snap_win).or_default().push(windows.len());
        files[h.file].windows.push(windows.len());
        windows.push(Win {
            header,
            file: h.file,
            out_links: vec![],
            in_links: vec![],
        });
    }
    for (sw, _) in &context {
        let sw = *sw;
        let s = &snap.windows[sw];
        let sf = &snap.files[s.file];
        let key = (sf.path.clone(), sf.blob.clone());
        let fi = *file_ix.entry(key).or_insert_with(|| {
            files.push(copy_file(sf));
            files.len() - 1
        });
        let id = unique(wid(&s.header.key));
        let mut header = s.header.clone();
        header.id = id.clone();
        header.kind = "context".into();
        header.fragment = None;
        extras.insert(
            id,
            Extra {
                minimized: opts.context_display != "expanded",
                ..Default::default()
            },
        );
        from_snap.entry(sw).or_default().push(windows.len());
        files[fi].windows.push(windows.len());
        windows.push(Win {
            header,
            file: fi,
            out_links: vec![],
            in_links: vec![],
        });
    }

    // Links among the shown windows: each end maps to the range window showing that line.
    let find = |sw: usize, line: u32| -> Option<usize> {
        from_snap.get(&sw)?.iter().copied().find(|&w| {
            let l = &windows[w].header.lines;
            l.start <= line && line <= l.end
        })
    };
    let mut links: Vec<LinkRec> = Vec::new();
    for l in &snap.links {
        if !from_snap.contains_key(&l.from) || !from_snap.contains_key(&l.to) {
            continue;
        }
        if let (Some(a), Some(b)) = (find(l.from, l.from_line), find(l.to, l.to_line)) {
            links.push(LinkRec {
                id: l.id.clone(),
                kind: l.kind,
                from: a,
                from_line: l.from_line,
                to: b,
                to_line: l.to_line,
                confidence: l.confidence,
                label: l.label.clone(),
                channel: l.channel.clone(),
            });
        }
    }
    for (li, l) in links.iter().enumerate() {
        windows[l.from].out_links.push(li);
        if !l.is_dynamic() {
            windows[l.to].in_links.push(li);
        }
    }
    for w in windows.iter_mut() {
        let mut ports: Vec<(bool, u32)> = w
            .out_links
            .iter()
            .map(|&l| (true, links[l].from_line))
            .chain(w.in_links.iter().map(|&l| (false, links[l].to_line)))
            .collect();
        ports.sort_unstable();
        ports.dedup();
        w.header.counts.ports = ports.len() as u32;
    }
    for f in &files {
        if f.windows.len() > 1 {
            let siblings: Vec<String> = f
                .windows
                .iter()
                .map(|&w| windows[w].header.id.clone())
                .collect();
            for (i, &w) in f.windows.iter().enumerate() {
                windows[w].header.fragment = Some(FileFragment {
                    index: i as u32 + 1,
                    of: f.windows.len() as u32,
                    chip: cv_lang::chunk::chip_slot(&f.path),
                    siblings: siblings.clone(),
                });
            }
        }
    }

    // Bootstrap: the snapshot's map, with per-cluster counts of what the range shows.
    let mut base = snap.bootstrap.base.clone();
    let mut per_cluster: HashMap<String, u32> = HashMap::new();
    for w in &windows {
        *per_cluster.entry(w.header.cluster.clone()).or_default() += 1;
    }
    for c in base.clusters.iter_mut() {
        c.windows = per_cluster.get(&c.id).copied().unwrap_or(0);
    }
    base.bundles
        .retain(|b| per_cluster.contains_key(&b.from) && per_cluster.contains_key(&b.to));
    base.view.id = inp.id.clone();
    base.view.spec = inp.spec.clone();
    base.view.resolved = Resolved {
        op: repo.op_id(),
        head: head.commit.clone(),
        commits: members.clone(),
    };
    let shown_files: HashSet<&str> = windows.iter().map(|w| w.header.path.as_str()).collect();
    base.view.stats = ViewStats {
        windows: windows.len() as u32,
        files: shown_files.len() as u32,
        clusters: per_cluster.len() as u32,
    };
    base.view.warnings = inp.warnings.as_array().cloned().unwrap_or_default();
    let mut view_ref = serde_json::to_value(&base.view).unwrap_or_default();
    view_ref["sharedPlacement"] = json!(snap.id());
    view_ref["resolved"]["base"] = json!(root.parents.first());
    view_ref["stats"]["added"] = json!(added_total);
    view_ref["stats"]["removed"] = json!(removed_total);

    // Grid index over base tiles.
    let mut tiles: HashMap<(i64, i64), (Vec<usize>, Vec<usize>)> = HashMap::new();
    let cell = |v: f64| (v / TILE_BASE_WU).floor() as i64;
    for (i, w) in windows.iter().enumerate() {
        let r = &w.header.rect;
        for tx in cell(r.x)..=cell(r.x + r.w) {
            for ty in cell(r.y)..=cell(r.y + r.h) {
                tiles.entry((tx, ty)).or_default().0.push(i);
            }
        }
    }
    for (li, l) in links.iter().enumerate() {
        if l.is_dynamic() {
            continue;
        }
        let (a, b) = index::link_points(&windows, l);
        let (ca, cb) = ((cell(a[0]), cell(a[1])), (cell(b[0]), cell(b[1])));
        tiles.entry(ca).or_default().1.push(li);
        if ca != cb {
            tiles.entry(cb).or_default().1.push(li);
        }
    }
    let by_id = windows
        .iter()
        .enumerate()
        .map(|(i, w)| (w.header.id.clone(), i))
        .collect();
    let by_blob = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.blob.clone(), i))
        .collect();
    let mut by_path = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        // Per-commit mode has several versions of a path: fragments list the tip-most.
        by_path.insert(f.path.clone(), i);
    }
    let view = View {
        bootstrap: BootstrapOut {
            base,
            churn: snap.bootstrap.churn.clone(),
        },
        windows,
        files,
        links,
        by_id,
        by_blob,
        by_path,
        tiles,
        timings: vec![],
        retired: HashMap::new(),
        renames: Default::default(),
        expansions: Default::default(),
    };
    Ok(RangeView {
        id: inp.id,
        view: Arc::new(view),
        view_ref,
        snapshot: snap.id().to_string(),
        diffs,
        extras,
    })
}

/// A layout PATCH moved windows in snapshot `snapshot`: rebuild every cached range drawn on it, so
/// hunk windows sit where their symbols now sit (D16), one layoutVersion past the old range. Returns
/// (range id, new layoutVersion) for the invalidation events.
pub fn rebuild_on(app: &AppState, snapshot: &str) -> Vec<(String, u32)> {
    let stale: Vec<Arc<RangeView>> = app
        .read
        .ranges
        .read()
        .unwrap()
        .values()
        .filter(|r| r.snapshot == snapshot)
        .cloned()
        .collect();
    let mut out = Vec::new();
    for old in stale {
        app.read.ranges.write().unwrap().remove(&old.id);
        let spec = old.view_ref["spec"].clone();
        let Ok(new) = get_or_build(app, &spec) else {
            // Keep serving the old range rather than none.
            app.read.ranges.write().unwrap().insert(old.id.clone(), old);
            continue;
        };
        let version = old.view.bootstrap.base.view.layout_version + 1;
        let mut v = (*new.view).clone();
        v.bootstrap.base.view.layout_version = version;
        replace_view(app, &new.id, Arc::new(v));
        out.push((new.id.clone(), version));
    }
    out
}

/// A layout PATCH replaced the view behind range `id`: the range keeps its extras and diffs and
/// serves the new view (and its layoutVersion).
pub fn replace_view(app: &AppState, id: &str, v: Arc<View>) {
    let mut ranges = app.read.ranges.write().unwrap();
    let Some(old) = ranges.get(id).cloned() else {
        return;
    };
    let mut view_ref = old.view_ref.clone();
    view_ref["layoutVersion"] = json!(v.bootstrap.base.view.layout_version);
    let r = RangeView {
        id: old.id.clone(),
        view: v,
        view_ref,
        snapshot: old.snapshot.clone(),
        diffs: old.diffs.clone(),
        extras: old.extras.clone(),
    };
    ranges.insert(id.to_string(), Arc::new(r));
}

#[cfg(test)]
mod tests {
    use super::hunk_spans;

    #[test]
    fn hunks_merge_and_widen_to_symbols() {
        // Two marks 4 lines apart merge with context 3; a symbol 10..30 encloses them.
        assert_eq!(
            hunk_spans(&[12, 16], (1, 100), &[(10, 30)], 3),
            vec![(10, 30)]
        );
        // Without a symbol: ± context, clipped to the window.
        assert_eq!(hunk_spans(&[2], (1, 100), &[], 3), vec![(1, 5)]);
        // Far apart: two hunks.
        assert_eq!(
            hunk_spans(&[20, 60], (1, 100), &[], 3),
            vec![(17, 23), (57, 63)]
        );
        // A symbol larger than 500 lines doesn't count.
        assert_eq!(
            hunk_spans(&[300], (1, 900), &[(1, 800)], 3),
            vec![(297, 303)]
        );
    }
}
