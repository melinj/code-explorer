//! Review, edit and conflict routes, the WebSocket and the long-poll (docs/contracts/http.ts
//! "Review", "Edits and conflicts", "Events"; docs/contracts/ws.ts), backed by
//! [`cv_review::Review`]. Same shapes as cv-mock.
//!
//! After every new jj operation the review service calls [`refresh_views`] (an op hook): each
//! snapshot view whose revision now resolves to a new commit gets the changed files re-read and
//! re-parsed, their windows re-served at the same places, and `view.invalidated` lists those
//! windows. Replaced blobs stay readable by id (`View::retired`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cv_contracts::review::*;
use cv_contracts::{HEADER_WU, LINE_WU, LineSpan, Resolved};
use cv_lang::chunk;
use cv_review::{OpChange, Review, ReviewError, ThreadFilter};
use cv_vcs::{CommitInfo, JjRepo};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::index::View;
use crate::routes::AppState;

/// How often the op-head watcher looks at `.jj/repo/op_heads` (DESIGN §9: canvas updates < 2 s
/// after a jj op).
pub const WATCH_INTERVAL: Duration = Duration::from_millis(250);

impl AppState {
    /// The review service for this repo, opened on first use (review.db at its XDG location),
    /// with the view-refresh op hook and the op-head watcher running.
    pub fn review(self: &Arc<Self>) -> Result<Arc<Review>, ReviewError> {
        let mut slot = self.review.lock().unwrap();
        if let Some(r) = slot.as_ref() {
            return Ok(r.clone());
        }
        let r = Review::open(self.repo.root())?;
        install_hook(self, &r);
        r.start_watcher(WATCH_INTERVAL);
        *slot = Some(r.clone());
        Ok(r)
    }

    /// Uses a review service built elsewhere (tests: in-memory review.db). Registers the view
    /// refresh hook; the caller decides about the watcher.
    pub fn set_review(self: &Arc<Self>, r: Arc<Review>) {
        install_hook(self, &r);
        *self.review.lock().unwrap() = Some(r);
    }
}

fn install_hook(state: &Arc<AppState>, r: &Arc<Review>) {
    // History badges (§7) and full workspace cards for workspace.updated (D12).
    crate::workspaces::install(state, r);
    let weak_state = Arc::downgrade(state);
    let weak_review = Arc::downgrade(r);
    r.add_op_hook(Box::new(move |c: &OpChange| {
        match (weak_state.upgrade(), weak_review.upgrade()) {
            (Some(s), Some(r)) => refresh_views(&s, &r, c),
            _ => vec![],
        }
    }));
}

// ── Errors ──

pub struct RErr(pub ReviewError);

impl From<ReviewError> for RErr {
    fn from(e: ReviewError) -> Self {
        RErr(e)
    }
}

impl IntoResponse for RErr {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        // http.ts: { error: ApiError }. `code`/`message` are repeated at the top level for clients
        // written against cv-mock, which answered with a bare ApiError.
        (
            status,
            Json(json!({
                "error": cv_contracts::api::ApiError {
                    code: self.0.code.into(),
                    message: self.0.message.clone(),
                    ..Default::default()
                },
                "code": self.0.code,
                "message": self.0.message,
            })),
        )
            .into_response()
    }
}

fn bad(message: impl Into<String>) -> RErr {
    RErr(ReviewError {
        status: 400,
        code: "invalid",
        message: message.into(),
    })
}

type St = State<Arc<AppState>>;
type R<T> = Result<T, RErr>;

/// Runs a blocking review call off the async runtime.
async fn blocking<T: Send + 'static>(
    app: &Arc<AppState>,
    f: impl FnOnce(Arc<Review>) -> Result<T, ReviewError> + Send + 'static,
) -> R<T> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || {
        let r = app.review()?;
        f(r)
    })
    .await
    .map_err(|e| {
        RErr(ReviewError {
            status: 500,
            code: "internal",
            message: e.to_string(),
        })
    })?
    .map_err(RErr)
}

/// The acting user: `X-CV-Actor: <Actor JSON>`, or cv-mcp's attribution headers
/// (`X-CV-Agent`, `X-CV-Agent-Client`, `X-CV-Workspace`), else the human at the UI. An agent's
/// `workspace` comes from `X-CV-Workspace` when the Actor JSON does not name one.
pub(crate) fn actor(headers: &HeaderMap) -> Actor {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
    };
    let ws = h("x-cv-workspace").map(String::from);
    let with_ws = |a: Actor| match a {
        Actor::Agent {
            name,
            client,
            workspace,
        } => Actor::Agent {
            name,
            client,
            workspace: workspace.or(ws.clone()),
        },
        a => a,
    };
    if let Some(a) = h("x-cv-actor").and_then(|v| serde_json::from_str::<Actor>(v).ok()) {
        return with_ws(a);
    }
    if let Some(name) = h("x-cv-agent") {
        return Actor::Agent {
            name: name.into(),
            client: h("x-cv-agent-client").map(String::from),
            workspace: ws,
        };
    }
    Actor::human("you")
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/threads", get(list_threads).post(create_thread))
        .route("/api/v1/threads/{id}/comments", post(comment))
        .route("/api/v1/threads/{id}", get(get_thread).patch(patch_thread))
        .route("/api/v1/reviews:submit", post(submit))
        .route("/api/v1/reviews/marks/{change}", axum::routing::put(mark))
        .route("/api/v1/edits", post(new_edit))
        .route("/api/v1/edits/{id}", get(get_edit))
        .route("/api/v1/edits/{id}/apply", post(apply_edit))
        .route("/api/v1/edits/{id}/undo", post(undo_edit))
        .route("/api/v1/refs", get(refs))
        .route("/api/v1/graph:op", post(graph_op))
        .route("/api/v1/graph:undo", post(graph_undo))
        .route("/api/v1/ops", get(ops))
        .route("/api/v1/ops/{op}", get(op_detail))
        .route("/api/v1/ops:restore", post(ops_restore))
        .route("/api/v1/git:fetch", post(git_fetch))
        .route("/api/v1/git:push", post(git_push))
        .route("/api/v1/conflicts", get(conflicts))
        .route(
            "/api/v1/conflicts/{commit}/{*path}",
            get(conflict_detail).post(resolve_conflict),
        )
        .route("/api/v1/events", get(events))
        .route("/api/v1/ws", get(ws))
}

