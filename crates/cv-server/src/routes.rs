//! HTTP routes (docs/contracts/http.ts), the subset the canvas needs: views, bootstrap (T0),
//! tiles (T1), window batches (T2), fragments and blob text (T3). Same shapes as cv-mock.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use cv_contracts::*;
use cv_lang::silhouette::PORT_BIT;
use cv_vcs::JjRepo;
use serde::Deserialize;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{DefaultPredicate, Predicate};

use crate::index::{self, Options, View};

/// Response extension: send the body uncompressed. Tiles are the latency-critical T1 path
/// (DESIGN §9: p95 < 10 ms) and the server only listens on 127.0.0.1 (§10), where gzip of a
/// megabyte-sized tile costs more time (≈ 20 ms at z3) than the transfer it saves.
#[derive(Clone, Copy)]
pub struct Uncompressed;

pub struct AppState {
    pub(crate) repo: JjRepo,
    pub(crate) registry: cv_lang::Registry,
    pub(crate) views: RwLock<HashMap<String, Arc<View>>>,
    /// Review, edits and live events (crate::review_api), opened on first use.
    pub(crate) review: Mutex<Option<Arc<cv_review::Review>>>,
    pub(crate) opts: Options,
    /// Serializes indexing so two requests for a new spec don't build it twice.
    pub(crate) build: Mutex<()>,
    /// History, ranges, workspaces, metrics and search (crate::readside).
    pub read: crate::readside::ReadSide,
    /// Workspaces (D12): live overlays, stack overlaps, agent activity (crate::workspaces).
    pub live: crate::workspaces::Live,
}

#[derive(Debug)]
pub struct ApiErr {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl IntoResponse for ApiErr {
    /// `{ error: ApiError }`; a message with a "\nhint: …" line carries the hint as
    /// `error.details.hint` (e.g. 400 range-not-chain).
    fn into_response(self) -> Response {
        let error = match self.message.split_once("\nhint: ") {
            Some((m, hint)) => cv_contracts::api::ApiError {
                code: self.code.into(),
                message: m.into(),
                details: Some(serde_json::json!({ "hint": hint })),
                retryable: None,
            },
            None => cv_contracts::api::ApiError {
                code: self.code.into(),
                message: self.message,
                details: None,
                retryable: None,
            },
        };
        (self.status, Json(serde_json::json!({ "error": error }))).into_response()
    }
}

fn not_found(message: impl Into<String>) -> ApiErr {
    ApiErr {
        status: StatusCode::NOT_FOUND,
        code: "not-found",
        message: message.into(),
    }
}

/// 404 `view-unknown`: no view with this id is loaded (an id from before a restart, a range view
/// that was dropped, or a typo). Distinct from a plain `not-found` (a path, a window), so a client
/// holding an old id knows to POST its spec again (http.ts "Views after a restart").
pub fn unknown_view(id: &str) -> ApiErr {
    ApiErr {
        status: StatusCode::NOT_FOUND,
        code: "view-unknown",
        message: format!(
            "unknown view {id}: not loaded (the server restarted, or the view was dropped); POST /views with its spec again"
        ),
    }
}

fn bad_request(code: &'static str, message: impl Into<String>) -> ApiErr {
    ApiErr {
        status: StatusCode::BAD_REQUEST,
        code,
        message: message.into(),
    }
}

impl AppState {
    pub fn new(repo: JjRepo, opts: Options) -> Self {
        AppState {
            repo,
            registry: index::registry(),
            opts,
            views: RwLock::new(HashMap::new()),
            review: Mutex::new(None),
            build: Mutex::new(()),
            read: crate::readside::ReadSide::new(),
            live: crate::workspaces::Live::default(),
        }
    }

    /// The repo at the current operation head. When the review service runs it first catches up
    /// with the repo (its op hooks refresh the snapshot views), so a read after a write — ours or
    /// an external `jj` command — sees the write.
    pub fn head_repo(&self) -> Result<JjRepo, ApiErr> {
        let review = self.review.lock().unwrap().clone();
        let internal = |e: cv_vcs::Error| ApiErr {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: e.to_string(),
        };
        match review {
            Some(r) => {
                if let Err(e) = r.sync() {
                    eprintln!("cv: sync: {e}");
                }
                r.vcs().reader().map_err(internal)
            }
            None => self.repo.reload().map_err(internal),
        }
    }

