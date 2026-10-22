//! Agent-facing routes (docs/contracts/http.ts "agent"): attention, workspace binding, the human's
//! view state, focus requests, tours and per-session permissions (DESIGN §8, §10). cv-mcp calls
//! these; the web client shows what they emit.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use cv_contracts::api::{AgentPermissions, AgentViewState, Tour, TourStep, ViewState};
use cv_contracts::review::{Actor, ServerEvent};
use cv_review::ReviewError;
use serde_json::{Value, json};

use crate::review_api::{RErr, actor};
use crate::routes::AppState;

type St = State<Arc<AppState>>;
type R<T> = Result<T, RErr>;

/// ISO-8601 (ids.ts).
fn now() -> String {
    cv_store::now_iso()
}

// ── Per-session permissions (DESIGN §10) ──
//
// Keyed by the UI session and the agent, stored in review.db (`agent_permission`) so they
// survive restarts. The UI session is the id the UI sends as the `X-CV-Session` header (or
// `?session=`): a stable id it keeps per browser profile, not the WebSocket connection, which
// changes on every reconnect. Without one the session is "default". The agent is the `agent`
// field / query (an agent name); without one the toggles apply to every agent ("*"), and an
// agent-specific row wins over "*" within one session.
//
// Effective for an agent across UI sessions: it may edit only while no session has switched
// editing off for it (the safe direction); it may move the camera when any session allows it.

// Expiry (N9): a session's toggles are in force only while the session is present: it has a
// WebSocket open (`/ws?session=`), or it was seen (a WebSocket, a permissions call naming it) or
// changed its toggles less than the TTL ago (`CV_SESSION_TTL_SECS`, default 600 s). The row's
// `updated_at` is the floor of "last seen", so after a restart a session nobody reopens lapses
// one TTL after its last change. Lapsed rows are kept, not deleted: a session that comes back
// is in force again, with the toggles it shows. A forgotten tab never blocks the agent forever.

/// How long a UI session without an open WebSocket keeps its toggles in force.
pub(crate) fn session_ttl() -> std::time::Duration {
    std::env::var("CV_SESSION_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(std::time::Duration::from_secs(600))
}

#[derive(Default)]
struct Presence {
    open: u32,
    seen_ms: i64,
}

fn presence() -> &'static std::sync::Mutex<std::collections::HashMap<String, Presence>> {
    static P: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Presence>>> =
        std::sync::OnceLock::new();
    P.get_or_init(Default::default)
}

/// How far tests have moved the session clock forward ([`advance_session_clock`]).
static CLOCK_SKEW_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Tests: moves the clock that session expiry reads forward by `d` (for this process), instead of
/// sleeping through a TTL.
#[doc(hidden)]
pub fn advance_session_clock(d: std::time::Duration) {
    CLOCK_SKEW_MS.fetch_add(d.as_millis() as i64, std::sync::atomic::Ordering::Relaxed);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
        + CLOCK_SKEW_MS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Milliseconds since the epoch of an ISO-8601 UTC time as `cv_store::now_iso` writes it.
fn ms_from_iso(s: &str) -> Option<i64> {
    let n = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hh, mm, ss) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let ms = if s.as_bytes().get(19) == Some(&b'.') {
        n(20, 23).unwrap_or(0)
    } else {
        0
    };
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000) + ms)
}

/// Marks a UI session as seen now.
pub(crate) fn touch_session(session: &str) {
    presence()
        .lock()
        .unwrap()
        .entry(session.to_string())
        .or_default()
        .seen_ms = now_ms();
}

/// Held while a UI session's WebSocket is open; on drop the session counts as seen then.
pub(crate) struct OpenSession(String);

pub(crate) fn open_session(session: &str) -> OpenSession {
    let mut p = presence().lock().unwrap();
    let e = p.entry(session.to_string()).or_default();
    e.open += 1;
    e.seen_ms = now_ms();
    OpenSession(session.to_string())
}

impl Drop for OpenSession {
    fn drop(&mut self) {
        if let Some(e) = presence().lock().unwrap().get_mut(&self.0) {
            e.open = e.open.saturating_sub(1);
            e.seen_ms = now_ms();
        }
    }
}

/// Whether `session`'s toggles are in force (see the expiry notes above); `changed` is the
/// newest `updated_at` of its rows.
fn session_present(session: &str, changed: Option<&str>) -> bool {
    let (open, seen) = presence()
        .lock()
        .unwrap()
        .get(session)
        .map(|p| (p.open, p.seen_ms))
        .unwrap_or((0, 0));
    let last = seen.max(changed.and_then(ms_from_iso).unwrap_or(0));
    in_force(open, last, now_ms(), session_ttl().as_millis() as i64)
}

