import type { NewThread } from "../contracts/http";
import type { ThreadId } from "../contracts/ids";
import type { ConflictDetail, ConflictRecord, EditMode, EditRequest, EditResult, EditWarning, Thread, ThreadStatus, WindowHeader } from "../contracts/model";

/** A non-2xx answer: http.ts `{ error: { code, message, details? } }` (the mock sends them at the top level). */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly hint?: string,
    readonly details?: Record<string, unknown>,
  ) {
    super(message);
  }
}

export type { EditWarning, UndoBlocked } from "../contracts/model";

/** The warnings of an edit result, with `divergent` folded in when no warning names it. */
export function editWarnings(r?: EditResult): EditWarning[] {
  const w = [...(r?.warnings ?? [])];
  if (r?.divergent?.length && !w.some((y) => y.code === "divergent")) w.push({ code: "divergent", message: `left divergent: ${r.divergent.join(", ")}`, changes: r.divergent });
  return w;
}

/** The last edit this tab applied, offered for undo until another replaces it. */
export interface AppliedEdit {
  result: EditResult;
  paths: string[];
  label: string;
  at: number;
}

/**
 * Review threads and edits (B8 routes of docs/contracts/http.ts). Threads are loaded once and kept
 * up to date from this client's own writes; listeners hear about every change.
 */
export class ReviewClient {
  threads = new Map<string, Thread>();
  byPath = new Map<string, Thread[]>();
  private listeners = new Set<() => void>();

  constructor(private base = "/api/v1") {}

  onChange(cb: () => void): () => void {
    this.listeners.add(cb);
    return () => void this.listeners.delete(cb);
  }

  private emit() {
    this.byPath.clear();
    for (const t of this.threads.values()) {
      const l = this.byPath.get(t.anchor.path);
      if (l) l.push(t);
      else this.byPath.set(t.anchor.path, [t]);
    }
    for (const cb of this.listeners) cb();
  }

  private async call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const r = await fetch(this.base + path, {
      method,
      headers: body === undefined ? undefined : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!r.ok) {
      // http.ts errors are { error: { code, message } }; the mock sends { code, message }.
      const body = await r.json().catch(() => ({}));
      const err = body.error ?? body;
      throw new ApiError(r.status, err.code ?? String(r.status), err.message ?? (r.status === 404 ? "this server does not offer that yet" : r.statusText), err.details?.hint ?? err.hint, err.details);
    }
    return r.json();
  }

  /** A thread from the event stream (another client, or the agent). */
  upsert(t: Thread) {
    return this.put(t);
  }

  private put(t: Thread) {
    this.threads.set(t.id, t);
    this.emit();
    return t;
  }

  /** Open conflicts with their details (regions and sides), by path. */
  conflicts = new Map<string, ConflictDetail>();

  /** Bumped by every load and resolve: an older load still in flight must not bring a resolved conflict back. */
  private conflictSeq = 0;

  async loadConflicts() {
    const seq = ++this.conflictSeq;
    // A failed list read is not "every conflict resolved": keep the previous list.
    const list = await this.call<ConflictRecord[]>("GET", "/conflicts").catch(() => undefined);
    if (!list) return;
    const details = await Promise.all(
      list.map((c) =>
        this.call<ConflictDetail>("GET", conflictUrl(c.commit, c.path)).catch(() => {
          // Detail read failed: keep what we had for the same conflict, if anything.
          const prev = this.conflicts.get(c.path);
          return prev && prev.commit === c.commit ? prev : undefined;
        }),
      ),
    );
    if (seq !== this.conflictSeq) return;
    this.conflicts = new Map(details.filter((d): d is ConflictDetail => !!d).map((d) => [d.path, d]));
    this.emit();
  }

  /** Resolve a conflict with the whole new file content (http.ts). */
  async resolveConflict(d: ConflictDetail, content: string) {
    const r = await this.call<EditResult>("POST", `${conflictUrl(d.commit, d.path)}/resolve`, { content });
    this.conflictSeq++;
    this.conflicts.delete(d.path);
    this.emit();
    return r;
  }

  async load() {
    const list = await this.call<Thread[]>("GET", "/threads");
    this.threads = new Map(list.map((t) => [t.id, t]));
    this.emit();
  }