// ── Threads ──

#[derive(Deserialize)]
struct ThreadQ {
    status: Option<String>,
    assignee: Option<String>,
    revset: Option<String>,
    path: Option<String>,
}

async fn list_threads(State(app): St, Query(q): Query<ThreadQ>) -> R<Json<Vec<Thread>>> {
    let ts = blocking(&app, move |r| {
        let f = ThreadFilter {
            status: q.status,
            assignee: q.assignee,
            path: q.path,
            changes: None,
        };
        match q.revset {
            Some(rs) if !rs.trim().is_empty() => r.list_threads_in(&rs, f),
            _ => r.list_threads(&f),
        }
    })
    .await?;
    Ok(Json(ts))
}

async fn get_thread(State(app): St, Path(id): Path<String>) -> R<Json<Thread>> {
    Ok(Json(blocking(&app, move |r| r.get_thread(&id)).await?))
}

async fn create_thread(
    State(app): St,
    headers: HeaderMap,
    Json(body): Json<NewThread>,
) -> R<Json<Thread>> {
    let who = actor(&headers);
    Ok(Json(
        blocking(&app, move |r| r.create_thread(body, &who)).await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentBody {
    #[serde(default)]
    body: String,
    set_status: Option<String>,
}

async fn comment(
    State(app): St,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<CommentBody>,
) -> R<Json<Thread>> {
    let who = actor(&headers);
    Ok(Json(
        blocking(&app, move |r| {
            r.reply(&id, &b.body, b.set_status.as_deref(), &who)
        })
        .await?,
    ))
}

async fn patch_thread(
    State(app): St,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> R<Json<Thread>> {
    let who = actor(&headers);
    let status = match b.get("status") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(bad("status must be a string")),
    };
    // Absent = unchanged; null = cleared.
    let assignee: Option<Option<String>> = match b.get("assignee") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(_) => return Err(bad("assignee must be a string or null")),
    };
    Ok(Json(
        blocking(&app, move |r| {
            r.patch_thread(
                &id,
                status.as_deref(),
                assignee.as_ref().map(|a| a.as_deref()),
                &who,
            )
        })
        .await?,
    ))
}

#[derive(Deserialize)]
struct SubmitBody {
    threads: Vec<String>,
    note: Option<String>,
}

async fn submit(State(app): St, headers: HeaderMap, Json(b): Json<SubmitBody>) -> R<Json<Value>> {
    let who = actor(&headers);
    let cursor = blocking(&app, move |r| {
        r.submit_review(&b.threads, b.note.as_deref(), &who)
    })
    .await?;
    Ok(Json(json!({ "cursor": cursor.to_string() })))
}

#[derive(Deserialize)]
struct MarkBody {
    commit: String,
}

/// PUT /reviews/marks/:change → RevisionReview.
async fn mark(
    State(app): St,
    headers: HeaderMap,
    Path(change): Path<String>,
    Json(b): Json<MarkBody>,
) -> R<Json<Value>> {
    let who = actor(&headers);
    let res = blocking(&app, move |r| {
        let c = r.vcs().resolve_rev(&change).map_err(|e| ReviewError {
            status: 404,
            code: "not-found",
            message: e.0,
        })?;
        r.mark_reviewed(&c.change, &b.commit, &who.label())?;
        let open = r
            .list_threads(&ThreadFilter {
                status: Some("open,in-progress,addressed".into()),
                changes: Some(vec![c.change.clone()]),
                ..Default::default()
            })?
            .len();
        let touched = r
            .list_threads(&ThreadFilter {
                changes: Some(vec![c.change.clone()]),
                ..Default::default()
            })?
            .iter()
            .any(|t| {
                t.comments
                    .iter()
                    .any(|m| matches!(m.author, Actor::Agent { .. }))
            });
        Ok(json!({
            "change": c.change,
            "openThreads": open,
            "reviewedAt": b.commit,
            "changedSinceReview": c.commit != b.commit,
            "touchedByAgent": touched,
        }))
    })
    .await?;
    Ok(Json(res))
}

// ── Edits ──

async fn new_edit(
    State(app): St,
    headers: HeaderMap,
    Json(mut req): Json<EditRequest>,
) -> R<Json<EditResult>> {
    let who = actor(&headers);
    crate::agent_api::check_edit(&app, &who)?;
    crate::workspaces::rebase_live_edit(&app, &mut req);
    crate::workspaces::note_edit(&app, &who);
    Ok(Json(blocking(&app, move |r| r.edit(req, &who)).await?))
}

async fn get_edit(State(app): St, Path(id): Path<String>) -> R<Json<EditResult>> {
    Ok(Json(blocking(&app, move |r| r.get_edit(&id)).await?))
}

async fn apply_edit(
    State(app): St,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<Json<EditResult>> {
    let who = actor(&headers);
    crate::agent_api::check_edit(&app, &who)?;
    crate::workspaces::note_edit(&app, &who);
    Ok(Json(
        blocking(&app, move |r| r.apply_edit(&id, &who)).await?,
    ))
}

/// POST /edits/:id/undo. 409 `undo-blocked` with `error.details` = `UndoBlocked` when reverting
/// would leave changes divergent or conflicted (nothing is written then).
async fn undo_edit(State(app): St, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let who = actor(&headers);
    if let Err(e) = crate::agent_api::check_edit(&app, &who) {
        return e.into_response();
    }
    match blocking(&app, move |r| r.undo_edit_checked(&id, &who)).await {
        Ok(Ok(result)) => Json(result).into_response(),
        Ok(Err(blocked)) => {
            let message = cv_review::undo_blocked_text(&blocked);
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": cv_contracts::api::ApiError {
                        code: "undo-blocked".into(),
                        message: message.clone(),
                        details: serde_json::to_value(&blocked).ok(),
                        retryable: None,
                    },
                    "code": "undo-blocked",
                    "message": message,
                })),
            )
                .into_response()
        }
        Err(e) => e.into_response(),
    }
}

