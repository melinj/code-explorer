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
export type MetricPoint = { key: string, label: string, lane: string, heat: Heat, conn: Connectedness, heatPct: number, connPct: number, risk: number, window?: string, };

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
