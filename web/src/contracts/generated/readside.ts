// Generated from crates/cv-contracts by scripts/gen-contracts.sh (ts-rs). Do not edit.
// The hand-written spec is docs/contracts/*.ts; check.ts compares the two.
import type { JsonValue } from "./json";
import type { SiteRef, Heat, Connectedness } from "./wire";

export type Signature = { name: string, email: string, 
/**
 * ISO-8601 (ids.ts).
 */
time: string, };

export type RevisionFlags = { conflict: boolean, empty: boolean, immutable: boolean, divergent: boolean, hidden: boolean, workingCopy: boolean, };

export type ShortestPrefix = { change: number, commit: number, };

export type Revision = { change: string, commit: string, parents: Array<string>, description: string, author: Signature, committerTime: string, bookmarks: Array<string>, tags: Array<string>, workspaces: Array<string>, flags: RevisionFlags, shortest: ShortestPrefix, };

/**
 * Review overlay drawn on the history panel.
 */
export type RevisionReview = { change: string, openThreads: number, reviewedAt?: string, changedSinceReview: boolean, touchedByAgent: boolean, };

export type GraphEdge = { toLane: number, parent: string, 
/**
 * Commits hidden on this edge (indirect edges only).
 */
elided?: number, };

export type GraphRow = { rev: Revision, lane: number, edges: Array<GraphEdge>, review?: RevisionReview, };

export type RevisionsResponse = { rows: Array<GraphRow>, truncated: boolean, };

export type WorkspaceWc = { change: string, commit: string, };

export type StackSummary = { revset: string, changes: number, files: number, conflicts: number, };

export type AgentBinding = { name: string, client?: string, activity: string, lastSeen: string, };

export type OpRef = { op: string, 
/**
 * ISO-8601 (ids.ts).
 */
time: string, description: string, };

export type WorkspaceOverlap = { workspace: string, paths: Array<string>, };

/**
 * Why a workspace card is flagged ([`WorkspaceSummary::stale`]).
 */
export type StaleReason = "working-copy" | "divergent";

export type WorkspaceStale = { reason: StaleReason, 
/**
 * What happened and the jj command that fixes it.
 */
note: string, 
/**
 * The other visible commits of `@`'s change (divergent), newest first; else empty.
 */
commits: Array<string>, };

export type WorkspaceSummary = { name: string, root: string, wc: WorkspaceWc, serving: boolean, stack: StackSummary, unsnapshottedFiles: number, 
/**
 * Bound by MCP (`bind_workspace`); absent until an agent binds.
 */
agent?: AgentBinding, lastOp: OpRef, overlaps: Array<WorkspaceOverlap>, 
/**
 * The stack's changed files (first 50), which the Workspaces card opens (model.ts `files?`;
 * always written, see ts-divergences.txt).
 */
files: Array<string>, 
/**
 * Set when an operation run elsewhere left this workspace stale or its `@` divergent and
 * recovering is the workspace owner's job (D12, DESIGN §6): cv never snapshots or updates
 * another workspace's files on its own.
 */
stale?: WorkspaceStale, };

export type RangeWarning = { "kind": "gap", missing: Array<string>, } | { "kind": "multiple-roots", changes: Array<string>, } | { "kind": "multiple-heads", changes: Array<string>, } | { "kind": "includes-immutable", changes: Array<string>, } | { "kind": "conflicted", changes: Array<string>, };

export type RevsetSummary = { ok: boolean, count: number, 
/**
 * First 50, newest first.
 */
changes: Array<string>, files: number, added: number, removed: number, conflicts: number, warnings: Array<RangeWarning>, };

export type RevsetError = { ok: boolean, error: string, hint?: string, };

export type RevsetValidation = RevsetSummary | RevsetError;

/**
 * Range view options (model.ts RangeOptions), with the documented defaults.
 */
export type RangeOptions = { 
/**
 * "cumulative" | "per-commit".
 */
diff: string, contextHops: number, 
/**
 * "minimized" | "hidden" | "expanded".
 */
contextDisplay: string, contextLines: number, ignoreWhitespace: boolean, 
/**
 * "districts" (the only layout; anything else is 400 "invalid").
 */
layout?: string, 
/**
 * Build a range that is not a chain anyway, shown per commit (D16; Compare uses it).
 */
allowNonChain?: boolean, };

export type SavedRange = { name: string, revset: string, fileset?: string, options?: RangeOptions, createdBy: JsonValue, };

export type DiffLine = { 
/**
 * "ctx" | "add" | "del".
 */
kind: string, oldLine?: number, newLine?: number, text?: string, owner?: string, };

export type WindowDiff = { id: string, lines: Array<DiffLine>, };

/**
 * One point of the Hotspots chart: x = connectedness percentile, y = heat percentile.
 */
export type MetricPoint = { key: string, label: string, lane: string, heat: Heat, conn: Connectedness, heatPct: number, connPct: number, 
/**
 * Review priority: √(heat pct × conn pct) × (0.5 + 0.5 × `outside`).
 */
risk: number, window?: string, 
/**
 * Share of its links leaving its module (its directory and below), 0..1; absent without
 * counted links or on the import-graph basis (whose risk is the plain geometric mean).
 */
outside?: number, };

export type CoChangePair = { a: string, b: string, support: number, jaccard: number, 
/**
 * The clusters of `a` and `b` (of each file's first window): the L0 co-change lens.
 */
clusterA?: string, clusterB?: string, };

