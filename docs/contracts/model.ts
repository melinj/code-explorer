/**
 * Domain model shared by server, web client and MCP. Owned by C0; changes are additive within v1.
 */
import type {
  Actor,
  BlobId,
  ChangeId,
  ClusterId,
  CommentId,
  CommitId,
  EditId,
  Fileset,
  HubId,
  LaneId,
  LinkId,
  OpId,
  RepoPath,
  Revset,
  ThreadId,
  ViewId,
  WindowId,
  WindowKey,
  WorkspaceName,
} from "./ids";

export type Lang =
  | "python"
  | "typescript"
  | "tsx"
  | "javascript"
  | "rust"
  | "cpp"
  | "c"
  | "go"
  | "yaml"
  | "json"
  | "toml"
  | "markdown"
  | "other";

// ─── Revisions (jj) ──────────────────────────────────────────────────────────

export interface Revision {
  change: ChangeId;
  commit: CommitId;
  parents: CommitId[];
  description: string; // full text; UI shows the first line
  author: { name: string; email: string; time: string };
  committerTime: string;
  bookmarks: string[]; // "name" for local, "name@remote" for remote
  tags: string[];
  workspaces: WorkspaceName[]; // workspaces whose @ is this commit
  flags: RevisionFlags;
  shortest: { change: number; commit: number }; // unique-prefix lengths for display
}

export interface RevisionFlags {
  conflict: boolean;
  empty: boolean;
  immutable: boolean;
  divergent: boolean;
  hidden: boolean;
  workingCopy: boolean;
}

/** Review overlay drawn on the history panel. */
export interface RevisionReview {
  change: ChangeId;
  openThreads: number;
  reviewedAt?: CommitId; // commit the human last marked reviewed
  changedSinceReview: boolean; // the change was rewritten after reviewedAt (offer interdiff)
  touchedByAgent: boolean;
}

/** One row of the history graph. Lanes are computed server-side so every client draws the same graph. */
export interface GraphRow {
  rev: Revision;
  lane: number;
  edges: Array<{ toLane: number; parent: CommitId; elided?: number /* commits hidden on this edge */ }>;
  review?: RevisionReview;
}

export interface WorkspaceInfo {
  name: WorkspaceName;
  root: string; // absolute path of the workspace checkout
  wc: { change: ChangeId; commit: CommitId };
}

/** Workspaces are first-class: one card per workspace in the Workspaces tab. */
export interface WorkspaceSummary extends WorkspaceInfo {
  serving: boolean; // the workspace `cv serve` runs in: shown as "you"
  stack: { revset: Revset; changes: number; files: number; conflicts: number }; // trunk()..<name>@
  unsnapshottedFiles: number;
  agent?: { name: string; client?: string; activity: "reading" | "editing" | "idle"; lastSeen: string };
  lastOp: { op: OpId; time: string; description: string };
  /**
   * Other workspaces whose stacks change the same files: likely conflicts when they land. Counted on
   * each stack's own commits (not ancestors of the other's @), so history both share, e.g. all of it
   * when there is no remote and trunk() is root(), is no overlap.
   */
  overlaps: Array<{ workspace: WorkspaceName; paths: RepoPath[] }>;
  files?: RepoPath[]; // the stack's changed files (first 50); "Open @" opens a snapshot of `<name>@` with its live files (SnapshotOptions.workspace), then flies to the first
  /**
   * An operation run elsewhere left this workspace needing its owner (D12, §6; cv never updates
   * another workspace's files): "working-copy" = the files on disk belong to an older @ (`jj
   * workspace update-stale` there); "divergent" = @'s change has other visible commits (`commits`).
   */
  stale?: { reason: "working-copy" | "divergent"; note: string; commits: CommitId[] };
}

// ─── Graph view: bookmarks, landing, history edits (jj) ─────────────────────

/** A local bookmark and the remote bookmarks of the same name (`@git` left out). */
export interface BookmarkInfo {
  name: string;
  targets: CommitId[]; // more than one: conflicted; none: deleted locally but still on a remote
  /** ahead / behind: tracked, one target on each side; commits only local / only on the remote (capped at 1000). */
  remotes: Array<{ remote: string; targets: CommitId[]; tracked: boolean; ahead?: number; behind?: number }>;
}

