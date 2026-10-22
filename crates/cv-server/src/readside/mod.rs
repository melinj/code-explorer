//! Read-side routes (F4 panels): history graph, revset validation, saved ranges, workspaces,
//! range views, Hotspots metrics with co-change, search and window expansion.
//!
//! Every route reads the repo at the current operation head through cv-vcs (reloaded per request,
//! never merging divergent heads) and never snapshots a working copy or writes an operation.

pub mod lanes;
pub mod metrics;
pub mod range;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cv_contracts::readside::*;
use cv_contracts::{LINE_WU, SiteRef, WindowBatchRequest, WindowBatchResponse};
use cv_vcs::JjRepo;
use cv_vcs::read::{self, RevsetEnv};
use serde::Deserialize;

use crate::index::View;
use crate::routes::{ApiErr, AppState};

pub use range::RangeView;

/// Review badges for the history panel, supplied by cv-review once review.db exists:
/// (change id, commit id) → overlay.
pub type ReviewHook = Arc<dyn Fn(&str, &str) -> Option<RevisionReview> + Send + Sync>;

/// Read-side state held in [`AppState`]: range views and cached history-derived data.
#[derive(Default)]
pub struct ReadSide {
    ranges: RwLock<HashMap<String, Arc<RangeView>>>,
    /// Co-change per view and head (cluster ids belong to the view's layout).
    cochange: Mutex<HashMap<String, Arc<metrics::CoChange>>>,
    review_hook: RwLock<Option<ReviewHook>>,
    /// Serializes range builds so two requests for one spec don't build it twice.
    build: Mutex<()>,
    workspaces: Mutex<Option<(Instant, Vec<WorkspaceSummary>)>>,
    symbols: Mutex<HashMap<String, Arc<Vec<SymbolEntry>>>>,
}

/// One declaration of a view, for search (windows can hold several, e.g. chunks).
pub struct SymbolEntry {
    pub name: String,
    pub qualname: String,
    pub kind: String,
    pub line: u32,
    pub window: usize,
}

/// Every declaration in the view's files, mapped to the window showing its name line.
pub fn symbol_index(app: &AppState, v: &View) -> Vec<SymbolEntry> {
    use rayon::prelude::*;
    v.files
        .par_iter()
        .flat_map_iter(|f| {
            let syms = app
                .registry
                .pack_for(&f.path)
                .map(|(_, p)| p.analyze(&f.path, &f.text).symbols)
                .unwrap_or_default();
            syms.into_iter().filter_map(move |s| {
                let w = f.windows.iter().copied().find(|&w| {
                    let l = &v.windows[w].header.lines;
                    l.start <= s.name_line && s.name_line <= l.end
                })?;
                Some(SymbolEntry {
                    name: s.name,
                    qualname: s.qualname,
                    kind: s.kind.as_str().to_string(),
                    line: s.name_line,
                    window: w,
                })
            })
        })
        .collect()
}

/// [`symbol_index`] of a view, cached by view and revision (shared with search).
pub fn symbols_of(app: &AppState, v: &View) -> Arc<Vec<SymbolEntry>> {
    let key = format!("{}|{}", v.id(), v.bootstrap.base.view.resolved.head);
    if let Some(s) = app.read.symbols.lock().unwrap().get(&key) {
        return s.clone();
    }
    let s = Arc::new(symbol_index(app, v));
    app.read.symbols.lock().unwrap().insert(key, s.clone());
    s
}

impl ReadSide {
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs the review overlay for history rows (cv-review calls this when it starts).
    pub fn set_review_hook(&self, hook: ReviewHook) {
        *self.review_hook.write().unwrap() = Some(hook);
    }

    pub fn range(&self, id: &str) -> Option<Arc<RangeView>> {
        self.ranges.read().unwrap().get(id).cloned()
    }

    /// Every cached range view.
    pub fn ranges(&self) -> Vec<Arc<RangeView>> {
        self.ranges.read().unwrap().values().cloned().collect()
    }

    /// Range views: tile headers with the range fields (kind, heat.inRange, diffStat, minimized).
    pub fn patch_tile(&self, view: &str, res: cv_contracts::TileResponse) -> serde_json::Value {
        match self.range(view) {
            Some(r) => r.tile_json(res),
            None => serde_json::to_value(res).unwrap_or_default(),
        }
    }