    /// The snapshot view already built for exactly this spec (its id stays stable while its
    /// revision moves: op hooks refresh it in place).
    fn view_of_spec(&self, spec: &serde_json::Value) -> Option<(String, Arc<View>)> {
        self.views
            .read()
            .unwrap()
            .iter()
            .find(|(_, v)| v.bootstrap.base.view.spec == *spec)
            .map(|(k, v)| (k.clone(), v.clone()))
    }

    /// Any snapshot view showing exactly `commit` with the same [`SpecVariant`]. Snapshot content
    /// depends only on the commit, the fileset and the live overlay (other spec options do not
    /// change it), so `rev: "<change>"`, `"<ws>@"` or a commit id that resolves to what `@` shows
    /// reuse that view instead of indexing the repo again; a fileset, `liveWorkingCopy: false` or
    /// another workspace's overlay is a view of its own.
    fn view_at_commit(
        &self,
        repo: &JjRepo,
        commit: &str,
        variant: &SpecVariant,
    ) -> Option<Arc<View>> {
        let views = self.views.read().unwrap();
        let mut hits: Vec<&Arc<View>> = views
            .values()
            .filter(|v| {
                let r = &v.bootstrap.base.view;
                r.spec["mode"] != "range"
                    && r.resolved.head == commit
                    && SpecVariant::of(repo, &r.spec, commit).is_ok_and(|x| x == *variant)
            })
            .collect();
        // Deterministic: the oldest-built view (the startup @ view first).
        hits.sort_by(|a, b| a.id().cmp(b.id()));
        hits.sort_by_key(|v| v.bootstrap.base.view.spec["rev"] != "@");
        hits.first().map(|v| (*v).clone())
    }

    /// Resolves the spec at the current operation head. A spec seen before returns the same view
    /// (same id, same window ids), refreshed to the revision it resolves to now; a spec resolving
    /// to a commit some snapshot view already shows returns that view; anything else is indexed.
    /// A read after a write therefore sees the write under the ids the agent already has.
    pub fn view_for_spec(&self, spec: &serde_json::Value) -> Result<Arc<View>, ApiErr> {
        check_layout(spec)?;
        let mode = spec
            .get("mode")
            .and_then(|m| m.as_str())
            .unwrap_or("snapshot");
        if mode != "snapshot" {
            return Err(bad_request(
                "unsupported",
                format!("view mode {mode:?} is not supported yet; use snapshot"),
            ));
        }
        let repo = self.head_repo()?;
        // ViewSpec.options (model.ts SnapshotOptions) are checked here, not ignored: a workspace
        // that does not exist or a bad fileset is a 400. Without `rev`, a `workspace` option
        // shows that workspace's `@` (else "@"); the stored spec spells the rev out.
        let spec = &normalize_snapshot_spec(&repo, spec)?;
        let rev = spec["rev"].as_str().unwrap_or("@");
        // N6: with the user's revset aliases (`trunk()`, `immutable_heads()` …), as ranges do.
        let invalid = |message: String| ApiErr {
            status: StatusCode::BAD_REQUEST,
            code: "revset-invalid",
            message,
        };
        let mut found = repo
            .eval(&repo.revset_env(), rev, 2)
            .map_err(|e| invalid(e.to_string()))?;
        if found.len() != 1 {
            return Err(invalid(format!(
                "revset {rev:?} resolved to {} commits, expected one",
                found.len()
            )));
        }
        let commit = found.remove(0);
        let variant = SpecVariant::of(&repo, spec, &commit.commit)?;
        if let Some((_, v)) = self.view_of_spec(spec)
            && v.bootstrap.base.view.resolved.head == commit.commit
        {
            return Ok(v);
        }
        if let Some(v) = self.view_at_commit(&repo, &commit.commit, &variant) {
            return Ok(v);
        }
        let _guard = self.build.lock().unwrap();
        if let Some(v) = self.view_at_commit(&repo, &commit.commit, &variant) {
            return Ok(v);
        }
        if let Some((key, v)) = self.view_of_spec(spec) {
            // An op landed between the sync above and now: let the op hook refresh the view (it
            // tells the clients which windows changed), and refresh here only if it did not.
            let review = self.review.lock().unwrap().clone();
            if let Some(r) = review {
                let _ = r.sync();
                if let Some((_, v)) = self.view_of_spec(spec)
                    && v.bootstrap.base.view.resolved.head == commit.commit
                {
                    return Ok(v);
                }
            }
            let (nv, _) =
                crate::review_api::refresh_view(&v, &self.registry, &repo, &commit, &repo.op_id())
                    .map_err(|e| ApiErr {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        code: "index-failed",
                        message: e,
                    })?;
            let nv = Arc::new(nv);
            self.views.write().unwrap().insert(key, nv.clone());
            return Ok(nv);
        }
        let id = index::view_id(spec, &commit.commit, &repo.op_id());
        let view =
            crate::layout_api::build_snapshot(self, &repo, spec.clone(), &commit).map_err(|e| {
                ApiErr {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    code: "index-failed",
                    message: e,
                }
            })?;
        let view = Arc::new(view);
        self.views.write().unwrap().insert(id, view.clone());
        Ok(view)
    }