/// A session with an open socket, or seen less than `ttl_ms` before `now`.
fn in_force(open: u32, last_seen_ms: i64, now: i64, ttl_ms: i64) -> bool {
    open > 0 || now - last_seen_ms < ttl_ms
}

#[cfg(test)]
mod expiry_tests {
    use super::*;

    #[test]
    fn iso_times_round_trip() {
        for ms in [0, 1_700_000_000_123, 1_791_590_745_007, 951_782_400_000] {
            assert_eq!(ms_from_iso(&cv_store::iso_from_ms(ms)), Some(ms));
        }
        assert_eq!(ms_from_iso("garbage"), None);
    }

    #[test]
    fn sessions_lapse_after_the_ttl_unless_a_socket_is_open() {
        let ttl = 600_000;
        assert!(in_force(0, 1_000, 1_000 + ttl - 1, ttl));
        assert!(!in_force(0, 1_000, 1_000 + ttl, ttl));
        assert!(
            in_force(1, 0, i64::MAX / 2, ttl),
            "an open socket never lapses"
        );
        // The guard: open while held, seen when dropped.
        let s = "expiry-test-session";
        let before = now_ms();
        let g = open_session(s);
        assert_eq!(presence().lock().unwrap()[s].open, 1);
        drop(g);
        let p = presence().lock().unwrap();
        assert_eq!(p[s].open, 0);
        assert!(p[s].seen_ms >= before, "seen when dropped");
    }
}

/// The UI session when the client names none.
pub(crate) const DEFAULT_UI_SESSION: &str = "default";
/// The agent key that covers every agent.
pub(crate) const ANY_AGENT: &str = "*";

const DEFAULT_EDIT: bool = true;
const DEFAULT_MOVE_CAMERA: bool = false;

/// `X-CV-Session`, else `?session=`, else "default".
pub(crate) fn ui_session(headers: &HeaderMap, query: Option<&str>) -> String {
    headers
        .get("x-cv-session")
        .and_then(|v| v.to_str().ok())
        .or(query)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_UI_SESSION)
        .to_string()
}

/// (edit, moveCamera) one UI session chose for `agent` (None = for every agent).
fn session_toggles(
    rows: &[cv_store::PermissionRow],
    session: &str,
    agent: Option<&str>,
) -> (bool, bool) {
    let row = |a: &str| {
        rows.iter()
            .find(|r| r.ui_session == session && r.agent == a)
    };
    let specific = agent.filter(|a| *a != ANY_AGENT).and_then(row);
    let any = row(ANY_AGENT);
    let edit = specific
        .and_then(|r| r.edit)
        .or(any.and_then(|r| r.edit))
        .unwrap_or(DEFAULT_EDIT);
    let mv = specific
        .and_then(|r| r.move_camera)
        .or(any.and_then(|r| r.move_camera))
        .unwrap_or(DEFAULT_MOVE_CAMERA);
    (edit, mv)
}

fn permission_rows(app: &Arc<AppState>) -> R<Vec<cv_store::PermissionRow>> {
    review(app)?.db().permissions().map_err(|e| {
        RErr(ReviewError {
            status: 500,
            code: "store-error",
            message: e.0,
        })
    })
}

/// (edit, moveCamera) for `agent` across every UI session (see the module notes).
pub(crate) fn effective(app: &Arc<AppState>, agent: Option<&str>) -> R<(bool, bool)> {
    let rows = permission_rows(app)?;
    // Rows come newest first: the first row of a session carries its last change.
    let mut changed: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for r in &rows {
        changed
            .entry(r.ui_session.as_str())
            .or_insert(r.updated_at.as_str());
    }
    let sessions: std::collections::BTreeSet<&str> = changed
        .iter()
        .filter(|(s, at)| session_present(s, Some(at)))
        .map(|(s, _)| *s)
        .collect();
    if sessions.is_empty() {
        return Ok((DEFAULT_EDIT, DEFAULT_MOVE_CAMERA));
    }
    let per: Vec<(bool, bool)> = sessions
        .iter()
        .map(|s| session_toggles(&rows, s, agent))
        .collect();
    Ok((per.iter().all(|(e, _)| *e), per.iter().any(|(_, m)| *m)))
}

/// Whether the human lets `agent` (None = agents in general) move the camera.
pub(crate) fn may_move_camera(app: &Arc<AppState>, agent: Option<&str>) -> bool {
    effective(app, agent)
        .map(|(_, m)| m)
        .unwrap_or(DEFAULT_MOVE_CAMERA)
}