    /// Range views: WindowDiff lines for the requested hunk windows when `parts` has "diff".
    pub fn add_diffs(&self, view: &str, req: &WindowBatchRequest, res: &mut WindowBatchResponse) {
        if !req.parts.iter().any(|p| p == "diff") {
            return;
        }
        if let Some(r) = self.range(view) {
            for id in &req.ids {
                if let Some(d) = r.diffs.get(id) {
                    res.diffs.push(serde_json::to_value(d).unwrap_or_default());
                }
            }
        }
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/revisions", get(revisions))
        .route("/api/v1/revsets:validate", post(validate))
        .route("/api/v1/workspaces", get(workspaces))
        .route("/api/v1/workspaces:compare", post(compare))
        .route("/api/v1/views/{view}/metrics", get(metrics_route))
        .route("/api/v1/views/{view}/windows/{window}/expand", post(expand))
        .route("/api/v1/search", get(search))
}

type St = State<Arc<AppState>>;

pub(crate) fn api_err(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
) -> ApiErr {
    ApiErr {
        status,
        code,
        message: message.into(),
    }
}

fn internal(e: impl std::fmt::Display) -> ApiErr {
    api_err(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
}

/// The repo at the current operation head (other agents keep committing while we serve).
pub(crate) fn fresh(app: &AppState) -> Result<JjRepo, ApiErr> {
    app.head_repo()
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiErr> + Send + 'static,
) -> Result<T, ApiErr> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

/// ISO-8601 (ids.ts: "Times are ISO-8601 strings with offset").
fn iso(ms: i64) -> String {
    cv_store::iso_from_ms(ms)
}

// ─── History ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RevisionsQ {
    revset: Option<String>,
    limit: Option<usize>,
}

/// GraphRows with server-computed lanes. Default revset: the user's `revsets.log` plus every
/// workspace's `@`.
pub fn graph_rows(
    repo: &JjRepo,
    env: &RevsetEnv,
    revset: &str,
    limit: usize,
    hook: Option<&ReviewHook>,
) -> Result<RevisionsResponse, read::RevsetError> {
    let (rows, truncated) = repo.graph(env, revset, limit)?;
    let lane_in: Vec<(String, Vec<String>)> = rows
        .iter()
        .map(|r| {
            (
                r.info.commit.clone(),
                r.edges.iter().map(|e| e.parent.clone()).collect(),
            )
        })
        .collect();
    let lanes = lanes::assign(&lane_in);
    let rows = rows
        .into_iter()
        .zip(lanes)
        .map(|(r, (lane, to))| {
            let review = hook.and_then(|h| h(&r.info.change, &r.info.commit));
            GraphRow {
                lane,
                edges: r
                    .edges
                    .iter()
                    .zip(to)
                    .map(|(e, to_lane)| GraphEdge {
                        to_lane,
                        parent: e.parent.clone(),
                        elided: (e.elided > 0).then_some(e.elided),
                    })
                    .collect(),
                rev: Revision {
                    change: r.info.change.clone(),
                    commit: r.info.commit.clone(),
                    parents: r.info.parents.clone(),
                    description: r.info.description.clone(),
                    author: Signature {
                        name: r.author.0.clone(),
                        email: r.author.1.clone(),
                        time: iso(r.author.2 * 1000),
                    },
                    committer_time: iso(r.info.committer_time_ms),
                    bookmarks: r.bookmarks.clone(),
                    tags: r.tags.clone(),
                    workspaces: r.workspaces.clone(),
                    flags: RevisionFlags {
                        conflict: r.conflict,
                        empty: r.empty,
                        immutable: r.immutable,
                        divergent: r.divergent,
                        hidden: r.hidden,
                        working_copy: r.working_copy,
                    },
                    shortest: ShortestPrefix {
                        change: r.shortest_change as u32,
                        commit: r.shortest_commit as u32,
                    },
                },
                review,
            }
        })
        .collect();
    Ok(RevisionsResponse { rows, truncated })
}

async fn revisions(State(app): St, Query(q): Query<RevisionsQ>) -> Result<Response, ApiErr> {
    Ok(Json(revisions_of(app, q.revset, q.limit).await?).into_response())
}

/// GET /revisions (also the in-process MCP backend).
pub async fn revisions_of(
    app: Arc<AppState>,
    revset: Option<String>,
    limit: Option<usize>,
) -> Result<RevisionsResponse, ApiErr> {
    let q = RevisionsQ { revset, limit };
    blocking(move || {
        let repo = fresh(&app)?;
        let env = repo.revset_env();
        let revset = q
            .revset
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| format!("({}) | working_copies()", env.log));
        let hook = app.read.review_hook.read().unwrap().clone();
        graph_rows(
            &repo,
            &env,
            &revset,
            q.limit.unwrap_or(200).clamp(1, 5000),
            hook.as_ref(),
        )
        .map_err(|e| api_err(StatusCode::BAD_REQUEST, "revset-invalid", e.to_string()))
    })
    .await
}

// ─── Revset validation ──────────────────────────────────────────────────────

/// A range's fileset filter: jj fileset syntax, parsed by jj-lib (cv_vcs::Fileset).
pub type PathFilter = cv_vcs::Fileset;

/// Parses a spec's fileset; a bad one is a 400 `fileset-invalid`.
pub fn path_filter(fileset: Option<&str>) -> Result<PathFilter, ApiErr> {
    cv_vcs::Fileset::parse(fileset)
        .map_err(|e| api_err(StatusCode::BAD_REQUEST, "fileset-invalid", e.to_string()))
}

fn warnings(info: &read::RangeInfo) -> Vec<RangeWarning> {
    let ids = |v: &[cv_vcs::CommitInfo]| v.iter().map(|c| c.change.clone()).collect::<Vec<_>>();
    let mut w = Vec::new();
    if !info.gap.is_empty() {
        w.push(RangeWarning::Gap {
            missing: ids(&info.gap),
        });
    }
    if info.roots.len() > 1 {
        w.push(RangeWarning::MultipleRoots {
            changes: ids(&info.roots),
        });
    }
    if info.heads.len() > 1 {
        w.push(RangeWarning::MultipleHeads {
            changes: ids(&info.heads),
        });
    }
    if !info.immutable.is_empty() {
        w.push(RangeWarning::IncludesImmutable {
            changes: ids(&info.immutable),
        });
    }
    if !info.conflicted.is_empty() {
        w.push(RangeWarning::Conflicted {
            changes: ids(&info.conflicted),
        });
    }
    w
}

