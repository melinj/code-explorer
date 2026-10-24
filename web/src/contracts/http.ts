/**
 * Browser ↔ server HTTP contract (B7 provides, F2/F4 consume). Base path /api/v1.
 * Errors: non-2xx with body { error: ApiError } (the real server also repeats `code` and `message` at
 * the top level, the shape cv-mock used). Optional header `X-CV-Actor: <Actor JSON>` attributes a
 * request (e.g. cv-mcp over HTTP); without it the actor is { kind: "human", name: "you" }.
 * Auth: bearer token printed by `cv serve`, as `Authorization: Bearer <token>` or `?token=` (the
 * WebSocket). No cookie: one for 127.0.0.1 would reach every other local port. The UI keeps the
 * token in its origin's localStorage. Server binds 127.0.0.1 only and refuses requests whose Host
 * is not loopback (403 forbidden-host) or whose Origin is another site or port (403 forbidden-origin).
 *
 * Caching: /blobs/* responses are immutable (Cache-Control: public, max-age=31536000, immutable).
 * View-scoped responses carry ETag = layoutVersion; a newer op yields view.invalidated over WS.
 *   Bootstrap, tiles and windows:batch send `ETag: "<layoutVersion>-<16 hex content hash>"` and
 *   `Cache-Control: no-cache`; `If-None-Match` with that tag answers 304. The hash covers what an op
 *   or a live overlay changes without a layout bump (blobs, spans, flags).
 *   GET /views/:id sends the same ETag (its body is the ViewRef).
 * Errors may carry `error.details.hint` (e.g. 400 "range-not-chain").
 * Views after a restart: view ids live in the server's memory. An id it has not loaded (from before
 *   a restart, or a dropped range view) answers 404 `{ error: { code: "view-unknown" } }` on every
 *   view-scoped route except bootstrap, tiles and GET /views/:id, which index.db still serves for a
 *   saved snapshot view (GET /views/:id then says status "stale"). The client re-POSTs the view's
 *   spec and reloads in place; a plain "not-found" (an unknown path or window) is not that.
 */
import type {
  Actor,
  BlobId,
  ClusterId,
  ChangeId,
  CommitId,
  EventCursor,
  Fileset,
  HubId,
  LaneId,
  LinkId,
  OpId,
  RepoPath,
  Revset,
  ThreadId,
  TourId,
  ViewId,
  WindowId,
  WindowKey,
  WorkspaceName,
} from "./ids";
import type {
  BookmarkInfo,
  ChurnSummary,
  Cluster,
  CoChangePair,
  CoChangeClusterPair,
  ConflictDetail,
  ConflictRecord,
  EdgeBundle,
  EditRequest,
  EditResult,
  FileDiff,
  GraphOpRequest,
  GraphOpResult,
  OpLog,
  OpDetail,
  RestoreRequest,
  FetchRequest,
  PushRequest,
  RemoteResult,
  GraphRow,
  GridColumn,
  Hub,
  Lang,
  LaneBand,
  LaneDef,
  Link,
  LinkKind,
  MetricPoint,
  RangeOptions,
  RangeWarning,
  Rect,
  RevisionReview,
  SiteRef,
  Thread,
  ThreadKind,
  ThreadStatus,
  ViewRef,
  ViewSpec,
  WindowDiff,
  WindowHeader,
  WindowOutline,
  WorkspaceInfo,
  WorkspaceSummary,
} from "./model";
import type { LineTokens } from "./wire";
import type { ServerEvent, ViewState } from "./ws";