    pub(crate) fn view(&self, id: &str) -> Result<Arc<View>, ApiErr> {
        self.views
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| unknown_view(id))
    }
}

/// What a snapshot spec changes about the content besides its commit (see
/// [`AppState::view_at_commit`]): the fileset, whether unsnapshotted files are overlaid, and whose.
#[derive(Debug, PartialEq, Eq)]
pub struct SpecVariant {
    fileset: Option<String>,
    live: bool,
    /// An explicit `options.workspace`, `None` when it names the workspace the overlay would use
    /// anyway (the one whose `@` is the commit; the served one first).
    workspace: Option<String>,
}

impl SpecVariant {
    pub fn of(repo: &JjRepo, spec: &serde_json::Value, commit: &str) -> Result<Self, ApiErr> {
        let fileset = spec["fileset"]
            .as_str()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(String::from);
        let live = spec["options"]["liveWorkingCopy"] != serde_json::json!(false);
        let workspace = match spec["options"]["workspace"].as_str() {
            Some(name) => {
                let at = workspace_commit(repo, name)?;
                let serving = repo.workspace_name();
                let default = at == commit
                    && (name == serving
                        || workspace_commit(repo, serving).ok().as_deref() != Some(commit));
                (!default).then(|| name.to_string())
            }
            None => None,
        };
        Ok(SpecVariant {
            fileset,
            live,
            workspace,
        })
    }
}

/// The commit at `name@`; 400 "invalid" when there is no such workspace.
fn workspace_commit(repo: &JjRepo, name: &str) -> Result<String, ApiErr> {
    let rev = cv_vcs::read::workspace_wc_revset(name);
    match repo.eval(&repo.revset_env(), &rev, 1) {
        Ok(found) if !found.is_empty() => Ok(found[0].commit.clone()),
        _ => Err(bad_request(
            "invalid",
            format!("options.workspace {name:?}: no such workspace"),
        )),
    }
}

/// A snapshot spec with its options checked and its `rev` spelled out: `options.workspace` must
/// name a workspace, `options.liveWorkingCopy` must be a boolean, `fileset` must parse (400
/// "fileset-invalid"), and a missing `rev` is `<workspace>@` (or "@").
pub fn normalize_snapshot_spec(
    repo: &JjRepo,
    spec: &serde_json::Value,
) -> Result<serde_json::Value, ApiErr> {
    let mut spec = spec.clone();
    let opts = &spec["options"];
    if !(opts.is_null() || opts.is_object()) {
        return Err(bad_request("invalid", "options must be an object"));
    }
    let live = &opts["liveWorkingCopy"];
    if !(live.is_null() || live.is_boolean()) {
        return Err(bad_request(
            "invalid",
            format!("options.liveWorkingCopy {live}: expected a boolean"),
        ));
    }
    let ws = match &opts["workspace"] {
        serde_json::Value::Null => None,
        serde_json::Value::String(name) => {
            workspace_commit(repo, name)?;
            Some(name.clone())
        }
        other => {
            return Err(bad_request(
                "invalid",
                format!("options.workspace {other}: expected a workspace name"),
            ));
        }
    };
    match &spec["fileset"] {
        serde_json::Value::Null => {}
        serde_json::Value::String(f) => {
            crate::readside::path_filter(Some(f))?;
        }
        other => {
            return Err(bad_request(
                "fileset-invalid",
                format!("fileset {other}: expected a string"),
            ));
        }
    }
    if spec["rev"].is_null() {
        spec["rev"] = serde_json::json!(match ws {
            Some(name) => cv_vcs::read::workspace_wc_revset(&name),
            None => "@".to_string(),
        });
    }
    Ok(spec)
}

