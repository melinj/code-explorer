/**
 * Agent contract: MCP tools served by B9 at http://127.0.0.1:<port>/mcp (Streamable HTTP), plus a
 * `cv mcp` stdio shim for clients without HTTP transport. Tools call the same services as the UI,
 * so agent writes go through the same mutation queue, immutability checks and undo log.
 *
 * Design rules for tool outputs:
 * - Stable ids everywhere (WindowId, ThreadId, ChangeId, HubId) so the human and the agent can
 *   refer to the same thing.
 * - Code comes back as AnnotatedCode text: numbered lines with ports and threads inline, so the
 *   agent can follow links without grepping.
 * - Every tool call records agent attention (WS agent.attention: the tool and the windows it read,
 *   wrote or listed, else none) so the human sees what the agent is looking at and doing.
 *
 * Transports (B9): `cv serve` mounts Streamable HTTP at /mcp (configured profile, editor by default)
 * and /mcp/reader, /mcp/reviewer, /mcp/editor, /mcp/full; `cv mcp --server URL [--profile P]
 * [--agent NAME] [--workspace WS]` (or the standalone `cv-mcp stdio …`) is the stdio shim. The agent
 * name defaults from the MCP clientInfo ("claude-code" → "claude"), or the `X-CV-Agent` header over
 * HTTP; bindings are keyed by agent name and shared by every session of one server.
 *
 * Workspace: no bind is needed. An agent works in its bound workspace, else the `X-CV-Workspace`
 * header (HTTP), else `--workspace`, else the jj workspace holding the shim's cwd (stdio), else the
 * served workspace (`serving: true`), else the only one. bind_workspace (reader/reviewer/full)
 * switches.
 *
 * Outputs are slimmed for a model's context (the HTTP API the UI uses is unchanged): compact JSON,
 * change ids shortened to 12 characters, floats rounded to 3 significant digits, code as numbered
 * text blocks. Rejected or failed edits are tool errors (isError) with a one-line message naming the
 * EditErrorCode and what to do ("stale-base: re-read …", "immutable: … mode \"branch\"").
 * wait_for_events defaults `kinds` to every type except agent.attention and index.progress (hello
 * is not an event); unknown kinds are an error.
 */
import type {
  Actor,
  BlobId,
  ChangeId,
  ClusterId,
  CommitId,
  EditId,
  EventCursor,
  Fileset,
  LaneId,
  RepoPath,
  Revset,
  ThreadId,
  TourId,
  WindowId,
  WorkspaceName,
} from "./ids";
import type {
  ConflictDetail,
  ConflictRecord,
  Connectedness,
  EditMode,
  EditResult,
  FileDiff,
  Heat,
  Hub,
  Link,
  LinkKind,
  LinePatch,
  MetricPoint,
  Protocol,
  Revision,
  RevisionReview,
  ThreadKind,
  ThreadStatus,
  ThreadSummary,
  ViewRef,
  ViewSpec,
  WorkspaceSummary,
} from "./model";
import type { SearchHit } from "./http";
import type { ServerEvent, ViewState } from "./ws";

/**
 * Numbered source with inline annotations, e.g.
 *   41│     envelope = EventEnvelope(topic=ENTITY_MOVED, payload=moved)
 *   42│     bus.emit(envelope)                  ⇢ hub bus:world.entity.moved (2 consumers)
 *     ┆ [t_12 open · assigned agent] "Cap retries at 5" (suggestion)
 * `target` strings accepted by tools: a WindowId, "path:line", a qualname, a HubId or a ThreadId.
 */
export interface AnnotatedCode {
  path: RepoPath;
  change: ChangeId;
  commit: CommitId;
  blob: BlobId; // pass back as baseBlob when editing
  start: number;
  end: number;
  fileLines: number;
  windows: WindowId[];
  text: string; // its own text block after the JSON metadata
}

/**
 * A thread as MCP shows it (get_thread, list_threads { withCode }): no fingerprint, author objects,
 * comment ids or timestamps; the code is the next text block (numbered lines of the enclosing symbol,
 * at most 80, or ±contextLines), without this thread's own inline line.
 */