/** Typed route table: "METHOD /path" → request/response shapes. */
export interface Routes {
  // ── Repo, workspaces, lanes and views ──
  /**
   * Additive: what the server serves. `vcs: "git"`: `cv serve` was pointed at a plain git repository
   * (no jj yet). Then only /setup, /setup:init-jj, /revisions (git history, read-only), /refs,
   * /workspaces (always []) and /git/commits/:commit answer; every other route is 409 "jj-required".
   */
  "GET /api/v1/setup": { res: SetupInfo };
  /**
   * Git mode only: run SetupInfo.initCommand (`jj git init --colocate`) at the repository root, then
   * restart into the full server (`restarting`; poll /setup until vcs is "jj"). Humans only (403 for
   * an agent); 409 "jj-missing" when jj is not installed, 409 "already-jj" in jj mode; 400 without
   * `confirm: true`.
   */
  "POST /api/v1/setup:init-jj": { body: { confirm: true }; res: { ok: true; restarting: boolean; message: string } };
  /** Git mode only: the files a commit changes against its first parent (no renames). */
  "GET /api/v1/git/commits/:commit": { res: GitCommitDetail };
  "GET /api/v1/repo": { res: RepoInfo };
  "GET /api/v1/workspaces": { res: WorkspaceSummary[] }; // first-class workspaces; updates via ws workspace.updated
  /**
   * Additive (D12 "Compare", DESIGN §7): a per-commit range view of both stacks
   * (`(trunk()..a@) | (trunk()..b@)`, options.allowNonChain), filtered to the files both change
   * (fileset of quoted paths). 404 "not-found" for an unknown workspace, 400 "no-overlap" when the
   * stacks share no file. Open it like any range ViewRef.
   */
  "POST /api/v1/workspaces:compare": { body: { a: WorkspaceName; b: WorkspaceName }; res: ViewRef };
  "GET /api/v1/lanes": { res: LaneDef[] }; // defaults + the `@` revision's .cv/lanes.toml, hidden lanes included (DESIGN §4.3)
  /**
   * Idempotent: same spec at same op → same id. Snapshot specs: `fileset`, `options.workspace` and
   * `options.liveWorkingCopy` are honoured (model.ts SnapshotOptions) and each makes a view of its
   * own; an unknown workspace or a non-boolean liveWorkingCopy is 400 "invalid", a bad fileset 400
   * "fileset-invalid". Range specs: options outside RangeOptions (wrong type, unknown field, value
   * out of range) are 400 "invalid", never the defaults.
   */
  "POST /api/v1/views": { body: ViewSpec; res: ViewRef };
  "GET /api/v1/views/:viewId": { res: ViewRef };
  "GET /api/v1/views/:viewId/bootstrap": { res: ViewBootstrap }; // T0
  "GET /api/v1/views/:viewId/tiles/:z/:x/:y": { res: TileResponse }; // T1
  "POST /api/v1/views/:viewId/windows:batch": { body: WindowBatchRequest; res: WindowBatchResponse }; // T2
  /** Snapshot views: a layout change (the window keeps its top, what it would cover moves down; res carries the new layoutVersion). Range views: an overlay. */
  "POST /api/v1/views/:viewId/windows/:windowId/expand": { body: ExpandRequest; res: WindowHeader & { layoutVersion?: number } };
  /** Back to WindowHeader.expandedFrom: in a snapshot view the reverse of the expand (a layout change, with layoutVersion); 409 not-expanded otherwise. */
  "POST /api/v1/views/:viewId/windows/:windowId/collapse": { res: WindowHeader & { layoutVersion?: number } };
  "PATCH /api/v1/views/:viewId/layout": { body: LayoutPatch; res: { layoutVersion: number } };
  /** Range views (additive): every point carries heat.inRange; rank "inRange" orders by it (then risk) before `limit`. */
  "GET /api/v1/views/:viewId/metrics": {
    /** basis "imports" (snapshot, level file): connectedness on the file import graph alone, appendix C's reference definition; no co-change. */
    query: { level?: "cluster" | "file"; lane?: LaneId; limit?: number; rank?: "risk" | "inRange"; basis?: "links" | "imports" };
    res: { points: MetricPoint[]; cochange: CoChangePair[]; cochangeClusters?: CoChangeClusterPair[] }; // Hotspots panel and co-change lens
  };
  /** File chip clicked: every fragment of the file, including off-screen ones, for highlight and minimap. */
  "GET /api/v1/views/:viewId/fragments": { query: { path: RepoPath }; res: FragmentsResponse };