/// Files and line counts of a range: the cumulative diff for a chain (one root, one head), else
/// the union of per-commit diffs with summed line counts.
fn range_stats(repo: &JjRepo, info: &read::RangeInfo, filter: &PathFilter) -> (u32, u32, u32) {
    const MAX_LINE_FILES: usize = 3000;
    let mut changes: Vec<read::FileChange> = Vec::new();
    let mut paths = BTreeSet::new();
    if info.roots.len() == 1 && info.heads.len() == 1 && info.gap.is_empty() {
        changes = repo
            .range_diff(&info.roots[0].commit, &info.heads[0].commit)
            .unwrap_or_default();
    } else {
        for c in &info.commits {
            changes.extend(repo.commit_diff(&c.commit).unwrap_or_default());
        }
    }
    changes.retain(|f| filter.matches(&f.path));
    let (mut added, mut removed) = (0, 0);
    for f in changes.iter().take(MAX_LINE_FILES) {
        let read = |b: &Option<String>| {
            b.as_ref()
                .and_then(|b| repo.read_blob(&f.path, b).ok())
                .unwrap_or_default()
        };
        let (old, new) = (read(&f.before), read(&f.after));
        if old[..old.len().min(8000)].contains(&0) || new[..new.len().min(8000)].contains(&0) {
            continue; // binary
        }
        let (a, d) = read::line_stat(&old, &new);
        added += a;
        removed += d;
    }
    paths.extend(changes.iter().map(|f| f.path.clone()));
    (paths.len() as u32, added, removed)
}

pub fn validate_revset(
    repo: &JjRepo,
    env: &RevsetEnv,
    revset: &str,
    fileset: Option<&str>,
) -> RevsetValidation {
    let info = match repo.analyze_range(env, revset, 5000) {
        Ok(i) => i,
        Err(e) => {
            return RevsetValidation::Err(RevsetError {
                ok: false,
                error: e.message,
                hint: e.hint,
            });
        }
    };
    if info.commits.is_empty() {
        return RevsetValidation::Err(RevsetError {
            ok: false,
            error: format!("`{revset}` resolves to no changes"),
            hint: Some("try trunk()..@".into()),
        });
    }
    let filter = match PathFilter::parse(fileset) {
        Ok(f) => f,
        Err(e) => {
            return RevsetValidation::Err(RevsetError {
                ok: false,
                error: e.to_string(),
                hint: Some("a jj fileset, e.g. src/ | \"a b.py\" | glob:\"**/*.py\"".into()),
            });
        }
    };
    let (files, added, removed) = range_stats(repo, &info, &filter);
    RevsetValidation::Ok(RevsetSummary {
        ok: true,
        count: info.commits.len() as u32,
        changes: info
            .commits
            .iter()
            .take(50)
            .map(|c| c.change.clone())
            .collect(),
        files,
        added,
        removed,
        conflicts: info.conflicted.len() as u32,
        warnings: warnings(&info),
    })
}

#[derive(Deserialize)]
struct ValidateReq {
    revset: String,
    fileset: Option<String>,
}

async fn validate(State(app): St, Json(req): Json<ValidateReq>) -> Result<Response, ApiErr> {
    let res = blocking(move || {
        let repo = fresh(&app)?;
        let env = repo.revset_env();
        Ok(validate_revset(
            &repo,
            &env,
            &req.revset,
            req.fileset.as_deref(),
        ))
    })
    .await?;
    Ok(Json(res).into_response())
}

// ─── Workspaces ─────────────────────────────────────────────────────────────

/// Workspace cards without agents (stacks, unsnapshotted files, overlaps, ISO last op); see
/// [`crate::workspaces::summaries`].
pub fn workspace_summaries(repo: &JjRepo, env: &RevsetEnv, serving: &str) -> Vec<WorkspaceSummary> {
    let states = repo.workspaces(env);
    let ops = crate::workspaces::Stacks::compute(repo, env, &states);
    crate::workspaces::summaries(&states, &ops, serving)
}

/// The workspace poller's fresh cards serve GET /workspaces until they age out.
pub(crate) fn remember_cards(app: &AppState, cards: &serde_json::Value) {
    if let Ok(list) = serde_json::from_value::<Vec<WorkspaceSummary>>(cards.clone()) {
        *app.read.workspaces.lock().unwrap() = Some((Instant::now(), list));
    }
}

async fn workspaces(State(app): St) -> Result<Response, ApiErr> {
    Ok(Json(workspace_cards(&app).await?).into_response())
}

/// GET /workspaces: summaries with the bound agents (also the in-process MCP backend).
pub async fn workspace_cards(app: &Arc<AppState>) -> Result<serde_json::Value, ApiErr> {
    // Unsnapshotted counts stat every workspace's files: reuse a result for a couple of seconds.
    const TTL: Duration = Duration::from_secs(2);
    if let Some((t, v)) = app.read.workspaces.lock().unwrap().as_ref()
        && t.elapsed() < TTL
    {
        let mut cards = serde_json::to_value(v).unwrap_or_default();
        crate::agent_api::fill_agents(app, &mut cards);
        return Ok(cards);
    }
    let app2 = app.clone();
    let cards = blocking(move || {
        let repo = fresh(&app2)?;
        Ok(crate::workspaces::cards_now(&app2, &repo).1)
    })
    .await?;
    remember_cards(app, &cards);
    Ok(cards)
}

