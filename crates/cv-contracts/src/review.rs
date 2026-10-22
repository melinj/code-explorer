//! Review, edit, conflict and live-event wire types (docs/contracts/model.ts "Review" and "Edits and
//! conflicts", http.ts `NewThread`, ws.ts). Field names follow the TS spec (camelCase).
//!
//! Enumerations that the TS spec types as string unions (thread status, kind, severity, edit mode)
//! are plain strings here; `cv-review` validates them against the constants below.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{BlobId, ChangeId, CommitId, OpId, RepoPath, SiteRef, ViewId, WindowId};

pub type ThreadId = String;
pub type CommentId = String;
pub type EditId = String;
pub type EventCursor = String;
pub type WorkspaceName = String;

pub const THREAD_STATUSES: [&str; 6] = [
    "open",
    "in-progress",
    "addressed",
    "resolved",
    "outdated",
    "wontfix",
];
pub const THREAD_KINDS: [&str; 5] = ["comment", "suggestion", "question", "task", "finding"];
pub const SEVERITIES: [&str; 4] = ["info", "minor", "major", "blocker"];
pub const EDIT_MODES: [&str; 5] = [
    "amend",
    "insert-after",
    "branch",
    "keep-descendants",
    "absorb",
];

/// Who did something (ids.ts `Actor`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Actor {
    Human {
        name: String,
    },
    Agent {
        name: String,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client: Option<String>,
        /// The jj workspace the agent works in (`X-CV-Workspace`, or the MCP session's bound or
        /// default workspace). Additive (DESIGN §7 "Agent binding", §10 per-actor attribution).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<WorkspaceName>,
    },
    External,
}

impl Actor {
    pub fn human(name: &str) -> Self {
        Actor::Human { name: name.into() }
    }

    /// An agent actor (no client), optionally in a workspace.
    pub fn agent(name: &str, workspace: Option<&str>) -> Self {
        Actor::Agent {
            name: name.into(),
            client: None,
            workspace: workspace.map(String::from),
        }
    }

    /// "you", "claude@gym", "external" — for op descriptions and logs.
    pub fn label(&self) -> String {
        match self {
            Actor::Human { name } => name.clone(),
            Actor::Agent {
                name,
                workspace: Some(w),
                ..
            } => format!("{name}@{w}"),
            Actor::Agent { name, .. } => name.clone(),
            Actor::External => "external".into(),
        }
    }

    /// The agent's workspace, if this is an agent working in one.
    pub fn workspace(&self) -> Option<&str> {
        match self {
            Actor::Agent { workspace, .. } => workspace.as_deref(),
            _ => None,
        }
    }