/// Agent writes (edits, apply, undo, resolve) are refused (403) while a UI session has switched
/// editing off for that agent.
pub(crate) fn check_edit(app: &Arc<AppState>, who: &Actor) -> R<()> {
    let Some(name) = who.agent_name() else {
        return Ok(());
    };
    if !effective(app, Some(name))?.0 {
        return Err(RErr(ReviewError {
            status: 403,
            code: "agent-edit-disabled",
            message: "The human has switched agent editing off for this session; propose the change in the thread instead.".into(),
        }));
    }
    Ok(())
}

fn review(app: &Arc<AppState>) -> R<Arc<cv_review::Review>> {
    Ok(app.review()?)
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/agent/attention", post(attention))
        .route("/api/v1/agent/bind", post(bind))
        .route("/api/v1/agent/view-state", get(view_state))
        .route("/api/v1/agent/focus", post(focus))
        .route("/api/v1/agent/tours", post(create_tour))
        .route("/api/v1/tours", get(list_tours))
        .route(
            "/api/v1/agent/permissions",
            get(get_permissions).put(put_permissions),
        )
}

async fn attention(State(app): St, Json(b): Json<Value>) -> R<Json<Value>> {
    Ok(Json(attention_of(&app, &b)?))
}

/// POST /agent/attention (also the in-process MCP backend).
pub(crate) fn attention_of(app: &Arc<AppState>, b: &Value) -> R<Value> {
    crate::workspaces::note_attention(
        app,
        b["agent"].as_str().unwrap_or("agent"),
        b["tool"].as_str().unwrap_or(""),
    );
    let ev = ServerEvent::AgentAttention {
        agent: b["agent"].as_str().unwrap_or("agent").into(),
        workspace: b["workspace"].as_str().map(Into::into),
        tool: b["tool"].as_str().unwrap_or("").into(),
        windows: serde_json::from_value(b["windows"].clone()).unwrap_or_default(),
        note: b["note"].as_str().map(Into::into),
        at: b["at"].as_str().map(String::from).unwrap_or_else(now),
    };
    let cursor = review(app)?.emit(ev);
    Ok(json!({ "ok": true, "cursor": cursor.to_string() }))
}

/// MCP bind_workspace: records the session (review.db) and emits workspace.updated.
async fn bind(State(app): St, Json(b): Json<Value>) -> R<Json<Value>> {
    Ok(Json(bind_of(&app, &b).await?))
}