// ─── Compare (D12, DESIGN §7 "Compare") ─────────────────────────────────────

#[derive(Deserialize)]
pub struct CompareReq {
    pub a: String,
    pub b: String,
}

/// POST /workspaces:compare { a, b } → ViewRef: a per-commit range view of both stacks
/// (`(trunk()..a@) | (trunk()..b@)`, `options.allowNonChain`), filtered to the files both change.
/// 404 for an unknown workspace, 400 "no-overlap" when the stacks share no file.
pub fn compare_view(app: &AppState, req: &CompareReq) -> Result<serde_json::Value, ApiErr> {
    let repo = fresh(app)?;
    let env = repo.revset_env();
    for w in [&req.a, &req.b] {
        if repo.resolve_one(&read::workspace_wc_revset(w)).is_err() {
            return Err(api_err(
                StatusCode::NOT_FOUND,
                "not-found",
                format!("no workspace {w:?}; GET /workspaces lists them"),
            ));
        }
    }
    if req.a == req.b {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            "invalid",
            "compare two different workspaces",
        ));
    }
    // Each stack's own commits (not the history both share: with no remote, trunk() is root()),
    // as the overlap warnings count them (crate::workspaces::Stack::overlap).
    let stack = |w: &str, other: &str| {
        let revset = format!(
            "(trunk()..{}) ~ ::{}",
            read::workspace_wc_revset(w),
            read::workspace_wc_revset(other)
        );
        let commits = repo.eval(&env, &revset, 500).unwrap_or_default();
        (revset, read::files_of(&repo, &commits).0)
    };
    let (ra, fa) = stack(&req.a, &req.b);
    let (rb, fb) = stack(&req.b, &req.a);
    let shared: Vec<String> = fa.intersection(&fb).cloned().collect();
    if shared.is_empty() {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            "no-overlap",
            format!(
                "the stacks of {} and {} change no file in common",
                req.a, req.b
            ),
        ));
    }
    let fileset = shared
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(" | ");
    let spec = serde_json::json!({
        "mode": "range",
        "revset": format!("({ra}) | ({rb})"),
        "fileset": fileset,
        "options": {
            "diff": "per-commit",
            "contextHops": 0,
            "contextDisplay": "minimized",
            "contextLines": 3,
            "ignoreWhitespace": false,
            "allowNonChain": true,
        },
    });
    let r = range::get_or_build(app, &spec)?;
    Ok(r.view_ref.clone())
}

async fn compare(State(app): St, Json(req): Json<CompareReq>) -> Result<Response, ApiErr> {
    Ok(Json(blocking(move || compare_view(&app, &req)).await?).into_response())
}

// ─── Metrics ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MetricsQ {
    level: Option<String>,
    lane: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct MetricsRouteQ {
    level: Option<String>,
    lane: Option<String>,
    limit: Option<usize>,
    /// "risk" (default) | "inRange" (range views: by in-range commits, D16).
    rank: Option<String>,
    /// "links" (default): file points take their windows' typed-link connectedness (§4.8).
    /// "imports": file points use the file import graph alone, appendix C's reference definition.
    basis: Option<String>,
}

async fn metrics_route(
    State(app): St,
    Path(view): Path<String>,
    Query(q): Query<MetricsRouteQ>,
) -> Result<Response, ApiErr> {
    let Some(r) = app.read.range(&view) else {
        if q.basis.as_deref() == Some("imports") {
            return Ok(
                Json(file_import_metrics(&app, &view, q.lane, q.limit).await?).into_response(),
            );
        }
        return Ok(Json(metrics_of(&app, &view, q.level, q.lane, q.limit).await?).into_response());
    };
    // Range views: every point carries heat.inRange; rank=inRange orders by it before the limit.
    let by_range = q.rank.as_deref() == Some("inRange");
    let limit = q.limit.unwrap_or(2000);
    let res = metrics_of(
        &app,
        &view,
        q.level.clone(),
        q.lane,
        Some(if by_range { 100_000 } else { limit }),
    )
    .await?;
    let mut out = serde_json::to_value(&res).map_err(internal)?;
    in_range_points(&r, q.level.as_deref() == Some("file"), &mut out["points"]);
    if by_range && let Some(points) = out["points"].as_array_mut() {
        let key = |p: &serde_json::Value| {
            (
                p["heat"]["inRange"]["commits"].as_u64().unwrap_or(0),
                p["risk"].as_f64().unwrap_or(0.0),
            )
        };
        points.sort_by(|a, b| {
            let (ka, kb) = (key(a), key(b));
            kb.0.cmp(&ka.0).then(kb.1.total_cmp(&ka.1))
        });
        points.truncate(limit);
    }
    Ok(Json(out).into_response())
}