/** What a "land" operation brings onto the trunk bookmark; exactly one field. */
export interface LandSource {
  workspace?: WorkspaceName; // its @, or @'s parent when @ is an empty undescribed scratch commit (which then follows)
  bookmark?: string;
  commit?: CommitId;
}

/**
 * One operation from the Graph view; each is one jj operation, undone with POST /graph:undo. None
 * moves the serving workspace's @ on purpose ("new" is `jj new --no-edit`).
 */
export type GraphOp =
  | { kind: "set-bookmark"; name: string; to: CommitId; allowBackwards?: boolean } // jj bookmark set (creates or moves)
  | { kind: "delete-bookmark"; name: string } // jj bookmark delete
  | {
      kind: "land";
      source: LandSource;
      trunk: string; // a local bookmark
      strategy: "fast-forward" | "rebase" | "merge"; // ff when possible whatever the strategy
      message?: string; // merge commit description
      deleteBookmark?: string; // delete it afterwards (the landed feature bookmark)
    }
  | { kind: "rebase"; source: CommitId; mode: "revision" | "source" | "branch"; dest: CommitId[] } // jj rebase -r | -s | -b
  | { kind: "describe"; commit: CommitId; message: string }
  | { kind: "abandon"; commits: CommitId[] }
  | { kind: "new"; parents: CommitId[]; message?: string } // jj new --no-edit
  | { kind: "commit"; workspace: WorkspaceName; message: string; bookmark?: string } // jj commit in that workspace (@ described, a new empty @ on top); bookmark created or moved forward to the committed change
  | { kind: "squash"; source: CommitId; into?: CommitId; message?: string }; // jj squash --from source --into (default: its only parent); no message: the two descriptions combined (destination, blank line, source)

/** One jj operation (GET /ops), newest first. user: user@host. */
export interface OpEntry {
  op: OpId;
  parents: OpId[];
  time: string;
  description: string;
  user: string;
  snapshot: boolean;
}

export interface OpLog {
  ops: OpEntry[];
  truncated: boolean;
  current: OpId;
}

/** GET /ops/:op: what an operation changed, against its first parent. */
export interface OpDetail {
  op: OpId;
  changed: ChangeId[];
  abandoned: ChangeId[];
  bookmarks: Array<{ name: string; from?: CommitId; to?: CommitId }>;
}

export interface RestoreRequest {
  op: OpId;
  dryRun?: boolean;
}

export interface FetchRequest {
  remote?: string; // jj's default remotes when absent
}

export interface PushRequest {
  bookmarks: string[];
  remote?: string;
  dryRun?: boolean; // jj git push --dry-run: output says what would be pushed
}

/** A fetch or push: what jj printed and which bookmarks moved (`name`, or `name@remote` for remote-tracking ones). */
export interface RemoteResult {
  status: "previewed" | "applied" | "failed";
  summary: string;
  output: string;
  moved: Array<{ name: string; from?: CommitId; to?: CommitId }>;
  opAfter?: OpId;
  error?: { code: string; message: string };
}

export interface GraphOpRequest {
  op: GraphOp;
  dryRun?: boolean; // exact preview: nothing is written
}

export interface GraphOpResult {
  status: "previewed" | "applied" | "rejected" | "failed";
  actor: Actor;
  summary: string; // one line: what it did (or would do)
  opBefore?: OpId;
  opAfter?: OpId; // undo: POST /graph:undo { op: opAfter }
  created?: ChangeId;
  rewritten: ChangeId[];
  bookmarks: Array<{ name: string; from?: CommitId; to?: CommitId }>; // created, moved or deleted local bookmarks
  workspaces: Array<{ name: WorkspaceName; snapshotted: boolean; updated: boolean; recoveredInto?: CommitId }>;
  conflicts: ConflictRecord[]; // new conflicts (predicted for a preview)
  divergent: ChangeId[];
  error?: { code: EditErrorCode | "invalid"; message: string };
}

// ─── Views ───────────────────────────────────────────────────────────────────