  // ── Content (immutable, content-addressed) ──
  "GET /api/v1/blobs/:blobId/text": { query: { from?: number; to?: number }; res: BlobText }; // T3

  // ── History and ranges ──
  "GET /api/v1/revisions": { query: { revset?: Revset; limit?: number }; res: { rows: GraphRow[]; truncated: boolean } };
  "POST /api/v1/revsets:validate": { body: { revset: Revset; fileset?: Fileset }; res: RevsetValidation };
  "GET /api/v1/ranges": { res: SavedRange[] }; // creation order
  /** :name wins over body.name; createdBy is optional in the body: the server sets it from the actor on creation and keeps it on updates. */
  "PUT /api/v1/ranges/:name": { body: SavedRange; res: SavedRange };
  "DELETE /api/v1/ranges/:name": { res: { ok: true } }; // 404 when no range has that name
  /** Hunk old/new are [start, count]; every DiffLine carries text here (no blob to read from). */
  /** The Graph view's bookmark pickers. `trunk`: the local bookmark "land" targets by default (trunk()'s name, else main, master or trunk). */
  "GET /api/v1/refs": { res: { bookmarks: BookmarkInfo[]; trunk?: string; colocated: boolean; remotes: string[] } };
  /**
   * Graph view operations (model.ts GraphOp). Same rules as edits: agents need the edit permission
   * (403 "agent-edit-disabled"), 409 "locked" when the mutation queue stays busy. A refused
   * operation (immutable commit, backwards bookmark move, not fast-forwardable, …) answers 200 with
   * status "rejected" and error; nothing is written.
   */
  "POST /api/v1/graph:op": { body: GraphOpRequest; res: GraphOpResult };
  /** jj op revert <op> (a GraphOpResult's opAfter); "rejected" with error "undo-blocked" when that would leave changes divergent or conflicted. */
  "POST /api/v1/graph:undo": { body: { op: OpId }; res: GraphOpResult };
  /** The operation log (jj op log), newest first; limit default 100, at most 1000. */
  "GET /api/v1/ops": { query: { limit?: number }; res: OpLog };
  "GET /api/v1/ops/:op": { res: OpDetail };
  /** jj op restore: the repo as it was after that operation. Humans only (403 "human-only"); dryRun previews. */
  "POST /api/v1/ops:restore": { body: RestoreRequest; res: GraphOpResult };
  /**
   * jj git fetch / jj git push -b … through the user's git setup, never interactive (a remote that
   * asks for a password fails). Humans only (403 "human-only"). Refusals and jj errors answer 200
   * with status "failed" and error.
   */
  "POST /api/v1/git:fetch": { body: FetchRequest; res: RemoteResult };
  "POST /api/v1/git:push": { body: PushRequest; res: RemoteResult };
  "GET /api/v1/interdiff": { query: { change: ChangeId; from: CommitId }; res: { files: FileDiff[]; description?: [string, string] } };
  /**
   * Additive: the files that differ between two revisions' trees, as `jj diff --from <from> --to
   * <to>` (each revset resolves to one commit; fileset: jj fileset syntax). MCP get_diff {from, to}.
   */
  "GET /api/v1/diff": { query: { from: Revset; to: Revset; fileset?: Fileset }; res: { files: FileDiff[]; from: CommitId; to: CommitId } };

  // ── Navigation ──
  "GET /api/v1/search": { query: { q: string; kinds?: string; viewId?: ViewId; limit?: number }; res: SearchHit[] };
  "GET /api/v1/links": {
    query: { viewId: ViewId; target: string /* WindowId | path:line | qualname */; dir?: "in" | "out" | "both"; kinds?: string; depth?: 1 | 2 | 3 };
    res: { links: Link[]; hubs: Hub[] };
  };
  "GET /api/v1/hubs/:hubId": { query: { viewId: ViewId; protocol?: string }; res: Hub }; // :hubId may also be a hub key (additive)