/// A view's fileset filter (`ViewSpec.fileset`); everything when it has none or it does not parse
/// (POST /views refuses a bad one, so a stored spec parses).
pub fn spec_fileset(spec: &serde_json::Value) -> cv_vcs::Fileset {
    cv_vcs::Fileset::parse(spec["fileset"].as_str())
        .unwrap_or_else(|_| cv_vcs::Fileset::parse(None).expect("empty fileset"))
}

type St = State<Arc<AppState>>;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/views", post(create_view))
        .route("/api/v1/views/{view}", get(get_view))
        .route("/api/v1/views/{view}/bootstrap", get(bootstrap))
        .route("/api/v1/views/{view}/tiles/{z}/{x}/{y}", get(tile))
        .route("/api/v1/views/{view}/windows:batch", post(batch))
        .route("/api/v1/views/{view}/fragments", get(fragments))
        .route("/api/v1/blobs/{blob}/text", get(blob_text))
        .merge(crate::review_api::router())
        .merge(crate::agent_api::router())
        .merge(crate::readside::router())
        .merge(crate::extras_api::router())
        .merge(crate::layout_api::router())
        .merge(crate::repo_api::router())
        .layer(
            CompressionLayer::new().compress_when(DefaultPredicate::new().and(
                |_: StatusCode,
                 _: axum::http::Version,
                 _: &HeaderMap,
                 ext: &axum::http::Extensions| {
                    ext.get::<Uncompressed>().is_none()
                },
            )),
        )
        .with_state(state)
}

/// `options.layout`: only "districts" (D8) exists. A "dependency" layout (clusters only, no axes)
/// was in an early contract but never specified or built; accepting it and drawing districts
/// anyway would be a silent lie, so anything else is a 400 (DESIGN §5, "Layouts considered").
pub fn check_layout(spec: &serde_json::Value) -> Result<(), ApiErr> {
    match &spec["options"]["layout"] {
        serde_json::Value::Null => Ok(()),
        serde_json::Value::String(s) if s == "districts" => Ok(()),
        other => Err(bad_request(
            "invalid",
            format!("options.layout {other}: only \"districts\" is supported"),
        )),
    }
}

async fn create_view(
    State(app): St,
    body: Option<Json<serde_json::Value>>,
) -> Result<Response, ApiErr> {
    let spec = body
        .map(|b| b.0)
        .unwrap_or_else(|| serde_json::json!({ "mode": "snapshot", "rev": "@" }));
    Ok(Json(view_ref_for(app, spec).await?).into_response())
}

/// POST /views: the ViewRef of a snapshot or range spec, resolved at the current op head (also
/// the in-process MCP backend).
pub async fn view_ref_for(
    app: Arc<AppState>,
    spec: serde_json::Value,
) -> Result<serde_json::Value, ApiErr> {
    tokio::task::spawn_blocking(move || {
        if spec["mode"] == "range" {
            return crate::readside::range::get_or_build(&app, &spec).map(|r| r.view_ref.clone());
        }
        let before: std::collections::HashSet<String> =
            app.views.read().unwrap().keys().cloned().collect();
        let v = app.view_for_spec(&spec)?;
        // A view built just now: its live overlay before the client asks for tiles.
        if !before.contains(v.id()) {
            crate::workspaces::overlay_now(&app, v.id());
        }
        Ok(serde_json::to_value(&v.bootstrap.base.view).unwrap_or_default())
    })
    .await
    .map_err(|e| ApiErr {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal",
        message: e.to_string(),
    })?
}