// ── Graph view ──

async fn refs(State(app): St) -> R<Json<RefsResponse>> {
    Ok(Json(blocking(&app, move |r| r.refs()).await?))
}

/// POST /graph:op: bookmarks, landing and history edits from the Graph view. Agents need the edit
/// permission, like edits.
async fn graph_op(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<GraphOpRequest>,
) -> R<Json<GraphOpResult>> {
    let who = actor(&headers);
    crate::agent_api::check_edit(&app, &who)?;
    if !req.dry_run.unwrap_or(false) {
        crate::workspaces::note_edit(&app, &who);
    }
    Ok(Json(blocking(&app, move |r| r.graph_op(req, &who)).await?))
}

#[derive(Deserialize)]
struct UndoOp {
    op: String,
}

async fn graph_undo(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<UndoOp>,
) -> R<Json<GraphOpResult>> {
    let who = actor(&headers);
    crate::agent_api::check_edit(&app, &who)?;
    Ok(Json(
        blocking(&app, move |r| r.undo_graph_op(&req.op, &who)).await?,
    ))
}

/// Operations that reach past this session (restoring the whole repo, the network): the human only.
fn human_only(who: &Actor) -> R<()> {
    if who.agent_name().is_some() {
        return Err(RErr(ReviewError {
            status: 403,
            code: "human-only",
            message: "Only the human at the UI can do this.".into(),
        }));
    }
    Ok(())
}

#[derive(Deserialize)]
struct OpsQ {
    limit: Option<usize>,
}

async fn ops(State(app): St, Query(q): Query<OpsQ>) -> R<Json<OpLog>> {
    Ok(Json(blocking(&app, move |r| r.ops(q.limit)).await?))
}

async fn op_detail(State(app): St, Path(op): Path<String>) -> R<Json<OpDetail>> {
    Ok(Json(blocking(&app, move |r| r.op_detail(&op)).await?))
}

async fn ops_restore(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<RestoreRequest>,
) -> R<Json<GraphOpResult>> {
    let who = actor(&headers);
    human_only(&who)?;
    Ok(Json(
        blocking(&app, move |r| r.restore_op(req, &who)).await?,
    ))
}

async fn git_fetch(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<FetchRequest>,
) -> R<Json<RemoteResult>> {
    let who = actor(&headers);
    human_only(&who)?;
    Ok(Json(blocking(&app, move |r| r.git_fetch(req, &who)).await?))
}

async fn git_push(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<PushRequest>,
) -> R<Json<RemoteResult>> {
    let who = actor(&headers);
    human_only(&who)?;
    Ok(Json(blocking(&app, move |r| r.git_push(req, &who)).await?))
}

// ── Conflicts ──

#[derive(Deserialize)]
struct RevsetQ {
    revset: Option<String>,
}

async fn conflicts(State(app): St, Query(q): Query<RevsetQ>) -> R<Json<Vec<ConflictRecord>>> {
    Ok(Json(
        blocking(&app, move |r| r.list_conflicts(q.revset.as_deref())).await?,
    ))
}

async fn conflict_detail(
    State(app): St,
    Path((commit, path)): Path<(String, String)>,
) -> R<Json<ConflictDetail>> {
    Ok(Json(
        blocking(&app, move |r| r.conflict_detail(&commit, &path)).await?,
    ))
}

#[derive(Deserialize)]
struct ResolveBody {
    content: String,
}

/// POST .../conflicts/:commit/*path/resolve (axum's wildcard takes "/resolve" with the path).
async fn resolve_conflict(
    State(app): St,
    headers: HeaderMap,
    Path((commit, path)): Path<(String, String)>,
    Json(b): Json<ResolveBody>,
) -> R<Json<EditResult>> {
    let Some(path) = path.strip_suffix("/resolve").map(String::from) else {
        return Err(bad("POST to .../conflicts/:commit/:path/resolve"));
    };
    let who = actor(&headers);
    crate::agent_api::check_edit(&app, &who)?;
    Ok(Json(
        blocking(&app, move |r| {
            r.resolve_conflict(&commit, &path, &b.content, &who)
        })
        .await?,
    ))
}

// ── Events ──

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventsQ {
    cursor: Option<String>,
    kinds: Option<String>,
    timeout_s: Option<f64>,
}

/// GET /events: the long-poll twin of the WebSocket (MCP `wait_for_events`).
async fn events(State(app): St, Query(q): Query<EventsQ>) -> R<Json<EventsResponse>> {
    let after = match q.cursor.as_deref().filter(|c| !c.is_empty()) {
        Some(c) => Some(
            c.parse::<u64>()
                .map_err(|_| bad(format!("bad cursor {c:?}")))?,
        ),
        None => None,
    };
    let kinds: Option<Vec<String>> = q.kinds.filter(|k| !k.is_empty()).map(|k| {
        k.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });
    let timeout = Duration::from_secs_f64(q.timeout_s.unwrap_or(25.0).clamp(0.0, 120.0));
    let (cursor, events, expired) = blocking(&app, move |r| {
        // Checked before reading: the read may block while a prune runs.
        let expired = after.is_some_and(|a| r.events().expired(a));
        let (c, e) = r.events().read(after, kinds.as_deref(), timeout);
        Ok((c, e, expired))
    })
    .await?;
    Ok(Json(EventsResponse {
        cursor: cursor.to_string(),
        events,
        resync: expired.then_some(true),
    }))
}