/// `heat.inRange` of each Hotspots point of a range view: how many range commits touch its hunk
/// windows (distinct in-range owners of their lines, else the windows' own counts).
fn in_range_points(r: &RangeView, file_level: bool, points: &mut serde_json::Value) {
    let v = &r.view;
    let of = v.bootstrap.base.view.resolved.commits.len() as u32;
    let Some(points) = points.as_array_mut() else {
        return;
    };
    for p in points {
        let key = p["key"].as_str().unwrap_or("").to_string();
        let label = p["label"].as_str().unwrap_or("").to_string();
        let mut owners: BTreeSet<String> = BTreeSet::new();
        let mut most = 0u32;
        for w in &v.windows {
            let h = &w.header;
            let member = if file_level {
                h.path == label
            } else {
                h.cluster == key
            };
            if !member {
                continue;
            }
            if let Some((c, _)) = r.in_range(&h.id) {
                most = most.max(c);
            }
            if let Some(d) = r.diffs.get(&h.id) {
                owners.extend(d.lines.iter().filter_map(|l| l.owner.clone()));
            }
        }
        let commits = (owners.len() as u32).max(most).min(of);
        p["heat"]["inRange"] = serde_json::json!({ "commits": commits, "of": of });
    }
}

/// GET /views/:view/metrics?level=file&basis=imports: file points whose connectedness comes from
/// the file import graph alone (appendix C). No co-change pairs.
async fn file_import_metrics(
    app: &Arc<AppState>,
    view: &str,
    lane: Option<String>,
    limit: Option<usize>,
) -> Result<MetricsResponse, ApiErr> {
    let v = app.view(view)?;
    let app2 = app.clone();
    let points = blocking(move || {
        let repo = fresh(&app2)?;
        let reg = &app2.registry;
        let facts = crate::incremental::view_facts(&v, reg, repo.root());
        let head = v.bootstrap.base.view.resolved.head.clone();
        let cuts = crate::grades::grade_config(&repo, &head).conn;
        let fc = metrics::file_import_conn(&v, &facts, cuts);
        Ok(metrics::points_with(
            &v,
            "file",
            lane.as_deref(),
            limit.unwrap_or(2000),
            Some(&fc),
        ))
    })
    .await?;
    Ok(MetricsResponse {
        points,
        cochange: Vec::new(),
        cochange_clusters: None,
    })
}

/// GET /views/:view/metrics (also the in-process MCP backend).
pub async fn metrics_of(
    app: &Arc<AppState>,
    view: &str,
    level: Option<String>,
    lane: Option<String>,
    limit: Option<usize>,
) -> Result<MetricsResponse, ApiErr> {
    let q = MetricsQ { level, lane, limit };
    let v = app.view(view)?;
    let head = v.bootstrap.base.view.resolved.head.clone();
    let key = format!("{view}@{head}");
    let cached = app.read.cochange.lock().unwrap().get(&key).cloned();
    let cochange = match cached {
        Some(c) => c,
        None => {
            let (app2, v2, head2) = (app.clone(), v.clone(), head.clone());
            let c = blocking(move || {
                let repo = fresh(&app2)?;
                Ok(Arc::new(metrics::cochange(
                    &repo,
                    &v2,
                    &head2,
                    metrics::CoChangeLimits::default(),
                )))
            })
            .await?;
            app.read.cochange.lock().unwrap().insert(key, c.clone());
            c
        }
    };
    let points = metrics::points(
        &v,
        q.level.as_deref().unwrap_or("cluster"),
        q.lane.as_deref(),
        q.limit.unwrap_or(2000),
    );
    // Co-change pairs limited to files the view shows (range views show fewer).
    let files: Vec<CoChangePair> = cochange
        .files
        .iter()
        .filter(|p| v.by_path.contains_key(&p.a) && v.by_path.contains_key(&p.b))
        .cloned()
        .collect();
    Ok(MetricsResponse {
        points,
        cochange: files,
        cochange_clusters: Some(cochange.clusters.clone()),
    })
}

// ─── Search ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchQ {
    pub q: String,
    pub kinds: Option<String>,
    #[serde(rename = "viewId")]
    pub view_id: Option<String>,
    pub limit: Option<usize>,
}

/// Score of a symbol for query `q` (`ql` = trimmed, lowercased): an exact name first, then a
/// case-insensitive name, the qualname, a name prefix, a qualname substring. Shared by the
/// in-memory and the FTS search, which must rank the same.
pub fn symbol_score(name: &str, qualname: &str, q: &str, ql: &str) -> Option<f64> {
    let (n, qual) = (name.to_lowercase(), qualname.to_lowercase());
    Some(if name == q.trim() {
        1.0
    } else if n == ql {
        0.95
    } else if qual == ql {
        0.9
    } else if n.starts_with(ql) {
        0.8
    } else if qual.contains(ql) {
        0.6
    } else {
        return None;
    })
}

/// Score of a file path for `ql`: its name or stem, a name prefix, a path substring.
pub fn file_score(path: &str, ql: &str) -> Option<f64> {
    let p = path.to_lowercase();
    let base = p.rsplit('/').next().unwrap_or(&p);
    let stem = base.split('.').next().unwrap_or(base);
    Some(if base == ql || stem == ql {
        0.7
    } else if base.starts_with(ql) {
        0.5
    } else if p.contains(ql) {
        0.4
    } else {
        return None;
    })
}

/// A symbol hit (`window` indexes `v.windows`); the sort key adds a little risk.
fn symbol_hit(v: &View, window: usize, qualname: &str, line: u32, score: f64) -> (f64, SearchHit) {
    let h = &v.windows[window].header;
    (
        score + h.risk * 0.05,
        SearchHit {
            kind: "symbol".into(),
            label: qualname.to_string(),
            // "path:line": the client flies there by parsing it.
            detail: Some(format!("{}:{}", h.path, line)),
            target: SearchTarget {
                window: Some(h.id.clone()),
                site: Some(SiteRef {
                    path: h.path.clone(),
                    line,
                    window: Some(h.id.clone()),
                    col: None,
                    symbol: None,
                }),
                ..Default::default()
            },
            score,
        },
    )
}