  // ── Review ──
  "GET /api/v1/threads": { query: { status?: string; assignee?: "agent" | "human"; revset?: Revset; path?: RepoPath }; res: Thread[] };
  "POST /api/v1/threads": { body: NewThread; res: Thread };
  "POST /api/v1/threads/:threadId/comments": { body: { body: string; setStatus?: ThreadStatus }; res: Thread };
  "GET /api/v1/threads/:threadId": { res: Thread }; // additive (B8)
  "PATCH /api/v1/threads/:threadId": { body: { status?: ThreadStatus; assignee?: "agent" | "human" | null }; res: Thread };
  "POST /api/v1/reviews:submit": { body: { threads: ThreadId[]; note?: string }; res: { cursor: EventCursor } }; // "Send to agent"
  "PUT /api/v1/reviews/marks/:change": { body: { commit: CommitId }; res: RevisionReview };

  // ── Edits and conflicts ──
  "POST /api/v1/edits": { body: EditRequest; res: EditResult }; // dryRun → "previewed"
  "GET /api/v1/edits/:editId": { res: EditResult }; // additive (B8)
  "POST /api/v1/edits/:editId/apply": { res: EditResult };
  /** jj op revert <opAfter>. 409 { code: "undo-blocked", details: UndoBlocked } when that would leave
   *  changes divergent or conflicted (e.g. a later jj command rewrote the edited change); nothing is
   *  written. Agents need the same permission as for edits (403 "agent-edit-disabled"). Apply answers
   *  409 "locked" when the mutation queue stays busy past the server's timeout (30 s), and returns
   *  status "rejected" / "stale-base" when the repo moved on and the edit would no longer do what
   *  was previewed. */
  "POST /api/v1/edits/:editId/undo": { res: EditResult };
  "GET /api/v1/conflicts": { query: { revset?: Revset }; res: ConflictRecord[] };
  "GET /api/v1/conflicts/:commit/*path": { res: ConflictDetail };
  "POST /api/v1/conflicts/:commit/*path/resolve": { body: { content: string }; res: EditResult };

  // ── Agent routes (B9, additive). Writes from MCP carry the agent in headers, and the server
  //    attributes them as Actor { kind: "agent", name, client }:
  //      X-CV-Agent: claude · X-CV-Agent-Client: claude-code · X-CV-Workspace: gym
  //    (threads, comments, PATCH, edits, resolve, links). Without X-CV-Agent the actor is the human.
  //    X-CV-Workspace fills Actor.workspace (stored on threads, comments, edits; op descriptions
  //    read "(claude@gym)"). Tours are stored in review.db. ──
  /** Append { type: "agent.attention", ...body } to the event log (server may overwrite `at`). */
  "POST /api/v1/agent/attention": {
    body: { agent: string; workspace?: WorkspaceName; tool: string; windows: WindowId[]; note?: string; at?: string };
    res: { ok: true; cursor: EventCursor };
  };
  /** Bind an agent to a workspace: sets WorkspaceSummary.agent and emits workspace.updated. 404 "not-found" for an unknown workspace. */
  "POST /api/v1/agent/bind": { body: { agent: string; client?: string; workspace: WorkspaceName }; res: WorkspaceSummary };
  /** Latest view.state received from a UI over WS (MCP get_view_state). */
  "GET /api/v1/agent/view-state": { res: { view: ViewRef; state: ViewState } | { view: null } };
  /** Agent navigation, only when the human allowed it; 403 { code: "not-allowed" } otherwise. Emits view.requested. */
  "POST /api/v1/agent/view": { body: { spec: ViewSpec }; res: ViewRef };
  /** Emits focus.request; "moved" when a connected UI follows the agent (its latest ws view.state has followAgent: true), else "suggested". */
  "POST /api/v1/agent/focus": { body: { target: SiteRef; lines?: [number, number]; note?: string }; res: { delivered: "moved" | "suggested" } };
  "POST /api/v1/agent/tours": { body: { title: string; steps: TourStep[] }; res: { tour: string } };
  /** Tours agents created, newest first (the Agent panel plays them). */
  "GET /api/v1/tours": { res: Tour[] };
  /** Per-session permissions (DESIGN §10): may the agent edit, may it move the human's camera.
   *  Keyed by UI session and agent, stored in review.db. The UI session is the `X-CV-Session` header
   *  (or `?session=`): a stable id the UI keeps per browser profile, "default" when absent. `agent`
   *  (query or body) narrows to one agent; without it the toggles cover every agent ("*"), and an
   *  agent-specific setting wins within a session. In force for an agent: it may edit only while no
   *  session has editing off for it; it may move the camera when any session allows it. GET with
   *  X-CV-Agent and no session returns what is in force for that agent (`effective: true`). PUT from
   *  an agent is 403 "not-allowed". */
  "GET /api/v1/agent/permissions": { query: { session?: string; agent?: string }; res: AgentPermissions };
  "PUT /api/v1/agent/permissions": {
    query: { session?: string; agent?: string };
    body: Partial<AgentPermissions> & { session?: string; agent?: string };
    res: AgentPermissions;
  };
  /** from/to: WindowId | path:line | qualname | HubId (to only). viewId (additive): whose window ids the Link carries. */
  "POST /api/v1/links": { body: { from: string; to: string; kind: LinkKind; label: string; note?: string; viewId?: ViewId }; res: Link };
  /** Link ids are per view: viewId (additive) names the view the id comes from (default: the human's view). */
  "POST /api/v1/links/:linkId/rate": { body: { verdict: "confirmed" | "rejected"; note?: string; viewId?: ViewId }; res: Link };
  "PUT /api/v1/clusters/:clusterId/label": { body: { label: string; summary?: string }; res: { ok: true } };

