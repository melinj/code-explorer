// Generated from crates/cv-contracts by scripts/gen-contracts.sh (ts-rs). Do not edit.
// The hand-written spec is docs/contracts/*.ts; check.ts compares the two.
import type { JsonValue } from "./json";

export type Rect = { x: number, y: number, w: number, h: number, };

export type Texture = "plain" | "hatch" | "dots" | "lines" | "cross";

export type LaneDef = { id: string, label: string, order: number, texture: Texture, collapsed: boolean, hidden: boolean, };

export type LaneRow = { conn: number, y: number, h: number, };

export type LaneBand = { lane: string, rect: Rect, files: number, clusters: number, rows: Array<LaneRow>, };

export type GridColumn = { heat: number, x: number, w: number, clusters: number, };

export type Cluster = { id: string, parent?: string, lane: string, label: string, labelSource: string, 
/**
 * Agent-written one-liner (label_cluster).
 */
summary?: string, rect: Rect, heat: number, conn: number, windows: number, openThreads: number, conflicts: number, 
/**
 * Pinned by a layout PATCH (`cluster:<id>` with `pinned: true`); it keeps its spot on re-index.
 */
pinned: boolean, 
/**
 * Heat on a continuous 0..1 scale inside its grade (grade g spans [g/5, (g+1)/5)): the colour
 * ramp's position, so clusters of one column still differ. Absent from older indexes.
 */
heatLevel?: number, 
/**
 * Share of the cluster's windows that are review first (WindowHeader.reviewFirst), 0..1: the
 * L0 Hotspots lens. Absent from older indexes.
 */
review?: number, 
/**
 * Folders layout (`options.layout: "folders"`): the directory whose own files the cluster
 * holds ("" for the repo root). A large directory is split into several clusters.
 */
folder?: string, };

/**
 * Folders layout: the frame of a directory (with everything below it) on the map. Directories
 * with no files of their own and a single subdirectory are folded into it (`label` "a/b").
 */
export type FolderFrame = { path: string, 
/**
 * The path below the enclosing frame (or the lane).
 */
label: string, lane: string, 
/**
 * Nesting depth inside its lane, 0 for the outermost frames.
 */
depth: number, rect: Rect, };

export type SiteRef = { path: string, line: number, 
/**
 * Columns [start, end) of the site on its line, when known.
 */
col?: [number, number], 
/**
 * Qualified name of the symbol at the site, when known.
 */
symbol?: string, window?: string, };

export type Hub = { id: string, protocol: string, key: string, family?: string, display: string, producers: Array<SiteRef>, consumers: Array<SiteRef>, rect?: Rect, 
/**
 * Drawn hubs: world points of the producers' out ports then the consumers' in ports (window
 * edge at the site's line), so the client can draw spokes without loading those windows.
 */
anchors?: Array<[number, number]>, };

export type EdgeBundle = { from: string, to: string, kinds: { [key in string]: number }, weight: number, };

export type ViewStats = { windows: number, files: number, clusters: number, };

export type Resolved = { op: string, head: string, commits: Array<string>, };

export type ViewRef = { id: string, spec: JsonValue, resolved: Resolved, status: string, layoutVersion: number, 
/**
 * Range views: the snapshot view (of the range tip) whose placement this view reuses.
 */
sharedPlacement?: string, stats: ViewStats, warnings: Array<JsonValue>, };

/**
 * How far the churn scan went and where its grade bands lie (ViewBootstrap.churn).
 */
export type ChurnSummary = { head: string, scannedCommits: number, oldest: string, 
/**
 * Share of change mass in settled files, 0..1.
 */
settledMass: number, 
/**
 * "settled" | "max-commits" | "max-age" | "history-exhausted".
 */
stop: string, halfLifeDays: number, 
/**
 * Ascending rate thresholds between grades.
 */
bands: [number, number, number, number], 
/**
 * The highest file rate: the top of the Hot grade on the continuous heat scale.
 */
topRate?: number, };