#[derive(Deserialize)]
struct WsQ {
    cursor: Option<String>,
    /// The UI session (`?session=`): an open socket keeps its agent toggles in force (N9).
    session: Option<String>,
}

async fn ws(State(app): St, Query(q): Query<WsQ>, up: WebSocketUpgrade) -> R<Response> {
    let app2 = app.clone();
    let review = tokio::task::spawn_blocking(move || app2.review())
        .await
        .map_err(|e| bad(e.to_string()))??;
    let cursor = q.cursor.and_then(|c| c.parse::<u64>().ok());
    let session = q.session.filter(|s| !s.trim().is_empty());
    Ok(up.on_upgrade(move |socket| async move {
        let _present = session.map(|s| crate::agent_api::open_session(s.trim()));
        serve_ws(review, socket, cursor).await
    }))
}

fn frame(seq: u64, event: &ServerEvent) -> Message {
    let f = json!({ "seq": seq.to_string(), "event": event });
    Message::Text(f.to_string().into())
}

static WS_CONN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The view a view-scoped event is about (`view.invalidated`, `index.progress` with a view).
pub fn view_scope(ev: &ServerEvent) -> Option<&str> {
    match ev {
        ServerEvent::ViewInvalidated { view_id, .. } => Some(view_id),
        ServerEvent::IndexProgress {
            view_id: Some(v), ..
        } => Some(v),
        _ => None,
    }
}

/// One WS client's `view.subscribe` set (ws.ts). A client that never subscribed keeps receiving
/// every view's events (older clients), except `reason: "expand"`, which only subscribers get
/// (it is informational: the expanding client already has the widened header).
#[derive(Default)]
pub struct Subscriptions {
    ever: bool,
    views: std::collections::HashSet<String>,
}

impl Subscriptions {
    pub fn subscribe(&mut self, view: &str) {
        self.ever = true;
        self.views.insert(view.to_string());
    }

    pub fn unsubscribe(&mut self, view: &str) {
        self.ever = true;
        self.views.remove(view);
    }

    pub fn wants(&self, ev: &ServerEvent) -> bool {
        let Some(view) = view_scope(ev) else {
            return true;
        };
        if self.ever {
            return self.views.contains(view);
        }
        !matches!(ev, ServerEvent::ViewInvalidated { reason, .. } if reason == "expand")
    }
}

/// Answer to a client `ping`: not logged; `seq` is the last frame this client was sent.
fn pong(seq: u64) -> Message {
    let f =
        json!({ "seq": seq.to_string(), "event": { "type": "pong", "at": cv_store::now_iso() } });
    Message::Text(f.to_string().into())
}