/// A file hit (`file` indexes `v.files`); `None` for a file without windows.
fn file_hit(v: &View, file: usize, score: f64) -> Option<(f64, SearchHit)> {
    let f = &v.files[file];
    let &w0 = f.windows.first()?;
    Some((
        score,
        SearchHit {
            kind: "file".into(),
            label: f.path.clone(),
            detail: Some(format!("{}:1", f.path)),
            target: SearchTarget {
                window: Some(v.windows[w0].header.id.clone()),
                site: Some(SiteRef {
                    path: f.path.clone(),
                    line: 1,
                    window: None,
                    col: None,
                    symbol: None,
                }),
                ..Default::default()
            },
            score,
        },
    ))
}

/// Hub hits (always from memory: channel hubs follow an op in the background).
fn hub_hits(v: &View, ql: &str, hits: &mut Vec<(f64, SearchHit)>) {
    for h in &v.bootstrap.base.hubs {
        let key = h.key.to_lowercase();
        if key.contains(ql) || h.display.to_lowercase().contains(ql) {
            // Hubs drawn on the canvas rank with symbols; the thousands of config hubs that
            // have no rect rank below them and navigate to their first site.
            let score = match (h.rect.is_some(), key == ql) {
                (true, true) => 0.9,
                (true, false) => 0.7,
                (false, true) => 0.55,
                (false, false) => 0.45,
            };
            hits.push((
                score,
                SearchHit {
                    kind: "hub".into(),
                    label: h.display.clone(),
                    detail: Some(h.protocol.clone()),
                    target: SearchTarget {
                        hub: Some(h.id.clone()),
                        site: h.producers.first().or(h.consumers.first()).cloned(),
                        ..Default::default()
                    },
                    score,
                },
            ));
        }
    }
}

fn ranked(mut hits: Vec<(f64, SearchHit)>, limit: usize) -> Vec<SearchHit> {
    // Stable: equal scores keep symbol, file, hub order.
    hits.sort_by(|a, b| b.0.total_cmp(&a.0));
    hits.into_iter().take(limit).map(|(_, h)| h).collect()
}

/// In-memory search over one view: an exact symbol name ranks first, then qualname matches,
/// prefixes, substrings, then paths; ties break on risk.
pub fn search_view(
    v: &View,
    symbols: &[SymbolEntry],
    q: &str,
    kinds: &[&str],
    limit: usize,
) -> Vec<SearchHit> {
    let ql = q.trim().to_lowercase();
    if ql.is_empty() {
        return vec![];
    }
    let want = |k: &str| kinds.is_empty() || kinds.contains(&k);
    let mut hits: Vec<(f64, SearchHit)> = Vec::new();
    if want("symbol") {
        for s in symbols {
            if let Some(score) = symbol_score(&s.name, &s.qualname, q, &ql) {
                hits.push(symbol_hit(v, s.window, &s.qualname, s.line, score));
            }
        }
    }
    if want("file") {
        for (i, f) in v.files.iter().enumerate() {
            if let Some(hit) = file_score(&f.path, &ql).and_then(|score| file_hit(v, i, score)) {
                hits.push(hit);
            }
        }
    }
    if want("hub") {
        hub_hits(v, &ql, &mut hits);
    }
    // Threads come from review.db (cv-review); none until it exists.
    ranked(hits, limit)
}

/// [`search_view`] over the view's FTS rows in index.db (B2): the trigram index narrows symbols
/// and files to those containing the query, then they are ranked exactly as in memory. `None`
/// when index.db holds no rows for this view at this revision.
pub fn search_view_fts(
    v: &View,
    db: &cv_store::IndexDb,
    q: &str,
    kinds: &[&str],
    limit: usize,
) -> Option<Vec<SearchHit>> {
    let ql = q.trim().to_lowercase();
    if ql.is_empty() {
        return Some(vec![]);
    }
    let want = |k: &str| kinds.is_empty() || kinds.contains(&k);
    let mut hits: Vec<(f64, SearchHit)> = Vec::new();
    if want("symbol") || want("file") {
        let scope = crate::view_store::search_scope(v);
        if !db.has_search(&scope).ok()? {
            return None;
        }
        let rows = db.search(&scope, &ql).ok()?;
        // Symbols first, then files: the in-memory order.
        for r in rows.iter().filter(|r| r.kind == "symbol" && want("symbol")) {
            let mut it = r.reference.split('\t');
            let (Some(w), Some(line)) = (it.next(), it.next().and_then(|l| l.parse().ok())) else {
                continue;
            };
            let Some(&wi) = v.by_id.get(w) else { continue };
            if let Some(score) = symbol_score(&r.detail, &r.label, q, &ql) {
                hits.push(symbol_hit(v, wi, &r.label, line, score));
            }
        }
        for r in rows.iter().filter(|r| r.kind == "file" && want("file")) {
            let Some(&fi) = v.by_path.get(&r.label) else {
                continue;
            };
            if let Some(hit) = file_score(&r.label, &ql).and_then(|score| file_hit(v, fi, score)) {
                hits.push(hit);
            }
        }
    }
    if want("hub") {
        hub_hits(v, &ql, &mut hits);
    }
    Some(ranked(hits, limit))
}