export type ViewSpec =
  /**
   * rev resolves to one commit; default "@", or "<workspace>@" when options.workspace is set (the
   * ViewRef's spec spells it out). fileset: only the files it matches are indexed and shown (jj
   * fileset syntax). Specs at the same commit share a view only when fileset, liveWorkingCopy and
   * the overlaid workspace agree.
   */
  | { mode: "snapshot"; rev?: Revset; fileset?: Fileset; options?: SnapshotOptions }
  | { mode: "range"; revset: Revset; fileset?: Fileset; options?: RangeOptions };

/**
 * districts = lanes × heat columns × connectedness rows (D8), the default. folders = each lane's
 * folder tree, one cluster per directory, in path order (the None and Tension lenses); flow = the
 * same tree with each directory's items ordered by their links, linked ones side by side (the Depth
 * lens). The trees have no pins, labels or moves. An unspecified "dependency" value was dropped (DESIGN §4.9 "Layouts considered"): any
 * other value is 400 "invalid".
 */
export type ViewLayout = "districts" | "folders" | "flow";

export interface SnapshotOptions {
  /**
   * Workspace whose unsnapshotted files are overlaid live; defaults to the workspace whose @ is `rev`
   * (the served one first). Must exist (else 400 "invalid"). Without `rev`, the view shows its @.
   */
  workspace?: WorkspaceName;
  /** Overlay files edited on disk but not yet snapshotted by jj. Default true. */
  liveWorkingCopy?: boolean;
  layout?: ViewLayout;
}

/**
 * Range views keep the full view's map: same lanes, heat columns, connectedness rows and cluster
 * positions as the snapshot of the range tip. Windows are diff-focused hunks placed where their symbol
 * sits in the full view; unchanged clusters are not drawn (faint outlines at L0/L1 keep orientation).
 */
/** Missing fields take the defaults; a wrong type, an unknown field or a value outside these is 400 "invalid". */
export interface RangeOptions {
  diff: "cumulative" | "per-commit";
  contextHops: 0 | 1 | 2; // unchanged callers/callees of changed code, as context windows
  /** How context windows start: minimized = header-only pill, expandable on click (default). */
  contextDisplay: "minimized" | "hidden" | "expanded";
  contextLines: number; // default 3, at most 1000
  ignoreWhitespace: boolean;
  layout?: ViewLayout;
  /**
   * Additive. A range must be a chain (one root, one head; D16): otherwise POST /views answers
   * 400 "range-not-chain" with error.details.hint. true builds it anyway, shown per commit
   * (diff is forced to "per-commit"); the multiple-roots/heads warnings stay on the ViewRef.
   */
  allowNonChain?: boolean;
}

export interface ViewRef {
  id: ViewId;
  spec: ViewSpec;
  resolved: {
    op: OpId;
    head: CommitId;
    base?: CommitId; // range mode: parent of the chain's root
    commits: CommitId[]; // range members, newest first (snapshot: [head])
  };
  status: "ready" | "indexing" | "stale"; // stale = a newer op changed the resolution; client re-POSTs the spec
  layoutVersion: number;
  /** Range views: the snapshot view (of the range tip) whose placement this view reuses. */
  sharedPlacement?: ViewId;
  stats: { windows: number; files: number; clusters: number; added?: number; removed?: number };
  warnings: RangeWarning[];
}

export type RangeWarning =
  | { kind: "gap"; missing: ChangeId[] } // selection is not convex; offer "fill gap" or per-commit
  | { kind: "multiple-roots" | "multiple-heads"; changes: ChangeId[] }
  | { kind: "includes-immutable"; changes: ChangeId[] }
  | { kind: "conflicted"; changes: ChangeId[] };

// ─── Canvas geometry ─────────────────────────────────────────────────────────

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Semantic zoom tiers, chosen by on-screen line height h = 18·zoom px, with ±10% hysteresis. */
export type ZoomTier =
  | "L0" // galaxy: zoom < 0.03 — clusters, bundled edges, hubs
  | "L1" // map: 0.03–0.09 — window tiles, titles, edges without labels
  | "L2" // outline: 0.09–0.5 — silhouettes, symbol headings, port pills with labels
  | "L3"; // code: ≥ 0.5 — text, tokens, inline threads, editable on activation

export interface Viewport {
  rect: Rect; // world-space rectangle on screen
  zoom: number; // screen px per wu
  tier: ZoomTier;
}

// ─── Heat (change frequency) and connectedness: the two axes ────────────────

