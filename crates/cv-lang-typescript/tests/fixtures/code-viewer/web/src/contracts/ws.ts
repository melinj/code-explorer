/**
 * Realtime contract over WebSocket /api/v1/ws (B7 provides; F2 consumes; B9 reads the same events
 * through GET /api/v1/events). Every server event is appended to the event log first, so a client
 * that reconnects with ?cursor=<last seq> gets what it missed.
 */
import type { Actor, ChangeId, CommitId, EventCursor, OpId, RepoPath, ThreadId, ViewId, WindowId, WorkspaceName } from "./ids";
import type { ConflictRecord, EditResult, Lens, SiteRef, Thread, ViewSpec, Viewport, WorkspaceSummary } from "./model";
import type { AgentPermissions, Tour } from "./http";

export interface ServerFrame {
  seq: EventCursor;
  event: ServerEvent;
}

export type ServerEvent =
  | { type: "hello"; session: string; op: OpId; serverVersion: string; cursor: EventCursor }
  | { type: "op.changed"; op: OpId; changes: ChangeId[]; actor: Actor } // any new jj operation
  | { type: "index.progress"; job: IndexJobKind; done: number; total: number; viewId?: ViewId }
  | {
      type: "view.invalidated";
      viewId: ViewId;
      layoutVersion: number;
      reason: "op" | "layout" | "index" | "expand";
      windows?: WindowId[]; // refetch these (T1–T3) … (reason "layout": the windows a PATCH moved; [] = labels changed, reload the bootstrap)
      tiles?: string[]; // … or these tiles ("z/x/y"); neither = everything
    }
  | { type: "thread.upserted"; thread: Thread }
  | { type: "review.submitted"; threads: ThreadId[]; by: Actor; note?: string }
  | { type: "edit.updated"; edit: EditResult }
  | { type: "conflicts.changed"; added: ConflictRecord[]; resolved: ConflictRecord[] }
  | { type: "agent.attention"; agent: string; workspace?: WorkspaceName; tool: string; windows: WindowId[]; note?: string; at: string }
  | { type: "focus.request"; by: Actor; target: SiteRef; lines?: [number, number]; note?: string }
  | { type: "tour.created"; tour: Tour }
  | { type: "agent.permissions"; permissions: AgentPermissions }
  | { type: "workspace.updated"; workspace: WorkspaceSummary } // @ moved, files dirtied, agent bound, overlap changed
  | { type: "workspace.recovered"; workspace: WorkspaceName; divergentCommit: CommitId; note: string }
  /** POST /agent/view (moveCamera allowed): the UI switches to the view while it follows the agent. Additive. */
  | { type: "view.requested"; viewId: ViewId; spec: ViewSpec; by: Actor };

export type IndexJobKind = "sync" | "parse" | "link" | "churn" | "layout" | "reanchor";

export type ClientMessage =
  | { type: "view.subscribe"; viewId: ViewId }
  | { type: "view.unsubscribe"; viewId: ViewId }
  | { type: "view.state"; state: ViewState } // throttled to 4 Hz; feeds MCP get_view_state
  | { type: "ping" };

/** What the human is looking at. Shared with the agent so "fix this" has a referent. */
export interface ViewState {
  viewId: ViewId;
  viewport: Viewport;
  focus?: WindowId;
  selection?: { window: WindowId; start: number; end: number; text: string };
  selectedFile?: RepoPath; // file chip clicked: all fragments highlighted
  lens: Lens;
  panel?: "history" | "review" | "workspaces" | "hotspots" | "agent";
  followAgent: boolean; // when true, focus.request from the agent moves the camera
}