/// One client: hello, replay after its cursor, then live frames. Accepts `view.state` (kept per
/// connection for MCP `get_view_state`), `ping` and view (un)subscribe.
async fn serve_ws(review: Arc<Review>, mut ws: WebSocket, cursor: Option<u64>) {
    let conn = format!(
        "{}-{}",
        review.session(),
        WS_CONN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let mut rx = review.events().subscribe();
    let head = review.events().cursor();
    // A cursor older than the log keeps cannot be replayed without a hole: say so, replay nothing.
    let expired = cursor.is_some_and(|c| review.events().expired(c));
    let hello = ServerEvent::Hello {
        session: conn.clone(),
        op: review.op(),
        server_version: format!("cv {}", env!("CARGO_PKG_VERSION")),
        cursor: head.to_string(),
        resync: expired.then_some(true),
    };
    if ws.send(frame(head, &hello)).await.is_err() {
        return;
    }
    let mut sent = head;
    let mut subs = Subscriptions::default();
    if let Some(c) = cursor.filter(|_| !expired) {
        // Replay what the client missed, in pages, then hand over to the live stream.
        let mut from = c;
        loop {
            let r = review.clone();
            let page = tokio::task::spawn_blocking(move || r.events().since(from, None))
                .await
                .unwrap_or_default();
            let Some((last, _)) = page.last() else { break };
            from = *last;
            for (seq, ev) in &page {
                if subs.wants(ev) && ws.send(frame(*seq, ev)).await.is_err() {
                    return;
                }
            }
            sent = sent.max(from);
        }
    }
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok((seq, ev)) => {
                    if seq <= sent {
                        continue;
                    }
                    if subs.wants(&ev) && ws.send(frame(seq, &ev)).await.is_err() {
                        break;
                    }
                    sent = seq;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let r = review.clone();
                    let from = sent;
                    let page = tokio::task::spawn_blocking(move || r.events().since(from, None))
                        .await
                        .unwrap_or_default();
                    for (seq, ev) in &page {
                        if subs.wants(ev) && ws.send(frame(*seq, ev)).await.is_err() {
                            review.clear_view_state(&conn);
                            return;
                        }
                        sent = *seq;
                    }
                }
                Err(_) => break,
            },
            incoming = ws.recv() => match incoming {
                Some(Ok(Message::Text(t))) => {
                    match serde_json::from_str::<ClientMessage>(&t) {
                        Ok(ClientMessage::ViewState { state }) => review.set_view_state(&conn, state),
                        Ok(ClientMessage::ViewSubscribe { view_id }) => subs.subscribe(&view_id),
                        Ok(ClientMessage::ViewUnsubscribe { view_id }) => subs.unsubscribe(&view_id),
                        Ok(ClientMessage::Ping) => {
                            if ws.send(pong(sent)).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    // Gone: this UI no longer follows the agent (agent/focus answers "suggested").
    review.clear_view_state(&conn);
}

// ── View refresh after an operation ──

/// The op hook: refreshes every snapshot view whose revision moved, and says which windows to
/// refetch. Views whose commit did not move still get an (empty) `view.invalidated`, so clients
/// know the op was seen.
pub fn refresh_views(state: &Arc<AppState>, review: &Review, _c: &OpChange) -> Vec<ServerEvent> {
    let Ok(repo) = review.vcs().reader() else {
        return vec![];
    };
    let mut views: Vec<(String, Arc<View>)> = state
        .views
        .read()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // IndexScheduler: the view on screen first, its files in view before the rest.
    let focus = crate::scheduler::Focus::latest(review);
    crate::scheduler::order_views(&mut views, focus.as_ref());
    let mut out = Vec::new();
    // Range views: the members are fixed commits, so a range is stale exactly when its revset
    // now resolves to other commits (ViewRef.status "stale"; the client re-POSTs the spec).
    out.extend(crate::readside::range::invalidate_on_op(state, &repo));
    for (key, v) in views {
        let spec = &v.bootstrap.base.view.spec;
        if spec
            .get("mode")
            .and_then(|m| m.as_str())
            .unwrap_or("snapshot")
            != "snapshot"
        {
            continue;
        }
        let rev = spec.get("rev").and_then(|r| r.as_str()).unwrap_or("@");
        let Ok(commit) = review.vcs().resolve_rev(rev) else {
            continue;
        };
        let mut windows = Vec::new();
        if commit.commit != v.bootstrap.base.view.resolved.head {
            let t = std::time::Instant::now();
            let mut publish = |nv: Arc<View>, ids: Vec<String>, (done, total): (usize, usize)| {
                let events = crate::scheduler::stage_events(&nv, ids, done, total);
                state.views.write().unwrap().insert(key.clone(), nv);
                for ev in events {
                    review.emit(ev);
                }
            };
            let staged = crate::scheduler::refresh_staged(
                &v,
                &state.registry,
                &repo,
                &commit,
                &review.op(),
                focus.as_ref(),
                &mut publish,
            );
            match staged {
                Ok((nv, changed)) => {
                    if std::env::var_os("CV_DEBUG_REFRESH").is_some() {
                        eprintln!(
                            "cv: refreshed view {key} in {:.1?} ({} windows)",
                            t.elapsed(),
                            changed.len()
                        );
                    }
                    let moved = !changed.is_empty();
                    windows = changed;
                    state
                        .views
                        .write()
                        .unwrap()
                        .insert(key.clone(), Arc::new(nv));
                    if moved {
                        settle_in_background(state, key);
                    }
                }
                Err(e) => eprintln!("cv: refreshing view {}: {e}", v.id()),
            }
        }
        out.push(ServerEvent::ViewInvalidated {
            view_id: v.id().to_string(),
            layout_version: v.bootstrap.base.view.layout_version,
            reason: "op".into(),
            windows: Some(windows),
            tiles: None,
        });
    }
    out
}

/// View keys with a background settle running → whether another round was asked for meanwhile.
static SETTLING: std::sync::Mutex<Option<HashMap<String, bool>>> = std::sync::Mutex::new(None);

/// Runs [`crate::incremental::settle`] for view `key` on a background thread (channel links, hubs,
/// connectedness and heat after an incremental refresh, DESIGN §4.1), then stores the result and
/// emits `view.invalidated { reason: "index" }` for the windows it changed (no list when the hubs
/// changed: refetch everything). Requests while one runs coalesce into one more round.
pub fn settle_in_background(state: &Arc<AppState>, key: String) {
    {
        let mut g = SETTLING.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::new);
        if let Some(again) = m.get_mut(&key) {
            *again = true;
            return;
        }
        m.insert(key.clone(), false);
    }
    let weak = Arc::downgrade(state);
    let k = key.clone();
    let spawned = std::thread::Builder::new()
        .name("cv-settle".into())
        .spawn(move || {
            let key = k;
            loop {
                if let Some(app) = weak.upgrade() {
                    settle_now(&app, &key);
                }
                let mut g = SETTLING.lock().unwrap();
                let m = g.get_or_insert_with(HashMap::new);
                if m.get(&key) == Some(&true) && weak.strong_count() > 0 {
                    m.insert(key.clone(), false);
                    continue;
                }
                m.remove(&key);
                break;
            }
        });
    if spawned.is_err() {
        SETTLING.lock().unwrap().as_mut().map(|m| m.remove(&key));
    }
}

/// One settle round for view `key` (see [`settle_in_background`]); returns the event it emitted.
/// When the view was replaced while settling, retries on the new one (a few times).
pub fn settle_now(app: &Arc<AppState>, key: &str) -> Option<ServerEvent> {
    for _ in 0..3 {
        let v = app.views.read().unwrap().get(key).cloned()?;
        let review = app.review.lock().unwrap().clone();
        let repo = match &review {
            Some(r) => r.vcs().reader().ok()?,
            None => app.repo.reload().ok()?,
        };
        let t = std::time::Instant::now();
        let s = match crate::incremental::settle(&v, &app.registry, &repo, app.opts.max_commits) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cv: settling view {key}: {e}");
                return None;
            }
        };
        let mut views = app.views.write().unwrap();
        if !views.get(key).is_some_and(|cur| Arc::ptr_eq(cur, &v)) {
            continue;
        }
        let ev = ServerEvent::ViewInvalidated {
            view_id: s.view.id().to_string(),
            layout_version: s.view.bootstrap.base.view.layout_version,
            reason: "index".into(),
            windows: (!s.hubs_changed).then_some(s.windows),
            tiles: None,
        };
        views.insert(key.to_string(), Arc::new(s.view));
        drop(views);
        if std::env::var_os("CV_DEBUG_REFRESH").is_some() {
            eprintln!("cv: settled view {key} in {:.1?}", t.elapsed());
        }
        if let Some(r) = review {
            r.emit(ev.clone());
        }
        return Some(ev);
    }
    None
}

/// A copy of `old` at `commit`: changed files re-read and re-parsed, their windows kept at the
/// same places (same ids and positions; spans re-chunked, heights follow the new line counts).
/// Returns the new view and the ids of the windows whose text changed, were removed (deleted
/// files) or were added (new files, see [`crate::incremental`]).
pub fn refresh_view(
    old: &View,
    reg: &cv_lang::Registry,
    repo: &JjRepo,
    commit: &CommitInfo,
    op: &str,
) -> Result<(View, Vec<String>), String> {
    let t0 = std::time::Instant::now();
    let mut v = old.clone();
    if std::env::var_os("CV_DEBUG_REFRESH").is_some() {
        eprintln!("  refresh: clone {:.1?}", t0.elapsed());
    }
    let tree: HashMap<String, String> = repo
        .list_tree(&commit.commit)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|e| (e.path, e.blob))
        .collect();
    let mut changed = Vec::new();
    for fi in 0..v.files.len() {
        let path = v.files[fi].path.clone();
        let Some(blob) = tree.get(&path).cloned() else {
            continue;
        };
        if blob == v.files[fi].blob {
            continue;
        }
        let bytes = repo.read_blob(&path, &blob).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        changed.extend(replace_file(&mut v, fi, reg, Some(repo.root()), blob, text));
    }
    // Deleted and new files (§4.1): windows leave or join without a full index.
    let gone: std::collections::HashSet<usize> = (0..v.files.len())
        .filter(|&fi| !tree.contains_key(&v.files[fi].path))
        .collect();
    changed.extend(crate::incremental::remove_files(&mut v, &gone));
    let new = new_files(repo, reg, commit, &v, &tree);
    changed.extend(crate::incremental::add_files(
        &mut v,
        reg,
        Some(repo.root()),
        commit,
        new,
    ));
    // Links into new files, re-resolved refs and connectedness (§4.1); channel links and heat
    // follow in the background ([`settle_in_background`]).
    if !changed.is_empty() {
        let dbg = std::env::var_os("CV_DEBUG_REFRESH").is_some();
        let mut t = std::time::Instant::now();
        let mut lap = |what: &str| {
            if dbg {
                eprintln!("  refresh: {what} {:.1?}", t.elapsed());
            }
            t = std::time::Instant::now();
        };
        lap("structure");
        let facts = crate::incremental::view_facts(&v, reg, repo.root());
        lap("facts");
        let scip = crate::index::scip_index(repo, &commit.commit).map(|(_, ix)| ix);
        lap("scip");
        let mut touched = crate::incremental::relink_code(&mut v, reg, &facts, scip.as_ref());
        lap("code links");
        let cuts = crate::grades::grade_config(repo, &commit.commit).conn;
        touched.extend(crate::incremental::reconnect(&mut v, reg, &facts, cuts));
        lap("connectedness");
        let known: std::collections::HashSet<String> = changed.iter().cloned().collect();
        let mut more: Vec<String> = touched
            .into_iter()
            .map(|w| v.windows[w].header.id.clone())
            .filter(|id| !known.contains(id))
            .collect();
        more.sort();
        changed.extend(more);
    }
    for w in v.windows.iter_mut() {
        w.header.rev = commit.commit.clone();
        w.header.change = commit.change.clone();
    }
    // Ports must stay inside their windows' spans.
    finish_refresh(&mut v, old, !changed.is_empty());
    v.bootstrap.base.view.resolved = Resolved {
        op: op.to_string(),
        head: commit.commit.clone(),
        commits: vec![commit.commit.clone()],
    };
    Ok((v, changed))
}