/** 0 frozen · 1 cool · 2 mild · 3 warm · 4 hot. Change-mass quantiles, never fixed limits. */
export type HeatGrade = 0 | 1 | 2 | 3 | 4;

/** 0 isolated · 1 low · 2 medium · 3 high · 4 core. Rank quantiles of `dependents2`. */
export type ConnGrade = 0 | 1 | 2 | 3 | 4;

export interface Connectedness {
  grade: ConnGrade;
  /** Windows that depend on this one directly or through one intermediary (code lane, tests excluded). */
  dependents2: number;
  fanIn: number;
  fanOut: number;
  /** Direct test dependents, shown as "covered by N tests" instead of inflating connectedness. */
  tests: number;
  /** Transitive dependents; a detail only (saturates in import cycles). */
  reach: number;
  /** Size of the import cycle this window's file belongs to, when > 1. */
  cycle?: number;
}

export interface Heat {
  grade: HeatGrade;
  rate: number; // recency-weighted touches per commit
  ci: [number, number]; // Wilson 95% interval on rate, using the effective sample size
  settled: boolean; // ci lies inside one grade band
  touches: number; // raw touches in the scan window
  /** Range mode only: how many commits of the range touch this window. Drives range layout. */
  inRange?: { commits: number; of: number };
}

export interface ChurnSummary {
  head: CommitId;
  scannedCommits: number;
  oldest: string;
  settledMass: number; // share of change mass in settled files, 0..1
  stop: "settled" | "max-commits" | "max-age" | "history-exhausted";
  halfLifeDays: number;
  /** Ascending rate thresholds between grades: [frozen|cool, cool|mild, mild|warm, warm|hot]. */
  bands: [number, number, number, number];
  /** Additive: the highest file rate, the top of Hot on the continuous heat scale. */
  topRate?: number;
}

// ─── Clusters, windows, symbols ──────────────────────────────────────────────

export interface Cluster {
  id: ClusterId;
  parent?: ClusterId; // two levels: subsystem > module (subsystem ids "s…" are not listed in clusters[] yet)
  lane: LaneId;
  label: string;
  labelSource: "auto" | "agent" | "human";
  summary?: string; // agent-written one-liner, optional
  rect: Rect;
  /** District cell = (heat column, conn row) within the lane; changes only when a grade settles. */
  heat: HeatGrade;
  conn: ConnGrade;
  windows: number;
  openThreads: number;
  conflicts: number;
  /** Additive: pinned by PATCH /views/:id/layout (`cluster:<id>`, pinned: true); keeps its spot on re-index. */
  pinned?: boolean;
  /** Additive: heat on a continuous 0..1 scale inside its grade (grade g spans [g/5, (g+1)/5)): the colour ramp's position. */
  heatLevel?: number;
  /** Additive: share of the cluster's windows that are review first (WindowHeader.reviewFirst), 0..1: the L0 Hotspots lens. */
  review?: number;
  /** Folders layout: the directory whose own files the cluster holds ("" = repo root); a large one is split into several clusters. */
  folder?: RepoPath;
}

/** Folders layout: a directory's frame (with everything below it). A directory with no files of its own and one subdirectory folds into it (label "a/b"). */
export interface FolderFrame {
  path: RepoPath;
  label: string; // the path below the enclosing frame (or the lane)
  lane: LaneId;
  depth: number; // nesting inside its lane, 0 = outermost
  rect: Rect;
}

// ─── Lanes and the district grid ─────────────────────────────────────────────

/** Lanes are told apart by texture and a label rail, never by hue (hue is heat). */
export interface LaneDef {
  id: LaneId;
  label: string;
  order: number;
  texture: "plain" | "hatch" | "dots" | "lines" | "cross";
  collapsed: boolean; // docs and other start collapsed
  hidden: boolean;
}

/** Geometry of one lane in the district grid (T0). Rows are per lane; columns are shared. */
export interface LaneBand {
  lane: LaneId;
  rect: Rect;
  files: number; // shown on the lane rail
  clusters: number;
  rows: Array<{ conn: ConnGrade; y: number; h: number }>; // empty rows collapse to h ≈ 0
}

export interface GridColumn {
  heat: HeatGrade;
  x: number;
  w: number;
  clusters: number; // shown in the column header
}