export interface McpThread {
  id: ThreadId;
  status: ThreadStatus;
  kind: ThreadKind;
  assignee?: "agent" | "human";
  severity?: "info" | "minor" | "major" | "blocker";
  path: RepoPath;
  start: number;
  end: number;
  change: string; // ChangeId, 12-character prefix
  outdated?: "lines-changed" | "file-removed" | "change-abandoned";
  comments: Array<{ by: string; body: string }>; // by: the author's name
  blob?: BlobId; // of the code shown; edit_code { thread } uses it as baseBlob
  lines?: [number, number]; // the code shown
  suggestion?: LinePatch; // the newest ```suggestion as a patch of the anchored lines
  code?: string; // "unavailable: …" when the code could not be shown
}

/** What a thread write returns to the agent. */
export interface ThreadAck {
  id: ThreadId;
  status: ThreadStatus;
  comments: number;
}

/**
 * An EditResult as MCP returns it: no actor, opBefore/opAfter or `predicted`; change ids shortened
 * to 12 characters; workspaces only when one was recovered; `thread` when the call also answered a
 * thread (edit_code / apply_suggestion with reply or setStatus). Rejected and failed edits are tool
 * errors instead.
 */
export interface McpEditResult {
  id: EditId;
  status: "previewed" | "applied" | "undone";
  created?: string;
  rewritten?: string[];
  files?: Array<{ path: RepoPath; newBlob?: BlobId }>; // the baseBlob for a follow-up edit
  conflicts?: Array<{ change: string; path: RepoPath; sides: number }>;
  workspaces?: EditResult["workspaces"];
  thread?: ThreadAck;
  threadError?: string; // the edit applied, the reply did not
}

/** A Link as MCP returns it: both ends as "path:line" (or "hub:<HubId>"). */
export interface McpLink {
  id: string;
  kind: LinkKind;
  label?: string;
  from: string;
  to: string;
  confidence?: number; // only when < 1
  feedback?: Link["feedback"];
}

/**
 * get_diff / get_interdiff: two text blocks, a JSON summary and a unified diff ("--- <status> <path>
 * (+a -r)", "@@ -o,n +o,n @@", lines prefixed "+", "-" or " "). At most 600 diff lines are printed;
 * later files are only counted (narrow with fileset). Conflicted files have no hunks (get_conflict).
 */
export type McpDiff = [Array<{ path: RepoPath; status: FileDiff["status"]; added: number; removed: number; conflicted?: true }>, string];

export interface McpTools {
  // ── Orientation and shared attention ──
  get_view_state: { in: Record<string, never>; out: { view: ViewRef; state: ViewState } | { view: null } };
  set_view: { in: { spec: ViewSpec }; out: ViewRef }; // only when the human allows agent navigation
  focus: { in: { target: string; lines?: [number, number]; note?: string }; out: { delivered: "moved" | "suggested" } };
  create_tour: { in: { title: string; steps: Array<{ target: string; lines?: [number, number]; note: string }> }; out: { tour: TourId } };

  // ── Reading and navigation ──
  /** limit default 10. Hits: { kind, label, detail ("path:line" for code), window, …other target fields }. */
  search: { in: { query: string; kinds?: Array<SearchHit["kind"]>; limit?: number }; out: Array<{ kind: SearchHit["kind"]; label: string; detail?: string; window?: WindowId }> };
  read_window: { in: { window: WindowId; annotate?: Array<"ports" | "threads" | "diff"> }; out: AnnotatedCode & { window: WindowId; symbol?: unknown } };
  read_code: { in: { path: RepoPath; rev?: Revset; start?: number; end?: number; annotate?: Array<"ports" | "threads" | "diff"> }; out: AnnotatedCode };
  /**
   * target "path:line" or a qualname ("stored_attempts", "Type.method", "Type::method"; resolved
   * through the symbol index): only links starting (out) or ending (in) in the symbol at that line
   * (else the line itself), reported as `span`. A WindowId or path: the whole window or file.
   * limit default 30; `more` counts the rest.
   */
  get_links: {
    in: { target: string; dir?: "in" | "out" | "both"; kinds?: LinkKind[]; depth?: 1 | 2 | 3; limit?: number };
    out: { links: McpLink[]; hubs?: Array<Pick<Hub, "id" | "protocol" | "key">>; span?: string; more?: number };
  };
  get_hub: { in: { key: string; protocol?: Protocol }; out: Omit<Hub, "rect" | "anchors"> };
  /**
   * Both axes for one target: heat (change frequency) and connectedness, plus co-change partners
   * (≤ 10). No timeline: no route serves per-window months yet.
   */
  get_metrics: {
    in: { target: string };
    out: { window: WindowId; path: RepoPath; heat: Heat; conn: Connectedness; risk: number; reviewFirst: boolean; timeline?: Array<{ month: string; touches: number }>; coChanged: Array<{ path: RepoPath; jaccard: number }> };
  };
  /** Ranked "review first" list: highest geometric mean of heat and connectedness percentiles. limit default 10. */
  get_hotspots: {
    in: { lane?: LaneId; limit?: number };
    out: Array<Pick<MetricPoint, "label" | "window" | "lane" | "risk" | "heatPct" | "connPct"> & { touches: number; fanIn: number }>;
  };