    /// The agent's name, if this is an agent.
    pub fn agent_name(&self) -> Option<&str> {
        match self {
            Actor::Agent { name, .. } => Some(name),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
pub struct Fingerprint {
    pub text: String,
    pub before: String,
    pub after: String,
}

/// Threads anchor to a change, not just a commit, so they follow the change through rewrites.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
    pub change: ChangeId,
    /// Commit the anchor was last confirmed against.
    pub commit: CommitId,
    pub path: RepoPath,
    pub side: String,
    pub start: u32,
    pub end: u32,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<[u32; 2]>,
    pub fingerprint: Fingerprint,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: CommentId,
    pub author: Actor,
    pub body: String,
    pub created_at: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct Outdated {
    /// "lines-changed" | "file-removed" | "change-abandoned"
    pub reason: String,
    pub last_good: CommitId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: ThreadId,
    pub anchor: Anchor,
    pub status: String,
    pub kind: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub created_by: Actor,
    pub created_at: String,
    pub updated_at: String,
    pub comments: Vec<Comment>,
    pub produced_edits: Vec<EditId>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outdated: Option<Outdated>,
}

/// http.ts `NewThread.anchor`: `rev` is a change id or a commit id (prefixes allowed).
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NewAnchor {
    pub path: RepoPath,
    pub rev: String,
    pub start: u32,
    #[ts(optional = nullable)]
    #[serde(default)]
    pub end: Option<u32>,
    #[ts(optional = nullable)]
    #[serde(default)]
    pub side: Option<String>,
    #[ts(optional = nullable)]
    #[serde(default)]
    pub cols: Option<[u32; 2]>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NewThread {
    pub anchor: NewAnchor,
    pub body: String,
    #[ts(optional = nullable)]
    #[serde(default)]
    pub kind: Option<String>,
    #[ts(optional = nullable)]
    #[serde(default)]
    pub severity: Option<String>,
    #[ts(optional = nullable)]
    #[serde(default)]
    pub assignee: Option<String>,
}

/// Replace lines [start, end] (1-based, inclusive). end = start - 1 inserts before start.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct LinePatch {
    pub start: u32,
    pub end: u32,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct EditFile {
    pub path: RepoPath,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_blob: Option<String>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patches: Option<Vec<LinePatch>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct EditOrigin {
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct EditRequest {
    /// Change id or commit id (prefixes allowed); for absorb, the range tip.
    pub target: String,
    pub files: Vec<EditFile>,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absorb_into: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<EditOrigin>,
}

fn default_mode() -> String {
    "amend".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct EditWorkspace {
    pub name: WorkspaceName,
    pub snapshotted: bool,
    pub updated: bool,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_into: Option<CommitId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct EditError {
    /// "immutable" | "stale-base" | "invalid-target" | "locked" | "jj-error" | "undo-blocked"
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct EditResult {
    pub id: EditId,
    /// "previewed" | "applied" | "rejected" | "failed" | "undone"
    pub status: String,
    pub actor: Actor,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_before: Option<OpId>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_after: Option<OpId>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<ChangeId>,
    pub rewritten: Vec<ChangeId>,
    pub workspaces: Vec<EditWorkspace>,
    pub conflicts: Vec<ConflictRecord>,
    pub predicted: bool,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EditError>,
    /// Applied edits: each edited file's blob in the commit that now holds the edit (the baseBlob
    /// for a follow-up edit without re-reading). Additive; absent for absorb and failed edits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<EditedFile>,
    /// Things the human should know before or after applying (DESIGN §6), e.g. an affected
    /// workspace with an agent attached ("agent-attached") or a change left divergent
    /// ("divergent"). Additive.
    #[ts(optional, as = "Option<Vec<EditWarning>>")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<EditWarning>,
    /// Changes that have more than one visible commit after this edit (or its undo). Additive;
    /// normally empty: targeted undo refuses instead of creating divergence.
    #[ts(optional, as = "Option<Vec<ChangeId>>")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub divergent: Vec<ChangeId>,
}

/// EditResult.warnings item (additive).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct EditWarning {
    /// "agent-attached" | "divergent" | "workspace-recovered"
    pub code: String,
    pub message: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceName>,
    /// The agent bound to `workspace` ("agent-attached").
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Actor>,
    #[ts(optional, as = "Option<Vec<ChangeId>>")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<ChangeId>,
}

/// One operation after an edit's own that touched the changes the edit rewrote.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct LaterOp {
    pub op: OpId,
    pub description: String,
    /// ISO-8601.
    pub time: String,
    /// "user@host" from the op metadata.
    pub user: String,
    /// The edit's changes this op rewrote or abandoned.
    pub changes: Vec<ChangeId>,
}

/// `ApiError.details` of a 409 `undo-blocked` (POST /edits/:id/undo): reverting the edit's
/// operation now would leave changes divergent or conflicted, so nothing was done.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct UndoBlocked {
    pub edit: EditId,
    /// The operation undo would revert (the edit's opAfter).
    pub op: OpId,
    /// Later operations that touched the edit's changes, newest first.
    pub later_ops: Vec<LaterOp>,
    /// Changes the revert would leave with more than one visible commit.
    pub divergent: Vec<ChangeId>,
    /// Conflicts the revert would introduce.
    pub conflicted: Vec<ConflictRecord>,
    /// How to get out of it, in words.
    pub suggestion: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct EditedFile {
    pub path: RepoPath,
    /// None when the edit removed the file.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_blob: Option<BlobId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConflictRecord {
    pub change: ChangeId,
    pub commit: CommitId,
    pub path: RepoPath,
    pub sides: u32,
    pub first_seen_op: OpId,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introduced_by: Option<EditId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct ConflictSide {
    pub label: String,
    pub text: String,
}

/// One conflicted region. `start`/`end` are 1-based inclusive lines of `materialized`, marker lines
/// included; `base` is the (first) base text and `sides` the added texts, in jj's order.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct ConflictRegion {
    pub start: u32,
    pub end: u32,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub sides: Vec<ConflictSide>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct ConflictDetail {
    #[serde(flatten)]
    pub record: ConflictRecord,
    /// File text with jj conflict markers ("diff" style, labels name the changes).
    pub materialized: String,
    pub regions: Vec<ConflictRegion>,
}

/// ws.ts `ServerEvent`. `workspace.updated` carries a `WorkspaceSummary` (model.ts) as JSON (the
/// cards are assembled as JSON), and `view.requested` its `ViewSpec` as sent (parsing it would
/// fill in RangeOptions defaults the client did not send).
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "hello")]
    Hello {
        session: String,
        op: OpId,
        #[serde(rename = "serverVersion")]
        server_version: String,
        cursor: EventCursor,
        /// Additive: the `?cursor=` is older than the event log keeps (retention); nothing is
        /// replayed, the client refetches its state and continues from `cursor`.
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resync: Option<bool>,
    },
    #[serde(rename = "op.changed")]
    OpChanged {
        op: OpId,
        changes: Vec<ChangeId>,
        actor: Actor,
    },
    #[serde(rename = "index.progress")]
    IndexProgress {
        job: String,
        done: u32,
        total: u32,
        #[ts(optional)]
        #[serde(rename = "viewId", default, skip_serializing_if = "Option::is_none")]
        view_id: Option<ViewId>,
    },
    #[serde(rename = "view.invalidated", rename_all = "camelCase")]
    ViewInvalidated {
        view_id: ViewId,
        layout_version: u32,
        /// "op" | "layout" | "index" | "expand"
        reason: String,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        windows: Option<Vec<WindowId>>,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tiles: Option<Vec<String>>,
    },
    #[serde(rename = "thread.upserted")]
    ThreadUpserted { thread: Box<Thread> },
    #[serde(rename = "review.submitted")]
    ReviewSubmitted {
        threads: Vec<ThreadId>,
        by: Actor,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// A change was marked reviewed at `commit` (PUT /reviews/marks): History rows update live.
    #[serde(rename = "review.marked")]
    ReviewMarked {
        change: ChangeId,
        commit: CommitId,
        reviewer: String,
    },
    #[serde(rename = "edit.updated")]
    EditUpdated { edit: Box<EditResult> },
    #[serde(rename = "conflicts.changed")]
    ConflictsChanged {
        added: Vec<ConflictRecord>,
        resolved: Vec<ConflictRecord>,
    },
    #[serde(rename = "agent.attention")]
    AgentAttention {
        agent: String,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<WorkspaceName>,
        tool: String,
        windows: Vec<WindowId>,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        at: String,
    },
    #[serde(rename = "focus.request")]
    FocusRequest {
        by: Actor,
        target: SiteRef,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<[u32; 2]>,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    #[serde(rename = "workspace.updated")]
    WorkspaceUpdated { workspace: serde_json::Value },
    #[serde(rename = "tour.created")]
    TourCreated { tour: crate::api::Tour },
    #[serde(rename = "agent.permissions")]
    AgentPermissions {
        permissions: crate::api::AgentPermissions,
    },
    /// Reply to a client `ping` (sent only to that client, never logged).
    #[serde(rename = "pong")]
    Pong { at: String },
    #[serde(rename = "workspace.recovered", rename_all = "camelCase")]
    WorkspaceRecovered {
        workspace: WorkspaceName,
        divergent_commit: CommitId,
        note: String,
    },
    /// An agent asked the UI to open a view (POST /agent/view, allowed by `moveCamera`); the UI
    /// switches to it while it follows the agent.
    #[serde(rename = "view.requested", rename_all = "camelCase")]
    ViewRequested {
        view_id: ViewId,
        spec: serde_json::Value,
        by: Actor,
    },
}

impl ServerEvent {
    /// The wire `type` string ("op.changed", …), used for `kinds` filters.
    pub fn kind(&self) -> &'static str {
        match self {
            ServerEvent::Hello { .. } => "hello",
            ServerEvent::OpChanged { .. } => "op.changed",
            ServerEvent::IndexProgress { .. } => "index.progress",
            ServerEvent::ViewInvalidated { .. } => "view.invalidated",
            ServerEvent::ThreadUpserted { .. } => "thread.upserted",
            ServerEvent::ReviewSubmitted { .. } => "review.submitted",
            ServerEvent::ReviewMarked { .. } => "review.marked",
            ServerEvent::EditUpdated { .. } => "edit.updated",
            ServerEvent::ConflictsChanged { .. } => "conflicts.changed",
            ServerEvent::AgentAttention { .. } => "agent.attention",
            ServerEvent::FocusRequest { .. } => "focus.request",
            ServerEvent::WorkspaceUpdated { .. } => "workspace.updated",
            ServerEvent::TourCreated { .. } => "tour.created",
            ServerEvent::AgentPermissions { .. } => "agent.permissions",
            ServerEvent::Pong { .. } => "pong",
            ServerEvent::WorkspaceRecovered { .. } => "workspace.recovered",
            ServerEvent::ViewRequested { .. } => "view.requested",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct ServerFrame {
    pub seq: EventCursor,
    pub event: ServerEvent,
}

/// ws.ts `ClientMessage`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "view.subscribe", rename_all = "camelCase")]
    ViewSubscribe { view_id: ViewId },
    #[serde(rename = "view.unsubscribe", rename_all = "camelCase")]
    ViewUnsubscribe { view_id: ViewId },
    #[serde(rename = "view.state")]
    ViewState { state: serde_json::Value },
    #[serde(rename = "ping")]
    Ping,
}

/// GET /api/v1/events response.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct EventsResponse {
    pub cursor: EventCursor,
    pub events: Vec<ServerEvent>,
    /// Additive: the request's cursor is older than the event log keeps (retention), so events
    /// were missed; refetch state, then continue from `cursor`.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resync: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize_like_ws_ts() {
        let e = ServerEvent::ViewInvalidated {
            view_id: "v_1".into(),
            layout_version: 1,
            reason: "op".into(),
            windows: Some(vec!["w1".into()]),
            tiles: None,
        };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(
            j,
            serde_json::json!({ "type": "view.invalidated", "viewId": "v_1", "layoutVersion": 1, "reason": "op", "windows": ["w1"] })
        );
        let a: Actor = serde_json::from_value(
            serde_json::json!({ "kind": "agent", "name": "claude", "client": "claude-code" }),
        )
        .unwrap();
        assert_eq!(a.label(), "claude");
        let m: ClientMessage = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert_eq!(m, ClientMessage::Ping);
        let h = serde_json::to_value(ServerEvent::Hello {
            session: "s".into(),
            op: "o".into(),
            server_version: "v".into(),
            cursor: "0".into(),
            resync: None,
        })
        .unwrap();
        assert_eq!(h["serverVersion"], "v");
        assert!(h.get("resync").is_none(), "absent unless set");
    }
}