/** One point of the Hotspots chart: x = connectedness percentile, y = heat percentile. */
export interface MetricPoint {
  key: WindowKey | ClusterId;
  label: string;
  lane: LaneId;
  heat: Heat;
  conn: Connectedness;
  heatPct: number;
  connPct: number;
  risk: number; // review priority: √(heatPct × connPct) × (0.5 + 0.5 × outside), 0..1
  window?: WindowId;
  /** Share of its links leaving its module (its directory and below), 0..1; absent without links or on the import-graph basis (plain geometric mean). */
  outside?: number;
}

/**
 * GET /views/:id/structure (DESIGN §4.11): how the folder tree fits the data flow and the change
 * frequency. Links are distinct window pairs (an import counts only when the files share no other
 * link); links from the test and docs lanes are left out.
 */
export interface StructureResponse {
  summary: StructureSummary;
  folders: FolderStructure[]; // every directory holding counted files, with everything below it, in path order
  files: FileStructure[]; // files with links or change, in path order
  windows: WindowStructure[]; // windows with links or tension, in window order
}

/** A window's links against its directory, and its file's heat tension. */
export interface WindowStructure {
  key: WindowKey;
  path: RepoPath;
  internal: number; // links with windows of its directory or below it (its own file included)
  external: number; // links with siblings or parents of its directory, either way
  incoming: number; // of those, coming in
  tension: number; // its file's heat tension
}

export interface StructureSummary {
  folders: number; // directories holding files of their own
  files: number;
  links: number;
  crossFolderLinks: number;
  linkLocality: number; // share of links inside one directory, 0..1
  /** 1 − normalised mutual information of directories vs the district layout's dependency clusters (0 = same grouping); absent before a district layout exists. */
  divergence?: number;
  /** Share of windows whose dependency cluster is mostly another directory's code. */
  misgrouped?: number;
  heatTension: number; // change-weighted mean of the directories' own heat tension, 0..1
  pressure: number; // window-weighted mean of the directories' own flow pressure, 0..1
}

/** A directory with everything below it, seen as a module. */
export interface FolderStructure {
  path: RepoPath;
  depth: number;
  files: number;
  windows: number;
  internal: number; // links with both ends inside
  outgoing: number;
  incoming: number;
  pressure: number; // boundary links / all links touching it: 0 sealed .. 1 every link leaves
  inside: number; // its depth as a module: links staying in it or below / all links touching it (1 − pressure; 0 without links)
  interface: number; // windows inside linked from outside
  interfaceShare: number; // interface / windows: a narrow interface is small
  partners: number; // directories outside it linked either way
  gateways: number; // files inside with a link crossing its boundary
  /** (gateways − 1) / (files − 1): 0 when one file (or none) carries every boundary link, 1 when every file links outside. */
  spread: number;
  chain: number; // longest chain of links inside, in windows (cycles cut)
  modules?: number; // dependency clusters (district layout) its windows belong to
  mass: number; // sum of file change rates
  /** Share of change mass in warm or hot files × share of lines in cool or frozen files, 0..1. */
  tension: number;
  hotFiles: RepoPath[]; // warm or hot files among quiet siblings, busiest first (≤ 5)
}

export interface FileStructure {
  path: RepoPath;
  folder: RepoPath;
  lane: LaneId;
  heat: HeatGrade;
  rate: number;
  internal: number; // links with files of its directory
  external: number; // links with other directories
  exports: number; // its windows other directories link to
  /** > 0 for warm or hot files only: share of its siblings' lines that are cool or frozen (warm files × 0.6). */
  tension: number;
  /** The directory it has the most links with, when that beats its own (≥ 2 links; files with siblings only). */
  pull?: { folder: RepoPath; links: number };
}

/** Files that change together without a static link (co-change lens). */
export interface CoChangePair {
  a: RepoPath;
  b: RepoPath;
  support: number;
  jaccard: number;
  /** The clusters of a and b (each file's first window): the L0 co-change lens. */
  clusterA?: ClusterId;
  clusterB?: ClusterId;
}

/**
 * Clusters whose files change together (co-change lens at L0, the level above file pairs): support =
 * commits touching both, jaccard = support / commits touching either. Pairs joined by static links
 * are kept; `linked` says whether one is.
 */
