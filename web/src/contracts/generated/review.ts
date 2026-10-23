// Generated from crates/cv-contracts by scripts/gen-contracts.sh (ts-rs). Do not edit.
// The hand-written spec is docs/contracts/*.ts; check.ts compares the two.
import type { JsonValue } from "./json";
import type { SiteRef } from "./wire";
import type { Tour, AgentPermissions } from "./api";

/**
 * Who did something (ids.ts `Actor`).
 */
export type Actor = { "kind": "human", name: string, } | { "kind": "agent", name: string, client?: string, 
/**
 * The jj workspace the agent works in (`X-CV-Workspace`, or the MCP session's bound or
 * default workspace). Additive (DESIGN §7 "Agent binding", §10 per-actor attribution).
 */
workspace?: string | null, } | { "kind": "external" };

export type Fingerprint = { text: string, before: string, after: string, };

/**
 * Threads anchor to a change, not just a commit, so they follow the change through rewrites.
 */
export type Anchor = { change: string, 
/**
 * Commit the anchor was last confirmed against.
 */
commit: string, path: string, side: string, start: number, end: number, cols?: [number, number], fingerprint: Fingerprint, };

export type Comment = { id: string, author: Actor, body: string, createdAt: string, editedAt?: string, };

export type Outdated = { 
/**
 * "lines-changed" | "file-removed" | "change-abandoned"
 */
reason: string, lastGood: string, };

export type Thread = { id: string, anchor: Anchor, status: string, kind: string, severity?: string, assignee?: string, createdBy: Actor, createdAt: string, updatedAt: string, comments: Array<Comment>, producedEdits: Array<string>, outdated?: Outdated, };

/**
 * http.ts `NewThread.anchor`: `rev` is a change id or a commit id (prefixes allowed).
 */
export type NewAnchor = { path: string, rev: string, start: number, end?: number | null, side?: string | null, cols?: [number, number] | null, };

export type NewThread = { anchor: NewAnchor, body: string, kind?: string | null, severity?: string | null, assignee?: string | null, };

/**
 * Replace lines [start, end] (1-based, inclusive). end = start - 1 inserts before start.
 */
export type LinePatch = { start: number, end: number, text: string, };

export type EditFile = { path: string, baseBlob?: string, content?: string, patches?: Array<LinePatch>, };

export type EditOrigin = { thread?: string, };

export type EditRequest = { 
/**
 * Change id or commit id (prefixes allowed); for absorb, the range tip.
 */
target: string, files: Array<EditFile>, mode: string, message?: string, absorbInto?: string, dryRun: boolean, origin?: EditOrigin, };

export type EditWorkspace = { name: string, snapshotted: boolean, updated: boolean, recoveredInto?: string, };

export type EditError = { 
/**
 * "immutable" | "stale-base" | "invalid-target" | "locked" | "jj-error" | "undo-blocked"
 */
code: string, message: string, };

export type EditResult = { id: string, 
/**
 * "previewed" | "applied" | "rejected" | "failed" | "undone"
 */
status: string, actor: Actor, opBefore?: string, opAfter?: string, created?: string, rewritten: Array<string>, workspaces: Array<EditWorkspace>, conflicts: Array<ConflictRecord>, predicted: boolean, error?: EditError, 
/**
 * Applied edits: each edited file's blob in the commit that now holds the edit (the baseBlob
 * for a follow-up edit without re-reading). Additive; absent for absorb and failed edits.
 */
files?: Array<EditedFile>, 
/**
 * Things the human should know before or after applying (DESIGN §6), e.g. an affected
 * workspace with an agent attached ("agent-attached") or a change left divergent
 * ("divergent"). Additive.
 */
warnings?: Array<EditWarning>, 
/**
 * Changes that have more than one visible commit after this edit (or its undo). Additive;
 * normally empty: targeted undo refuses instead of creating divergence.
 */
divergent?: Array<string>, };

export type EditedFile = { path: string, 
/**
 * None when the edit removed the file.
 */
newBlob?: string, };

/**
 * EditResult.warnings item (additive).
 */
