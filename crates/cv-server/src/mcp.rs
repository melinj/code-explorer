//! B9 wiring: the MCP endpoint `cv serve` mounts at /mcp (Streamable HTTP; also /mcp/{reader,
//! reviewer,editor}) and the `cv mcp` stdio shim.
//!
//! /mcp runs the tools in-process ([`InProcess`]): reads go to [`AppState`]'s views and read side,
//! review and edits to [`cv_review::Review`], with the agent's [`Actor`] passed directly. The stdio
//! shim (`cv mcp --server URL`) stays on cv_mcp::http::HttpBackend; both end in the same functions
//! the /api/v1 handlers call, so they answer the same. Links, hubs, interdiff, declared links,
//! ratings, cluster labels and agent navigation run through [`crate::extras_api`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cv_contracts::review::{Actor, EditRequest, EventsResponse, NewThread, ServerEvent, Thread};
use cv_contracts::{TileResponse, WindowBatchRequest};
use cv_mcp::backend::{AgentId, BResult, BackendError, McpBackend, ThreadQuery};
use cv_review::{Review, ReviewError, ThreadFilter};
use serde_json::{Value, json};

use crate::review_api::RErr;
use crate::routes::{ApiErr, AppState};

/// The MCP router `cv serve` mounts, on the in-process backend.
pub fn router(state: Arc<AppState>) -> axum::Router {
    cv_mcp::router(
        Arc::new(InProcess::new(state)),
        cv_mcp::McpConfig::default(),
    )
}

/// `McpBackend` on top of the server's own state: no loopback HTTP.
pub struct InProcess {
    app: Arc<AppState>,
}

impl InProcess {
    pub fn new(app: Arc<AppState>) -> Self {
        InProcess { app }
    }

    /// Runs a blocking review call off the async runtime.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(Arc<Review>) -> Result<T, ReviewError> + Send + 'static,
    ) -> BResult<T> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || f(app.review()?))
            .await
            .map_err(join_err)?
            .map_err(review_err)
    }
}

fn join_err(e: tokio::task::JoinError) -> BackendError {
    BackendError::api(500, "internal", e.to_string())
}

fn review_err(e: ReviewError) -> BackendError {
    BackendError::api(e.status, e.code, e.message)
}

fn rerr(e: RErr) -> BackendError {
    review_err(e.0)
}

fn api_err(e: ApiErr) -> BackendError {
    BackendError::api(e.status.as_u16(), e.code, e.message)
}

fn to_value<T: serde::Serialize>(v: T) -> BResult<Value> {
    serde_json::to_value(v).map_err(|e| BackendError::api(500, "internal", e.to_string()))
}

fn array(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        _ => Vec::new(),
    }
}

/// The agent's identity as the `Actor` its writes are attributed to (what cv-server derives from
/// X-CV-Agent / X-CV-Agent-Client over HTTP).
fn actor(who: &AgentId) -> Actor {
    Actor::Agent {
        name: who.name.clone(),
        client: who.client.clone(),
        workspace: who.workspace.clone(),
    }
}

fn parse<T: serde::de::DeserializeOwned>(v: Value) -> BResult<T> {
    serde_json::from_value(v).map_err(|e| BackendError::api(422, "invalid", e.to_string()))
}

/// Runs a blocking read-side or extras call off the async runtime.
async fn blocking_api<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiErr> + Send + 'static,
) -> BResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(join_err)?
        .map_err(api_err)
}

#[async_trait]
impl McpBackend for InProcess {
    async fn workspaces(&self) -> BResult<Vec<Value>> {
        Ok(array(
            crate::readside::workspace_cards(&self.app)
                .await
                .map_err(api_err)?,
        ))
    }

    async fn bind_workspace(&self, who: &AgentId, workspace: &str) -> BResult<Value> {
        let body = json!({ "agent": who.name, "client": who.client, "workspace": workspace });
        crate::agent_api::bind_of(&self.app, &body)
            .await
            .map_err(rerr)
    }

