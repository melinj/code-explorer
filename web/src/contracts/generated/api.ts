// Generated from crates/cv-contracts by scripts/gen-contracts.sh (ts-rs). Do not edit.
// The hand-written spec is docs/contracts/*.ts; check.ts compares the two.
import type { Rect, SiteRef, ViewRef, ChurnSummary } from "./wire";
import type { WorkspaceWc, RangeOptions, DiffLine } from "./readside";
import type { Actor } from "./review";

/**
 * The body of every non-2xx answer is `{ error: ApiError }`.
 */
export type ApiError = { 
/**
 * Stable, machine-readable, e.g. "immutable", "stale-base", "revset-invalid".
 */
code: string, 
/**
 * Human-readable; says how to fix it.
 */
message: string, 
/**
 * e.g. `{ hint }` (400 range-not-chain) or the 409 undo-blocked details.
 */
details?: unknown, retryable?: boolean, };

export type WorkspaceInfo = { name: string, 
/**
 * Absolute path of the workspace checkout.
 */
root: string, wc: WorkspaceWc, };

export type SnapshotOptions = { 
/**
 * Workspace whose unsnapshotted files are overlaid live; defaults to the workspace whose @ is `rev`.
 */
workspace?: string, 
/**
 * Overlay files edited on disk but not yet snapshotted by jj. Default true.
 */
liveWorkingCopy?: boolean, 
/**
 * "districts" (the only layout; anything else is 400 "invalid").
 */
layout?: string, };

/**
 * What a view shows: one revision (snapshot) or a revset (range review, D16).
 */
export type ViewSpec = { "mode": "snapshot", 
/**
 * Resolves to one commit, e.g. "@"; default "@", or "<workspace>@" with
 * `options.workspace` (POST /views spells it out in the ViewRef).
 */
rev?: string, fileset?: string, options?: SnapshotOptions, } | { "mode": "range", 
/**
 * A chain: one root and one head, e.g. "trunk()..@".
 */
revset: string, fileset?: string, options?: RangeOptions, };

export type Viewport = { 
/**
 * World-space rectangle on screen.
 */
rect: Rect, 
/**
 * Screen px per wu.
 */
zoom: number, 
/**
 * ZoomTier: "L0" … "L3".
 */
tier: string, };

/**
 * `ViewState.selection`.
 */
export type ViewSelection = { window: string, start: number, end: number, text: string, };

/**
 * One hunk of a [`FileDiff`]: [first line, line count] on each side.
 */
export type FileHunk = { old: [number, number], new: [number, number], lines: Array<DiffLine>, };

export type FileDiff = { path: string, oldPath?: string, 
/**
 * "added" | "modified" | "removed" | "renamed" | "copied".
 */
status: string, hunks: Array<FileHunk>, 
/**
 * Conflicted at the head: no hunks (MCP get_diff). Only ever written as `true`.
 */
conflicted?: boolean, };

export type ThreadSummary = { id: string, status: string, kind: string, 
/**
 * "agent" | "human".
 */
assignee?: string, path: string, start: number, end: number, comments: number, 
/**
 * First line of the first comment.
 */
preview: string, };

/**
 * What the human is looking at. Shared with the agent so "fix this" has a referent.
 */
export type ViewState = { viewId: string, viewport: Viewport, focus?: string, selection?: ViewSelection, 
/**
 * File chip clicked: all fragments highlighted.
 */
selectedFile?: string, lens: string, 
/**
 * "history" | "review" | "workspaces" | "hotspots" | "agent".
 */
panel?: string, 
/**
 * When true, focus.request from the agent moves the camera.
 */
followAgent: boolean, };

/**
 * `RepoInfo.index`.
 */
export type RepoIndex = { 
/**
 * "idle" | "running".
 */
state: string, lastOp?: string, files: number, blobsParsed: number, churn?: ChurnSummary, };

/**
 * `RepoInfo.capabilities`.
 */
export type RepoCapabilities = { scip: boolean, runtimeLinks: boolean, jjLib: boolean, };

/**
 * `SetupInfo.jj`: the jj CLI on the server's PATH.
 */
export type JjInstall = { installed: boolean, version?: string, };

/**
 * GET /setup: what the server serves; "git" is a plain git repository without jj yet.
 */
export type SetupInfo = { 
/**
 * "jj" | "git"
 */
vcs: string, root: string, jj: JjInstall, initCommand: string, };

/**
 * One file of [`GitCommitDetail`]; counts absent for binary files.
 */
export type GitFileStat = { path: string, added?: number, removed?: number, };

/**
 * GET /git/commits/:commit (git mode).
 */
export type GitCommitDetail = { commit: string, files: Array<GitFileStat>, };

/**
 * GET /repo.
 */
export type RepoInfo = { name: string, root: string, jjVersion: string, op: string, trunk?: string, workspaces: Array<WorkspaceInfo>, index: RepoIndex, capabilities: RepoCapabilities, };

export type TourStep = { target: SiteRef, lines?: [number, number], note: string, };

export type Tour = { id: string, title: string, by: Actor, createdAt: string, steps: Array<TourStep>, };

/**
 * Agent writes are rejected with 403 "agent-edit-disabled" while `edit` is false.
 */
export type AgentPermissions = { edit: boolean, moveCamera: boolean, 
/**
 * The UI session these toggles belong to.
 */
session?: string, 
/**
 * The agent they apply to, "*" = every agent.
 */
agent?: string, 
/**
 * True when this is what is in force for an agent across sessions.
 */
effective?: boolean, };

/**
 * One window of a file in `FragmentsResponse`.
 */
export type FragmentRef = { window: string, index: number, rect: Rect, cluster: string, };

/**
 * GET /views/:viewId/fragments.
 */
export type FragmentsResponse = { path: string, 
/**
 * The file chip slot (as in the file fragments of a window header).
 */
chip: number, fragments: Array<FragmentRef>, };

/**
 * One move of a [`LayoutPatch`]: `key` is a WindowKey or `cluster:<id|label>`.
 */
export type LayoutMove = { key: string, x: number, y: number, pinned: boolean, };

/**
 * PATCH /views/:viewId/layout.
 */
export type LayoutPatch = { moves: Array<LayoutMove>, baseLayoutVersion: number, };

/**
 * GET /interdiff.
 */
export type InterdiffResponse = { files: Array<FileDiff>, 
/**
 * [reviewed, current] when the description changed.
 */
description?: [string, string], };

/**
 * GET /agent/view-state: `{ view, state }`, or `{ view: null }` without a UI.
 */
export type AgentViewState = { view: ViewRef, state: ViewState, } | { view: null, };