export type EditWarning = { 
/**
 * "agent-attached" | "divergent" | "workspace-recovered"
 */
code: string, message: string, workspace?: string, 
/**
 * The agent bound to `workspace` ("agent-attached").
 */
agent?: Actor, changes?: Array<string>, };

/**
 * One operation after an edit's own that touched the changes the edit rewrote.
 */
export type LaterOp = { op: string, description: string, 
/**
 * ISO-8601.
 */
time: string, 
/**
 * "user@host" from the op metadata.
 */
user: string, 
/**
 * The edit's changes this op rewrote or abandoned.
 */
changes: Array<string>, };

/**
 * `ApiError.details` of a 409 `undo-blocked` (POST /edits/:id/undo): reverting the edit's
 * operation now would leave changes divergent or conflicted, so nothing was done.
 */
export type UndoBlocked = { edit: string, 
/**
 * The operation undo would revert (the edit's opAfter).
 */
op: string, 
/**
 * Later operations that touched the edit's changes, newest first.
 */
laterOps: Array<LaterOp>, 
/**
 * Changes the revert would leave with more than one visible commit.
 */
divergent: Array<string>, 
/**
 * Conflicts the revert would introduce.
 */
conflicted: Array<ConflictRecord>, 
/**
 * How to get out of it, in words.
 */
suggestion: string, };

/**
 * A remote bookmark of the same name as a local one.
 */
export type BookmarkRemote = { remote: string, targets: Array<string>, tracked: boolean, 
/**
 * Tracked, one target on each side: local commits the remote lacks (capped at 1000).
 */
ahead?: number, 
/**
 * Tracked, one target on each side: remote commits not in the local bookmark (capped at 1000).
 */
behind?: number, };

/**
 * A local bookmark and the remote bookmarks of the same name (`@git` left out).
 */
export type BookmarkInfo = { name: string, 
/**
 * More than one: conflicted; none: deleted locally but still on a remote.
 */
targets: Array<string>, remotes: Array<BookmarkRemote>, };

/**
 * GET /refs.
 */
export type RefsResponse = { bookmarks: Array<BookmarkInfo>, trunk?: string, colocated: boolean, 
/**
 * The git remotes fetch and push can use, sorted.
 */
remotes: Array<string>, };

/**
 * What a "land" operation brings onto the trunk bookmark; exactly one field.
 */
export type LandSource = { workspace?: string, bookmark?: string, commit?: string, };

/**
 * One Graph view operation (model.ts `GraphOp`). `strategy` and `mode` are validated by cv-review.
 */
export type GraphOp = { "kind": "set-bookmark", name: string, to: string, allowBackwards?: boolean, } | { "kind": "delete-bookmark", name: string, } | { "kind": "land", source: LandSource, trunk: string, 
/**
 * "fast-forward" | "rebase" | "merge"
 */
strategy: string, message?: string, deleteBookmark?: string, } | { "kind": "rebase", source: string, 
/**
 * "revision" | "source" | "branch"
 */
mode: string, dest: Array<string>, } | { "kind": "describe", commit: string, message: string, } | { "kind": "abandon", commits: Array<string>, } | { "kind": "new", parents: Array<string>, message?: string, } | { "kind": "commit", workspace: string, message: string, bookmark?: string, } | { "kind": "squash", source: string, into?: string, message?: string, };

/**
 * POST /graph:op.
 */
export type GraphOpRequest = { op: GraphOp, dryRun?: boolean, };

/**
 * A local bookmark a graph operation created (no `from`), moved or deleted (no `to`).
 */
export type BookmarkMove = { name: string, from?: string, to?: string, };

export type GraphOpResult = { 
/**
 * "previewed" | "applied" | "rejected" | "failed"
 */
status: string, actor: Actor, summary: string, opBefore?: string, opAfter?: string, created?: string, rewritten: Array<string>, bookmarks: Array<BookmarkMove>, workspaces: Array<EditWorkspace>, conflicts: Array<ConflictRecord>, divergent: Array<string>, error?: EditError, };

/**
 * One jj operation (GET /ops), newest first.
 */
export type OpEntry = { op: string, parents: Array<string>, time: string, description: string, 
/**
 * user@host
 */
user: string, snapshot: boolean, };

/**
 * GET /ops.
 */
