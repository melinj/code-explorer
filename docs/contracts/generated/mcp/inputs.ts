// Generated from the cv-mcp tool input schemas by scripts/gen-contracts.sh. Do not edit.
// The hand-written spec is docs/contracts/mcp.ts (McpTools[*].in); check.ts compares the two.

/** Each MCP tool's input as its JSON Schema states it. */
export interface McpInputs {
  get_view_state: Record<string, never>;
  set_view: { spec: ({ fileset?: string; mode: "snapshot"; options?: { layout?: string; liveWorkingCopy?: boolean; workspace?: string }; rev?: string }) | ({ fileset?: string; mode: "range"; options?: { allowNonChain?: boolean; contextDisplay?: string; contextHops?: number; contextLines?: number; diff?: string; ignoreWhitespace?: boolean; layout?: string }; revset: string }) };
  focus: { lines?: [number, number]; note?: string; target: string };
  create_tour: { steps: Array<{ lines?: [number, number]; note: string; target: string }>; title: string };
  search: { kinds?: Array<"symbol" | "file" | "hub" | "thread" | "revision">; limit?: number; query: string };
  read_window: { annotate?: Array<"ports" | "threads" | "diff">; window: string };
  read_code: { annotate?: Array<"ports" | "threads" | "diff">; end?: number; path: string; rev?: string; start?: number };
  get_links: { depth?: number; dir?: "in" | "out" | "both"; kinds?: Array<string>; limit?: number; target: string };
  get_hub: { key: string; protocol?: string };
  get_metrics: { target: string };
  get_hotspots: { lane?: string; limit?: number };
  list_workspaces: Record<string, never>;
  bind_workspace: { workspace: string };
  list_threads: { assignee?: "agent" | "human"; path?: string; revset?: string; status?: Array<"open" | "in-progress" | "addressed" | "resolved" | "outdated" | "wontfix">; withCode?: boolean };
  get_thread: { contextLines?: number; thread: string };
  create_thread: { anchor: { end: number; path: string; rev?: string; side?: "new" | "old"; start: number }; body: string; kind?: "comment" | "suggestion" | "question" | "task" | "finding"; severity?: "info" | "minor" | "major" | "blocker" };
  reply: { body?: string; setStatus?: "open" | "in-progress" | "addressed" | "resolved" | "outdated" | "wontfix"; thread: string };
  set_thread_status: { status: "open" | "in-progress" | "addressed" | "resolved" | "outdated" | "wontfix"; thread: string };
  apply_suggestion: { dryRun?: boolean; mode?: "amend" | "insert-after"; reply?: string; setStatus?: "open" | "in-progress" | "addressed" | "resolved" | "outdated" | "wontfix"; thread: string };
  list_revisions: { limit?: number; revset?: string };
  get_diff: { fileset?: string; from?: string; revset?: string; to?: string };
  get_interdiff: { change: string; from: string };
  edit_code: { baseBlob?: string; content?: string; dryRun?: boolean; message?: string; mode?: "amend" | "insert-after" | "branch" | "keep-descendants" | "absorb"; patches?: Array<{ end: number; start: number; text: string }>; path?: string; reply?: string; rev?: string; setStatus?: "open" | "in-progress" | "addressed" | "resolved" | "outdated" | "wontfix"; thread?: string };
  propose_edit: { baseBlob?: string; content?: string; dryRun?: boolean; message?: string; mode?: "amend" | "insert-after" | "branch" | "keep-descendants" | "absorb"; patches?: Array<{ end: number; start: number; text: string }>; path?: string; reply?: string; rev?: string; setStatus?: "open" | "in-progress" | "addressed" | "resolved" | "outdated" | "wontfix"; thread?: string };
  apply_edit: { edit: string };
  list_conflicts: { revset?: string };
  get_conflict: { path: string; rev: string };
  resolve_conflict: { content: string; path: string; rev: string };
  undo_edit: { edit: string };
  declare_link: { from: string; kind: string; label: string; note?: string; to: string };
  rate_link: { link: string; note?: string; verdict: "confirmed" | "rejected" };
  label_cluster: { cluster: string; label: string; summary?: string };
  wait_for_events: { cursor?: string; kinds?: Array<string>; limit?: number; timeoutS?: number };
}