/**
 * Clusters whose files change together (the co-change lens at L0, the level above file pairs):
 * support = commits touching both, jaccard = support / commits touching either. Unlike file
 * pairs, pairs joined by static links are kept; `linked` says whether one is.
 */
export type CoChangeClusterPair = { a: string, b: string, support: number, jaccard: number, linked: boolean, };

export type MetricsResponse = { points: Array<MetricPoint>, cochange: Array<CoChangePair>, cochangeClusters?: Array<CoChangeClusterPair>, };

/**
 * GET /views/:id/structure: how the folder tree fits the code's data flow and change frequency
 * (DESIGN §4.11). Links are distinct window pairs (an import counts only when the two files share
 * no other link); links from the test and docs lanes are left out.
 */
export type StructureResponse = { summary: StructureSummary, 
/**
 * Every directory holding counted files, each with everything below it, in path order.
 */
folders: Array<FolderStructure>, 
/**
 * Files with links or change, in path order.
 */
files: Array<FileStructure>, 
/**
 * Windows with links or tension, in window order.
 */
windows: Array<WindowStructure>, };

export type StructureSummary = { 
/**
 * Directories holding files of their own.
 */
folders: number, files: number, links: number, 
/**
 * Links whose two ends sit in different directories.
 */
crossFolderLinks: number, 
/**
 * Share of links inside one directory, 0..1.
 */
linkLocality: number, 
/**
 * How far the folder tree is from the dependency clusters of the district (change) layout:
 * 1 − normalised mutual information of the two groupings of windows, 0 = the same grouping.
 * Absent when no district layout of the repo is known yet.
 */
divergence?: number, 
/**
 * Share of windows whose dependency cluster is mostly another directory's code.
 */
misgrouped?: number, 
/**
 * Change-weighted mean of the directories' own heat tension, 0..1.
 */
heatTension: number, 
/**
 * Window-weighted mean of the directories' own flow pressure, 0..1.
 */
pressure: number, };

/**
 * A directory with everything below it, seen as a module.
 */
export type FolderStructure = { path: string, 
/**
 * Path components (0 for the repo root).
 */
depth: number, files: number, windows: number, 
/**
 * Links with both ends inside.
 */
internal: number, 
/**
 * Links from inside to outside, and from outside in.
 */
outgoing: number, incoming: number, 
/**
 * Flow pressure: boundary links / all links touching it, 0 (sealed) .. 1 (every link leaves).
 */
pressure: number, 
/**
 * Its depth as a module: links staying inside it (in it or below) / all links touching it,
 * 1 − pressure (0 when no link touches it).
 */
inside: number, 
/**
 * Windows inside that something outside links to: the module's interface.
 */
interface: number, 
/**
 * interface / windows: a narrow interface is small.
 */
interfaceShare: number, 
/**
 * Directories outside it that it links with (either way).
 */
partners: number, 
/**
 * Files inside with a link crossing its boundary: its gateways.
 */
gateways: number, 
/**
 * How the boundary links are spread over its files: (gateways − 1) / (files − 1), 0 when one
 * file (or none) carries them all, 1 when every file links outside.
 */
spread: number, 
/**
 * Longest chain of links inside it, in windows (cycles cut).
 */
chain: number, 
/**
 * Dependency clusters of the district layout its windows belong to.
 */
modules?: number, 
/**
 * Change mass (sum of file change rates).
 */
mass: number, 
/**
 * Heat tension: share of the change mass in warm or hot files × share of the lines in cool or
 * frozen files, 0..1. High when a few busy files live among code that rarely changes.
 */
tension: number, 
/**
 * Warm or hot files among cool or frozen siblings, busiest first (at most 5).
 */
hotFiles: Array<string>, };

export type FileStructure = { path: string, 
/**
 * Its directory.
 */
folder: string, lane: string, 
/**
 * File heat grade (0 frozen .. 4 hot) and rate.
 */
heat: number, rate: number, 
/**
 * Links to or from files of the same directory, and to or from other directories.
 */
internal: number, external: number, 
/**
 * Its windows that other directories link to.
 */
exports: number, 
/**
 * How much busier it is than its directory: 0..1, > 0 only for warm or hot files, the share
 * of its siblings' lines that are cool or frozen (warm files count 0.6).
 */
tension: number, 
/**
 * The other directory it has the most links with, when that beats its own directory (files
 * with siblings only).
 */
pull?: FolderPull, };

/**
 * A window's links against its directory, and its file's heat tension.
 */
export type WindowStructure = { key: string, path: string, 
/**
 * Links with windows of its directory or below it (its own file included).
 */
internal: number, 
/**
 * Links with siblings or parents of its directory (anything not below it), either way;
 * `incoming` of them come in.
 */
external: number, incoming: number, 
/**
 * Its file's heat tension (FileStructure.tension).
 */
tension: number, };

export type FolderPull = { folder: string, links: number, };

export type SearchTarget = { window?: string, site?: SiteRef, hub?: string, thread?: string, change?: string, };

export type SearchHit = { 
/**
 * "symbol" | "file" | "hub" | "thread" | "revision".
 */
kind: string, label: string, detail?: string, target: SearchTarget, score: number, };

/**
 * How far to widen a window: a line count, or "symbol" / "file".
 */
export type ExpandAmount = number | string;

export type ExpandRequest = { before?: ExpandAmount | null, after?: ExpandAmount | null, };