  /** Open threads anchored inside a window's lines. */
  threadsIn(w: WindowHeader): Thread[] {
    return (this.byPath.get(w.path) ?? []).filter((t) => t.anchor.start >= w.lines.start && t.anchor.start <= w.lines.end);
  }

  async create(t: NewThread) {
    return this.put(await this.call<Thread>("POST", "/threads", t));
  }

  async reply(id: string, body: string, setStatus?: ThreadStatus) {
    return this.put(await this.call<Thread>("POST", `/threads/${id}/comments`, { body, setStatus }));
  }

  /** "Send to agent": assign the threads to the agent bound to the workspace (reviews:submit). */
  async submit(ids: string[], note?: string) {
    await this.call<{ cursor: string }>("POST", "/reviews:submit", { threads: ids, note });
    for (const id of ids) {
      const t = this.threads.get(id);
      if (t) this.threads.set(id, { ...t, assignee: "agent" });
    }
    this.emit();
  }

  async setStatus(id: string, status: ThreadStatus, assignee?: "agent" | "human" | null) {
    return this.put(await this.call<Thread>("PATCH", `/threads/${id}`, assignee === undefined ? { status } : { status, assignee }));
  }

  /**
   * Replace lines [start, end] of `w`'s file. dryRun previews (exact preview on the real server).
   * `target` overrides the window's change (range views: the tip for absorb, or a chosen commit);
   * `absorbInto` names the candidate commits for mode "absorb".
   */
  edit(w: WindowHeader, start: number, end: number, text: string, mode: EditMode, dryRun: boolean, message?: string, thread?: string, opts?: { target?: string; absorbInto?: string }) {
    const req: EditRequest = {
      target: (opts?.target ?? w.change) as EditRequest["target"],
      files: [{ path: w.path, baseBlob: w.blob, patches: [{ start, end, text }] }],
      mode,
      message,
      absorbInto: opts?.absorbInto as EditRequest["absorbInto"],
      dryRun,
      origin: thread ? { thread: thread as ThreadId } : undefined,
    };
    return this.call<EditResult>("POST", "/edits", req);
  }

  async apply(editId: string, label = "edit", paths: string[] = []) {
    const r = await this.call<EditResult>("POST", `/edits/${editId}/apply`);
    if (r.status === "applied") {
      this.lastApplied = { result: r, paths: paths.length ? paths : (r.files ?? []).map((f) => f.path), label, at: Date.now() };
      for (const cb of this.appliedListeners) cb(this.lastApplied);
    }
    return r;
  }

  /** The last edit applied from this tab (the undo bar offers it). */
  lastApplied?: AppliedEdit;
  private appliedListeners = new Set<(a?: AppliedEdit) => void>();
  onApplied(cb: (a?: AppliedEdit) => void): () => void {
    this.appliedListeners.add(cb);
    return () => void this.appliedListeners.delete(cb);
  }

  /**
   * POST /edits/:id/undo (`jj op revert <opAfter>`). The server refuses when undoing is no longer
   * safe (e.g. the change was rewritten since); that refusal is thrown as an ApiError to show.
   */
  async undo(editId: string) {
    const r = await this.call<EditResult>("POST", `/edits/${editId}/undo`);
    if (r.error) throw new ApiError(409, r.error.code, r.error.message);
    if (this.lastApplied?.result.id === editId) {
      this.lastApplied = undefined;
      for (const cb of this.appliedListeners) cb(undefined);
    }
    return r;
  }

  /**
   * "Ask agent" (§6): file a task thread and send it with reviews:submit, which assigns it to the
   * agent bound to the workspace and wakes it (review.submitted).
   */
  async askAgent(t: NewThread, note?: string) {
    const th = await this.create({ ...t, kind: t.kind ?? "task", assignee: undefined });
    await this.submit([th.id], note);
    return this.threads.get(th.id) ?? th;
  }
}

/** `/conflicts/:commit/*path` with each path segment encoded (spaces, `#`, `?`, `%` in file names). */
export const conflictUrl = (commit: string, path: string) => `/conflicts/${encodeURIComponent(commit)}/${path.split("/").map(encodeURIComponent).join("/")}`;

/** The replacement text of the first ```suggestion block in a comment body, if any. */
export function suggestionOf(body: string): string | undefined {
  const m = /```suggestion\n([\s\S]*?)\n?```/.exec(body);
  return m?.[1];
}
