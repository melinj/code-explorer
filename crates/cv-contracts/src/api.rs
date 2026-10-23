//! Spec types that used to travel as ad-hoc JSON (DESIGN D3): view specs and view state
//! (model.ts, ws.ts), the repo, tour, permission, fragment and layout bodies of http.ts, file diffs
//! and `ApiError` (ids.ts). Field names follow the TS spec (camelCase).
//!
//! String unions of the spec (ViewSpec modes aside) are plain strings here, as elsewhere in the
//! crate. `ViewSpec`, `SnapshotOptions` and `RangeOptions` also derive `JsonSchema`: the MCP
//! `set_view` input schema is generated from them.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::readside::{DiffLine, RangeOptions, WorkspaceWc};
use crate::review::{Actor, ThreadId, WorkspaceName};
use crate::{
    ChurnSummary, ClusterId, CommitId, OpId, Rect, RepoPath, SiteRef, ViewId, ViewRef, WindowId,
};

// ─── ids.ts ──────────────────────────────────────────────────────────────────

/// The body of every non-2xx answer is `{ error: ApiError }`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, TS)]
pub struct ApiError {
    /// Stable, machine-readable, e.g. "immutable", "stale-base", "revset-invalid".
    pub code: String,
    /// Human-readable; says how to fix it.
    pub message: String,
    /// e.g. `{ hint }` (400 range-not-chain) or the 409 undo-blocked details.
    #[ts(optional, type = "unknown")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

// ─── model.ts ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct WorkspaceInfo {
    pub name: WorkspaceName,
    /// Absolute path of the workspace checkout.
    pub root: String,
    pub wc: WorkspaceWc,
}

#[derive(schemars::JsonSchema, Clone, Debug, Default, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotOptions {
    /// Workspace whose unsnapshotted files are overlaid live; defaults to the workspace whose @ is `rev`.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceName>,
    /// Overlay files edited on disk but not yet snapshotted by jj. Default true.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_working_copy: Option<bool>,
    /// "districts" (the only layout; anything else is 400 "invalid").
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
}

/// What a view shows: one revision (snapshot) or a revset (range review, D16).
#[derive(schemars::JsonSchema, Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ViewSpec {
    Snapshot {
        /// Resolves to one commit, e.g. "@"; default "@", or "<workspace>@" with
        /// `options.workspace` (POST /views spells it out in the ViewRef).
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rev: Option<String>,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fileset: Option<String>,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<SnapshotOptions>,
    },
    Range {
        /// A chain: one root and one head, e.g. "trunk()..@".
        revset: String,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fileset: Option<String>,
        #[ts(optional)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<RangeOptions>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct Viewport {
    /// World-space rectangle on screen.
    pub rect: Rect,
    /// Screen px per wu.
    pub zoom: f64,
    /// ZoomTier: "L0" … "L3".
    pub tier: String,
}

/// `ViewState.selection`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct ViewSelection {
    pub window: WindowId,
    pub start: u32,
    pub end: u32,
    pub text: String,
}

/// One hunk of a [`FileDiff`]: [first line, line count] on each side.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct FileHunk {
    pub old: [u32; 2],
    pub new: [u32; 2],
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: RepoPath,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<RepoPath>,
    /// "added" | "modified" | "removed" | "renamed" | "copied".
    pub status: String,
    pub hunks: Vec<FileHunk>,
    /// Conflicted at the head: no hunks (MCP get_diff). Only ever written as `true`.
    #[ts(optional, as = "Option<bool>")]
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conflicted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct ThreadSummary {
    pub id: ThreadId,
    pub status: String,
    pub kind: String,
    /// "agent" | "human".
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub path: RepoPath,
    pub start: u32,
    pub end: u32,
    pub comments: u32,
    /// First line of the first comment.
    pub preview: String,
}

// ─── ws.ts ───────────────────────────────────────────────────────────────────

/// What the human is looking at. Shared with the agent so "fix this" has a referent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct ViewState {
    pub view_id: ViewId,
    pub viewport: Viewport,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<WindowId>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ViewSelection>,
    /// File chip clicked: all fragments highlighted.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_file: Option<RepoPath>,
    pub lens: String,
    /// "history" | "review" | "workspaces" | "hotspots" | "agent".
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
    /// When true, focus.request from the agent moves the camera.
    pub follow_agent: bool,
}

// ─── http.ts ─────────────────────────────────────────────────────────────────

/// `RepoInfo.index`.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoIndex {
    /// "idle" | "running".
    pub state: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_op: Option<OpId>,
    pub files: u32,
    pub blobs_parsed: u32,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub churn: Option<ChurnSummary>,
}

/// `RepoInfo.capabilities`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoCapabilities {
    pub scip: bool,
    pub runtime_links: bool,
    pub jj_lib: bool,
}

/// `SetupInfo.jj`: the jj CLI on the server's PATH.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct JjInstall {
    pub installed: bool,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// GET /setup: what the server serves; "git" is a plain git repository without jj yet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct SetupInfo {
    /// "jj" | "git"
    pub vcs: String,
    pub root: String,
    pub jj: JjInstall,
    pub init_command: String,
}

/// One file of [`GitCommitDetail`]; counts absent for binary files.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct GitFileStat {
    pub path: RepoPath,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<u32>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<u32>,
}

/// GET /git/commits/:commit (git mode).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct GitCommitDetail {
    pub commit: CommitId,
    pub files: Vec<GitFileStat>,
}

/// GET /repo.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoInfo {
    pub name: String,
    pub root: String,
    pub jj_version: String,
    pub op: OpId,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trunk: Option<CommitId>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub index: RepoIndex,
    pub capabilities: RepoCapabilities,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct TourStep {
    pub target: SiteRef,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<[u32; 2]>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Tour {
    pub id: String,
    pub title: String,
    pub by: Actor,
    pub created_at: String,
    pub steps: Vec<TourStep>,
}

/// Agent writes are rejected with 403 "agent-edit-disabled" while `edit` is false.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentPermissions {
    pub edit: bool,
    pub move_camera: bool,
    /// The UI session these toggles belong to.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The agent they apply to, "*" = every agent.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// True when this is what is in force for an agent across sessions.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective: Option<bool>,
}

/// One window of a file in `FragmentsResponse`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct FragmentRef {
    pub window: WindowId,
    pub index: u32,
    pub rect: Rect,
    pub cluster: ClusterId,
}

/// GET /views/:viewId/fragments.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct FragmentsResponse {
    pub path: RepoPath,
    /// The file chip slot (as in the file fragments of a window header).
    pub chip: u32,
    pub fragments: Vec<FragmentRef>,
}

/// One move of a [`LayoutPatch`]: `key` is a WindowKey or `cluster:<id|label>`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct LayoutMove {
    pub key: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub pinned: bool,
}

/// PATCH /views/:viewId/layout.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct LayoutPatch {
    pub moves: Vec<LayoutMove>,
    pub base_layout_version: u32,
}

/// GET /interdiff.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct InterdiffResponse {
    pub files: Vec<FileDiff>,
    /// [reviewed, current] when the description changed.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<(String, String)>,
}

/// GET /agent/view-state: `{ view, state }`, or `{ view: null }` without a UI.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum AgentViewState {
    Shown {
        view: Box<ViewRef>,
        state: Box<ViewState>,
    },
    None {
        view: (),
    },
}