    async fn attention(&self, event: Value) -> BResult<()> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || crate::agent_api::attention_of(&app, &event))
            .await
            .map_err(join_err)?
            .map_err(rerr)?;
        Ok(())
    }

    async fn view_state(&self) -> BResult<Value> {
        let v = crate::agent_api::view_state_of(&self.app).map_err(rerr)?;
        Ok(serde_json::to_value(v).unwrap_or(Value::Null))
    }

    async fn set_view(&self, who: &AgentId, spec: Value) -> BResult<Value> {
        let (app, who) = (self.app.clone(), actor(who));
        blocking_api(move || crate::extras_api::set_view(&app, &who, &spec)).await
    }

    async fn focus(&self, who: &AgentId, body: Value) -> BResult<Value> {
        crate::agent_api::focus_of(&self.app, actor(who), &body).map_err(rerr)
    }

    async fn create_tour(&self, who: &AgentId, body: Value) -> BResult<Value> {
        crate::agent_api::create_tour_of(&self.app, actor(who), &body).map_err(rerr)
    }

    async fn create_view(&self, spec: Value) -> BResult<Value> {
        crate::routes::view_ref_for(self.app.clone(), spec)
            .await
            .map_err(api_err)
    }

    async fn view_windows(&self, view: &str) -> BResult<Vec<Value>> {
        let v = self.app.view(view).map_err(api_err)?;
        let headers: Vec<_> = v.windows.iter().map(|w| w.header.clone()).collect();
        match self.app.read.range(view) {
            // Range views: headers with the range fields, as their tiles serve them.
            Some(r) => Ok(array(
                r.tile_json(TileResponse {
                    layout_version: v.bootstrap.base.view.layout_version,
                    windows: headers,
                    edges: vec![],
                })["windows"]
                    .take(),
            )),
            None => headers.into_iter().map(to_value).collect(),
        }
    }

    async fn view_hubs(&self, view: &str) -> BResult<Vec<Value>> {
        let v = self.app.view(view).map_err(api_err)?;
        Ok(array(to_value(&v.bootstrap.base.hubs)?))
    }

    async fn window_batch(&self, view: &str, ids: &[String], parts: &[&str]) -> BResult<Value> {
        let v = self.app.view(view).map_err(api_err)?;
        let req: WindowBatchRequest =
            parse(json!({ "ids": ids, "parts": parts, "layoutVersion": 0 }))?;
        let mut res = crate::routes::batch_response(&v, &req);
        crate::extras_api::overlay_batch(&self.app, &v, &req, &mut res);
        self.app.read.add_diffs(view, &req, &mut res);
        to_value(res)
    }

    async fn blob_text(&self, blob: &str, from: Option<u32>, to: Option<u32>) -> BResult<Value> {
        to_value(
            crate::routes::blob_text_of(&self.app, blob.to_string(), from, to).map_err(api_err)?,
        )
    }

    async fn metrics(
        &self,
        view: &str,
        level: &str,
        lane: Option<&str>,
        limit: usize,
    ) -> BResult<Value> {
        to_value(
            crate::readside::metrics_of(
                &self.app,
                view,
                Some(level.to_string()),
                lane.map(String::from),
                Some(limit),
            )
            .await
            .map_err(api_err)?,
        )
    }

    async fn search(
        &self,
        q: &str,
        kinds: Option<&str>,
        view: Option<&str>,
        limit: usize,
    ) -> BResult<Vec<Value>> {
        let q = crate::readside::SearchQ {
            q: q.to_string(),
            kinds: kinds.map(String::from),
            view_id: view.map(String::from),
            limit: Some(limit),
        };
        let hits = crate::readside::search_of(&self.app, q)
            .await
            .map_err(api_err)?;
        hits.into_iter().map(to_value).collect()
    }

    async fn links(
        &self,
        view: &str,
        target: &str,
        dir: &str,
        kinds: Option<&str>,
        depth: u8,
    ) -> BResult<Value> {
        let q = crate::extras_api::LinksQuery {
            view_id: view.to_string(),
            target: target.to_string(),
            dir: Some(dir.to_string()),
            kinds: kinds.map(String::from),
            depth: Some(depth),
            headers: None,
        };
        let app = self.app.clone();
        blocking_api(move || crate::extras_api::get_links(&app, &q)).await
    }

    async fn hub(&self, view: &str, hub: &str) -> BResult<Value> {
        to_value(crate::extras_api::get_hub(&self.app, view, hub, None).map_err(api_err)?)
    }

    async fn revisions(&self, revset: Option<&str>, limit: Option<usize>) -> BResult<Value> {
        to_value(
            crate::readside::revisions_of(self.app.clone(), revset.map(String::from), limit)
                .await
                .map_err(api_err)?,
        )
    }

    async fn interdiff(&self, change: &str, from: &str) -> BResult<Value> {
        let (app, change, from) = (self.app.clone(), change.to_string(), from.to_string());
        blocking_api(move || {
            crate::extras_api::interdiff(&app, &change, &from)
                .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
        })
        .await
    }

    async fn range_diff(&self, revset: &str, fileset: Option<&str>) -> BResult<Value> {
        let (app, revset, fileset) = (
            self.app.clone(),
            revset.to_string(),
            fileset.map(String::from),
        );
        blocking_api(move || crate::extras_api::range_diff(&app, &revset, fileset.as_deref())).await
    }

    async fn tree_diff(&self, from: &str, to: &str, fileset: Option<&str>) -> BResult<Value> {
        let (app, from, to, fileset) = (
            self.app.clone(),
            from.to_string(),
            to.to_string(),
            fileset.map(String::from),
        );
        blocking_api(move || crate::extras_api::tree_diff(&app, &from, &to, fileset.as_deref()))
            .await
    }

    async fn threads(&self, q: &ThreadQuery) -> BResult<Vec<Value>> {
        let f = ThreadFilter {
            status: (!q.status.is_empty()).then(|| q.status.join(",")),
            assignee: q.assignee.clone(),
            path: q.path.clone(),
            changes: None,
        };
        let revset = q.revset.clone().filter(|r| !r.trim().is_empty());
        let ts: Vec<Thread> = self
            .blocking(move |r| match revset {
                Some(rs) => r.list_threads_in(&rs, f),
                None => r.list_threads(&f),
            })
            .await?;
        ts.into_iter().map(to_value).collect()
    }

    async fn create_thread(&self, who: &AgentId, body: Value) -> BResult<Value> {
        let input: NewThread = parse(body)?;
        let who = actor(who);
        to_value(self.blocking(move |r| r.create_thread(input, &who)).await?)
    }

    async fn comment(
        &self,
        who: &AgentId,
        thread: &str,
        body: &str,
        set_status: Option<&str>,
    ) -> BResult<Value> {
        let (who, thread, body) = (actor(who), thread.to_string(), body.to_string());
        let status = set_status.map(String::from);
        to_value(
            self.blocking(move |r| r.reply(&thread, &body, status.as_deref(), &who))
                .await?,
        )
    }

    async fn patch_thread(&self, who: &AgentId, thread: &str, patch: Value) -> BResult<Value> {
        let status = match patch.get("status") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(BackendError::invalid("status must be a string")),
        };
        // Absent = unchanged; null = cleared.
        let assignee: Option<Option<String>> = match patch.get("assignee") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            Some(_) => {
                return Err(BackendError::invalid("assignee must be a string or null"));
            }
        };
        let (who, thread) = (actor(who), thread.to_string());
        to_value(
            self.blocking(move |r| {
                r.patch_thread(
                    &thread,
                    status.as_deref(),
                    assignee.as_ref().map(|a| a.as_deref()),
                    &who,
                )
            })
            .await?,
        )
    }

    async fn declare_link(&self, who: &AgentId, body: Value) -> BResult<Value> {
        let b: crate::extras_api::DeclareBody = parse(body)?;
        let (app, who) = (self.app.clone(), actor(who));
        to_value(blocking_api(move || crate::extras_api::declare_link(&app, &who, &b)).await?)
    }

    async fn rate_link(&self, who: &AgentId, link: &str, body: Value) -> BResult<Value> {
        let b: crate::extras_api::RateBody = parse(body)?;
        let (app, who, link) = (self.app.clone(), actor(who), link.to_string());
        to_value(blocking_api(move || crate::extras_api::rate_link(&app, &who, &link, &b)).await?)
    }

    async fn label_cluster(&self, who: &AgentId, cluster: &str, body: Value) -> BResult<Value> {
        let b: crate::extras_api::LabelBody = parse(body)?;
        let (app, who, cluster) = (self.app.clone(), actor(who), cluster.to_string());
        blocking_api(move || crate::extras_api::label_cluster(&app, &who, &cluster, &b)).await
    }

    async fn edit(&self, who: &AgentId, req: Value) -> BResult<Value> {
        let req: EditRequest = parse(req)?;
        let who = actor(who);
        crate::agent_api::check_edit(&self.app, &who).map_err(rerr)?;
        to_value(self.blocking(move |r| r.edit(req, &who)).await?)
    }

    async fn apply_edit(&self, who: &AgentId, edit: &str) -> BResult<Value> {
        let (who, edit) = (actor(who), edit.to_string());
        crate::agent_api::check_edit(&self.app, &who).map_err(rerr)?;
        to_value(self.blocking(move |r| r.apply_edit(&edit, &who)).await?)
    }

    async fn undo_edit(&self, who: &AgentId, edit: &str) -> BResult<Value> {
        let (who, edit) = (actor(who), edit.to_string());
        crate::agent_api::check_edit(&self.app, &who).map_err(rerr)?;
        to_value(self.blocking(move |r| r.undo_edit(&edit, &who)).await?)
    }

    async fn conflicts(&self, revset: Option<&str>) -> BResult<Vec<Value>> {
        let revset = revset.map(String::from);
        let list = self
            .blocking(move |r| r.list_conflicts(revset.as_deref()))
            .await?;
        list.into_iter().map(to_value).collect()
    }

    async fn conflict(&self, commit: &str, path: &str) -> BResult<Value> {
        let (commit, path) = (commit.to_string(), path.to_string());
        to_value(
            self.blocking(move |r| r.conflict_detail(&commit, &path))
                .await?,
        )
    }

    async fn resolve_conflict(
        &self,
        who: &AgentId,
        commit: &str,
        path: &str,
        content: &str,
    ) -> BResult<Value> {
        let who = actor(who);
        crate::agent_api::check_edit(&self.app, &who).map_err(rerr)?;
        let (commit, path, content) = (commit.to_string(), path.to_string(), content.to_string());
        to_value(
            self.blocking(move |r| r.resolve_conflict(&commit, &path, &content, &who))
                .await?,
        )
    }

    async fn events(
        &self,
        cursor: Option<&str>,
        kinds: &[String],
        timeout: Duration,
    ) -> BResult<Value> {
        let after = match cursor.filter(|c| !c.is_empty()) {
            Some(c) => Some(
                c.parse::<u64>()
                    .map_err(|_| BackendError::invalid(format!("bad cursor {c:?}")))?,
            ),
            None => None,
        };
        let kinds: Option<Vec<String>> = (!kinds.is_empty()).then(|| kinds.to_vec());
        // Same bounds as GET /events over HTTP (whole seconds, at least one, at most two minutes).
        let timeout = Duration::from_secs(timeout.as_secs().clamp(1, 120));
        let (cursor, events, expired): (u64, Vec<ServerEvent>, bool) = self
            .blocking(move |r| {
                let expired = after.is_some_and(|a| r.events().expired(a));
                let (c, e) = r.events().read(after, kinds.as_deref(), timeout);
                Ok((c, e, expired))
            })
            .await?;
        to_value(EventsResponse {
            cursor: cursor.to_string(),
            events,
            resync: expired.then_some(true),
        })
    }

    async fn cursor(&self) -> BResult<Option<String>> {
        let c = self.blocking(|r| Ok(r.events().cursor())).await?;
        Ok(Some(c.to_string()))
    }
}
