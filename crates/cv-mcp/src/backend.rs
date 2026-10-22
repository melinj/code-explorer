//! The narrow service interface the MCP tools need (`McpBackend`), and who is calling (`AgentId`).
//!
//! Wire objects (Thread, EditResult, WindowHeader, …) travel as `serde_json::Value` in the exact JSON
//! of `docs/contracts/*.ts`, so the HTTP implementation can pass server responses through untouched
//! and an in-process implementation can serialize its own types. Three implementations exist or are
//! planned: [`crate::fake::FakeBackend`] (tests), [`crate::http::HttpBackend`] (a running cv server's
//! /api/v1) and, at integration, one on top of cv-review in-process.

use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

/// The agent behind an MCP session: attribution for writes and for `agent.attention`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AgentId {
    /// Display name, e.g. "claude" or "opencode".
    pub name: String,
    /// MCP client, e.g. "claude-code".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Workspace the session is bound to (`bind_workspace`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl AgentId {
    /// The `Actor` of ids.ts.
    pub fn actor(&self) -> Value {
        let mut a = json!({ "kind": "agent", "name": self.name });
        if let Some(c) = &self.client {
            a["client"] = json!(c);
        }
        if let Some(w) = &self.workspace {
            a["workspace"] = json!(w);
        }
        a
    }
}

/// An error from the backend, carrying the server's ApiError code when there is one.
#[derive(Clone, Debug, PartialEq)]
pub enum BackendError {
    /// The server does not serve this route (e.g. cv-mock has no GET /events). Tools fall back or
    /// report which route is missing.
    Unsupported(String),
    /// The server answered with an ApiError ({ code, message }) or a transport failure.
    Api {
        status: u16,
        code: String,
        message: String,
    },
}

impl BackendError {
    pub fn api(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        BackendError::Api {
            status,
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::api(404, "not-found", message)
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::api(400, "invalid", message)
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unsupported(route) => {
                write!(
                    f,
                    "this cv server does not serve {route} (not available yet)"
                )
            }
            BackendError::Api { code, message, .. } => write!(f, "{code}: {message}"),
        }
    }
}

impl std::error::Error for BackendError {}

pub type BResult<T> = Result<T, BackendError>;

/// Filters of GET /threads.
#[derive(Clone, Debug, Default)]
pub struct ThreadQuery {
    pub status: Vec<String>,
    pub assignee: Option<String>,
    pub revset: Option<String>,
    pub path: Option<String>,
}