export type ViewBootstrap = { view: ViewRef, world: Rect, laneDefs: Array<LaneDef>, lanes: Array<LaneBand>, columns: Array<GridColumn>, clusters: Array<Cluster>, hubs: Array<Hub>, bundles: Array<EdgeBundle>, churn?: ChurnSummary, heatLabels: [string, string, string, string, string], 
/**
 * Folders layout: the directory frames, outermost first. Absent from the district layout.
 */
folders?: Array<FolderFrame>, };

/**
 * Heat.inRange.
 */
export type InRange = { commits: number, of: number, };

export type Heat = { grade: number, rate: number, ci: [number, number], settled: boolean, touches: number, 
/**
 * Range views only: how many commits of the range touch this window.
 */
inRange?: InRange, };

export type Connectedness = { grade: number, dependents2: number, fanIn: number, fanOut: number, tests: number, reach: number, 
/**
 * Size of the dependency cycle this window's file belongs to, when > 1 (model.ts `cycle?`).
 */
cycle?: number, };

export type SymbolRef = { name: string, qualname: string, kind: string, };

export type LineSpan = { start: number, end: number, fileLines: number, };

/**
 * The span a window showed before it was expanded (`WindowHeader.expandedFrom`).
 */
export type LineRange = { start: number, end: number, };

export type FileFragment = { index: number, of: number, chip: number, siblings: Array<string>, };

export type WindowCounts = { openThreads: number, ports: number, conflicts: number, };

export type WindowFlags = { pinned: boolean, truncated: boolean, };

/**
 * WindowHeader.diffStat (range views): lines added and removed over the range.
 */
export type DiffStat = { added: number, removed: number, };

export type WindowHeader = { id: string, key: string, kind: string, cluster: string, lane: string, path: string, lang: string, rev: string, change: string, blob: string, symbol?: SymbolRef, lines: LineSpan, rect: Rect, heat: Heat, conn: Connectedness, risk: number, reviewFirst: boolean, fragment?: FileFragment, counts: WindowCounts, flags: WindowFlags, 
/**
 * Range views: lines added and removed over the range.
 */
diffStat?: DiffStat, 
/**
 * Set while the window is expanded: the span it showed before (POST …/collapse restores it).
 */
expandedFrom?: LineRange, };

/**
 * Window-to-window link geometry for L1/L2 edges (additive to TileResponse). Endpoints are world
 * coordinates of the out and in ports, so an edge draws even when only one end's tile is loaded.
 * A tile carries every edge with either end inside it; clients de-duplicate by id.
 */
export type TileEdge = { id: string, kind: string, from: string, to: string, fromCluster: string, toCluster: string, a: [number, number], b: [number, number], confidence: number, };

export type TileResponse = { layoutVersion: number, windows: Array<WindowHeader>, edges: Array<TileEdge>, };

export type WindowBatchRequest = { ids: Array<string>, parts: Array<string>, layoutVersion: number, };

export type Heading = { line: number, depth: number, label: string, };

export type Port = { id: string, side: string, line: number, label: string, kind: string, links: Array<string>, };

export type WindowOutline = { id: string, silhouette: string, headings: Array<Heading>, ports: Array<Port>, };

export type Link = { id: string, kind: string, from: SiteRef, to: JsonValue, label: string, confidence: number, provenance: string, 
/**
 * "confirmed" | "rejected": human or agent rating (rate_link); overrides the score.
 */
feedback?: string, 
/**
 * Declared links (declare_link): the declarer's note.
 */
note?: string, };

export type WindowBatchResponse = { outlines: Array<WindowOutline>, diffs: Array<JsonValue>, links: Array<Link>, missing: Array<string>, };

export type BlobText = { blob: string, lang: string, from: number, to: number, lines: Array<string>, tokens: Array<Array<number>>, };