async fn search(State(app): St, Query(q): Query<SearchQ>) -> Result<Response, ApiErr> {
    Ok(Json(search_of(&app, q).await?).into_response())
}

/// GET /search (also the in-process MCP backend).
pub async fn search_of(app: &Arc<AppState>, q: SearchQ) -> Result<Vec<SearchHit>, ApiErr> {
    let v = match &q.view_id {
        Some(id) => app.view(id)?,
        None => default_view(app)
            .ok_or_else(|| api_err(StatusCode::NOT_FOUND, "not-found", "no view indexed yet"))?,
    };
    let kinds: Vec<&str> = q
        .kinds
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .collect();
    let limit = q.limit.unwrap_or(30).min(500);
    // index.db's FTS rows when this view at this revision has them (B2), else in memory.
    let fts = {
        let (v2, text, root) = (v.clone(), q.q.clone(), app.repo.root().to_path_buf());
        let kinds2: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        blocking(move || {
            let kinds: Vec<&str> = kinds2.iter().map(String::as_str).collect();
            let db = crate::view_store::db(&root);
            Ok(db.and_then(|db| search_view_fts(&v2, &db, &text, &kinds, limit)))
        })
        .await?
    };
    let mut hits = match fts {
        Some(h) => h,
        None => {
            // Keyed by view and revision: refreshed views keep their id.
            let sym_key = format!("{}|{}", v.id(), v.bootstrap.base.view.resolved.head);
            let cached = app.read.symbols.lock().unwrap().get(&sym_key).cloned();
            let symbols = match cached {
                Some(s) => s,
                None => {
                    let (app2, v2) = (app.clone(), v.clone());
                    let s = blocking(move || Ok(Arc::new(symbol_index(&app2, &v2)))).await?;
                    app.read.symbols.lock().unwrap().insert(sym_key, s.clone());
                    // Later searches of this revision use FTS.
                    crate::view_store::index_search(app.repo.root(), &v, &s);
                    s
                }
            };
            search_view(&v, &symbols, &q.q, &kinds, limit)
        }
    };
    // Review threads whose comments mention the query (as cv-mock and http.ts SearchHit "thread").
    if kinds.is_empty() || kinds.contains(&"thread") {
        let app2 = app.clone();
        let text = q.q.clone();
        let found = blocking(move || Ok(thread_hits(&app2, &text))).await?;
        if !found.is_empty() {
            hits.extend(found);
            hits.sort_by(|a, b| b.score.total_cmp(&a.score));
            hits.truncate(limit);
        }
    }
    if kinds.contains(&"revision") {
        let app2 = app.clone();
        let text = q.q.clone();
        let revs = blocking(move || {
            let repo = fresh(&app2)?;
            let env = repo.revset_env();
            Ok(search_revisions(&repo, &env, &text, limit))
        })
        .await?;
        hits.extend(revs);
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
    }
    Ok(hits)
}

/// Thread hits: threads with a comment containing `q` (case-insensitive), labelled with the first
/// comment; open ones rank above settled ones. Empty when review.db cannot be opened.
pub fn thread_hits(app: &Arc<AppState>, q: &str) -> Vec<SearchHit> {
    let ql = q.trim().to_lowercase();
    if ql.is_empty() {
        return vec![];
    }
    let Ok(r) = app.review() else {
        return vec![];
    };
    let Ok(threads) = r.list_threads(&cv_review::ThreadFilter::default()) else {
        return vec![];
    };
    threads
        .into_iter()
        .filter(|t| {
            t.comments
                .iter()
                .any(|c| c.body.to_lowercase().contains(&ql))
        })
        .map(|t| {
            let first = t.comments.first().map(|c| c.body.as_str()).unwrap_or("");
            let label: String = first
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect();
            let settled = matches!(t.status.as_str(), "resolved" | "wontfix");
            let score = if settled { 0.35 } else { 0.5 };
            SearchHit {
                kind: "thread".into(),
                label,
                detail: Some(format!("{}:{}", t.anchor.path, t.anchor.start)),
                target: SearchTarget {
                    thread: Some(t.id.clone()),
                    site: Some(SiteRef {
                        path: t.anchor.path.clone(),
                        line: t.anchor.start,
                        window: None,
                        col: None,
                        symbol: None,
                    }),
                    ..Default::default()
                },
                score,
            }
        })
        .collect()
}

/// The snapshot view at `@` (the one `cv serve` indexes at startup), else any snapshot view.
fn default_view(app: &AppState) -> Option<Arc<View>> {
    let views = app.views.read().unwrap();
    let snapshots: Vec<&Arc<View>> = views
        .values()
        .filter(|v| v.bootstrap.base.view.spec["mode"] != "range")
        .collect();
    snapshots
        .iter()
        .find(|v| v.bootstrap.base.view.spec["rev"] == "@")
        .or(snapshots.first())
        .map(|v| (*v).clone())
}