/// Replaces file `fi` of `v` with `text` under `blob`: re-parsed, its windows kept at the same
/// places (same ids; spans re-chunked when the window count matches, else clamped; heights follow
/// the new line count). The old record stays readable by blob id. Returns the window ids.
/// Callers finish with [`finish_refresh`].
pub fn replace_file(
    v: &mut View,
    fi: usize,
    reg: &cv_lang::Registry,
    root: Option<&std::path::Path>,
    blob: String,
    text: String,
) -> Vec<String> {
    let path = v.files[fi].path.clone();
    let mut changed = Vec::new();
    // Committed blobs go through the parse cache (blob × pack version, index.db); live overlay
    // text (`root` None) is parsed directly, as it is one-off.
    let (pack, facts) = crate::parse_cache::parse_text(reg, root, &path, &blob, &text);
    let lang = match pack {
        Some(i) => reg.pack(i).lang(&path),
        None => v.files[fi].lang,
    };
    let wins = v.files[fi].windows.clone();
    let lane = v.files[fi].lane;
    let mut rec =
        crate::incremental::file_record(path.clone(), blob.clone(), lang, lane, text, &facts);
    rec.windows = wins.clone();
    let n = rec.line_count();
    let old_rec = std::mem::replace(&mut v.files[fi], rec);
    if v.by_blob.get(&old_rec.blob) == Some(&fi) {
        v.by_blob.remove(&old_rec.blob);
    }
    v.retired.insert(old_rec.blob.clone(), Arc::new(old_rec));
    v.by_blob.insert(blob.clone(), fi);
    // Window spans: the new chunking when it has as many windows, else the old spans clamped.
    let mut specs = chunk::chunk(n, &facts.symbols);
    chunk::assign_keys(&mut specs, &facts.symbols);
    let spans: Vec<(u32, u32)> = if specs.len() == wins.len() {
        specs.iter().map(|s| (s.start, s.end)).collect()
    } else {
        let k = wins.len();
        wins.iter()
            .enumerate()
            .map(|(i, &w)| {
                let l = &v.windows[w].header.lines;
                let s = l.start.min(n.max(1));
                let e = if i + 1 == k {
                    n.max(s)
                } else {
                    l.end.clamp(s, n.max(s))
                };
                (s, e)
            })
            .collect()
    };
    for (&w, &(s, e)) in wins.iter().zip(&spans) {
        let h = &mut v.windows[w].header;
        h.blob = blob.clone();
        h.lines = LineSpan {
            start: s,
            end: e,
            file_lines: n,
        };
        h.rect.h = HEADER_WU + (e - s + 1) as f64 * LINE_WU;
        // Re-chunked at its normal span: no longer expanded.
        h.expanded_from = None;
        changed.push(h.id.clone());
    }
    for &w in &wins {
        v.expansions.remove(&v.windows[w].header.id);
    }
    changed
}

