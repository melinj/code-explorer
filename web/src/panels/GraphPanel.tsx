import { useEffect, useMemo, useRef, useState } from "react";
import type { CommitId, WorkspaceName } from "../contracts/ids";
import type { BookmarkInfo, GraphOp, GraphOpResult, GraphRow, LandSource, RemoteResult, WorkspaceSummary } from "../contracts/model";
import type { Live } from "../data/live";
import { ago } from "./Panels";
import { api, post } from "./graphApi";
import { OpsLog } from "./OpsLog";
import { PushDialog } from "./PushDialog";
import { ROW, edgePaths, gutterWidth, laneColor, laneX, pickRange, rowY, selectionView, type SelectionView } from "./graphLayout";


type Refs = { bookmarks: BookmarkInfo[]; trunk?: string; colocated: boolean; remotes: string[] };
type MapMode = "all" | "changes";

const PRESETS: Array<[string, string]> = [
  ["Default", ""],
  ["Workspaces", "trunk() | (trunk()..working_copies()) | working_copies()"],
  ["Bookmarks", "trunk() | bookmarks() | (trunk()..bookmarks()) | working_copies()"],
  ["All", "all()"],
];
/** Git mode: git revisions (`main`, `HEAD~50..HEAD`) and the few options the server allows. */
const GIT_PRESETS: Array<[string, string]> = [
  ["All branches", ""],
  ["HEAD", "HEAD"],
  ["Local branches", "--branches"],
];
/** Git mode has no live events: re-read the history this often while the page is visible. */
const GIT_POLL_MS = 5000;
const PAGE = 300;

const short = (id: string) => id.slice(0, 8);
const title = (r: GraphRow) => r.rev.description.split("\n")[0] || "(no description set)";
const isRoot = (r: GraphRow) => /^0+$/.test(r.rev.commit);
/** A squash's description when the user writes none, as the server makes it (jj squash's default). */
const combine = (dest: string, source: string) => (!source.trim() ? dest : !dest.trim() ? source : `${dest.trimEnd()}\n\n${source.trimEnd()}\n`);
/** How a bookmark stands against its tracked remotes: "↑2" ahead, "↓1" behind, both when diverged. */
const syncMark = (b: BookmarkInfo) =>
  b.remotes
    .filter((r) => r.tracked && (r.ahead || r.behind))
    .map((r) => `${r.ahead ? `↑${r.ahead}` : ""}${r.behind ? `↓${r.behind}` : ""}`)
    .join(" ");

/** What a drag carries: a bookmark (move it) or a change (rebase it). */
type Drag = { kind: "bookmark"; name: string } | { kind: "change"; commit: CommitId };
const DRAG_MIME = "application/x-cv-graph";

export interface GraphPanelProps {
  live: Live;
  width: number;
  onResize: (w: number) => void;
  onClose?: () => void;
  /** Show these changes on the map (a range view); undefined: back to all files. Resolves once shown. */
  onShow?: (v: SelectionView | undefined) => Promise<void>;
  /** The range revset the map shows now, if it shows a range. */
  shownRange?: () => string | undefined;
  onSnapshot?: (rev: string) => void;
  onRangeDialog?: (revset: string) => void;
  /**
   * "git": a plain git repository served before jj is set up (http.ts GET /setup). The graph is
   * read-only there: no map to drive, no operations, worktrees instead of workspaces.
   */
  vcs?: "jj" | "git";
  /** The rows picked, whenever the selection changes. */
  onPick?: (rows: GraphRow[]) => void;
  /** Expanded: the graph fills the whole area (the map and the side bar stay underneath). */
  expanded?: boolean;
  onExpand?: (on: boolean) => void;
  /** Select these changes and scroll to them (the History tab's "Open in Graph"); n: a new request. */
  focus?: { changes: string[]; n: number };
}

/**
 * The jj graph as a docked side panel (DESIGN §7): lanes, workspaces, bookmarks; picking changes
 * shows them on the map ("Selected") or leaves the map on all files (the default). Bookmarks
 * and changes drag onto other changes (move the bookmark, rebase the change); workspaces land on the
 * trunk bookmark by fast-forward, rebase or merge. Every operation is previewed exactly first and
 * can be undone. In a git-colocated repo bookmarks are the git branches. Clicking a bookmark (on a
 * row or in the bookmark strip) opens its moves: to a workspace's @, to the selected change, to a
 * change clicked next, landing on the trunk, deleting it.
 */
