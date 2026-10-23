//! Repo-level routes (docs/contracts/http.ts): `GET /repo` (RepoInfo), `GET /lanes` (LaneDef[])
//! and saved ranges (`GET /ranges`, `PUT /ranges/:name`, `DELETE /ranges/:name`, persisted in
//! review.db's `saved_range` table, in creation order).
//!
//! Lanes: `GET /lanes` returns every lane in display order (hidden ones included, `other` hidden
//! and collapsed): the defaults with the `.cv/lanes.toml` of the indexed `@` revision over them.

use std::sync::{Arc, Mutex, OnceLock};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, put},
};
use cv_contracts::LaneDef;
use cv_contracts::api::{RepoCapabilities, RepoIndex, RepoInfo, WorkspaceInfo};
use cv_contracts::readside::SavedRange;
use cv_review::Review;
use serde_json::{Value, json};

use crate::routes::{ApiErr, AppState};

/// The jj-lib version cv links (pinned in the workspace Cargo.toml, DESIGN D4).
pub const JJ_LIB_VERSION: &str = "0.45.1";

type R<T> = Result<T, ApiErr>;
type St = State<Arc<AppState>>;

fn err(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiErr {
    ApiErr {
        status,
        code,
        message: message.into(),
    }
}

fn internal(e: impl std::fmt::Display) -> ApiErr {
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/setup", get(setup_route))
        .route("/api/v1/repo", get(repo_route))
        .route("/api/v1/lanes", get(lanes_route))
        .route("/api/v1/ranges", get(ranges_route))
        .route(
            "/api/v1/ranges/{name}",
            put(put_range_route).delete(delete_range_route),
        )
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> R<T> + Send + 'static) -> R<T> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

/// GET /setup: a jj repository (git mode answers this route itself, crate::git_mode).
async fn setup_route(State(app): St) -> Json<cv_contracts::api::SetupInfo> {
    let root = app.repo.root().to_path_buf();
    Json(
        tokio::task::spawn_blocking(move || crate::git_mode::jj_setup(&root))
            .await
            .expect("blocking task"),
    )
}

// ─── Lanes ───────────────────────────────────────────────────────────────────

/// Every lane of `commit` in display order: defaults plus its `.cv/lanes.toml`.
pub fn lane_defs(repo: &cv_vcs::JjRepo, commit: &str) -> Vec<LaneDef> {
    crate::index::lane_defs(&crate::index::lane_config(repo, commit).lanes)
}

/// The serving workspace's `@` snapshot view, else any snapshot view.
fn shown_snapshot(app: &AppState) -> Option<Arc<crate::index::View>> {
    let views = app.views.read().unwrap();
    let mut snaps: Vec<_> = views
        .values()
        .filter(|v| v.bootstrap.base.view.spec["mode"] != "range")
        .cloned()
        .collect();
    snaps.sort_by(|a, b| a.id().cmp(b.id()));
    snaps.sort_by_key(|v| v.bootstrap.base.view.spec["rev"] != "@");
    snaps.into_iter().next()
}

/// The lanes of the revision the serving workspace's `@` view shows (else of `@` now).
async fn lanes_route(State(app): St) -> R<Json<Vec<LaneDef>>> {
    let shown = shown_snapshot(&app).map(|v| v.bootstrap.base.view.resolved.head.clone());
    let app2 = app.clone();
    blocking(move || {
        let repo = app2.head_repo()?;
        let commit = match shown {
            Some(c) => c,
            None => repo.resolve_one("@").map_err(internal)?.commit,
        };
        Ok(Json(lane_defs(&repo, &commit)))
    })
    .await
}

// ─── Repo ────────────────────────────────────────────────────────────────────

/// GET /repo: name, root, jj version, op head, trunk, workspaces, index state and capabilities.
pub async fn repo_info(app: &Arc<AppState>) -> R<RepoInfo> {
    let cards = crate::readside::workspace_cards(app)
        .await
        .unwrap_or(json!([]));
    let workspaces: Vec<WorkspaceInfo> = cards
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|w| {
                    serde_json::from_value(
                        json!({ "name": w["name"], "root": w["root"], "wc": w["wc"] }),
                    )
                    .ok()
                })
                .collect()
        })
        .unwrap_or_default();
    let app2 = app.clone();
    let (op, trunk) = blocking(move || {
        let repo = app2.head_repo()?;
        let trunk = repo
            .resolve_one("trunk()")
            .ok()
            .map(|c| c.commit)
            .filter(|c| !c.chars().all(|ch| ch == '0'));
        Ok((repo.op_id(), trunk))
    })
    .await?;
    let root = app.repo.root();
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    // The serving workspace's `@` view (else any snapshot view) stands for the index.
    let view = shown_snapshot(app);
    // SCIP (§4.5 tier 2) counts as available when the shown index ingested one.
    let scip = view.as_ref().is_some_and(|v| {
        v.links
            .iter()
            .any(|l| l.channel.as_ref().is_some_and(|c| c.provenance == "scip"))
    });
    let running = app.build.try_lock().is_err();
    let mut index = RepoIndex {
        state: if running { "running" } else { "idle" }.into(),
        last_op: None,
        files: 0,
        blobs_parsed: 0,
        churn: None,
    };
    if let Some(v) = view {
        index.last_op = Some(v.bootstrap.base.view.resolved.op.clone());
        index.files = v.files.len() as u32;
        index.blobs_parsed = v.files.iter().filter(|f| f.lang != "other").count() as u32;
        index.churn = Some(v.bootstrap.churn.clone());
    }
    Ok(RepoInfo {
        name,
        root: root.display().to_string(),
        jj_version: JJ_LIB_VERSION.into(),
        op,
        trunk,
        workspaces,
        index,
        capabilities: RepoCapabilities {
            scip,
            runtime_links: false,
            jj_lib: true,
        },
    })
}