/// After [`replace_file`] (and [`crate::incremental::add_files`]): a window that grew or is new
/// since `before` and now overlaps another window takes the nearest free spot of its cluster
/// ([`resolve_overlaps`]; nothing else moves), ports stay inside their windows' spans, and the
/// tile grid is rebuilt when anything changed.
pub fn finish_refresh(v: &mut View, before: &View, changed: bool) {
    if changed {
        resolve_overlaps(v, before);
    }
    let spans: Vec<(u32, u32)> = v
        .windows
        .iter()
        .map(|w| (w.header.lines.start, w.header.lines.end))
        .collect();
    for l in v.links.iter_mut() {
        l.from_line = l.from_line.clamp(spans[l.from].0, spans[l.from].1);
        l.to_line = l.to_line.clamp(spans[l.to].0, spans[l.to].1);
    }
    if changed {
        v.tiles = crate::index::grid_index(&v.windows, &v.links);
    }
}

/// Overlap repair for in-place refreshes (DESIGN §4.10.6). A refresh keeps every window where it
/// was, but a window whose text grew gets taller and may now cover its neighbour, and a new
/// file's windows were placed before the heights changed. Each such window (taller than in
/// `before`, or not in it) that overlaps another one moves to the nearest free spot
/// ([`cv_layout::free_spot`]): inside its cluster's frame when there is room, keeping the layout's
/// window gap, else just outside it (the cluster frame, lane band and world then grow to hold
/// it), where the grown frame does not run into a neighbouring cluster's frame when any nearby
/// spot allows that. Windows that did not grow never move. Grown windows go first, in window
/// order, then new ones. Returns the number of windows moved.
pub fn resolve_overlaps(v: &mut View, before: &View) -> usize {
    const CELL: f64 = cv_contracts::TILE_BASE_WU;
    let cell = |x: f64| (x / CELL).floor() as i64;
    let mut grown: Vec<usize> = Vec::new();
    let mut fresh: Vec<usize> = Vec::new();
    for (i, w) in v.windows.iter().enumerate() {
        match before.by_id.get(&w.header.id) {
            Some(&b) => {
                if before.windows.get(b).map(|o| o.header.rect.h) < Some(w.header.rect.h) {
                    grown.push(i);
                }
            }
            None => fresh.push(i),
        }
    }
    if grown.is_empty() && fresh.is_empty() {
        return 0;
    }
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    let add = |grid: &mut HashMap<(i64, i64), Vec<usize>>, i: usize, r: &cv_contracts::Rect| {
        for tx in cell(r.x)..=cell(r.x + r.w) {
            for ty in cell(r.y)..=cell(r.y + r.h) {
                grid.entry((tx, ty)).or_default().push(i);
            }
        }
    };
    for (i, w) in v.windows.iter().enumerate() {
        add(&mut grid, i, &w.header.rect);
    }
    // Windows whose rects intersect `r` (other than `skip`).
    let hits = |grid: &HashMap<(i64, i64), Vec<usize>>,
                windows: &[crate::index::Win],
                r: &cv_contracts::Rect,
                skip: usize| {
        let mut out: Vec<usize> = Vec::new();
        for tx in cell(r.x)..=cell(r.x + r.w) {
            for ty in cell(r.y)..=cell(r.y + r.h) {
                for &j in grid.get(&(tx, ty)).map(Vec::as_slice).unwrap_or(&[]) {
                    if j != skip && windows[j].header.rect.intersects(r) {
                        out.push(j);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    };
    let cluster_ix: HashMap<String, usize> = v
        .bootstrap
        .base
        .clusters
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.clone(), i))
        .collect();
    let (pad, title, gap) = (
        cv_layout::CLUSTER_PAD,
        cv_layout::CLUSTER_TITLE,
        cv_layout::WIN_GAP,
    );
    let mut moved = 0;
    for i in grown.into_iter().chain(fresh) {
        let r = v.windows[i].header.rect;
        if hits(&grid, &v.windows, &r, i).is_empty() {
            continue;
        }
        let frame = cluster_ix
            .get(&v.windows[i].header.cluster)
            .map(|&c| v.bootstrap.base.clusters[c].rect)
            .unwrap_or(r);
        let min = ((frame.x + pad).min(r.x), (frame.y + title).min(r.y));
        let room = (frame.x + frame.w - pad, frame.y + frame.h - pad);
        // Neighbourhood: the windows around the frame; grows with any window a spot still hits.
        let reach = frame.w.max(frame.h).max(r.w + r.h) + 4.0 * gap;
        let area = cv_contracts::Rect {
            x: frame.x.min(r.x) - reach,
            y: frame.y.min(r.y) - reach,
            w: frame.w.max(r.w) + 2.0 * reach,
            h: frame.h.max(r.h) + 2.0 * reach,
        };
        let mut near: Vec<usize> = hits(&grid, &v.windows, &area, i);
        // Neighbouring cluster frames (that did not already overlap this one): a spot outside
        // the frame grows it, and the grown frame must not run into them. They are obstacles
        // (grown by the frame padding), and a spot whose grown frame still covers one (the frame
        // grows as a box) is set aside for the next best; after `TRIES` the first spot is kept.
        const TRIES: usize = 48;
        let own = cluster_ix.get(&v.windows[i].header.cluster).copied();
        let frames: Vec<cv_contracts::Rect> = v
            .bootstrap
            .base
            .clusters
            .iter()
            .enumerate()
            .filter(|&(c, k)| {
                Some(c) != own
                    && k.rect.w > 0.0
                    && k.rect.intersects(&area)
                    && !k.rect.intersects(&frame)
            })
            .map(|(_, k)| k.rect)
            .collect();
        let walls: Vec<cv_contracts::Rect> = frames
            .iter()
            .map(|f| cv_contracts::Rect {
                x: f.x - pad,
                y: f.y - pad,
                w: f.w + 2.0 * pad,
                h: f.h + 2.0 * pad,
            })
            .collect();
        let mut set_aside: Vec<cv_contracts::Rect> = Vec::new();
        let mut first: Option<cv_contracts::Rect> = None;
        let spot = loop {
            let placed: Vec<cv_contracts::Rect> = near
                .iter()
                .map(|&j| v.windows[j].header.rect)
                .chain(walls.iter().copied())
                .chain(set_aside.iter().copied())
                .collect();
            let (x, y) =
                cv_layout::free_spot(&placed, r.w, r.h, (r.x, r.y), min, f64::INFINITY, room, gap);
            let cand = cv_contracts::Rect { x, y, ..r };
            let more: Vec<usize> = hits(&grid, &v.windows, &cand, i)
                .into_iter()
                .filter(|j| !near.contains(j))
                .collect();
            if !more.is_empty() {
                near.extend(more);
                continue;
            }
            let grown = grown_frame(&frame, &cand, pad);
            if !frames.iter().any(|f| f.intersects(&grown)) {
                break cand;
            }
            let first = *first.get_or_insert(cand);
            if set_aside.len() >= TRIES {
                break first;
            }
            set_aside.push(cand);
        };
        v.windows[i].header.rect = spot;
        for tx in cell(r.x)..=cell(r.x + r.w) {
            for ty in cell(r.y)..=cell(r.y + r.h) {
                if let Some(c) = grid.get_mut(&(tx, ty)) {
                    c.retain(|&j| j != i);
                }
            }
        }
        add(&mut grid, i, &spot);
        moved += 1;
        // The frames that hold the window grow with it.
        let base = &mut v.bootstrap.base;
        let lane = v.windows[i].header.lane.clone();
        let grow = |a: &mut cv_contracts::Rect| *a = grown_frame(a, &spot, pad);
        if let Some(&c) = cluster_ix.get(&v.windows[i].header.cluster) {
            grow(&mut base.clusters[c].rect);
        }
        if let Some(band) = base.lanes.iter_mut().find(|b| b.lane == lane) {
            grow(&mut band.rect);
        }
        grow(&mut base.world);
    }
    moved
}

/// A frame grown to hold `spot` (with `pad` right of and below it), as [`resolve_overlaps`] grows
/// cluster frames, lane bands and the world.
pub fn grown_frame(
    a: &cv_contracts::Rect,
    spot: &cv_contracts::Rect,
    pad: f64,
) -> cv_contracts::Rect {
    let (x0, y0) = (a.x.min(spot.x), a.y.min(spot.y));
    let (x1, y1) = (
        (a.x + a.w).max(spot.x + spot.w + pad),
        (a.y + a.h).max(spot.y + spot.h + pad),
    );
    cv_contracts::Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// Files of `tree` the view lacks, in a lane the view shows (classified with the revision's lane
/// config; binary, oversized and hidden files are skipped, as in a full index).
fn new_files(
    repo: &JjRepo,
    reg: &cv_lang::Registry,
    commit: &CommitInfo,
    v: &View,
    tree: &HashMap<String, String>,
) -> Vec<crate::incremental::NewFile> {
    // A view with a fileset only ever gains files it matches.
    let fileset = crate::routes::spec_fileset(&v.bootstrap.base.view.spec);
    let mut paths: Vec<(&String, &String)> = tree
        .iter()
        .filter(|(p, _)| !v.by_path.contains_key(p.as_str()) && fileset.matches(p))
        .collect();
    if paths.is_empty() {
        return vec![];
    }
    paths.sort();
    let cfg = crate::index::lane_config(repo, &commit.commit);
    let classifier = cv_lang::lanes::LaneClassifier::with_config(reg, &cfg);
    let shown: Vec<&str> = v
        .bootstrap
        .base
        .lanes
        .iter()
        .map(|b| b.lane.as_str())
        .collect();
    paths
        .into_iter()
        .filter_map(|(path, blob)| {
            if cfg.is_hidden(classifier.classify(path, b"")) {
                return None;
            }
            let bytes = repo.read_blob(path, blob).ok()?;
            if bytes.len() > crate::index::MAX_FILE_BYTES
                || bytes[..bytes.len().min(8000)].contains(&0)
            {
                return None;
            }
            let lane = classifier.classify(path, &bytes);
            if !shown.contains(&lane) {
                return None;
            }
            Some(crate::incremental::NewFile {
                path: path.clone(),
                blob: blob.clone(),
                lane,
                text: String::from_utf8_lossy(&bytes).into_owned(),
            })
        })
        .collect()
}

/// The current state of a view (tests, cv-mcp): refreshed views replace the stored `Arc`.
pub fn view_snapshot(state: &AppState, view_id: &str) -> Option<Arc<View>> {
    state.views.read().unwrap().get(view_id).cloned()
}