/// Revisions whose change id starts with `q` or whose description contains it (log revset).
fn search_revisions(repo: &JjRepo, env: &RevsetEnv, q: &str, limit: usize) -> Vec<SearchHit> {
    let ql = q.to_lowercase();
    let revset = format!("({}) | working_copies()", env.log);
    let commits = repo.eval(env, &revset, 2000).unwrap_or_default();
    commits
        .into_iter()
        .filter_map(|c| {
            let score = if c.change.starts_with(&ql) {
                0.85
            } else if c.description.to_lowercase().contains(&ql) {
                0.45
            } else {
                return None;
            };
            Some(SearchHit {
                kind: "revision".into(),
                label: c
                    .description
                    .lines()
                    .next()
                    .filter(|l| !l.is_empty())
                    .unwrap_or("(no description set)")
                    .to_string(),
                detail: Some(c.change[..12.min(c.change.len())].to_string()),
                target: SearchTarget {
                    change: Some(c.change.clone()),
                    ..Default::default()
                },
                score,
            })
        })
        .take(limit)
        .collect()
}

// ─── Expand ─────────────────────────────────────────────────────────────────

/// Widens a window's line span. Semantics: the result is an overlay — the window keeps its
/// x/width and its first original line stays where it was (lines added before it grow the frame
/// upward, lines after it downward); neighbours don't move, the layout version doesn't change and
/// later tiles still return the original header. `"symbol"` widens to the innermost declaration
/// that contains the whole current span (the file if none), `"file"` to the whole file, a number
/// by that many lines. A minimized context window comes back expanded.
pub fn expand_header(
    app: &AppState,
    v: &View,
    range: Option<&RangeView>,
    window: &str,
    req: &ExpandRequest,
) -> Result<serde_json::Value, ApiErr> {
    let &wi = v.by_id.get(window).ok_or_else(|| {
        api_err(
            StatusCode::NOT_FOUND,
            "not-found",
            format!("unknown window {window}"),
        )
    })?;
    let w = &v.windows[wi];
    let f = &v.files[w.file];
    let mut h = w.header.clone();
    let n = f.line_count();
    let (s0, e0) = (h.lines.start, h.lines.end);
    let symbol_span = || {
        app.registry
            .pack_for(&f.path)
            .map(|(_, p)| p.analyze(&f.path, &f.text).symbols)
            .unwrap_or_default()
            .iter()
            .filter(|s| s.start <= s0 && e0 <= s.end && (s.start, s.end) != (s0, e0))
            .min_by_key(|s| s.end - s.start)
            .map(|s| (s.start, s.end))
            .unwrap_or((1, n))
    };
    let start = match &req.before {
        None => s0,
        Some(ExpandAmount::Lines(k)) => s0.saturating_sub(*k).max(1),
        Some(ExpandAmount::To(t)) if t == "file" => 1,
        Some(ExpandAmount::To(_)) => symbol_span().0.min(s0),
    };
    let end = match &req.after {
        None => e0,
        Some(ExpandAmount::Lines(k)) => (e0 + k).min(n),
        Some(ExpandAmount::To(t)) if t == "file" => n,
        Some(ExpandAmount::To(_)) => symbol_span().1.max(e0),
    };
    h.lines.start = start;
    h.lines.end = end;
    h.rect.y -= (s0 - start) as f64 * LINE_WU;
    h.rect.h += (s0 - start + end - e0) as f64 * LINE_WU;
    h.flags.truncated = false;
    let mut out = match range {
        Some(r) => r.header_json(&h),
        None => serde_json::to_value(&h).map_err(internal)?,
    };
    out["flags"]["minimized"] = serde_json::Value::Bool(false);
    Ok(out)
}

async fn expand(
    State(app): St,
    Path((view, window)): Path<(String, String)>,
    body: Option<Json<ExpandRequest>>,
) -> Result<Response, ApiErr> {
    let v = crate::workspaces::display_view(&app, &view)?;
    let range = app.read.range(&view);
    let req = body.map(|b| b.0).unwrap_or_default();
    let mut h = expand_header(&app, &v, range.as_deref(), &window, &req)?;
    if range.is_none() {
        // Snapshot views: the widened window pushes what it would cover down (a layout change);
        // the client reloads the layout. Range views keep the overlay below (pills, hunks).
        let lines = (
            h["lines"]["start"].as_u64().unwrap_or(0) as u32,
            h["lines"]["end"].as_u64().unwrap_or(0) as u32,
        );
        let app2 = app.clone();
        let (view2, window2) = (view.clone(), window.clone());
        let (header, version) = tokio::task::spawn_blocking(move || {
            crate::layout_api::expand_in_layout(&app2, &view2, &window2, lines)
        })
        .await
        .map_err(internal)??;
        let mut h = serde_json::to_value(&header).map_err(internal)?;
        crate::workspaces::decorate_headers(&app, &view, &mut h);
        h["layoutVersion"] = version.into();
        return Ok(Json(h).into_response());
    }
    crate::workspaces::decorate_headers(&app, &view, &mut h);
    // ws.ts reason "expand": the window was widened (an overlay: layoutVersion and tiles stay as
    // they were). Only clients subscribed to the view get it.
    let review = app.review.lock().unwrap().clone();
    if let Some(r) = review {
        r.emit(cv_contracts::review::ServerEvent::ViewInvalidated {
            view_id: view.clone(),
            layout_version: v.bootstrap.base.view.layout_version,
            reason: "expand".into(),
            windows: Some(vec![window.clone()]),
            tiles: None,
        });
    }
    Ok(Json(h).into_response())
}
