//! Tool inputs (docs/contracts/mcp.ts `McpTools[*].in`), with JSON Schemas from schemars.
//! Doc comments become the schema descriptions the agent reads; [`crate::server::slim_schema`]
//! strips the generator boilerplate. Closed vocabularies are enums, so a typo ("opne") is an
//! argument error instead of an empty result.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The wire string of a unit enum variant ("in-progress", "insert-after", …).
pub fn wire<T: Serialize>(t: &T) -> String {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ThreadStatus {
    Open,
    InProgress,
    Addressed,
    Resolved,
    Outdated,
    Wontfix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Assignee {
    Agent,
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ThreadKind {
    Comment,
    Suggestion,
    Question,
    Task,
    Finding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Minor,
    Major,
    Blocker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    New,
    Old,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EditMode {
    Amend,
    InsertAfter,
    Branch,
    KeepDescendants,
    Absorb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SuggestionMode {
    Amend,
    InsertAfter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Confirmed,
    Rejected,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetView {
    /// ViewSpec: { mode: "snapshot", rev } or { mode: "range", revset, fileset?, options? }.
    /// Passed through as JSON; the schema is cv-contracts' ViewSpec.
    #[schemars(with = "cv_contracts::api::ViewSpec")]
    pub spec: Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Focus {
    /// A WindowId, "path:line", a qualname, a HubId or a ThreadId.
    pub target: String,
    /// [start, end] lines to highlight.
    pub lines: Option<[u32; 2]>,
    /// Shown to the human next to the highlight.
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TourStep {
    /// A WindowId, "path:line", a qualname, a HubId or a ThreadId.
    pub target: String,
    pub lines: Option<[u32; 2]>,
    pub note: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateTour {
    pub title: String,
    pub steps: Vec<TourStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, serde::Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchKind {
    Symbol,
    File,
    Hub,
    Thread,
    Revision,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Search {
    /// Symbol name or qualname, path fragment, hub key or thread text.
    pub query: String,
    pub kinds: Option<Vec<SearchKind>>,
    /// Default 10.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Annotation {
    Ports,
    Threads,
    Diff,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadWindow {
    /// WindowId (from search, get_view_state, get_hotspots or a thread).
    pub window: String,
    /// Inline annotations; default ports and threads.
    pub annotate: Option<Vec<Annotation>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadCode {
    /// Repo-relative path, e.g. "src/world/bus.py".
    pub path: String,
    /// Revision to read; default the workspace's @.
    pub rev: Option<String>,
    /// First line (1-based). Default 1.
    pub start: Option<u32>,
    /// Last line (inclusive). Default start + 199.
    pub end: Option<u32>,
    /// Inline annotations; default ports and threads.
    pub annotate: Option<Vec<Annotation>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    In,
    Out,
    Both,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetLinks {
    /// "path:line" (the symbol there), a qualname, a WindowId or a HubId.
    pub target: String,
    /// Default both.
    pub dir: Option<Dir>,
    /// LinkKind filter: call, inherit, implement, import, reference, …
    pub kinds: Option<Vec<String>>,
    /// 1, 2 or 3. Default 1.
    pub depth: Option<u8>,
    /// Default 30.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetHub {
    /// HubId or normalized key, e.g. "world.entity.moved" or "GET /api/node/{}".
    pub key: String,
    pub protocol: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMetrics {
    /// A WindowId, "path:line", a path or a qualname.
    pub target: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetHotspots {
    /// LaneId filter, e.g. "code" or "tests".
    pub lane: Option<String>,
    /// Default 10.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BindWorkspace {
    /// Workspace name from list_workspaces, e.g. "gym".
    pub workspace: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListThreads {
    pub status: Option<Vec<ThreadStatus>>,
    pub assignee: Option<Assignee>,
    pub revset: Option<String>,
    pub path: Option<String>,
    /// Each thread with its comments and the code around its anchor (enclosing symbol, ≤ 80
    /// lines): enough to address it without get_thread.
    pub with_code: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetThread {
    pub thread: String,
    /// Lines of context around the anchor. Default: the enclosing symbol (≤ 80 lines).
    pub context_lines: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NewAnchor {
    pub path: String,
    /// Change or commit; default the file's change at the workspace's @.
    pub rev: Option<String>,
    pub start: u32,
    pub end: u32,
    /// Default new.
    pub side: Option<Side>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateThread {
    pub anchor: NewAnchor,
    /// Markdown. A ```suggestion block proposes a replacement for the anchored lines.
    pub body: String,
    /// Default finding.
    pub kind: Option<ThreadKind>,
    pub severity: Option<Severity>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub thread: String,
    /// Markdown. Omit to only set the status.
    pub body: Option<String>,
    pub set_status: Option<ThreadStatus>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetThreadStatus {
    pub thread: String,
    pub status: ThreadStatus,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplySuggestion {
    pub thread: String,
    /// Default amend (rewrite the anchored change; descendants rebase).
    pub mode: Option<SuggestionMode>,
    /// Preview only: the exact predicted result, nothing written.
    pub dry_run: Option<bool>,
    /// Reply posted in the thread once the edit is applied.
    pub reply: Option<String>,
    /// Status set with the reply; default addressed when reply is given.
    pub set_status: Option<ThreadStatus>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListRevisions {
    /// Default: the workspace's stack, else the server default.
    pub revset: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDiff {
    /// The range to diff (cumulative), e.g. "trunk()..gym@". Default: the workspace's stack.
    pub revset: Option<String>,
    /// Instead of revset: diff the trees of two revisions, as `jj diff --from --to` (each must
    /// resolve to one commit; a missing side is the workspace's working copy).
    pub from: Option<String>,
    /// See `from`.
    pub to: Option<String>,
    /// jj fileset limiting the diff, e.g. "src/" or 'glob:"**/*.py"'.
    pub fileset: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetInterdiff {
    pub change: String,
    /// The commit the human last reviewed.
    pub from: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinePatch {
    /// First replaced line (1-based).
    pub start: u32,
    /// Last replaced line (inclusive); end = start - 1 inserts before start.
    pub end: u32,
    /// Replacement lines joined by "\n" (no trailing newline).
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EditCode {
    /// Thread this edit answers: rev, path and baseBlob default to its anchor and the code last
    /// shown for it.
    pub thread: Option<String>,
    /// Change or commit to edit. Default: the thread anchor's change.
    pub rev: Option<String>,
    /// Default: the thread's path.
    pub path: Option<String>,
    /// AnnotatedCode.blob the patches are against; stale bases are rejected. Default with thread:
    /// the blob last shown for the path.
    pub base_blob: Option<String>,
    /// Line patches against baseBlob…
    pub patches: Option<Vec<LinePatch>>,
    /// …or the whole new file content.
    pub content: Option<String>,
    /// Default amend, or branch when the target is immutable.
    pub mode: Option<EditMode>,
    /// Description for created changes.
    pub message: Option<String>,
    /// Preview only: the exact predicted result, nothing written.
    pub dry_run: Option<bool>,
    /// Reply posted in the thread once the edit is applied.
    pub reply: Option<String>,
    /// Status set with the reply; default addressed when reply is given.
    pub set_status: Option<ThreadStatus>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyEdit {
    /// EditId (from an edit_code dryRun preview, or an applied edit for undo_edit).
    pub edit: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListConflicts {
    pub revset: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetConflict {
    /// Change or commit holding the conflict.
    pub rev: String,
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResolveConflict {
    pub rev: String,
    pub path: String,
    /// The whole resolved file, without conflict markers.
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeclareLink {
    /// "path:line" or a HubId.
    pub from: String,
    /// "path:line" or a HubId.
    pub to: String,
    pub kind: String,
    pub label: String,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RateLink {
    pub link: String,
    pub verdict: Verdict,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LabelCluster {
    pub cluster: String,
    pub label: String,
    pub summary: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WaitForEvents {
    /// Cursor from the previous call; omit on the first call to start from now.
    pub cursor: Option<String>,
    /// Event types, e.g. thread.upserted, review.submitted, op.changed, edit.updated. Default:
    /// all but agent.attention and index.progress.
    pub kinds: Option<Vec<String>>,
    /// Seconds, at most 60. Default 30.
    pub timeout_s: Option<u64>,
    /// Newest events returned, default 20 (older ones are counted in `skipped`).
    pub limit: Option<usize>,
}
