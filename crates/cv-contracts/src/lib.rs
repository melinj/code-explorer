//! Wire types for the HTTP API, mirroring `docs/contracts/*.ts`.
//!
//! This is the subset the canvas needs first (T0 bootstrap, T1 tiles, T2 outlines, T3 text).
//! Field names follow the TS spec (camelCase); every wire type derives ts-rs
//! (`ts.rs` generates the TS; `api.rs` holds the spec types that were ad-hoc JSON before), and
//! types used in MCP tool inputs also derive schemars.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub mod readside;

pub type ViewId = String;
pub type WindowId = String;
pub type WindowKey = String;
pub type ClusterId = String;
pub type LaneId = String;
pub type HubId = String;
pub type LinkId = String;
pub type BlobId = String;
pub type CommitId = String;
pub type ChangeId = String;
pub type OpId = String;
pub type RepoPath = String;

// wire.ts constants
pub const TILE_BASE_WU: f64 = 4096.0;
pub const FILE_CHIP_SLOTS: u32 = 10;
pub const WINDOW_MAX_LINES: u32 = 500;
pub const LINE_WU: f64 = 18.0;
pub const COL_WU: f64 = 9.0;
pub const HEADER_WU: f64 = 36.0;
pub const OUTLINE_RECORD_BYTES: usize = 3;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, TS)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum Texture {
    Plain,
    Hatch,
    Dots,
    Lines,
    Cross,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct LaneDef {
    pub id: LaneId,
    pub label: String,
    pub order: u32,
    pub texture: Texture,
    pub collapsed: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct LaneRow {
    pub conn: u8,
    pub y: f64,
    pub h: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct LaneBand {
    pub lane: LaneId,
    pub rect: Rect,
    pub files: u32,
    pub clusters: u32,
    pub rows: Vec<LaneRow>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct GridColumn {
    pub heat: u8,
    pub x: f64,
    pub w: f64,
    pub clusters: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Cluster {
    pub id: ClusterId,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ClusterId>,
    pub lane: LaneId,
    pub label: String,
    pub label_source: String,
    /// Agent-written one-liner (label_cluster).
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub rect: Rect,
    pub heat: u8,
    pub conn: u8,
    pub windows: u32,
    pub open_threads: u32,
    pub conflicts: u32,
    /// Pinned by a layout PATCH (`cluster:<id>` with `pinned: true`); it keeps its spot on re-index.
    #[serde(default)]
    pub pinned: bool,
    /// Heat on a continuous 0..1 scale inside its grade (grade g spans [g/5, (g+1)/5)): the colour
    /// ramp's position, so clusters of one column still differ. Absent from older indexes.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heat_level: Option<f64>,
    /// Share of the cluster's windows that are review first (WindowHeader.reviewFirst), 0..1: the
    /// L0 Hotspots lens. Absent from older indexes.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<f64>,
    /// Folders layout (`options.layout: "folders"`): the directory whose own files the cluster
    /// holds ("" for the repo root). A large directory is split into several clusters.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<RepoPath>,
}

/// Folders layout: the frame of a directory (with everything below it) on the map. Directories
/// with no files of their own and a single subdirectory are folded into it (`label` "a/b").
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TS)]
pub struct FolderFrame {
    pub path: RepoPath,
    /// The path below the enclosing frame (or the lane).
    pub label: String,
    pub lane: LaneId,
    /// Nesting depth inside its lane, 0 for the outermost frames.
    pub depth: u32,
    pub rect: Rect,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct SiteRef {
    pub path: RepoPath,
    pub line: u32,
    /// Columns [start, end) of the site on its line, when known.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col: Option<[u32; 2]>,
    /// Qualified name of the symbol at the site, when known.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct Hub {
    pub id: HubId,
    pub protocol: String,
    pub key: String,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    pub display: String,
    pub producers: Vec<SiteRef>,
    pub consumers: Vec<SiteRef>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    /// Drawn hubs: world points of the producers' out ports then the consumers' in ports (window
    /// edge at the site's line), so the client can draw spokes without loading those windows.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchors: Option<Vec<[f64; 2]>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct EdgeBundle {
    pub from: ClusterId,
    pub to: ClusterId,
    pub kinds: std::collections::BTreeMap<String, u32>,
    pub weight: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct ViewStats {
    pub windows: u32,
    pub files: u32,
    pub clusters: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct Resolved {
    pub op: OpId,
    pub head: CommitId,
    pub commits: Vec<CommitId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ViewRef {
    pub id: ViewId,
    pub spec: serde_json::Value,
    pub resolved: Resolved,
    pub status: String,
    pub layout_version: u32,
    /// Range views: the snapshot view (of the range tip) whose placement this view reuses.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_placement: Option<ViewId>,
    pub stats: ViewStats,
    pub warnings: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ViewBootstrap {
    pub view: ViewRef,
    pub world: Rect,
    pub lane_defs: Vec<LaneDef>,
    pub lanes: Vec<LaneBand>,
    pub columns: Vec<GridColumn>,
    pub clusters: Vec<Cluster>,
    pub hubs: Vec<Hub>,
    pub bundles: Vec<EdgeBundle>,
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub churn: Option<ChurnSummary>,
    pub heat_labels: [String; 5],
    /// Folders layout: the directory frames, outermost first. Absent from the district layout.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folders: Option<Vec<FolderFrame>>,
}

/// How far the churn scan went and where its grade bands lie (ViewBootstrap.churn).
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChurnSummary {
    pub head: CommitId,
    pub scanned_commits: u32,
    pub oldest: String,
    /// Share of change mass in settled files, 0..1.
    pub settled_mass: f64,
    /// "settled" | "max-commits" | "max-age" | "history-exhausted".
    pub stop: String,
    pub half_life_days: f64,
    /// Ascending rate thresholds between grades.
    pub bands: [f64; 4],
    /// The highest file rate: the top of the Hot grade on the continuous heat scale.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_rate: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Heat {
    pub grade: u8,
    pub rate: f64,
    pub ci: [f64; 2],
    pub settled: bool,
    pub touches: u32,
    /// Range views only: how many commits of the range touch this window.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_range: Option<InRange>,
}

/// Heat.inRange.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct InRange {
    pub commits: u32,
    pub of: u32,
}

/// WindowHeader.diffStat (range views): lines added and removed over the range.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct DiffStat {
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Connectedness {
    pub grade: u8,
    pub dependents2: u32,
    pub fan_in: u32,
    pub fan_out: u32,
    pub tests: u32,
    pub reach: u32,
    /// Size of the dependency cycle this window's file belongs to, when > 1 (model.ts `cycle?`).
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct SymbolRef {
    pub name: String,
    pub qualname: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LineSpan {
    pub start: u32,
    pub end: u32,
    pub file_lines: u32,
}

/// The span a window showed before it was expanded (`WindowHeader.expandedFrom`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct FileFragment {
    pub index: u32,
    pub of: u32,
    pub chip: u32,
    pub siblings: Vec<WindowId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WindowCounts {
    pub open_threads: u32,
    pub ports: u32,
    pub conflicts: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct WindowFlags {
    pub pinned: bool,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WindowHeader {
    pub id: WindowId,
    pub key: WindowKey,
    pub kind: String,
    pub cluster: ClusterId,
    pub lane: LaneId,
    pub path: RepoPath,
    pub lang: String,
    pub rev: CommitId,
    pub change: ChangeId,
    pub blob: BlobId,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SymbolRef>,
    pub lines: LineSpan,
    pub rect: Rect,
    pub heat: Heat,
    pub conn: Connectedness,
    pub risk: f64,
    pub review_first: bool,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragment: Option<FileFragment>,
    pub counts: WindowCounts,
    pub flags: WindowFlags,
    /// Range views: lines added and removed over the range.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<DiffStat>,
    /// Set while the window is expanded: the span it showed before (POST …/collapse restores it).
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded_from: Option<LineRange>,
}

/// Window-to-window link geometry for L1/L2 edges (additive to TileResponse). Endpoints are world
/// coordinates of the out and in ports, so an edge draws even when only one end's tile is loaded.
/// A tile carries every edge with either end inside it; clients de-duplicate by id.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TileEdge {
    pub id: LinkId,
    pub kind: String,
    pub from: WindowId,
    pub to: WindowId,
    pub from_cluster: ClusterId,
    pub to_cluster: ClusterId,
    pub a: [f64; 2],
    pub b: [f64; 2],
    pub confidence: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TileResponse {
    pub layout_version: u32,
    pub windows: Vec<WindowHeader>,
    pub edges: Vec<TileEdge>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WindowBatchRequest {
    pub ids: Vec<WindowId>,
    pub parts: Vec<String>,
    pub layout_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct Heading {
    pub line: u32,
    pub depth: u32,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct Port {
    pub id: String,
    pub side: String,
    pub line: u32,
    pub label: String,
    pub kind: String,
    pub links: Vec<LinkId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct WindowOutline {
    pub id: WindowId,
    pub silhouette: String,
    pub headings: Vec<Heading>,
    pub ports: Vec<Port>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct Link {
    pub id: LinkId,
    pub kind: String,
    pub from: SiteRef,
    pub to: serde_json::Value,
    pub label: String,
    pub confidence: f64,
    pub provenance: String,
    /// "confirmed" | "rejected": human or agent rating (rate_link); overrides the score.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// Declared links (declare_link): the declarer's note.
    #[ts(optional)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct WindowBatchResponse {
    pub outlines: Vec<WindowOutline>,
    pub diffs: Vec<serde_json::Value>,
    pub links: Vec<Link>,
    pub missing: Vec<WindowId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct BlobText {
    pub blob: BlobId,
    pub lang: String,
    pub from: u32,
    pub to: u32,
    pub lines: Vec<String>,
    pub tokens: Vec<Vec<u32>>,
}

pub mod api;
pub mod review;
pub mod ts;