/// GET /views/:id, with an ETag like the other view-scoped routes (a range's status turns
/// "stale" after an op without a layout bump; the content hash covers that).
async fn get_view(
    State(app): St,
    Path(view): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiErr> {
    if let Some(r) = app.read.range(&view) {
        let lv = r.view_ref["layoutVersion"].as_u64().unwrap_or(0) as u32;
        return Ok(etagged(&headers, lv, &r.view_ref));
    }
    match app.view(&view) {
        Ok(v) => {
            let r = &v.bootstrap.base.view;
            Ok(etagged(&headers, r.layout_version, r))
        }
        // A saved view that is not loaded: what index.db has, marked "stale" (http.ts: the client
        // re-POSTs the spec); its other view-scoped routes answer 404 "view-unknown".
        Err(e) => match disk_bootstrap(&app, &view).await {
            Some(mut b) => {
                b.view.status = "stale".into();
                Ok(etagged(&headers, b.view.layout_version, &b.view))
            }
            None => Err(e),
        },
    }
}

/// A saved view that is not loaded: its bootstrap from index.db (see the tile route).
async fn disk_bootstrap(app: &Arc<AppState>, view: &str) -> Option<cv_contracts::ViewBootstrap> {
    let root = app.repo.root().to_path_buf();
    let view = view.to_string();
    tokio::task::spawn_blocking(move || crate::view_store::disk_bootstrap(&root, &view))
        .await
        .ok()
        .flatten()
}

async fn bootstrap(
    State(app): St,
    Path(view): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiErr> {
    // N7: the live overlay's clusters too (new unsnapshotted files get windows, §4.1), as tiles.
    let v = match crate::workspaces::display_view(&app, &view) {
        Ok(v) => v,
        // A saved view that is not loaded (an id from before a restart): from index.db, like its
        // tiles, so a client that kept the id still gets a consistent view.
        Err(e) => {
            return match disk_bootstrap(&app, &view).await {
                Some(b) => Ok(etagged(&headers, b.view.layout_version, &b)),
                None => Err(e),
            };
        }
    };
    Ok(etagged(
        &headers,
        v.bootstrap.base.view.layout_version,
        &v.bootstrap,
    ))
}

/// A view-scoped JSON response with `ETag: "<layoutVersion>-<content hash>"` (http.ts "Caching"):
/// the layoutVersion leads, and the hash covers what an op or a live overlay changes without a
/// layout bump (blobs, spans, flags). `If-None-Match` with the same tag answers 304.
pub fn etagged(headers: &HeaderMap, layout_version: u32, body: &impl serde::Serialize) -> Response {
    etagged_bytes(
        headers,
        layout_version,
        serde_json::to_vec(body).unwrap_or_default(),
    )
}

/// A word-at-a-time content hash for ETags (several times faster than byte-wise FNV on the
/// megabyte-sized tile and bootstrap bodies; ETags are opaque, so only stability matters).
pub fn etag_hash(bytes: &[u8]) -> u64 {
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ bytes.len() as u64;
    let (words, rest) = bytes.as_chunks::<8>();
    for c in words {
        let w = u64::from_le_bytes(*c);
        h = (h ^ w).wrapping_mul(K).rotate_left(29);
    }
    for &b in rest {
        h = (h ^ b as u64).wrapping_mul(K);
    }
    h ^ (h >> 31)
}

/// [`etagged`] for an already serialized JSON body.
pub fn etagged_bytes(headers: &HeaderMap, layout_version: u32, bytes: Vec<u8>) -> Response {
    let tag = format!("\"{layout_version}-{:016x}\"", etag_hash(&bytes));
    let matches = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().trim_start_matches("W/"))
        .any(|t| t == tag || t == "*");
    let etag = header::HeaderValue::from_str(&tag).expect("ascii etag");
    if matches {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::CACHE_CONTROL, "no-cache".parse().unwrap()),
            ],
        )
            .into_response();
    }
    (
        [
            (header::ETAG, etag),
            (header::CACHE_CONTROL, "no-cache".parse().unwrap()),
            (header::CONTENT_TYPE, "application/json".parse().unwrap()),
        ],
        bytes,
    )
        .into_response()
}

/// Every window intersecting the tile, plus every link with a port inside it.
pub fn tile_response(v: &View, z: u32, x: i64, y: i64) -> TileResponse {
    let (wins, mut t) = tile_parts(v, z, x, y);
    t.windows = wins.iter().map(|&w| v.windows[w].header.clone()).collect();
    t
}