/// POST /agent/bind (also the in-process MCP backend). 404 for a workspace the repo doesn't have.
pub(crate) async fn bind_of(app: &Arc<AppState>, b: &Value) -> R<Value> {
    let agent = b["agent"].as_str().unwrap_or("agent").to_string();
    let workspace = b["workspace"].as_str().unwrap_or("default").to_string();
    let who: Actor = serde_json::from_value(
        json!({ "kind": "agent", "name": agent, "client": b["client"], "workspace": workspace }),
    )
    .unwrap_or(Actor::human("you"));
    let r = review(app)?;
    let (a, w) = (agent.clone(), workspace.clone());
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        let known = r.vcs().workspaces().map_err(|e| ReviewError {
            status: 500,
            code: "jj-error",
            message: e.to_string(),
        })?;
        if !known.iter().any(|k| k.name == w) {
            let names: Vec<&str> = known.iter().map(|k| k.name.as_str()).collect();
            return Err(ReviewError {
                status: 404,
                code: "not-found",
                message: format!(
                    "no workspace {w:?} in this repo (workspaces: {})",
                    names.join(", ")
                ),
            });
        }
        crate::workspaces::note_attention(&app2, &a, "bind_workspace");
        r.bind_session(&a, &w, &who, "reading")?;
        // http.ts: the bound workspace's full WorkspaceSummary card, as GET /workspaces shows it.
        let repo = app2.head_repo().map_err(|e| ReviewError {
            status: 500,
            code: "jj-error",
            message: e.message,
        })?;
        let (_, cards) = crate::workspaces::cards_now(&app2, &repo);
        cards
            .as_array()
            .and_then(|c| c.iter().find(|c| c["name"] == w.as_str()).cloned())
            .ok_or_else(|| ReviewError {
                status: 404,
                code: "not-found",
                message: format!("no workspace {w:?} in this repo"),
            })
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

async fn view_state(State(app): St) -> R<Json<AgentViewState>> {
    Ok(Json(view_state_of(&app)?))
}

/// GET /agent/view-state (also the in-process MCP backend): the latest `view.state` a UI sent,
/// with the ViewRef of its view (snapshot or range). `{ view: null }` without a UI, or when the
/// state does not parse as a ViewState or names a view this server no longer has.
pub(crate) fn view_state_of(app: &Arc<AppState>) -> R<AgentViewState> {
    let r = review(app)?;
    let Some((_, _, raw)) = r.view_state(None) else {
        return Ok(AgentViewState::None { view: () });
    };
    let Ok(state) = serde_json::from_value::<ViewState>(raw) else {
        return Ok(AgentViewState::None { view: () });
    };
    let view = app
        .views
        .read()
        .unwrap()
        .get(&state.view_id)
        .map(|v| v.bootstrap.base.view.clone())
        .or_else(|| {
            app.read
                .range(&state.view_id)
                .and_then(|rv| serde_json::from_value(rv.view_ref.clone()).ok())
        });
    Ok(match view {
        Some(view) => AgentViewState::Shown {
            view: Box::new(view),
            state: Box::new(state),
        },
        None => AgentViewState::None { view: () },
    })
}

async fn focus(State(app): St, headers: HeaderMap, Json(b): Json<Value>) -> R<Json<Value>> {
    Ok(Json(focus_of(&app, actor(&headers), &b)?))
}

/// POST /agent/focus (also the in-process MCP backend).
pub(crate) fn focus_of(app: &Arc<AppState>, by: Actor, b: &Value) -> R<Value> {
    let ev = ServerEvent::FocusRequest {
        by,
        target: serde_json::from_value(b["target"].clone()).map_err(|e| {
            RErr(ReviewError {
                status: 400,
                code: "invalid",
                message: e.to_string(),
            })
        })?,
        lines: serde_json::from_value(b["lines"].clone()).ok(),
        note: b["note"].as_str().map(Into::into),
    };
    let agent = match &ev {
        ServerEvent::FocusRequest { by, .. } => by.agent_name().map(String::from),
        _ => None,
    };
    let r = review(app)?;
    r.emit(ev);
    // "moved" only when the agent may move the camera and a connected UI follows it (its
    // view.state has followAgent: true); otherwise the UI shows it as a suggestion.
    let moves = may_move_camera(app, agent.as_deref()) && r.following();
    Ok(json!({ "delivered": if moves { "moved" } else { "suggested" } }))
}

async fn create_tour(State(app): St, headers: HeaderMap, Json(b): Json<Value>) -> R<Json<Value>> {
    Ok(Json(create_tour_of(&app, actor(&headers), &b)?))
}

/// POST /agent/tours (also the in-process MCP backend).
/// Tours are stored in review.db (`tour`, `tour_step`) and survive restarts.
pub(crate) fn create_tour_of(app: &Arc<AppState>, by: Actor, b: &Value) -> R<Value> {
    let r = review(app)?;
    let store = |e: cv_store::Error| {
        RErr(ReviewError {
            status: 500,
            code: "store-error",
            message: e.0,
        })
    };
    let existing = r.db().tours().map_err(store)?;
    let mut n = existing.len() + 1;
    while existing
        .iter()
        .any(|t| t["id"] == format!("tour{n}").as_str())
    {
        n += 1;
    }
    let id = format!("tour{n}");
    let steps: Vec<TourStep> = serde_json::from_value(b.get("steps").cloned().unwrap_or(json!([])))
        .map_err(|e| {
            RErr(ReviewError {
                status: 400,
                code: "invalid",
                message: format!("steps: {e}"),
            })
        })?;
    let tour = Tour {
        id: id.clone(),
        title: b["title"].as_str().unwrap_or("Tour").to_string(),
        by,
        created_at: cv_store::now_iso(),
        steps,
    };
    let stored: Vec<Value> = tour
        .steps
        .iter()
        .map(|s| serde_json::to_value(s).unwrap_or_default())
        .collect();
    r.db()
        .put_tour(&id, &tour.title, &tour.by, &tour.created_at, &stored)
        .map_err(store)?;
    r.emit(ServerEvent::TourCreated { tour });
    Ok(json!({ "tour": id }))
}

async fn list_tours(State(app): St) -> R<Json<Vec<Tour>>> {
    let r = review(&app)?;
    let list = r.db().tours().map_err(|e| {
        RErr(ReviewError {
            status: 500,
            code: "store-error",
            message: e.0,
        })
    })?;
    // Rows written before steps were validated may not parse; they are skipped.
    Ok(Json(
        list.into_iter()
            .filter_map(|t| serde_json::from_value(t).ok())
            .collect(),
    ))
}

#[derive(serde::Deserialize, Default)]
struct PermQ {
    session: Option<String>,
    agent: Option<String>,
}

/// GET /agent/permissions: a UI gets its session's toggles (for `?agent=`, default every
/// agent); an agent (X-CV-Agent) without `?session=` gets what is in force for it.
async fn get_permissions(
    State(app): St,
    headers: HeaderMap,
    Query(q): Query<PermQ>,
) -> R<Json<AgentPermissions>> {
    let who = actor(&headers);
    let explicit_session =
        headers.contains_key("x-cv-session") || q.session.as_deref().is_some_and(|s| !s.is_empty());
    if let (Some(name), false) = (who.agent_name(), explicit_session) {
        let (edit, mv) = effective(&app, Some(name))?;
        return Ok(Json(AgentPermissions {
            edit,
            move_camera: mv,
            session: None,
            agent: Some(name.to_string()),
            effective: Some(true),
        }));
    }
    let session = ui_session(&headers, q.session.as_deref());
    touch_session(&session);
    let agent = q.agent.filter(|a| !a.is_empty());
    let rows = permission_rows(&app)?;
    let (edit, mv) = session_toggles(&rows, &session, agent.as_deref());
    Ok(Json(AgentPermissions {
        edit,
        move_camera: mv,
        session: Some(session),
        agent: Some(agent.unwrap_or_else(|| ANY_AGENT.to_string())),
        effective: None,
    }))
}

/// PUT /agent/permissions body (http.ts: `Partial<AgentPermissions> & { session?, agent? }`).
/// Unknown fields are a 400: a misspelt toggle must not silently change nothing.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PermBody {
    pub edit: Option<bool>,
    pub move_camera: Option<bool>,
    pub session: Option<String>,
    pub agent: Option<String>,
    /// Read-only (GET answers it); accepted so a GET result can be PUT back, and ignored.
    #[allow(dead_code)]
    pub effective: Option<bool>,
}