  // ── Events (long-poll twin of the WebSocket; MCP wait_for_events uses it) ──
  /** resync (additive): `cursor` is older than the event log keeps; events were missed, refetch state. */
  "GET /api/v1/events": { query: { cursor?: EventCursor; kinds?: string; timeoutS?: number }; res: { cursor: EventCursor; events: ServerEvent[]; resync?: boolean } };
}

export interface TourStep {
  target: SiteRef;
  lines?: [number, number];
  note: string;
}

export interface Tour {
  id: TourId;
  title: string;
  by: Actor;
  createdAt: string;
  steps: TourStep[];
}

/** Agent writes are rejected with 403 "agent-edit-disabled" while `edit` is false. */
export interface AgentPermissions {
  edit: boolean;
  moveCamera: boolean;
  session?: string; // additive: the UI session these toggles belong to
  agent?: string; // additive: the agent they apply to, "*" = every agent
  effective?: boolean; // additive: true when this is what is in force for an agent across sessions
}

export interface SetupInfo {
  vcs: "jj" | "git";
  root: string; // the repository root
  jj: { installed: boolean; version?: string }; // the jj CLI on the server's PATH
  initCommand: string; // what "Initialise jj" runs in root: "jj git init --colocate"
}

export interface GitCommitDetail {
  commit: CommitId;
  files: Array<{ path: RepoPath; added?: number; removed?: number }>; // counts absent for binary files
}

export interface RepoInfo {
  name: string;
  root: string;
  jjVersion: string;
  op: OpId;
  trunk?: CommitId;
  workspaces: WorkspaceInfo[];
  index: { state: "idle" | "running"; lastOp?: OpId; files: number; blobsParsed: number; churn?: ChurnSummary };
  capabilities: { scip: boolean; runtimeLinks: boolean; jjLib: boolean };
}

/** T0: small, fetched once per view; enough to draw L0 and the minimap. */
export interface ViewBootstrap {
  view: ViewRef;
  world: Rect;
  laneDefs: LaneDef[];
  lanes: LaneBand[]; // district grid: one band per visible lane, rows per lane
  columns: GridColumn[]; // heat columns shared by every lane
  clusters: Cluster[];
  hubs: Hub[];
  bundles: EdgeBundle[];
  churn?: ChurnSummary;
  heatLabels: [string, string, string, string, string]; // "Frozen", "Cool", "Mild", "Warm", "Hot"
}