  // ── Workspaces (first-class) ──
  /** Without root and lastOp.op. */
  list_workspaces: { in: Record<string, never>; out: Array<Omit<WorkspaceSummary, "root"> & { boundToThisSession?: true }> };
  /** Binds this MCP session to another workspace: attention, edits and default revsets are attributed to it. */
  bind_workspace: { in: { workspace: WorkspaceName }; out: { bound: WorkspaceName; stack: Revset } };

  // ── Review ──
  /**
   * withCode: one McpThread metadata block and one code block per thread, enough to answer each with
   * a single edit_code { thread, patches, reply }. Without it: ThreadSummary[].
   */
  list_threads: { in: { status?: ThreadStatus[]; assignee?: "agent" | "human"; revset?: Revset; path?: RepoPath; withCode?: boolean }; out: ThreadSummary[] | Array<McpThread | string> };
  /** contextLines: ±lines around the anchor instead of the enclosing symbol. */
  get_thread: { in: { thread: ThreadId; contextLines?: number }; out: [McpThread, string] };
  create_thread: {
    in: {
      anchor: { path: RepoPath; rev?: Revset; start: number; end: number; side?: "new" | "old" };
      body: string;
      kind?: ThreadKind; // agents default to "finding"
      severity?: "info" | "minor" | "major" | "blocker";
    };
    out: ThreadAck & { kind: ThreadKind; createdBy: Actor["kind"] };
  };
  /** body and/or setStatus (set_thread_status folded in). */
  reply: { in: { thread: ThreadId; body?: string; setStatus?: ThreadStatus }; out: ThreadAck };
  set_thread_status: { in: { thread: ThreadId; status: ThreadStatus }; out: ThreadAck };
  /** reply / setStatus answer the thread once the edit applied (setStatus defaults to "addressed" with a reply). */
  apply_suggestion: { in: { thread: ThreadId; mode?: "amend" | "insert-after"; dryRun?: boolean; reply?: string; setStatus?: ThreadStatus }; out: McpEditResult };

  // ── History, edits, conflicts ──
  list_revisions: { in: { revset?: Revset; limit?: number }; out: Array<Revision & { review?: RevisionReview }> };
  /** Default revset: the workspace's stack. fileset: a path or directory prefix. */
  /**
   * revset: the cumulative diff of that range (default: the workspace's stack). from/to instead:
   * two revisions' trees, as `jj diff --from <from> --to <to>` (each resolves to one commit; a
   * missing side is the workspace's @; not with revset). fileset: jj fileset syntax.
   */
  get_diff: { in: { revset?: Revset; from?: Revset; to?: Revset; fileset?: Fileset }; out: McpDiff };
  get_interdiff: { in: { change: ChangeId; from: CommitId }; out: McpDiff }; // what changed since the human reviewed
  /**
   * With thread: rev, path and baseBlob default to the thread's anchor change, its path and the blob
   * of the code last shown to this agent for that path; reply / setStatus answer it in the same call
   * once the edit applied. A stale baseBlob is rebased by a three-way merge unless the edit overlaps
   * what changed (then "stale-base"). dryRun previews (the id works with apply_edit, which then posts
   * the reply / setStatus given here). Without thread, rev defaults to the workspace's @; a file that
   * does not exist at rev (a new file) needs no baseBlob, an existing one does.
   */
  edit_code: {
    in: {
      thread?: ThreadId;
      rev?: Revset;
      path?: RepoPath;
      baseBlob?: BlobId; // from AnnotatedCode.blob / McpThread.blob / McpEditResult.files
      patches?: LinePatch[];
      content?: string;
      mode?: EditMode; // default "amend" when the target is mutable, "branch" when it is immutable
      message?: string;
      dryRun?: boolean;
      reply?: string;
      setStatus?: ThreadStatus;
    };
    out: McpEditResult;
  };
  /** edit_code with dryRun forced (full profile). */
  propose_edit: { in: McpTools["edit_code"]["in"]; out: McpEditResult };
  /** Apply an edit previewed with dryRun (POST /edits/:id/apply). */
  apply_edit: { in: { edit: EditId }; out: McpEditResult };
  list_conflicts: { in: { revset?: Revset }; out: ConflictRecord[] }; // change, commit, firstSeenOp: 12-character prefixes (get_conflict takes them)
  get_conflict: { in: { rev: Revset; path: RepoPath }; out: ConflictDetail };
  resolve_conflict: { in: { rev: Revset; path: RepoPath; content: string }; out: McpEditResult };
  undo_edit: { in: { edit: EditId }; out: McpEditResult };

