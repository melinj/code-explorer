/**
 * Identifiers and conventions shared by every piece (server, web, MCP).
 *
 * Wire conventions (all contracts):
 * - Every id is a plain string on the wire; the brands below are compile-time only.
 * - Lines are 1-based and inclusive. Columns are 0-based UTF-16 code units (what the
 *   browser and CodeMirror use); the parser converts from tree-sitter byte columns.
 * - Paths are repo-relative, forward slashes, no leading "./".
 * - Times are ISO-8601 strings with offset.
 * - World units (wu) on the canvas: 1 code line = 18 wu tall, 1 monospace column = 9 wu wide,
 *   window header = 36 wu.
 */

declare const brand: unique symbol;
export type Brand<T, B extends string> = T & { readonly [brand]: B };

/** jj change id (32 reverse-hex chars). Stable across rewrites; UI shows the shortest unique prefix. */
export type ChangeId = Brand<string, "ChangeId">;
/** jj/git commit id (40 hex). Changes whenever a change is rewritten. */
export type CommitId = Brand<string, "CommitId">;
/** jj operation id. Every repo mutation (ours, the user's, an agent's) creates one. */
export type OpId = Brand<string, "OpId">;
/** git blob id of file content. Content-addressed cache key for parse results and text. */
export type BlobId = Brand<string, "BlobId">;
export type RepoPath = Brand<string, "RepoPath">;
/** jj workspace name, e.g. "default", "gym". Agents usually work in their own workspace. */
export type WorkspaceName = Brand<string, "WorkspaceName">;
/** Lane id from the lane rules: "infra", "config", "code", "test", "docs", "other", or user-defined. */
export type LaneId = Brand<string, "LaneId">;
/** jj revset expression. Passed to jj as an argv element, never through a shell. */
export type Revset = Brand<string, "Revset">;
/** jj fileset expression, e.g. `glob:"src/**" & ~glob:"**\/tests/**"`. */
export type Fileset = Brand<string, "Fileset">;

/** "v_" + hash(ViewSpec + resolved commits + op). A new op that changes the resolution yields a new id. */
export type ViewId = Brand<string, "ViewId">;
/** Per-view window id: hash(ViewId, WindowKey). */
export type WindowId = Brand<string, "WindowId">;
/**
 * Revision-independent layout key, so positions and pins survive history moving on:
 * "<path>#<qualname>" | "<path>#chunk:<first-symbol>" | "<path>#hunk:<ordinal>" | "<path>#file".
 */
export type WindowKey = Brand<string, "WindowKey">;
export type ClusterId = Brand<string, "ClusterId">;
/** "<protocol>:<normalized key>", e.g. "bus:world.entity.moved", "http:GET /api/node/{}". */
export type HubId = Brand<string, "HubId">;
export type LinkId = Brand<string, "LinkId">;
export type ThreadId = Brand<string, "ThreadId">;
export type CommentId = Brand<string, "CommentId">;
export type EditId = Brand<string, "EditId">;
export type TourId = Brand<string, "TourId">;
/** Monotonic position in the server event log (stringified integer). */
export type EventCursor = Brand<string, "EventCursor">;

/** Who did something. Shown on threads, edits and the activity feed. */
export type Actor =
  | { kind: "human"; name: string }
  // e.g. { name: "claude", client: "claude-code", workspace: "gym" }. `workspace` (additive): the jj
  // workspace the agent works in (X-CV-Workspace, or the MCP session's bound/default workspace);
  // op descriptions read "(claude@gym)".
  | { kind: "agent"; name: string; client?: string; workspace?: WorkspaceName }
  | { kind: "external" }; // jj CLI or an editor, seen through the op log or the file watcher

export type Unsubscribe = () => void;

export interface ApiError {
  code: string; // stable, machine-readable, e.g. "immutable", "stale-base", "revset-invalid"
  message: string; // human-readable, says how to fix it
  details?: unknown;
  retryable?: boolean;
}