/// The tile's windows (indices into `v.windows`) and a response holding its edges only, so the
/// route can serialize the headers by reference (no clone, no `serde_json::Value` tree): the
/// §9 tile budget.
pub fn tile_parts(v: &View, z: u32, x: i64, y: i64) -> (Vec<usize>, TileResponse) {
    let span = 1i64 << z;
    let rect = Rect {
        x: (x * span) as f64 * TILE_BASE_WU,
        y: (y * span) as f64 * TILE_BASE_WU,
        w: span as f64 * TILE_BASE_WU,
        h: span as f64 * TILE_BASE_WU,
    };
    let mut seen_w = std::collections::HashSet::new();
    let mut seen_l = std::collections::HashSet::new();
    let (mut windows, mut edges) = (Vec::new(), Vec::new());
    for tx in x * span..(x + 1) * span {
        for ty in y * span..(y + 1) * span {
            let Some((ws, ls)) = v.tiles.get(&(tx, ty)) else {
                continue;
            };
            for &w in ws {
                if seen_w.insert(w) && v.windows[w].header.rect.intersects(&rect) {
                    windows.push(w);
                }
            }
            for &l in ls {
                if seen_l.insert(l) {
                    let link = &v.links[l];
                    let (a, b) = v.link_points(link);
                    let (from, to) = (&v.windows[link.from].header, &v.windows[link.to].header);
                    edges.push(TileEdge {
                        id: link.id.clone(),
                        kind: link.kind.into(),
                        from: from.id.clone(),
                        to: to.id.clone(),
                        from_cluster: from.cluster.clone(),
                        to_cluster: to.cluster.clone(),
                        a,
                        b,
                        confidence: link.confidence,
                    });
                }
            }
        }
    }
    (
        windows,
        TileResponse {
            layout_version: v.bootstrap.base.view.layout_version,
            windows: Vec::new(),
            edges,
        },
    )
}

/// A tile as `TileResponse` serializes it, with headers borrowed from the view, or decorated
/// copies where a live overlay flags them (D12).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TileOut<'a> {
    layout_version: u32,
    windows: Vec<HeaderOut<'a>>,
    edges: &'a [TileEdge],
}

#[derive(serde::Serialize)]
#[serde(untagged)]
enum HeaderOut<'a> {
    Plain(&'a WindowHeader),
    Decorated(serde_json::Value),
}

async fn tile(
    State(app): St,
    Path((view, z, x, y)): Path<(String, u32, i64, i64)>,
    headers: HeaderMap,
) -> Result<Response, ApiErr> {
    if z > 12 {
        return Err(not_found("tile level out of range"));
    }
    // The live overlay (unsnapshotted files, D12) when the view has one.
    let v = match crate::workspaces::display_view(&app, &view) {
        Ok(v) => v,
        // A saved view that is not loaded (an id from before a restart): from index.db's R*Tree.
        Err(e) => {
            let root = app.repo.root().to_path_buf();
            let view2 = view.clone();
            let disk = tokio::task::spawn_blocking(move || {
                crate::view_store::disk_tile(&root, &view2, z, x, y)
            })
            .await
            .ok()
            .flatten();
            return match disk {
                Some(t) => Ok(etagged(&headers, t.layout_version, &t)),
                None => Err(e),
            };
        }
    };
    let (wins, mut t) = tile_parts(&v, z, x, y);
    crate::extras_api::overlay_tile(&app, &v, z, x, y, &mut t);
    let lv = t.layout_version;
    if app.read.range(&view).is_some() {
        // Range views rewrite headers (in-range activity, D16): the general path.
        t.windows = wins.iter().map(|&w| v.windows[w].header.clone()).collect();
        let mut res = app.read.patch_tile(&view, t);
        crate::workspaces::decorate_headers(&app, &view, &mut res["windows"]);
        let mut res = etagged(&headers, lv, &res);
        res.extensions_mut().insert(Uncompressed);
        return Ok(res);
    }
    let decorated = crate::workspaces::decorated_ids(&app, &view);
    let windows = wins
        .iter()
        .map(|&w| {
            let h = &v.windows[w].header;
            match &decorated {
                Some(d) if d(&h.id) => {
                    let mut j = serde_json::to_value(h).unwrap_or_default();
                    crate::workspaces::decorate_headers(&app, &view, &mut j);
                    HeaderOut::Decorated(j)
                }
                _ => HeaderOut::Plain(h),
            }
        })
        .collect();
    let out = TileOut {
        layout_version: lv,
        windows,
        edges: &t.edges,
    };
    let mut res = etagged_bytes(&headers, lv, serde_json::to_vec(&out).unwrap_or_default());
    res.extensions_mut().insert(Uncompressed);
    Ok(res)
}

