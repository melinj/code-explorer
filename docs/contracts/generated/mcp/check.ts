// Generated from the cv-mcp tool input schemas by scripts/gen-contracts.sh. Do not edit.
// The hand-written spec is docs/contracts/mcp.ts (McpTools[*].in); check.ts compares the two.
// Exact: mutually assignable (U<T> widens brands and literal unions, as in ../check.ts) and the
// same top-level field names. In: the spec's input is accepted by Rust.

import type { McpInputs } from "./inputs";
import type { McpTools } from "../../mcp";
import type { AllKeys, Assert, Exact, Extends, U } from "../check";

export type _ToolNames = Assert<Exact<keyof McpInputs, keyof McpTools>>;
export type _get_view_state = Assert<Exact<U<McpInputs["get_view_state"]>, U<McpTools["get_view_state"]["in"]>>>;
export type _get_view_stateKeys = Assert<Exact<AllKeys<McpInputs["get_view_state"]>, AllKeys<McpTools["get_view_state"]["in"]>>>;
export type _set_view = Assert<Extends<U<McpTools["set_view"]["in"]>, U<McpInputs["set_view"]>>>;
export type _set_viewKeys = Assert<Exact<AllKeys<McpInputs["set_view"]>, AllKeys<McpTools["set_view"]["in"]>>>;
export type _focus = Assert<Exact<U<McpInputs["focus"]>, U<McpTools["focus"]["in"]>>>;
export type _focusKeys = Assert<Exact<AllKeys<McpInputs["focus"]>, AllKeys<McpTools["focus"]["in"]>>>;
export type _create_tour = Assert<Exact<U<McpInputs["create_tour"]>, U<McpTools["create_tour"]["in"]>>>;
export type _create_tourKeys = Assert<Exact<AllKeys<McpInputs["create_tour"]>, AllKeys<McpTools["create_tour"]["in"]>>>;
export type _search = Assert<Exact<U<McpInputs["search"]>, U<McpTools["search"]["in"]>>>;
export type _searchKeys = Assert<Exact<AllKeys<McpInputs["search"]>, AllKeys<McpTools["search"]["in"]>>>;
export type _read_window = Assert<Exact<U<McpInputs["read_window"]>, U<McpTools["read_window"]["in"]>>>;
export type _read_windowKeys = Assert<Exact<AllKeys<McpInputs["read_window"]>, AllKeys<McpTools["read_window"]["in"]>>>;
export type _read_code = Assert<Exact<U<McpInputs["read_code"]>, U<McpTools["read_code"]["in"]>>>;
export type _read_codeKeys = Assert<Exact<AllKeys<McpInputs["read_code"]>, AllKeys<McpTools["read_code"]["in"]>>>;
export type _get_links = Assert<Exact<U<McpInputs["get_links"]>, U<McpTools["get_links"]["in"]>>>;
export type _get_linksKeys = Assert<Exact<AllKeys<McpInputs["get_links"]>, AllKeys<McpTools["get_links"]["in"]>>>;
export type _get_hub = Assert<Exact<U<McpInputs["get_hub"]>, U<McpTools["get_hub"]["in"]>>>;
export type _get_hubKeys = Assert<Exact<AllKeys<McpInputs["get_hub"]>, AllKeys<McpTools["get_hub"]["in"]>>>;
export type _get_metrics = Assert<Exact<U<McpInputs["get_metrics"]>, U<McpTools["get_metrics"]["in"]>>>;
export type _get_metricsKeys = Assert<Exact<AllKeys<McpInputs["get_metrics"]>, AllKeys<McpTools["get_metrics"]["in"]>>>;
export type _get_hotspots = Assert<Exact<U<McpInputs["get_hotspots"]>, U<McpTools["get_hotspots"]["in"]>>>;
export type _get_hotspotsKeys = Assert<Exact<AllKeys<McpInputs["get_hotspots"]>, AllKeys<McpTools["get_hotspots"]["in"]>>>;
export type _get_structure = Assert<Exact<U<McpInputs["get_structure"]>, U<McpTools["get_structure"]["in"]>>>;
export type _get_structureKeys = Assert<Exact<AllKeys<McpInputs["get_structure"]>, AllKeys<McpTools["get_structure"]["in"]>>>;
export type _list_workspaces = Assert<Exact<U<McpInputs["list_workspaces"]>, U<McpTools["list_workspaces"]["in"]>>>;
export type _list_workspacesKeys = Assert<Exact<AllKeys<McpInputs["list_workspaces"]>, AllKeys<McpTools["list_workspaces"]["in"]>>>;
export type _bind_workspace = Assert<Exact<U<McpInputs["bind_workspace"]>, U<McpTools["bind_workspace"]["in"]>>>;
export type _bind_workspaceKeys = Assert<Exact<AllKeys<McpInputs["bind_workspace"]>, AllKeys<McpTools["bind_workspace"]["in"]>>>;
export type _list_threads = Assert<Exact<U<McpInputs["list_threads"]>, U<McpTools["list_threads"]["in"]>>>;
export type _list_threadsKeys = Assert<Exact<AllKeys<McpInputs["list_threads"]>, AllKeys<McpTools["list_threads"]["in"]>>>;
export type _get_thread = Assert<Exact<U<McpInputs["get_thread"]>, U<McpTools["get_thread"]["in"]>>>;
export type _get_threadKeys = Assert<Exact<AllKeys<McpInputs["get_thread"]>, AllKeys<McpTools["get_thread"]["in"]>>>;
export type _create_thread = Assert<Exact<U<McpInputs["create_thread"]>, U<McpTools["create_thread"]["in"]>>>;
export type _create_threadKeys = Assert<Exact<AllKeys<McpInputs["create_thread"]>, AllKeys<McpTools["create_thread"]["in"]>>>;
export type _reply = Assert<Exact<U<McpInputs["reply"]>, U<McpTools["reply"]["in"]>>>;
export type _replyKeys = Assert<Exact<AllKeys<McpInputs["reply"]>, AllKeys<McpTools["reply"]["in"]>>>;
export type _set_thread_status = Assert<Exact<U<McpInputs["set_thread_status"]>, U<McpTools["set_thread_status"]["in"]>>>;
export type _set_thread_statusKeys = Assert<Exact<AllKeys<McpInputs["set_thread_status"]>, AllKeys<McpTools["set_thread_status"]["in"]>>>;
export type _apply_suggestion = Assert<Exact<U<McpInputs["apply_suggestion"]>, U<McpTools["apply_suggestion"]["in"]>>>;
export type _apply_suggestionKeys = Assert<Exact<AllKeys<McpInputs["apply_suggestion"]>, AllKeys<McpTools["apply_suggestion"]["in"]>>>;
export type _list_revisions = Assert<Exact<U<McpInputs["list_revisions"]>, U<McpTools["list_revisions"]["in"]>>>;
export type _list_revisionsKeys = Assert<Exact<AllKeys<McpInputs["list_revisions"]>, AllKeys<McpTools["list_revisions"]["in"]>>>;
export type _get_diff = Assert<Exact<U<McpInputs["get_diff"]>, U<McpTools["get_diff"]["in"]>>>;
export type _get_diffKeys = Assert<Exact<AllKeys<McpInputs["get_diff"]>, AllKeys<McpTools["get_diff"]["in"]>>>;
export type _get_interdiff = Assert<Exact<U<McpInputs["get_interdiff"]>, U<McpTools["get_interdiff"]["in"]>>>;
export type _get_interdiffKeys = Assert<Exact<AllKeys<McpInputs["get_interdiff"]>, AllKeys<McpTools["get_interdiff"]["in"]>>>;
export type _edit_code = Assert<Exact<U<McpInputs["edit_code"]>, U<McpTools["edit_code"]["in"]>>>;
export type _edit_codeKeys = Assert<Exact<AllKeys<McpInputs["edit_code"]>, AllKeys<McpTools["edit_code"]["in"]>>>;
export type _propose_edit = Assert<Exact<U<McpInputs["propose_edit"]>, U<McpTools["propose_edit"]["in"]>>>;
export type _propose_editKeys = Assert<Exact<AllKeys<McpInputs["propose_edit"]>, AllKeys<McpTools["propose_edit"]["in"]>>>;
export type _apply_edit = Assert<Exact<U<McpInputs["apply_edit"]>, U<McpTools["apply_edit"]["in"]>>>;
export type _apply_editKeys = Assert<Exact<AllKeys<McpInputs["apply_edit"]>, AllKeys<McpTools["apply_edit"]["in"]>>>;
export type _list_conflicts = Assert<Exact<U<McpInputs["list_conflicts"]>, U<McpTools["list_conflicts"]["in"]>>>;
export type _list_conflictsKeys = Assert<Exact<AllKeys<McpInputs["list_conflicts"]>, AllKeys<McpTools["list_conflicts"]["in"]>>>;
export type _get_conflict = Assert<Exact<U<McpInputs["get_conflict"]>, U<McpTools["get_conflict"]["in"]>>>;
export type _get_conflictKeys = Assert<Exact<AllKeys<McpInputs["get_conflict"]>, AllKeys<McpTools["get_conflict"]["in"]>>>;
export type _resolve_conflict = Assert<Exact<U<McpInputs["resolve_conflict"]>, U<McpTools["resolve_conflict"]["in"]>>>;
export type _resolve_conflictKeys = Assert<Exact<AllKeys<McpInputs["resolve_conflict"]>, AllKeys<McpTools["resolve_conflict"]["in"]>>>;
export type _undo_edit = Assert<Exact<U<McpInputs["undo_edit"]>, U<McpTools["undo_edit"]["in"]>>>;
export type _undo_editKeys = Assert<Exact<AllKeys<McpInputs["undo_edit"]>, AllKeys<McpTools["undo_edit"]["in"]>>>;
export type _declare_link = Assert<Exact<U<McpInputs["declare_link"]>, U<McpTools["declare_link"]["in"]>>>;
export type _declare_linkKeys = Assert<Exact<AllKeys<McpInputs["declare_link"]>, AllKeys<McpTools["declare_link"]["in"]>>>;
export type _rate_link = Assert<Exact<U<McpInputs["rate_link"]>, U<McpTools["rate_link"]["in"]>>>;
export type _rate_linkKeys = Assert<Exact<AllKeys<McpInputs["rate_link"]>, AllKeys<McpTools["rate_link"]["in"]>>>;
export type _label_cluster = Assert<Exact<U<McpInputs["label_cluster"]>, U<McpTools["label_cluster"]["in"]>>>;
export type _label_clusterKeys = Assert<Exact<AllKeys<McpInputs["label_cluster"]>, AllKeys<McpTools["label_cluster"]["in"]>>>;
export type _wait_for_events = Assert<Exact<U<McpInputs["wait_for_events"]>, U<McpTools["wait_for_events"]["in"]>>>;
export type _wait_for_eventsKeys = Assert<Exact<AllKeys<McpInputs["wait_for_events"]>, AllKeys<McpTools["wait_for_events"]["in"]>>>;

// set_view (in): range options: Rust fills every RangeOptions field the spec requires with its default
