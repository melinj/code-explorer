//! `GET /views/:id/structure` (DESIGN §4.11): how the folder tree fits the data flow and the
//! change frequency of a snapshot view, from `cv_layout::structure`. Cached per view and layout
//! version; MCP `get_structure` reads the same.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
    routing::get,
};
use cv_contracts::readside::StructureResponse;
use cv_layout::structure;
use serde::Deserialize;

use crate::index::{View, is_uncertain};
use crate::routes::{ApiErr, AppState};

type Cache = Mutex<HashMap<String, Arc<StructureResponse>>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/views/{view}/structure", get(structure_route))
}

#[derive(Deserialize)]
struct StructureQ {
    /// Only this directory and what is below it (folders and files); the summary stays whole.
    folder: Option<String>,
}

async fn structure_route(
    State(app): State<Arc<AppState>>,
    Path(view): Path<String>,
    Query(q): Query<StructureQ>,
) -> Result<Response, ApiErr> {
    let s = structure_of(&app, &view).await?;
    Ok(Json(within(&s, q.folder.as_deref())).into_response())
}

/// The response narrowed to `folder` and what is below it.
pub fn within(s: &StructureResponse, folder: Option<&str>) -> StructureResponse {
    let Some(f) = folder
        .map(|f| f.trim_matches('/'))
        .filter(|f| !f.is_empty())
    else {
        return s.clone();
    };
    let under = |p: &str| p == f || p.starts_with(&format!("{f}/"));
    StructureResponse {
        summary: s.summary.clone(),
        folders: s
            .folders
            .iter()
            .filter(|d| under(&d.path))
            .cloned()
            .collect(),
        files: s
            .files
            .iter()
            .filter(|x| under(&x.folder))
            .cloned()
            .collect(),
        windows: s
            .windows
            .iter()
            .filter(|w| under(cv_layout::dirname(&w.path)))
            .cloned()
            .collect(),
    }
}

/// The structure of a snapshot view (cached by view and layout version).
pub async fn structure_of(
    app: &Arc<AppState>,
    view: &str,
) -> Result<Arc<StructureResponse>, ApiErr> {
    let v = app.view(view)?;
    let key = format!("{view}@{}", v.bootstrap.base.view.layout_version);
    if let Some(s) = cache().lock().unwrap().get(&key) {
        return Ok(s.clone());
    }
    let repo_key = app.repo.root().display().to_string();
    let s = tokio::task::spawn_blocking(move || Arc::new(analyse(&v, &repo_key)))
        .await
        .map_err(|e| ApiErr {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: e.to_string(),
        })?;
    let mut c = cache().lock().unwrap();
    // Older layout versions of this view are not asked for again.
    c.retain(|k, _| !k.starts_with(&format!("{view}@")));
    c.insert(key, s.clone());
    Ok(s)
}

/// The view's files, windows and solid links, with each window's dependency cluster: its own on
/// the district grid; on the folder tree, the one the repo's last district layout gave its key.
fn analyse(v: &View, repo_key: &str) -> StructureResponse {
    let folders = crate::layout_api::is_folders(v);
    let placement = folders
        .then(|| crate::layout_input::last_placement(repo_key))
        .flatten();
    let mut first: HashMap<usize, usize> = HashMap::new();
    for (i, w) in v.windows.iter().enumerate() {
        first.entry(w.file).or_insert(i);
    }
    let files: Vec<structure::FileIn> = v
        .files
        .iter()
        .enumerate()
        .map(|(fi, f)| {
            let h = first.get(&fi).map(|&w| &v.windows[w].header);
            structure::FileIn {
                path: &f.path,
                lane: f.lane,
                heat: h.map_or(0, |h| h.heat.grade),
                rate: h.map_or(0.0, |h| h.heat.rate),
                lines: f.line_starts.len() as u32,
            }
        })
        .collect();
    let windows: Vec<structure::WinIn> = v
        .windows
        .iter()
        .map(|w| structure::WinIn {
            key: &w.header.key,
            file: w.file,
            module: match &placement {
                Some(p) => p.windows.get(&w.header.key).map(|&(c, _)| c),
                None if folders => None,
                None => w
                    .header
                    .cluster
                    .strip_prefix('c')
                    .and_then(|n| n.parse().ok()),
            },
        })
        .collect();
    let links: Vec<structure::LinkIn> = v
        .links
        .iter()
        .filter(|l| !l.is_dynamic() && !is_uncertain(l.kind, l.confidence))
        .map(|l| structure::LinkIn {
            from: l.from,
            to: l.to,
            import: l.kind == "import",
        })
        .collect();
    structure::analyse(&files, &windows, &links)
}