fn symbol_name(v: &View, w: usize) -> String {
    let h = &v.windows[w].header;
    h.symbol
        .as_ref()
        .map(|s| s.qualname.clone())
        .unwrap_or_else(|| h.path.rsplit('/').next().unwrap_or(&h.path).to_string())
}

pub fn batch_response(v: &View, req: &WindowBatchRequest) -> WindowBatchResponse {
    let mut res = WindowBatchResponse {
        outlines: vec![],
        diffs: vec![],
        links: vec![],
        missing: vec![],
    };
    for id in &req.ids {
        let Some(&wi) = v.by_id.get(id) else {
            res.missing.push(id.clone());
            continue;
        };
        let w = &v.windows[wi];
        let h = &w.header;
        let file = &v.files[w.file];
        // One port per (side, line): several links from one callsite share it.
        let mut ports: BTreeMap<(u8, u32), Port> = BTreeMap::new();
        for (side, list) in [("out", &w.out_links), ("in", &w.in_links)] {
            for &li in list.iter() {
                let l = &v.links[li];
                let (line, other) = if side == "out" {
                    (l.from_line, l.to)
                } else {
                    (l.to_line, l.from)
                };
                let arrow = if side == "out" { "→" } else { "←" };
                let name = if side == "out" {
                    l.label.clone()
                } else {
                    symbol_name(v, other)
                };
                let chan = l.channel.as_deref();
                let p = ports
                    .entry((u8::from(side == "in"), line))
                    .or_insert_with(|| Port {
                        id: format!("{side}{line}"),
                        side: side.into(),
                        line,
                        label: match chan {
                            Some(c) => c.port.clone(),
                            None => format!("{arrow} {name}"),
                        },
                        kind: l.kind.into(),
                        links: vec![],
                    });
                p.links.push(l.id.clone());
                // Channel ports keep their key ("⇢ world.entity.moved"); call ports count.
                if p.links.len() > 1 && !p.label.starts_with('⇢') {
                    p.label = if side == "out" {
                        format!("→ {} targets", p.links.len())
                    } else {
                        format!("← {} callers", p.links.len())
                    };
                }
                res.links.push(crate::extras_api::render_link(v, l));
            }
        }
        let (s, e) = ((h.lines.start - 1) as usize * 3, h.lines.end as usize * 3);
        let mut sil =
            file.silhouette[s.min(file.silhouette.len())..e.min(file.silhouette.len())].to_vec();
        for &(_, line) in ports.keys() {
            if let Some(b) = sil.get_mut((line - h.lines.start) as usize * 3 + 2) {
                *b |= PORT_BIT;
            }
        }
        let headings = file
            .headings
            .iter()
            .filter(|(l, _, _)| *l >= h.lines.start && *l <= h.lines.end)
            .map(|(l, d, label)| Heading {
                line: *l,
                depth: *d,
                label: label.clone(),
            })
            .collect();
        res.outlines.push(WindowOutline {
            id: id.clone(),
            silhouette: base64::engine::general_purpose::STANDARD.encode(sil),
            headings,
            ports: ports.into_values().collect(),
        });
    }
    res.links.sort_by(|a, b| a.id.cmp(&b.id));
    res.links.dedup_by(|a, b| a.id == b.id);
    res
}

async fn batch(
    State(app): St,
    Path(view): Path<String>,
    headers: HeaderMap,
    Json(req): Json<WindowBatchRequest>,
) -> Result<Response, ApiErr> {
    if req.ids.len() > 200 {
        return Err(bad_request("too-many-ids", "at most 200 ids per batch"));
    }
    let v = crate::workspaces::display_view(&app, &view)?;
    let mut res = batch_response(&v, &req);
    crate::extras_api::overlay_batch(&app, &v, &req, &mut res);
    app.read.add_diffs(&view, &req, &mut res);
    Ok(etagged(
        &headers,
        v.bootstrap.base.view.layout_version,
        &res,
    ))
}