  // ── Graph enrichment ──
  declare_link: {
    in: { from: string; to: string; kind: LinkKind; label: string; note?: string }; // from/to: "path:line" or HubId
    out: McpLink;
  };
  rate_link: { in: { link: string; verdict: "confirmed" | "rejected"; note?: string }; out: McpLink };
  label_cluster: { in: { cluster: ClusterId; label: string; summary?: string }; out: { ok: true } };

  // ── Events ──
  /**
   * Long-poll (≤ 60 s). Returns at once if events newer than cursor exist. The newest `limit`
   * (default 20) events; `skipped` counts older ones (the cursor moves past them). thread.upserted
   * carries a ThreadSummary, edit.updated an McpEditResult.
   */
  wait_for_events: {
    in: { cursor?: EventCursor; kinds?: Array<ServerEvent["type"]>; timeoutS?: number; limit?: number };
    out: { cursor: EventCursor; events: ServerEvent[]; skipped?: number; resync?: true }; // resync: cursor older than the log keeps
  };
}

export type McpToolName = keyof McpTools;

/**
 * Tool profiles keep the agent's tool list short: `cv mcp --profile reviewer`, /mcp/<profile>.
 * editor addresses review threads end to end (find with code, edit + answer in one call, conflicts,
 * findings); graph, metrics, tours, navigation and workspace binding are in reviewer and full.
 */
export const MCP_PROFILES = {
  reader: ["get_view_state", "focus", "search", "read_window", "read_code", "get_links", "get_hub", "get_metrics", "get_hotspots", "list_workspaces", "bind_workspace", "list_revisions", "get_diff", "get_interdiff", "wait_for_events"],
  reviewer: ["get_view_state", "set_view", "focus", "create_tour", "search", "read_window", "read_code", "get_links", "get_hub", "get_metrics", "get_hotspots", "list_workspaces", "bind_workspace", "list_threads", "get_thread", "create_thread", "reply", "list_revisions", "get_diff", "get_interdiff", "declare_link", "rate_link", "label_cluster", "wait_for_events"],
  editor: ["list_threads", "get_thread", "reply", "create_thread", "edit_code", "apply_suggestion", "read_code", "search", "get_links", "get_diff", "list_conflicts", "get_conflict", "resolve_conflict", "wait_for_events", "set_view"],
  full: ["get_view_state", "set_view", "focus", "create_tour", "search", "read_window", "read_code", "get_links", "get_hub", "get_metrics", "get_hotspots", "list_workspaces", "bind_workspace", "list_threads", "get_thread", "create_thread", "reply", "set_thread_status", "apply_suggestion", "list_revisions", "get_diff", "get_interdiff", "edit_code", "propose_edit", "apply_edit", "list_conflicts", "get_conflict", "resolve_conflict", "undo_edit", "declare_link", "rate_link", "label_cluster", "wait_for_events"],
} as const satisfies Record<string, readonly McpToolName[]>;

/**
 * MCP resources, subscribable (resources/subscribe, or subscriptions/listen from 2026-07-28):
 * notifications/resources/updated follows thread.upserted / review.submitted (openThreads),
 * conflicts.changed / op.changed (conflicts), and view.requested / view.invalidated or a changed
 * ViewState (view, compared about once a second while subscribed).
 */
export const MCP_RESOURCES = {
  view: "cv://view/state",
  openThreads: "cv://threads?status=open",
  conflicts: "cv://conflicts",
} as const;

/** MCP prompts offered to clients as slash commands. */
export const MCP_PROMPTS = {
  address_review: "Fetch the open threads assigned to the agent with their code, fix and answer each in one call.",
  review_range: "Review a revset: read the range diff window by window and post findings as threads.",
  resolve_conflicts: "List conflicts in a revset, resolve each from its sides, re-check descendants.",
} as const;
