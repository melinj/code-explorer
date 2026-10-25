//! Read-side wire types (model.ts / http.ts): history graph, workspaces, revset validation, range
//! views, Hotspots metrics, search and window expansion.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ChangeId, ClusterId, CommitId, Connectedness, Heat, HubId, LaneId, OpId, RepoPath, SiteRef,
    WindowId, WindowKey,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct Signature {
    pub name: String,
    pub email: String,
    /// ISO-8601 (ids.ts).
    pub time: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct RevisionFlags {
    pub conflict: bool,
    pub empty: bool,
    pub immutable: bool,
    pub divergent: bool,
    pub hidden: bool,
    pub working_copy: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct ShortestPrefix {
    pub change: u32,
    pub commit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    pub change: ChangeId,
    pub commit: CommitId,
    pub parents: Vec<CommitId>,
    pub description: String,
    pub author: Signature,
    pub committer_time: String,
    pub bookmarks: Vec<String>,
    pub tags: Vec<String>,
    pub workspaces: Vec<String>,
    pub flags: RevisionFlags,
    pub shortest: ShortestPrefix,
}

/// Review overlay drawn on the history panel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct RevisionReview {
    pub change: ChangeId,
    pub open_threads: u32,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<CommitId>,
    pub changed_since_review: bool,
    pub touched_by_agent: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub to_lane: u32,
    pub parent: CommitId,
    /// Commits hidden on this edge (indirect edges only).
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elided: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct GraphRow {
    pub rev: Revision,
    pub lane: u32,
    pub edges: Vec<GraphEdge>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<RevisionReview>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct RevisionsResponse {
    pub rows: Vec<GraphRow>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct WorkspaceWc {
    pub change: ChangeId,
    pub commit: CommitId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct StackSummary {
    pub revset: String,
    pub changes: u32,
    pub files: u32,
    pub conflicts: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentBinding {
    pub name: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    pub activity: String,
    pub last_seen: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct OpRef {
    pub op: OpId,
    /// ISO-8601 (ids.ts).
    pub time: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct WorkspaceOverlap {
    pub workspace: String,
    pub paths: Vec<RepoPath>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub name: String,
    pub root: String,
    pub wc: WorkspaceWc,
    pub serving: bool,
    pub stack: StackSummary,
    pub unsnapshotted_files: u32,
    /// Bound by MCP (`bind_workspace`); absent until an agent binds.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentBinding>,
    pub last_op: OpRef,
    pub overlaps: Vec<WorkspaceOverlap>,
    /// The stack's changed files (first 50), which the Workspaces card opens (model.ts `files?`;
    /// always written, see ts-divergences.txt).
    #[serde(default)]
    pub files: Vec<RepoPath>,
    /// Set when an operation run elsewhere left this workspace stale or its `@` divergent and
    /// recovering is the workspace owner's job (D12, DESIGN §6): cv never snapshots or updates
    /// another workspace's files on its own.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<WorkspaceStale>,
}

/// Why a workspace card is flagged ([`WorkspaceSummary::stale`]).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "kebab-case")]
pub enum StaleReason {
    /// The files on disk were checked out from an older `@` (`jj workspace update-stale`).
    WorkingCopy,
    /// `@`'s change has more than one visible commit.
    Divergent,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct WorkspaceStale {
    pub reason: StaleReason,
    /// What happened and the jj command that fixes it.
    pub note: String,
    /// The other visible commits of `@`'s change (divergent), newest first; else empty.
    pub commits: Vec<CommitId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RangeWarning {
    Gap { missing: Vec<ChangeId> },
    MultipleRoots { changes: Vec<ChangeId> },
    MultipleHeads { changes: Vec<ChangeId> },
    IncludesImmutable { changes: Vec<ChangeId> },
    Conflicted { changes: Vec<ChangeId> },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct RevsetSummary {
    pub ok: bool,
    pub count: u32,
    /// First 50, newest first.
    pub changes: Vec<ChangeId>,
    pub files: u32,
    pub added: u32,
    pub removed: u32,
    pub conflicts: u32,
    pub warnings: Vec<RangeWarning>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct RevsetError {
    pub ok: bool,
    pub error: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(untagged)]
pub enum RevsetValidation {
    Ok(RevsetSummary),
    Err(RevsetError),
}

/// Range view options (model.ts RangeOptions), with the documented defaults.
#[derive(schemars::JsonSchema, Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct RangeOptions {
    /// "cumulative" | "per-commit".
    pub diff: String,
    pub context_hops: u8,
    /// "minimized" | "hidden" | "expanded".
    pub context_display: String,
    pub context_lines: u32,
    pub ignore_whitespace: bool,
    /// "districts" (the only layout; anything else is 400 "invalid").
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    /// Build a range that is not a chain anyway, shown per commit (D16; Compare uses it).
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_non_chain: Option<bool>,
}

impl Default for RangeOptions {
    fn default() -> Self {
        RangeOptions {
            diff: "cumulative".into(),
            context_hops: 1,
            context_display: "minimized".into(),
            context_lines: 3,
            ignore_whitespace: false,
            layout: None,
            allow_non_chain: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct SavedRange {
    pub name: String,
    pub revset: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fileset: Option<String>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<RangeOptions>,
    pub created_by: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    /// "ctx" | "add" | "del".
    pub kind: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_line: Option<u32>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_line: Option<u32>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<ChangeId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct WindowDiff {
    pub id: WindowId,
    pub lines: Vec<DiffLine>,
}

/// One point of the Hotspots chart: x = connectedness percentile, y = heat percentile.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MetricPoint {
    pub key: String,
    pub label: String,
    pub lane: LaneId,
    pub heat: Heat,
    pub conn: Connectedness,
    pub heat_pct: f64,
    pub conn_pct: f64,
    /// Review priority: √(heat pct × conn pct) × (0.5 + 0.5 × `outside`).
    pub risk: f64,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowId>,
    /// Share of its links leaving its module (its directory and below), 0..1; absent without
    /// counted links or on the import-graph basis (whose risk is the plain geometric mean).
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct CoChangePair {
    pub a: RepoPath,
    pub b: RepoPath,
    pub support: u32,
    pub jaccard: f64,
    /// The clusters of `a` and `b` (of each file's first window): the L0 co-change lens.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_a: Option<ClusterId>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_b: Option<ClusterId>,
}

/// Clusters whose files change together (the co-change lens at L0, the level above file pairs):
/// support = commits touching both, jaccard = support / commits touching either. Unlike file
/// pairs, pairs joined by static links are kept; `linked` says whether one is.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct CoChangeClusterPair {
    pub a: ClusterId,
    pub b: ClusterId,
    pub support: u32,
    pub jaccard: f64,
    pub linked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct MetricsResponse {
    pub points: Vec<MetricPoint>,
    pub cochange: Vec<CoChangePair>,
    #[ts(optional)]
    #[serde(
        rename = "cochangeClusters",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cochange_clusters: Option<Vec<CoChangeClusterPair>>,
}

/// GET /views/:id/structure: how the folder tree fits the code's data flow and change frequency
/// (DESIGN §4.11). Links are distinct window pairs (an import counts only when the two files share
/// no other link); links from the test and docs lanes are left out.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct StructureResponse {
    pub summary: StructureSummary,
    /// Every directory holding counted files, each with everything below it, in path order.
    pub folders: Vec<FolderStructure>,
    /// Files with links or change, in path order.
    pub files: Vec<FileStructure>,
    /// Windows with links or tension, in window order.
    pub windows: Vec<WindowStructure>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct StructureSummary {
    /// Directories holding files of their own.
    pub folders: u32,
    pub files: u32,
    pub links: u32,
    /// Links whose two ends sit in different directories.
    pub cross_folder_links: u32,
    /// Share of links inside one directory, 0..1.
    pub link_locality: f64,
    /// How far the folder tree is from the dependency clusters of the district (change) layout:
    /// 1 − normalised mutual information of the two groupings of windows, 0 = the same grouping.
    /// Absent when no district layout of the repo is known yet.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divergence: Option<f64>,
    /// Share of windows whose dependency cluster is mostly another directory's code.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub misgrouped: Option<f64>,
    /// Change-weighted mean of the directories' own heat tension, 0..1.
    pub heat_tension: f64,
    /// Window-weighted mean of the directories' own flow pressure, 0..1.
    pub pressure: f64,
}

/// A directory with everything below it, seen as a module.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct FolderStructure {
    pub path: RepoPath,
    /// Path components (0 for the repo root).
    pub depth: u32,
    pub files: u32,
    pub windows: u32,
    /// Links with both ends inside.
    pub internal: u32,
    /// Links from inside to outside, and from outside in.
    pub outgoing: u32,
    pub incoming: u32,
    /// Flow pressure: boundary links / all links touching it, 0 (sealed) .. 1 (every link leaves).
    pub pressure: f64,
    /// Its depth as a module: links staying inside it (in it or below) / all links touching it,
    /// 1 − pressure (0 when no link touches it).
    pub inside: f64,
    /// Windows inside that something outside links to: the module's interface.
    pub interface: u32,
    /// interface / windows: a narrow interface is small.
    pub interface_share: f64,
    /// Directories outside it that it links with (either way).
    pub partners: u32,
    /// Files inside with a link crossing its boundary: its gateways.
    pub gateways: u32,
    /// How the boundary links are spread over its files: (gateways − 1) / (files − 1), 0 when one
    /// file (or none) carries them all, 1 when every file links outside.
    pub spread: f64,
    /// Longest chain of links inside it, in windows (cycles cut).
    pub chain: u32,
    /// Dependency clusters of the district layout its windows belong to.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modules: Option<u32>,
    /// Change mass (sum of file change rates).
    pub mass: f64,
    /// Heat tension: share of the change mass in warm or hot files × share of the lines in cool or
    /// frozen files, 0..1. High when a few busy files live among code that rarely changes.
    pub tension: f64,
    /// Warm or hot files among cool or frozen siblings, busiest first (at most 5).
    pub hot_files: Vec<RepoPath>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileStructure {
    pub path: RepoPath,
    /// Its directory.
    pub folder: RepoPath,
    pub lane: LaneId,
    /// File heat grade (0 frozen .. 4 hot) and rate.
    pub heat: u8,
    pub rate: f64,
    /// Links to or from files of the same directory, and to or from other directories.
    pub internal: u32,
    pub external: u32,
    /// Its windows that other directories link to.
    pub exports: u32,
    /// How much busier it is than its directory: 0..1, > 0 only for warm or hot files, the share
    /// of its siblings' lines that are cool or frozen (warm files count 0.6).
    pub tension: f64,
    /// The other directory it has the most links with, when that beats its own directory (files
    /// with siblings only).
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull: Option<FolderPull>,
}

/// A window's links against its directory, and its file's heat tension.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
pub struct WindowStructure {
    pub key: WindowKey,
    pub path: RepoPath,
    /// Links with windows of its directory or below it (its own file included).
    pub internal: u32,
    /// Links with siblings or parents of its directory (anything not below it), either way;
    /// `incoming` of them come in.
    pub external: u32,
    pub incoming: u32,
    /// Its file's heat tension (FileStructure.tension).
    pub tension: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct FolderPull {
    pub folder: RepoPath,
    pub links: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, TS)]
pub struct SearchTarget {
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowId>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<SiteRef>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub: Option<HubId>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<ChangeId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct SearchHit {
    /// "symbol" | "file" | "hub" | "thread" | "revision".
    pub kind: String,
    pub label: String,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub target: SearchTarget,
    pub score: f64,
}

/// How far to widen a window: a line count, or "symbol" / "file".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
#[serde(untagged)]
pub enum ExpandAmount {
    Lines(u32),
    To(String),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, TS)]
pub struct ExpandRequest {
    #[ts(optional = nullable)]
    #[serde(default)]
    pub before: Option<ExpandAmount>,
    #[ts(optional = nullable)]
    #[serde(default)]
    pub after: Option<ExpandAmount>,
}