export function GraphPanel({ live, width, onResize, onClose, onShow, shownRange, onSnapshot, onRangeDialog, vcs = "jj", onPick, expanded = false, onExpand, focus }: GraphPanelProps) {
  const git = vcs === "git";
  const presets = git ? GIT_PRESETS : PRESETS;
  const [rows, setRows] = useState<GraphRow[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [limit, setLimit] = useState(PAGE);
  const [input, setInput] = useState("");
  const [revset, setRevset] = useState("");
  const [err, setErr] = useState<string>();
  const [refs, setRefs] = useState<Refs>({ bookmarks: [], colocated: false, remotes: [] });
  const [spaces, setSpaces] = useState<WorkspaceSummary[]>([]);
  const [pick, setPick] = useState<string[]>([]);
  const [mode, setMode] = useState<MapMode>(() => (localStorage.getItem("cv.graph.map") === "changes" ? "changes" : "all"));
  const [mapBusy, setMapBusy] = useState(false);
  const [dialog, setDialog] = useState<GraphOp>();
  const [destFor, setDestFor] = useState<CommitId>(); // "Rebase…": the next click picks the destination
  const [moveFor, setMoveFor] = useState<string>(); // a bookmark's "Move to a change…": the next click picks it
  const [squashFor, setSquashFor] = useState<CommitId>(); // a squash's "Pick another change…": the next click is where it goes
  const [view, setView] = useState<"graph" | "ops">("graph");
  const [push, setPush] = useState<string>(); // the bookmark the Push dialog is for
  const [fetching, setFetching] = useState(false);
  const [bmMenu, setBmMenu] = useState<{ name: string; x: number; y: number }>();
  const [dropOn, setDropOn] = useState<string>();
  const [done, setDone] = useState<GraphOpResult>();
  const [note, setNote] = useState<string>();
  const drag = useRef<Drag>(undefined);
  const root = useRef<HTMLDivElement>(null);
  const reload = useRef<() => void>(() => {});

  // ── Data: rows, bookmarks and workspaces; reloaded after every jj operation ──
  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const load = () => {
      const q = `/revisions?limit=${limit}${revset ? `&revset=${encodeURIComponent(revset)}` : ""}`;
      api<{ rows: GraphRow[]; truncated: boolean }>(q)
        .then((r) => {
          if (!alive) return;
          setRows((old) => {
            // Picks follow their change across rewrites.
            const changeOf = new Map<string, string>(old.map((o) => [o.rev.commit, o.rev.change]));
            const byChange = new Map<string, string>(r.rows.map((n) => [n.rev.change, n.rev.commit]));
            setPick((p) => p.map((c) => (r.rows.some((n) => n.rev.commit === c) ? c : byChange.get(changeOf.get(c) ?? "") ?? "")).filter(Boolean));
            return r.rows;
          });
          setTruncated(r.truncated);
          setErr(undefined);
        })
        .catch((e) => alive && setErr((e as Error).message));
      api<Refs>("/refs")
        .then((r) => alive && setRefs({ ...r, remotes: r.remotes ?? [] }))
        .catch(() => {});
      api<WorkspaceSummary[]>("/workspaces").then((w) => alive && Array.isArray(w) && setSpaces(w)).catch(() => {});
    };
    reload.current = load;
    load();
    const off = live.on((e) => {
      if (e.type === "op.changed" || e.type === "workspace.updated") {
        clearTimeout(timer);
        timer = setTimeout(load, 150);
      }
    });
    const poll = git ? setInterval(() => document.visibilityState === "visible" && load(), GIT_POLL_MS) : undefined;
    return () => {
      alive = false;
      clearTimeout(timer);
      clearInterval(poll);
      off();
    };
  }, [live, revset, limit, git]);

  const byCommit = useMemo(() => new Map<string, GraphRow>(rows.map((r) => [r.rev.commit, r])), [rows]);
  const paths = useMemo(() => edgePaths(rows), [rows]);
  const gutter = gutterWidth(rows);
  const picked = pick.map((c) => byCommit.get(c)).filter((r): r is GraphRow => !!r);
  const one = picked.length === 1 ? picked[0] : undefined;
  const trunk = refs.trunk;
  useEffect(() => onPick?.(picked), [pick.join(","), rows]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── The map follows the selection in "Selected" (one view at a time, the latest wins) ──
  const want = useRef<SelectionView | undefined>(undefined);
  // The range this panel put on the map: clearing the selection only takes that one away, never a
  // range opened elsewhere (the Range dialog, an agent).
  const opened = useRef<string | undefined>(undefined);
  const busy = useRef(false);
  const follow = async () => {
    if (busy.current) return;
    busy.current = true;
    setMapBusy(true);
    try {
      for (;;) {
        const w = want.current;
        const shown = shownRange?.();
        if (!onShow || (w && shown === w.revset) || (!w && (!shown || shown !== opened.current))) break;
        await onShow(w).catch(() => {});
        opened.current = w?.revset;
        if (want.current === w) break;
      }
    } finally {
      busy.current = false;
      setMapBusy(false);
    }
  };
  useEffect(() => {
    if (git) return;
    localStorage.setItem("cv.graph.map", mode);
    if (mode !== "changes") {
      if (want.current) {
        want.current = undefined;
        void follow();
      }
      return;
    }
    const t = setTimeout(() => {
      want.current = selectionView(rows, pick);
      void follow();
    }, 250);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode, pick.join(","), rows.length]);

  // ── What the map shows: the state at a commit, or the diff of the selection ──
  /** State at commit: the whole repository at this change (a snapshot); the map stops following. */
  const stateAt = (r: GraphRow) => {
    want.current = undefined;
    opened.current = undefined;
    setMode("all");
    onExpand?.(false);
    onSnapshot?.(r.rev.change.slice(0, 12));
  };
  /** Diff of commit / range / commits: the map shows the selection's diff and follows it. */
  const showDiff = (commits: string[] = pick) => {
    setMode("changes");
    onExpand?.(false);
    want.current = selectionView(rows, commits);
    void follow();
  };
  const sv = selectionView(rows, pick);
  const diffLabel = picked.length === 1 ? "Diff of commit" : sv?.diff === "cumulative" ? "Diff of range" : "Diff of commits";

  // ── The History tab's "Open in Graph": select its changes once they are loaded ──
  const focused = useRef(0);
  useEffect(() => {
    if (!focus || focused.current === focus.n || !rows.length) return;
    focused.current = focus.n;
    setView("graph");
    const commits = rows.filter((r) => focus.changes.includes(r.rev.change)).map((r) => r.rev.commit);
    if (focus.changes.length && !commits.length) {
      setNote(`${focus.changes.map(short).join(", ")} is not in this graph's revset; try the All preset.`);
      return;
    }
    setPick(commits);
    if (commits[0]) setTimeout(() => root.current?.querySelector(`[data-commit="${commits[0]}"]`)?.scrollIntoView({ block: "center" }), 0);
  }, [focus?.n, rows]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Selection ──
  const click = (r: GraphRow, e: React.MouseEvent) => {
    if (destFor) {
      if (destFor !== r.rev.commit) setDialog({ kind: "rebase", source: destFor, mode: "source", dest: [r.rev.commit] });
      setDestFor(undefined);
      return;
    }
    if (moveFor) {
      setDialog({ kind: "set-bookmark", name: moveFor, to: r.rev.commit });
      setMoveFor(undefined);
      return;
    }
    if (squashFor) {
      if (squashFor !== r.rev.commit) setDialog(squashOp(squashFor, r.rev.commit));
      setSquashFor(undefined);
      return;
    }
    const c = r.rev.commit;
    if (git) {
      // Read-only: one commit at a time (its details show beside the graph).
      setPick((p) => (p.length === 1 && p[0] === c ? [] : [c]));
      return;
    }
    if (e.shiftKey && pick.length) setPick(pickRange(rows, pick[pick.length - 1], c));
    else if (e.metaKey || e.ctrlKey) setPick((p) => (p.includes(c) ? p.filter((x) => x !== c) : [...p, c]));
    else setPick((p) => (p.length === 1 && p[0] === c ? [] : [c]));
  };
  useEffect(() => {
    const el = root.current;
    const k = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).closest?.("input,textarea,select")) return;
      if (e.key === "Escape" && !dialog) {
        if (bmMenu) setBmMenu(undefined);
        else if (destFor || moveFor || squashFor) (setDestFor(undefined), setMoveFor(undefined), setSquashFor(undefined));
        else setPick([]);
      }
    };
    el?.addEventListener("keydown", k);
    return () => el?.removeEventListener("keydown", k);
  }, [destFor, moveFor, squashFor, dialog, bmMenu]);
  // The bookmark menu closes on a click anywhere else.
  useEffect(() => {
    if (!bmMenu) return;
    const down = (e: PointerEvent) => !(e.target as HTMLElement).closest?.(".gp-menu,[data-bm]") && setBmMenu(undefined);
    window.addEventListener("pointerdown", down);
    return () => window.removeEventListener("pointerdown", down);
  }, [bmMenu]);

  // ── Drag and drop: a bookmark onto a change moves it, a change onto another rebases it ──
  const onDrop = (r: GraphRow) => {
    const d = drag.current;
    drag.current = undefined;
    setDropOn(undefined);
    if (!d) return;
    if (d.kind === "bookmark") setDialog({ kind: "set-bookmark", name: d.name, to: r.rev.commit });
    else if (d.commit !== r.rev.commit) setDialog({ kind: "rebase", source: d.commit, mode: "source", dest: [r.rev.commit] });
  };

  // ── Resize handle ──
  const startResize = (e: React.PointerEvent) => {
    const left = root.current!.getBoundingClientRect().left;
    const move = (ev: PointerEvent) => onResize(Math.round(Math.max(280, Math.min(window.innerWidth * 0.7, ev.clientX - left))));
    const up = () => (window.removeEventListener("pointermove", move), window.removeEventListener("pointerup", up));
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    e.preventDefault();
  };

  const landFrom = (r: GraphRow): GraphOp | undefined => {
    if (!trunk) return undefined;
    const b = r.rev.bookmarks.find((b) => !b.includes("@") && b !== trunk);
    return { kind: "land", source: b ? { bookmark: b } : { commit: r.rev.commit }, trunk, strategy: "fast-forward", ...(b ? { deleteBookmark: b } : {}) };
  };
  const wsRow = (name: WorkspaceName) => rows.find((r) => r.rev.workspaces.includes(name));
  /** A workspace's work: its @, or the parent of an empty, undescribed @ (as the server lands it). Undefined when @ is not loaded. */
  const headOf = (name: WorkspaceName): GraphRow | undefined => {
    const r = wsRow(name);
    return r && r.rev.flags.empty && !r.rev.description.trim() && r.rev.parents.length === 1 ? byCommit.get(r.rev.parents[0]) : r;
  };
  /** Whether a workspace's work is already in the trunk: its head carries the trunk bookmark. */
  const landed = (name: WorkspaceName) => !!trunk && !!headOf(name)?.rev.bookmarks.includes(trunk);
  /** Whether a workspace has work to commit: its @ has changes or a message (or files not yet snapshotted). */
  const dirty = (w: WorkspaceSummary) => {
    const r = wsRow(w.name);
    return w.unsnapshottedFiles > 0 || !r || !r.rev.flags.empty || !!r.rev.description.trim();
  };
  const commitOp = (ws: WorkspaceName): GraphOp => ({ kind: "commit", workspace: ws, message: wsRow(ws)?.rev.description ?? "" });
  /** Squash `source` into `into` (default: its only parent), with the combined description to edit. */
  const squashOp = (source: CommitId, into?: CommitId): GraphOp => {
    const src = byCommit.get(source);
    const dest = byCommit.get(into ?? src?.rev.parents[0] ?? "");
    return { kind: "squash", source, ...(into ? { into } : {}), message: combine(dest?.rev.description ?? "", src?.rev.description ?? "") };
  };
  const fetchAll = async () => {
    setFetching(true);
    try {
      const r = await post<RemoteResult>("/git:fetch", {});
      setDone(undefined);
      setNote(r.status === "applied" ? r.summary : `Fetch failed: ${r.error?.message ?? r.summary}`);
      reload.current();
    } catch (e) {
      setNote(`Fetch failed: ${(e as Error).message}`);
    } finally {
      setFetching(false);
    }
  };
  const openBookmark = (name: string, e: React.MouseEvent) => {
    e.stopPropagation();
    const box = root.current!.getBoundingClientRect();
    const at = (e.currentTarget as HTMLElement).getBoundingClientRect();
    setBmMenu((m) => (m?.name === name ? undefined : { name, x: Math.max(6, Math.min(at.left - box.left, box.width - 300)), y: at.bottom - box.top + 4 }));
  };
  /** The moves the bookmark menu offers for `name`. */
  const bookmarkMoves = (name: string): Array<{ label: string; hint?: string; op?: GraphOp; run?: () => void; danger?: boolean }> => {
    const at = refs.bookmarks.find((b) => b.name === name)?.targets ?? [];
    const out: Array<{ label: string; hint?: string; op?: GraphOp; run?: () => void; danger?: boolean }> = [];
    for (const w of spaces) {
      const h = headOf(w.name);
      if (h && at.length === 1 && at[0] === h.rev.commit) continue;
      out.push({
        label: `Move to ${w.name}@${w.serving ? " (you)" : ""}`,
        hint: h ? `${short(h.rev.change)} ${title(h)}` : "its stack head",
        // Not loaded (a narrow revset): land by fast-forward, which the server resolves.
        op: h ? { kind: "set-bookmark", name, to: h.rev.commit } : { kind: "land", source: { workspace: w.name }, trunk: name, strategy: "fast-forward" },
      });
    }
    if (one && !(at.length === 1 && at[0] === one.rev.commit)) out.push({ label: "Move to the selected change", hint: `${short(one.rev.change)} ${title(one)}`, op: { kind: "set-bookmark", name, to: one.rev.commit } });
    out.push({ label: "Move to a change…", hint: "then click it in the graph", run: () => (setMoveFor(name), setPick([])) });
    if (trunk && name !== trunk) out.push({ label: `Land on ${trunk}…`, hint: "fast-forward, rebase or merge", op: { kind: "land", source: { bookmark: name }, trunk, strategy: "fast-forward", deleteBookmark: name } });
    if (refs.remotes.length) {
      const info = refs.bookmarks.find((b) => b.name === name);
      const mark = info ? syncMark(info) : "";
      out.push({ label: `Push to ${refs.remotes.includes("origin") ? "origin" : refs.remotes[0]}…`, hint: mark ? `${mark} against the remote` : info?.remotes.length ? "jj git push" : "new on the remote", run: () => setPush(name) });
    }
    out.push({ label: `Delete ${name}…`, hint: "its changes stay", op: { kind: "delete-bookmark", name }, danger: true });
    return out;
  };
  const locals = refs.bookmarks.filter((b) => b.targets.length);
  const focusRow = (commit: string) => {
    setPick([commit]);
    root.current?.querySelector(`[data-commit="${commit}"]`)?.scrollIntoView({ block: "nearest" });
  };

  const applied = (r: GraphOpResult) => {
    setDialog(undefined);
    setDone(r);
    reload.current();
  };
  const undo = async () => {
    if (!done?.opAfter) return;
    try {
      const r = await post<GraphOpResult>("/graph:undo", { op: done.opAfter });
      setDone(undefined);
      setNote(r.status === "applied" ? `Undone: ${done.summary}` : `Could not undo: ${r.error?.message ?? r.summary}`);
      reload.current();
    } catch (e) {
      setNote(`Could not undo: ${(e as Error).message}`);
    }
  };

  return (
    <section className={`graph-panel ${expanded ? "max" : ""}`} ref={root} tabIndex={-1} style={expanded ? undefined : { width }} aria-label="Repository graph">
      <header className="gp-head">
        {git ? (
          <b>Graph</b>
        ) : (
          <div className="seg small gp-view" role="tablist" aria-label="Graph panel">
            <button role="tab" aria-selected={view === "graph"} className={view === "graph" ? "on" : ""} onClick={() => setView("graph")}>
              Graph
            </button>
            <button role="tab" aria-selected={view === "ops"} className={view === "ops" ? "on" : ""} onClick={() => setView("ops")} title="The operation log: undo or restore any operation">
              Operations
            </button>
          </div>
        )}
        {git ? (
          <span className="chip gp-ro" title="A plain git repository: initialise jj for the map, bookmarks, landing and history edits">
            git · read-only
          </span>
        ) : (
          view === "graph" && (
            <span className="gp-map small">
              <span className="muted">Map</span>
              <span className="seg small" role="group" aria-label="The map shows">
                <button className={mode === "all" ? "on" : ""} aria-pressed={mode === "all"} onClick={() => setMode("all")} title="Repo: the whole repository (at @, or at the commit whose state you opened)">
                  Repo
                </button>
                <button className={mode === "changes" ? "on" : ""} aria-pressed={mode === "changes"} onClick={() => showDiff()} title="Diff: the map follows the selection, showing what one commit changed, or a range of them">
                  Diff
                </button>
              </span>
            </span>
          )
        )}
        {mapBusy && <span className="gp-busy small muted" role="status">opening…</span>}
        <span className="grow" />
        {onExpand && (
          <button className="ghost small gp-expand" aria-pressed={expanded} aria-label={expanded ? "Dock" : "Expand"} onClick={() => onExpand(!expanded)} title={expanded ? "Dock: back to the side panel (G)" : "Expand: fill the window with the graph (G)"}>
            {expanded ? "⤡" : "⤢"}
          </button>
        )}
        {onClose && (
          <button className="x" onClick={onClose} aria-label="Close the graph" title="Close (g)">
            ×
          </button>
        )}
      </header>
      {view === "ops" && !git ? (
        <OpsLog live={live} label={(c) => (rows.find((r) => r.rev.change === c) ? `${short(c)} ${title(rows.find((r) => r.rev.change === c)!)}` : short(c))} onApplied={applied} />
      ) : (
        <>
      <form
        className="revset-bar"
        onSubmit={(e) => {
          e.preventDefault();
          setRevset(input.trim());
        }}
      >
        <input className={`mono ${err ? "bad-in" : ""}`} aria-label="Graph revset" placeholder={git ? "all branches · main · HEAD~50..HEAD · --branches" : "revsets.log | working_copies()"} value={input} onChange={(e) => setInput(e.target.value)} />
        <select
          aria-label="Graph preset"
          value={presets.find(([, r]) => r === revset)?.[1] ?? "custom"}
          onChange={(e) => {
            if (e.target.value === "custom") return;
            setInput(e.target.value);
            setRevset(e.target.value);
          }}
        >
          {presets.map(([l, r]) => (
            <option key={l} value={r}>
              {l}
            </option>
          ))}
          {!presets.some(([, r]) => r === revset) && <option value="custom">Custom</option>}
        </select>
      </form>
      {err && <div className="err small pad0">{err}</div>}
      {spaces.length > 0 && (
        <div className="gp-spaces" aria-label="Workspaces">
          {spaces.map((w) => {
            const r = wsRow(w.name);
            return (
              <span key={w.name} className="gp-ws">
                <button className="chip ws mono" onClick={() => r && focusRow(r.rev.commit)} title={`${w.stack.changes} change(s) on ${w.stack.revset}${w.agent ? ` · ${w.agent.name} ${w.agent.activity}` : ""}`}>
                  {w.name}@{w.serving ? " (you)" : ""}
                  {w.agent && <i className={`dot ${w.agent.activity}`} />}
                </button>
                {dirty(w) && (
                  <button className="small gp-land" onClick={() => setDialog(commitOp(w.name))} title={`jj commit in ${w.name}: describe ${w.name}@ and start a new empty @ on top`}>
                    Commit…
                  </button>
                )}
                {trunk && w.stack.changes > 0 && !landed(w.name) && (
                  <button className="small gp-land" onClick={() => setDialog({ kind: "land", source: { workspace: w.name }, trunk, strategy: "fast-forward" })} title={`Bring ${w.name}@'s stack into ${trunk}: move ${trunk} forward to it, or rebase or merge`}>
                    Land on {trunk}
                  </button>
                )}
              </span>
            );
          })}
        </div>
      )}
      {!git && (locals.length > 0 || refs.remotes.length > 0) && (
        <div className="gp-spaces gp-bms" aria-label="Bookmarks">
          <span className="small muted">Bookmarks</span>
          {[...locals]
            .sort((a, b) => Number(b.name === trunk) - Number(a.name === trunk) || a.name.localeCompare(b.name))
            .map((b) => (
              <button key={b.name} data-bm className={`chip bm mono ${b.name === trunk ? "trunk" : ""} ${bmMenu?.name === b.name ? "open" : ""}`} aria-haspopup="menu" aria-expanded={bmMenu?.name === b.name} onClick={(e) => openBookmark(b.name, e)} title={`${b.name} at ${b.targets.map(short).join(", ")}${b.remotes.map((r) => ` · ${r.remote}: ${r.ahead ? `${r.ahead} ahead` : ""}${r.ahead && r.behind ? ", " : ""}${r.behind ? `${r.behind} behind` : ""}${!r.ahead && !r.behind ? (r.tracked ? "in sync" : "not tracked") : ""}`).join("")}: click to move or push it`}>
                {b.name}
                {syncMark(b) && <span className="sync">{syncMark(b)}</span>} ▾
              </button>
            ))}
          {refs.remotes.length > 0 && (
            <button className="small gp-fetch" disabled={fetching} onClick={fetchAll} title={`jj git fetch from ${refs.remotes.join(", ")}`}>
              {fetching ? "Fetching…" : "Fetch"}
            </button>
          )}
        </div>
      )}
      {refs.colocated && <div className="small muted pad0 gp-git">Git-colocated: bookmarks are git branches; changes here move them.</div>}
      {destFor && (
        <div className="warn-box small gp-pick" role="status">
          Click the destination for rebasing {short(destFor)} (Esc cancels).
        </div>
      )}
      {moveFor && (
        <div className="warn-box small gp-pick" role="status">
          Click the change to move {moveFor} to (Esc cancels).
        </div>
      )}
      {squashFor && (
        <div className="warn-box small gp-pick" role="status">
          Click the change to squash {short(squashFor)} into (Esc cancels).
        </div>
      )}
      <div className="gp-body">
        <div className="gp-rows" style={{ height: rows.length * ROW, ["--gutter" as string]: `${gutter}px` }}>
          <svg className="gp-lanes" width={gutter} height={rows.length * ROW} aria-hidden>
            {paths.map((p, k) => (
              <path key={k} d={p.d} stroke={p.color} strokeWidth={1.6} fill="none" strokeDasharray={p.dashed ? "3 3" : undefined} opacity={p.dashed ? 0.7 : 0.95} />
            ))}
            {rows.map((r, i) => {
              const x = laneX(r.lane);
              const y = rowY(i);
              const c = laneColor(r.lane);
              const on = pick.includes(r.rev.commit);
              const f = r.rev.flags;
              return (
                <g key={r.rev.commit}>
                  {on && <circle cx={x} cy={y} r={8.5} fill="none" stroke="var(--accent)" strokeWidth={2} />}
                  {f.conflict ? (
                    <text x={x} y={y + 5} textAnchor="middle" fontSize="15" fontWeight="700" fill="var(--bad)">
                      ×
                    </text>
                  ) : r.rev.workspaces.length ? (
                    <>
                      <circle cx={x} cy={y} r={6.5} fill="var(--surface)" stroke={c} strokeWidth={1.6} />
                      <text x={x} y={y + 3.5} textAnchor="middle" fontSize="9.5" fontWeight="700" fill={c}>
                        @
                      </text>
                    </>
                  ) : f.immutable ? (
                    <path d={`M${x},${y - 5.5} L${x + 5.5},${y} L${x},${y + 5.5} L${x - 5.5},${y} Z`} fill={c} />
                  ) : (
                    <circle cx={x} cy={y} r={4.5} fill={f.empty ? "var(--surface)" : c} stroke={c} strokeWidth={1.6} />
                  )}
                </g>
              );
            })}
          </svg>
          {rows.map((r) => {
            const f = r.rev.flags;
            const on = pick.includes(r.rev.commit);
            return (
              <div
                key={r.rev.commit}
                data-commit={r.rev.commit}
                className={`gp-row ${on ? "on" : ""} ${dropOn === r.rev.commit ? "drop" : ""} ${destFor || moveFor || squashFor ? "picking" : ""}`}
                style={{ height: ROW }}
                onClick={(e) => click(r, e)}
                onDoubleClick={() => !git && (setPick([r.rev.commit]), showDiff([r.rev.commit]))}
                draggable={!git && !f.immutable}
                onDragStart={(e) => {
                  drag.current = { kind: "change", commit: r.rev.commit };
                  e.dataTransfer.setData(DRAG_MIME, "change");
                  e.dataTransfer.effectAllowed = "move";
                }}
                onDragOver={(e) => {
                  if (!drag.current) return;
                  e.preventDefault();
                  setDropOn(r.rev.commit);
                }}
                onDragLeave={() => setDropOn((d) => (d === r.rev.commit ? undefined : d))}
                onDrop={(e) => (e.preventDefault(), onDrop(r))}
                onDragEnd={() => ((drag.current = undefined), setDropOn(undefined))}
                title={git ? `${r.rev.commit.slice(0, 12)}\n${r.rev.description}` : `${r.rev.change.slice(0, 12)} · ${r.rev.commit.slice(0, 12)}\n${r.rev.description || "(no description set)"}\nDouble-click: diff of this commit on the map`}
              >
                <span className="mono gp-id">
                  <b>{r.rev.change.slice(0, r.rev.shortest.change)}</b>
                  {r.rev.change.slice(r.rev.shortest.change, 8)}
                </span>
                {r.rev.workspaces.map((w) => (
                  <span key={w} className="chip ws">
                    {git ? w : `${w}@`}
                  </span>
                ))}
                {r.rev.bookmarks.map((b) =>
                  b.includes("@") ? (
                    <span key={b} className="chip bm remote" title="remote bookmark">
                      {b}
                    </span>
                  ) : (
                    <span
                      key={b}
                      data-bm
                      className={`chip bm ${b === trunk ? "trunk" : ""} ${bmMenu?.name === b ? "open" : ""}`}
                      draggable={!git}
                      role={git ? undefined : "button"}
                      aria-haspopup={git ? undefined : "menu"}
                      onClick={git ? undefined : (e) => openBookmark(b, e)}
                      title={git ? undefined : "Click to move this bookmark, or drag it onto another change"}
                      onDragStart={(e) => {
                        e.stopPropagation();
                        drag.current = { kind: "bookmark", name: b };
                        e.dataTransfer.setData(DRAG_MIME, "bookmark");
                        e.dataTransfer.effectAllowed = "move";
                      }}
                    >
                      {b}
                    </span>
                  ),
                )}
                {r.rev.tags.map((t) => (
                  <span key={t} className="chip tag">
                    {t}
                  </span>
                ))}
                {f.conflict && <span className="chip bad">conflict</span>}
                {f.divergent && <span className="chip warn">divergent</span>}
                {isRoot(r) ? (
                  <span className="gp-desc muted mono">root()</span>
                ) : (
                  <>
                    <span className={`gp-desc ${r.rev.description ? "" : "muted"}`}>
                      {f.empty && <span className="muted">(empty) </span>}
                      {title(r)}
                    </span>
                    <span className="gp-meta small muted">
                      {r.rev.author.name} · {ago(r.rev.committerTime)}
                    </span>
                    {expanded && (
                      <span className="gp-when small muted mono" title={r.rev.commit}>
                        {new Date(r.rev.committerTime).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })} · {r.rev.commit.slice(0, 8)}
                      </span>
                    )}
                  </>
                )}
              </div>
            );
          })}
        </div>
        {!rows.length && !err && <div className="muted small pad0">No changes in this revset.</div>}
        {truncated && (
          <button className="gp-more" onClick={() => setLimit((l) => l + PAGE)}>
            Show {PAGE} more
          </button>
        )}
      </div>
        </>
      )}
      {done && (
        <div className="gp-done small" role="status">
          <span className="grow">{done.summary}</span>
          {done.opAfter && <button onClick={undo}>Undo</button>}
          <button className="x" aria-label="Dismiss" onClick={() => setDone(undefined)}>
            ×
          </button>
        </div>
      )}
      {note && !done && (
        <div className="gp-done small" role="status">
          <span className="grow">{note}</span>
          <button className="x" aria-label="Dismiss" onClick={() => setNote(undefined)}>
            ×
          </button>
        </div>
      )}
      {view === "graph" && <footer className="gp-actions">
        {git ? (
          <span className="small muted">Read-only git history. Initialise jj for the map, bookmarks, landing and history edits.</span>
        ) : !picked.length ? (
          <span className="small muted">Click a change for its state, its diff and actions; ⇧-click a second for a range, ⌘ adds one. Double-click: diff of commit. Click a bookmark to move or push it.</span>
        ) : (
          <>
            <span className="small muted">Show {picked.length === 1 ? short(picked[0].rev.change) : `${picked.length} changes`}:</span>
            {one && (
              <button onClick={() => stateAt(one)} title="State at commit: the whole repository as it was at this change">
                State at commit
              </button>
            )}
            <button className={mode === "changes" ? "on" : ""} aria-pressed={mode === "changes"} onClick={() => showDiff()} title={picked.length === 1 ? "Diff of commit: only what this change changed, against its parent" : sv?.diff === "cumulative" ? "Diff of range: what these changes changed together, oldest to newest" : "Diff of commits: each change's own diff (they are not one chain)"}>
              {diffLabel}
            </button>
            <span className="gp-break" />
            {one && (
              <>
                <button onClick={() => setDialog({ kind: "set-bookmark", name: one.rev.bookmarks.find((b) => !b.includes("@")) ?? "", to: one.rev.commit })}>Bookmark…</button>
                {trunk && !one.rev.bookmarks.includes(trunk) && <button onClick={() => setDialog(landFrom(one))}>Land on {trunk}…</button>}
                {one.rev.workspaces.length > 0 && !one.rev.flags.immutable && (
                  <button onClick={() => setDialog(commitOp(one.rev.workspaces[0]))} title={`jj commit: describe ${one.rev.workspaces[0]}@ and start a new empty @ on top`}>
                    Commit…
                  </button>
                )}
                <button onClick={() => setDialog({ kind: "new", parents: [one.rev.commit] })} title="jj new --no-edit: a new empty change on top">
                  New child
                </button>
                {!one.rev.flags.immutable && (
                  <>
                    {one.rev.parents.length === 1 && !isRoot(one) && (
                      <button onClick={() => setDialog(squashOp(one.rev.commit))} title="jj squash: move this change's changes into its parent (or a change you pick)">
                        Squash…
                      </button>
                    )}
                    <button onClick={() => setDialog({ kind: "describe", commit: one.rev.commit, message: one.rev.description })}>Describe…</button>
                    <button onClick={() => setDestFor(one.rev.commit)} title="Then click the destination (or drag the change onto it)">
                      Rebase…
                    </button>
                  </>
                )}
              </>
            )}
            {picked.length > 1 && (
              <>
                <button onClick={() => setDialog({ kind: "new", parents: picked.map((r) => r.rev.commit) })} title="jj new --no-edit with these parents">
                  New merge
                </button>
                <button onClick={() => onRangeDialog?.(selectionView(rows, pick)?.revset ?? "")}>Range…</button>
              </>
            )}
            {picked.every((r) => !r.rev.flags.immutable) && (
              <button className="danger" onClick={() => setDialog({ kind: "abandon", commits: picked.map((r) => r.rev.commit) })}>
                Abandon
              </button>
            )}
          </>
        )}
      </footer>}
      {!expanded && <div className="gp-resize" onPointerDown={startResize} role="separator" aria-orientation="vertical" aria-label="Resize the graph" />}
      {bmMenu && (
        <div className="gp-menu" role="menu" aria-label={`Bookmark ${bmMenu.name}`} style={{ left: bmMenu.x, top: bmMenu.y }}>
          <div className="gp-menu-head mono">
            {bmMenu.name}
            <span className="muted"> at {(refs.bookmarks.find((b) => b.name === bmMenu.name)?.targets ?? []).map((c) => (byCommit.get(c) ? short(byCommit.get(c)!.rev.change) : short(c))).join(", ") || "?"}</span>
          </div>
          {bookmarkMoves(bmMenu.name).map((m) => (
            <button
              key={m.label}
              role="menuitem"
              className={m.danger ? "danger-ink" : ""}
              onClick={() => {
                setBmMenu(undefined);
                if (m.op) setDialog(m.op);
                else m.run?.();
              }}
            >
              <span>{m.label}</span>
              {m.hint && <span className="small muted">{m.hint}</span>}
            </button>
          ))}
          <div className="small muted gp-menu-foot">Or drag the bookmark onto any change.</div>
        </div>
      )}
      {dialog && <OpDialog key={JSON.stringify(dialog)} initial={dialog} refs={refs} spaces={spaces} byCommit={byCommit} onClose={() => setDialog(undefined)} onApplied={applied} onPickSquash={(source) => (setDialog(undefined), setSquashFor(source))} />}
      {push && (
        <PushDialog
          bookmark={push}
          remotes={refs.remotes}
          onClose={() => setPush(undefined)}
          onDone={(r) => (setPush(undefined), setDone(undefined), setNote(r.summary), reload.current())}
        />
      )}
    </section>
  );
}