export interface CoChangeClusterPair {
  a: ClusterId;
  b: ClusterId;
  support: number;
  jaccard: number;
  linked: boolean;
}

export type SymbolKind =
  | "function"
  | "method"
  | "class"
  | "interface"
  | "struct"
  | "enum"
  | "module"
  | "constant"
  | "type"
  | "test"
  | "config-block";

export type WindowKind =
  | "symbol" // one declaration
  | "chunk" // several small declarations, or one part of an oversized one
  | "hunk" // range mode: merged diff hunks, widened to the enclosing symbol when it fits
  | "conflict" // a conflicted region of a file at a conflicted commit
  | "context"; // range mode: unchanged caller/callee, drawn dimmed

/** T1 payload: everything needed to draw a window frame before any content arrives. */
export interface WindowHeader {
  id: WindowId;
  key: WindowKey;
  kind: WindowKind;
  cluster: ClusterId;
  lane: LaneId;
  path: RepoPath;
  lang: Lang;
  rev: CommitId;
  change: ChangeId;
  blob: BlobId; // content shown; fetch text from GET /blobs/{blob}/text
  symbol?: { name: string; qualname: string; kind: SymbolKind };
  /** Lines shown (≤ 500 unless expanded on purpose); fileLines drives the fade hints. */
  lines: { start: number; end: number; fileLines: number };
  rect: Rect;
  heat: Heat;
  conn: Connectedness;
  risk: number; // 0..1; top 5% of a lane are "review first"
  reviewFirst: boolean;
  /** Set when the file is split into several windows: drives the file chip. */
  fragment?: FileFragment;
  counts: { openThreads: number; ports: number; conflicts: number };
  flags: {
    /** Set by PATCH /views/:id/layout (pinned: true); the window keeps this rect origin across re-indexes and restarts. */
    pinned: boolean;
    truncated: boolean;
    unsnapshotted?: boolean; // edited on disk in the viewed workspace ("edited, live"); the header's blob is then the live text ("live…" id)
    alsoChangedIn?: WorkspaceName[]; // other workspaces whose stacks change these lines (overlap chip)
    minimized?: boolean; // range-view context window drawn as a header-only pill until expanded
  };
  /** Range-view hunk windows: lines added and removed in this window (header "+12 −3"). */
  diffStat?: { added: number; removed: number };
  /** Set while the window is expanded: the span it showed before (POST …/collapse restores it). */
  expandedFrom?: { start: number; end: number };
}

/** Fragments of one file attract each other in layout; the chip selects all of them. */
export interface FileFragment {
  index: number; // 1-based
  of: number;
  chip: number; // slot in the 8-colour file-chip palette (wire.ts FILE_CHIP_SLOTS), stable per path
  siblings: WindowId[];
}

/** T2 payload: silhouette placeholder, headings and ports. */
export interface WindowOutline {
  id: WindowId;
  silhouette: string; // base64 of OutlineEncoding records (wire.ts), one per displayed line
  headings: Array<{ line: number; depth: number; label: string }>;
  ports: Port[];
}

/** Range-mode overlay for a hunk window. Deleted lines carry their text (it is not in the tip blob). */
export interface WindowDiff {
  id: WindowId;
  lines: DiffLine[];
}

export interface DiffLine {
  kind: "ctx" | "add" | "del";
  oldLine?: number;
  newLine?: number;
  text?: string; // only for "del"
  owner?: ChangeId; // commit of the range that last touched the line (in-range blame)
}

export interface FileDiff {
  path: RepoPath;
  oldPath?: RepoPath;
  status: "added" | "modified" | "removed" | "renamed" | "copied";
  hunks: Array<{ old: [number, number]; new: [number, number]; lines: DiffLine[] }>;
  conflicted?: true; // conflicted at the head: no hunks (MCP get_diff)
}

// ─── Links, ports, hubs ──────────────────────────────────────────────────────

export type LinkKind =
  | "call"
  | "inherit"
  | "implement"
  | "import"
  | "reference"
  | "publish" // producer site → hub
  | "subscribe" // hub → subscription site
  | "dispatch" // subscription site → handler definition
  | "http-request" // client call → endpoint hub
  | "http-route" // endpoint hub → route handler
  | "config" // hub ↔ config file entry (e.g. YAML `topic:`)
  | "define" // config/infra defines a key (env var, setting) → key hub
  | "read" // key hub → code that reads the key
  | "deploy" // infra runs or builds code: Dockerfile CMD, compose command, CI run:
  | "ipc"; // any other channel