#[derive(Deserialize)]
struct FragQ {
    path: String,
}

async fn fragments(
    State(app): St,
    Path(view): Path<String>,
    Query(q): Query<FragQ>,
) -> Result<Response, ApiErr> {
    // N7: fragments of live new files (unsnapshotted, §4.1) as the tiles serve them.
    let v = crate::workspaces::display_view(&app, &view)?;
    let &fi = v
        .by_path
        .get(&q.path)
        .ok_or_else(|| not_found(format!("unknown path {}", q.path)))?;
    let f = &v.files[fi];
    let fragments = f
        .windows
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            let h = &v.windows[w].header;
            cv_contracts::api::FragmentRef {
                window: h.id.clone(),
                index: i as u32 + 1,
                rect: h.rect,
                cluster: h.cluster.clone(),
            }
        })
        .collect();
    Ok(Json(cv_contracts::api::FragmentsResponse {
        path: f.path.clone(),
        chip: cv_lang::chunk::chip_slot(&f.path),
        fragments,
    })
    .into_response())
}

#[derive(Deserialize)]
struct TextQ {
    from: Option<u32>,
    to: Option<u32>,
}

async fn blob_text(
    State(app): St,
    Path(blob): Path<String>,
    Query(q): Query<TextQ>,
) -> Result<Response, ApiErr> {
    let res = blob_text_of(&app, blob, q.from, q.to)?;
    // Blobs are content-addressed, so their text never changes.
    Ok((
        [(header::CACHE_CONTROL, "public, max-age=31536000, immutable")],
        Json(res),
    )
        .into_response())
}

/// A blob no view shows (e.g. what an edit wrote at a non-@ commit whose descendants change the
/// file again): its text from the repo store, untokenized (lang "other"). 404 when the store does
/// not have it either.
fn store_blob_text(app: &AppState, blob: String, q: TextQ) -> Result<BlobText, ApiErr> {
    let unknown = || not_found(format!("unknown blob {blob}"));
    if blob.len() != 40 || !blob.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(unknown());
    }
    let repo = app.head_repo()?;
    // Git blobs are addressed by id alone; the path only labels errors.
    let bytes = repo.read_blob("blob", &blob).map_err(|_| unknown())?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len() as u32;
    let from = q.from.unwrap_or(1).max(1);
    let to = q.to.unwrap_or(n).min(n);
    let picked: Vec<String> = (from..=to)
        .filter_map(|l| lines.get((l - 1) as usize).map(|s| s.to_string()))
        .collect();
    Ok(BlobText {
        blob,
        lang: "other".into(),
        from,
        to,
        tokens: vec![Vec::new(); picked.len()],
        lines: picked,
    })
}

/// GET /blobs/:blob/text: lines [from, to] of a blob any view shows or showed (also the
/// in-process MCP backend).
pub fn blob_text_of(
    app: &AppState,
    blob: String,
    from: Option<u32>,
    to: Option<u32>,
) -> Result<BlobText, ApiErr> {
    let q = TextQ { from, to };
    let mut views: Vec<Arc<View>> = app.views.read().unwrap().values().cloned().collect();
    // Live overlays serve the on-disk text of unsnapshotted files under their own blob ids.
    views.extend(crate::workspaces::live_views(app));
    let f = views
        .iter()
        .find_map(|v| v.by_blob.get(&blob).map(|&fi| &v.files[fi]))
        .or_else(|| {
            views
                .iter()
                .find_map(|v| v.retired.get(&blob).map(|f| &**f))
        });
    let Some(f) = f else {
        return store_blob_text(app, blob, q);
    };
    let n = f.line_count();
    let from = q.from.unwrap_or(1).max(1);
    let to = q.to.unwrap_or(n).min(n);
    let range = from..=to;
    let res = BlobText {
        blob,
        lang: f.lang.into(),
        from,
        to,
        lines: range.clone().map(|l| f.line(l).to_string()).collect(),
        tokens: range
            .map(|l| f.tokens.get((l - 1) as usize).cloned().unwrap_or_default())
            .collect(),
    };
    Ok(res)
}