/// PUT /agent/permissions: only the human sets them (an agent gets 403 "not-allowed").
async fn put_permissions(
    State(app): St,
    headers: HeaderMap,
    Query(q): Query<PermQ>,
    Json(b): Json<Value>,
) -> R<Json<AgentPermissions>> {
    if actor(&headers).agent_name().is_some() {
        return Err(RErr(ReviewError {
            status: 403,
            code: "not-allowed",
            message: "Only the human changes agent permissions.".into(),
        }));
    }
    let b: PermBody = serde_json::from_value(b).map_err(|e| {
        RErr(ReviewError {
            status: 400,
            code: "invalid",
            message: format!("agent permissions: {e}"),
        })
    })?;
    let session = ui_session(&headers, q.session.as_deref().or(b.session.as_deref()));
    let agent = q
        .agent
        .or_else(|| b.agent.clone())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| ANY_AGENT.to_string());
    touch_session(&session);
    let r = review(&app)?;
    r.db()
        .put_permission(&session, &agent, b.edit, b.move_camera)
        .map_err(|e| {
            RErr(ReviewError {
                status: 500,
                code: "store-error",
                message: e.0,
            })
        })?;
    let rows = permission_rows(&app)?;
    let (edit, mv) = session_toggles(&rows, &session, Some(&agent));
    let out = AgentPermissions {
        edit,
        move_camera: mv,
        session: Some(session),
        agent: Some(agent),
        effective: None,
    };
    r.emit(ServerEvent::AgentPermissions {
        permissions: out.clone(),
    });
    Ok(Json(out))
}

/// Workspace cards show the agent bound to each workspace (from review.db sessions).
pub(crate) fn fill_agents(app: &Arc<AppState>, cards: &mut Value) {
    let Ok(r) = app.review() else { return };
    let Ok(sessions) = r.db().sessions() else {
        return;
    };
    let Some(list) = cards.as_array_mut() else {
        return;
    };
    for card in list {
        if let Some(s) = sessions
            .iter()
            .find(|s| card["name"] == s.workspace.as_str())
        {
            let (name, client) = match &s.agent {
                Actor::Agent { name, client, .. } => (name.clone(), client.clone()),
                _ => continue,
            };
            // Activity from the agent's recent tool calls and edits (agent.attention, /edits).
            let (activity, last_seen) = match crate::workspaces::activity_of(app, &name) {
                Some((a, at)) => (a.to_string(), at.max(s.last_seen.clone())),
                None => ("idle".to_string(), s.last_seen.clone()),
            };
            card["agent"] = json!({ "name": name, "client": client, "activity": activity, "lastSeen": last_seen });
        }
    }
}