/** One per channel pack; packs are plugins, so this list grows with them. */
export type Protocol = "bus" | "http" | "env" | "deploy" | "grpc" | "ipc" | "config";

/** A code location; `window` is set when the location is inside a window of the current view. */
export interface SiteRef {
  path: RepoPath;
  line: number;
  col?: [number, number];
  symbol?: string;
  window?: WindowId;
}

export interface Link {
  id: LinkId;
  kind: LinkKind;
  from: SiteRef;
  /** `dynamic`: there is a link here but its target is only known at runtime (no tracing, by design). */
  to: SiteRef | { hub: HubId } | { dynamic: { protocol: Protocol; hint: string; partial?: string } };
  label: string;
  confidence: number; // 0..1; below 0.6 drawn faded with "?"
  provenance: "tree-sitter" | "scip" | "agent" | "human" | `rule:${string}`; // "human": declared from the UI (additive)
  feedback?: "confirmed" | "rejected";
  note?: string; // declared links: the declarer's note (additive)
}

/**
 * A port is where an edge meets a window: outgoing on the right edge at the callsite line,
 * incoming on the left edge at the definition line. Several links from one line share a port (×N).
 */
export interface Port {
  id: string; // unique within the window
  side: "in" | "out";
  line: number;
  label: string; // "→ EventBus.emit", "⇢ world.entity.moved", "⇢ GET /api/node/{}", "← 3 callers"
  kind: LinkKind;
  links: LinkId[];
}

/** Many-to-many channel drawn as a node: event topic, HTTP endpoint, IPC channel. */
export interface Hub {
  id: HubId;
  protocol: Protocol;
  key: string; // normalized: "world.entity.moved", "GET /api/node/{}", "capacity"
  /** Config hubs: the file family the key belongs to, e.g. "node.yaml" (one hub per family and key). */
  family?: string;
  display: string;
  producers: SiteRef[];
  consumers: SiteRef[];
  rect?: Rect;
  /** Drawn hubs (additive): world points of the producers' out ports, then the consumers' in ports. */
  anchors?: Array<[number, number]>;
}

/** L0/L1 aggregate of links between two clusters. */
export interface EdgeBundle {
  from: ClusterId;
  to: ClusterId;
  kinds: Partial<Record<LinkKind, number>>;
  weight: number;
}

// ─── Review ──────────────────────────────────────────────────────────────────

export type ThreadStatus =
  | "open"
  | "in-progress" // an agent picked it up
  | "addressed" // the agent produced a change and replied; waiting for the human
  | "resolved"
  | "outdated" // anchored lines changed underneath the thread
  | "wontfix";

export type ThreadKind = "comment" | "suggestion" | "question" | "task" | "finding";

/** Threads anchor to a change, not just a commit, so they follow the change through rewrites. */
export interface Anchor {
  change: ChangeId;
  commit: CommitId; // commit the anchor was last confirmed against
  path: RepoPath;
  side: "new" | "old";
  start: number;
  end: number;
  cols?: [number, number];
  fingerprint: { text: string; before: string; after: string }; // hashes of anchored lines and 2 lines either side
}

export interface Thread {
  id: ThreadId;
  anchor: Anchor;
  status: ThreadStatus;
  kind: ThreadKind;
  severity?: "info" | "minor" | "major" | "blocker";
  assignee?: "agent" | "human";
  createdBy: Actor;
  createdAt: string;
  updatedAt: string;
  comments: Comment[];
  producedEdits: EditId[];
  outdated?: { reason: "lines-changed" | "file-removed" | "change-abandoned"; lastGood: CommitId };
}

export interface Comment {
  id: CommentId;
  author: Actor;
  body: string; // markdown; may contain ```suggestion fenced blocks (replacement for the anchored lines)
  createdAt: string;
  editedAt?: string;
}

export interface ThreadSummary {
  id: ThreadId;
  status: ThreadStatus;
  kind: ThreadKind;
  assignee?: "agent" | "human";
  path: RepoPath;
  start: number;
  end: number;
  comments: number;
  preview: string; // first line of the first comment
}