async fn repo_route(State(app): St) -> R<Json<RepoInfo>> {
    Ok(Json(repo_info(&app).await?))
}

// ─── Saved ranges ────────────────────────────────────────────────────────────

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(Default::default)
}

fn load(r: &Review) -> Vec<SavedRange> {
    r.db()
        .saved_ranges()
        .unwrap_or_default()
        .into_iter()
        .map(|row| SavedRange {
            name: row.name,
            revset: row.revset,
            fileset: row.fileset,
            options: row.options_json.and_then(|o| serde_json::from_str(&o).ok()),
            created_by: serde_json::from_str(&row.created_by_json).unwrap_or(Value::Null),
        })
        .collect()
}

fn save(r: &Review, ranges: &[SavedRange]) -> R<()> {
    let mut rows = Vec::with_capacity(ranges.len());
    for s in ranges {
        rows.push(cv_store::SavedRangeRow {
            name: s.name.clone(),
            revset: s.revset.clone(),
            fileset: s.fileset.clone(),
            options_json: s
                .options
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(internal)?,
            created_by_json: s.created_by.to_string(),
        });
    }
    r.db().put_saved_ranges(&rows).map_err(internal)
}

fn review(app: &Arc<AppState>) -> R<Arc<Review>> {
    app.review()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.code, e.message))
}

/// GET /ranges: saved ranges in creation order (empty while review.db is not open: read paths
/// never open it).
pub fn list_ranges(app: &AppState) -> Vec<SavedRange> {
    let r = app.review.lock().unwrap().clone();
    r.map(|r| load(&r)).unwrap_or_default()
}

/// PUT /ranges/:name: creates or replaces a saved range. The path names it; `createdBy` is the
/// acting user on creation and kept on updates.
pub fn put_range(
    app: &Arc<AppState>,
    who: &cv_contracts::review::Actor,
    name: &str,
    mut body: SavedRange,
) -> R<SavedRange> {
    let name = name.trim();
    if name.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "invalid",
            "range name is empty",
        ));
    }
    if body.revset.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "invalid", "revset is empty"));
    }
    let r = review(app)?;
    let _g = lock().lock().unwrap();
    let mut ranges = load(&r);
    body.name = name.to_string();
    match ranges.iter_mut().find(|s| s.name == name) {
        Some(old) => {
            body.created_by = old.created_by.clone();
            *old = body.clone();
        }
        None => {
            body.created_by = serde_json::to_value(who).map_err(internal)?;
            ranges.push(body.clone());
        }
    }
    save(&r, &ranges)?;
    Ok(body)
}

/// DELETE /ranges/:name: 404 when no range has that name.
pub fn delete_range(app: &Arc<AppState>, name: &str) -> R<Value> {
    let r = review(app)?;
    let _g = lock().lock().unwrap();
    let mut ranges = load(&r);
    let n = ranges.len();
    ranges.retain(|s| s.name != name);
    if ranges.len() == n {
        return Err(err(
            StatusCode::NOT_FOUND,
            "not-found",
            format!("no saved range {name}"),
        ));
    }
    save(&r, &ranges)?;
    Ok(json!({ "ok": true }))
}

async fn ranges_route(State(app): St) -> Json<Vec<SavedRange>> {
    Json(list_ranges(&app))
}

/// The body's `createdBy` is optional on PUT (the server sets it).
async fn put_range_route(
    State(app): St,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> R<Json<SavedRange>> {
    let who = crate::review_api::actor(&headers);
    if body.get("createdBy").is_none_or(Value::is_null) {
        body["createdBy"] = json!(null);
    }
    if body.get("name").is_none() {
        body["name"] = json!(name);
    }
    let body: SavedRange = serde_json::from_value(body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, "invalid", e.to_string()))?;
    Ok(Json(
        blocking(move || put_range(&app, &who, &name, body)).await?,
    ))
}

async fn delete_range_route(State(app): St, Path(name): Path<String>) -> R<Json<Value>> {
    Ok(Json(blocking(move || delete_range(&app, &name)).await?))
}