const OP_TITLE: Record<GraphOp["kind"], string> = {
  "set-bookmark": "Set bookmark",
  "delete-bookmark": "Delete bookmark",
  land: "Land on the trunk",
  rebase: "Rebase",
  describe: "Describe",
  abandon: "Abandon",
  new: "New change",
  commit: "Commit",
  squash: "Squash",
};

/**
 * One operation: its form, an exact preview (POST /graph:op dryRun, re-run as the form changes) and
 * Apply. Nothing is written until Apply; the result can be undone from the panel.
 */
function OpDialog({
  initial,
  refs,
  spaces,
  byCommit,
  onClose,
  onApplied,
  onPickSquash,
}: {
  initial: GraphOp;
  refs: Refs;
  spaces: WorkspaceSummary[];
  byCommit: Map<string, GraphRow>;
  onClose: () => void;
  onApplied: (r: GraphOpResult) => void;
  /** Squash: close the dialog and let the next click in the graph pick where it goes. */
  onPickSquash: (source: CommitId) => void;
}) {
  const [op, setOp] = useState<GraphOp>(initial);
  const [preview, setPreview] = useState<GraphOpResult>();
  const [err, setErr] = useState<string>();
  const [busy, setBusy] = useState(false);
  const set = (patch: Partial<GraphOp>) => setOp((o) => ({ ...o, ...patch }) as GraphOp);
  const ready = op.kind === "set-bookmark" ? op.name.trim().length > 0 : op.kind === "commit" ? op.message.trim().length > 0 && (op.bookmark === undefined || op.bookmark.trim().length > 0) : true;
  useEffect(() => {
    if (!ready) return setPreview(undefined);
    const t = setTimeout(() => {
      post<GraphOpResult>("/graph:op", { op, dryRun: true })
        .then((r) => (setPreview(r), setErr(undefined)))
        .catch((e) => (setPreview(undefined), setErr((e as Error).message)));
    }, 250);
    return () => clearTimeout(t);
  }, [JSON.stringify(op)]);
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && (e.stopPropagation(), onClose());
    window.addEventListener("keydown", k, true);
    return () => window.removeEventListener("keydown", k, true);
  }, [onClose]);
  const apply = async () => {
    setBusy(true);
    try {
      const r = await post<GraphOpResult>("/graph:op", { op });
      if (r.status === "applied") onApplied(r);
      else (setPreview(r), setErr(undefined));
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const label = (c: string) => {
    const r = byCommit.get(c);
    return r ? `${short(r.rev.change)} ${title(r)}` : short(c);
  };
  const locals = refs.bookmarks.filter((b) => b.targets.length);
  /** The bookmark a commit in `ws` would most likely advance: one on @'s parent, else the trunk. */
  const suggestBookmark = (ws: WorkspaceName) => {
    const w = spaces.find((x) => x.name === ws);
    const parents = (w && byCommit.get(w.wc.commit)?.rev.parents) ?? [];
    return locals.find((b) => b.targets.some((t) => parents.includes(t)) && b.name === refs.trunk)?.name ?? locals.find((b) => b.targets.some((t) => parents.includes(t)))?.name ?? refs.trunk;
  };
  const backwards = preview?.status === "rejected" && /backwards|sideways/.test(preview.error?.message ?? "");
  return (
    <div className="modal-back" onClick={onClose}>
      <div className="modal gp-dialog" role="dialog" aria-label={OP_TITLE[op.kind]} onClick={(e) => e.stopPropagation()}>
        <h2>{OP_TITLE[op.kind]}</h2>
        {op.kind === "set-bookmark" && (
          <>
            <label className="field">
              <span>Bookmark</span>
              <input className="mono" autoFocus list="gp-bookmarks" value={op.name} onChange={(e) => set({ name: e.target.value })} placeholder="new or existing name" />
              <datalist id="gp-bookmarks">
                {locals.map((b) => (
                  <option key={b.name} value={b.name} />
                ))}
              </datalist>
            </label>
            <div className="small muted">at {label(op.to)}</div>
            {(() => {
              // jj bookmarks follow rewrites: one on a working copy moves along as it is edited.
              const w = spaces.find((w) => w.wc.commit === op.to);
              const r = byCommit.get(op.to);
              return w && r && !r.rev.flags.empty ? (
                <div className="small warn-inline">
                  That is {w.name}@, still being edited: {op.name || "the bookmark"} follows it as files change. To land a finished change, commit it first.{" "}
                  <button className="linkish small" onClick={() => setOp({ kind: "commit", workspace: w.name, message: r.rev.description, ...(op.name.trim() ? { bookmark: op.name.trim() } : {}) })}>
                    Commit {w.name}@ and move {op.name || "the bookmark"} to it…
                  </button>
                </div>
              ) : null;
            })()}
            <label className={`row small ${backwards ? "warn-inline" : ""}`}>
              <input type="checkbox" checked={!!op.allowBackwards} onChange={(e) => set({ allowBackwards: e.target.checked })} /> Allow moving it backwards or sideways
            </label>
            {locals.some((b) => b.name === op.name) && (
              <button className="linkish small" onClick={() => setOp({ kind: "delete-bookmark", name: op.name })}>
                Delete {op.name} instead
              </button>
            )}
          </>
        )}
        {op.kind === "delete-bookmark" && <div className="small">Deletes the local bookmark {op.name}; a tracked remote bookmark is deleted on the next push. Its commits stay.</div>}
        {op.kind === "land" && (
          <>
            <label className="field">
              <span>Land</span>
              <select
                value={op.source.workspace ? `w:${op.source.workspace}` : op.source.bookmark ? `b:${op.source.bookmark}` : `c:${op.source.commit}`}
                onChange={(e) => {
                  const [k, v] = [e.target.value.slice(0, 1), e.target.value.slice(2)];
                  const source: LandSource = k === "w" ? { workspace: v as WorkspaceName } : k === "b" ? { bookmark: v } : { commit: v as CommitId };
                  setOp({ ...op, source, deleteBookmark: k === "b" ? op.deleteBookmark ?? v : undefined });
                }}
              >
                {op.source.commit && <option value={`c:${op.source.commit}`}>{label(op.source.commit)}</option>}
                {spaces.map((w) => (
                  <option key={w.name} value={`w:${w.name}`}>
                    workspace {w.name}@ ({w.stack.changes} change{w.stack.changes === 1 ? "" : "s"})
                  </option>
                ))}
                {locals
                  .filter((b) => b.name !== op.trunk)
                  .map((b) => (
                    <option key={b.name} value={`b:${b.name}`}>
                      bookmark {b.name}
                    </option>
                  ))}
              </select>
            </label>
            <label className="field">
              <span>Onto</span>
              <select value={op.trunk} onChange={(e) => set({ trunk: e.target.value })}>
                {locals.map((b) => (
                  <option key={b.name} value={b.name}>
                    {b.name}
                    {b.name === refs.trunk ? " (trunk)" : ""}
                  </option>
                ))}
              </select>
            </label>
            <div className="seg small gp-strategy" role="radiogroup" aria-label="Strategy">
              {(
                [
                  ["fast-forward", "Fast-forward", "Move the bookmark forward; only when it is an ancestor"],
                  ["rebase", "Rebase", "jj rebase -b onto the trunk, then move the bookmark"],
                  ["merge", "Merge", "A merge commit of the trunk and the stack; the workspace's empty @ follows it"],
                ] as const
              ).map(([v, l, t]) => (
                <button key={v} role="radio" aria-checked={op.strategy === v} className={op.strategy === v ? "on" : ""} title={t} onClick={() => set({ strategy: v })}>
                  {l}
                </button>
              ))}
            </div>
            {op.strategy === "merge" && (
              <label className="field">
                <span>Message</span>
                <input value={op.message ?? ""} placeholder="Merge … into …" onChange={(e) => set({ message: e.target.value || undefined })} />
              </label>
            )}
            {op.source.bookmark && (
              <label className="row small">
                <input type="checkbox" checked={op.deleteBookmark === op.source.bookmark} onChange={(e) => set({ deleteBookmark: e.target.checked ? op.source.bookmark : undefined })} /> Delete {op.source.bookmark} after landing
              </label>
            )}
          </>
        )}
        {op.kind === "rebase" && (
          <>
            <div className="small">
              {label(op.source)} → onto {op.dest.map(label).join(", ")}
            </div>
            <div className="seg small" role="radiogroup" aria-label="What moves">
              {(
                [
                  ["source", "With descendants", "jj rebase -s"],
                  ["revision", "Only this change", "jj rebase -r: its descendants stay, on its parents"],
                  ["branch", "Whole branch", "jj rebase -b: everything not already in the destination"],
                ] as const
              ).map(([v, l, t]) => (
                <button key={v} role="radio" aria-checked={op.mode === v} className={op.mode === v ? "on" : ""} title={t} onClick={() => set({ mode: v })}>
                  {l}
                </button>
              ))}
            </div>
          </>
        )}
        {op.kind === "describe" && (
          <label className="field">
            <span>{label(op.commit).split(" ")[0]}</span>
            <textarea rows={4} autoFocus value={op.message} onChange={(e) => set({ message: e.target.value })} />
          </label>
        )}
        {op.kind === "new" && (
          <>
            <div className="small">On {op.parents.map(label).join(" and ")}. Your working copy stays where it is.</div>
            <label className="field">
              <span>Message</span>
              <input autoFocus value={op.message ?? ""} onChange={(e) => set({ message: e.target.value || undefined })} placeholder="(optional)" />
            </label>
          </>
        )}
        {op.kind === "commit" && (
          <>
            <div className="small muted">
              {op.workspace}@{(() => {
                const w = spaces.find((x) => x.name === op.workspace);
                const r = w && byCommit.get(w.wc.commit);
                return r ? ` · ${short(r.rev.change)}` : "";
              })()}
              , with everything edited on disk, gets this message, and a new empty @ starts on top (jj commit). The files on disk stay as they are.
            </div>
            <label className="field">
              <span>Message</span>
              <textarea
                rows={4}
                autoFocus
                value={op.message}
                placeholder="What this change does"
                onChange={(e) => set({ message: e.target.value })}
                onKeyDown={(e) => e.key === "Enter" && (e.metaKey || e.ctrlKey) && preview?.status === "previewed" && !busy && (e.preventDefault(), void apply())}
              />
            </label>
            <label className="row small">
              <input type="checkbox" checked={op.bookmark !== undefined} onChange={(e) => set({ bookmark: e.target.checked ? (suggestBookmark(op.workspace) ?? "") : undefined })} /> Move a bookmark to it
            </label>
            {op.bookmark !== undefined && (
              <label className="field">
                <span>Bookmark</span>
                <input className="mono" list="gp-bookmarks" value={op.bookmark} onChange={(e) => set({ bookmark: e.target.value })} placeholder="new or existing name" />
                <datalist id="gp-bookmarks">
                  {locals.map((b) => (
                    <option key={b.name} value={b.name} />
                  ))}
                </datalist>
              </label>
            )}
            {!op.message.trim() && <div className="small muted">Write a message to preview. ⌘/Ctrl+Enter applies.</div>}
          </>
        )}
        {op.kind === "squash" && (
          <>
            <div className="small">
              {label(op.source)} → into {op.into ? label(op.into) : (() => {
                const p = byCommit.get(op.source)?.rev.parents[0];
                return p ? `${label(p)} (its parent)` : "its parent";
              })()}{" "}
              <button className="linkish small" onClick={() => onPickSquash(op.source)}>
                Pick another change…
              </button>
            </div>
            <div className="small muted">Its changes move there and it is abandoned (a working copy gets a new empty @). Changes in between are rebased.</div>
            <label className="field">
              <span>Message</span>
              <textarea rows={4} value={op.message ?? ""} onChange={(e) => set({ message: e.target.value })} placeholder="(both descriptions, combined)" />
            </label>
          </>
        )}
        {op.kind === "abandon" && (
          <ul className="small gp-list">
            {op.commits.map((c) => (
              <li key={c}>{label(c)}</li>
            ))}
            <li className="muted">Descendants move onto the parents; a workspace whose @ is abandoned gets a new empty @.</li>
          </ul>
        )}
        <div className="gp-preview" aria-live="polite">
          {err && <div className="err small">{err}</div>}
          {!preview && !err && ready && <div className="muted small">previewing…</div>}
          {preview && preview.status === "rejected" && <div className="warn-box small">{preview.error?.message ?? preview.summary}</div>}
          {preview && preview.status === "failed" && <div className="err small">{preview.error?.message ?? preview.summary}</div>}
          {preview && (preview.status === "previewed" || preview.status === "applied") && (
            <div className="small">
              <div>
                <b>Preview:</b> {preview.summary}
              </div>
              {preview.bookmarks.map((b) => (
                <div key={b.name} className="mono">
                  {b.name}: {b.from ? short(b.from) : "(new)"} → {b.to ? short(b.to) : "(deleted)"}
                </div>
              ))}
              {preview.workspaces.length > 0 && <div>Updates the files of {preview.workspaces.map((w) => `${w.name}@`).join(", ")} on disk.</div>}
              {preview.conflicts.length > 0 && (
                <div className="warn-inline">
                  Leaves {preview.conflicts.length} conflict{preview.conflicts.length === 1 ? "" : "s"}: {[...new Set(preview.conflicts.map((c) => c.path))].slice(0, 5).join(", ")}
                </div>
              )}
              {preview.divergent.length > 0 && <div className="warn-inline">Leaves {preview.divergent.length} divergent change(s).</div>}
              {refs.colocated && preview.bookmarks.length > 0 && <div className="muted">The git branches move with the bookmarks.</div>}
            </div>
          )}
        </div>
        <div className="row end">
          <button onClick={onClose}>Cancel</button>
          <button className="primary" disabled={busy || preview?.status !== "previewed"} onClick={apply}>
            {busy ? "Applying…" : "Apply"}
          </button>
        </div>
      </div>
    </div>
  );
}