// ─── Edits and conflicts ─────────────────────────────────────────────────────

/** Default: amend whenever the target is mutable; branch for immutable targets; absorb in range views. */
export type EditMode =
  | "amend" // rewrite the target; descendants rebase
  | "insert-after" // new change between target and its children; descendants rebase
  | "branch" // new change on top of target as a new head; nothing rebases
  | "keep-descendants" // amend target but keep descendants' content (jj --restore-descendants)
  | "absorb"; // write on top of the range tip, then jj absorb into the commits that own the lines

/**
 * Emphasis lenses change opacity and weight. Each also picks the layout of a snapshot: "none" and
 * "tension" (busy files among quiet ones) show the folder tree (ViewLayout "folders"), "depth" (the
 * share of each directory's links that stay in it or below) the tree ordered by links (ViewLayout
 * "flow"); the others the district grid. "hotspots" is labelled Review priority.
 */
export type Lens = "none" | "heat" | "connectedness" | "hotspots" | "cochange" | "tension" | "depth";

/** Replace lines [start, end] (1-based, inclusive). end = start - 1 inserts before start. */
export interface LinePatch {
  start: number;
  end: number;
  text: string;
}

export interface EditRequest {
  target: ChangeId | CommitId; // for absorb: the range tip
  files: Array<{
    path: RepoPath;
    baseBlob?: BlobId; // optimistic concurrency: reject with "stale-base" if the file moved on
    content?: string; // whole new content, or…
    patches?: LinePatch[]; // …line patches against baseBlob
  }>;
  mode: EditMode;
  message?: string; // description for created changes
  absorbInto?: Revset; // mode "absorb": candidate commits, usually the range
  dryRun: boolean;
  origin?: { thread?: ThreadId };
}

export interface EditResult {
  id: EditId;
  status: "previewed" | "applied" | "rejected" | "failed" | "undone";
  actor: Actor;
  opBefore?: OpId;
  opAfter?: OpId;
  created?: ChangeId;
  rewritten: ChangeId[]; // target and every rebased descendant
  workspaces: Array<{ name: WorkspaceName; snapshotted: boolean; updated: boolean; recoveredInto?: CommitId }>;
  conflicts: ConflictRecord[]; // new conflicts; predicted when status = "previewed"
  predicted: boolean;
  error?: { code: EditErrorCode; message: string };
  /** Applied edits: each edited file's blob in the commit now holding the edit (absent for absorb). */
  files?: Array<{ path: RepoPath; newBlob?: BlobId }>;
  /** Additive (DESIGN §6): e.g. an affected workspace with an agent attached (in the preview). */
  warnings?: EditWarning[];
  /** Additive: changes left with more than one visible commit (normally absent: undo refuses instead). */
  divergent?: ChangeId[];
}

export interface EditWarning {
  code: "agent-attached" | "divergent" | "workspace-recovered";
  message: string;
  workspace?: WorkspaceName;
  agent?: Actor; // "agent-attached": the agent bound to `workspace`
  changes?: ChangeId[];
}

/** ApiError.details of 409 "undo-blocked" (POST /edits/:id/undo): nothing was written. */
export interface UndoBlocked {
  edit: EditId;
  op: OpId; // the edit's opAfter, which undo would revert
  laterOps: Array<{ op: OpId; description: string; time: string; user: string; changes: ChangeId[] }>; // newest first
  divergent: ChangeId[]; // changes the revert would leave divergent
  conflicted: ConflictRecord[]; // conflicts the revert would introduce
  suggestion: string;
}

export type EditErrorCode =
  | "immutable"
  | "stale-base"
  | "invalid-target"
  | "locked" // another mutation holds the queue longer than the client timeout
  | "jj-error"
  | "undo-blocked"; // additive: see UndoBlocked

export interface ConflictRecord {
  change: ChangeId;
  commit: CommitId;
  path: RepoPath;
  sides: number;
  firstSeenOp: OpId;
  introducedBy?: EditId;
}

export interface ConflictDetail extends ConflictRecord {
  materialized: string; // file text with jj conflict markers (labels name the changes)
  regions: Array<{
    start: number; // 1-based lines of `materialized`, marker lines included
    end: number;
    base?: string;
    sides: Array<{ label: string; text: string }>;
  }>;
}