export interface FragmentsResponse {
  path: RepoPath;
  chip: number; // FileFragment.chip slot
  fragments: Array<{ window: WindowId; index: number; rect: Rect; cluster: ClusterId }>;
}

/** T1: window frames intersecting one tile (see wire.ts TILE_BASE_WU). */
export interface TileResponse {
  layoutVersion: number;
  windows: WindowHeader[];
  /** Links with either port inside the tile, so L1/L2 draw edges before T2 arrives. De-duplicate by id. */
  edges: TileEdge[];
}

/** Port-to-port geometry of one link, in world units: a = out port (callsite), b = in port (definition). */
export interface TileEdge {
  id: LinkId;
  kind: LinkKind;
  from: WindowId;
  to: WindowId;
  /** Clusters of the two ends: cross-cluster links draw as bundles until a window or cluster is selected. */
  fromCluster: ClusterId;
  toCluster: ClusterId;
  a: [number, number];
  b: [number, number];
  confidence: number;
}

export interface WindowBatchRequest {
  ids: WindowId[]; // ≤ 200 per request
  parts: Array<"outline" | "diff">;
  layoutVersion: number;
}

export interface WindowBatchResponse {
  outlines: WindowOutline[];
  diffs: WindowDiff[];
  links: Link[]; // every link referenced by the returned ports, de-duplicated
  missing: WindowId[]; // ids not in this layoutVersion; client refetches tiles
}

export interface ExpandRequest {
  before?: number | "symbol" | "file";
  after?: number | "symbol" | "file";
}

/**
 * Drags and pins, keyed by layout key so they survive new revisions.
 * - `key`: a WindowHeader.key (moves that window's rect origin to x, y), or `cluster:<Cluster.id>` of
 *   this view (moves the cluster rect origin to x, y and every unpinned window of it by the same delta;
 *   pinned windows never move). The server also accepts `cluster:<label>`.
 * - `pinned: true` persists the position (review.db) and sets WindowHeader.flags.pinned / Cluster.pinned;
 *   `pinned: false` is a plain move and unpins. To unpin in place, send the current position with
 *   `pinned: false`.
 * - The patch applies whole or not at all: 409 { code: "layout-stale" } when baseLayoutVersion is not
 *   the view's layoutVersion; 400 { code: "unknown-key" } for a key not in the view.
 * - On success layoutVersion bumps by one, cluster rects (and the world) grow to contain their windows,
 *   hub anchors follow, and view.invalidated { reason: "layout", windows: <moved window ids> } is emitted.
 *   A moved window can land in another tile: the client that sent the patch reloads its tiles.
 */
export interface LayoutPatch {
  moves: Array<{ key: WindowKey | `cluster:${string}`; x: number; y: number; pinned: boolean }>;
  baseLayoutVersion: number;
}

export interface BlobText {
  blob: BlobId;
  lang: Lang;
  from: number;
  to: number;
  lines: string[];
  tokens: LineTokens[];
}

export type RevsetValidation =
  | {
      ok: true;
      count: number;
      changes: ChangeId[]; // first 50, newest first
      files: number;
      added: number;
      removed: number;
      conflicts: number;
      warnings: RangeWarning[];
    }
  | { ok: false; error: string; hint?: string }; // e.g. hint: "did you mean trunk()?"

export interface SavedRange {
  name: string;
  revset: Revset;
  fileset?: Fileset;
  options?: RangeOptions;
  createdBy: Actor;
}

export interface SearchHit {
  kind: "symbol" | "file" | "hub" | "thread" | "revision";
  label: string;
  detail?: string;
  target: { window?: WindowId; site?: SiteRef; hub?: HubId; thread?: ThreadId; change?: ChangeId };
  score: number;
}

export interface NewThread {
  anchor: { path: RepoPath; rev: ChangeId | CommitId; start: number; end: number; side?: "new" | "old"; cols?: [number, number] };
  body: string;
  kind?: ThreadKind;
  severity?: "info" | "minor" | "major" | "blocker";
  assignee?: "agent" | "human";
}