/// Everything the MCP tools need from the services behind the UI. Method names follow the HTTP routes
/// of docs/contracts/http.ts; the `agent/*` methods are the additive agent routes documented there.
#[async_trait]
pub trait McpBackend: Send + Sync + 'static {
    // ── Workspaces and shared attention ──
    /// GET /workspaces → WorkspaceSummary[]
    async fn workspaces(&self) -> BResult<Vec<Value>>;
    /// POST /agent/bind { agent, client?, workspace } → WorkspaceSummary (emits workspace.updated)
    async fn bind_workspace(&self, who: &AgentId, workspace: &str) -> BResult<Value>;
    /// POST /agent/attention: append an agent.attention event to the event log.
    async fn attention(&self, event: Value) -> BResult<()>;
    /// GET /agent/view-state → { view, state } | { view: null }
    async fn view_state(&self) -> BResult<Value>;
    /// POST /agent/view { spec } → ViewRef (only when the human allows agent navigation)
    async fn set_view(&self, who: &AgentId, spec: Value) -> BResult<Value>;
    /// POST /agent/focus { target: SiteRef, lines?, note? } → { delivered }
    async fn focus(&self, who: &AgentId, body: Value) -> BResult<Value>;
    /// POST /agent/tours { title, steps } → { tour }
    async fn create_tour(&self, who: &AgentId, body: Value) -> BResult<Value>;

    // ── Views and content ──
    /// POST /views → ViewRef
    async fn create_view(&self, spec: Value) -> BResult<Value>;
    /// Every WindowHeader of a view (HTTP: the tiles covering the bootstrap world).
    async fn view_windows(&self, view: &str) -> BResult<Vec<Value>>;
    /// Hubs of a view (HTTP: from the bootstrap).
    async fn view_hubs(&self, view: &str) -> BResult<Vec<Value>>;
    /// POST /views/:view/windows:batch → WindowBatchResponse
    async fn window_batch(&self, view: &str, ids: &[String], parts: &[&str]) -> BResult<Value>;
    /// GET /blobs/:blob/text → BlobText
    async fn blob_text(&self, blob: &str, from: Option<u32>, to: Option<u32>) -> BResult<Value>;
    /// GET /views/:view/metrics → { points, cochange }
    async fn metrics(
        &self,
        view: &str,
        level: &str,
        lane: Option<&str>,
        limit: usize,
    ) -> BResult<Value>;
    /// GET /search → SearchHit[]
    async fn search(
        &self,
        q: &str,
        kinds: Option<&str>,
        view: Option<&str>,
        limit: usize,
    ) -> BResult<Vec<Value>>;
    /// GET /links → { links, hubs }
    async fn links(
        &self,
        view: &str,
        target: &str,
        dir: &str,
        kinds: Option<&str>,
        depth: u8,
    ) -> BResult<Value>;
    /// GET /hubs/:hub → Hub
    async fn hub(&self, view: &str, hub: &str) -> BResult<Value>;
    /// GET /revisions → { rows, truncated }
    async fn revisions(&self, revset: Option<&str>, limit: Option<usize>) -> BResult<Value>;
    /// GET /interdiff → { files, description? }
    async fn interdiff(&self, change: &str, from: &str) -> BResult<Value>;
    /// The cumulative diff of a range (merged parents of its root → its head) with the text of
    /// every line: { files: FileDiff[] }. Not an HTTP route: get_diff falls back to the range
    /// view's hunk windows when a backend does not serve it.
    async fn range_diff(&self, _revset: &str, _fileset: Option<&str>) -> BResult<Value> {
        Err(BackendError::Unsupported("range diff".into()))
    }
    /// `jj diff --from <from> --to <to>`: files that differ between two revisions' trees →
    /// `{ files: FileDiff[] }` (GET /diff; in-process on cv serve).
    async fn tree_diff(&self, _from: &str, _to: &str, _fileset: Option<&str>) -> BResult<Value> {
        Err(BackendError::Unsupported("two-revision diff".into()))
    }

    // ── Review ──
    /// GET /threads → Thread[]
    async fn threads(&self, q: &ThreadQuery) -> BResult<Vec<Value>>;
    /// POST /threads (NewThread) → Thread
    async fn create_thread(&self, who: &AgentId, body: Value) -> BResult<Value>;
    /// POST /threads/:id/comments → Thread
    async fn comment(
        &self,
        who: &AgentId,
        thread: &str,
        body: &str,
        set_status: Option<&str>,
    ) -> BResult<Value>;
    /// PATCH /threads/:id → Thread
    async fn patch_thread(&self, who: &AgentId, thread: &str, patch: Value) -> BResult<Value>;
    /// POST /links (declare) → Link
    async fn declare_link(&self, who: &AgentId, body: Value) -> BResult<Value>;
    /// POST /links/:id/rate → Link
    async fn rate_link(&self, who: &AgentId, link: &str, body: Value) -> BResult<Value>;
    /// PUT /clusters/:id/label → { ok: true }
    async fn label_cluster(&self, who: &AgentId, cluster: &str, body: Value) -> BResult<Value>;

    // ── Edits and conflicts ──
    /// POST /edits (EditRequest) → EditResult
    async fn edit(&self, who: &AgentId, req: Value) -> BResult<Value>;
    /// POST /edits/:id/apply → EditResult
    async fn apply_edit(&self, who: &AgentId, edit: &str) -> BResult<Value>;
    /// POST /edits/:id/undo → EditResult
    async fn undo_edit(&self, who: &AgentId, edit: &str) -> BResult<Value>;
    /// GET /conflicts → ConflictRecord[]
    async fn conflicts(&self, revset: Option<&str>) -> BResult<Vec<Value>>;
    /// GET /conflicts/:commit/*path → ConflictDetail
    async fn conflict(&self, commit: &str, path: &str) -> BResult<Value>;
    /// POST /conflicts/:commit/*path/resolve → EditResult
    async fn resolve_conflict(
        &self,
        who: &AgentId,
        commit: &str,
        path: &str,
        content: &str,
    ) -> BResult<Value>;

    // ── Events ──
    /// GET /events (long-poll) → { cursor, events }. `kinds` empty = every kind.
    async fn events(
        &self,
        cursor: Option<&str>,
        kinds: &[String],
        timeout: Duration,
    ) -> BResult<Value>;
    /// The event log's current cursor, if the backend can tell without waiting. Resource
    /// subscriptions start their watcher there, so an event right after `resources/subscribe`
    /// answers is never missed; `None` (the default) starts it at the first long-poll instead.
    async fn cursor(&self) -> BResult<Option<String>> {
        Ok(None)
    }
}