export type OpLog = { ops: Array<OpEntry>, truncated: boolean, current: string, };

/**
 * GET /ops/:op: what an operation changed, against its first parent.
 */
export type OpDetail = { op: string, changed: Array<string>, abandoned: Array<string>, bookmarks: Array<BookmarkMove>, };

/**
 * POST /ops:restore.
 */
export type RestoreRequest = { op: string, dryRun?: boolean, };

/**
 * POST /git:fetch.
 */
export type FetchRequest = { remote?: string, };

/**
 * POST /git:push.
 */
export type PushRequest = { bookmarks: Array<string>, remote?: string, dryRun?: boolean, };

/**
 * A fetch or push: what jj said and which bookmarks (`name`, `name@remote`) moved.
 */
export type RemoteResult = { 
/**
 * "previewed" | "applied" | "failed"
 */
status: string, summary: string, output: string, moved: Array<BookmarkMove>, opAfter?: string, error?: EditError, };

export type ConflictRecord = { change: string, commit: string, path: string, sides: number, firstSeenOp: string, introducedBy?: string, };

export type ConflictSide = { label: string, text: string, };

/**
 * One conflicted region. `start`/`end` are 1-based inclusive lines of `materialized`, marker lines
 * included; `base` is the (first) base text and `sides` the added texts, in jj's order.
 */
export type ConflictRegion = { start: number, end: number, base?: string, sides: Array<ConflictSide>, };

export type ConflictDetail = { 
/**
 * File text with jj conflict markers ("diff" style, labels name the changes).
 */
materialized: string, regions: Array<ConflictRegion>, change: string, commit: string, path: string, sides: number, firstSeenOp: string, introducedBy?: string, };

/**
 * ws.ts `ServerEvent`. `workspace.updated` carries a `WorkspaceSummary` (model.ts) as JSON (the
 * cards are assembled as JSON), and `view.requested` its `ViewSpec` as sent (parsing it would
 * fill in RangeOptions defaults the client did not send).
 */
export type ServerEvent = { "type": "hello", session: string, op: string, serverVersion: string, cursor: string, 
/**
 * Additive: the `?cursor=` is older than the event log keeps (retention); nothing is
 * replayed, the client refetches its state and continues from `cursor`.
 */
resync?: boolean, } | { "type": "op.changed", op: string, changes: Array<string>, actor: Actor, } | { "type": "index.progress", job: string, done: number, total: number, viewId?: string, } | { "type": "view.invalidated", viewId: string, layoutVersion: number, 
/**
 * "op" | "layout" | "index" | "expand"
 */
reason: string, windows?: Array<string>, tiles?: Array<string>, } | { "type": "thread.upserted", thread: Thread, } | { "type": "review.submitted", threads: Array<string>, by: Actor, note?: string, } | { "type": "review.marked", change: string, commit: string, reviewer: string, } | { "type": "edit.updated", edit: EditResult, } | { "type": "conflicts.changed", added: Array<ConflictRecord>, resolved: Array<ConflictRecord>, } | { "type": "agent.attention", agent: string, workspace?: string, tool: string, windows: Array<string>, note?: string, at: string, } | { "type": "focus.request", by: Actor, target: SiteRef, lines?: [number, number], note?: string, } | { "type": "workspace.updated", workspace: JsonValue, } | { "type": "tour.created", tour: Tour, } | { "type": "agent.permissions", permissions: AgentPermissions, } | { "type": "pong", at: string, } | { "type": "workspace.recovered", workspace: string, divergentCommit: string, note: string, } | { "type": "view.requested", viewId: string, spec: JsonValue, by: Actor, };

export type ServerFrame = { seq: string, event: ServerEvent, };

/**
 * ws.ts `ClientMessage`.
 */
export type ClientMessage = { "type": "view.subscribe", viewId: string, } | { "type": "view.unsubscribe", viewId: string, } | { "type": "view.state", state: JsonValue, } | { "type": "ping" };

/**
 * GET /api/v1/events response.
 */
export type EventsResponse = { cursor: string, events: Array<ServerEvent>, 
/**
 * Additive: the request's cursor is older than the event log keeps (retention), so events
 * were missed; refetch state, then continue from `cursor`.
 */
resync?: boolean, };
